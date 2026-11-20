// SPDX-License-Identifier: Apache-2.0

use ax_errno::{AxError, AxResult};
use axvcpu::x86_regs as reg;

use super::VmxVcpu;
use crate::vmx::vmcs::{VmcsGuest16, VmcsGuest32, VmcsGuest64, VmcsGuestNW};

const SELECTORS: [VmcsGuest16; 8] = [
    VmcsGuest16::ES_SELECTOR,
    VmcsGuest16::CS_SELECTOR,
    VmcsGuest16::SS_SELECTOR,
    VmcsGuest16::DS_SELECTOR,
    VmcsGuest16::FS_SELECTOR,
    VmcsGuest16::GS_SELECTOR,
    VmcsGuest16::LDTR_SELECTOR,
    VmcsGuest16::TR_SELECTOR,
];
const BASES: [VmcsGuestNW; 10] = [
    VmcsGuestNW::ES_BASE,
    VmcsGuestNW::CS_BASE,
    VmcsGuestNW::SS_BASE,
    VmcsGuestNW::DS_BASE,
    VmcsGuestNW::FS_BASE,
    VmcsGuestNW::GS_BASE,
    VmcsGuestNW::LDTR_BASE,
    VmcsGuestNW::TR_BASE,
    VmcsGuestNW::GDTR_BASE,
    VmcsGuestNW::IDTR_BASE,
];
const LIMITS: [VmcsGuest32; 10] = [
    VmcsGuest32::ES_LIMIT,
    VmcsGuest32::CS_LIMIT,
    VmcsGuest32::SS_LIMIT,
    VmcsGuest32::DS_LIMIT,
    VmcsGuest32::FS_LIMIT,
    VmcsGuest32::GS_LIMIT,
    VmcsGuest32::LDTR_LIMIT,
    VmcsGuest32::TR_LIMIT,
    VmcsGuest32::GDTR_LIMIT,
    VmcsGuest32::IDTR_LIMIT,
];
const ACCESS: [VmcsGuest32; 8] = [
    VmcsGuest32::ES_ACCESS_RIGHTS,
    VmcsGuest32::CS_ACCESS_RIGHTS,
    VmcsGuest32::SS_ACCESS_RIGHTS,
    VmcsGuest32::DS_ACCESS_RIGHTS,
    VmcsGuest32::FS_ACCESS_RIGHTS,
    VmcsGuest32::GS_ACCESS_RIGHTS,
    VmcsGuest32::LDTR_ACCESS_RIGHTS,
    VmcsGuest32::TR_ACCESS_RIGHTS,
];

impl VmxVcpu {
    pub(super) fn read_control_reg(&self, key: u64) -> AxResult<u64> {
        match key {
            4 => Ok(self.stack_pointer() as u64),
            0..=15 => Ok(self.regs().get_reg_of_index(key as u8)),
            reg::RIP => Ok(self.rip() as u64),
            reg::RFLAGS => Ok(VmcsGuestNW::RFLAGS.read()? as u64),
            reg::CR0 => Ok(self.cr(0) as u64),
            reg::CR2 => Ok(self.guest_cr2),
            reg::CR3 => Ok(VmcsGuestNW::CR3.read()? as u64),
            reg::CR4 => Ok(self.cr(4) as u64),
            reg::EFER => VmcsGuest64::IA32_EFER.read(),
            reg::APIC_BASE => Ok(self.vlapic.apic_base()),
            reg::INTERRUPT_SHADOW => {
                Ok((VmcsGuest32::INTERRUPTIBILITY_STATE.read()? & 3 != 0) as u64)
            }
            reg::DR0..=reg::DR3 => Ok(self.debugregs.guest_db[(key - reg::DR0) as usize]),
            reg::DR6 => Ok(self.guest_dr6),
            reg::DR7 => Ok(VmcsGuestNW::DR7.read()? as u64),
            reg::ES..=0x127 => {
                let index = ((key - reg::ES) / 4) as usize;
                match (key - reg::ES) % 4 {
                    0 if index < 8 => Ok(SELECTORS[index].read()? as u64),
                    reg::BASE => Ok(BASES[index].read()? as u64),
                    reg::LIMIT => Ok(LIMITS[index].read()? as u64),
                    reg::ACCESS if index < 8 => Ok(ACCESS[index].read()? as u64),
                    _ => Err(AxError::Unsupported),
                }
            }
            _ => Err(AxError::Unsupported),
        }
    }

    pub(super) fn write_control_reg(&mut self, key: u64, value: u64) -> AxResult {
        match key {
            4 => self.set_stack_pointer(value as usize),
            0..=15 => self.regs_mut().set_reg_of_index(key as u8, value),
            reg::RIP => return self.set_rip(value as usize),
            reg::RFLAGS => return VmcsGuestNW::RFLAGS.write(value as usize),
            reg::CR0 | reg::CR3 | reg::CR4 => {
                self.set_cr((key - reg::CR0) as usize, value);
                if key == reg::CR0 {
                    return self.update_guest_efer();
                }
            }
            reg::CR2 => self.guest_cr2 = value,
            reg::APIC_BASE => return self.vlapic.set_apic_base(value),
            reg::EFER => {
                VmcsGuest64::IA32_EFER.write(value)?;
                return self.update_guest_efer();
            }
            reg::INTERRUPT_SHADOW => {
                if value > 1 {
                    return Err(AxError::InvalidInput);
                }
                let state = VmcsGuest32::INTERRUPTIBILITY_STATE.read()?;
                return VmcsGuest32::INTERRUPTIBILITY_STATE.write((state & !3) | value as u32);
            }
            reg::DR0..=reg::DR3 => self.debugregs.guest_db[(key - reg::DR0) as usize] = value,
            reg::DR6 => self.guest_dr6 = value,
            reg::DR7 => return VmcsGuestNW::DR7.write(value as usize),
            reg::ES..=0x127 => {
                let index = ((key - reg::ES) / 4) as usize;
                return match (key - reg::ES) % 4 {
                    0 if index < 8 => {
                        SELECTORS[index].write(value.try_into().map_err(|_| AxError::InvalidInput)?)
                    }
                    reg::BASE => BASES[index].write(value as usize),
                    reg::LIMIT => {
                        LIMITS[index].write(value.try_into().map_err(|_| AxError::InvalidInput)?)
                    }
                    reg::ACCESS if index < 8 => {
                        ACCESS[index].write(value.try_into().map_err(|_| AxError::InvalidInput)?)
                    }
                    _ => Err(AxError::Unsupported),
                };
            }
            _ => return Err(AxError::Unsupported),
        }
        Ok(())
    }
}
