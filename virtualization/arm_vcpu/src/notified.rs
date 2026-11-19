//! Retry RAM aborts before considering MMIO. Capture syndrome before restoring
//! the host; decoding may take place after a reschedule on a different CPU.
use ax_errno::{AxError, AxResult};
use axaddrspace::{GuestPhysAddr, MappingFlags, device::AccessWidth};
use axvcpu::AxVCpuExitReason;

use crate::{
    TrapFrame,
    exception_utils::{exception_esr, exception_fault_addr},
};

#[derive(Debug)]
pub(crate) struct GuestAbort {
    esr: usize,
    ipa: GuestPhysAddr,
}
impl GuestAbort {
    pub(crate) fn capture() -> AxResult<Option<Self>> {
        let esr = exception_esr();
        let ec = esr >> 26;
        if ec != 0x20 && ec != 0x24 {
            return Ok(None);
        }
        // Address-size, translation, access-flag and permission faults are
        // reported without advancing the faulting PC or guessing MMIO.
        Ok(Some(Self {
            esr,
            ipa: exception_fault_addr()?,
        }))
    }
    pub(crate) fn exit(&self) -> AxVCpuExitReason {
        let flags = if self.esr >> 26 == 0x20 && self.esr & (1 << 7) == 0 {
            MappingFlags::EXECUTE
        } else if self.esr & (1 << 6) != 0 {
            MappingFlags::WRITE
        } else {
            MappingFlags::READ
        };
        AxVCpuExitReason::NestedPageFault {
            addr: self.ipa,
            access_flags: flags,
        }
    }
    pub(crate) fn decode(self, ctx: &mut TrapFrame) -> AxResult<Option<AxVCpuExitReason>> {
        // Instruction fetch and stage-1 table walks are never MMIO operations.
        if self.esr >> 26 != 0x24 || self.esr & (1 << 7) != 0 {
            return Ok(None);
        }
        if self.esr & (1 << 24) == 0 {
            return Err(AxError::Unsupported);
        }
        let width = AccessWidth::try_from(1usize << ((self.esr >> 22) & 3))
            .map_err(|_| AxError::InvalidInput)?;
        let reg = (self.esr >> 16) & 31;
        let reg_width = if self.esr & (1 << 15) != 0 {
            AccessWidth::Qword
        } else {
            AccessWidth::Dword
        };
        let exit = if self.esr & (1 << 6) != 0 {
            AxVCpuExitReason::MmioWrite {
                addr: self.ipa,
                width,
                data: if reg == 31 { 0 } else { ctx.gpr(reg) as u64 },
            }
        } else {
            AxVCpuExitReason::MmioRead {
                addr: self.ipa,
                width,
                reg,
                reg_width,
                signed_ext: self.esr & (1 << 21) != 0,
            }
        };
        ctx.set_exception_pc(ctx.exception_pc() + 4);
        Ok(Some(exit))
    }
}
