use alloc::{boxed::Box, string::String};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_kspin::SpinRwLock as RwLock;

use super::time::{has_time_provider, wall_time};

pub trait BlockTaskOps: Send + Sync {
    fn current_task_id(&self) -> Option<u64>;
    fn task_yield(&self);
    fn task_wait(&self);
    fn task_wait_timeout(&self, dur: Duration) -> bool {
        let _ = dur;
        self.task_yield();
        true
    }
    fn task_wait_until(&self, condition: &dyn Fn() -> bool) {
        while !condition() {
            self.task_wait();
        }
    }
    /// Wait until `condition` holds or `timeout` elapses, whichever comes first.
    /// Returns `true` if the condition was observed, `false` on timeout.
    ///
    /// Default impl polls the condition with cooperative yields, bounded by the
    /// wall clock. Runtimes with a timed wait queue should override this so the
    /// task actually sleeps. Used by the block runtime to escape an IRQ-driven
    /// completion wait when the completion IRQ never fires (e.g. a board whose
    /// FDT→GIC routing leaves the controller IRQ undelivered) and fall back to
    /// polling instead of hanging forever.
    fn task_wait_until_timeout(&self, condition: &dyn Fn() -> bool, timeout: Duration) -> bool {
        if !has_time_provider() {
            // No clock to bound the wait: degrade to a single yield + recheck so
            // the caller can decide to poll, rather than blocking unbounded here.
            if condition() {
                return true;
            }
            self.task_yield();
            return condition();
        }
        let deadline = wall_time() + timeout;
        while !condition() {
            if wall_time() >= deadline {
                return condition();
            }
            self.task_yield();
        }
        true
    }
    fn wake_task(&self, task_id: u64);
    fn notify_waiters(&self) {}
    fn notify_drain(&self) {
        self.notify_waiters();
    }
    fn notify_drain_from_irq(&self) {
        self.notify_drain();
    }
    fn wait_for_drain_notification(&self) {
        self.task_wait();
    }
    fn spawn(&self, _name: String, _f: Box<dyn FnOnce() + Send + 'static>) {}
}

static TASK_OPS: RwLock<Option<&'static dyn BlockTaskOps>> = RwLock::new(None);
static TASK_READY: AtomicBool = AtomicBool::new(false);

pub fn set_task_ops(ops: &'static dyn BlockTaskOps) {
    *TASK_OPS.write() = Some(ops);
    TASK_READY.store(true, Ordering::Release);
}

fn task_ops() -> Option<&'static dyn BlockTaskOps> {
    TASK_OPS.read().as_ref().copied()
}

pub fn current_task_id() -> Option<u64> {
    task_ops().and_then(|ops| ops.current_task_id())
}

pub fn task_yield() {
    if let Some(ops) = task_ops() {
        ops.task_yield();
    }
}

pub fn task_wait() {
    if let Some(ops) = task_ops() {
        ops.task_wait();
    }
}

pub fn task_wait_timeout(dur: Duration) -> bool {
    task_ops().is_some_and(|ops| ops.task_wait_timeout(dur))
}

pub fn task_wait_until(condition: impl Fn() -> bool) {
    if let Some(ops) = task_ops() {
        ops.task_wait_until(&condition);
    } else {
        while !condition() {
            core::hint::spin_loop();
        }
    }
}

/// Wait until `condition` holds or `timeout` elapses. Returns `true` if the
/// condition was observed, `false` on timeout. Without registered task ops,
/// spins on the condition (no clock available to time out against).
pub fn task_wait_until_timeout(condition: impl Fn() -> bool, timeout: Duration) -> bool {
    if let Some(ops) = TASK_OPS.read().as_ref() {
        ops.task_wait_until_timeout(&condition, timeout)
    } else {
        while !condition() {
            core::hint::spin_loop();
        }
        true
    }
}

pub fn wake_task(task_id: u64) {
    if let Some(ops) = task_ops() {
        ops.wake_task(task_id);
    }
}

pub fn notify_waiters() {
    if let Some(ops) = task_ops() {
        ops.notify_waiters();
    }
}

pub fn notify_drain() {
    if let Some(ops) = task_ops() {
        ops.notify_drain();
    }
}

pub fn notify_drain_from_irq() {
    if let Some(ops) = task_ops() {
        ops.notify_drain_from_irq();
    }
}

pub fn wait_for_drain_notification() {
    if let Some(ops) = task_ops() {
        ops.wait_for_drain_notification();
    } else {
        core::hint::spin_loop();
    }
}

pub fn spawn_task(name: String, f: Box<dyn FnOnce() + Send + 'static>) {
    if let Some(ops) = task_ops() {
        ops.spawn(name, f);
    }
}

pub fn has_task_ops() -> bool {
    TASK_READY.load(Ordering::Acquire)
}
