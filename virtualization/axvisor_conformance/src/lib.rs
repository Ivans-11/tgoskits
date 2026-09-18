// Copyright 2026 The Axvisor Team
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

//! Host-independent conformance tests for implementations of `axvisor_api`.
//!
//! The tests exercise the real adapter linked into the host. They intentionally
//! test semantic obligations rather than the interface dispatch machinery.

#![no_std]

extern crate alloc;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use axvisor_api::{
    memory,
    sync::WaitQueue,
    task::{self, TaskOptions},
    time,
};

const TEST_STACK_SIZE: usize = 64 * 1024;
const WATCHDOG_YIELDS: usize = 100_000;
const RECLAIM_SAMPLE_FRAMES: usize = 32;

/// Outcome of one observable contract check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Pass,
    Fail,
    NotRun,
}

impl Outcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotRun => "NOT-RUN",
        }
    }
}

/// Result of one contract check.
#[derive(Clone, Copy, Debug)]
pub struct CaseResult {
    pub family: &'static str,
    pub check: &'static str,
    pub outcome: Outcome,
}

/// Results returned by a conformance run.
#[derive(Debug, Default)]
pub struct Report {
    cases: Vec<CaseResult>,
}

impl Report {
    fn record(&mut self, family: &'static str, check: &'static str, passed: bool) {
        self.cases.push(CaseResult {
            family,
            check,
            outcome: if passed { Outcome::Pass } else { Outcome::Fail },
        });
    }

    fn not_run(&mut self, family: &'static str, check: &'static str) {
        self.cases.push(CaseResult {
            family,
            check,
            outcome: Outcome::NotRun,
        });
    }

    pub fn cases(&self) -> &[CaseResult] {
        &self.cases
    }

    pub fn passed(&self) -> bool {
        self.cases.iter().all(|case| case.outcome != Outcome::Fail)
    }

    pub fn complete(&self) -> bool {
        self.cases.iter().all(|case| case.outcome == Outcome::Pass)
    }

    /// Emits stable, machine-readable result lines through the host logger.
    pub fn log(&self) {
        for case in &self.cases {
            log::info!(
                "CONFORMANCE family={} check={} status={}",
                case.family,
                case.check,
                case.outcome.as_str()
            );
        }
        log::info!(
            "CONFORMANCE summary={} complete={}",
            if self.passed() { "PASS" } else { "FAIL" },
            self.complete()
        );
    }
}

/// Host-supplied stimuli that are not part of the production host contract.
///
/// A runner may implement these hooks to observe a one-shot timer notification
/// or safely exercise a host-interrupt route. Keeping the hooks here avoids
/// adding test-only operations to `axvisor_api`.
pub trait Stimulus {
    /// Initializes test-side per-CPU state after the host adapter has
    /// initialized the current CPU.
    fn init_percpu(&self) {}

    fn verify_oneshot_timer(&self) -> Option<bool> {
        None
    }

    /// Selects a host-valid vector for registration and interrupt-ingress tests.
    fn test_irq_vector(&self) -> usize {
        #[cfg(target_arch = "riscv64")]
        return 10;
        #[cfg(target_arch = "x86_64")]
        return 0x2a;
        #[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
        return 32;
    }

    fn verify_irq_ingress(&self, _vector: usize) -> Option<bool> {
        None
    }
}

/// A runner with no host-specific external stimuli.
pub struct NoStimulus;

impl Stimulus for NoStimulus {}

/// Runs all common host-contract checks against the linked adapter.
pub fn run(stimulus: &'static (impl Stimulus + Sync)) -> Report {
    let mut report = Report::default();
    check_host(&mut report, stimulus);
    check_memory(&mut report);
    check_task(&mut report);
    check_sync(&mut report);
    check_time(&mut report, stimulus);
    check_irq(&mut report, stimulus);
    check_arch(&mut report);
    report
}

fn check_host(report: &mut Report, stimulus: &'static (impl Stimulus + Sync)) {
    let cpu_count = axvisor_api::host::get_host_cpu_num();
    report.record("HostIf", "cpu-discovery", cpu_count > 0);

    if cpu_count == 0 || cpu_count > usize::BITS as usize {
        report.record("HostIf", "percpu-initialization", false);
        return;
    }
    let initialized = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(cpu_count);
    for cpu_id in 0..cpu_count {
        let initialized = initialized.clone();
        tasks.push(task::spawn_task(
            TaskOptions {
                name: String::from("axvisor-conformance-percpu"),
                stack_size: TEST_STACK_SIZE,
                cpu_set: Some(1usize << cpu_id),
            },
            move || {
                axvisor_api::host::init_percpu();
                stimulus.init_percpu();
                initialized.fetch_add(1, Ordering::Release);
            },
        ));
    }
    for task in tasks {
        task::join_task(task);
    }
    report.record(
        "HostIf",
        "percpu-initialization",
        initialized.load(Ordering::Acquire) == cpu_count,
    );
}

fn check_memory(report: &mut Report) {
    const PAGE_SIZE: usize = 4096;
    let mut frames = Vec::with_capacity(RECLAIM_SAMPLE_FRAMES);
    let mut valid = true;
    for index in 0..RECLAIM_SAMPLE_FRAMES {
        let Some(frame) = memory::alloc_frame() else {
            valid = false;
            break;
        };
        let vaddr = memory::phys_to_virt(frame);
        valid &= frame.as_usize().is_multiple_of(PAGE_SIZE);
        valid &= memory::virt_to_phys(vaddr) == frame;
        valid &= frames.iter().all(|(allocated, _)| *allocated != frame);
        let pattern = 0x4652_4545_0000_0000u64 | index as u64;
        // SAFETY: Each address belongs to a distinct live frame returned by
        // the adapter and remains host-accessible until the matching free.
        unsafe { vaddr.as_mut_ptr_of::<u64>().write_volatile(pattern) };
        frames.push((frame, pattern));
    }
    for (frame, pattern) in &frames {
        // SAFETY: All sampled frames are still live here.
        valid &= unsafe {
            memory::phys_to_virt(*frame)
                .as_ptr_of::<u64>()
                .read_volatile()
                == *pattern
        };
    }
    for (frame, _) in frames.drain(..).rev() {
        memory::dealloc_frame(frame);
    }

    let mut reclaimed = Vec::with_capacity(RECLAIM_SAMPLE_FRAMES);
    for _ in 0..RECLAIM_SAMPLE_FRAMES {
        if let Some(frame) = memory::alloc_frame() {
            reclaimed.push(frame);
        } else {
            break;
        }
    }
    valid &= reclaimed.len() == RECLAIM_SAMPLE_FRAMES;
    for frame in reclaimed {
        memory::dealloc_frame(frame);
    }
    report.record(
        "MemoryIf",
        "frame-ownership-stability-and-reclamation",
        valid,
    );

    let Some(first) = memory::alloc_contiguous_frames(2, 2 * PAGE_SIZE) else {
        report.record("MemoryIf", "contiguous-aligned-frames", false);
        return;
    };
    let contiguous_aligned = first.as_usize().is_multiple_of(2 * PAGE_SIZE);
    let end = first + PAGE_SIZE;
    let end_round_trip = memory::virt_to_phys(memory::phys_to_virt(end)) == end;
    memory::dealloc_contiguous_frames(first, 2);
    report.record(
        "MemoryIf",
        "contiguous-aligned-frames",
        contiguous_aligned && end_round_trip,
    );
}

fn check_task(report: &mut Report) {
    let stage = Arc::new(AtomicUsize::new(0));
    let identity_seen = Arc::new(AtomicBool::new(false));
    let stage_for_task = stage.clone();
    let identity_for_task = identity_seen.clone();
    let handle = task::spawn_task(
        TaskOptions {
            name: String::from("axvisor-conformance-task"),
            stack_size: TEST_STACK_SIZE,
            cpu_set: None,
        },
        move || {
            let identity_before_yield = task::current_task();
            stage_for_task.store(1, Ordering::Release);
            task::yield_now();
            let identity_after_yield = task::current_task();
            identity_for_task.store(
                identity_before_yield.is_some() && identity_before_yield == identity_after_yield,
                Ordering::Release,
            );
            stage_for_task.store(2, Ordering::Release);
        },
    );
    task::join_task(handle);
    report.record(
        "TaskIf",
        "spawn-identity-yield-join",
        identity_seen.load(Ordering::Acquire) && stage.load(Ordering::Acquire) == 2,
    );
}

fn check_sync(report: &mut Report) {
    let queue = Arc::new(WaitQueue::new());
    let permits = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let rescued = Arc::new(AtomicBool::new(false));
    let mut waiters = Vec::with_capacity(2);
    for _ in 0..2 {
        let waiter_queue = queue.clone();
        let waiter_permits = permits.clone();
        let waiter_ready = ready.clone();
        let waiter_done = done.clone();
        waiters.push(task::spawn_task(
            TaskOptions {
                name: String::from("axvisor-conformance-waiter"),
                stack_size: TEST_STACK_SIZE,
                cpu_set: None,
            },
            move || {
                waiter_ready.fetch_add(1, Ordering::Release);
                waiter_queue.wait_until(move || {
                    waiter_permits
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                            if value > 0 { Some(value - 1) } else { None }
                        })
                        .is_ok()
                });
                waiter_done.fetch_add(1, Ordering::Release);
            },
        ));
    }

    while ready.load(Ordering::Acquire) != 2 {
        task::yield_now();
    }
    for _ in 0..64 {
        task::yield_now();
    }
    permits.store(1, Ordering::Release);
    queue.wake_one();
    for _ in 0..256 {
        if done.load(Ordering::Acquire) == 1 {
            break;
        }
        task::yield_now();
    }
    let wake_one_ordered = done.load(Ordering::Acquire) == 1;

    permits.store(2, Ordering::Release);
    queue.wake_all();

    let watchdog_queue = queue.clone();
    let watchdog_done = done.clone();
    let watchdog_rescued = rescued.clone();
    let watchdog = task::spawn_task(
        TaskOptions {
            name: String::from("axvisor-conformance-watchdog"),
            stack_size: TEST_STACK_SIZE,
            cpu_set: None,
        },
        move || {
            for _ in 0..WATCHDOG_YIELDS {
                if watchdog_done.load(Ordering::Acquire) == 2 {
                    return;
                }
                task::yield_now();
            }
            watchdog_rescued.store(true, Ordering::Release);
            permits.store(2, Ordering::Release);
            watchdog_queue.wake_all();
        },
    );

    for waiter in waiters {
        task::join_task(waiter);
    }
    task::join_task(watchdog);
    report.record(
        "SyncIf",
        "conditional-wait-and-wakeup-ordering",
        wake_one_ordered && done.load(Ordering::Acquire) == 2 && !rescued.load(Ordering::Acquire),
    );

    // The last owning reference must remain with this task after all waiters
    // terminate. Dropping it then exercises destruction after waiter join.
    let exclusively_owned = Arc::strong_count(&queue) == 1;
    drop(queue);
    report.record("SyncIf", "destruction-after-waiter-join", exclusively_owned);
}

fn check_time(report: &mut Report, stimulus: &impl Stimulus) {
    let mut previous = time::current_time_nanos();
    let mut monotonic = true;
    for _ in 0..1024 {
        let current = time::current_time_nanos();
        monotonic &= current >= previous;
        previous = current;
        spin_loop();
    }
    report.record("TimeIf", "monotonic-time", monotonic);

    match stimulus.verify_oneshot_timer() {
        Some(passed) => report.record("TimeIf", "oneshot-deadline-notification", passed),
        None => report.not_run("TimeIf", "oneshot-deadline-notification"),
    }
}

fn check_irq(report: &mut Report, stimulus: &impl Stimulus) {
    static DISPATCHED_VECTOR: AtomicUsize = AtomicUsize::new(0);

    fn record_dispatch(vector: usize) {
        DISPATCHED_VECTOR.store(vector, Ordering::Release);
    }

    let test_vector = stimulus.test_irq_vector();

    DISPATCHED_VECTOR.store(0, Ordering::Release);
    let registered = axvisor_api::irq::register_irq_handler(test_vector, record_dispatch);
    let dispatched = registered && axvisor_api::irq::handle_irq(test_vector);
    report.record(
        "IrqIf",
        "registration-and-dispatch",
        dispatched && DISPATCHED_VECTOR.load(Ordering::Acquire) == test_vector,
    );

    match stimulus.verify_irq_ingress(test_vector) {
        Some(passed) => report.record("IrqIf", "host-interrupt-ingress-and-dispatch", passed),
        None => report.not_run("IrqIf", "host-interrupt-ingress-and-dispatch"),
    }
}

#[cfg(target_arch = "riscv64")]
fn check_arch(report: &mut Report) {
    let fdt_present = axvisor_api::arch::host_fdt_paddr().is_some();
    axvisor_api::arch::remote_hfence_vvma_all();
    report.record("ArchIf", "riscv-host-facts-and-fence-path", fdt_present);
}

#[cfg(target_arch = "x86_64")]
fn check_arch(report: &mut Report) {
    report.record(
        "ArchIf",
        "x86-host-tsc-frequency",
        axvisor_api::arch::host_tsc_frequency_mhz().is_some_and(|mhz| mhz > 0),
    );
}

#[cfg(target_arch = "aarch64")]
fn check_arch(report: &mut Report) {
    let fdt_present = axvisor_api::arch::host_fdt_paddr().is_some();
    let gic_present = axvisor_api::arch::get_host_gicd_base().as_usize() != 0;
    report.record(
        "ArchIf",
        "aarch64-host-facts-and-gic",
        fdt_present && gic_present,
    );
}

#[cfg(target_arch = "loongarch64")]
fn check_arch(report: &mut Report) {
    report.record(
        "ArchIf",
        "loongarch-host-fdt",
        axvisor_api::arch::host_fdt_paddr().is_some(),
    );
}
