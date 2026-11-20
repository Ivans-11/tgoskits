// SPDX-License-Identifier: Apache-2.0

//! FreeBSD releng/14.5 amd64 vmm device ABI.
//!
//! FreeBSD ioctl direction bits intentionally differ from Linux's encoding.

use ax_errno::{AxError, AxResult};
use axvcpu::x86_regs as reg;
use axvisor_api::control;

pub(super) const CREATE: u32 = 0x8100_4201;
pub(super) const DESTROY: u32 = 0x8100_4202;
pub(super) const ALLOC_MEMSEG: u32 = 0x8020_760e;
pub(super) const GET_MEMSEG: u32 = 0xc020_760f;
pub(super) const MMAP_MEMSEG: u32 = 0x8028_7610;
pub(super) const MMAP_GETNEXT: u32 = 0xc028_7611;
pub(super) const MUNMAP_MEMSEG: u32 = 0x8010_7613;
pub(super) const RUN: u32 = 0x8020_7601;
pub(super) const IOAPIC_ASSERT_IRQ: u32 = 0x8004_7621;
pub(super) const IOAPIC_DEASSERT_IRQ: u32 = 0x8004_7622;
pub(super) const IOAPIC_PULSE_IRQ: u32 = 0x8004_7623;
pub(super) const IOAPIC_PINCOUNT: u32 = 0x4004_7626;
pub(super) const ISA_ASSERT_IRQ: u32 = 0x8008_7650;
pub(super) const ISA_DEASSERT_IRQ: u32 = 0x8008_7651;
pub(super) const ISA_PULSE_IRQ: u32 = 0x8008_7652;
pub(super) const SET_REGISTER: u32 = 0x8010_7614;
pub(super) const GET_REGISTER: u32 = 0xc010_7615;
pub(super) const SET_SEGMENT: u32 = 0x8018_7616;
pub(super) const GET_SEGMENT: u32 = 0xc018_7617;
pub(super) const SET_REGISTER_SET: u32 = 0x8018_7618;
pub(super) const GET_REGISTER_SET: u32 = 0xc018_7619;
pub(super) const SET_CAPABILITY: u32 = 0x8010_7602;
pub(super) const GET_CAPABILITY: u32 = 0xc010_7603;
pub(super) const SUSPEND: u32 = 0x8004_7604;
pub(super) const SET_X2APIC: u32 = 0x8008_763c;
pub(super) const GET_X2APIC: u32 = 0xc008_763d;
pub(super) const SET_TOPOLOGY: u32 = 0x8008_763f;
pub(super) const GET_TOPOLOGY: u32 = 0x4008_7640;
pub(super) const ACTIVATE_CPU: u32 = 0x8004_765a;
pub(super) const GET_CPUS: u32 = 0x8010_765b;
pub(super) const SUSPEND_CPU: u32 = 0x8004_765c;
pub(super) const RESUME_CPU: u32 = 0x8004_765d;
pub(super) const RTC_READ: u32 = 0xc008_7664;
pub(super) const RTC_WRITE: u32 = 0x8008_7665;
pub(super) const RTC_SETTIME: u32 = 0x8008_7666;
pub(super) const RTC_GETTIME: u32 = 0x4008_7667;

// Values from FreeBSD's machine/vmm.h.
pub(super) mod capability {
    pub const HALT_EXIT: u32 = 0;
    pub const UNRESTRICTED_GUEST: u32 = 3;
    pub const IPI_EXIT: u32 = 8;
}

pub(super) mod exitcode {
    pub const INOUT: u32 = 0;
    pub const HLT: u32 = 5;
    pub const SUSPENDED: u32 = 14;
    pub const IPI: u32 = 24;
}

pub(super) const SUSPEND_HALT: u32 = 4;
pub(super) const INOUT_IN: u16 = 1 << 3;

/// Encodes `struct vm_exit`, including the unused union bytes and padding.
pub(super) fn write_exit(
    address: usize,
    code: u32,
    instruction: reg::ExitInstruction,
    payload: [u8; 8],
) -> AxResult {
    // The union's largest member is inst_emul (120 bytes). Supported exits
    // use at most its first eight bytes; the 16-byte header precedes it.
    let mut output = [0u8; 136];
    output[..4].copy_from_slice(&code.to_le_bytes());
    output[4..8].copy_from_slice(&instruction.len.to_le_bytes());
    output[8..16].copy_from_slice(&instruction.rip.to_le_bytes());
    output[16..24].copy_from_slice(&payload);
    control::copy_to_user(address, &output)
}

pub(super) fn read<const N: usize>(address: usize) -> AxResult<[u8; N]> {
    let mut bytes = [0; N];
    control::copy_from_user(address, &mut bytes)?;
    Ok(bytes)
}

/// Copies a fixed ioctl argument according to FreeBSD's IOC_IN/IOC_OUT bits.
pub(super) fn ioctl<const N: usize>(
    command: u32,
    address: usize,
    operation: impl FnOnce(&mut [u8; N]) -> AxResult,
) -> AxResult {
    let mut data = if command & 0x8000_0000 != 0 {
        read(address)?
    } else {
        [0; N]
    };
    operation(&mut data)?;
    if command & 0x4000_0000 != 0 {
        control::copy_to_user(address, &data)?;
    }
    Ok(())
}

pub(super) fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

pub(super) fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

pub(super) fn write_cpuset(address: usize, len: usize, mask: u64) -> AxResult {
    if len == 0 || len > 128 || len < ((64 - mask.leading_zeros()) as usize).div_ceil(8) {
        return Err(AxError::InvalidInput);
    }
    let mut data = [0u8; 128];
    data[..8].copy_from_slice(&mask.to_le_bytes());
    control::copy_to_user(address, &data[..len])
}

pub(super) fn name(bytes: &[u8]) -> AxResult<&str> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(AxError::InvalidInput)?;
    let name = core::str::from_utf8(&bytes[..end]).map_err(|_| AxError::InvalidInput)?;
    if name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(AxError::InvalidInput);
    }
    Ok(name)
}

pub(super) fn register(number: u32) -> AxResult<u64> {
    const REGISTERS: [u64; 34] = [
        0,
        3,
        1,
        2,
        6,
        7,
        5,
        8,
        9,
        10,
        11,
        12,
        13,
        14,
        15,
        reg::CR0,
        reg::CR3,
        reg::CR4,
        reg::DR7,
        4,
        reg::RIP,
        reg::RFLAGS,
        reg::ES,
        reg::CS,
        reg::SS,
        reg::DS,
        reg::FS,
        reg::GS,
        reg::LDTR,
        reg::TR,
        reg::IDTR,
        reg::GDTR,
        reg::EFER,
        reg::CR2,
    ];
    match number {
        0..=33 => Ok(REGISTERS[number as usize]),
        38 => Ok(reg::INTERRUPT_SHADOW),
        39..=42 => Ok(reg::DR0 + u64::from(number - 39)),
        43 => Ok(reg::DR6),
        45 => Ok(reg::FS + reg::BASE),
        46 => Ok(reg::GS + reg::BASE),
        _ => Err(AxError::Unsupported),
    }
}
