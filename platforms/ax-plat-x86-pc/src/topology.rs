//! Firmware CPU enumeration and dense logical CPU to hardware APIC ID mapping.

use ax_lazyinit::LazyInit;
use heapless::Vec;

use crate::bootinfo::{physical_bytes, u32_at, u64_at};

static APIC_IDS: LazyInit<Vec<u32, { crate::config::plat::MAX_CPU_NUM }>> = LazyInit::new();

fn hardware_id() -> u32 {
    // CPUID.0BH supplies the full x2APIC ID, even before enabling x2APIC.
    let cpuid = raw_cpuid::CpuId::new();
    if let Some(mut topology) = cpuid.get_extended_topology_info()
        && let Some(level) = topology.next()
    {
        return level.x2apic_id();
    }
    cpuid.get_feature_info().unwrap().initial_local_apic_id() as u32
}

fn checksum(data: &[u8]) -> bool {
    data.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

fn table(address: usize) -> Option<&'static [u8]> {
    let header = physical_bytes(address, 36)?;
    let length = u32_at(header, 4) as usize;
    if !(36..=1024 * 1024).contains(&length) {
        return None;
    }
    let data = physical_bytes(address, length)?;
    checksum(data).then_some(data)
}

fn madt(rsdp: &[u8]) -> Option<&'static [u8]> {
    if rsdp.len() < 20 || &rsdp[..8] != b"RSD PTR " || !checksum(&rsdp[..20]) {
        return None;
    }
    let (root, stride) = if rsdp[15] >= 2 && rsdp.len() >= 36 {
        let length = u32_at(rsdp, 20) as usize;
        if length < 36 || !checksum(rsdp.get(..length)?) {
            return None;
        }
        let xsdt = u64_at(rsdp, 24);
        if xsdt != 0 {
            (table(usize::try_from(xsdt).ok()?)?, 8)
        } else {
            (table(u32_at(rsdp, 16) as usize)?, 4)
        }
    } else {
        (table(u32_at(rsdp, 16) as usize)?, 4)
    };
    if &root[..4] != (if stride == 8 { b"XSDT" } else { b"RSDT" }) {
        return None;
    }
    for entry in root[36..].chunks_exact(stride) {
        let address = if stride == 8 {
            u64_at(entry, 0)
        } else {
            u32_at(entry, 0) as u64
        };
        if let Some(data) = table(usize::try_from(address).ok()?)
            && &data[..4] == b"APIC"
            && data.len() >= 44
        {
            return Some(data);
        }
    }
    None
}

fn legacy_madt() -> Option<&'static [u8]> {
    let bda = physical_bytes(0x40e, 2)?;
    let ebda = u16::from_le_bytes(bda.try_into().ok()?) as usize * 16;
    for (start, length) in [(ebda, 1024), (0xe0000, 0x20000)] {
        if start == 0 {
            continue;
        }
        for address in (start..start + length).step_by(16) {
            if let Some(rsdp) = physical_bytes(address, 36)
                && let Some(data) = madt(rsdp)
            {
                return Some(data);
            }
        }
    }
    None
}

pub(crate) fn init(rsdp: Option<&[u8]>) {
    let bsp = hardware_id();
    let mut ids = Vec::new();
    ids.push(bsp)
        .expect("CPU capacity must include the boot CPU");
    let mut enabled = 0;
    if let Some(data) = rsdp.and_then(madt).or_else(legacy_madt) {
        let mut offset = 44;
        while offset + 2 <= data.len() {
            let length = data[offset + 1] as usize;
            assert!(
                length >= 2 && offset + length <= data.len(),
                "invalid MADT entry"
            );
            let entry = &data[offset..offset + length];
            let cpu = match (entry[0], length) {
                (0, 8..) => Some((entry[3] as u32, u32_at(entry, 4))),
                (9, 16..) => Some((u32_at(entry, 4), u32_at(entry, 8))),
                _ => None,
            };
            if let Some((id, flags)) = cpu
                && flags & 1 != 0
            {
                enabled += 1;
                if !ids.contains(&id) {
                    let _ = ids.push(id);
                }
            }
            offset += length;
        }
        ax_plat::console_println!(
            "CPU topology: MADT enabled={enabled}, capacity={}, using={}",
            ids.capacity(),
            ids.len()
        );
    } else {
        ax_plat::console_println!("CPU topology: no valid ACPI MADT; using boot CPU only");
    }
    for (logical, hardware) in ids.iter().enumerate() {
        ax_plat::console_println!("CPU topology: logical {logical} -> APIC {hardware:#x}");
    }
    APIC_IDS.init_once(ids);
}

pub(crate) fn cpu_num() -> usize {
    APIC_IDS.len()
}

#[cfg(any(feature = "irq", feature = "smp"))]
pub(crate) fn apic_id(cpu_id: usize) -> u32 {
    APIC_IDS[cpu_id]
}

#[cfg(feature = "smp")]
pub(crate) fn current_cpu_id() -> usize {
    let id = hardware_id();
    APIC_IDS
        .iter()
        .position(|candidate| *candidate == id)
        .expect("CPU absent from firmware topology")
}
