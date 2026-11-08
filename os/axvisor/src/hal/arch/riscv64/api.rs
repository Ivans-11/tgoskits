#[cfg(not(feature = "dyn-plat"))]
compile_error!("riscv64 Axvisor requires the dyn-plat feature");

use axvisor_api::{arch::ArchIf, memory::PhysAddr};

struct ArchImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchImpl {
    fn current_cpu_id() -> usize {
        ax_hal::percpu::this_cpu_id()
    }

    fn kick_cpu_wait(cpu: usize) -> ax_errno::AxResult {
        // SAFETY: the callback ignores its null argument and completes before return.
        unsafe {
            std::os::arceos::modules::ax_ipi::run_on_cpu_sync_raw(
                cpu,
                |_| {},
                core::ptr::null_mut(),
            )
        }
        .map_err(|_| ax_errno::AxError::InvalidInput)
    }

    fn current_hart_id() -> Option<usize> {
        axplat_dyn::cpu_hardware_id(ax_hal::percpu::this_cpu_id())
    }

    fn host_fdt_paddr() -> Option<PhysAddr> {
        let bootarg = ax_hal::dtb::get_bootarg();
        (bootarg != 0).then(|| bootarg.into())
    }

    fn remote_hfence_vvma_all() {
        axvisor_core::arch::riscv64::hfence_vvma_all();
    }
}
