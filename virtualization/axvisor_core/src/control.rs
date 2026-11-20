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

//! Shared control-frontend synchronization and VM identities.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use axvisor_api::sync::WaitQueue;

static NEXT_VM_ID: AtomicUsize = AtomicUsize::new(1);

pub(crate) fn next_vm_id() -> AxResult<usize> {
    NEXT_VM_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| AxError::NoMemory)
}

/// Serializes stateful operations without holding a spin lock while sleeping.
pub(crate) struct OperationLock {
    held: AtomicBool,
    wait_queue: WaitQueue,
}

impl OperationLock {
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn try_lock(self: &Arc<Self>) -> Option<OperationGuard> {
        self.held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| OperationGuard {
                lock: Arc::clone(self),
            })
    }

    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            held: AtomicBool::new(false),
            wait_queue: WaitQueue::new(),
        })
    }

    pub(crate) fn lock(self: &Arc<Self>) -> OperationGuard {
        if self
            .held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            let lock = Arc::clone(self);
            self.wait_queue.wait_until(move || {
                lock.held
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            });
        }
        OperationGuard {
            lock: Arc::clone(self),
        }
    }
}

pub(crate) struct OperationGuard {
    lock: Arc<OperationLock>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.lock.held.store(false, Ordering::Release);
        self.lock.wait_queue.wake_one();
    }
}
