use alloc::alloc::{alloc_zeroed, dealloc, handle_alloc_error};
use core::{alloc::Layout, arch::asm, ptr::NonNull};

use raw_cpuid::CpuId;
use x86::controlregs::{Xcr0, xcr0_write};
use x86_64::registers::control::{Cr4, Cr4Flags};

use crate::msr::Msr;

// The x86 crate's xcr0() truncates bits unknown to its Xcr0 flags, including
// AMX. Preserve all bits so the host can restore its existing XSAVE images.
unsafe fn read_xcr0() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: callers check XSAVE support and run with OSXSAVE enabled.
    unsafe {
        asm!("xgetbv", in("ecx") 0u32, out("eax") low, out("edx") high,
             options(nomem, nostack, preserves_flags));
    }
    u64::from(low) | (u64::from(high) << 32)
}

#[derive(Debug)]
struct XsaveBuffer {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl XsaveBuffer {
    fn new(size: usize) -> Self {
        let layout = Layout::from_size_align(size, 64).expect("invalid XSAVE area layout");
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout));
        Self { ptr, layout }
    }

    fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Drop for XsaveBuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

// The buffer is owned by its vCPU and is accessed only while that vCPU is
// exclusively locked by the caller.
unsafe impl Send for XsaveBuffer {}

unsafe fn save_xstate(buffer: &XsaveBuffer, mask: u64) {
    unsafe {
        asm!(
            "xsave64 [{buffer}]",
            buffer = in(reg) buffer.as_ptr(),
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack),
        );
    }
}

unsafe fn restore_xstate(buffer: &XsaveBuffer, mask: u64) {
    unsafe {
        asm!(
            "xrstor64 [{buffer}]",
            buffer = in(reg) buffer.as_ptr(),
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack),
        );
    }
}

/// Extended processor state switched between host and guest.
#[derive(Debug)]
pub struct XState {
    pub guest_xcr0: u64,

    host_xcr0: u64,
    host_xss: u64,
    guest_xss: u64,
    xsave_available: bool,
    xsaves_available: bool,
    host_state: Option<XsaveBuffer>,
    guest_state: Option<XsaveBuffer>,
}

impl XState {
    pub fn new() -> Self {
        let xsave_available = xsave_available();
        let xsaves_supported = xsave_available && xsaves_available();
        let xcr0 = if xsave_available {
            unsafe { read_xcr0() }
        } else {
            0
        };
        let xss = if xsaves_supported {
            Msr::IA32_XSS.read()
        } else {
            0
        };
        let (host_state, guest_state) = if xsave_available {
            let leaf = core::arch::x86_64::__cpuid_count(0x0d, 0);
            let size = (leaf.ecx as usize).max(576);
            (Some(XsaveBuffer::new(size)), Some(XsaveBuffer::new(size)))
        } else {
            (None, None)
        };

        Self {
            host_xcr0: xcr0,
            guest_xcr0: xcr0,
            host_xss: xss,
            guest_xss: xss,
            xsave_available,
            xsaves_available: xsaves_supported,
            host_state,
            guest_state,
        }
    }

    pub fn switch_to_guest(&mut self) {
        unsafe {
            if self.xsave_available {
                self.host_xcr0 = read_xcr0();
                save_xstate(self.host_state.as_ref().unwrap(), self.host_xcr0);
                // Like KVM, avoid a serializing write (and a possible nested
                // VM exit) when the hardware already has the required value.
                // Still sample every switch: host state can change between
                // runs, and all XCR0 bits, including AMX, must be preserved.
                if self.host_xcr0 != self.guest_xcr0 {
                    xcr0_write(Xcr0::from_bits_unchecked(self.guest_xcr0));
                }

                if self.xsaves_available {
                    self.host_xss = Msr::IA32_XSS.read();
                    if self.host_xss != self.guest_xss {
                        Msr::IA32_XSS.write(self.guest_xss);
                    }
                }
                restore_xstate(self.guest_state.as_ref().unwrap(), self.guest_xcr0);
            }
        }
    }

    pub fn switch_to_host(&mut self) {
        unsafe {
            if self.xsave_available {
                self.guest_xcr0 = read_xcr0();
                save_xstate(self.guest_state.as_ref().unwrap(), self.guest_xcr0);
                if self.guest_xcr0 != self.host_xcr0 {
                    xcr0_write(Xcr0::from_bits_unchecked(self.host_xcr0));
                }

                if self.xsaves_available {
                    self.guest_xss = Msr::IA32_XSS.read();
                    if self.guest_xss != self.host_xss {
                        Msr::IA32_XSS.write(self.host_xss);
                    }
                }
                restore_xstate(self.host_state.as_ref().unwrap(), self.host_xcr0);
            }
        }
    }
}

pub fn xsave_available() -> bool {
    CpuId::new()
        .get_feature_info()
        .map(|features| features.has_xsave())
        .unwrap_or(false)
}

pub fn xsaves_available() -> bool {
    CpuId::new()
        .get_extended_state_info()
        .map(|features| features.has_xsaves_xrstors())
        .unwrap_or(false)
}

pub fn enable_xsave() {
    if xsave_available() {
        unsafe {
            Cr4::write(Cr4::read() | Cr4Flags::OSXSAVE);
        }
    }
}
