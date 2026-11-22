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

//! Architecture-specific APIs for the AxVisor hypervisor.
//!
//! This module provides APIs that are specific to certain CPU architectures.
//! Currently, it mainly contains AArch64 GIC (Generic Interrupt Controller)
//! related operations.
//!
//! # Supported Architectures
//!
//! - **AArch64**: GIC distributor and redistributor operations for virtual
//!   interrupt injection and GIC initialization.
//! - **x86_64**: host TSC frequency discovery for guest CPUID emulation.
//! - **DTB boot architectures**: host FDT discovery for guest FDT generation.
//!
//! # Usage
//!
//! The API functions in this module are conditionally compiled based on the
//! target architecture. On non-AArch64 platforms, this module is essentially
//! empty.
//!
//! # Implementation
//!
//! To implement these APIs, use the [`api_impl`](crate::api_impl) attribute
//! macro on an impl block:
//!
//! ```rust,ignore
//! struct ArchIfImpl;
//!
//! #[axvisor_api::api_impl]
//! impl axvisor_api::arch::ArchIf for ArchIfImpl {
//!     // Implement the required functions...
//! }
//! ```

use super::memory::PhysAddr;

/// The API trait for architecture-specific functionalities.
///
/// This trait defines the interface for architecture-specific operations
/// required by the hypervisor. Implementations should be provided by the
/// host system or HAL layer.
#[crate::api_def]
pub trait ArchIf {
    /// Stable identity of the CPU executing a translation synchronization.
    /// Caller must prevent migration through the subsequent guest access.
    /// None preserves conservative unconditional flushing on existing adapters.
    fn translation_cpu_id() -> Option<usize> {
        None
    }

    /// Nonblocking doorbell for a published, CPU-pinned guest executor.
    /// The provider may be called from IRQ context. It must arrange an exit
    /// on the specified host CPU without waiting for an IPI acknowledgement.
    /// A delayed IPI after unbind may be spurious. Pending work is owned by
    /// the vCPU and is checked again at entry. This does not invalidate a TLB.
    /// Providers of this operation must also supply `translation_cpu_id`.
    fn guest_exit_ipi() -> Option<fn(usize)> {
        None
    }

    /// Returns the current physical CPU. The caller must prevent migration
    /// throughout publication and guest entry, not merely during this call.
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    fn current_cpu_id() -> usize;

    /// Forces `cpu` out of guest execution and waits for its host IPI handler.
    /// Call with interrupts enabled and without holding Core locks. This does
    /// not inject a guest interrupt or wait for the target KVM_RUN to return.
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    fn kick_cpu_wait(cpu: usize) -> ax_errno::AxResult;

    /// Returns the current physical hart ID, if the host can identify it.
    ///
    /// This is the hardware ID used in the host FDT, not a logical CPU index.
    /// Callers must prevent migration while associating it with per-CPU state.
    #[cfg(target_arch = "riscv64")]
    fn current_hart_id() -> Option<usize>;

    /// Returns the physical address of the host-provided FDT blob, if any.
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    ))]
    fn host_fdt_paddr() -> Option<PhysAddr>;

    /// Get the host TSC frequency in MHz.
    ///
    /// This is used by x86 vCPU CPUID emulation when the host CPUID frequency
    /// leaf does not provide a usable value.
    #[cfg(target_arch = "x86_64")]
    fn host_tsc_frequency_mhz() -> Option<u32>;

    /// Calibrated, stable TSC rate for guests using an unscaled host TSC.
    /// Hosts without a stability guarantee leave frequency reporting disabled.
    #[cfg(target_arch = "x86_64")]
    fn guest_tsc_frequency_khz() -> Option<u32> {
        None
    }

    /// Flushes guest VS-stage address-translation caches on all host CPUs.
    ///
    /// RISC-V SBI RFENCE calls require the firmware/hypervisor to complete
    /// remote TLB maintenance before returning to the guest.
    #[cfg(target_arch = "riscv64")]
    fn remote_hfence_vvma_all();

    /// Inject a virtual interrupt to the current virtual CPU using hardware
    /// virtualization support.
    ///
    /// This function uses the GIC virtualization interface to directly inject
    /// an interrupt into the guest without causing a VM exit.
    ///
    /// # Arguments
    ///
    /// * `vector` - The interrupt vector number to inject.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms with GICv2/v3
    /// virtualization extensions.
    #[cfg(target_arch = "aarch64")]
    fn hardware_inject_virtual_interrupt(vector: u8);

    /// Read the TYPER (Type Register) of the GIC distributor.
    ///
    /// The TYPER register provides information about the GIC implementation,
    /// including the maximum number of SPIs supported and the number of
    /// implemented CPU interfaces.
    ///
    /// # Returns
    ///
    /// The 32-bit value of the GICD_TYPER register.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms.
    #[cfg(target_arch = "aarch64")]
    fn read_vgicd_typer() -> u32;

    /// Read the IIDR (Implementer Identification Register) of the GIC
    /// distributor.
    ///
    /// The IIDR register provides identification information about the GIC
    /// implementation, including the implementer, revision, and variant.
    ///
    /// # Returns
    ///
    /// The 32-bit value of the GICD_IIDR register.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms.
    #[cfg(target_arch = "aarch64")]
    fn read_vgicd_iidr() -> u32;

    /// Get the base physical address of the GIC distributor in the host system.
    ///
    /// The GIC distributor is responsible for interrupt prioritization and
    /// distribution to CPU interfaces.
    ///
    /// # Returns
    ///
    /// The physical address of the GICD (GIC Distributor) registers.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms.
    #[cfg(target_arch = "aarch64")]
    fn get_host_gicd_base() -> PhysAddr;

    /// Get the base physical address of the GIC redistributor in the host
    /// system.
    ///
    /// The GIC redistributor (GICv3+) handles per-PE (Processing Element)
    /// interrupt configuration and provides the LPI (Locality-specific
    /// Peripheral Interrupt) configuration interface.
    ///
    /// # Returns
    ///
    /// The physical address of the GICR (GIC Redistributor) registers.
    ///
    /// # Platform Support
    ///
    /// This function is only available on AArch64 platforms with GICv3 or
    /// later.
    #[cfg(target_arch = "aarch64")]
    fn get_host_gicr_base() -> PhysAddr;

    /// Retrieve the current pending interrupt for the CPU from the physical hardware.
    #[cfg(target_arch = "aarch64")]
    fn fetch_irq() -> u64;
}
