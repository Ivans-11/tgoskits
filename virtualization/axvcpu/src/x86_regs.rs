// SPDX-License-Identifier: Apache-2.0

//! x86 scalar register keys for `get_arch_reg` and `set_arch_reg`.
//!
//! GPR keys 0..16 use x86 instruction encoding order, including RSP at 4.
//! Segment keys use a four-field record: selector, base, limit, access rights.
//! Access rights use the VMCS architectural layout (including unusable bit 16).

pub const RIP: u64 = 0x10;
pub const RFLAGS: u64 = 0x11;
pub const CR0: u64 = 0x20;
pub const CR2: u64 = 0x22;
pub const CR3: u64 = 0x23;
pub const CR4: u64 = 0x24;
pub const EFER: u64 = 0x25;
pub const INTERRUPT_SHADOW: u64 = 0x26;
pub const APIC_BASE: u64 = 0x29;
pub const DR0: u64 = 0x30;
pub const DR1: u64 = 0x31;
pub const DR2: u64 = 0x32;
pub const DR3: u64 = 0x33;
pub const DR6: u64 = 0x36;
pub const DR7: u64 = 0x37;
pub const ES: u64 = 0x100;
pub const CS: u64 = 0x104;
pub const SS: u64 = 0x108;
pub const DS: u64 = 0x10c;
pub const FS: u64 = 0x110;
pub const GS: u64 = 0x114;
pub const LDTR: u64 = 0x118;
pub const TR: u64 = 0x11c;
pub const GDTR: u64 = 0x120;
pub const IDTR: u64 = 0x124;
pub const BASE: u64 = 1;
pub const LIMIT: u64 = 2;
pub const ACCESS: u64 = 3;

/// Instruction context saved before the backend advances RIP on VM exit.
#[derive(Debug, Clone, Copy)]
pub struct ExitInstruction {
    pub rip: u64,
    pub len: u32,
}
