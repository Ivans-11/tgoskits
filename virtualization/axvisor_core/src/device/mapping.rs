// SPDX-License-Identifier: Apache-2.0

//! Authorized BAR windows shared by direct assignment and VFIO.

use ax_errno::{AxError, AxResult};
use axvisor_api::device::{AccessPermissions, DeviceRegion, DirectMapping, RegionKind};

pub(crate) struct BarRegion<'a> {
    pub size: u64,
    pub permissions: AccessPermissions,
    pub mappings: &'a [DirectMapping],
}

impl<'a> BarRegion<'a> {
    pub(super) fn new(region: &'a DeviceRegion) -> AxResult<Self> {
        if region.kind != RegionKind::Mmio {
            return Err(AxError::InvalidInput);
        }
        for (index, mapping) in region.direct_mappings.iter().enumerate() {
            let end = mapping
                .offset
                .checked_add(mapping.length as u64)
                .ok_or(AxError::InvalidInput)?;
            if mapping.length == 0
                || end > region.size
                || (mapping.offset as usize | mapping.length | mapping.physical_address.as_usize())
                    & 4095
                    != 0
                || mapping
                    .physical_address
                    .as_usize()
                    .checked_add(mapping.length)
                    .is_none()
                || (!region.permissions.read && !region.permissions.write)
                || region.direct_mappings[..index]
                    .iter()
                    .any(|old| old.offset < end && mapping.offset < old.offset + old.length as u64)
            {
                return Err(AxError::InvalidInput);
            }
        }
        Ok(Self {
            size: region.size,
            permissions: region.permissions,
            mappings: &region.direct_mappings,
        })
    }

    /// Resolve one page-aligned subrange without crossing an unauthorized gap.
    pub fn slice(
        &self,
        offset: u64,
        length: usize,
        permissions: AccessPermissions,
    ) -> AxResult<DirectMapping> {
        if length == 0 || (offset as usize | length) & 4095 != 0 {
            return Err(AxError::InvalidInput);
        }
        if (permissions.read && !self.permissions.read)
            || (permissions.write && !self.permissions.write)
        {
            return Err(AxError::PermissionDenied);
        }
        let range = self
            .mappings
            .iter()
            .find(|r| {
                offset.checked_sub(r.offset).is_some_and(|delta| {
                    delta <= r.length as u64 && length as u64 <= r.length as u64 - delta
                })
            })
            .ok_or(AxError::PermissionDenied)?;
        Ok(DirectMapping {
            offset,
            physical_address: (range.physical_address.as_usize()
                + (offset - range.offset) as usize)
                .into(),
            length,
        })
    }
}

/// Device pages have the same guest cache and permission policy in either
/// frontend. USER is also required by architectures whose stage-2 uses it.
pub(crate) fn guest_flags(permissions: AccessPermissions) -> axaddrspace::MappingFlags {
    use axaddrspace::MappingFlags;
    let mut flags = MappingFlags::DEVICE | MappingFlags::USER;
    if permissions.read {
        flags |= MappingFlags::READ;
    }
    if permissions.write {
        flags |= MappingFlags::WRITE;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_bar_authorization_rejects_gaps_and_excess_permissions() {
        let mut region = DeviceRegion {
            index: 0,
            kind: RegionKind::Mmio,
            size: 0x4000,
            permissions: AccessPermissions {
                read: true,
                write: false,
            },
            direct_mappings: alloc::vec![DirectMapping {
                offset: 0x1000,
                physical_address: 0x801000usize.into(),
                length: 0x2000
            }],
        };
        let bar = BarRegion::new(&region).unwrap();
        assert_eq!(
            bar.slice(0x2000, 0x1000, region.permissions)
                .unwrap()
                .physical_address
                .as_usize(),
            0x802000
        );
        assert!(matches!(
            bar.slice(0, 0x2000, region.permissions),
            Err(AxError::PermissionDenied)
        ));
        assert!(matches!(
            bar.slice(0x2000, 0x2000, region.permissions),
            Err(AxError::PermissionDenied)
        ));
        assert!(matches!(
            bar.slice(
                0x1000,
                0x1000,
                AccessPermissions {
                    read: true,
                    write: true
                }
            ),
            Err(AxError::PermissionDenied)
        ));
        region.direct_mappings[0].length = 0x4000;
        assert!(matches!(
            BarRegion::new(&region),
            Err(AxError::InvalidInput)
        ));
    }
}
