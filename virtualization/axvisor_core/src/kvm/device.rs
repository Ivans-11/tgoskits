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

use ax_errno::{AxError, AxResult, ax_err};
use axvisor_api::control as api_control;
use axvm::config::{HOST_CONTROLLED_GIC_DIST_BASE, HOST_CONTROLLED_GIC_REDIST_BASE};
use kvm_uapi::arm64::*;

use super::{
    CONTROL_FILES, ControlFileState, DeviceFileState, KVM_CONTROL_OPS, KVM_HAS_DEVICE_ATTR,
    KVM_SET_DEVICE_ATTR, abi::raw as abi, close, next_control_file_id,
};

pub(in crate::kvm) fn create_device(
    vm_file: api_control::ControlFileId,
    arg: usize,
) -> AxResult<isize> {
    let mut bytes = [0u8; abi::KVM_CREATE_DEVICE_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;
    let type_ = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    let flags = u32::from_ne_bytes(bytes[8..12].try_into().unwrap());
    if type_ != KVM_DEV_TYPE_ARM_VGIC_V3 || flags & !KVM_CREATE_DEVICE_TEST != 0 {
        return ax_err!(Unsupported);
    }
    if flags == KVM_CREATE_DEVICE_TEST {
        return Ok(0);
    }

    let device_file = next_control_file_id()?;
    {
        let mut control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&vm_file) else {
            return ax_err!(NotFound);
        };
        if vm.device_file.is_some() {
            return ax_err!(AlreadyExists);
        }
        vm.device_file = Some(device_file);
        control_files.insert(
            device_file,
            ControlFileState::Device(DeviceFileState {
                vm_file,
                dist_configured: false,
                redist_configured: false,
            }),
        );
    }

    let fd = match api_control::create_user_fd(device_file, KVM_CONTROL_OPS, None) {
        Ok(fd) => fd,
        Err(err) => {
            let _ = close(device_file);
            return Err(err);
        }
    };
    if let Err(err) = api_control::copy_to_user(arg + 4, &(fd as u32).to_ne_bytes()) {
        let _ = close(device_file);
        return Err(err);
    }
    Ok(0)
}

pub(in crate::kvm) fn device_ioctl(
    control_file: api_control::ControlFileId,
    cmd: u32,
    arg: usize,
) -> AxResult<isize> {
    let attr = read_device_attr(arg)?;
    match cmd {
        KVM_HAS_DEVICE_ATTR => {
            validate_attr(&attr)?;
            Ok(0)
        }
        KVM_SET_DEVICE_ATTR => {
            validate_attr(&attr)?;
            set_device_attr(control_file, attr)?;
            Ok(0)
        }
        _ => Err(AxError::Unsupported),
    }
}

fn validate_attr(attr: &KvmDeviceAttr) -> AxResult {
    if attr.flags != 0 {
        return ax_err!(InvalidInput);
    }
    match (attr.group, attr.attr) {
        (KVM_DEV_ARM_VGIC_GRP_ADDR, KVM_VGIC_V3_ADDR_TYPE_DIST)
        | (KVM_DEV_ARM_VGIC_GRP_ADDR, KVM_VGIC_V3_ADDR_TYPE_REDIST)
        | (KVM_DEV_ARM_VGIC_GRP_NR_IRQS, 0)
        | (KVM_DEV_ARM_VGIC_GRP_CTRL, KVM_DEV_ARM_VGIC_CTRL_INIT) => Ok(()),
        _ => Err(AxError::Unsupported),
    }
}

fn set_device_attr(control_file: api_control::ControlFileId, attr: KvmDeviceAttr) -> AxResult {
    let value = if attr.group == KVM_DEV_ARM_VGIC_GRP_CTRL {
        0
    } else if attr.group == KVM_DEV_ARM_VGIC_GRP_NR_IRQS {
        u64::from(read_u32_user(attr.addr as usize)?)
    } else {
        read_u64_user(attr.addr as usize)?
    };

    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Device(device)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    match (attr.group, attr.attr) {
        (KVM_DEV_ARM_VGIC_GRP_ADDR, KVM_VGIC_V3_ADDR_TYPE_DIST)
            if value == HOST_CONTROLLED_GIC_DIST_BASE as u64 =>
        {
            device.dist_configured = true;
        }
        (KVM_DEV_ARM_VGIC_GRP_ADDR, KVM_VGIC_V3_ADDR_TYPE_REDIST)
            if value == HOST_CONTROLLED_GIC_REDIST_BASE as u64 =>
        {
            device.redist_configured = true;
        }
        (KVM_DEV_ARM_VGIC_GRP_ADDR, _) => return ax_err!(InvalidInput),
        (KVM_DEV_ARM_VGIC_GRP_NR_IRQS, 0) => {
            let nr_irqs = u32::try_from(value).map_err(|_| AxError::InvalidInput)?;
            if !(64..=256).contains(&nr_irqs) || nr_irqs % 32 != 0 {
                return ax_err!(InvalidInput);
            }
        }
        (KVM_DEV_ARM_VGIC_GRP_CTRL, KVM_DEV_ARM_VGIC_CTRL_INIT) => {
            if !device.dist_configured || !device.redist_configured {
                return ax_err!(BadState);
            }
            let vm_file = device.vm_file;
            let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&vm_file) else {
                return ax_err!(NotFound);
            };
            vm.irqchip_created = true;
        }
        _ => return ax_err!(Unsupported),
    }
    Ok(())
}

fn read_device_attr(arg: usize) -> AxResult<KvmDeviceAttr> {
    let mut bytes = [0u8; abi::KVM_DEVICE_ATTR_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;
    Ok(KvmDeviceAttr {
        flags: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
        group: u32::from_ne_bytes(bytes[4..8].try_into().unwrap()),
        attr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
        addr: u64::from_ne_bytes(bytes[16..24].try_into().unwrap()),
    })
}

fn read_u32_user(arg: usize) -> AxResult<u32> {
    let mut bytes = [0; 4];
    api_control::copy_from_user(arg, &mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

fn read_u64_user(arg: usize) -> AxResult<u64> {
    let mut bytes = [0; 8];
    api_control::copy_from_user(arg, &mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}
