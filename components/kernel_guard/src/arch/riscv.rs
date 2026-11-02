use core::arch::asm;

const STATUS: usize = if cfg!(feature = "m-mode") {
    0x300
} else {
    0x100
};
const IRQ_ENABLE: usize = if cfg!(feature = "m-mode") {
    1 << 3
} else {
    1 << 1
};

#[inline]
pub fn local_irq_save_and_disable() -> usize {
    let flags: usize;
    unsafe { asm!("csrrc {}, {}, {}", out(reg) flags, const STATUS, const IRQ_ENABLE) };
    flags & IRQ_ENABLE
}

#[inline]
pub fn local_irq_restore(flags: usize) {
    unsafe { asm!("csrrs x0, {}, {}", const STATUS, in(reg) flags) };
}
