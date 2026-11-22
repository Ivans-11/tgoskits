//! VM-scoped G-stage tags. Active reservations prevent recycling a live VMID;
//! each binding validates its lease incarnation, CPU and translation epoch.
use axvcpu::vmid::Lease;
const VMID_MASK: usize = (1 << 14) - 1;
const VMID_SHIFT: usize = 44;

pub(crate) struct TranslationContext {
    vm: usize,
    capability: Option<(usize, usize)>,
    lease: Option<Lease>,
    stamp: Option<(usize, u64, usize)>,
    notified_stamp: Option<(usize, u64, u64)>,
    flushed: bool,
}
impl TranslationContext {
    pub(crate) fn new(vm: usize) -> Self {
        Self {
            vm,
            capability: None,
            lease: None,
            stamp: None,
            notified_stamp: None,
            flushed: false,
        }
    }
    pub(crate) fn invalidate(&mut self) {
        self.stamp = None;
        self.notified_stamp = None;
    }
    pub(crate) fn unbind(&mut self) {
        self.lease = None;
    }
    fn flush() {
        unsafe {
            core::arch::asm!("fence rw, rw", options(nostack));
            core::arch::riscv64::hfence_gvma_all();
            core::arch::riscv64::hfence_vvma_all();
            core::arch::asm!("fence.i", options(nostack));
        }
    }
    pub(crate) fn sync_notified(&mut self, generation: u64) -> bool {
        let stamp = self.lease.as_ref().and_then(|l| {
            axvisor_api::arch::translation_cpu_id().map(|cpu| (cpu, l.0.incarnation, generation))
        });
        if stamp.is_none() || stamp != self.notified_stamp {
            Self::flush();
            self.flushed = true;
        }
        self.notified_stamp = stamp;
        core::mem::take(&mut self.flushed)
    }
    pub(crate) unsafe fn bind(&mut self, root: usize) {
        assert!(self.lease.is_none());
        let root = root & !(VMID_MASK << VMID_SHIFT);
        let cpu = axvisor_api::arch::current_cpu_id();
        let maximum = match self.capability {
            Some((old_cpu, maximum)) if old_cpu == cpu => maximum,
            _ => {
                let supported: usize;
                // Probe while vsatp is Bare and no guest access is possible.
                // The new CPU/context is fenced below before it can execute.
                unsafe {
                    core::arch::asm!("csrw hgatp, {probe}", "csrr {bits}, hgatp", probe=in(reg) root | (VMID_MASK << VMID_SHIFT), bits=out(reg) supported);
                    core::arch::asm!("csrw hgatp, {root}", root=in(reg) root);
                }
                let maximum = (supported >> VMID_SHIFT) & VMID_MASK;
                self.capability = Some((cpu, maximum));
                maximum
            }
        };
        self.lease = Lease::acquire(self.vm, root, maximum);
        let id = self.lease.as_ref().map_or(0, |l| l.0.id);
        unsafe {
            core::arch::asm!("csrw hgatp, {root}", root=in(reg) root | (id << VMID_SHIFT));
        }
        let generation = axaddrspace::guest_tlb_generation();
        let stamp = self.lease.as_ref().map(|l| {
            (
                axvisor_api::arch::current_cpu_id(),
                l.0.incarnation,
                generation,
            )
        });
        self.flushed = stamp.is_none() || stamp != self.stamp || generation == usize::MAX;
        if self.flushed {
            Self::flush();
        }
        self.stamp = stamp;
    }
}

impl Default for TranslationContext {
    fn default() -> Self {
        Self::new(usize::MAX)
    }
}
