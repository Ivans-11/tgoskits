// SPDX-License-Identifier: Apache-2.0

//! Connect authorized DeviceIf resources to the axdevice PCI model.

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
#[cfg(feature = "posted-interrupt")]
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axaddrspace::{GuestPhysAddrRange, MappingFlags};
use axdevice::pci::{MsiMessage, PciBackend, PciFunction, PciIdentity, PciWindow};
use axvisor_api::device::{AccessWidth, DirectMapping, RegionKind};
use axvm::config::PassThroughDeviceConfig;
use kvm_uapi::{KvmMsi, x86::decode_msi_route};

use crate::{
    device::{DeviceAccess, DeviceAssignment},
    vmm::{
        VMRef,
        interrupt::{InterruptRoute, VirtualInterrupt, deliver_interrupt},
    },
};

struct Backend {
    access: Arc<DeviceAccess>,
    vm: Weak<crate::vmm::VM>,
    mappings: Arc<BarMappings>,
    #[cfg(feature = "posted-interrupt")]
    source: u32,
    #[cfg(feature = "posted-interrupt")]
    accelerated: AtomicBool,
}

impl PciBackend for Backend {
    fn configure_bars(&self, changes: &[(u32, bool)]) -> AxResult {
        self.mappings.configure(changes)
    }
    #[cfg(feature = "posted-interrupt")]
    fn configure_msi(&self, message: Option<MsiMessage>) -> AxResult {
        let target = message.and_then(|message| {
            let vm = self.vm.upgrade()?;
            let mask = u64::MAX
                .checked_shr(64u32.saturating_sub(vm.vcpu_num() as u32))
                .unwrap_or(0);
            let route = decode_msi_route(
                KvmMsi {
                    address_lo: message.address as u32,
                    address_hi: (message.address >> 32) as u32,
                    data: message.data,
                    flags: 0,
                    devid: 0,
                },
                mask,
            )
            .ok()?;
            if route.level_triggered || route.target_mask.count_ones() != 1 {
                return None;
            }
            let vm_id = vm.id();
            let vcpu_id = route.target_mask.trailing_zeros() as usize;
            axvm::hardware_interrupt_target(
                vm_id,
                vcpu_id,
                route.vector,
                Arc::new(move || {
                    crate::vmm::vcpus::notify_vcpu_wakeup(vm_id, vcpu_id);
                }),
            )
            .ok()
        });
        let enabling = target.is_some();
        let was_enabled = self.accelerated.load(Ordering::Acquire);
        if !enabling && !was_enabled {
            return Ok(());
        }
        match self.access.set_interrupt_target(self.source, target) {
            Ok(()) => {
                self.accelerated.store(enabling, Ordering::Release);
                Ok(())
            }
            Err(AxError::Unsupported) if !was_enabled => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn read_bar(&self, index: u32, offset: u64, width: AccessWidth) -> AxResult<u64> {
        self.access.read(index, offset, width)
    }

    fn write_bar(&self, index: u32, offset: u64, width: AccessWidth, value: u64) -> AxResult {
        self.access.write(index, offset, width, value)
    }

    fn send_msi(&self, message: MsiMessage) -> bool {
        let Some(vm) = self.vm.upgrade() else {
            return false;
        };
        let mask = u64::MAX
            .checked_shr(64u32.saturating_sub(vm.vcpu_num() as u32))
            .unwrap_or(0);
        let msi = KvmMsi {
            address_lo: message.address as u32,
            address_hi: (message.address >> 32) as u32,
            data: message.data,
            flags: 0,
            devid: 0,
        };
        let Ok(route) = decode_msi_route(msi, mask) else {
            return false;
        };
        if route.level_triggered || route.target_mask == 0 {
            return false;
        }
        for vcpu_id in 0..vm.vcpu_num().min(64) {
            if route.target_mask & (1u64 << vcpu_id) != 0 {
                deliver_interrupt(InterruptRoute::new(
                    vm.id(),
                    vcpu_id,
                    VirtualInterrupt::edge(route.vector as usize),
                ));
            }
        }
        true
    }
}

struct MappedBar {
    index: u32,
    base: usize,
    flags: MappingFlags,
    ranges: Vec<(DirectMapping, bool)>,
}

/// CPU mappings owned by the direct frontend, revoked before releasing the
/// assignment. Only the owning, pinned vCPU changes live PCI decoding.
pub(super) struct BarMappings {
    vm: Weak<crate::vmm::VM>,
    bars: SpinNoIrq<Vec<MappedBar>>,
}

impl BarMappings {
    fn new(
        vm: &VMRef,
        assignment: &DeviceAssignment,
        configs: &[PassThroughDeviceConfig],
    ) -> AxResult<Self> {
        let mut bars = Vec::new();
        let cpus = vm.with_config(|c| c.vcpu_topology());
        // Other configurations keep trapped access until they can synchronously
        // revoke EPT translations on every CPU which may run the guest.
        if cfg!(feature = "vmx")
            && cpus.len() == 1
            && cpus[0].1.is_some_and(|set| set.count_ones() == 1)
        {
            for config in configs {
                let binding = config.assignment.as_ref().ok_or(AxError::InvalidInput)?;
                let region = assignment.bar_region(binding.region)?;
                if config.length as u64 != region.size || config.base_gpa & 4095 != 0 {
                    return Err(AxError::InvalidInput);
                }
                let flags = crate::device::mapping::guest_flags(region.permissions);
                let mut ranges = Vec::new();
                for range in region.mappings {
                    let mapping = region.slice(range.offset, range.length, region.permissions)?;
                    let end = mapping.offset + mapping.length as u64;
                    // map_region is not transactional across several leaf
                    // entries. Install one leaf at a time so every completed
                    // CPU mapping is tracked and can be revoked on failure.
                    let mut offset = mapping.offset;
                    let mut physical = mapping.physical_address.as_usize();
                    while offset < end {
                        let huge = 2 * 1024 * 1024;
                        let length = if (config.base_gpa + offset as usize | physical) & (huge - 1)
                            == 0
                            && end - offset >= huge as u64
                        {
                            huge
                        } else {
                            4096
                        };
                        ranges.push((
                            DirectMapping {
                                offset,
                                physical_address: physical.into(),
                                length,
                            },
                            false,
                        ));
                        offset += length as u64;
                        physical += length;
                    }
                }
                bars.push(MappedBar {
                    index: binding.region,
                    base: config.base_gpa,
                    flags,
                    ranges,
                });
            }
        }
        Ok(Self {
            vm: Arc::downgrade(vm),
            bars: SpinNoIrq::new(bars),
        })
    }

    fn configure(&self, changes: &[(u32, bool)]) -> AxResult {
        let mut bars = self.bars.lock();
        if !bars.iter().any(|b| !b.ranges.is_empty()) {
            return Ok(());
        }
        let vm = self.vm.upgrade().ok_or(AxError::BadState)?;
        let result = (|| {
            for &(index, enable) in changes {
                let bar = bars
                    .iter_mut()
                    .find(|b| b.index == index)
                    .ok_or(AxError::NotFound)?;
                for (mapping, mapped) in &mut bar.ranges {
                    if *mapped == enable {
                        continue;
                    }
                    let gpa = (bar.base + mapping.offset as usize).into();
                    if enable {
                        vm.map_region(
                            gpa,
                            mapping.physical_address.as_usize().into(),
                            mapping.length,
                            bar.flags,
                        )?;
                    } else {
                        vm.unmap_region(gpa, mapping.length)?;
                    }
                    *mapped = enable;
                }
            }
            Ok(())
        })();
        // Also flush partial updates before propagating an error. The PCI
        // model rolls back the requested decoding state if a mapping fails.
        #[cfg(feature = "vmx")]
        vm.invalidate_device_mappings()?;
        result
    }

    /// Called only after all vCPU tasks have stopped. A stopped VM cannot be
    /// restarted; any new VM initializes and invalidates its own EPT context.
    pub(super) fn revoke(&self) -> AxResult {
        let mut bars = self.bars.lock();
        if let Some(vm) = self.vm.upgrade() {
            for bar in &mut *bars {
                for (mapping, mapped) in &mut bar.ranges {
                    if *mapped {
                        vm.unmap_region(
                            (bar.base + mapping.offset as usize).into(),
                            mapping.length,
                        )?;
                        *mapped = false;
                    }
                }
            }
        }
        Ok(())
    }
}

fn header_index(assignment: &DeviceAssignment) -> AxResult<u32> {
    assignment
        .info()
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Configuration && r.size >= 64)
        .map(|r| r.index)
        .ok_or(AxError::Unsupported)
}

pub(super) fn new_function(
    assignment: &DeviceAssignment,
    vm: &VMRef,
    _source: u32,
    configs: &[PassThroughDeviceConfig],
) -> AxResult<(PciFunction, Arc<BarMappings>)> {
    let access = assignment.access();
    let header = header_index(assignment)?;
    let read = |offset| {
        access
            .read(header, offset, AccessWidth::Dword)
            .map(|v| v as u32)
    };
    let identity = PciIdentity {
        vendor_device: read(0)?,
        class_revision: read(8)?,
        subsystem: read(0x2c)?,
    };
    let mappings = Arc::new(BarMappings::new(vm, assignment, configs)?);
    let function = PciFunction::new(
        identity,
        Arc::new(Backend {
            access,
            vm: Arc::downgrade(vm),
            mappings: mappings.clone(),
            #[cfg(feature = "posted-interrupt")]
            source: _source,
            #[cfg(feature = "posted-interrupt")]
            accelerated: AtomicBool::new(false),
        }),
    );
    Ok((function, mappings))
}

pub(super) fn add_bar(
    function: &Arc<PciFunction>,
    assignment: &DeviceAssignment,
    index: u32,
    range: GuestPhysAddrRange,
) -> AxResult<PciWindow> {
    let region = assignment
        .info()
        .regions
        .iter()
        .find(|r| r.index == index)
        .ok_or(AxError::NotFound)?;
    if region.kind != RegionKind::Mmio || range.size() as u64 != region.size || index >= 6 {
        return Err(AxError::InvalidInput);
    }
    let flags = assignment.access().read(
        header_index(assignment)?,
        0x10 + u64::from(index) * 4,
        AccessWidth::Dword,
    )? as u32
        & 0xf;
    function.add_bar(index, range, flags)
}
