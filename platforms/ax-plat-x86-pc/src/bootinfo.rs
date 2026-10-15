//! Multiboot2 information used before replacing the bootstrap page tables.

use ax_plat::mem::{pa, phys_to_virt};

pub(crate) fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

pub(crate) fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

/// Firmware addresses are accessible through the initial 512 GiB direct map.
/// Call only during early boot, for bootloader/firmware-owned memory.
pub(crate) fn physical_bytes(address: usize, length: usize) -> Option<&'static [u8]> {
    if address == 0 || address.checked_add(length)? > 512 * 1024 * 1024 * 1024 {
        return None;
    }
    // SAFETY: the caller supplies a firmware/bootloader range, within the
    // bootstrap mapping. No reference survives into reclaiming that memory.
    Some(unsafe { core::slice::from_raw_parts(phys_to_virt(pa!(address)).as_ptr(), length) })
}

pub(crate) fn tags(address: usize) -> impl Iterator<Item = (u32, &'static [u8])> {
    let header = physical_bytes(address, 8).expect("invalid Multiboot2 address");
    let size = u32_at(header, 0) as usize;
    assert!((16..=16 * 1024 * 1024).contains(&size));
    let data = physical_bytes(address, size).expect("Multiboot2 info outside bootstrap mapping");
    let mut offset = 8;
    core::iter::from_fn(move || {
        let header = data.get(offset..offset + 8)?;
        let kind = u32_at(header, 0);
        let length = u32_at(header, 4) as usize;
        if kind == 0 {
            return None;
        }
        assert!(length >= 8, "invalid Multiboot2 tag length");
        let payload = data.get(offset + 8..offset.checked_add(length)?)?;
        offset += (length + 7) & !7;
        Some((kind, payload))
    })
}
