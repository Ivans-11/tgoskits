// SPDX-License-Identifier: Apache-2.0

//! Legacy VFIO container/group/device frontend for QEMU.
//!
//! DeviceIf owns physical arbitration and isolation. This frontend owns the
//! VFIO file protocol, DMA leases and eventfd notifications; QEMU owns the
//! guest PCI topology. Published groups contain one independently assignable
//! PCI function, as required by the current DeviceIf backends.

mod abi;
mod irq;
mod mmap;
mod pci;

use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};

use abi::*;
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axvisor_api::{control as api, device::*};

use crate::{
    control::{OperationLock, next_file_id},
    device::DeviceAssignment,
};

const OPS: api::ControlOps = api::ControlOps {
    open,
    close,
    ioctl,
    mmap: Some(mmap::map),
    io: Some(api::FileIoOps {
        read: read_file,
        write: write_file,
    }),
};
static SERIAL: spin::Once<Arc<OperationLock>> = spin::Once::new();
static REGISTRY: SpinNoIrq<Registry> = SpinNoIrq::new(Registry::new());

struct Registration {
    id: DeviceId,
    name: String,
    endpoint: Option<api::EndpointId>,
    opened: Option<u64>,
    removing: bool,
}

struct Container {
    open: bool,
    groups: usize,
    space: Option<(DmaSpace, DmaConstraints)>,
    mappings: BTreeMap<u64, Mapping>,
}

struct Mapping {
    len: u64,
    handle: DmaMapping,
}

struct Group {
    open: bool,
    number: u32,
    container: Option<u64>,
    devices: usize,
}

struct Device {
    group: u64,
    assignment: DeviceAssignment,
    pci: Arc<axdevice::pci::PciFunction>,
    config_region: u32,
    source: u32,
    irq: Option<irq::Irq>,
    notification: Arc<irq::Notification>,
}

enum File {
    Container(Container),
    Group(Group),
    Device(Device),
}

struct Registry {
    endpoint: Option<api::EndpointId>,
    groups: BTreeMap<u32, Registration>,
    files: BTreeMap<u64, File>,
    retired: Vec<DeviceAssignment>,
    pending_close: Vec<u64>,
    release_fds: Vec<api::UserFdRefId>,
}

impl Registry {
    const fn new() -> Self {
        Self {
            endpoint: None,
            groups: BTreeMap::new(),
            files: BTreeMap::new(),
            retired: Vec::new(),
            pending_close: Vec::new(),
            release_fds: Vec::new(),
        }
    }

    fn resolve_fd(&mut self, fd: i32) -> AxResult<u64> {
        let retained = api::get_user_fd_ref(fd)?;
        self.release_fds.push(retained);
        let mut bytes = [0; size_of::<api::ControlFileId>()];
        if api::read_user_fd_ref(retained, &mut bytes)? != bytes.len() {
            return Err(AxError::BadFileDescriptor);
        }
        Ok(api::ControlFileId::from_ne_bytes(bytes))
    }

    fn container(&mut self, id: u64) -> AxResult<&mut Container> {
        match self.files.get_mut(&id) {
            Some(File::Container(container)) => Ok(container),
            _ => Err(AxError::BadFileDescriptor),
        }
    }

    fn collect_container(&mut self, id: u64) -> AxResult {
        let container = self.container(id)?;
        if container.open || container.groups != 0 {
            return Ok(());
        }
        while let Some((&iova, mapping)) = container.mappings.first_key_value() {
            axvisor_api::device::unmap_dma(mapping.handle)?;
            container.mappings.remove(&iova);
        }
        if let Some((space, _)) = container.space {
            axvisor_api::device::destroy_dma_space(space)?;
        }
        self.files.remove(&id);
        Ok(())
    }

    fn collect_group(&mut self, id: u64) -> AxResult {
        let Some(File::Group(group)) = self.files.get(&id) else {
            return Err(AxError::BadFileDescriptor);
        };
        if group.open || group.devices != 0 {
            return Ok(());
        }
        let (number, container) = (group.number, group.container);
        self.files.remove(&id);
        if let Some(registration) = self.groups.get_mut(&number) {
            registration.opened = None;
            if registration.removing && registration.endpoint.is_none() {
                self.groups.remove(&number);
            }
        }
        if let Some(container) = container {
            self.container(container)?.groups -= 1;
            self.collect_container(container)?;
        }
        Ok(())
    }

    fn close(&mut self, id: u64) -> AxResult {
        match self.files.get_mut(&id).ok_or(AxError::BadFileDescriptor)? {
            File::Container(container) => {
                container.open = false;
                self.collect_container(id)
            }
            File::Group(group) => {
                group.open = false;
                self.collect_group(id)
            }
            File::Device(device) => {
                if let Some(irq) = device.irq.as_mut() {
                    irq.stop(&mut device.assignment)?;
                }
                device.irq = None;
                device.assignment.close()?;
                let group_id = device.group;
                self.files.remove(&id);
                let Some(File::Group(group)) = self.files.get_mut(&group_id) else {
                    return Err(AxError::BadState);
                };
                group.devices -= 1;
                self.collect_group(group_id)
            }
        }
    }

    fn install(&mut self, file: File) -> AxResult<isize> {
        let id = next_file_id()?;
        self.files.insert(id, file);
        match api::create_user_fd(id, OPS) {
            Ok(fd) => Ok(fd as isize),
            Err(error) => {
                if let Err(cleanup) = self.close_or_defer(id) {
                    warn!("VFIO fd rollback retained resources: {cleanup:?}");
                }
                Err(error)
            }
        }
    }

    fn close_or_defer(&mut self, id: u64) -> AxResult {
        let result = self.close(id);
        if result.is_err() && !self.pending_close.contains(&id) {
            self.pending_close.push(id);
        }
        result
    }

    fn retry_cleanup(&mut self) {
        self.retired
            .retain_mut(|assignment| assignment.close().is_err());
        for id in core::mem::take(&mut self.pending_close) {
            if self.files.contains_key(&id) {
                let _ = self.close_or_defer(id);
            }
        }
    }
}

// Only registry access uses the IRQ-off lock. Host operations and callbacks
// may sleep; retained fds are released after serialization, since their final
// close may re-enter this frontend.
fn with_registry<T>(operation: impl FnOnce(&mut Registry) -> AxResult<T>) -> AxResult<T> {
    let guard = SERIAL.call_once(OperationLock::new).lock();
    let mut registry = core::mem::replace(&mut *REGISTRY.lock(), Registry::new());
    registry.retry_cleanup();
    let result = operation(&mut registry);
    let release = core::mem::take(&mut registry.release_fds);
    *REGISTRY.lock() = registry;
    drop(guard);
    for fd in release {
        let _ = api::release_user_fd_ref(fd);
    }
    result
}

/// Publish the VFIO container endpoint. Call during control-mode setup.
pub fn init() -> AxResult {
    // Setup is serialized with group publication by the host initialization path.
    if with_registry(|registry| Ok(registry.endpoint.is_some()))? {
        return Ok(());
    }
    let endpoint = api::register_endpoint("vfio/vfio", u64::MAX, OPS)?;
    with_registry(|registry| {
        registry.endpoint = Some(endpoint);
        Ok(())
    })
}

/// Publish a host-authorized, individually isolated PCI function. The host
/// chooses the group number and provides QEMU's matching discovery metadata.
/// Publishing never acquires or unbinds a physical device.
pub fn register_pci(group: u32, address: PciAddress) -> AxResult {
    let name = format!(
        "{:04x}:{:02x}:{:02x}.{:x}",
        address.segment, address.bus, address.device, address.function
    );
    with_registry(|registry| {
        if registry.groups.contains_key(&group) {
            return Err(AxError::AlreadyExists);
        }
        registry.groups.insert(
            group,
            Registration {
                id: DeviceId::Pci(address),
                name,
                endpoint: None,
                opened: None,
                removing: false,
            },
        );
        Ok(())
    })?;
    match api::register_endpoint(&format!("vfio/{group}"), u64::from(group), OPS) {
        Ok(endpoint) => with_registry(|registry| {
            registry
                .groups
                .get_mut(&group)
                .ok_or(AxError::NotFound)?
                .endpoint = Some(endpoint);
            Ok(())
        }),
        Err(error) => {
            with_registry(|registry| {
                registry.groups.remove(&group);
                Ok(())
            })?;
            Err(error)
        }
    }
}

/// Prevent new opens while existing files retain their assigned resources.
pub fn unregister_pci(group: u32) -> AxResult {
    let endpoint = with_registry(|registry| {
        let entry = registry.groups.get_mut(&group).ok_or(AxError::NotFound)?;
        entry.removing = true;
        entry.endpoint.ok_or(AxError::ResourceBusy)
    })?;
    api::unregister_endpoint(endpoint)?;
    with_registry(|registry| {
        let entry = registry.groups.get_mut(&group).ok_or(AxError::NotFound)?;
        entry.endpoint = None;
        if entry.opened.is_none() {
            registry.groups.remove(&group);
        }
        Ok(())
    })
}

fn open(context: u64) -> AxResult<u64> {
    with_registry(|registry| {
        let id = next_file_id()?;
        let file = if context == u64::MAX {
            File::Container(Container {
                open: true,
                groups: 0,
                space: None,
                mappings: BTreeMap::new(),
            })
        } else {
            let number = u32::try_from(context).map_err(|_| AxError::InvalidInput)?;
            let entry = registry
                .groups
                .get_mut(&number)
                .ok_or(AxError::NoSuchDevice)?;
            if entry.removing {
                return Err(AxError::NoSuchDevice);
            }
            if entry.opened.is_some() {
                return Err(AxError::ResourceBusy);
            }
            entry.opened = Some(id);
            File::Group(Group {
                open: true,
                number,
                container: None,
                devices: 0,
            })
        };
        registry.files.insert(id, file);
        Ok(id)
    })
}

fn close(id: u64) -> AxResult {
    with_registry(|registry| registry.close_or_defer(id))
}

fn ioctl(id: u64, command: u32, argument: usize) -> AxResult<isize> {
    with_registry(
        |registry| match registry.files.get(&id).ok_or(AxError::BadFileDescriptor)? {
            File::Container(_) => container_ioctl(registry, id, command, argument),
            File::Group(_) => group_ioctl(registry, id, command, argument),
            File::Device(_) => device_ioctl(registry, id, command, argument),
        },
    )
}

struct DmaLease(api::PinnedPagesId);
impl Drop for DmaLease {
    fn drop(&mut self) {
        let _ = api::release_pinned_pages(self.0);
    }
}

fn container_ioctl(registry: &mut Registry, id: u64, command: u32, arg: usize) -> AxResult<isize> {
    let container = registry.container(id)?;
    match command {
        GET_API_VERSION => Ok(0),
        CHECK_EXTENSION => Ok(isize::from(arg == 3)), // TYPE1v2
        SET_IOMMU => {
            if arg != 3 {
                return Err(AxError::Unsupported);
            }
            if container.groups == 0 || container.space.is_some() {
                return Err(AxError::InvalidInput);
            }
            container.space = Some(axvisor_api::device::create_dma_space()?);
            Ok(0)
        }
        IOMMU_GET_INFO => {
            let mut bytes = payload::<16>(arg)?;
            let (_, constraints) = container.space.ok_or(AxError::BadState)?;
            put32(&mut bytes, 0, 16);
            put32(&mut bytes, 4, 1); // PGSIZES, no dirty tracking or migration
            put64(&mut bytes, 8, constraints.granule as u64);
            api::copy_to_user(arg, &bytes)?;
            Ok(0)
        }
        IOMMU_MAP_DMA => {
            // A CPU stage-2 shootdown cannot retire an IOMMU translation.
            // This provider selects notified RAM for all new control VMs.
            if api::revocable_memory_ops().is_some() {
                return Err(AxError::Unsupported);
            }
            let bytes = payload::<32>(arg)?;
            let flags = u32_at(&bytes, 4);
            let address = u64_at(&bytes, 8);
            let iova = u64_at(&bytes, 16);
            let len = u64_at(&bytes, 24);
            let (space, limits) = container.space.ok_or(AxError::BadState)?;
            if flags == 0
                || flags & !3 != 0
                || len == 0
                || address.checked_add(len).is_none()
                || iova.checked_add(len).is_none()
                || (address | iova | len) & (limits.granule as u64 - 1) != 0
                || iova < limits.start
                || iova + len - 1 > limits.end
                || container
                    .mappings
                    .iter()
                    .any(|(&start, mapping)| iova < start + mapping.len && start < iova + len)
            {
                return Err(AxError::InvalidInput);
            }
            let mm = api::retain_current_user_address_space()?;
            let pinned =
                api::pin_user_pages(mm, address as usize, len as usize, flags & 2 != 0, true);
            let _ = api::release_user_address_space(mm);
            let pinned = pinned?;
            let lease = Arc::new(DmaLease(pinned.id));
            if pinned.is_device || pinned.pages.len().checked_mul(4096) != Some(len as usize) {
                return Err(AxError::BadAddress);
            }
            let permissions = AccessPermissions {
                read: flags & 1 != 0,
                write: flags & 2 != 0,
            };
            // SAFETY: the host's DMA pin guarantees stable authorized RAM;
            // the lease is retained by every successful DeviceIf DMA mapping.
            let backing =
                unsafe { DmaBacking::from_pages(pinned.pages, 4096, permissions, lease)? };
            let handle = axvisor_api::device::map_dma(space, iova, backing, permissions)?;
            container.mappings.insert(iova, Mapping { len, handle });
            Ok(0)
        }
        IOMMU_UNMAP_DMA => {
            let mut bytes = payload::<24>(arg)?;
            let start = u64_at(&bytes, 8);
            let len = u64_at(&bytes, 16);
            let end = start.checked_add(len).ok_or(AxError::InvalidInput)?;
            if u32_at(&bytes, 4) != 0 || len == 0 || (start | len) & 4095 != 0 {
                return Err(AxError::InvalidInput);
            }
            // TYPE1v2 rejects splitting a previously mapped extent.
            if container.mappings.iter().any(|(&base, mapping)| {
                base < end
                    && start < base + mapping.len
                    && (base < start || base + mapping.len > end)
            }) {
                return Err(AxError::InvalidInput);
            }
            let ranges: Vec<u64> = container
                .mappings
                .range(start..end)
                .map(|(&base, _)| base)
                .collect();
            let mut unmapped = 0;
            for base in ranges {
                let mapping = &container.mappings[&base];
                axvisor_api::device::unmap_dma(mapping.handle)?;
                unmapped += mapping.len;
                container.mappings.remove(&base);
            }
            put64(&mut bytes, 16, unmapped);
            api::copy_to_user(arg, &bytes)?;
            Ok(0)
        }
        _ => Err(AxError::Unsupported),
    }
}

fn group_ioctl(registry: &mut Registry, id: u64, command: u32, arg: usize) -> AxResult<isize> {
    let Some(File::Group(group)) = registry.files.get(&id) else {
        return Err(AxError::BadFileDescriptor);
    };
    let (number, container_id, devices) = (group.number, group.container, group.devices);
    match command {
        GROUP_GET_STATUS => {
            let mut bytes = payload::<8>(arg)?;
            put32(&mut bytes, 0, 8);
            put32(
                &mut bytes,
                4,
                1 | if container_id.is_some() { 2 } else { 0 },
            );
            api::copy_to_user(arg, &bytes)?;
            Ok(0)
        }
        GROUP_SET_CONTAINER => {
            if container_id.is_some() {
                return Err(AxError::ResourceBusy);
            }
            let fd = i32::from_le_bytes(read::<4>(arg)?);
            let container = registry.resolve_fd(fd)?;
            registry.container(container)?.groups += 1;
            let Some(File::Group(group)) = registry.files.get_mut(&id) else {
                unreachable!()
            };
            group.container = Some(container);
            Ok(0)
        }
        GROUP_UNSET_CONTAINER => {
            if devices != 0 {
                return Err(AxError::ResourceBusy);
            }
            let container = container_id.ok_or(AxError::InvalidInput)?;
            registry.container(container)?.groups -= 1;
            let Some(File::Group(group)) = registry.files.get_mut(&id) else {
                unreachable!()
            };
            group.container = None;
            registry.collect_container(container)?;
            Ok(0)
        }
        GROUP_GET_DEVICE_FD => {
            let container = container_id.ok_or(AxError::BadState)?;
            if registry.container(container)?.space.is_none() {
                return Err(AxError::BadState);
            }
            let requested = name(arg)?;
            let entry = registry.groups.get(&number).ok_or(AxError::NoSuchDevice)?;
            if requested != entry.name || entry.removing {
                return Err(AxError::NoSuchDevice);
            }
            if devices != 0 {
                return Err(AxError::ResourceBusy);
            }
            let mut assignment = DeviceAssignment::open(&entry.id)?;
            let device = match pci::new(&assignment) {
                Ok((pci, config_region, source, notification)) => Device {
                    group: id,
                    assignment,
                    pci,
                    config_region,
                    source,
                    notification,
                    irq: None,
                },
                Err(error) => {
                    if assignment.close().is_err() {
                        registry.retired.push(assignment);
                    }
                    return Err(error);
                }
            };
            let Some(File::Group(group)) = registry.files.get_mut(&id) else {
                unreachable!()
            };
            group.devices += 1;
            registry.install(File::Device(device))
        }
        _ => Err(AxError::Unsupported),
    }
}

fn device_ioctl(registry: &mut Registry, id: u64, command: u32, arg: usize) -> AxResult<isize> {
    let Some(File::Device(device)) = registry.files.get_mut(&id) else {
        return Err(AxError::BadFileDescriptor);
    };
    match command {
        DEVICE_GET_INFO => {
            let mut bytes = payload::<16>(arg)?;
            put32(&mut bytes, 0, 16);
            put32(&mut bytes, 4, 2); // PCI; reset/migration not advertised
            put32(&mut bytes, 8, 9);
            put32(&mut bytes, 12, 5);
            api::copy_to_user(arg, &bytes)?;
        }
        DEVICE_GET_REGION_INFO => {
            mmap::region_info(device, arg)?;
        }
        DEVICE_GET_IRQ_INFO => {
            let mut bytes = payload::<16>(arg)?;
            let index = u32_at(&bytes, 8);
            if index >= 5 {
                return Err(AxError::InvalidInput);
            }
            put32(&mut bytes, 0, 16);
            put32(&mut bytes, 4, if index == 1 { 1 | 8 } else { 0 }); // MSI, eventfd, noresize
            put32(&mut bytes, 12, u32::from(index == 1));
            api::copy_to_user(arg, &bytes)?;
        }
        DEVICE_SET_IRQS => {
            let bytes = payload::<20>(arg)?;
            let flags = u32_at(&bytes, 4);
            let index = u32_at(&bytes, 8);
            let start = u32_at(&bytes, 12);
            let count = u32_at(&bytes, 16);
            if index != 1 || start != 0 || count > 1 {
                return Err(AxError::InvalidInput);
            }
            match (flags, count) {
                (0x21, 0) => {
                    if let Some(irq) = device.irq.as_mut() {
                        irq.stop(&mut device.assignment)?;
                    }
                    device.irq = None;
                }
                (0x24, 1) => {
                    if u32_at(&bytes, 0) < 24 {
                        return Err(AxError::InvalidInput);
                    }
                    let fd = i32::from_le_bytes(read::<4>(
                        arg.checked_add(20).ok_or(AxError::InvalidInput)?,
                    )?);
                    if fd < -1 {
                        return Err(AxError::InvalidInput);
                    }
                    // Retain the replacement before revoking the current source.
                    // The caller supplies an eventfd, as required by the ABI.
                    let retained = if fd >= 0 {
                        let retained = api::get_user_fd_ref(fd)?;
                        registry.release_fds.push(retained);
                        Some(retained)
                    } else {
                        None
                    };
                    if let Some(irq) = device.irq.as_mut() {
                        irq.stop(&mut device.assignment)?;
                    }
                    device.irq = None;
                    if let Some(fd) = retained {
                        let irq = irq::Irq::bind(
                            &mut device.assignment,
                            device.source,
                            fd,
                            device.pci.clone(),
                            device.notification.clone(),
                        )?;
                        registry.release_fds.pop();
                        device.irq = Some(irq);
                        device.irq.as_ref().unwrap().unmask(&device.assignment)?;
                    }
                }
                (0x21, 1) => {
                    if let Some(irq) = &device.irq {
                        irq.trigger();
                    }
                }
                _ => return Err(AxError::Unsupported),
            }
        }
        _ => return Err(AxError::Unsupported),
    }
    Ok(0)
}

fn read_file(id: u64, offset: u64, buffer: &mut [u8]) -> AxResult<usize> {
    with_registry(|registry| {
        let Some(File::Device(device)) = registry.files.get(&id) else {
            return Err(AxError::InvalidInput);
        };
        pci::read(device, offset, buffer)
    })
}

fn write_file(id: u64, offset: u64, buffer: &[u8]) -> AxResult<usize> {
    with_registry(|registry| {
        let Some(File::Device(device)) = registry.files.get(&id) else {
            return Err(AxError::InvalidInput);
        };
        let group_id = device.group;
        let Some(File::Group(group)) = registry.files.get(&group_id) else {
            return Err(AxError::BadState);
        };
        let container = group.container.ok_or(AxError::BadState)?;
        let (space, _) = registry
            .container(container)?
            .space
            .ok_or(AxError::BadState)?;
        let Some(File::Device(device)) = registry.files.get_mut(&id) else {
            unreachable!()
        };
        pci::write(device, space, offset, buffer)
    })
}
