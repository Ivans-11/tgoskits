// SPDX-License-Identifier: Apache-2.0

//! VFIO sparse BAR mappings. Only host-authorized whole pages are exposed.

use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use axvisor_api::{control as api, device::RegionKind};

use super::{Device, File, abi::*, with_registry};

pub(super) fn region_info(device: &Device, arg: usize) -> AxResult {
    let request = payload::<32>(arg)?;
    let capacity = u32_at(&request, 0) as usize;
    let index = u32_at(&request, 8);
    if index >= 9 {
        return Err(AxError::InvalidInput);
    }
    let region = device
        .assignment
        .info()
        .regions
        .iter()
        .find(|r| index < 6 && r.index == index && r.kind != RegionKind::Configuration);
    let mappings = match region {
        Some(r) if r.kind == RegionKind::Mmio => device.assignment.bar_region(index)?.mappings,
        _ => &[],
    };
    let size = 32
        + if mappings.is_empty() {
            0
        } else {
            16 + mappings.len() * 16
        };
    let mut bytes = alloc::vec![0; size];
    let (length, mut flags) = if index == CONFIG_REGION {
        (256, 3)
    } else {
        region.map_or((0, 0), |r| {
            (
                r.size,
                u32::from(r.permissions.read) | (u32::from(r.permissions.write) << 1),
            )
        })
    };
    if !mappings.is_empty() {
        flags |= 4 | 8; // MMAP | CAPS
        // VFIO_REGION_INFO_CAP_SPARSE_MMAP, version 1, no next capability.
        bytes[32..34].copy_from_slice(&1u16.to_ne_bytes());
        bytes[34..36].copy_from_slice(&1u16.to_ne_bytes());
        put32(&mut bytes, 40, mappings.len() as u32);
        for (i, mapping) in mappings.iter().enumerate() {
            put64(&mut bytes, 48 + i * 16, mapping.offset);
            put64(&mut bytes, 56 + i * 16, mapping.length as u64);
        }
        // Like VFIO, report the required size before exposing the cap chain.
        if capacity >= size {
            put32(&mut bytes, 12, 32);
        }
    }
    put32(&mut bytes, 0, size as u32);
    put32(&mut bytes, 4, flags);
    put32(&mut bytes, 8, index);
    put64(&mut bytes, 16, length);
    put64(&mut bytes, 24, u64::from(index) << REGION_SHIFT);
    api::copy_to_user(arg, &bytes[..capacity.min(size)])
}

pub(super) fn map(
    id: u64,
    offset: usize,
    len: usize,
    requested: api::MmapProtection,
    shared: bool,
) -> AxResult<(Arc<api::MmapArea>, usize, api::MmapProtection)> {
    if !shared || requested.execute || len == 0 || (offset | len) & 4095 != 0 {
        return Err(AxError::InvalidInput);
    }
    let index = (offset as u64 >> REGION_SHIFT) as u32;
    let at = offset as u64 & ((1 << REGION_SHIFT) - 1);
    with_registry(|registry| {
        let Some(File::Device(device)) = registry.files.get(&id) else {
            return Err(AxError::BadFileDescriptor);
        };
        let (area, permissions) = device.assignment.mmap(
            index,
            at,
            len,
            axvisor_api::device::AccessPermissions {
                read: requested.read,
                write: requested.write,
            },
        )?;
        let maximum = api::MmapProtection {
            read: permissions.read,
            write: permissions.write,
            execute: false,
        };
        Ok((Arc::new(area), 0, maximum))
    })
}
