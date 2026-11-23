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

#[cfg(target_arch = "x86_64")]
use ax_errno::AxError;
use ax_errno::AxResult;
#[cfg(target_arch = "x86_64")]
use axaddrspace::GuestPhysAddr;
use axvisor_api::control as api_control;
#[cfg(target_arch = "x86_64")]
use axvm::AxVMRef;

#[cfg(target_arch = "x86_64")]
use crate::kvm::abi::raw as abi;
#[cfg(all(target_arch = "x86_64", feature = "vmx"))]
use crate::kvm::queue_vcpu_startup_by_id;
#[cfg(all(target_arch = "x86_64", feature = "svm"))]
use crate::kvm::set_vcpu_file_mp_state_by_id;
#[cfg(target_arch = "x86_64")]
use crate::kvm::{
    cpuid::default_tsc_khz,
    state::{PvClockVcpuTimeInfo, PvClockWallClock},
    util::{read_mmap_area_u8, write_mmap_area_u8, write_mmap_area_u16},
};
#[cfg(target_arch = "x86_64")]
use crate::vmm::vcpus::guest_cpu_id_to_vcpu_id;

#[cfg(target_arch = "x86_64")]
pub(super) fn handle_kvm_msr_write(
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    msr: usize,
    value: u64,
) -> AxResult<bool> {
    match msr {
        abi::MSR_KVM_WALL_CLOCK | abi::MSR_KVM_WALL_CLOCK_NEW => {
            if value != 0 {
                write_kvm_wall_clock(vm, GuestPhysAddr::from(value as usize))?;
            }
            Ok(true)
        }
        abi::MSR_KVM_SYSTEM_TIME | abi::MSR_KVM_SYSTEM_TIME_NEW => {
            if value & abi::KVM_SYSTEM_TIME_ENABLE != 0 {
                let gpa = GuestPhysAddr::from((value & !abi::KVM_SYSTEM_TIME_ENABLE) as usize);
                write_kvm_system_time(vm, vcpu, gpa)?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(target_arch = "x86_64")]
pub(in crate::kvm) fn apply_kvm_pv_msr_write(
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    msr: usize,
    value: u64,
) -> AxResult<bool> {
    handle_kvm_msr_write(vm, vcpu, msr, value)
}

#[cfg(target_arch = "x86_64")]
pub(super) fn handle_cpu_up(
    control_file: api_control::ControlFileId,
    vm: &AxVMRef,
    target_cpu: usize,
    entry_point: GuestPhysAddr,
) -> AxResult {
    let target_vcpu_id = guest_cpu_id_to_vcpu_id(vm, target_cpu).ok_or(AxError::InvalidInput)?;
    #[cfg(feature = "svm")]
    {
        let target_vcpu = vm.vcpu(target_vcpu_id).ok_or(AxError::InvalidInput)?;
        target_vcpu.set_entry(entry_point)?;
        set_vcpu_file_mp_state_by_id(control_file, target_vcpu_id, abi::KVM_MP_STATE_RUNNABLE)
    }
    #[cfg(feature = "vmx")]
    {
        queue_vcpu_startup_by_id(control_file, target_vcpu_id, entry_point)
    }
}

#[cfg(target_arch = "x86_64")]
pub(super) use crate::vmm::devices::x86::handle_in_kernel_device_exit;

#[cfg(target_arch = "x86_64")]
pub(super) fn inject_in_kernel_device_irqs(
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    irqchip_created: bool,
    pit2_created: bool,
) -> bool {
    if !irqchip_created {
        return false;
    }
    let mut injected = false;
    if pit2_created {
        injected |= crate::vmm::devices::x86::inject_due_pit_irq_for_control(vm, vcpu, 0);
    }
    injected |= crate::vmm::devices::x86::inject_pending_serial_irq_for_control(vm, vcpu);
    injected
}

#[cfg(target_arch = "x86_64")]
pub(super) fn handle_control_ioapic_eoi(
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    irqchip_created: bool,
    vector: Option<u8>,
) {
    if irqchip_created && let Some(vector) = vector {
        crate::vmm::devices::x86::inject_pending_ioapic_irq_after_eoi_for_control(vm, vcpu, vector);
    }
}

#[cfg(target_arch = "x86_64")]
pub(super) fn update_vcpu_run_interrupt_state(
    mmap_area: &api_control::MmapArea,
    vcpu: &axvm::AxVCpuRef,
) -> AxResult {
    let if_flag = vcpu
        .get_arch_reg(axvcpu::x86_regs::RFLAGS)
        .map(|rflags| u8::from(rflags & abi::X86_RFLAGS_IF != 0))
        .unwrap_or(1);

    write_mmap_area_u8(
        mmap_area,
        abi::KVM_RUN_READY_FOR_INTERRUPT_INJECTION_OFFSET,
        if_flag,
    )?;
    write_mmap_area_u8(mmap_area, abi::KVM_RUN_IF_FLAG_OFFSET, if_flag)?;
    write_mmap_area_u16(mmap_area, abi::KVM_RUN_FLAGS_OFFSET, 0)?;
    Ok(())
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn update_vcpu_run_interrupt_state(
    _mmap_area: &api_control::MmapArea,
    _vcpu: &axvm::AxVCpuRef,
) -> AxResult {
    Ok(())
}

#[cfg(target_arch = "x86_64")]
pub(super) fn vcpu_run_irq_window_open(
    mmap_area: &api_control::MmapArea,
    vcpu: &axvm::AxVCpuRef,
) -> AxResult<bool> {
    if read_mmap_area_u8(mmap_area, abi::KVM_RUN_REQUEST_INTERRUPT_WINDOW_OFFSET)? == 0 {
        return Ok(false);
    }
    update_vcpu_run_interrupt_state(mmap_area, vcpu)?;
    Ok(read_mmap_area_u8(mmap_area, abi::KVM_RUN_READY_FOR_INTERRUPT_INJECTION_OFFSET)? != 0)
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn vcpu_run_irq_window_open(
    _mmap_area: &api_control::MmapArea,
    _vcpu: &axvm::AxVCpuRef,
) -> AxResult<bool> {
    Ok(false)
}

#[cfg(target_arch = "x86_64")]
fn write_kvm_wall_clock(vm: &AxVMRef, gpa: GuestPhysAddr) -> AxResult {
    let now_ns = axvisor_api::time::current_time_nanos() as u64;
    let wall_clock = PvClockWallClock {
        version: 2,
        sec: (now_ns / 1_000_000_000) as u32,
        nsec: (now_ns % 1_000_000_000) as u32,
    };
    vm.write_to_guest_of(gpa, &wall_clock)
}

#[cfg(target_arch = "x86_64")]
fn write_kvm_system_time(vm: &AxVMRef, vcpu: &axvm::AxVCpuRef, gpa: GuestPhysAddr) -> AxResult {
    const IA32_TSC: u32 = 0x10;

    let tsc_khz = default_tsc_khz().max(1);
    let info = PvClockVcpuTimeInfo {
        version: 2,
        pad0: 0,
        // The guest observes the hardware TSC plus the per-vCPU offset.  The
        // pvclock reference point must use that same domain; using the raw
        // host TSC makes clocks disagree as execution migrates between vCPUs.
        tsc_timestamp: vcpu.get_msr(IA32_TSC)?,
        system_time: axvisor_api::time::current_time_nanos() as u64,
        tsc_to_system_mul: tsc_to_system_mul(tsc_khz),
        tsc_shift: 0,
        // Do not claim cross-vCPU TSC stability until offset synchronization
        // and migration handling provide that guarantee.
        flags: 0,
        pad: [0; 2],
    };
    vm.write_to_guest_of(gpa, &info)
}

#[cfg(target_arch = "x86_64")]
fn tsc_to_system_mul(tsc_khz: u32) -> u32 {
    (((1_000_000u128) << 32) / u128::from(tsc_khz)).min(u128::from(u32::MAX)) as u32
}
