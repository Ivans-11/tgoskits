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

const PSCI_0_2_FN_BASE: u64 = 0x8400_0000;
const PSCI_0_2_FN64_BASE: u64 = 0xc400_0000;
const PSCI_FN_RANGE_SIZE: u64 = 0x20;
const PSCI_0_2_FN_PSCI_VERSION: u64 = PSCI_0_2_FN_BASE;
const PSCI_0_2_FN_MIGRATE_INFO_TYPE: u64 = PSCI_0_2_FN_BASE + 6;
const PSCI_VERSION_0_2: usize = 2;
const PSCI_0_2_TOS_MP: usize = 2;
const PSCI_RET_NOT_SUPPORTED: usize = usize::MAX;

pub(super) fn handle_psci_hypercall(vcpu: &axvm::AxVCpuRef, function: u64) -> bool {
    let is_psci = (PSCI_0_2_FN_BASE..PSCI_0_2_FN_BASE + PSCI_FN_RANGE_SIZE).contains(&function)
        || (PSCI_0_2_FN64_BASE..PSCI_0_2_FN64_BASE + PSCI_FN_RANGE_SIZE).contains(&function);
    if !is_psci {
        return false;
    }

    let result = match function {
        PSCI_0_2_FN_PSCI_VERSION => PSCI_VERSION_0_2,
        PSCI_0_2_FN_MIGRATE_INFO_TYPE => PSCI_0_2_TOS_MP,
        _ => PSCI_RET_NOT_SUPPORTED,
    };
    vcpu.set_gpr(0, result);
    true
}

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
