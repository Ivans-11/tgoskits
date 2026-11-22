//! VM-scoped tags and generation-checked translation synchronization.
use axvcpu::vmid::Lease;
#[derive(Debug)]
pub(crate) struct TranslationContext {
    vm: usize,
    lease: Option<Lease>,
    stamp: Option<(usize, u64, usize)>,
    notified_stamp: Option<(usize, u64, u64)>,
    root: u64,
    flushed: bool,
}
impl TranslationContext {
    pub(crate) fn new(vm: usize) -> Self {
        Self {
            vm,
            lease: None,
            stamp: None,
            notified_stamp: None,
            root: 0,
            flushed: false,
        }
    }
    pub(crate) fn invalidate(&mut self) {
        self.stamp = None;
        self.notified_stamp = None;
    }
    pub(crate) fn bind(&mut self, root: u64) -> u64 {
        assert!(self.lease.is_none());
        let root = root & 0x0000_ffff_ffff_ffff;
        self.lease = Lease::acquire(self.vm, root as usize, 255);
        self.root = root | ((self.lease.as_ref().map_or(0, |l| l.0.id) as u64) << 48);
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
            self.flush();
        }
        self.stamp = stamp;
        self.root
    }
    pub(crate) fn unbind(&mut self) {
        self.lease = None;
    }
    fn flush(&self) {
        // Select the target VMID for TLBI while preserving the native host
        // translation regime. TGE must be clear for the guest EL1 operation.
        unsafe {
            let hcr: u64;
            let vttbr: u64;
            core::arch::asm!("mrs {hcr}, hcr_el2", "mrs {vttbr}, vttbr_el2", hcr=out(reg) hcr,vttbr=out(reg) vttbr);
            core::arch::asm!(
                "dsb ishst",
                "msr vttbr_el2, {root}",
                "isb",
                "msr hcr_el2, {guest_hcr}",
                "isb",
                "tlbi vmalls12e1",
                "dsb ish",
                "ic iallu",
                "dsb ish",
                "isb",
                "msr hcr_el2, {host_hcr}",
                "isb",
                "msr vttbr_el2, {host_vttbr}",
                "isb",
                root = in(reg) self.root,
                guest_hcr = in(reg) hcr & !(1 << 27),
                host_hcr = in(reg) hcr,
                host_vttbr = in(reg) vttbr,
                options(nostack)
            );
        }
    }
    pub(crate) fn sync_notified(&mut self, generation: u64) -> bool {
        let stamp = self.lease.as_ref().and_then(|l| {
            axvisor_api::arch::translation_cpu_id().map(|cpu| (cpu, l.0.incarnation, generation))
        });
        if stamp.is_none() || stamp != self.notified_stamp {
            self.flush();
            self.flushed = true;
        }
        self.notified_stamp = stamp;
        core::mem::take(&mut self.flushed)
    }
}
