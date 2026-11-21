// SPDX-License-Identifier: Apache-2.0

//! Connect authorized DeviceIf resources to the axdevice PCI model.

use alloc::sync::{Arc, Weak};
#[cfg(feature = "posted-interrupt")]
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult};
use axaddrspace::GuestPhysAddrRange;
use axdevice::pci::{MsiMessage, PciBackend, PciFunction, PciIdentity, PciWindow};
use axvisor_api::device::{AccessWidth, RegionKind};
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
    #[cfg(feature = "posted-interrupt")]
    source: u32,
    #[cfg(feature = "posted-interrupt")]
    accelerated: AtomicBool,
}

impl PciBackend for Backend {
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
) -> AxResult<PciFunction> {
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
    Ok(PciFunction::new(
        identity,
        Arc::new(Backend {
            access,
            vm: Arc::downgrade(vm),
            #[cfg(feature = "posted-interrupt")]
            source: _source,
            #[cfg(feature = "posted-interrupt")]
            accelerated: AtomicBool::new(false),
        }),
    ))
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
