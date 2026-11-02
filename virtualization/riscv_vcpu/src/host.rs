// SPDX-License-Identifier: Apache-2.0
//! Build-time selection of the host trap bank. Guest VS state is unchanged.

use core::arch::asm;

pub(crate) const MACHINE: bool = cfg!(feature = "m-mode");
const BANK: usize = if MACHINE { 0x200 } else { 0 };
pub(crate) const STATUS: usize = 0x100 + BANK;
pub(crate) const IE: usize = 0x104 + BANK;
pub(crate) const TVEC: usize = 0x105 + BANK;
pub(crate) const SCRATCH: usize = 0x140 + BANK;
pub(crate) const EPC: usize = 0x141 + BANK;
pub(crate) const CAUSE: usize = 0x142 + BANK;
pub(crate) const TVAL: usize = 0x143 + BANK;
pub(crate) const TVAL2: usize = if MACHINE { 0x34b } else { 0x643 };
pub(crate) const TINST: usize = if MACHINE { 0x34a } else { 0x64a };
const IRQ_ENABLE: usize = if MACHINE { 1 << 3 } else { 1 << 1 };

#[inline]
pub(crate) fn irq_save() -> usize {
    let flags;
    unsafe { asm!("csrrc {}, {}, {}", out(reg) flags, const STATUS, const IRQ_ENABLE) };
    flags
}

#[inline]
pub(crate) unsafe fn irq_restore(flags: usize) {
    unsafe { asm!("csrs {}, {}", const STATUS, in(reg) flags & IRQ_ENABLE) };
}

#[cfg(feature = "m-mode")]
#[derive(Default)]
pub(crate) struct MachineState {
    medeleg: usize,
    mideleg: usize,
    mcounteren: usize,
    menvcfg: usize,
}

#[cfg(feature = "m-mode")]
impl MachineState {
    // The M-mode host owns PMP and must grant access to the guest RAM pool.
    // Only exceptions delegated all the way to VS bypass the core's M trap.
    pub(crate) unsafe fn bind(&mut self) {
        unsafe {
            asm!(
                "csrr {md}, medeleg", "csrr {mi}, mideleg",
                "csrr {mc}, mcounteren", "csrr {me}, menvcfg",
                md = out(reg) self.medeleg, mi = out(reg) self.mideleg,
                mc = out(reg) self.mcounteren, me = out(reg) self.menvcfg,
            );
            let env = self.menvcfg
                | crate::regs::GUEST_HENVCFG
                | if cfg!(feature = "sstc") {
                    1usize << 63
                } else {
                    0
                };
            asm!(
                "csrw medeleg, {md}", "csrw mideleg, {mi}",
                "csrw mcounteren, {mc}", "csrw menvcfg, {me}",
                md = in(reg) riscv_h::register::hedeleg::read().bits(),
                mi = in(reg) riscv_h::register::hideleg::read().bits(),
                mc = in(reg) self.mcounteren | 7, me = in(reg) env,
            );
        }
    }

    pub(crate) unsafe fn unbind(&self) {
        unsafe {
            asm!(
                "csrw medeleg, {md}", "csrw mideleg, {mi}",
                "csrw mcounteren, {mc}", "csrw menvcfg, {me}",
                md = in(reg) self.medeleg, mi = in(reg) self.mideleg,
                mc = in(reg) self.mcounteren, me = in(reg) self.menvcfg,
            );
        }
    }
}
