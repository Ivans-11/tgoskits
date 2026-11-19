//! Host-notified backing. Page resolution is outside both the publication
//! lock and the guest access gate. Notifiers drain only non-sleeping readers.
use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ax_errno::{AxResult, ax_err};
use ax_kspin::SpinNoIrq;
use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags};
use axvisor_api::control::{self as api, MemoryCallbacks, RevocableMemoryOps};
use axvm::AxVMRef;

use super::state::UserspaceMemoryRegion;

struct Slot {
    region: UserspaceMemoryRegion,
    pages: BTreeMap<u64, bool>,
}
#[derive(Default)]
struct State {
    slots: BTreeMap<u32, Slot>,
    epoch: u64,
    invalidating: usize,
    dead: bool,
    #[cfg(feature = "revocable-test")]
    published: usize,
    #[cfg(feature = "revocable-test")]
    revoked: usize,
}
/// One subscription per VM, bound to its creating userspace address space.
pub(super) struct RevocableMemory {
    vm: AxVMRef,
    state: SpinNoIrq<State>,
    ops: RevocableMemoryOps,
    subscription: AtomicU64,
    resolving: AtomicUsize,
}
impl RevocableMemory {
    pub fn new(vm: AxVMRef) -> AxResult<Option<Arc<Self>>> {
        let Some(ops) = api::revocable_memory_ops() else {
            return Ok(None);
        };
        if !vm.supports_notified_backing() {
            // An explicit request must not silently degrade to retained memory.
            return ax_err!(Unsupported);
        }
        let this = Arc::new(Self {
            vm,
            ops,
            state: SpinNoIrq::new(State::default()),
            subscription: AtomicU64::new(0),
            resolving: AtomicUsize::new(0),
        });
        this.vm.guest_access.enable();
        let a = Arc::downgrade(&this);
        let b = a.clone();
        let c = a.clone();
        let id = (ops.register)(MemoryCallbacks {
            start: Box::new(move |start, end| {
                if let Some(m) = a.upgrade() {
                    m.begin(start, end);
                }
            }),
            end: Box::new(move || {
                if let Some(m) = b.upgrade() {
                    m.end();
                }
            }),
            release: Box::new(move || {
                if let Some(m) = c.upgrade() {
                    m.begin(0, usize::MAX);
                    m.state.lock().dead = true; // terminal blocker intentionally remains
                }
            }),
        })?;
        this.subscription.store(id, Ordering::Release);
        Ok(Some(this))
    }
    fn block_access(&self) {
        {
            let mut state = self.state.lock();
            state.invalidating += 1;
            state.epoch = state.epoch.wrapping_add(1);
            self.vm.guest_access.block();
        }
        (self.ops.kick)();
        self.vm.guest_access.drain();
    }
    /// Revoke page-aligned GPA coverage. All areas are individual 4 KiB pages:
    /// range unmap can remove them, including holes, without splitting/allocating.
    fn revoke(&self, slot: &mut Slot, first: u64, last: u64) -> AxResult<usize> {
        let count = slot.pages.range(first..=last).count();
        if count != 0 {
            self.vm.unmap_region(
                GuestPhysAddr::from(first as usize),
                (last - first + 4096) as usize,
            )?;
            // Remove only the invalidated interval. Scanning every resident
            // page for a one-page permission upgrade makes demand-mapped RAM
            // quadratic in the VM's working set (visible during Linux boot).
            // BTreeMap::remove needs no new allocation in notifier context.
            while let Some(gpa) = slot.pages.range(first..=last).next().map(|(&gpa, _)| gpa) {
                slot.pages.remove(&gpa);
            }
            self.vm.guest_access.translation_changed();
        }
        Ok(count)
    }
    fn fail_closed(&self, state: &mut State) {
        if !state.dead {
            state.dead = true;
            self.vm.guest_access.block();
            error!("revocable backing: unmap failed, VM access permanently disabled");
        }
    }
    fn begin(&self, start: usize, end: usize) {
        self.block_access();
        let mut state = self.state.lock();
        let mut failed = false;
        let mut removed = 0;
        // Examine every aliasing slot. Translate the HVA intersection to GPA,
        // rounding outwards to the 4 KiB mapping granule (also for partial ranges).
        for slot in state.slots.values_mut() {
            let r = slot.region;
            let lo = (start as u64).max(r.userspace_addr);
            let hi = (end as u64).min(r.userspace_addr + r.memory_size);
            if lo < hi {
                let first = r.guest_phys_addr + ((lo - r.userspace_addr) & !4095);
                let last = r.guest_phys_addr + ((hi - 1 - r.userspace_addr) & !4095);
                match self.revoke(slot, first, last) {
                    Ok(n) => removed += n,
                    Err(_) => failed = true,
                }
            }
        }
        #[cfg(feature = "revocable-test")]
        {
            state.revoked += removed;
        }
        #[cfg(not(feature = "revocable-test"))]
        let _ = removed;
        if failed {
            self.fail_closed(&mut state);
        }
    }
    fn end(&self) {
        let mut state = self.state.lock();
        assert!(state.invalidating != 0);
        state.epoch = state.epoch.wrapping_add(1);
        state.invalidating -= 1;
        self.vm.guest_access.unblock();
    }
    pub fn is_dead(&self) -> bool {
        self.state.lock().dead
    }
    pub fn close(&self) {
        {
            let mut state = self.state.lock();
            state.dead = true;
            self.vm.guest_access.block();
        }
        (self.ops.kick)();
        self.vm.guest_access.drain();
        // Teardown (unlike notifier start) is sleepable. No MM/core locks held.
        while self.resolving.load(Ordering::Acquire) != 0 {
            axvisor_api::task::yield_now();
        }
        #[cfg(feature = "revocable-test")]
        {
            let state = self.state.lock();
            let (checks, flushes) = self.vm.guest_access.sync_stats();
            warn!(
                "REVOCABLE_STATS vm={} published={} revoked={} checks={} flushes={}",
                self.vm.id(),
                state.published,
                state.revoked,
                checks,
                flushes
            );
        }
        let id = self.subscription.swap(0, Ordering::AcqRel);
        if id != 0 {
            (self.ops.unregister)(id);
        }
        let mut state = self.state.lock();
        for slot in state.slots.values() {
            let r = slot.region;
            let _ = self.vm.unmap_region(
                GuestPhysAddr::from(r.guest_phys_addr as usize),
                r.memory_size as usize,
            );
        }
        state.slots.clear();
    }
    pub fn set_slot(&self, region: UserspaceMemoryRegion) -> AxResult {
        self.block_access();
        let result = (|| {
            let mut state = self.state.lock();
            if state.dead {
                return ax_err!(BadState);
            }
            if region.memory_size != 0
                && state.slots.iter().any(|(&id, s)| {
                    id != region.slot
                        && region.guest_phys_addr < s.region.guest_phys_addr + s.region.memory_size
                        && s.region.guest_phys_addr < region.guest_phys_addr + region.memory_size
                })
            {
                return ax_err!(InvalidInput);
            }
            if let Some(slot) = state.slots.get_mut(&region.slot) {
                let r = slot.region;
                let result = self.revoke(
                    slot,
                    r.guest_phys_addr,
                    r.guest_phys_addr + r.memory_size - 4096,
                );
                match result {
                    Ok(n) => {
                        #[cfg(feature = "revocable-test")]
                        {
                            state.revoked += n;
                        }
                        #[cfg(not(feature = "revocable-test"))]
                        let _ = n;
                    }
                    Err(e) => {
                        self.fail_closed(&mut state);
                        return Err(e);
                    }
                }
            }
            state.slots.remove(&region.slot);
            if region.memory_size != 0 {
                state.slots.insert(
                    region.slot,
                    Slot {
                        region,
                        pages: BTreeMap::new(),
                    },
                );
            }
            Ok(())
        })();
        self.end();
        result
    }
    #[cfg(not(target_arch = "aarch64"))]
    pub fn contains(&self, addr: GuestPhysAddr) -> bool {
        let gpa = addr.as_usize() as u64;
        self.state.lock().slots.values().any(|s| {
            s.region.guest_phys_addr <= gpa && gpa < s.region.guest_phys_addr + s.region.memory_size
        })
    }
    pub fn fault(&self, addr: GuestPhysAddr, access: MappingFlags) -> AxResult<bool> {
        let gpa = (addr.as_usize() & !4095) as u64;
        let write = access.contains(MappingFlags::WRITE);
        let (id, hva, epoch) = {
            let state = self.state.lock();
            if state.dead {
                return ax_err!(BadState);
            }
            let Some((&id, slot)) = state.slots.iter().find(|(_, s)| {
                s.region.guest_phys_addr <= gpa
                    && gpa < s.region.guest_phys_addr + s.region.memory_size
            }) else {
                return Ok(false);
            };
            if state.invalidating != 0 {
                return Err(ax_errno::AxError::WouldBlock);
            }
            if slot.pages.get(&gpa).is_some_and(|w| !write || *w) {
                return Ok(true);
            }
            self.resolving.fetch_add(1, Ordering::AcqRel);
            (
                id,
                (slot.region.userspace_addr + gpa - slot.region.guest_phys_addr) as usize,
                state.epoch,
            )
        };
        let resolved = (self.ops.resolve)(self.subscription.load(Ordering::Acquire), hva, write);
        let result = match resolved {
            Ok(page) => {
                let mut state = self.state.lock();
                if state.dead || state.epoch != epoch || state.invalidating != 0 {
                    (self.ops.finish)(page, false);
                    if state.dead {
                        ax_err!(BadState)
                    } else {
                        Err(ax_errno::AxError::WouldBlock)
                    }
                } else {
                    // Every slot update increments epoch: same-config ABA also retries.
                    let slot = state
                        .slots
                        .get_mut(&id)
                        .expect("epoch protects slot identity");
                    let mapped = slot.pages.get(&gpa).copied();
                    if mapped.is_some_and(|w| !write || w) {
                        (self.ops.finish)(page, false);
                        Ok(true)
                    } else if write && !page.writable {
                        (self.ops.finish)(page, false);
                        Err(ax_errno::AxError::BadAddress)
                    } else if mapped.is_some() {
                        // Keep the resolved writable page across quiescence and
                        // publish the upgrade before reopening guest access. An
                        // unmap-and-retry can livelock when the next instruction
                        // fetch remaps this same code/data page read-only.
                        drop(state);
                        self.block_access();
                        let mut state = self.state.lock();
                        let result = if state.dead
                            || state.epoch != epoch.wrapping_add(1)
                            || state.invalidating != 1
                        {
                            Err(ax_errno::AxError::WouldBlock)
                        } else {
                            let slot = state
                                .slots
                                .get_mut(&id)
                                .expect("epoch protects slot identity");
                            let result = (|| {
                                self.revoke(slot, gpa, gpa)?;
                                self.vm.map_region(
                                    GuestPhysAddr::from(gpa as usize),
                                    HostPhysAddr::from(page.paddr.as_usize()),
                                    4096,
                                    MappingFlags::READ
                                        | MappingFlags::WRITE
                                        | MappingFlags::EXECUTE
                                        | MappingFlags::USER,
                                )?;
                                slot.pages.insert(gpa, true);
                                self.vm.guest_access.translation_changed();
                                Ok(true)
                            })();
                            #[cfg(feature = "revocable-test")]
                            if result.is_ok() {
                                state.revoked += 1;
                                state.published += 1;
                            }
                            if result.is_err() {
                                self.fail_closed(&mut state);
                            }
                            result
                        };
                        (self.ops.finish)(page, result.is_ok());
                        drop(state);
                        self.end();
                        result
                    } else {
                        let mut flags =
                            MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::USER;
                        if page.writable {
                            flags |= MappingFlags::WRITE;
                        }
                        let result = (|| {
                            self.vm.map_region(
                                GuestPhysAddr::from(gpa as usize),
                                HostPhysAddr::from(page.paddr.as_usize()),
                                4096,
                                flags,
                            )?;
                            slot.pages.insert(gpa, page.writable);
                            // Also version pure additions: no invalid translation is
                            // allowed to survive a subsequent entry after publication.
                            self.vm.guest_access.translation_changed();
                            Ok(true)
                        })();
                        #[cfg(feature = "revocable-test")]
                        if result.is_ok() {
                            state.published += 1;
                        }
                        (self.ops.finish)(page, result.is_ok());
                        result
                    }
                }
            }
            Err(e) => Err(e),
        };
        self.resolving.fetch_sub(1, Ordering::Release);
        result
    }
}

pub(super) fn for_vm(id: api::ControlFileId) -> Option<Arc<RevocableMemory>> {
    let files = super::CONTROL_FILES.lock();
    match files.get(&id) {
        Some(super::ControlFileState::Vm(vm)) => vm.revocable.clone(),
        _ => None,
    }
}
