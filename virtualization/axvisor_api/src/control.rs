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

//! Host control endpoint APIs for AxVisor.
//!
//! The host adapter provides file/device plumbing and memory backing. Shared
//! frontends own control-command semantics and per-file state.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use ax_errno::AxResult;

use crate::memory::PhysAddr;

/// A core-provided identifier, unique across all control frontends.
pub type ControlFileId = u64;

/// A userspace file descriptor returned to the current userspace task.
///
/// On Unix-like hosts this is typically a file descriptor.
pub type Fd = i32;

/// A published host control-device endpoint.
pub type EndpointId = u64;

/// An owned handle to a host-provided mappable memory area.
///
/// The raw identifier belongs to the host adapter. Share this object through
/// `Arc<MmapArea>`; its destructor calls `release_mmap_area` exactly once. User
/// mappings and pinned pages must retain an Arc or their own storage reference.
#[derive(Debug)]
pub struct MmapArea {
    raw: usize,
}

impl Drop for MmapArea {
    #[inline]
    fn drop(&mut self) {
        let _ = release_mmap_area(self.raw);
    }
}

impl MmapArea {
    /// Creates a zero-initialized host memory area and owns its release obligation.
    #[inline]
    pub fn new(len: usize) -> AxResult<Self> {
        Ok(Self {
            raw: create_mmap_area(len)?,
        })
    }

    /// Borrows the host identifier while this handle remains held.
    /// This does not transfer ownership or acquire another storage reference.
    #[inline]
    pub fn as_raw(&self) -> usize {
        self.raw
    }
}

/// Access permissions requested or permitted for a userspace mapping.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MmapProtection {
    /// Permit reads.
    pub read: bool,
    /// Permit writes.
    pub write: bool,
    /// Permit instruction fetches.
    pub execute: bool,
}

/// Frontend operation selecting the backing for a userspace file mapping.
///
/// Arguments are the file identity, file byte offset, byte length, requested
/// permissions, and whether the mapping is shared. The result contains an owned
/// area handle, byte offset within that area, and maximum permitted permissions.
///
/// Called in sleepable syscall context. The host checks bounds, alignment and
/// permissions and installs the user mapping, retaining backing storage before
/// dropping the returned handle. The maximum permissions also constrain later
/// protection changes. Private mappings must preserve private-write semantics;
/// a frontend or host may reject unsupported mapping kinds, not silently share.
pub type MmapCallback = fn(
    ControlFileId,
    usize,
    usize,
    MmapProtection,
    bool,
) -> AxResult<(Arc<MmapArea>, usize, MmapProtection)>;

/// A host-provided identifier for one retained userspace file descriptor reference.
pub type UserFdRefId = u64;

/// A host-provided identifier for one pinned page range, from either source.
pub type PinnedPagesId = u64;

/// A host-provided identifier for a retained userspace address space.
pub type UserAddressSpaceId = u64;

/// Callback invoked after a host eventfd notification is consumed.
pub type EventFdCallback = Box<dyn Fn() + Send + Sync>;

/// Optional eventfd notification operations supplied by a host adapter.
///
/// The host consumes eventfd counts, coalescing writes as necessary. Existing
/// pending counts also trigger a notification. Callbacks run in task context
/// with local interrupts enabled and without host eventfd/queue locks held;
/// they must not sleep. Register failure leaves no callbacks or consumed events.
/// The retained fd must remain valid until `unregister` returns.
#[derive(Clone, Copy)]
pub struct EventFdOps {
    /// Registers a callback for a retained eventfd reference.
    pub register: fn(UserFdRefId, EventFdCallback) -> AxResult<u64>,
    /// Removes a callback and waits for all queued/running callbacks to finish.
    /// Called once, in sleepable context, without core locks held. No callback
    /// may start or remain running after this operation returns.
    pub unregister: fn(u64),
}

/// Stable host physical pages backing a user range or mappable area.
pub struct PinnedPages {
    /// Host-owned identifier used to release the pinned pages.
    pub id: PinnedPagesId,
    /// Page-sized host physical addresses, in requested range order.
    pub pages: Vec<PhysAddr>,
}

/// Optional positioned byte I/O for a control file. The host copies between
/// userspace and these buffers; the frontend interprets offsets and contents.
#[derive(Clone, Copy)]
pub struct FileIoOps {
    /// Reads bytes at a file offset in sleepable task context.
    pub read: fn(ControlFileId, u64, &mut [u8]) -> AxResult<usize>,
    /// Writes bytes at a file offset in sleepable task context.
    pub write: fn(ControlFileId, u64, &[u8]) -> AxResult<usize>,
}

/// Operations implemented by `axvisor-core` for one control file.
#[derive(Clone, Copy)]
pub struct ControlOps {
    /// Opens a new control file.
    pub open: fn(context: u64) -> AxResult<ControlFileId>,
    /// Closes a previously opened control file.
    pub close: fn(ControlFileId) -> AxResult,
    /// Dispatches a frontend ioctl command.
    ///
    /// The `cmd` and `arg` values are the raw ioctl request and third argument
    /// supplied by userspace. The callback return value is the ioctl syscall
    /// return value. Frontend commands decide whether `arg` is unused, an immediate
    /// value, or a userspace pointer.
    pub ioctl: fn(ControlFileId, u32, usize) -> AxResult<isize>,
    /// Selects backing storage; `None` means this file cannot be mapped.
    pub mmap: Option<MmapCallback>,
    /// Optional byte I/O; `None` preserves ioctl/mmap-only file behavior.
    pub io: Option<FileIoOps>,
}

/// The host control endpoint API required by AxVisor.
#[crate::api_def]
pub trait ControlIf {
    // Endpoint publication.

    /// Registers a host-visible control endpoint.
    ///
    /// The host applies its device access policy. Registration failure must
    /// leave no visible endpoint; callbacks may run before this call returns.
    /// `name` is relative to the host device namespace, e.g. `vmm/test`; the
    /// host copies it as needed. `context` is passed unchanged to `ops.open`.
    fn register_endpoint(name: &str, context: u64, ops: ControlOps) -> AxResult<EndpointId>;

    /// Prevents new opens and waits for in-flight open callbacks to finish.
    /// Existing files retain their state and operation tables independently.
    /// Call once per successful registration, in sleepable context, outside
    /// the endpoint's callbacks. Frontend code and state needed by existing
    /// files must remain alive independently of the registration.
    fn unregister_endpoint(_id: EndpointId) -> AxResult {
        Err(ax_errno::AxError::Unsupported)
    }

    // Userspace file descriptors.

    /// Creates a userspace file descriptor owned by the current userspace process.
    ///
    /// On success, the open file description owns `control_file` and closes it
    /// exactly once after its last reference (including dup and file mappings).
    /// File mappings are dispatched through `ops.mmap`. On failure,
    /// ownership remains with `axvisor-core`.
    fn create_user_fd(control_file: ControlFileId, ops: ControlOps) -> AxResult<Fd>;

    /// Retains a userspace file descriptor from the current userspace task.
    ///
    /// The host may implement this by capturing the kernel object referenced by
    /// `fd`, such as a file, doorbell, or other userspace-visible handle.
    fn get_user_fd_ref(fd: Fd) -> AxResult<UserFdRefId>;

    /// Writes raw bytes to a previously retained userspace fd.
    ///
    /// The host performs the underlying object I/O. `axvisor-core` owns any
    /// higher-level protocol semantics imposed on those bytes.
    fn write_user_fd_ref(user_fd_ref: UserFdRefId, buf: &[u8]) -> AxResult<usize>;

    /// Reads raw bytes from a previously retained userspace fd.
    ///
    /// For a core frontend file, this internal operation returns its
    /// [`ControlFileId`] as eight native-endian bytes without changing the file
    /// offset. The buffer must have room for that value. This does not change
    /// userspace `read` semantics. Other files retain their normal read behavior,
    /// including consuming eventfd counts.
    ///
    /// If the fd is not currently readable, the host may wait for a readable
    /// event or return [`ax_errno::AxErrorKind::WouldBlock`]. The caller must
    /// arrange to wake a waiting reader when cancelling it, and wait for the
    /// read to finish before releasing the retained fd.
    fn read_user_fd_ref(user_fd_ref: UserFdRefId, buf: &mut [u8]) -> AxResult<usize>;

    /// Returns optional eventfd callback operations. Hosts without this
    /// capability may omit this method and use the listener-task implementation.
    fn eventfd_ops() -> Option<EventFdOps> {
        None
    }

    /// Releases a previously retained userspace fd.
    fn release_user_fd_ref(user_fd_ref: UserFdRefId) -> AxResult;

    // Userspace-mappable memory areas.

    /// Creates a zero-initialized userspace-mappable memory area.
    /// Returns a fresh host identifier with one release obligation. The backing
    /// must support cross-thread access. `MmapArea::new` wraps the identifier;
    /// callers share that ownership record through Arc.
    fn create_mmap_area(len: usize) -> AxResult<usize>;

    /// Reads bytes from a userspace-mappable memory area.
    fn read_mmap_area(area: &MmapArea, offset: usize, buf: &mut [u8]) -> AxResult;

    /// Writes bytes into a userspace-mappable memory area.
    fn write_mmap_area(area: &MmapArea, offset: usize, buf: &[u8]) -> AxResult;

    /// Releases the host reference owned by a mappable-area handle.
    /// Called by the last handle's destructor; callers using `MmapArea` must
    /// drop the handle instead of calling this directly. Independent user
    /// mapping/page references keep storage alive. The adapter must defer
    /// reclamation if the calling context cannot sleep.
    fn release_mmap_area(raw: usize) -> AxResult;

    // Current userspace address space access.

    /// Copies bytes from the current userspace task.
    ///
    /// This is the host-neutral copy-from-user primitive used by KVM ioctls
    /// whose third argument is a userspace pointer. The host validates and
    /// copies from its current user address space; `axvisor-core` owns the ABI
    /// layout and command semantics.
    fn copy_from_user(addr: usize, buf: &mut [u8]) -> AxResult;

    /// Copies bytes into the current userspace task.
    ///
    /// This is the host-neutral copy-to-user primitive used by KVM ioctls whose
    /// third argument is a userspace pointer. The host validates and copies
    /// into its current user address space; `axvisor-core` owns the ABI layout
    /// and command semantics.
    fn copy_to_user(addr: usize, buf: &[u8]) -> AxResult;

    /// Returns whether a signal should interrupt the current userspace control operation.
    ///
    /// `blocked_signals` uses the native signal-set representation supplied through
    /// `KVM_SET_SIGNAL_MASK`. An empty slice asks the host to use the current thread's normal
    /// signal mask. The host must not consume a deliverable signal when reporting it here.
    fn current_thread_has_pending_signal(blocked_signals: &[u8]) -> AxResult<bool>;

    /// Interruptibly waits for a control predicate using the KVM signal mask.
    /// Returns when the predicate is true, or Interrupted for a deliverable
    /// signal. Predicate checking and queue notification must
    /// not lose a wakeup. Generic synchronization waits retain their semantics.
    #[cfg(target_arch = "aarch64")]
    fn wait_control_until(
        queue: usize,
        condition: Box<dyn Fn() -> bool + Send + 'static>,
        blocked_signals: &[u8],
    ) -> AxResult;

    // Retained userspace address spaces and pinned pages.

    /// Retains the current task's userspace address space.
    ///
    /// The returned handle lets a later control operation resolve userspace
    /// virtual addresses against the address space that registered them,
    /// even when that operation runs in a different host task.
    fn retain_current_user_address_space() -> AxResult<UserAddressSpaceId>;

    /// Releases a previously retained userspace address space.
    fn release_user_address_space(id: UserAddressSpaceId) -> AxResult;

    /// Pins userspace memory from a retained address space and returns its backing pages.
    ///
    /// The host must keep the returned pages stable until [`release_pinned_pages`]
    /// is called with the returned handle. The host may implement this by pinning
    /// frames, holding VM object references, or any other mechanism with the same
    /// lifetime semantics.
    ///
    /// With `dma`, the host must additionally validate long-term DMA suitability
    /// and keep the backing stable across migration and COW. Unsupported hosts
    /// must reject DMA requests. The caller must revoke device access before
    /// releasing the pages.
    fn pin_user_pages(
        user_address_space: UserAddressSpaceId,
        addr: usize,
        len: usize,
        writable: bool,
        dma: bool,
    ) -> AxResult<PinnedPages>;

    /// Pins pages directly from a mappable object without a user mapping.
    /// The byte range must be page aligned. Successful handles retain the
    /// object's backing pages, shared with all user mappings of that range.
    fn pin_mmap_area(
        _area: &MmapArea,
        _offset: usize,
        _len: usize,
        _writable: bool,
    ) -> AxResult<PinnedPages> {
        Err(ax_errno::AxError::Unsupported)
    }

    /// Releases a pinned range from either userspace or a mappable object.
    /// The caller must first remove guest mappings and complete TLB invalidation.
    fn release_pinned_pages(id: PinnedPagesId) -> AxResult;
}
