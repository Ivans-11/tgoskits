// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Bare-metal runner for the Axvisor host-contract conformance suite.

#![no_std]
#![no_main]
#![cfg(target_os = "none")]

extern crate alloc;
extern crate ax_std as std;

use std::os::arceos::modules::ax_task;

struct ArceosStimulus;

impl axvisor_conformance::Stimulus for ArceosStimulus {
    fn verify_oneshot_timer(&self) -> Option<bool> {
        use alloc::string::String;
        use core::{
            sync::atomic::{AtomicU64, AtomicUsize, Ordering},
            time::Duration,
        };

        static TICKS: AtomicUsize = AtomicUsize::new(0);
        static ARMED_DEADLINE: AtomicU64 = AtomicU64::new(0);
        static FIRED_AT: AtomicU64 = AtomicU64::new(0);
        static PASSED: AtomicUsize = AtomicUsize::new(0);
        const DELAY_NANOS: u64 = 2_000_000;
        const LATE_TOLERANCE_NANOS: u64 = 3_000_000;
        const TIMEOUT_NANOS: u64 = 100_000_000;

        PASSED.store(0, Ordering::Release);
        let handle = axvisor_api::task::spawn_task(
            axvisor_api::task::TaskOptions {
                name: String::from("axvisor-conformance-timer"),
                stack_size: 64 * 1024,
                cpu_set: Some(1),
            },
            || {
                ax_task::register_timer_callback(|_| {
                    TICKS.fetch_add(1, Ordering::Release);
                    let now = axvisor_api::time::current_time_nanos();
                    let deadline = ARMED_DEADLINE.load(Ordering::Acquire);
                    if deadline != 0 && now >= deadline {
                        let _ =
                            FIRED_AT.compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
                    }
                });

                let sync_timeout =
                    axvisor_api::time::current_time_nanos().saturating_add(TIMEOUT_NANOS);
                while TICKS.load(Ordering::Acquire) == 0
                    && axvisor_api::time::current_time_nanos() < sync_timeout
                {
                    axvisor_api::task::yield_now();
                }
                if TICKS.load(Ordering::Acquire) == 0 {
                    return;
                }

                FIRED_AT.store(0, Ordering::Release);
                let deadline = axvisor_api::time::current_time_nanos().saturating_add(DELAY_NANOS);
                ARMED_DEADLINE.store(deadline, Ordering::Release);
                axvisor_api::time::set_oneshot_timer(Duration::from_nanos(deadline));
                let timeout = deadline.saturating_add(TIMEOUT_NANOS);
                while FIRED_AT.load(Ordering::Acquire) == 0
                    && axvisor_api::time::current_time_nanos() < timeout
                {
                    axvisor_api::task::yield_now();
                }
                let fired_at = FIRED_AT.load(Ordering::Acquire);
                if fired_at >= deadline && fired_at <= deadline.saturating_add(LATE_TOLERANCE_NANOS)
                {
                    PASSED.store(1, Ordering::Release);
                }
            },
        );
        axvisor_api::task::join_task(handle);
        Some(PASSED.load(Ordering::Acquire) == 1)
    }

    fn verify_irq_ingress(&self, test_vector: usize) -> Option<bool> {
        use core::sync::atomic::{AtomicUsize, Ordering};
        use core::time::Duration;

        static RESULT: AtomicUsize = AtomicUsize::new(0);
        static ARMED: AtomicUsize = AtomicUsize::new(0);
        const DELAY_NANOS: u64 = 2_000_000;
        const TIMEOUT_NANOS: u64 = 100_000_000;

        let handle = axvisor_api::task::spawn_task(
            axvisor_api::task::TaskOptions {
                name: alloc::string::String::from("axvisor-conformance-irq"),
                stack_size: 64 * 1024,
                cpu_set: Some(1),
            },
            move || {
                ax_task::register_timer_callback(move |_| {
                    if ARMED.load(Ordering::Acquire) == 1 {
                        RESULT.store(
                            if axvisor_api::irq::handle_irq(test_vector) {
                                1
                            } else {
                                2
                            },
                            Ordering::Release,
                        );
                        ARMED.store(0, Ordering::Release);
                    }
                });
                RESULT.store(0, Ordering::Release);
                ARMED.store(1, Ordering::Release);
                let deadline = axvisor_api::time::current_time_nanos().saturating_add(DELAY_NANOS);
                axvisor_api::time::set_oneshot_timer(Duration::from_nanos(deadline));
                let timeout = deadline.saturating_add(TIMEOUT_NANOS);
                while RESULT.load(Ordering::Acquire) == 0
                    && axvisor_api::time::current_time_nanos() < timeout
                {
                    axvisor_api::task::yield_now();
                }
            },
        );
        axvisor_api::task::join_task(handle);
        Some(RESULT.load(Ordering::Acquire) == 1)
    }
}

static STIMULUS: ArceosStimulus = ArceosStimulus;

#[unsafe(no_mangle)]
fn main() {
    axvisor::link_host_adapter();

    #[cfg(target_arch = "riscv64")]
    axvisor::hal::arch::prepare_virtualization();

    let report = axvisor_conformance::run(&STIMULUS);
    report.log();
    std::process::exit((!report.passed()) as i32);
}
