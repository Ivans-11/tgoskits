//! Retained G-stage contexts for the core-owned virtualization hardware.
//!
//! Each vCPU owns a distinct VMID. A new lease and every migration flush the
//! destination hart, so a released ID may be reused without retaining any old
//! translation. Mapping changes invalidate cached bindings through a generation.

use ax_kspin::SpinNoIrq;

const VMID_BITS: usize = 14;
const VMID_MASK: usize = (1 << VMID_BITS) - 1;
const VMID_SHIFT: usize = 44;
static IDS: SpinNoIrq<[u64; 256]> = SpinNoIrq::new([0; 256]);

struct Vmid(usize);

impl Vmid {
    // Caller has interrupts disabled and vsatp cleared; no guest access is
    // permitted until the final hgatp is installed and fenced below.
    unsafe fn allocate() -> Option<Self> {
        let old: usize;
        let supported: usize;
        unsafe {
            core::arch::asm!("csrr {old}, hgatp", old = out(reg) old);
            core::arch::asm!("csrw hgatp, {probe}", probe = in(reg) old | (VMID_MASK << VMID_SHIFT));
            core::arch::asm!("csrr {bits}, hgatp", bits = out(reg) supported);
            core::arch::asm!("csrw hgatp, {old}", old = in(reg) old);
        }
        let maximum = (supported >> VMID_SHIFT) & VMID_MASK;
        let mut ids = IDS.lock();
        for id in 1..=maximum {
            let bit = 1u64 << (id % 64);
            if ids[id / 64] & bit == 0 {
                ids[id / 64] |= bit;
                return Some(Self(id));
            }
        }
        None
    }
}

impl Drop for Vmid {
    fn drop(&mut self) {
        IDS.lock()[self.0 / 64] &= !(1u64 << (self.0 % 64));
    }
}

#[derive(Default)]
pub(crate) struct TranslationContext {
    id: Option<Vmid>,
    last_cpu: Option<usize>,
    generation: usize,
}

impl TranslationContext {
    pub(crate) fn invalidate(&mut self) {
        self.last_cpu = None;
    }

    /// Install a context with vsatp still Bare and host interrupts disabled.
    pub(crate) unsafe fn bind(&mut self, root: usize) {
        if self.id.is_none() {
            self.id = unsafe { Vmid::allocate() };
        }
        let id = self.id.as_ref().map_or(0, |id| id.0);
        let hgatp = (root & !(VMID_MASK << VMID_SHIFT)) | (id << VMID_SHIFT);
        let actual: usize;
        unsafe {
            core::arch::asm!("csrw hgatp, {hgatp}", hgatp = in(reg) hgatp);
            core::arch::asm!("csrr {actual}, hgatp", actual = out(reg) actual);
        }
        let supported = (actual >> VMID_SHIFT) & VMID_MASK == id;
        let cpu = axvisor_api::arch::current_cpu_id();
        let generation = axaddrspace::guest_tlb_generation();
        if id == 0
            || !supported
            || self.last_cpu != Some(cpu)
            || self.generation != generation
            || generation == usize::MAX
        {
            // A heterogeneous hart may implement fewer VMID bits. Fall back
            // to the reserved, always-flushed VMID zero instead of aliasing.
            if !supported {
                unsafe {
                    core::arch::asm!("csrw hgatp, {root}", root = in(reg) root & !(VMID_MASK << VMID_SHIFT));
                }
            }
            unsafe {
                core::arch::riscv64::hfence_gvma_all();
                core::arch::riscv64::hfence_vvma_all();
            }
        }
        self.last_cpu = supported.then_some(cpu);
        self.generation = generation;
    }
}
