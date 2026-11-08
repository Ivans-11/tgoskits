//! Guest-visible IMSIC MSI pages for static virtual machines.
//!
//! Configure device type `0x31` with one 4 KiB page per vCPU. The optional
//! configuration value is the maximum interrupt identity (63 by default).
//! The guest FDT must describe matching `riscv,imsics` pages and CPU targets.

use alloc::{sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult};
use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange, device::AccessWidth};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use riscv_vcpu::aia::ImsicFile;

/// One MSI page per vCPU, in the order of the configured guest CPU topology.
pub struct Imsic {
    base: usize,
    length: usize,
    files: Vec<Arc<ImsicFile>>,
}

impl Imsic {
    /// Builds software interrupt files with optional hardware backing.
    pub fn new(base: usize, length: usize, ids: usize, cpus: usize) -> AxResult<Self> {
        if cpus == 0
            || base & 0xfff != 0
            || cpus.checked_mul(4096) != Some(length)
            || base.checked_add(length).is_none()
            || !riscv_vcpu::detect_aia()
        {
            return Err(AxError::InvalidInput);
        }
        let files = (0..cpus)
            .map(|_| ImsicFile::new(ids, true).ok_or(AxError::Unsupported))
            .collect::<AxResult<Vec<_>>>()?;
        Ok(Self {
            base,
            length,
            files,
        })
    }

    /// Returns the interrupt state associated with a VM-local vCPU ID.
    pub fn file(&self, vcpu: usize) -> Option<Arc<ImsicFile>> {
        self.files.get(vcpu).cloned()
    }

    fn decode(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<(usize, bool)> {
        let offset = addr
            .as_usize()
            .checked_sub(self.base)
            .ok_or(AxError::InvalidInput)?;
        if !matches!(width, AccessWidth::Dword)
            || offset >= self.length
            || !matches!(offset & 0xfff, 0 | 4)
        {
            return Err(AxError::InvalidInput);
        }
        Ok((offset / 4096, offset & 0xfff == 4))
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for Imsic {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::RiscvImsic
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        (self.base..self.base + self.length).try_into().unwrap()
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        self.decode(addr, width)?;
        Ok(0)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, value: usize) -> AxResult {
        let (cpu, big_endian) = self.decode(addr, width)?;
        let value = value as u32;
        let id = if big_endian {
            value.swap_bytes()
        } else {
            value
        } as usize;
        // Identity zero and unimplemented identities are ignored by IMSIC.
        self.files[cpu].inject(id)?;
        Ok(())
    }
}
