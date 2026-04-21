use ax_plat::mem::{Aligned4K, pa};

use crate::config::plat::{BOOT_STACK_SIZE, PHYS_VIRT_OFFSET};

#[unsafe(link_section = ".bss.stack")]
static mut BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];

#[unsafe(link_section = ".data")]
static mut BOOT_PT_SV39: Aligned4K<[u64; 512]> = Aligned4K::new([0; 512]);

#[allow(clippy::identity_op)]
unsafe fn init_boot_page_table() {
    unsafe {
        BOOT_PT_SV39[0] = (0x0 << 10) | 0xef;
        BOOT_PT_SV39[2] = (0x80000 << 10) | 0xef;
        BOOT_PT_SV39[0x100] = (0x0 << 10) | 0xef;
        BOOT_PT_SV39[0x102] = (0x80000 << 10) | 0xef;
    }
}

unsafe fn init_mmu() {
    unsafe {
        ax_cpu::asm::write_kernel_page_table(pa!(&raw const BOOT_PT_SV39 as usize));
        ax_cpu::asm::flush_tlb(None);
    }
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!("
        mv      s0, a0
        mv      s1, a1
        la      sp, {boot_stack}
        li      t0, {boot_stack_size}
        add     sp, sp, t0

        call    {init_boot_page_table}
        call    {init_mmu}

        li      s2, {phys_virt_offset}
        add     sp, sp, s2

        mv      a0, s0
        mv      a1, s1
        la      a2, {entry}
        add     a2, a2, s2
        jalr    a2
        j       .",
        phys_virt_offset = const PHYS_VIRT_OFFSET,
        boot_stack_size = const BOOT_STACK_SIZE,
        boot_stack = sym BOOT_STACK,
        init_boot_page_table = sym init_boot_page_table,
        init_mmu = sym init_mmu,
        entry = sym ax_plat::call_main,
    )
}

#[cfg(feature = "smp")]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn _start_secondary() -> ! {
    core::arch::naked_asm!("
        mv      s0, a0
        mv      sp, a1

        call    {init_mmu}

        li      s1, {phys_virt_offset}
        add     a1, a1, s1
        add     sp, sp, s1

        mv      a0, s0
        la      a1, {entry}
        add     a1, a1, s1
        jalr    a1
        j       .",
        phys_virt_offset = const PHYS_VIRT_OFFSET,
        init_mmu = sym init_mmu,
        entry = sym ax_plat::call_secondary_main,
    )
}
