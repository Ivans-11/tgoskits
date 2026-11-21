// SPDX-License-Identifier: Apache-2.0

use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use axdevice::pci::{MsiMessage, PciBackend, PciFunction, PciIdentity};
use axvisor_api::device::{AccessWidth, DmaSpace, InterruptKind, RegionKind};

use super::{
    Device,
    abi::{CONFIG_REGION, REGION_SHIFT},
};
use crate::device::DeviceAssignment;

// QEMU routes MSI through VFIO eventfds. The config model is used only for
// filtered config bytes/BAR probing, never to send guest MSI itself.
struct ConfigBackend(Arc<super::irq::Notification>);
impl PciBackend for ConfigBackend {
    fn read_bar(&self, _: u32, _: u64, _: AccessWidth) -> AxResult<u64> {
        Err(AxError::Unsupported)
    }
    fn write_bar(&self, _: u32, _: u64, _: AccessWidth, _: u64) -> AxResult {
        Err(AxError::Unsupported)
    }
    fn send_msi(&self, _: MsiMessage) -> bool {
        self.0.signal();
        true
    }
}

pub(super) fn new(
    assignment: &DeviceAssignment,
) -> AxResult<(Arc<PciFunction>, u32, u32, Arc<super::irq::Notification>)> {
    let header = assignment
        .info()
        .regions
        .iter()
        .find(|region| region.kind == RegionKind::Configuration && region.size >= 64)
        .ok_or(AxError::Unsupported)?
        .index;
    let source = assignment
        .info()
        .interrupts
        .iter()
        .find(|irq| irq.kind == InterruptKind::Edge)
        .ok_or(AxError::Unsupported)?
        .index;
    let read = |offset| {
        assignment
            .read(header, offset, AccessWidth::Dword)
            .map(|value| value as u32)
    };
    let notification = super::irq::Notification::new();
    let pci = Arc::new(PciFunction::new(
        PciIdentity {
            vendor_device: read(0)?,
            class_revision: read(8)?,
            subsystem: read(0x2c)?,
        },
        Arc::new(ConfigBackend(notification.clone())),
    ));
    for region in &assignment.info().regions {
        if region.kind == RegionKind::Configuration || region.size == 0 {
            continue;
        }
        if region.index >= 6 || region.kind != RegionKind::Mmio {
            return Err(AxError::Unsupported);
        }
        let range = (0..usize::try_from(region.size).map_err(|_| AxError::InvalidInput)?)
            .try_into()
            .map_err(|_| AxError::InvalidInput)?;
        pci.add_bar(
            region.index,
            range,
            read(0x10 + u64::from(region.index) * 4)? & 0xf,
        )?;
    }
    Ok((pci, header, source, notification))
}

fn region(device: &Device, offset: u64, len: usize, write: bool) -> AxResult<(u32, u64, usize)> {
    let index = u32::try_from(offset >> REGION_SHIFT).map_err(|_| AxError::InvalidInput)?;
    let offset = offset & ((1 << REGION_SHIFT) - 1);
    let size = if index == CONFIG_REGION {
        256
    } else {
        let region = device
            .assignment
            .info()
            .regions
            .iter()
            .find(|region| region.index == index && region.kind != RegionKind::Configuration)
            .ok_or(AxError::InvalidInput)?;
        if (write && !region.permissions.write) || (!write && !region.permissions.read) {
            return Err(AxError::PermissionDenied);
        }
        region.size
    };
    if offset > size {
        return Err(AxError::InvalidInput);
    }
    Ok((index, offset, len.min((size - offset) as usize)))
}

fn width(offset: u64, len: usize, mmio: bool) -> AccessWidth {
    if mmio && offset & 7 == 0 && len >= 8 {
        AccessWidth::Qword
    } else if offset & 3 == 0 && len >= 4 {
        AccessWidth::Dword
    } else if offset & 1 == 0 && len >= 2 {
        AccessWidth::Word
    } else {
        AccessWidth::Byte
    }
}

pub(super) fn read(device: &Device, offset: u64, buffer: &mut [u8]) -> AxResult<usize> {
    let (index, offset, len) = region(device, offset, buffer.len(), false)?;
    let mut done = 0;
    while done < len {
        let at = offset + done as u64;
        let width = width(at, len - done, index != CONFIG_REGION);
        let value = if index == CONFIG_REGION {
            device.pci.config_read(at as usize, width)?
        } else {
            device.assignment.read(index, at, width)?
        };
        buffer[done..done + width.size()].copy_from_slice(&value.to_le_bytes()[..width.size()]);
        done += width.size();
    }
    Ok(done)
}

pub(super) fn write(
    device: &mut Device,
    space: DmaSpace,
    offset: u64,
    buffer: &[u8],
) -> AxResult<usize> {
    let (index, offset, len) = region(device, offset, buffer.len(), true)?;
    let mut done = 0;
    while done < len {
        let at = offset + done as u64;
        let width = width(at, len - done, index != CONFIG_REGION);
        let mut bytes = [0; 8];
        bytes[..width.size()].copy_from_slice(&buffer[done..done + width.size()]);
        let value = u64::from_le_bytes(bytes);
        if index == CONFIG_REGION {
            if at <= 4 && at + width.size() as u64 > 4 {
                let command = ((value >> ((4 - at) * 8)) & 7) as u16;
                if command & 4 != 0 && !device.assignment.is_attached() {
                    device.assignment.attach_to(space)?;
                }
                device.assignment.write(
                    device.config_region,
                    4,
                    AccessWidth::Word,
                    u64::from(command),
                )?;
            }
            device.pci.config_write(at as usize, width, value)?;
        } else {
            device.assignment.write(index, at, width, value)?;
        }
        done += width.size();
    }
    Ok(done)
}
