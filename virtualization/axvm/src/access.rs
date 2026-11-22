//! Short, non-sleeping guest-memory access intervals. Host MM fault resolution
//! is deliberately outside these intervals, so notifier quiescence cannot wait
//! for a reader blocked on the notifier caller's MM locks.
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use ax_errno::AxResult;

#[derive(Default)]
pub struct GuestAccess {
    enabled: AtomicBool,
    blockers: AtomicUsize,
    readers: AtomicUsize,
    generation: AtomicU64,
    #[cfg(feature = "revocable-test")]
    sync_checks: AtomicU64,
    #[cfg(feature = "revocable-test")]
    sync_flushes: AtomicU64,
}
impl GuestAccess {
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
    /// Caller must prevent preemption until the guard is dropped.
    pub fn enter(&self) -> AxResult<GuestAccessGuard<'_>> {
        if !self.enabled() {
            return Ok(GuestAccessGuard(None));
        }
        if self.blockers.load(Ordering::SeqCst) != 0 {
            return Err(ax_errno::AxError::WouldBlock);
        }
        self.readers.fetch_add(1, Ordering::SeqCst);
        if self.blockers.load(Ordering::SeqCst) != 0 {
            self.readers.fetch_sub(1, Ordering::SeqCst);
            return Err(ax_errno::AxError::WouldBlock);
        }
        Ok(GuestAccessGuard(Some(self)))
    }
    /// Publish a page-table change before reopening access. Invalidation callers
    /// hold the publication lock and have drained all guest access intervals.
    /// Pure additions may also advance this generation without draining readers.
    pub fn translation_changed(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    #[cfg(feature = "revocable-test")]
    pub fn record_sync(&self, flushed: bool) {
        self.sync_checks.fetch_add(1, Ordering::Relaxed);
        if flushed {
            self.sync_flushes.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[cfg(feature = "revocable-test")]
    pub fn sync_stats(&self) -> (u64, u64) {
        (
            self.sync_checks.load(Ordering::Relaxed),
            self.sync_flushes.load(Ordering::Relaxed),
        )
    }
    pub fn block(&self) {
        self.blockers.fetch_add(1, Ordering::SeqCst);
    }
    pub fn drain(&self) {
        while self.readers.load(Ordering::SeqCst) != 0 {
            core::hint::spin_loop();
        }
    }
    pub fn unblock(&self) {
        assert!(self.blockers.fetch_sub(1, Ordering::SeqCst) != 0);
    }
}
pub struct GuestAccessGuard<'a>(Option<&'a GuestAccess>);
impl Drop for GuestAccessGuard<'_> {
    fn drop(&mut self) {
        if let Some(gate) = self.0 {
            gate.readers.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
