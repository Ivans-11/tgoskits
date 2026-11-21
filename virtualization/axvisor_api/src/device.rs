// SPDX-License-Identifier: Apache-2.0

//! Physical device assignment, independent of the VM management frontend.
//!
//! Enabled by the `device` feature. The host owns authorization, physical
//! device arbitration, IRQ allocation and IOMMU isolation. The caller owns
//! guest resource presentation, virtual interrupt routing and VM lifetime.
//! No operation implicitly unbinds another driver or broadens authorization.
//!
//! Operations run in sleepable context, except the IRQ callback. Do not hold
//! core locks that a callback can acquire while calling this interface. A
//! failed teardown keeps its handle valid and retains resources for retry;
//! dropping a numeric handle never releases a physical resource.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use ax_errno::{AxError, AxResult};
pub use axaddrspace::device::AccessWidth;

use crate::memory::PhysAddr;

macro_rules! handle {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(transparent)]
        pub struct $name(pub u64);
    };
}

/// PCI location in the host's current enumeration. This identifies a device,
/// not permission to use it; the host checks assignment policy when opening.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PciAddress {
    /// PCI segment/domain.
    pub segment: u16,
    /// Bus number.
    pub bus: u8,
    /// Device number, 0..32.
    pub device: u8,
    /// Function number, 0..8.
    pub function: u8,
}

impl TryFrom<[u16; 4]> for PciAddress {
    type Error = AxError;

    fn try_from([segment, bus, device, function]: [u16; 4]) -> AxResult<Self> {
        if bus > 255 || device > 31 || function > 7 {
            return Err(AxError::InvalidInput);
        }
        Ok(Self {
            segment,
            bus: bus as u8,
            device: device as u8,
            function: function as u8,
        })
    }
}

/// Device locator resolved by the host, separate from permission and ownership.
/// Hosts support the locator kinds available in their device model and return
/// Unsupported for others. Identifiers are not guaranteed stable across hosts.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum DeviceId {
    /// PCI segment, bus, device and function in the host enumeration.
    Pci(PciAddress),
    /// Absolute path in the host's device tree.
    DeviceTreePath(String),
    /// Absolute path in the host's ACPI namespace.
    AcpiPath(String),
    /// Identifier defined and resolved by the host's device manager.
    Host(String),
}

handle!(
    DeviceHandle,
    "An acquired physical device; validated by the host on every use."
);
handle!(IrqHandle, "A registered physical interrupt source.");
handle!(DmaSpace, "An isolated device address space.");
handle!(
    DmaMapping,
    "A live DMA mapping retaining its backing memory."
);

/// Physical resource kind. A region index is local to one device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionKind {
    /// Memory-mapped device registers.
    Mmio,
    /// Port-mapped device registers.
    PortIo,
    /// Configuration accesses requiring host filtering and guest emulation.
    Configuration,
}

/// Access permissions, expressed from the device's perspective for DMA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessPermissions {
    /// Permit reads.
    pub read: bool,
    /// Permit writes.
    pub write: bool,
}

/// One host-authorized physical resource.
#[derive(Clone, Debug)]
pub struct DeviceRegion {
    /// Identifier accepted by `read_region` and `write_region`.
    pub index: u32,
    /// Resource access mechanism.
    pub kind: RegionKind,
    /// Region size in bytes.
    pub size: u64,
    /// Maximum allowed accesses. The host also checks individual offsets.
    pub permissions: AccessPermissions,
    /// Safe, page-aligned ranges that may be directly mapped into a guest.
    /// Empty means every access must use the controlled access operations.
    /// Sensitive MSI/MSI-X/configuration state must not be exposed this way.
    pub direct_mappings: Vec<DirectMapping>,
}

/// A host-authorized subrange for direct CPU mapping.
#[derive(Clone, Debug)]
pub struct DirectMapping {
    /// Offset within the resource.
    pub offset: u64,
    /// Physical address of the first byte.
    pub physical_address: PhysAddr,
    /// Length in bytes.
    pub length: usize,
}

/// Interrupt notification protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptKind {
    /// Edge/message notification; no guest EOI handshake is required.
    Edge,
    /// Host masks on notification. The caller unmasks after guest EOI and
    /// device acknowledgement, so an asserted level cannot flood the host.
    LevelMasked,
}

/// A physical interrupt source available for binding.
#[derive(Clone, Debug)]
pub struct InterruptSource {
    /// Device-local identifier, not a guest GSI or a host CPU vector.
    pub index: u32,
    /// Required notification/acknowledgement behavior.
    pub kind: InterruptKind,
}

/// Resources available for an acquired device.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// Authorized device register/configuration regions.
    pub regions: Vec<DeviceRegion>,
    /// Available physical interrupt sources.
    pub interrupts: Vec<InterruptSource>,
    /// Whether the device requires a DMA address space before use.
    pub dma: bool,
}

/// Initial address-space limits; attach also checks each device's constraints.
#[derive(Clone, Copy, Debug)]
pub struct DmaConstraints {
    /// First usable IOVA.
    pub start: u64,
    /// Last usable IOVA, inclusive.
    pub end: u64,
    /// Minimum mapping granularity in bytes (a power of two).
    pub granule: usize,
}

/// Stable, ordered physical pages with an owning memory lease.
///
/// The host retains a clone for every successful DMA mapping. The lease may
/// own a core allocation or host-approved, long-term-pinned user pages. A
/// transient CPU pin is not sufficient. This type has no dependency on
/// `ControlIf`; acquiring a suitable lease belongs to the memory owner.
#[derive(Clone)]
pub struct DmaBacking {
    pages: Arc<[PhysAddr]>,
    page_size: usize,
    permissions: AccessPermissions,
    _lease: Arc<dyn Send + Sync>,
}

impl DmaBacking {
    /// Describe memory already prepared for device access.
    ///
    /// # Safety
    /// The lease must keep every listed page allocated at the same address
    /// until its final reference is released, including after VM teardown.
    /// The pages must be authorized RAM suitable for long-term DMA, with the
    /// stated permissions. CPU and DMA users must reference these same pages;
    /// no COW, migration, or reuse may replace them while the lease exists.
    /// The host still validates platform DMA suitability when mapping.
    pub unsafe fn from_pages(
        pages: Vec<PhysAddr>,
        page_size: usize,
        permissions: AccessPermissions,
        lease: Arc<dyn Send + Sync>,
    ) -> AxResult<Self> {
        if pages.is_empty()
            || !page_size.is_power_of_two()
            || pages.len().checked_mul(page_size).is_none()
            || (!permissions.read && !permissions.write)
            || pages.iter().any(|p| p.as_usize() % page_size != 0)
        {
            return Err(AxError::InvalidInput);
        }
        Ok(Self {
            pages: pages.into(),
            page_size,
            permissions,
            _lease: lease,
        })
    }

    /// Ordered physical pages retained by this lease.
    pub fn pages(&self) -> &[PhysAddr] {
        &self.pages
    }

    /// Size of each page in bytes.
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Total size in bytes.
    pub fn len(&self) -> usize {
        self.pages.len() * self.page_size
    }

    /// Whether the backing has no bytes (always false for a valid backing).
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Maximum device permissions for this backing.
    pub fn permissions(&self) -> AccessPermissions {
        self.permissions
    }
}

/// Notification in non-sleepable context, without host resource locks held.
/// The callback must not call sleepable `DeviceIf` operations.
pub type DeviceIrqCallback = Box<dyn Fn() + Send + Sync>;

/// An owning hardware interrupt destination supplied by the virtualization
/// core. The host programs interrupt remapping, not guest interrupt routing.
#[derive(Clone)]
pub struct InterruptTarget {
    kind: InterruptTargetKind,
    _lease: Arc<dyn Send + Sync>,
}

/// Architecture-specific information consumed by the host interrupt remapper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InterruptTargetKind {
    /// Intel posted interrupts: a host-physical PI descriptor and guest vector.
    X86Posted {
        /// Cache-coherent, 64-byte-aligned descriptor address.
        descriptor: PhysAddr,
        /// Edge-triggered guest interrupt vector, 32..=255.
        vector: u8,
    },
}

impl InterruptTarget {
    /// Wrap a hardware destination with its owning lease.
    ///
    /// # Safety
    /// The caller must satisfy the selected kind's hardware requirements.
    /// The lease must retain its valid, initialized backing memory. Its owner
    /// must synchronize CPU/hardware accesses, maintain the notification
    /// destination across migration, and wake blocked vCPUs.
    /// These obligations last until the final lease is released, including
    /// when a host cannot safely revoke a hardware route.
    pub unsafe fn from_raw_parts(kind: InterruptTargetKind, lease: Arc<dyn Send + Sync>) -> Self {
        Self {
            kind,
            _lease: lease,
        }
    }

    /// Borrow the remapping information while retaining this target's lease.
    pub fn kind(&self) -> InterruptTargetKind {
        self.kind
    }
}

/// Optional host contract for device assignment, enabled as one feature.
#[crate::api_def]
pub trait DeviceIf {
    /// Acquire a host-authorized device, initially quiescent with DMA blocked.
    /// Validate exclusive ownership and the full isolation/reset unit. Return
    /// Busy if occupied; do not steal, unbind or silently acquire siblings.
    fn open_device(id: &DeviceId) -> AxResult<(DeviceHandle, DeviceInfo)>;

    /// Release a device after guest CPU access has stopped, mappings have been
    /// revoked, IRQs unbound and DMA detached. Perform necessary reset/cleanup.
    /// Failure retains ownership; host policy decides subsequent driver binding.
    fn close_device(device: DeviceHandle) -> AxResult;

    /// Read an authorized, aligned scalar register. Reject inaccessible bytes.
    fn read_region(
        device: DeviceHandle,
        region: u32,
        offset: u64,
        width: AccessWidth,
    ) -> AxResult<u64>;

    /// Write an authorized register. Filter physical bus addresses, MSI state,
    /// bus-master and reset controls; the caller emulates guest configuration.
    fn write_region(
        device: DeviceHandle,
        region: u32,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> AxResult;

    /// Register a source, initially masked. Failure retains no callback.
    fn bind_irq(
        device: DeviceHandle,
        source: u32,
        callback: DeviceIrqCallback,
    ) -> AxResult<IrqHandle>;

    /// Mask/unmask a source. Masking alone does not drain callbacks.
    fn set_irq_mask(irq: IrqHandle, masked: bool) -> AxResult;

    /// Optionally bypass the registered callback using hardware delivery.
    /// `None` restores callback delivery. The host retains the target until
    /// replacement/removal and interrupt-cache invalidation have completed;
    /// successful `unbind_irq` must revoke it too. Masking applies to either
    /// delivery mode. An error preserves the previous lease; if hardware
    /// completion is uncertain, quarantine the source and retain both targets
    /// until revocation can be proven. Unsupported leaves the route unchanged.
    /// Unsupported targets use the existing callback path instead.
    fn set_irq_target(_irq: IrqHandle, _target: Option<InterruptTarget>) -> AxResult {
        Err(AxError::Unsupported)
    }

    /// Stop notifications, wait for all callbacks and release IRQ resources.
    /// After success no callback is running or can start; failure retains it.
    fn unbind_irq(irq: IrqHandle) -> AxResult;

    /// Create an empty, isolated DMA space. A backend may select its IOMMU
    /// instance at first attach, but must validate all mappings before enabling
    /// any device access. No identity/passthrough fallback is permitted.
    fn create_dma_space() -> AxResult<(DmaSpace, DmaConstraints)>;

    /// Destroy a space only when it has no devices and no mappings.
    fn destroy_dma_space(space: DmaSpace) -> AxResult;

    /// Map the full backing with no stronger permissions than its lease.
    /// Retain backing until successful unmap. Mapping changes are rejected
    /// while any device is attached. Overlap and out-of-range IOVAs are errors.
    /// Failure exposes no partial device access; failed rollback quarantines
    /// the space and retains any potentially accessible backing.
    fn map_dma(
        space: DmaSpace,
        iova: u64,
        backing: DmaBacking,
        permissions: AccessPermissions,
    ) -> AxResult<DmaMapping>;

    /// Remove an entire mapping after all devices safely detach, synchronizing
    /// IOTLB invalidation before releasing backing. Failure keeps its lease.
    fn unmap_dma(mapping: DmaMapping) -> AxResult;

    /// Connect a device to a space after checking reserved ranges, address
    /// width, coherency, isolation groups and interrupt remapping. Failure
    /// leaves DMA blocked; uncertain hardware state retains all resources.
    fn attach_device(device: DeviceHandle, space: DmaSpace) -> AxResult;

    /// Quiesce the device and switch it to blocked DMA, not the host's default
    /// passthrough domain. Success guarantees old backing may be unmapped;
    /// IOTLB invalidation alone is not proof of completed in-flight DMA.
    /// Failure retains device/space association and all backing for retry.
    fn detach_device(device: DeviceHandle) -> AxResult;
}
