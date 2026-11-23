// SPDX-License-Identifier: Apache-2.0

//! DeviceIf adapter for the static VM configuration path.

#[cfg(target_arch = "x86_64")]
mod pci;

use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange};
use axdevice_base::{BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, EmuDeviceType};
use axvisor_api::device::*;
use axvm::config::{DeviceIdConfig, PassThroughDeviceConfig};

use crate::{
    device::{DeviceAccess, DeviceAssignment},
    vmm::VMRef,
};

// None reserves a VM ID while sleepable setup/teardown runs without this lock.
static ASSIGNMENTS: SpinNoIrq<BTreeMap<usize, Option<Assignments>>> =
    SpinNoIrq::new(BTreeMap::new());

#[derive(Default)]
struct Assignments {
    devices: Vec<DeviceAssignment>,
    #[cfg(target_arch = "x86_64")]
    bars: Vec<Arc<pci::BarMappings>>,
}

struct MmioWindow {
    range: GuestPhysAddrRange,
    region: u32,
    access: Arc<DeviceAccess>,
}

impl MmioWindow {
    fn offset(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<u64> {
        let offset = addr
            .as_usize()
            .checked_sub(self.range.start.as_usize())
            .ok_or(AxError::InvalidInput)?;
        if offset
            .checked_add(width.size())
            .is_none_or(|end| end > self.range.size())
        {
            return Err(AxError::InvalidInput);
        }
        Ok(offset as u64)
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for MmioWindow {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }
    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        self.access
            .read(self.region, self.offset(addr, width)?, width)
            .map(|v| v as usize)
    }
    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, value: usize) -> AxResult {
        self.access
            .write(self.region, self.offset(addr, width)?, width, value as u64)
    }
}

fn device_id(config: &DeviceIdConfig) -> AxResult<DeviceId> {
    Ok(match config {
        DeviceIdConfig::Pci(address) => DeviceId::Pci(PciAddress::try_from(*address)?),
        DeviceIdConfig::DeviceTreePath(path) => DeviceId::DeviceTreePath(path.clone()),
        DeviceIdConfig::AcpiPath(path) => DeviceId::AcpiPath(path.clone()),
        DeviceIdConfig::Host(id) => DeviceId::Host(id.clone()),
    })
}

fn validate(vm: &VMRef, configs: &[PassThroughDeviceConfig]) -> AxResult {
    let memory = vm.memory_regions();
    let emulated = vm.with_config(|c| c.emu_devices().clone());
    for (index, config) in configs.iter().enumerate() {
        let assignment = config
            .assignment
            .as_ref()
            .ok_or(AxError::PermissionDenied)?;
        let end = config
            .base_gpa
            .checked_add(config.length)
            .ok_or(AxError::InvalidInput)?;
        // Physical addresses never authorize a device in this path.
        device_id(&assignment.id)?;
        if config.base_hpa != 0 || config.length == 0 {
            return Err(AxError::InvalidInput);
        }
        if let Some([bus, device, function]) = assignment.guest_pci {
            if !matches!(assignment.id, DeviceIdConfig::Pci(_))
                || !cfg!(target_arch = "x86_64")
                || bus != 0
                || !(1..32).contains(&device)
                || function != 0
            {
                return Err(AxError::Unsupported);
            }
            if emulated
                .iter()
                .any(|r| r.base_gpa < 0xd00 && r.base_gpa.saturating_add(r.length) > 0xcf8)
            {
                return Err(AxError::InvalidInput);
            }
            if configs
                .iter()
                .filter_map(|c| c.assignment.as_ref())
                .filter(|a| a.id == assignment.id && a.irq_source.is_some())
                .count()
                != 1
            {
                return Err(AxError::InvalidInput);
            }
        }
        for previous in &configs[..index] {
            let other = previous.assignment.as_ref().ok_or(AxError::InvalidInput)?;
            if (other.id == assignment.id && other.guest_pci != assignment.guest_pci)
                || (assignment.guest_pci.is_some()
                    && other.guest_pci == assignment.guest_pci
                    && other.id != assignment.id)
            {
                return Err(AxError::InvalidInput);
            }
        }
        let overlaps = |base: usize, size: usize| {
            base.checked_add(size)
                .is_none_or(|other_end| config.base_gpa < other_end && base < end)
        };
        if memory.iter().any(|r| overlaps(r.gpa.as_usize(), r.size()))
            || emulated.iter().any(|r| overlaps(r.base_gpa, r.length))
            || configs[..index]
                .iter()
                .any(|r| overlaps(r.base_gpa, r.length))
            || overlaps(0xfee0_0000, 0x1000)
        {
            return Err(AxError::InvalidInput);
        }
        if let Some(source) = assignment.irq_source {
            if !cfg!(target_arch = "x86_64")
                || (assignment.guest_pci.is_none() && config.irq_id >= 24)
            {
                return Err(AxError::Unsupported);
            }
            if configs[..index].iter().any(|c| {
                c.assignment
                    .as_ref()
                    .is_some_and(|a| a.id == assignment.id && a.irq_source == Some(source))
            }) {
                return Err(AxError::InvalidInput);
            }
        }
    }
    Ok(())
}

fn map_ram(vm: &VMRef, assignment: &mut DeviceAssignment) -> AxResult {
    if vm.guest_access.enabled() {
        return Err(ax_errno::AxError::Unsupported);
    }
    if !assignment.info().dma {
        return Ok(());
    }
    let regions = vm.memory_regions();
    if regions.is_empty() {
        return Err(AxError::InvalidInput);
    }
    for region in regions {
        // Reserved/external memory needs its own owning lease; an address alone
        // cannot establish that it remains allocated while a device uses it.
        if !region.needs_dealloc || region.size() % 4096 != 0 {
            return Err(AxError::Unsupported);
        }
        let pages = (0..region.size())
            .step_by(4096)
            .map(|offset| region.host_paddr() + offset)
            .collect();
        let permissions = AccessPermissions {
            read: true,
            write: true,
        };
        // SAFETY: AxVM owns these contiguous allocations and only frees them
        // in Drop. The retained VMRef keeps CPU/DMA backing alive even when
        // teardown fails. Static allocations never migrate or use COW.
        let backing = unsafe { DmaBacking::from_pages(pages, 4096, permissions, vm.clone())? };
        assignment.map_memory(region.gpa.as_usize() as u64, backing, permissions)?;
    }
    assignment.attach()
}

/// Initialize configured devices and the VM before any vCPU starts.
pub fn init(vm: &VMRef) -> AxResult {
    let configs = vm.with_config(|c| c.pass_through_devices().clone());
    if configs.is_empty() {
        return vm.init();
    }
    validate(vm, &configs)?;
    {
        let mut table = ASSIGNMENTS.lock();
        if table.contains_key(&vm.id()) {
            return Err(AxError::ResourceBusy);
        }
        table.insert(vm.id(), None);
    }
    let mut resources = Assignments::default();
    let assignments = &mut resources.devices;
    let result = (|| {
        let mut indices = BTreeMap::new();
        let mut windows: Vec<Arc<dyn BaseMmioDeviceOps>> = Vec::new();
        let mut ports: Vec<Arc<dyn BasePortDeviceOps>> = Vec::new();
        #[cfg(target_arch = "x86_64")]
        let mut pci_functions = BTreeMap::new();
        for config in &configs {
            let binding = config.assignment.as_ref().ok_or(AxError::InvalidInput)?;
            let index = match indices.get(&binding.id) {
                Some(&index) => index,
                None => {
                    let index = assignments.len();
                    assignments.push(DeviceAssignment::open(&device_id(&binding.id)?)?);
                    indices.insert(&binding.id, index);
                    index
                }
            };
            let assignment = &assignments[index];
            let region = assignment
                .info()
                .regions
                .iter()
                .find(|r| r.index == binding.region)
                .ok_or(AxError::NotFound)?;
            if region.kind != RegionKind::Mmio || config.length as u64 > region.size {
                return Err(AxError::InvalidInput);
            }
            let range = (config.base_gpa..config.base_gpa + config.length)
                .try_into()
                .map_err(|_| AxError::InvalidInput)?;
            #[cfg(target_arch = "x86_64")]
            if let Some([bus, device, function]) = binding.guest_pci {
                let bdf = (u16::from(bus) << 8) | (u16::from(device) << 3) | u16::from(function);
                let function = match pci_functions.entry(bdf) {
                    alloc::collections::btree_map::Entry::Vacant(entry) => {
                        let source = configs
                            .iter()
                            .filter_map(|c| c.assignment.as_ref())
                            .find_map(|a| (a.id == binding.id).then_some(a.irq_source).flatten())
                            .ok_or(AxError::InvalidInput)?;
                        let device_configs: Vec<_> = configs
                            .iter()
                            .filter(|c| c.assignment.as_ref().is_some_and(|a| a.id == binding.id))
                            .cloned()
                            .collect();
                        let (function, mappings) =
                            pci::new_function(assignment, vm, source, &device_configs)?;
                        resources.bars.push(mappings);
                        entry.insert(Arc::new(function))
                    }
                    alloc::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                };
                windows.push(Arc::new(pci::add_bar(
                    function,
                    assignment,
                    binding.region,
                    range,
                )?));
                continue;
            }
            windows.push(Arc::new(MmioWindow {
                range,
                region: binding.region,
                access: assignment.access(),
            }));
        }
        #[cfg(target_arch = "x86_64")]
        if !pci_functions.is_empty() {
            ports.push(Arc::new(axdevice::pci::PciBus::new(pci_functions.clone())?));
        }
        vm.init_with_assigned_devices(windows, core::mem::take(&mut ports))?;
        for assignment in &mut *assignments {
            map_ram(vm, assignment)?;
        }
        for config in &configs {
            let binding = config.assignment.as_ref().ok_or(AxError::InvalidInput)?;
            if let Some(source) = binding.irq_source {
                let assignment = &mut assignments[indices[&binding.id]];
                if !assignment
                    .info()
                    .interrupts
                    .iter()
                    .any(|i| i.index == source && i.kind == InterruptKind::Edge)
                {
                    return Err(AxError::Unsupported);
                }
                let weak_vm = Arc::downgrade(vm);
                let gsi = config.irq_id;
                #[cfg(target_arch = "x86_64")]
                let pci_function = binding.guest_pci.map(|[bus, device, function]| {
                    pci_functions
                        [&((u16::from(bus) << 8) | (u16::from(device) << 3) | u16::from(function))]
                        .clone()
                });
                let irq = assignment.bind_interrupt(
                    source,
                    Box::new(move || {
                        #[cfg(target_arch = "x86_64")]
                        if let Some(function) = &pci_function {
                            function.interrupt();
                            return;
                        }
                        if let Some(vm) = weak_vm.upgrade() {
                            #[cfg(target_arch = "x86_64")]
                            super::x86::inject_assigned_gsi(&vm, gsi);
                            #[cfg(not(target_arch = "x86_64"))]
                            let _ = (&vm, gsi);
                        }
                    }),
                )?;
                assignment.set_interrupt_mask(irq, false)?;
            }
        }
        Ok(())
    })();
    *ASSIGNMENTS.lock().get_mut(&vm.id()).unwrap() = Some(resources);
    if result.is_err() {
        // No vCPU has started. Keep failed cleanup in the table for retry;
        // host DMA leases keep its VM and RAM alive.
        if let Err(error) = close(vm.id()) {
            warn!("VM[{}] assignment cleanup: {error:?}", vm.id());
        }
    }
    result
}

/// Release assignments after every vCPU has stopped and joined. A failed
/// close keeps the remaining resources available for a subsequent retry.
pub fn close(vm_id: usize) -> AxResult {
    let mut assignments = {
        let mut table = ASSIGNMENTS.lock();
        let Some(entry) = table.get_mut(&vm_id) else {
            return Ok(());
        };
        entry.take().ok_or(AxError::ResourceBusy)?
    };
    #[cfg(target_arch = "x86_64")]
    for mappings in &assignments.bars {
        if let Err(error) = mappings.revoke() {
            *ASSIGNMENTS.lock().get_mut(&vm_id).unwrap() = Some(assignments);
            return Err(error);
        }
    }
    while let Some(assignment) = assignments.devices.last_mut() {
        if let Err(error) = assignment.close() {
            *ASSIGNMENTS.lock().get_mut(&vm_id).unwrap() = Some(assignments);
            return Err(error);
        }
        assignments.devices.pop();
    }
    ASSIGNMENTS.lock().remove(&vm_id);
    Ok(())
}
