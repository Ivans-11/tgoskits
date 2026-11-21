// SPDX-License-Identifier: Apache-2.0

use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult};
use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags};
use axvisor_api::control::{self, ControlFileId, EndpointId, MmapArea, MmapProtection};

use super::{File, OPS, Vm, abi, close, file, new_file};

struct Segment {
    area: Arc<MmapArea>,
    len: usize,
    name: String,
    endpoint: Option<(EndpointId, ControlFileId)>,
}

struct Mapping {
    gpa: usize,
    segment: u32,
    offset: usize,
    len: usize,
    protection: u32,
    flags: u32,
    pages: control::PinnedPages,
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let _ = control::release_pinned_pages(self.pages.id);
    }
}

#[derive(Default)]
pub(super) struct Memory {
    segments: BTreeMap<u32, Segment>,
    mappings: BTreeMap<usize, Mapping>,
}

impl Memory {
    pub(super) fn endpoints(&self) -> Vec<(u32, EndpointId, ControlFileId)> {
        self.segments
            .iter()
            .filter_map(|(&id, segment)| {
                segment
                    .endpoint
                    .map(|(endpoint, file)| (id, endpoint, file))
            })
            .collect()
    }

    pub(super) fn forget_endpoint(&mut self, segment: u32) {
        if let Some(segment) = self.segments.get_mut(&segment) {
            segment.endpoint = None;
        }
    }

    pub(super) fn resolve(
        &self,
        segment: Option<u32>,
        offset: usize,
        len: usize,
        requested: MmapProtection,
    ) -> AxResult<(Arc<MmapArea>, usize, MmapProtection)> {
        if len == 0 || !offset.is_multiple_of(4096) || !len.is_multiple_of(4096) {
            return Err(AxError::InvalidInput);
        }
        let end = offset.checked_add(len).ok_or(AxError::InvalidInput)?;
        let (segment, offset, protection) = if let Some(segment) = segment {
            (segment, offset, 7)
        } else {
            let (_, mapping) = self
                .mappings
                .range(..=offset)
                .next_back()
                .ok_or(AxError::BadAddress)?;
            if end > mapping.gpa + mapping.len {
                return Err(AxError::BadAddress);
            }
            (
                mapping.segment,
                mapping.offset + offset - mapping.gpa,
                mapping.protection,
            )
        };
        let segment = self.segments.get(&segment).ok_or(AxError::NotFound)?;
        if offset.checked_add(len).is_none_or(|end| end > segment.len) {
            return Err(AxError::BadAddress);
        }
        let maximum = MmapProtection {
            read: protection & 1 != 0,
            write: protection & 2 != 0,
            execute: protection & 4 != 0,
        };
        if requested.read && !maximum.read
            || requested.write && !maximum.write
            || requested.execute && !maximum.execute
        {
            return Err(AxError::PermissionDenied);
        }
        Ok((segment.area.clone(), offset, maximum))
    }
}

fn open_memory(context: u64) -> AxResult<ControlFileId> {
    let state = file(context)?;
    let File::Memory(ref vm, _) = state else {
        return Err(AxError::NoSuchDevice);
    };
    if vm
        .runtime
        .removing
        .load(core::sync::atomic::Ordering::Acquire)
    {
        return Err(AxError::NoSuchDevice);
    }
    new_file(state)
}

fn allocate(vm: &Arc<Vm>, address: usize) -> AxResult {
    let bytes = abi::read::<32>(address)?;
    let id = abi::u32_at(&bytes, 0);
    let len = abi::u64_at(&bytes, 8) as usize;
    let name = abi::name(&bytes[16..])?;
    if id >= 4 || len == 0 || !len.is_multiple_of(4096) {
        return Err(AxError::InvalidInput);
    }
    {
        let memory = vm.memory.lock();
        if let Some(existing) = memory.segments.get(&id) {
            return if existing.len == len && existing.name == name {
                Ok(())
            } else {
                Err(AxError::InvalidInput)
            };
        }
        if !name.is_empty() && memory.segments.values().any(|segment| segment.name == name) {
            return Err(AxError::AlreadyExists);
        }
    }
    let area = Arc::new(control::MmapArea::new(len)?);
    let endpoint = if name.is_empty() {
        None
    } else {
        let context = new_file(File::Memory(vm.clone(), id))?;
        let ops = control::ControlOps {
            open: open_memory,
            ..OPS
        };
        match control::register_endpoint(&format!("vmm.io/{}.{}", vm.name, name), context, ops) {
            Ok(endpoint) => Some((endpoint, context)),
            Err(error) => {
                close(context)?;
                return Err(error);
            }
        }
    };
    vm.memory.lock().segments.insert(
        id,
        Segment {
            area,
            len,
            name: name.into(),
            endpoint,
        },
    );
    Ok(())
}

fn map(vm: &Vm, address: usize) -> AxResult {
    let bytes = abi::read::<40>(address)?;
    let gpa = abi::u64_at(&bytes, 0) as usize;
    let segment = abi::u32_at(&bytes, 8);
    let offset = abi::u64_at(&bytes, 16) as usize;
    let len = abi::u64_at(&bytes, 24) as usize;
    let protection = abi::u32_at(&bytes, 32);
    let flags = abi::u32_at(&bytes, 36);
    if !gpa.is_multiple_of(4096)
        || !offset.is_multiple_of(4096)
        || len == 0
        || !len.is_multiple_of(4096)
        || protection & !7 != 0
        || protection == 0
        || flags & !1 != 0
    {
        return Err(AxError::InvalidInput);
    }
    let end = gpa.checked_add(len).ok_or(AxError::InvalidInput)?;
    let area = {
        let memory = vm.memory.lock();
        if memory
            .mappings
            .values()
            .any(|mapping| gpa < mapping.gpa + mapping.len && mapping.gpa < end)
        {
            return Err(AxError::AlreadyExists);
        }
        let backing = memory.segments.get(&segment).ok_or(AxError::NotFound)?;
        if offset.checked_add(len).is_none_or(|end| end > backing.len) {
            return Err(AxError::InvalidInput);
        }
        backing.area.clone()
    };
    let pages = control::pin_mmap_area(&area, offset, len, protection & 2 != 0)?;
    let mapping = Mapping {
        gpa,
        segment,
        offset,
        len,
        protection,
        flags,
        pages,
    };
    if mapping.pages.pages.len() != len / 4096 {
        return Err(AxError::BadState);
    }
    let mut permissions = MappingFlags::empty();
    if protection & 1 != 0 {
        permissions |= MappingFlags::READ;
    }
    if protection & 2 != 0 {
        permissions |= MappingFlags::WRITE;
    }
    if protection & 4 != 0 {
        permissions |= MappingFlags::EXECUTE;
    }
    let regions: Vec<_> = mapping
        .pages
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            (
                GuestPhysAddr::from(gpa + index * 4096),
                HostPhysAddr::from(page.as_usize()),
            )
        })
        .collect();
    vm.machine.map_regions_linear(&regions, 4096, permissions)?;
    vm.memory.lock().mappings.insert(gpa, mapping);
    Ok(())
}

pub(super) fn ioctl(vm: &Arc<Vm>, command: u32, address: usize) -> AxResult {
    match command {
        abi::ALLOC_MEMSEG => allocate(vm, address),
        abi::MMAP_MEMSEG => map(vm, address),
        abi::GET_MEMSEG => abi::ioctl::<32>(command, address, |data| {
            let id = abi::u32_at(data, 0);
            if id >= 4 {
                return Err(AxError::InvalidInput);
            }
            data.fill(0);
            data[..4].copy_from_slice(&id.to_le_bytes());
            let memory = vm.memory.lock();
            // FreeBSD returns an empty segment descriptor for unallocated IDs.
            if let Some(segment) = memory.segments.get(&id) {
                data[8..16].copy_from_slice(&(segment.len as u64).to_le_bytes());
                data[16..16 + segment.name.len()].copy_from_slice(segment.name.as_bytes());
            }
            Ok(())
        }),
        abi::MMAP_GETNEXT => abi::ioctl::<40>(command, address, |data| {
            let gpa = abi::u64_at(data, 0) as usize;
            let memory = vm.memory.lock();
            let (_, mapping) = memory
                .mappings
                .range(gpa..)
                .next()
                .ok_or(AxError::NotFound)?;
            data.fill(0);
            data[..8].copy_from_slice(&(mapping.gpa as u64).to_le_bytes());
            data[8..12].copy_from_slice(&mapping.segment.to_le_bytes());
            data[16..24].copy_from_slice(&(mapping.offset as u64).to_le_bytes());
            data[24..32].copy_from_slice(&(mapping.len as u64).to_le_bytes());
            data[32..36].copy_from_slice(&mapping.protection.to_le_bytes());
            data[36..40].copy_from_slice(&mapping.flags.to_le_bytes());
            Ok(())
        }),
        abi::MUNMAP_MEMSEG => {
            let input = abi::read::<16>(address)?;
            let gpa = abi::u64_at(&input, 0) as usize;
            let len = abi::u64_at(&input, 8) as usize;
            {
                let memory = vm.memory.lock();
                let mapping = memory.mappings.get(&gpa).ok_or(AxError::NotFound)?;
                if len != mapping.len {
                    return Err(AxError::InvalidInput);
                }
            }
            vm.machine.unmap_region(GuestPhysAddr::from(gpa), len)?;
            let mapping = vm.memory.lock().mappings.remove(&gpa);
            drop(mapping);
            Ok(())
        }
        _ => Err(AxError::Unsupported),
    }
}
