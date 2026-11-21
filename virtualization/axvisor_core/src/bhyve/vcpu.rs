// SPDX-License-Identifier: Apache-2.0

//! FreeBSD register numbering and single-vCPU execution.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ax_errno::{AxError, AxResult};
use axvcpu::{AxVCpuExitReason, x86_regs as reg};
use axvisor_api::control;

use super::{
    Vm,
    abi::{self, capability, exitcode},
    platform,
};

#[derive(Default)]
pub(super) struct State {
    paused: AtomicBool,
    suspend: AtomicU32,
    halt_exit: AtomicBool,
    ipi_exit: AtomicBool,
}

pub(super) fn state_ioctl(vm: &Vm, command: u32, address: usize) -> AxResult<bool> {
    match command {
        abi::ACTIVATE_CPU | abi::SUSPEND_CPU | abi::RESUME_CPU => {
            cpu(vm, u32::from_le_bytes(abi::read::<4>(address)?))?;
            match command {
                abi::ACTIVATE_CPU => {
                    vm.vcpu_runtime.activate()?;
                }
                abi::SUSPEND_CPU => vm.cpu_state.paused.store(true, Ordering::Release),
                _ => vm.cpu_state.paused.store(false, Ordering::Release),
            }
        }
        abi::SUSPEND => {
            let how = u32::from_le_bytes(abi::read::<4>(address)?);
            if !(1..=4).contains(&how) {
                return Err(AxError::InvalidInput);
            }
            vm.cpu_state
                .suspend
                .compare_exchange(0, how, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| AxError::AlreadyExists)?;
        }
        abi::GET_CPUS => {
            let data = abi::read::<16>(address)?;
            let present = match abi::u32_at(&data, 0) {
                0 => vm.vcpu_runtime.active(),
                1 => vm.cpu_state.paused.load(Ordering::Acquire),
                2 => false,
                _ => return Err(AxError::InvalidInput),
            };
            let len = abi::u32_at(&data, 4) as usize;
            abi::write_cpuset(abi::u64_at(&data, 8) as usize, len, u64::from(present))?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn cpu(vm: &Vm, id: u32) -> AxResult<axvm::AxVCpuRef> {
    if id != 0 {
        return Err(AxError::InvalidInput);
    }
    vm.machine.vcpu(0).ok_or(AxError::BadState)
}

pub(super) fn ioctl(vm: &Vm, command: u32, address: usize) -> AxResult {
    match command {
        abi::RUN => run(vm, address),
        abi::SET_TOPOLOGY => {
            let data = abi::read::<8>(address)?;
            // This frontend currently exposes one socket, core and thread.
            if data[..6] != [1, 0, 1, 0, 1, 0] || !matches!(&data[6..], [0, 0] | [1, 0]) {
                return Err(AxError::InvalidInput);
            }
            Ok(())
        }
        abi::GET_TOPOLOGY => control::copy_to_user(address, &[1, 0, 1, 0, 1, 0, 1, 0]),
        abi::SET_CAPABILITY | abi::GET_CAPABILITY => abi::ioctl::<16>(command, address, |data| {
            let cpu = cpu(vm, abi::u32_at(data, 0))?;
            let cap = abi::u32_at(data, 4);
            let flag = match cap {
                capability::HALT_EXIT => Some(&vm.cpu_state.halt_exit),
                capability::IPI_EXIT => Some(&vm.cpu_state.ipi_exit),
                // Unrestricted guest is required by the VMX backend setup.
                capability::UNRESTRICTED_GUEST => None,
                _ => return Err(AxError::Unsupported),
            };
            if command == abi::GET_CAPABILITY {
                let value = flag.is_none_or(|flag| flag.load(Ordering::Acquire));
                data[8..12].copy_from_slice(&u32::from(value).to_le_bytes());
                return Ok(());
            }
            let requested = abi::u32_at(data, 8);
            if requested > 1 {
                return Err(AxError::InvalidInput);
            }
            if let Some(flag) = flag {
                if cap == capability::IPI_EXIT {
                    cpu.set_init_sipi_exiting(requested != 0)?;
                }
                flag.store(requested != 0, Ordering::Release);
            } else if requested == 0 {
                return Err(AxError::Unsupported);
            }
            Ok(())
        }),
        abi::SET_X2APIC | abi::GET_X2APIC => abi::ioctl::<8>(command, address, |data| {
            let cpu = cpu(vm, abi::u32_at(data, 0))?;
            let base = cpu.get_arch_reg(reg::APIC_BASE)?;
            if command == abi::SET_X2APIC {
                let state = abi::u32_at(data, 4);
                if state > 1 {
                    return Err(AxError::InvalidInput);
                }
                cpu.set_arch_reg(
                    reg::APIC_BASE,
                    (base & !(1 << 10)) | (u64::from(state) << 10),
                )?;
            } else {
                data[4..8].copy_from_slice(&(((base >> 10) & 1) as u32).to_le_bytes());
            }
            Ok(())
        }),
        abi::SET_REGISTER | abi::GET_REGISTER => abi::ioctl::<16>(command, address, |data| {
            let cpu = cpu(vm, abi::u32_at(data, 0))?;
            let key = abi::register(abi::u32_at(data, 4))?;
            if command == abi::SET_REGISTER {
                cpu.set_arch_reg(key, abi::u64_at(data, 8))?;
                if key == reg::RIP {
                    vm.resume_rip.lock().take();
                }
            } else {
                data[8..16].copy_from_slice(&cpu.get_arch_reg(key)?.to_le_bytes());
            }
            Ok(())
        }),
        abi::SET_SEGMENT | abi::GET_SEGMENT => abi::ioctl::<24>(command, address, |data| {
            let cpu = cpu(vm, abi::u32_at(data, 0))?;
            let key = abi::register(abi::u32_at(data, 4))?;
            if !(reg::ES..=reg::IDTR).contains(&key) {
                return Err(AxError::InvalidInput);
            }
            let mut regs = [
                (key + reg::BASE, abi::u64_at(data, 8)),
                (key + reg::LIMIT, abi::u32_at(data, 16).into()),
                (key + reg::ACCESS, abi::u32_at(data, 20).into()),
            ];
            let count = if key < reg::GDTR { 3 } else { 2 };
            if command == abi::SET_SEGMENT {
                cpu.set_arch_regs(&regs[..count])?;
            } else {
                cpu.get_arch_regs(&mut regs[..count])?;
                data[8..16].copy_from_slice(&regs[0].1.to_le_bytes());
                data[16..20].copy_from_slice(&(regs[1].1 as u32).to_le_bytes());
                let access = if count == 3 { regs[2].1 as u32 } else { 0 };
                data[20..24].copy_from_slice(&access.to_le_bytes());
            }
            Ok(())
        }),
        abi::SET_REGISTER_SET | abi::GET_REGISTER_SET => {
            let data = abi::read::<24>(address)?;
            let cpu = cpu(vm, abi::u32_at(&data, 0))?;
            let count = abi::u32_at(&data, 4) as usize;
            if count > 49 {
                return Err(AxError::InvalidInput);
            }
            let keys = abi::u64_at(&data, 8) as usize;
            let values = abi::u64_at(&data, 16) as usize;
            let mut regs = [(0u64, 0u64); 49];
            let regs = &mut regs[..count];
            for (i, entry) in regs.iter_mut().enumerate() {
                let key_address = keys.checked_add(i * 4).ok_or(AxError::BadAddress)?;
                entry.0 = abi::register(u32::from_le_bytes(abi::read::<4>(key_address)?))?;
                if command == abi::SET_REGISTER_SET {
                    let value_address = values.checked_add(i * 8).ok_or(AxError::BadAddress)?;
                    entry.1 = u64::from_le_bytes(abi::read::<8>(value_address)?);
                }
            }
            if command == abi::SET_REGISTER_SET {
                // Clear the saved continuation only after RIP was written.
                // A failing earlier register must leave the pending exit intact.
                if let Some(index) = regs.iter().position(|&(key, _)| key == reg::RIP) {
                    cpu.set_arch_regs(&regs[..=index])?;
                    vm.resume_rip.lock().take();
                    cpu.set_arch_regs(&regs[index + 1..])?;
                } else {
                    cpu.set_arch_regs(regs)?;
                }
            } else {
                cpu.get_arch_regs(regs)?;
                for (i, &(_, value)) in regs.iter().enumerate() {
                    let value_address = values.checked_add(i * 8).ok_or(AxError::BadAddress)?;
                    control::copy_to_user(value_address, &value.to_le_bytes())?;
                }
            }
            Ok(())
        }
        _ => Err(AxError::Unsupported),
    }
}

fn run(vm: &Vm, address: usize) -> AxResult {
    // struct vm_run: cpuid, cpuset pointer/size, vm_exit pointer.
    let input = abi::read::<32>(address)?;
    let vcpu = cpu(vm, abi::u32_at(&input, 0))?;
    let cpuset_address = abi::u64_at(&input, 8) as usize;
    let cpuset_len = abi::u64_at(&input, 16) as usize;
    let destination = abi::u64_at(&input, 24) as usize;
    if !vm.vcpu_runtime.active() {
        return Err(AxError::BadState);
    }
    if !vm.machine.running() {
        vm.machine.boot()?;
    }
    let _context = crate::context::bind_current_vcpu_context(vm.machine.id(), 0);
    if let Some(rip) = vm.resume_rip.lock().take() {
        vcpu.set_arch_reg(reg::RIP, rip)?;
    }
    let mut halted = false;
    loop {
        if vm.runtime.removing.load(Ordering::Acquire) {
            return Err(AxError::NoSuchDevice);
        }
        if control::current_thread_has_pending_signal(&[])? {
            return Err(AxError::Interrupted);
        }
        let how = vm.cpu_state.suspend.load(Ordering::Acquire);
        if how != 0 {
            let instruction = reg::ExitInstruction {
                rip: vcpu.get_arch_reg(reg::RIP)?,
                len: 0,
            };
            let mut payload = [0; 8];
            payload[..4].copy_from_slice(&how.to_le_bytes());
            return abi::write_exit(destination, exitcode::SUSPENDED, instruction, payload);
        }
        if vm.cpu_state.paused.load(Ordering::Acquire) {
            axvisor_api::task::yield_now();
            continue;
        }
        crate::vmm::timer::check_events();
        crate::vmm::devices::x86::inject_due_pit_irq_for_control(&vm.machine, &vcpu, 2);
        let pending = vm.vcpu_runtime.take_interrupts();
        let interrupt_pending = !pending.is_empty();
        for interrupt in pending {
            crate::vmm::interrupt::inject_virtual_interrupt(interrupt, &vcpu)?;
        }
        if halted && !interrupt_pending && !vcpu.has_pending_event() {
            axvisor_api::task::yield_now();
            continue;
        }
        halted = false;
        let exit = vm.machine.run_vcpu_raw(0, |_, _| Ok(false))?;
        if platform::handle_device_exit(vm, &vcpu, &exit)? {
            continue;
        }
        let mut payload = [0u8; 8];
        let code = match exit {
            AxVCpuExitReason::Nothing => continue,
            AxVCpuExitReason::InterruptEnd { vector } => {
                if let Some(vector) = vector {
                    crate::vmm::devices::x86::inject_pending_ioapic_irq_after_eoi_for_control(
                        &vm.machine,
                        &vcpu,
                        vector,
                    );
                }
                continue;
            }
            AxVCpuExitReason::ExternalInterrupt { .. } | AxVCpuExitReason::PreemptionTimer => {
                crate::vmm::vcpus::handle_internal_exit(&vm.machine, &vcpu, &exit);
                continue;
            }
            AxVCpuExitReason::InitSipi {
                target_mask,
                delivery_mode,
                vector,
            } => {
                abi::write_cpuset(cpuset_address, cpuset_len, target_mask)?;
                payload[..4].copy_from_slice(&(u32::from(delivery_mode) << 8).to_le_bytes());
                payload[4] = vector;
                exitcode::IPI
            }
            AxVCpuExitReason::IoRead { port, width } => {
                payload[..2]
                    .copy_from_slice(&((width.size() as u16) | abi::INOUT_IN).to_le_bytes());
                payload[2..4].copy_from_slice(&port.0.to_le_bytes());
                exitcode::INOUT
            }
            AxVCpuExitReason::IoWrite { port, width, data } => {
                payload[..2].copy_from_slice(&(width.size() as u16).to_le_bytes());
                payload[2..4].copy_from_slice(&port.0.to_le_bytes());
                payload[4..8].copy_from_slice(&(data as u32).to_le_bytes());
                exitcode::INOUT
            }
            AxVCpuExitReason::Halt => {
                if !vm.cpu_state.halt_exit.load(Ordering::Acquire) {
                    halted = true;
                    continue;
                }
                payload[..8].copy_from_slice(&vcpu.get_arch_reg(reg::RFLAGS)?.to_le_bytes());
                exitcode::HLT
            }
            AxVCpuExitReason::SystemDown => {
                payload[..4].copy_from_slice(&abi::SUSPEND_HALT.to_le_bytes());
                exitcode::SUSPENDED
            }
            _ => {
                warn!("bhyve unsupported exit: {exit:?}");
                return Err(AxError::Unsupported);
            }
        };
        let instruction = vcpu.exit_instruction()?;
        // FreeBSD exposes the exiting instruction through GET_REGISTER(RIP),
        // applying the continuation address on the next VM_RUN. Keep that
        // protocol detail here; the common backend retains its usual advance.
        let next_rip = vcpu.get_arch_reg(reg::RIP)?;
        vcpu.set_arch_reg(reg::RIP, instruction.rip)?;
        *vm.resume_rip.lock() = Some(next_rip);
        return abi::write_exit(destination, code, instruction, payload);
    }
}
