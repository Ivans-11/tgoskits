use alloc::{vec, vec::Vec};
use core::arch::{asm, x86_64::__cpuid_count};

#[derive(Clone)]
#[repr(C, align(64))]
struct Block([u8; 64]);

/// VMX does not switch floating-point/SIMD registers. Keep each guest's image
/// separate from the host thread that enters it, including across KVM_RUN calls.
pub(super) struct FpState {
    host: Vec<Block>,
    guest: Vec<Block>,
    xsave: bool,
    host_cr0: usize,
}

impl FpState {
    pub(super) fn new() -> Self {
        let xsave = crate::xstate::xsave_available();
        let size = if xsave {
            // ECX includes all supported user-state components, even if the
            // guest enables additional XCR0 bits after its first entry.
            (__cpuid_count(0xd, 0).ecx as usize).max(576)
        } else {
            512
        };
        let blocks = size.div_ceil(64);
        let host = vec![Block([0; 64]); blocks];
        let mut guest = host.clone();
        guest[0].0[0..2].copy_from_slice(&0x37fu16.to_le_bytes());
        guest[0].0[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
        Self {
            host,
            guest,
            xsave,
            host_cr0: 0,
        }
    }

    unsafe fn save(&self, ptr: *mut Block) {
        unsafe {
            if self.xsave {
                asm!("xsave64 [{}]", in(reg) ptr, in("eax") u32::MAX,
                     in("edx") u32::MAX, options(nostack));
            } else {
                asm!("fxsave64 [{}]", in(reg) ptr, options(nostack));
            }
        }
    }

    unsafe fn restore(&self, ptr: *const Block) {
        unsafe {
            if self.xsave {
                asm!("xrstor64 [{}]", in(reg) ptr, in("eax") u32::MAX,
                     in("edx") u32::MAX, options(nostack));
            } else {
                asm!("fxrstor64 [{}]", in(reg) ptr, options(nostack));
            }
        }
    }

    pub(super) fn save_host(&mut self) {
        unsafe {
            asm!("mov {}, cr0", out(reg) self.host_cr0, options(nomem, nostack));
            // A host may lazily disable FPU instructions with TS. Restore its
            // exact CR0 once its register image has been restored.
            asm!("clts", options(nomem, nostack));
            let ptr = self.host.as_mut_ptr();
            self.save(ptr);
        }
    }

    pub(super) fn restore_guest(&self) {
        unsafe { self.restore(self.guest.as_ptr()) };
    }

    pub(super) fn save_guest(&mut self) {
        let ptr = self.guest.as_mut_ptr();
        unsafe { self.save(ptr) };
    }

    pub(super) fn restore_host(&self) {
        unsafe {
            self.restore(self.host.as_ptr());
            asm!("mov cr0, {}", in(reg) self.host_cr0, options(nomem, nostack));
        }
    }
}
