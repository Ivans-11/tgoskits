// SPDX-License-Identifier: Apache-2.0

//! bhyve control frontend, independent of the KVM device protocol and file state.

mod abi;
mod memory;
mod platform;
mod vcpu;

use alloc::{
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::Arc,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axvisor_api::control::{self, ControlFileId, ControlOps, EndpointId, MmapArea, MmapProtection};
use axvm::{AxVM, AxVMRef, VMStatus, config::AxVMConfig};
use memory::Memory;
use spin::Mutex;

use crate::control::OperationLock;

static REGISTERED: AtomicBool = AtomicBool::new(false);
static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
static VMS: SpinNoIrq<BTreeMap<usize, Arc<Vm>>> = SpinNoIrq::new(BTreeMap::new());
static FILES: Mutex<BTreeMap<ControlFileId, File>> = Mutex::new(BTreeMap::new());

const OPS: ControlOps = ControlOps {
    open,
    close,
    ioctl,
    mmap: Some(mmap),
};

struct Vm {
    // Destroy the guest page tables before releasing the pinned backing pages.
    machine: AxVMRef,
    name: String,
    operation: Arc<OperationLock>,
    endpoint: Mutex<Option<EndpointId>>,
    removed: AtomicBool,
    memory: Mutex<Memory>,
    resume_rip: Mutex<Option<u64>>,
    cpu_state: vcpu::State,
    rtc: Mutex<x86_vlapic::rtc::Rtc>,
    pending_interrupts: SpinNoIrq<VecDeque<crate::vmm::interrupt::VirtualInterrupt>>,
}

#[derive(Clone)]
enum File {
    Manager,
    Vm(Arc<Vm>),
    Memory(Arc<Vm>, u32),
}

/// Publishes the management endpoint used by the Linux sysctl compatibility shim.
/// VM files retain the FreeBSD `/dev/vmm/<name>` interface.
pub fn init() -> AxResult {
    if REGISTERED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    if let Err(error) = control::register_endpoint("bhyve", 0, OPS) {
        REGISTERED.store(false, Ordering::Release);
        return Err(error);
    }
    Ok(())
}

fn new_file(file: File) -> AxResult<ControlFileId> {
    let id = NEXT_FILE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| AxError::NoMemory)?;
    FILES.lock().insert(id, file);
    Ok(id)
}

fn open(context: u64) -> AxResult<ControlFileId> {
    if context == 0 {
        return new_file(File::Manager);
    }
    let vm = VMS
        .lock()
        .get(&(context as usize))
        .cloned()
        .ok_or(AxError::NoSuchDevice)?;
    if vm.removed.load(Ordering::Acquire) {
        return Err(AxError::NoSuchDevice);
    }
    new_file(File::Vm(vm))
}

fn close(id: ControlFileId) -> AxResult {
    let file = FILES.lock().remove(&id).ok_or(AxError::NotFound)?;
    drop(file);
    Ok(())
}

fn file(id: ControlFileId) -> AxResult<File> {
    FILES.lock().get(&id).cloned().ok_or(AxError::NotFound)
}

fn create(name: &str) -> AxResult {
    if name.is_empty() || name.len() > 229 {
        return Err(AxError::InvalidInput);
    }
    let id = crate::control::next_vm_id()?;
    let mut config = AxVMConfig::new_host_controlled(id, format!("bhyve-{name}"), 1);
    config.kvm_pv = false;
    let machine = AxVM::new(config)?;
    machine.init()?;
    machine.set_vm_status(VMStatus::Loaded);
    let vm = Arc::new(Vm {
        machine,
        name: name.into(),
        operation: OperationLock::new(),
        endpoint: Mutex::new(None),
        removed: AtomicBool::new(false),
        memory: Mutex::new(Memory::default()),
        resume_rip: Mutex::new(None),
        cpu_state: vcpu::State::default(),
        rtc: Mutex::new(x86_vlapic::rtc::Rtc::default()),
        pending_interrupts: SpinNoIrq::new(VecDeque::new()),
    });
    {
        let mut vms = VMS.lock();
        if vms.values().any(|existing| existing.name == name) {
            return Err(AxError::AlreadyExists);
        }
        vms.insert(id, vm.clone());
    }
    match control::register_endpoint(&format!("vmm/{name}"), id as u64, OPS) {
        Ok(endpoint) => {
            *vm.endpoint.lock() = Some(endpoint);
            Ok(())
        }
        Err(error) => {
            VMS.lock().remove(&id);
            Err(error)
        }
    }
}

pub(crate) fn queue_interrupt(route: crate::vmm::interrupt::InterruptRoute) -> bool {
    let Some(vm) = VMS.lock().get(&route.vm_id).cloned() else {
        return false;
    };
    if route.vcpu_id == 0 && !vm.removed.load(Ordering::Acquire) {
        let mut pending = vm.pending_interrupts.lock();
        if !pending.contains(&route.interrupt) {
            pending.push_back(route.interrupt);
        }
    }
    true
}

pub(crate) fn active_vcpus(vm_id: usize) -> Option<usize> {
    VMS.lock()
        .get(&vm_id)
        .map(|vm| usize::from(vm.cpu_state.active.load(Ordering::Acquire)))
}

fn destroy(name: &str) -> AxResult {
    let vm = VMS
        .lock()
        .values()
        .find(|vm| vm.name == name)
        .cloned()
        .ok_or(AxError::NotFound)?;
    // Endpoint publication may still be in progress in another create call.
    if vm.endpoint.lock().is_none() && !vm.removed.load(Ordering::Acquire) {
        return Err(AxError::ResourceBusy);
    }
    vm.removed.store(true, Ordering::Release);
    let _guard = vm.operation.lock();
    // Another destroy may have completed while this request was waiting.
    // Copy under the operation lock, and release the endpoint mutex before
    // unregistering: host teardown may wait for in-flight opens.
    let endpoint = *vm.endpoint.lock();
    if let Some(endpoint) = endpoint {
        control::unregister_endpoint(endpoint)?;
        *vm.endpoint.lock() = None;
    }
    let endpoints = vm.memory.lock().endpoints();
    for (segment, endpoint, context) in endpoints {
        control::unregister_endpoint(endpoint)?;
        vm.memory.lock().forget_endpoint(segment);
        close(context)?;
    }
    VMS.lock().remove(&vm.machine.id());
    Ok(())
}

fn ioctl(id: ControlFileId, command: u32, address: usize) -> AxResult<isize> {
    match file(id)? {
        File::Manager => {
            if command != abi::CREATE && command != abi::DESTROY {
                return Err(AxError::Unsupported);
            }
            let bytes = abi::read::<256>(address)?;
            let name = abi::name(&bytes)?;
            if command == abi::CREATE {
                create(name)?;
            } else {
                destroy(name)?;
            }
        }
        File::Vm(vm) => {
            if vm.removed.load(Ordering::Acquire) {
                return Err(AxError::NoSuchDevice);
            }
            // These state transitions must also work while VM_RUN owns the
            // operation lock (in particular resume and suspend requests).
            if vcpu::state_ioctl(&vm, command, address)?
                || platform::irq_ioctl(&vm, command, address)?
            {
                return Ok(0);
            }
            let _guard = vm.operation.lock();
            if vm.removed.load(Ordering::Acquire) {
                return Err(AxError::NoSuchDevice);
            }
            match command {
                abi::ALLOC_MEMSEG
                | abi::GET_MEMSEG
                | abi::MMAP_MEMSEG
                | abi::MMAP_GETNEXT
                | abi::MUNMAP_MEMSEG => memory::ioctl(&vm, command, address)?,
                abi::RTC_READ | abi::RTC_WRITE | abi::RTC_SETTIME | abi::RTC_GETTIME => {
                    platform::ioctl(&vm, command, address)?;
                }
                _ => vcpu::ioctl(&vm, command, address)?,
            }
        }
        File::Memory(..) => return Err(AxError::Unsupported),
    }
    Ok(0)
}

fn mmap(
    id: ControlFileId,
    offset: usize,
    len: usize,
    protection: MmapProtection,
    shared: bool,
) -> AxResult<(Arc<MmapArea>, usize, MmapProtection)> {
    let (vm, segment) = match file(id)? {
        File::Vm(vm) => (vm, None),
        File::Memory(vm, segment) => (vm, Some(segment)),
        File::Manager => return Err(AxError::NoSuchDevice),
    };
    let _guard = vm.operation.lock();
    if vm.removed.load(Ordering::Acquire) {
        return Err(AxError::NoSuchDevice);
    }
    // bhyve requires shared RAM/ROM; a private COW mapping would diverge from guest RAM.
    if !shared {
        return Err(AxError::InvalidInput);
    }
    vm.memory.lock().resolve(segment, offset, len, protection)
}
