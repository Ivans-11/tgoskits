//! Cross-CPU edge interrupt posting for an actively bound VMX vCPU.
//!
//! The registry resolves targets; per-vCPU atomics serialize posting and exit. No sender
//! accesses the mutable vCPU or its virtual APIC page. Hardware alone consumes
//! PIR while the guest runs; remaining PIR bits are reclaimed after VM exit.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
mod descriptor;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
mod publication;
use ax_errno::AxResult;
use ax_kspin::SpinNoIrq;
use axvisor_api::memory::PhysFrame;
use descriptor::Descriptor;
use publication::Publication;

/// Host must reserve this vector and handle notifications received in root mode.
pub(super) const NOTIFICATION_VECTOR: usize = 0xf2;
const X2APIC_ID: u32 = 0x802;
const X2APIC_ICR: u32 = 0x830;

static ACTIVE: SpinNoIrq<BTreeMap<(usize, usize), Weak<PostedInterrupt>>> =
    SpinNoIrq::new(BTreeMap::new());
static VECTOR_READY: SpinNoIrq<bool> = SpinNoIrq::new(false);

pub(super) struct PostedInterrupt {
    frame: PhysFrame,
    key: (usize, usize),
    publication: Publication,
    apic_id: AtomicU32,
    eligible: [AtomicU64; 4],
    touched: AtomicBool,
    needs_sync: AtomicBool,
}

// A host-root notification needs only EOI. Host dispatch owns that operation.
fn notification_handler(_vector: usize) {}

pub(super) fn available() -> bool {
    // x2APIC writes are atomic; an xAPIC ICR pair would need host serialization.
    let base = unsafe { x86::msr::rdmsr(0x1b) };
    let pin = (super::Msr::IA32_VMX_TRUE_PINBASED_CTLS.read() >> 32) as u32;
    if base & (1 << 10) == 0 || pin & (1 << 7) == 0 {
        return false;
    }
    let mut ready = VECTOR_READY.lock();
    if !*ready {
        *ready = axvisor_api::irq::register_irq_handler(NOTIFICATION_VECTOR, notification_handler);
    }
    *ready
}

impl PostedInterrupt {
    pub(super) fn new(vm_id: usize, vcpu_id: usize) -> AxResult<Arc<Self>> {
        let result = Arc::new(Self {
            frame: PhysFrame::alloc_zero()?,
            key: (vm_id, vcpu_id),
            publication: Publication::new(),
            apic_id: AtomicU32::new(0),
            eligible: [const { AtomicU64::new(0) }; 4],
            touched: AtomicBool::new(false),
            needs_sync: AtomicBool::new(false),
        });
        unsafe {
            result
                .frame
                .as_mut_ptr()
                .cast::<Descriptor>()
                .write(Descriptor::new(NOTIFICATION_VECTOR as u8));
        }
        ACTIVE.lock().insert(result.key, Arc::downgrade(&result));
        Ok(result)
    }

    fn descriptor(&self) -> &Descriptor {
        // The zeroed frame is page-aligned, remains allocated for this object,
        // and all CPU accesses to hardware-mutated fields are atomic.
        unsafe { &*self.frame.as_mut_ptr().cast::<Descriptor>() }
    }

    pub(super) fn paddr(&self) -> u64 {
        self.frame.start_paddr().as_usize() as u64
    }

    /// Publish only with host IRQs disabled immediately before VM entry.
    pub(super) fn enter(self: &Arc<Self>, eligible: [u64; 4]) {
        let apic_id = unsafe { x86::msr::rdmsr(X2APIC_ID) } as u32;
        self.apic_id.store(apic_id, Ordering::Relaxed);
        for (slot, value) in self.eligible.iter().zip(eligible) {
            slot.store(value, Ordering::Relaxed);
        }
        self.publication.enter();
    }

    /// Stop senders before migration or host IRQs become enabled. Reclaim bits
    /// hardware did not consume (e.g. entry failed or raced with an exit).
    pub(super) fn exit(&self) -> Option<[u64; 4]> {
        self.publication.exit();
        if !self.needs_sync.load(Ordering::Relaxed) {
            return None;
        }
        if !self.touched.load(Ordering::Relaxed) {
            return Some([0; 4]);
        }
        self.touched.store(false, Ordering::Relaxed);
        Some(self.descriptor().reclaim())
    }

    /// No sender is admitted between exit and the next enter. Continue ISR
    /// synchronization until hardware has drained both virtual IRR and ISR.
    pub(super) fn finish_sync(&self, interrupt_status: u16) {
        self.needs_sync
            .store(interrupt_status != 0, Ordering::Relaxed);
    }
}

impl Drop for PostedInterrupt {
    fn drop(&mut self) {
        ACTIVE.lock().remove(&self.key);
    }
}

/// Post to a running vCPU. False leaves the caller responsible for normal
/// software delivery. No task handle or mutable remote vCPU access is needed.
pub fn try_post_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> bool {
    if !(32..256).contains(&vector) {
        return false;
    }
    // This CPU also needs x2APIC enabled to send the physical notification.
    if unsafe { x86::msr::rdmsr(0x1b) } & (1 << 10) == 0 {
        return false;
    }
    let owner = ACTIVE.lock().get(&(vm_id, vcpu_id)).and_then(Weak::upgrade);
    let Some(owner) = owner else {
        return false;
    };
    let Some(_posting) = owner.publication.try_post() else {
        return false;
    };
    if owner.eligible[vector / 64].load(Ordering::Relaxed) & (1u64 << (vector % 64)) == 0 {
        return false;
    }
    let apic_id = owner.apic_id.load(Ordering::Relaxed);
    let descriptor = owner.descriptor();
    owner.touched.store(true, Ordering::Relaxed);
    owner.needs_sync.store(true, Ordering::Relaxed);
    // Always set ON after publishing PIR, even for an already pending vector:
    // hardware may be clearing ON and consuming PIR concurrently.
    if descriptor.post(vector as u8) {
        // The publication guard prevents exit/migration until ICR is written.
        unsafe {
            x86::msr::wrmsr(
                X2APIC_ICR,
                ((apic_id as u64) << 32) | NOTIFICATION_VECTOR as u64,
            );
        }
    }
    // The posting guard retires before the strong descriptor reference drops.
    true
}
