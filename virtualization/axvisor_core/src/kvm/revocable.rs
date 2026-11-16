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
        if !cfg!(all(target_arch = "x86_64", feature = "vmx")) {
            return Ok(None);
        }
        let Some(ops) = api::revocable_memory_ops() else {
            return Ok(None);
        };
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
    fn begin(&self, start: usize, end: usize) {
        {
            let mut state = self.state.lock();
            state.invalidating += 1;
            state.epoch = state.epoch.wrapping_add(1);
            self.vm.guest_access.block();
        }
        (self.ops.kick)();
        self.vm.guest_access.drain();
        let mut state = self.state.lock();
        // Whole intersecting slots are revoked. Since each mapped MemoryArea is
        // a page contained in its slot, removal does not allocate/split areas.
        let mut failed = false;
        for slot in state.slots.values_mut() {
            let r = slot.region;
            if (start as u64) < r.userspace_addr + r.memory_size && r.userspace_addr < end as u64 {
                if self
                    .vm
                    .unmap_region(
                        GuestPhysAddr::from(r.guest_phys_addr as usize),
                        r.memory_size as usize,
                    )
                    .is_err()
                {
                    failed = true;
                }
                slot.pages.clear();
            }
        }
        if failed && !state.dead {
            state.dead = true;
            self.vm.guest_access.block(); // fail closed; never resume stale mappings
            error!("revocable backing: unmap failed, VM access permanently disabled");
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
        self.begin(0, usize::MAX);
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
                        // Permission upgrades must not free/rewrite page-table
                        // structures while another CPU is walking them.
                        let r = slot.region;
                        (self.ops.finish)(page, false);
                        drop(state);
                        self.begin(
                            r.userspace_addr as usize,
                            (r.userspace_addr + r.memory_size) as usize,
                        );
                        self.end();
                        Err(ax_errno::AxError::WouldBlock)
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
                            Ok(true)
                        })();
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
