// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Shared runtime support for userspace-controlled VMs.
//!
//! Owns VM identities, operation synchronization and vCPU interrupt/wakeup
//! state. Frontends retain file descriptors and protocol-specific power state;
//! common VMM paths access this module without depending on a frontend.

use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axvisor_api::sync::WaitQueue;
use vm_interrupt::{InterruptRoute, VirtualInterrupt};

static NEXT_VM_ID: AtomicUsize = AtomicUsize::new(1);

pub(crate) fn next_vm_id() -> AxResult<usize> {
    NEXT_VM_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| AxError::NoMemory)
}

static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_file_id() -> AxResult<u64> {
    NEXT_FILE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| AxError::OutOfRange)
}

/// Serializes stateful operations without holding a spin lock while sleeping.
pub(crate) struct OperationLock {
    held: AtomicBool,
    wait_queue: WaitQueue,
}

impl OperationLock {
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn try_lock(self: &Arc<Self>) -> Option<OperationGuard> {
        self.held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| OperationGuard {
                lock: Arc::clone(self),
            })
    }

    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            held: AtomicBool::new(false),
            wait_queue: WaitQueue::new(),
        })
    }

    pub(crate) fn lock(self: &Arc<Self>) -> OperationGuard {
        if self
            .held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            let lock = Arc::clone(self);
            self.wait_queue.wait_until(move || {
                lock.held
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            });
        }
        OperationGuard {
            lock: Arc::clone(self),
        }
    }
}

pub(crate) struct OperationGuard {
    lock: Arc<OperationLock>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.lock.held.store(false, Ordering::Release);
        self.lock.wait_queue.wake_one();
    }
}

static VMS: SpinNoIrq<BTreeMap<usize, Weak<VmState>>> = SpinNoIrq::new(BTreeMap::new());

pub(crate) struct VmState {
    id: usize,
    closed: AtomicBool,
    /// Frontends may stop accepting interrupts before endpoint teardown finishes.
    pub(crate) removing: AtomicBool,
    ignore_missing_vcpu: bool,
    vcpus: SpinNoIrq<BTreeMap<usize, Arc<VcpuState>>>,
}

impl VmState {
    pub(crate) fn new(id: usize, ignore_missing_vcpu: bool) -> AxResult<Arc<Self>> {
        let state = Arc::new(Self {
            id,
            closed: AtomicBool::new(false),
            removing: AtomicBool::new(false),
            ignore_missing_vcpu,
            vcpus: SpinNoIrq::new(BTreeMap::new()),
        });
        let mut vms = VMS.lock();
        if vms.get(&id).is_some_and(|vm| vm.strong_count() != 0) {
            // Release the registry before dropping the unused state.
            drop(vms);
            return Err(AxError::AlreadyExists);
        }
        vms.insert(id, Arc::downgrade(&state));
        Ok(state)
    }

    pub(crate) fn add_vcpu(
        &self,
        id: usize,
        active: bool,
        coalesce: bool,
    ) -> AxResult<Arc<VcpuState>> {
        let mut vcpus = self.vcpus.lock();
        if self.closed.load(Ordering::Acquire) {
            return Err(AxError::BadState);
        }
        if vcpus.contains_key(&id) {
            return Err(AxError::AlreadyExists);
        }
        let state = Arc::new(VcpuState::new(active, coalesce));
        vcpus.insert(id, state.clone());
        Ok(state)
    }

    pub(crate) fn remove_vcpu(&self, id: usize) {
        let state = self.vcpus.lock().remove(&id);
        if let Some(state) = state {
            state.close();
        }
    }

    /// Stop discovery before releasing frontend resources. Outstanding users
    /// may retain the state, but cannot queue interrupts into a retired vCPU.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let mut vms = VMS.lock();
        if vms
            .get(&self.id)
            .is_some_and(|vm| core::ptr::eq(vm.as_ptr(), self))
        {
            vms.remove(&self.id);
        }
        drop(vms);
        let vcpus = core::mem::take(&mut *self.vcpus.lock());
        for state in vcpus.into_values() {
            state.close();
        }
    }
}

impl Drop for VmState {
    fn drop(&mut self) {
        self.close();
    }
}

struct Events {
    pending: VecDeque<VirtualInterrupt>,
    halted: bool,
    closed: bool,
}

pub(crate) struct VcpuState {
    // "Active" means present/activated for interrupt routing, not MP_STATE
    // runnable. A stopped KVM secondary remains an interrupt destination.
    active: AtomicBool,
    coalesce: bool,
    events: SpinNoIrq<Events>,
    #[cfg(target_arch = "aarch64")]
    pub(crate) wait_queue: WaitQueue,
    #[cfg(target_arch = "aarch64")]
    pub(crate) entry_blocked: AtomicBool,
    #[cfg(target_arch = "aarch64")]
    active_cpu: AtomicUsize,
}

impl VcpuState {
    fn new(active: bool, coalesce: bool) -> Self {
        Self {
            active: AtomicBool::new(active),
            coalesce,
            events: SpinNoIrq::new(Events {
                pending: VecDeque::new(),
                halted: false,
                closed: false,
            }),
            #[cfg(target_arch = "aarch64")]
            wait_queue: WaitQueue::new(),
            #[cfg(target_arch = "aarch64")]
            entry_blocked: AtomicBool::new(true),
            #[cfg(target_arch = "aarch64")]
            active_cpu: AtomicUsize::new(usize::MAX),
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    #[cfg(all(feature = "bhyve", target_arch = "x86_64"))]
    pub(crate) fn activate(&self) -> AxResult {
        if self.active.swap(true, Ordering::AcqRel) {
            return Err(AxError::AlreadyExists);
        }
        Ok(())
    }

    fn queue(&self, interrupt: VirtualInterrupt) -> AxResult {
        let mut events = self.events.lock();
        if events.closed {
            return Err(AxError::NotFound);
        }
        events.halted = false;
        if !self.coalesce || !events.pending.contains(&interrupt) {
            events.pending.push_back(interrupt);
        }
        drop(events);
        #[cfg(target_arch = "aarch64")]
        self.wait_queue.wake_all();
        Ok(())
    }

    pub(crate) fn drain_interrupts(&self) -> Vec<VirtualInterrupt> {
        let mut events = self.events.lock();
        if !events.pending.is_empty() {
            events.halted = false;
        }
        events.pending.drain(..).collect()
    }

    #[cfg(all(feature = "bhyve", target_arch = "x86_64"))]
    pub(crate) fn take_interrupts(&self) -> VecDeque<VirtualInterrupt> {
        core::mem::take(&mut self.events.lock().pending)
    }

    pub(crate) fn set_halted(&self, halted: bool) {
        self.events.lock().halted = halted;
    }

    #[cfg(target_arch = "x86_64")]
    pub(crate) fn wakeup_pending(&self) -> AxResult<bool> {
        let events = self.events.lock();
        if events.closed {
            return Err(AxError::NotFound);
        }
        Ok(!events.halted || !events.pending.is_empty())
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn retain_interrupts(&self, keep: impl FnMut(&VirtualInterrupt) -> bool) {
        self.events.lock().pending.retain(keep);
    }

    fn close(&self) {
        let mut events = self.events.lock();
        events.closed = true;
        events.pending.clear();
    }

    // Sequential consistency pairs stop publication with the IRQ-off guest
    // entry window: either entry observes the stop or the stopper sees its CPU.
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn enter(&self) -> bool {
        self.active_cpu
            .store(axvisor_api::arch::current_cpu_id(), Ordering::SeqCst);
        if self.entry_blocked.load(Ordering::SeqCst) {
            self.leave();
            false
        } else {
            true
        }
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn leave(&self) {
        self.active_cpu.store(usize::MAX, Ordering::SeqCst);
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn kick(&self) -> AxResult {
        let cpu = self.active_cpu.load(Ordering::SeqCst);
        if cpu != usize::MAX {
            axvisor_api::arch::kick_cpu_wait(cpu)?;
        }
        Ok(())
    }
}

fn find_vm(id: usize) -> Option<Arc<VmState>> {
    VMS.lock().get(&id).and_then(Weak::upgrade)
}

pub(crate) fn active_vcpus(id: usize) -> Option<usize> {
    let vm = find_vm(id)?;
    let vcpus = vm.vcpus.lock();
    Some(
        vcpus
            .iter()
            .filter(|(_, state)| state.active())
            .fold(0, |mask, (&id, _)| {
                mask | u32::try_from(id)
                    .ok()
                    .and_then(|id| 1usize.checked_shl(id))
                    .unwrap_or(usize::MAX)
            }),
    )
}

pub(crate) fn queue_interrupt(route: InterruptRoute) -> AxResult {
    let vm = find_vm(route.vm_id).ok_or(AxError::NotFound)?;
    // Some frontends consume routes to absent vCPUs and discard new IRQs as
    // soon as removal starts, while keeping their endpoint registered.
    if vm.removing.load(Ordering::Acquire) {
        return Ok(());
    }
    let vcpu = vm.vcpus.lock().get(&route.vcpu_id).cloned();
    match vcpu {
        Some(vcpu) => vcpu.queue(route.interrupt),
        None if vm.ignore_missing_vcpu => Ok(()),
        None => Err(AxError::NotFound),
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn retirement_does_not_deliver_to_reused_ids() {
        let id = usize::MAX;
        let vm = VmState::new(id, false).unwrap();
        assert_eq!(active_vcpus(id), Some(0));
        assert!(matches!(
            VmState::new(id, false),
            Err(AxError::AlreadyExists)
        ));
        let old = vm.add_vcpu(0, true, false).unwrap();
        let irq = VirtualInterrupt::edge(48);
        let route = InterruptRoute::new(id, 0, irq);
        old.set_halted(true);
        assert_eq!(old.wakeup_pending(), Ok(false));
        queue_interrupt(route).unwrap();
        assert_eq!(old.wakeup_pending(), Ok(true));
        vm.remove_vcpu(0);
        assert_eq!(active_vcpus(id), Some(0));
        assert_eq!(old.queue(irq), Err(AxError::NotFound));
        assert_eq!(old.wakeup_pending(), Err(AxError::NotFound));
        let fresh = vm.add_vcpu(0, true, false).unwrap();
        assert!(fresh.drain_interrupts().is_empty());
        vm.close();
        assert_eq!(active_vcpus(id), None);
        assert_eq!(queue_interrupt(route), Err(AxError::NotFound));
        assert!(matches!(
            vm.add_vcpu(1, true, false),
            Err(AxError::BadState)
        ));
        let replacement = VmState::new(id, false).unwrap();
        drop(vm);
        assert_eq!(active_vcpus(id), Some(0));
        drop(replacement);
        assert_eq!(active_vcpus(id), None);
    }

    #[test]
    fn pending_interrupt_policy_preserves_edges_and_deassertions() {
        let vm = VmState::new(usize::MAX - 1, false).unwrap();
        for (id, coalesce) in [(0, false), (1, true)] {
            let vcpu = vm.add_vcpu(id, !coalesce, coalesce).unwrap();
            #[cfg(feature = "bhyve")]
            if coalesce {
                assert_eq!(active_vcpus(vm.id), Some(1));
                vcpu.activate().unwrap();
                assert_eq!(active_vcpus(vm.id), Some(3));
                assert_eq!(vcpu.activate(), Err(AxError::AlreadyExists));
            }
            let asserted = VirtualInterrupt::edge(48);
            let deasserted = VirtualInterrupt::deassert(48);
            vcpu.queue(asserted).unwrap();
            vcpu.queue(asserted).unwrap();
            vcpu.queue(deasserted).unwrap();
            let expected = if coalesce {
                vec![asserted, deasserted]
            } else {
                vec![asserted, asserted, deasserted]
            };
            assert_eq!(vcpu.drain_interrupts(), expected);
            vcpu.set_halted(true);
            assert_eq!(vcpu.wakeup_pending(), Ok(false));
        }
    }

    #[cfg(feature = "bhyve")]
    #[test]
    fn endpoint_removal_discards_new_interrupts_until_unregistered() {
        let id = usize::MAX - 2;
        let vm = VmState::new(id, true).unwrap();
        let vcpu = vm.add_vcpu(0, false, true).unwrap();
        vcpu.activate().unwrap();
        let irq = VirtualInterrupt::edge(48);
        let route = InterruptRoute::new(id, 0, irq);
        queue_interrupt(InterruptRoute::new(id, 1, irq)).unwrap();
        assert!(vcpu.take_interrupts().is_empty());
        queue_interrupt(route).unwrap();
        queue_interrupt(route).unwrap();
        vm.removing.store(true, Ordering::Release);
        queue_interrupt(InterruptRoute::new(id, 0, VirtualInterrupt::edge(49))).unwrap();
        assert_eq!(active_vcpus(id), Some(1));
        assert_eq!(vcpu.take_interrupts(), VecDeque::from([irq]));
        vm.close();
        assert_eq!(active_vcpus(id), None);
        assert_eq!(queue_interrupt(route), Err(AxError::NotFound));
    }
}
