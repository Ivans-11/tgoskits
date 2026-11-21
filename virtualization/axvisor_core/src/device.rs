// SPDX-License-Identifier: Apache-2.0

//! Device assignment lifecycle shared by in-kernel and userspace frontends.
//!
//! Frontends serialize management calls in sleepable context, implement guest
//! configuration semantics and forward trapped register accesses here. IRQ
//! callbacks carry their own virtual interrupt route. This module does not
//! depend on a control fd, eventfd, guest architecture or userspace runtime.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axvisor_api::device::{self, *};

/// Revocable register access retained by a trapped guest device model.
pub struct DeviceAccess {
    handle: DeviceHandle,
    closed: AtomicBool,
    users: AtomicUsize,
    interrupts: SpinNoIrq<BTreeMap<u32, IrqHandle>>,
}

impl DeviceAccess {
    fn access<T>(&self, f: impl FnOnce(DeviceHandle) -> AxResult<T>) -> AxResult<T> {
        self.users.fetch_add(1, Ordering::SeqCst);
        let result = if self.closed.load(Ordering::SeqCst) {
            Err(AxError::BadState)
        } else {
            f(self.handle)
        };
        self.users.fetch_sub(1, Ordering::SeqCst);
        result
    }

    /// Read an authorized register; fails after assignment teardown begins.
    pub fn read(&self, region: u32, offset: u64, width: AccessWidth) -> AxResult<u64> {
        self.access(|handle| device::read_region(handle, region, offset, width))
    }

    /// Write an authorized register; fails after assignment teardown begins.
    pub fn write(&self, region: u32, offset: u64, width: AccessWidth, value: u64) -> AxResult {
        self.access(|handle| device::write_region(handle, region, offset, width, value))
    }

    /// Update a bound source while preventing concurrent assignment teardown.
    pub fn set_interrupt_target(&self, source: u32, target: Option<InterruptTarget>) -> AxResult {
        self.access(|_| {
            let irq = self
                .interrupts
                .lock()
                .get(&source)
                .copied()
                .ok_or(AxError::NotFound)?;
            device::set_irq_target(irq, target)
        })
    }
}

/// An explicitly closed assignment. Failure during close preserves the exact
/// remaining resources so the caller can retry. The host retains DMA leases
/// and callbacks until their respective release operations succeed.
///
/// The frontend must stop guest access and revoke any direct CPU mappings
/// before closing. It must not perform teardown from an IRQ callback or while
/// holding a lock needed by one. Dropping this object does not implicitly run
/// sleepable teardown or free hardware resources; always call `close`.
#[must_use = "device assignments require explicit close after guest access stops"]
pub struct DeviceAssignment {
    device: Option<DeviceHandle>,
    info: DeviceInfo,
    space: Option<DmaSpace>,
    constraints: Option<DmaConstraints>,
    mappings: Vec<DmaMapping>,
    interrupts: Vec<IrqHandle>,
    attached: bool,
    closing: bool,
    access: Arc<DeviceAccess>,
}

impl DeviceAssignment {
    /// Acquire only the device authorized by the host.
    pub fn open(id: &DeviceId) -> AxResult<Self> {
        let (device, info) = device::open_device(id)?;
        Ok(Self {
            device: Some(device),
            info,
            space: None,
            constraints: None,
            mappings: Vec::new(),
            interrupts: Vec::new(),
            attached: false,
            closing: false,
            access: Arc::new(DeviceAccess {
                handle: device,
                closed: AtomicBool::new(false),
                users: AtomicUsize::new(0),
                interrupts: SpinNoIrq::new(BTreeMap::new()),
            }),
        })
    }

    /// Authorized resources used to construct the guest's device view.
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Obtain revocable register access for a guest device model.
    pub fn access(&self) -> Arc<DeviceAccess> {
        self.access.clone()
    }

    fn live_device(&self) -> AxResult<DeviceHandle> {
        if self.closing {
            return Err(AxError::BadState);
        }
        self.device.ok_or(AxError::BadState)
    }

    /// Add fixed DMA backing before attaching the device. The frontend must
    /// map these same physical pages into the guest's CPU address space.
    pub fn map_memory(
        &mut self,
        iova: u64,
        backing: DmaBacking,
        permissions: AccessPermissions,
    ) -> AxResult {
        self.live_device()?;
        if !self.info.dma {
            return Err(AxError::Unsupported);
        }
        if self.attached {
            return Err(AxError::ResourceBusy);
        }
        if self.space.is_none() {
            let (space, constraints) = device::create_dma_space()?;
            self.space = Some(space);
            self.constraints = Some(constraints);
        }
        let mapping = device::map_dma(self.space.unwrap(), iova, backing, permissions)?;
        self.mappings.push(mapping);
        Ok(())
    }

    /// Initial mapping constraints, available after creating a DMA space.
    pub fn dma_constraints(&self) -> Option<DmaConstraints> {
        self.constraints
    }

    /// Attach the prepared DMA space. All mapping changes are now prohibited
    /// until teardown. The host checks device-specific constraints here.
    pub fn attach(&mut self) -> AxResult {
        let device = self.live_device()?;
        if self.attached {
            return Err(AxError::ResourceBusy);
        }
        let space = self.space.ok_or(AxError::BadState)?;
        device::attach_device(device, space)?;
        self.attached = true;
        Ok(())
    }

    /// Bind an initially masked source to a frontend-provided guest route.
    /// The callback must retain every object that it accesses.
    pub fn bind_interrupt(
        &mut self,
        source: u32,
        callback: DeviceIrqCallback,
    ) -> AxResult<IrqHandle> {
        let device = self.live_device()?;
        if self.access.interrupts.lock().contains_key(&source) {
            return Err(AxError::AlreadyExists);
        }
        let irq = device::bind_irq(device, source, callback)?;
        self.interrupts.push(irq);
        self.access.interrupts.lock().insert(source, irq);
        Ok(irq)
    }

    /// Change delivery state for one of this assignment's interrupt sources.
    pub fn set_interrupt_mask(&self, irq: IrqHandle, masked: bool) -> AxResult {
        self.live_device()?;
        if !self.interrupts.contains(&irq) {
            return Err(AxError::NotFound);
        }
        device::set_irq_mask(irq, masked)
    }

    /// Forward a trapped, host-authorized register read.
    pub fn read(&self, region: u32, offset: u64, width: AccessWidth) -> AxResult<u64> {
        device::read_region(self.live_device()?, region, offset, width)
    }

    /// Forward a trapped, host-authorized register write.
    pub fn write(&self, region: u32, offset: u64, width: AccessWidth, value: u64) -> AxResult {
        device::write_region(self.live_device()?, region, offset, width, value)
    }

    /// Stop callbacks and DMA, then release mappings, space and ownership.
    ///
    /// Stop guest accesses and synchronize revocation of direct CPU mappings
    /// before calling. Once close starts, further register/mapping/bind calls
    /// fail even if teardown needs retry. Each completed step is recorded;
    /// failed steps and their dependencies remain held, never silently freed.
    pub fn close(&mut self) -> AxResult {
        self.closing = true;
        self.access.closed.store(true, Ordering::SeqCst);
        if self.access.users.load(Ordering::SeqCst) != 0 {
            return Err(AxError::ResourceBusy);
        }
        while let Some(&irq) = self.interrupts.last() {
            device::unbind_irq(irq)?;
            self.interrupts.pop();
        }
        if self.attached {
            device::detach_device(self.device.ok_or(AxError::BadState)?)?;
            self.attached = false;
        }
        while let Some(&mapping) = self.mappings.last() {
            device::unmap_dma(mapping)?;
            self.mappings.pop();
        }
        if let Some(space) = self.space {
            device::destroy_dma_space(space)?;
            self.space = None;
            self.constraints = None;
        }
        if let Some(device) = self.device {
            device::close_device(device)?;
            self.device = None;
        }
        Ok(())
    }

    /// Whether explicit teardown has released every host resource.
    pub fn is_closed(&self) -> bool {
        self.device.is_none()
    }
}
