// SPDX-License-Identifier: Apache-2.0

//! Linux legacy VFIO UAPI used by QEMU's PCI assignment backend.

use ax_errno::{AxError, AxResult};
use axvisor_api::control;

pub const GET_API_VERSION: u32 = 0x3b64;
pub const CHECK_EXTENSION: u32 = 0x3b65;
pub const SET_IOMMU: u32 = 0x3b66;
pub const GROUP_GET_STATUS: u32 = 0x3b67;
pub const GROUP_SET_CONTAINER: u32 = 0x3b68;
pub const GROUP_UNSET_CONTAINER: u32 = 0x3b69;
pub const GROUP_GET_DEVICE_FD: u32 = 0x3b6a;
pub const DEVICE_GET_INFO: u32 = 0x3b6b;
pub const DEVICE_GET_REGION_INFO: u32 = 0x3b6c;
pub const DEVICE_GET_IRQ_INFO: u32 = 0x3b6d;
pub const DEVICE_SET_IRQS: u32 = 0x3b6e;
pub const IOMMU_GET_INFO: u32 = 0x3b70;
pub const IOMMU_MAP_DMA: u32 = 0x3b71;
pub const IOMMU_UNMAP_DMA: u32 = 0x3b72;
pub const CONFIG_REGION: u32 = 7;
pub const REGION_SHIFT: u32 = 40;

pub fn read<const N: usize>(address: usize) -> AxResult<[u8; N]> {
    let mut bytes = [0; N];
    control::copy_from_user(address, &mut bytes)?;
    Ok(bytes)
}

pub fn payload<const N: usize>(address: usize) -> AxResult<[u8; N]> {
    let size = u32::from_le_bytes(read::<4>(address)?) as usize;
    if size < N {
        return Err(AxError::InvalidInput);
    }
    read(address)
}

pub fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

pub fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

pub fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub fn name(address: usize) -> AxResult<alloc::string::String> {
    let mut bytes = alloc::vec::Vec::new();
    for offset in 0..256 {
        let byte = read::<1>(address.checked_add(offset).ok_or(AxError::InvalidInput)?)?[0];
        if byte == 0 {
            return alloc::string::String::from_utf8(bytes).map_err(|_| AxError::InvalidInput);
        }
        bytes.push(byte);
    }
    Err(AxError::InvalidInput)
}
