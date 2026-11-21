//! Device-originated PI remains live while its vCPU runs in host context.
//! Root-mode notifications wake the task; only the vCPU consumes PIR. Routing
//! leases keep the descriptor alive until host interrupt remapping is revoked.

use alloc::sync::Arc;
use core::{
    ops::Bound::{Excluded, Unbounded},
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axvisor_api::device::{InterruptTarget, InterruptTargetKind};

use super::{ACTIVE, Descriptor, NOTIFICATION_VECTOR, PostedInterrupt, X2APIC_ICR, X2APIC_ID};

struct State {
    destination: Option<u32>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub(super) struct Hardware {
    leases: AtomicUsize,
    state: SpinNoIrq<State>,
}

fn notify_cpu(apic_id: u32) {
    // SAFETY: PI availability requires x2APIC. The destination is a host APIC
    // ID recorded on VM entry, never a guest-programmed MSI address.
    unsafe {
        x86::msr::wrmsr(
            X2APIC_ICR,
            ((apic_id as u64) << 32) | NOTIFICATION_VECTOR as u64,
        );
    }
}

impl Hardware {
    pub(super) fn new() -> Self {
        Self {
            leases: AtomicUsize::new(0),
            state: SpinNoIrq::new(State {
                destination: None,
                wake: None,
            }),
        }
    }

    pub(super) fn active(&self) -> bool {
        self.leases.load(Ordering::Acquire) != 0
    }

    pub(super) fn enter(&self, descriptor: &Descriptor, apic_id: u32) {
        let mut state = self.state.lock();
        state.destination = Some(apic_id);
        descriptor.set_destination(apic_id);
        // An interrupt may have notified the previous CPU, or its root-mode
        // handler may have cleared ON. With IRQs disabled a self-IPI remains
        // pending until VM entry, where hardware consumes PIR.
        if descriptor.has_pending() {
            descriptor.arm_notification();
            notify_cpu(apic_id);
        }
    }
}

struct Lease(Arc<PostedInterrupt>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.hardware.leases.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Retain a vCPU's PI destination for an assigned edge interrupt. The wake
/// callback must be IRQ-safe and wake this vCPU without injecting another IRQ.
/// The host must keep the returned target until hardware route revocation.
pub fn hardware_interrupt_target(
    vm_id: usize,
    vcpu_id: usize,
    vector: u8,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> AxResult<InterruptTarget> {
    if vector < 32 {
        return Err(AxError::InvalidInput);
    }
    let owner = ACTIVE
        .lock()
        .get(&(vm_id, vcpu_id))
        .and_then(alloc::sync::Weak::upgrade)
        .ok_or(AxError::Unsupported)?;
    let descriptor = owner.frame.start_paddr();
    if descriptor.as_usize() & 63 != 0 {
        return Err(AxError::InvalidInput);
    }
    {
        let mut state = owner.hardware.state.lock();
        if state.destination.is_none() {
            return Err(AxError::Unsupported);
        }
        state.wake = Some(wake);
        owner.hardware.leases.fetch_add(1, Ordering::Release);
    }
    // SAFETY: Lease owns the descriptor; entry/exit and notification handling
    // synchronize migration, pending bits and blocked-vCPU wakeups. The vector
    // and descriptor alignment satisfy Intel PI requirements checked above.
    Ok(unsafe {
        InterruptTarget::from_raw_parts(
            InterruptTargetKind::X86Posted { descriptor, vector },
            Arc::new(Lease(owner)),
        )
    })
}

pub(super) fn notify() {
    let cpu = unsafe { x86::msr::rdmsr(X2APIC_ID) } as u32;
    // Release the registry before calling a core wakeup callback, which may
    // itself request interrupt delivery. No mutable vCPU state is accessed.
    let mut after = Unbounded;
    loop {
        let entry = ACTIVE
            .lock()
            .range((after, Unbounded))
            .next()
            .map(|(&key, owner)| (key, owner.upgrade()));
        let Some((key, owner)) = entry else { break };
        after = Excluded(key);
        let Some(owner) = owner else { continue };
        if !owner.hardware.active() && !owner.descriptor().has_pending() {
            continue;
        }
        let wake = {
            let state = owner.hardware.state.lock();
            let Some(destination) = state.destination else {
                continue;
            };
            let descriptor = owner.descriptor();
            if destination != cpu {
                // Migration can race an IOMMU read of NDST. Forward a stale
                // notification to the current CPU instead of clearing ON.
                if descriptor.has_pending() && owner.publication.is_running() {
                    descriptor.arm_notification();
                    notify_cpu(destination);
                }
            } else {
                descriptor.clear_notification();
            }
            if !descriptor.has_pending() {
                continue;
            }
            state.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }
}
