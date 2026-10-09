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

use ax_errno::AxResult;
use axvcpu::AxVCpuExitReason;
use axvm::AxVMRef;

use crate::kvm::util::{access_width_mask, sign_extend_value};

pub(super) fn handle_in_kernel_device_exit(
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    irqchip_created: bool,
    exit_reason: &AxVCpuExitReason,
) -> AxResult<bool> {
    if !irqchip_created {
        return Ok(false);
    }

    match exit_reason {
        AxVCpuExitReason::MmioRead {
            addr,
            width,
            reg,
            reg_width,
            signed_ext,
        } if vm.get_devices().find_mmio_dev(*addr).is_some() => {
            let raw = vm.get_devices().handle_mmio_read(*addr, *width)?;
            let masked = raw & access_width_mask(*width);
            let value = if *signed_ext {
                sign_extend_value(masked, *width)
            } else {
                masked & access_width_mask(*reg_width)
            };
            vcpu.set_gpr(*reg, value);
            Ok(true)
        }
        AxVCpuExitReason::MmioWrite { addr, width, data }
            if vm.get_devices().find_mmio_dev(*addr).is_some() =>
        {
            vm.get_devices()
                .handle_mmio_write(*addr, *width, *data as usize)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
