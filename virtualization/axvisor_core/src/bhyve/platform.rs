// SPDX-License-Identifier: Apache-2.0

//! FreeBSD RTC and interrupt ioctl translation to shared x86 devices.

use ax_errno::{AxError, AxResult};
use axvcpu::AxVCpuExitReason;
use axvisor_api::{control, time};

use super::{Vm, abi};

/// Handles the RTC ports and shared in-kernel platform devices.
pub(super) fn handle_device_exit(
    vm: &Vm,
    vcpu: &axvm::AxVCpuRef,
    exit: &AxVCpuExitReason,
) -> AxResult<bool> {
    match *exit {
        AxVCpuExitReason::IoRead { port, width }
            if (0x70..=0x71).contains(&port.0) && width.size() == 1 =>
        {
            let value = vm
                .rtc
                .lock()
                .read_port(port.0, time::current_time_nanos())?;
            vcpu.set_arch_reg(0, (vcpu.get_arch_reg(0)? & !0xff) | u64::from(value))?;
            Ok(true)
        }
        AxVCpuExitReason::IoWrite { port, width, data }
            if (0x70..=0x71).contains(&port.0) && width.size() == 1 =>
        {
            vm.rtc
                .lock()
                .write_port(port.0, data as u8, time::current_time_nanos())?;
            Ok(true)
        }
        _ => crate::vmm::devices::x86::handle_in_kernel_device_exit(
            &vm.machine,
            vcpu,
            true,
            true,
            exit,
        ),
    }
}

// Device event threads must be able to queue interrupts while VM_RUN holds
// the operation lock. The shared IOAPIC owns routing and level state.
pub(super) fn irq_ioctl(vm: &Vm, command: u32, address: usize) -> AxResult<bool> {
    let (gsi, asserted, pulse) = match command {
        abi::IOAPIC_PINCOUNT => {
            control::copy_to_user(address, &24u32.to_le_bytes())?;
            return Ok(true);
        }
        abi::IOAPIC_ASSERT_IRQ | abi::IOAPIC_DEASSERT_IRQ | abi::IOAPIC_PULSE_IRQ => (
            u32::from_le_bytes(abi::read::<4>(address)?) as usize,
            command != abi::IOAPIC_DEASSERT_IRQ,
            command == abi::IOAPIC_PULSE_IRQ,
        ),
        abi::ISA_ASSERT_IRQ | abi::ISA_DEASSERT_IRQ | abi::ISA_PULSE_IRQ => {
            let data = abi::read::<8>(address)?;
            (
                abi::u32_at(&data, 4) as usize,
                command != abi::ISA_DEASSERT_IRQ,
                command == abi::ISA_PULSE_IRQ,
            )
        }
        _ => return Ok(false),
    };
    if gsi >= 24 {
        return Err(AxError::InvalidInput);
    }
    if let Some(irq) = vm
        .machine
        .get_devices()
        .x86_ioapic_set_gsi_level(gsi, asserted)
    {
        use vm_interrupt::InterruptTriggerMode;

        use crate::vmm::interrupt::{InterruptRoute, VirtualInterrupt, deliver_interrupt};
        deliver_interrupt(InterruptRoute::new(
            vm.machine.id(),
            irq.target_vcpu,
            VirtualInterrupt::with_trigger(
                irq.vector as usize,
                if irq.level_triggered {
                    InterruptTriggerMode::LevelTriggered
                } else {
                    InterruptTriggerMode::EdgeTriggered
                },
            ),
        ));
    }
    if pulse {
        vm.machine
            .get_devices()
            .x86_ioapic_set_gsi_level(gsi, false);
    }
    Ok(true)
}

pub(super) fn ioctl(vm: &Vm, command: u32, address: usize) -> AxResult {
    match command {
        abi::RTC_READ | abi::RTC_WRITE => abi::ioctl::<8>(command, address, |data| {
            let index = abi::u32_at(data, 0) as usize;
            if !(14..128).contains(&index) {
                return Err(AxError::InvalidInput);
            }
            if command == abi::RTC_WRITE {
                vm.rtc
                    .lock()
                    .write(index, data[4], time::current_time_nanos())?;
            } else {
                data[4] = vm.rtc.lock().read(index, time::current_time_nanos())?;
            }
            Ok(())
        }),
        abi::RTC_SETTIME | abi::RTC_GETTIME => abi::ioctl::<8>(command, address, |data| {
            let now = time::current_time_nanos();
            if command == abi::RTC_GETTIME {
                *data = vm.rtc.lock().time(now).to_le_bytes();
                return Ok(());
            }
            let epoch = u64::from_le_bytes(*data);
            if epoch > i64::MAX as u64 {
                return Err(AxError::InvalidInput);
            }
            vm.rtc.lock().set_time(epoch as i64, now)
        }),
        _ => Err(AxError::Unsupported),
    }
}
