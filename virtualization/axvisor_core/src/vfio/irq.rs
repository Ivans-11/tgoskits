// SPDX-License-Identifier: Apache-2.0

//! Signal eventfds directly when supported, otherwise defer to a host task.

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axvisor_api::{
    control,
    device::IrqHandle,
    sync::WaitQueue,
    task::{self, TaskHandle, TaskOptions},
};

use crate::device::DeviceAssignment;

pub(super) struct Notification {
    signal: Option<fn(control::UserFdRefId) -> AxResult>,
    fd: SpinNoIrq<Option<control::UserFdRefId>>,
    pending: AtomicBool,
    stopped: AtomicBool,
    wait: WaitQueue,
}

impl Notification {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            signal: control::eventfd_ops().and_then(|ops| ops.signal),
            fd: SpinNoIrq::new(None),
            pending: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            wait: WaitQueue::new(),
        })
    }

    pub(super) fn signal(&self) {
        if let Some(signal) = self.signal {
            // Retain the binding through the nonblocking host call. Teardown
            // clears it before releasing the fd, including software triggers.
            let fd = self.fd.lock();
            if let Some(fd) = *fd
                && signal(fd).is_ok()
            {
                return;
            }
        }
        self.pending.store(true, Ordering::Release);
        self.wait.wake_one();
    }
}

pub(super) struct Irq {
    handle: Option<IrqHandle>,
    notification: Arc<Notification>,
    task: Option<TaskHandle>,
    fd: Option<control::UserFdRefId>,
}

impl Irq {
    pub(super) fn bind(
        assignment: &mut DeviceAssignment,
        source: u32,
        fd: control::UserFdRefId,
        pci: Arc<axdevice::pci::PciFunction>,
        notification: Arc<Notification>,
    ) -> AxResult<Self> {
        notification.stopped.store(false, Ordering::Release);
        let handle = assignment.bind_interrupt(
            source,
            Box::new(move || {
                pci.interrupt();
            }),
        )?;
        *notification.fd.lock() = Some(fd);
        let state = notification.clone();
        let task = task::spawn_task(
            TaskOptions {
                name: "vfio-irq".into(),
                stack_size: 0x10000,
                cpu_set: None,
            },
            move || loop {
                let ready = state.clone();
                state.wait.wait_until(move || {
                    ready.stopped.load(Ordering::Acquire) || ready.pending.load(Ordering::Acquire)
                });
                if state.stopped.load(Ordering::Acquire) {
                    break;
                }
                if state.pending.swap(false, Ordering::AcqRel)
                    && let Err(error) = control::write_user_fd_ref(fd, &1u64.to_ne_bytes())
                {
                    warn!("VFIO eventfd notification failed: {error:?}");
                }
            },
        );
        Ok(Self {
            handle: Some(handle),
            notification,
            task: Some(task),
            fd: Some(fd),
        })
    }

    pub(super) fn unmask(&self, assignment: &DeviceAssignment) -> AxResult {
        assignment.set_interrupt_mask(self.handle.ok_or(AxError::BadState)?, false)
    }

    pub(super) fn trigger(&self) {
        self.notification.signal();
    }

    pub(super) fn stop(&mut self, assignment: &mut DeviceAssignment) -> AxResult {
        if let Some(handle) = self.handle {
            assignment.unbind_interrupt(handle)?;
            self.handle = None;
        }
        *self.notification.fd.lock() = None;
        self.notification.stopped.store(true, Ordering::Release);
        self.notification.wait.wake_one();
        if let Some(task) = self.task.take() {
            task::join_task(task);
        }
        if let Some(fd) = self.fd {
            control::release_user_fd_ref(fd)?;
            self.fd = None;
        }
        Ok(())
    }
}
