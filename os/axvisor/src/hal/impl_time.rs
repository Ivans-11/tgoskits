use axvisor_api::time::{Nanos, TimeIf, TimeValue};
use std::os::arceos::modules::ax_task;

struct TimeImpl;

#[ax_percpu::def_percpu]
static TIMER_ARMED: bool = false;

pub(super) fn init_percpu() {
    ax_task::register_timer_callback(|_| {
        // No VMM timers exist until all per-CPU timer lists are initialized.
        if TIMER_ARMED.read_current() {
            TIMER_ARMED.write_current(false);
            axvisor_core::vmm::timer::check_events();
        }
    });
}

#[axvisor_api::api_impl]
impl TimeIf for TimeImpl {
    fn current_time_nanos() -> Nanos {
        ax_hal::time::monotonic_time_nanos()
    }

    fn set_oneshot_timer(deadline: TimeValue) {
        TIMER_ARMED.write_current(true);
        ax_hal::time::set_oneshot_timer(deadline.as_nanos() as u64)
    }
}
