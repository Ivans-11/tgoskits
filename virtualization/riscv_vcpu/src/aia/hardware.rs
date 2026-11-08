//! Per-hart IMSIC guest file owned by the core for its entire lifetime.

use crate::{detect, host};

#[derive(Clone, Copy)]
pub(super) struct GuestFile {
    pub index: usize,
    pub address: usize,
    pub ids: usize,
}

#[ax_percpu::def_percpu]
static FILE: Option<GuestFile> = None;
#[ax_percpu::def_percpu]
static BUSY: bool = false;

/// Probes and registers guest file 1 on the current hart.
///
/// The caller runs once on a pinned CPU during initialization, after setting up
/// core per-CPU storage. The host reserves this file exclusively for the core
/// and provides a permanent device mapping. No guest state is resident yet.
pub fn init_on_cpu(address: usize, ids: usize) -> bool {
    if address == 0 || address & 4095 != 0 || !(63..=2047).contains(&ids) || ids & 63 != 63 {
        return false;
    }
    let flags = host::irq_save();
    let initialized = if FILE.read_current().is_some()
        || !detect::detect_h_extension()
        || !detect::detect_aia()
    {
        false
    } else {
        let (old, available): (usize, usize);
        // SAFETY: H/AIA are present, this CPU is pinned with interrupts disabled,
        // and the previous interrupt-enable register is restored immediately.
        unsafe {
            core::arch::asm!("csrr {old}, 0x607", old = out(reg) old);
            core::arch::asm!("csrw 0x607, {value}", value = in(reg) old | 2);
            core::arch::asm!("csrr {value}, 0x607", value = out(reg) available);
            core::arch::asm!("csrw 0x607, {value}", value = in(reg) old);
        }
        if available & 2 != 0 && old & 2 == 0 {
            FILE.write_current(Some(GuestFile {
                index: 1,
                address,
                ids,
            }));
            true
        } else {
            false
        }
    };
    // SAFETY: restores the interrupt state saved above on the same CPU.
    unsafe { host::irq_restore(flags) };
    initialized
}

pub(super) fn acquire() -> Option<GuestFile> {
    // Entry and exit run with interrupts and migration disabled; producers are
    // excluded by the owning virtual file's lock, not this per-CPU flag.
    let file = FILE.read_current()?;
    if BUSY.read_current() {
        return None;
    }
    BUSY.write_current(true);
    Some(file)
}

pub(super) fn release(file: GuestFile) {
    debug_assert_eq!(file.address, FILE.read_current().unwrap().address);
    debug_assert!(BUSY.read_current());
    BUSY.write_current(false);
}
