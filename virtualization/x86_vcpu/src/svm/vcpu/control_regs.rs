// SPDX-License-Identifier: Apache-2.0

use ax_errno::{AxError, AxResult};
use axvcpu::x86_regs as reg;
use tock_registers::interfaces::{Readable, Writeable};

use super::SvmVcpu;

impl SvmVcpu {
    pub(super) fn read_control_reg(&self, key: u64) -> AxResult<u64> {
        let state = &unsafe { self.vmcb.as_vmcb_ref() }.state;
        match key {
            4 => Ok(state.rsp.get()),
            0..=15 => Ok(self.regs().get_reg_of_index(key as u8)),
            reg::RIP => Ok(state.rip.get()),
            reg::RFLAGS => Ok(state.rflags.get()),
            reg::CR0 => Ok(state.cr0.get()),
            reg::CR2 => Ok(state.cr2.get()),
            reg::CR3 => Ok(state.cr3.get()),
            reg::CR4 => Ok(state.cr4.get()),
            reg::EFER => Ok(self.guest_visible_efer()),
            reg::APIC_BASE => Ok(self.vlapic.apic_base()),
            reg::INTERRUPT_SHADOW => {
                Ok((unsafe { self.vmcb.as_vmcb_ref() }.control.int_state.get() & 1) as u64)
            }
            reg::DR0..=reg::DR3 => Ok(self.debugregs.guest_db[(key - reg::DR0) as usize]),
            reg::DR6 => Ok(state.dr6.get()),
            reg::DR7 => Ok(state.dr7.get()),
            reg::ES..=0x127 => {
                let segments = [
                    &state.es,
                    &state.cs,
                    &state.ss,
                    &state.ds,
                    &state.fs,
                    &state.gs,
                    &state.ldtr,
                    &state.tr,
                    &state.gdtr,
                    &state.idtr,
                ];
                let index = ((key - reg::ES) / 4) as usize;
                let segment = segments[index];
                match (key - reg::ES) % 4 {
                    0 if index < 8 => Ok(segment.selector.get() as u64),
                    reg::BASE => Ok(segment.base.get()),
                    reg::LIMIT => Ok(segment.limit.get() as u64),
                    reg::ACCESS if index < 8 => {
                        let attr = segment.attr.get() as u64;
                        Ok((attr & 0xff)
                            | ((attr & 0xf00) << 4)
                            | (u64::from(attr & 0x80 == 0) << 16))
                    }
                    _ => Err(AxError::Unsupported),
                }
            }
            _ => Err(AxError::Unsupported),
        }
    }

    pub(super) fn write_control_reg(&mut self, key: u64, value: u64) -> AxResult {
        match key {
            4 => unsafe { self.vmcb.as_vmcb().state.rsp.set(value) },
            0..=15 => self.regs_mut().set_reg_of_index(key as u8, value),
            reg::RIP => self.set_rip(value),
            reg::RFLAGS => unsafe { self.vmcb.as_vmcb().state.rflags.set(value) },
            reg::CR0 | reg::CR3 | reg::CR4 => return self.set_cr((key - reg::CR0) as usize, value),
            reg::CR2 => unsafe { self.vmcb.as_vmcb().state.cr2.set(value) },
            reg::EFER => self.set_guest_efer(value),
            reg::APIC_BASE => return self.vlapic.set_apic_base(value),
            reg::INTERRUPT_SHADOW => {
                if value > 1 {
                    return Err(AxError::InvalidInput);
                }
                let field = &unsafe { self.vmcb.as_vmcb() }.control.int_state;
                field.set((field.get() & !1) | value as u32);
            }
            reg::DR0..=reg::DR3 => self.debugregs.guest_db[(key - reg::DR0) as usize] = value,
            reg::DR6 => unsafe { self.vmcb.as_vmcb().state.dr6.set(value) },
            reg::DR7 => unsafe { self.vmcb.as_vmcb().state.dr7.set(value) },
            reg::ES..=0x127 => {
                let state = &mut unsafe { self.vmcb.as_vmcb() }.state;
                let segments = [
                    &state.es,
                    &state.cs,
                    &state.ss,
                    &state.ds,
                    &state.fs,
                    &state.gs,
                    &state.ldtr,
                    &state.tr,
                    &state.gdtr,
                    &state.idtr,
                ];
                let index = ((key - reg::ES) / 4) as usize;
                let segment = segments[index];
                match (key - reg::ES) % 4 {
                    0 if index < 8 => segment
                        .selector
                        .set(value.try_into().map_err(|_| AxError::InvalidInput)?),
                    reg::BASE => segment.base.set(value),
                    reg::LIMIT => segment
                        .limit
                        .set(value.try_into().map_err(|_| AxError::InvalidInput)?),
                    reg::ACCESS if index < 8 => {
                        let mut attr = ((value & 0xff) | ((value >> 4) & 0xf00)) as u16;
                        if value & (1 << 16) != 0 {
                            attr &= !0x80;
                        }
                        segment.attr.set(attr);
                    }
                    _ => return Err(AxError::Unsupported),
                }
            }
            _ => return Err(AxError::Unsupported),
        }
        Ok(())
    }
}
