// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "device")]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use ax_errno::{AxError, AxResult};
use axvisor_api::device::*;
use axvisor_core::device::DeviceAssignment;

// Failure injection verifies teardown dependencies rather than merely matching
// call counts: the backend refuses to release memory while DMA is attached.
#[derive(Default)]
struct State {
    open: bool,
    space: bool,
    attached: bool,
    irq: Option<DeviceIrqCallback>,
    backing: Option<DmaBacking>,
    fail_detach: bool,
    fail_unbind: bool,
    fail_unmap: bool,
    fail_map: bool,
    fail_attach: bool,
    fail_close: bool,
}

static TEST_LOCK: Mutex<()> = Mutex::new(());
static HOLD_READ: AtomicBool = AtomicBool::new(false);
static READ_STARTED: AtomicBool = AtomicBool::new(false);

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

const READ_WRITE: AccessPermissions = AccessPermissions {
    read: true,
    write: true,
};

struct Host;
const DEVICE: DeviceId = DeviceId::Pci(PciAddress {
    segment: 0,
    bus: 1,
    device: 0,
    function: 0,
});
const OTHER_DEVICE: DeviceId = DeviceId::Pci(PciAddress {
    segment: 0,
    bus: 2,
    device: 0,
    function: 0,
});

#[axvisor_api::api_impl]
impl DeviceIf for Host {
    fn open_device(id: &DeviceId) -> AxResult<(DeviceHandle, DeviceInfo)> {
        let mut s = STATE.lock().unwrap();
        if *id != DEVICE {
            return Err(AxError::PermissionDenied);
        }
        if s.open {
            return Err(AxError::ResourceBusy);
        }
        s.open = true;
        Ok((
            DeviceHandle(1),
            DeviceInfo {
                regions: Vec::new(),
                interrupts: Vec::new(),
                dma: true,
            },
        ))
    }

    fn close_device(_device: DeviceHandle) -> AxResult {
        let mut s = STATE.lock().unwrap();
        assert!(!s.attached && s.irq.is_none() && s.backing.is_none());
        if s.fail_close {
            return Err(AxError::Io);
        }
        s.open = false;
        Ok(())
    }

    fn read_region(
        _device: DeviceHandle,
        _region: u32,
        _offset: u64,
        _width: AccessWidth,
    ) -> AxResult<u64> {
        READ_STARTED.store(true, Ordering::Release);
        while HOLD_READ.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        Ok(0)
    }

    fn write_region(
        _device: DeviceHandle,
        _region: u32,
        _offset: u64,
        _width: AccessWidth,
        _value: u64,
    ) -> AxResult {
        Ok(())
    }

    fn bind_irq(
        _device: DeviceHandle,
        _source: u32,
        callback: DeviceIrqCallback,
    ) -> AxResult<IrqHandle> {
        let mut s = STATE.lock().unwrap();
        if s.irq.is_some() {
            return Err(AxError::ResourceBusy);
        }
        s.irq = Some(callback);
        Ok(IrqHandle(1))
    }

    fn set_irq_mask(_irq: IrqHandle, _masked: bool) -> AxResult {
        Ok(())
    }

    fn unbind_irq(_irq: IrqHandle) -> AxResult {
        let mut s = STATE.lock().unwrap();
        if s.fail_unbind {
            return Err(AxError::Io);
        }
        s.irq = None;
        Ok(())
    }

    fn create_dma_space() -> AxResult<(DmaSpace, DmaConstraints)> {
        STATE.lock().unwrap().space = true;
        Ok((
            DmaSpace(1),
            DmaConstraints {
                start: 0,
                end: u32::MAX as u64,
                granule: 4096,
            },
        ))
    }

    fn destroy_dma_space(_space: DmaSpace) -> AxResult {
        let mut s = STATE.lock().unwrap();
        assert!(!s.attached && s.backing.is_none());
        s.space = false;
        Ok(())
    }

    fn map_dma(
        _space: DmaSpace,
        _iova: u64,
        backing: DmaBacking,
        _permissions: AccessPermissions,
    ) -> AxResult<DmaMapping> {
        let mut s = STATE.lock().unwrap();
        assert!(!s.attached);
        if s.fail_map {
            return Err(AxError::Io);
        }
        s.backing = Some(backing);
        Ok(DmaMapping(1))
    }

    fn unmap_dma(_mapping: DmaMapping) -> AxResult {
        let mut s = STATE.lock().unwrap();
        assert!(!s.attached);
        if s.fail_unmap {
            return Err(AxError::Io);
        }
        s.backing = None;
        Ok(())
    }

    fn attach_device(_device: DeviceHandle, _space: DmaSpace) -> AxResult {
        let mut s = STATE.lock().unwrap();
        assert!(s.backing.is_some());
        if s.fail_attach {
            return Err(AxError::Io);
        }
        s.attached = true;
        Ok(())
    }

    fn detach_device(_device: DeviceHandle) -> AxResult {
        let mut s = STATE.lock().unwrap();
        assert!(s.irq.is_none());
        if s.fail_detach {
            return Err(AxError::Io);
        }
        s.attached = false;
        Ok(())
    }
}

struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn optional_hardware_target_keeps_legacy_host_compatible() {
    let _serial = TEST_LOCK.lock().unwrap();
    *STATE.lock().unwrap() = State::default();
    let mut assignment = DeviceAssignment::open(&DEVICE).unwrap();
    assignment.bind_interrupt(0, Box::new(|| {})).unwrap();
    let access = assignment.access();
    // This host intentionally omits set_irq_target. Its weak default must
    // link and return Unsupported without disturbing the live IRQ binding.
    assert_eq!(
        access.set_interrupt_target(0, None),
        Err(AxError::Unsupported)
    );
    assert!(STATE.lock().unwrap().irq.is_some());
    assignment.close().unwrap();
    assert_eq!(access.set_interrupt_target(0, None), Err(AxError::BadState));
}

#[test]
fn close_revokes_register_access_and_preserves_an_inflight_reader() {
    let _serial = TEST_LOCK.lock().unwrap();
    *STATE.lock().unwrap() = State::default();
    let mut assignment = DeviceAssignment::open(&DEVICE).unwrap();
    let access = assignment.access();
    READ_STARTED.store(false, Ordering::Release);
    HOLD_READ.store(true, Ordering::Release);
    let reader = access.clone();
    let thread = std::thread::spawn(move || reader.read(0, 0, AccessWidth::Dword));
    while !READ_STARTED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    let result = assignment.close();
    // Always release the worker before asserting so a failed test cannot hang.
    HOLD_READ.store(false, Ordering::Release);
    assert_eq!(thread.join().unwrap(), Ok(0));
    assert_eq!(result, Err(AxError::ResourceBusy));
    assert!(STATE.lock().unwrap().open);
    assert_eq!(
        access.read(0, 0, AccessWidth::Dword),
        Err(AxError::BadState)
    );
    assignment.close().unwrap();
    assert_eq!(
        access.write(0, 0, AccessWidth::Dword, 0),
        Err(AxError::BadState)
    );
}

fn backing(released: &Arc<AtomicUsize>) -> DmaBacking {
    // SAFETY: mock physical addresses are never dereferenced. The lease
    // counter models the memory owner's lifetime obligation.
    unsafe {
        DmaBacking::from_pages(
            alloc::vec![axvisor_api::memory::PhysAddr::from(0x1000)],
            4096,
            READ_WRITE,
            Arc::new(Lease(released.clone())),
        )
        .unwrap()
    }
}

#[test]
fn teardown_failures_retain_dma_memory_and_allow_retry() {
    let _serial = TEST_LOCK.lock().unwrap();
    *STATE.lock().unwrap() = State::default();
    let released = Arc::new(AtomicUsize::new(0));
    let notified = Arc::new(AtomicUsize::new(0));
    assert!(DeviceAssignment::open(&OTHER_DEVICE).is_err());
    for round in 0..3 {
        let mut assignment = DeviceAssignment::open(&DEVICE).unwrap();
        assert!(DeviceAssignment::open(&DEVICE).is_err());
        assignment
            .map_memory(0x4000, backing(&released), READ_WRITE)
            .unwrap();
        assignment.attach().unwrap();
        let counter = notified.clone();
        assignment
            .bind_interrupt(
                0,
                Box::new(move || {
                    counter.fetch_add(1, Ordering::Relaxed);
                }),
            )
            .unwrap();
        {
            let mut s = STATE.lock().unwrap();
            s.irq.as_ref().unwrap()();
            s.fail_unbind = true;
        }
        assert!(assignment.close().is_err());
        assert!(STATE.lock().unwrap().attached);
        assert_eq!(released.load(Ordering::Relaxed), round);
        assert!(assignment.read(0, 0, AccessWidth::Dword).is_err());
        {
            let mut s = STATE.lock().unwrap();
            s.fail_unbind = false;
            s.fail_detach = true;
        }
        assert!(assignment.close().is_err());
        assert_eq!(released.load(Ordering::Relaxed), round);
        {
            let mut s = STATE.lock().unwrap();
            assert!(s.irq.is_none());
            s.fail_detach = false;
            s.fail_unmap = true;
        }
        assert!(assignment.close().is_err());
        assert!(!STATE.lock().unwrap().attached);
        assert_eq!(released.load(Ordering::Relaxed), round);
        {
            let mut s = STATE.lock().unwrap();
            s.fail_unmap = false;
            s.fail_close = true;
        }
        assert!(assignment.close().is_err());
        assert!(!assignment.is_closed());
        assert!(STATE.lock().unwrap().open);
        assert_eq!(released.load(Ordering::Relaxed), round + 1);
        STATE.lock().unwrap().fail_close = false;
        assignment.close().unwrap();
        assignment.close().unwrap();
        assert!(assignment.is_closed());
        assert_eq!(released.load(Ordering::Relaxed), round + 1);
        assert_eq!(notified.load(Ordering::Relaxed), round + 1);
    }
}

#[test]
fn setup_failure_preserves_ownership_until_explicit_cleanup() {
    let _serial = TEST_LOCK.lock().unwrap();
    *STATE.lock().unwrap() = State::default();
    let released = Arc::new(AtomicUsize::new(0));
    let mut assignment = DeviceAssignment::open(&DEVICE).unwrap();
    STATE.lock().unwrap().fail_map = true;
    assert!(
        assignment
            .map_memory(0x4000, backing(&released), READ_WRITE)
            .is_err()
    );
    assert_eq!(released.load(Ordering::Relaxed), 1);
    {
        let mut s = STATE.lock().unwrap();
        assert!(s.space && s.open && s.backing.is_none());
        s.fail_map = false;
        s.fail_attach = true;
    }
    assignment
        .map_memory(0x4000, backing(&released), READ_WRITE)
        .unwrap();
    assert!(assignment.attach().is_err());
    assert_eq!(released.load(Ordering::Relaxed), 1);
    assignment.close().unwrap();
    assert_eq!(released.load(Ordering::Relaxed), 2);
    let s = STATE.lock().unwrap();
    assert!(!s.open && !s.space && !s.attached && s.backing.is_none());
}
