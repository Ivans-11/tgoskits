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
use ax_kspin::SpinNoIrq as Mutex;

use crate::{interrupt::VgicInt, registers::GicRegister, vgicd::Vgicd};

/// Virtual Generic Interrupt Controller.
///
/// Manages virtual interrupt distribution for guest VMs.
pub struct Vgic {
    vgicd: Mutex<Vgicd>,
    base: usize,
    size: usize,
    dist_offset: usize,
    gicv3: bool,
}

impl Default for Vgic {
    fn default() -> Self {
        Self::new()
    }
}

impl Vgic {
    /// Creates a new VGIC instance.
    pub fn new() -> Vgic {
        Vgic {
            vgicd: Mutex::new(Vgicd::new()),
            base: 0x800_0000,
            size: 0x10000,
            dist_offset: 0,
            gicv3: false,
        }
    }

    /// Creates a single-redistributor GICv3 MMIO model.
    pub fn new_gicv3(base: usize, size: usize, dist_offset: usize) -> Vgic {
        Vgic {
            vgicd: Mutex::new(Vgicd::new()),
            base,
            size,
            dist_offset,
            gicv3: true,
        }
    }
    pub(crate) fn handle_read8(&self, addr: usize) -> AxResult<usize> {
        let value = self.handle_read32(addr)?;
        Ok((value >> (8 * (addr & 0x3))) & 0xff)
    }

    pub(crate) fn handle_read16(&self, addr: usize) -> AxResult<usize> {
        let value = self.handle_read32(addr)?;
        Ok((value >> (8 * (addr & 0x3))) & 0xffff)
    }

    /// Handles 32-bit read access to VGIC registers.
    pub fn handle_read32(&self, addr: usize) -> AxResult<usize> {
        if self.gicv3 && addr < self.dist_offset {
            return Ok(self.gicr_read(addr));
        }
        let addr = addr - self.dist_offset;
        if self.gicv3 && addr == 0xffe8 {
            return Ok(0x30);
        }
        match GicRegister::from_addr(addr as u32) {
            Some(reg) => match reg {
                GicRegister::GicdCtlr => Ok(self.vgicd.lock().ctrlr as usize),
                GicRegister::GicdTyper if self.gicv3 => Ok(7),
                GicRegister::GicdTyper => Ok(self.vgicd.lock().typer as usize),
                GicRegister::GicdIidr => Ok(self.vgicd.lock().iidr as usize),
                // // GicRegister::GicdStatusr => self.read_statusr(),
                // // GicRegister::GicdIgroupr(idx) => self.read_igroupr(idx),
                GicRegister::GicdIsenabler(idx) => Ok(self.vgicd.lock().vgicd_isenabler_read(idx)),
                // GicRegister::GicdIcenabler(idx) => self.read_icenabler(idx),
                // GicRegister::GicdIspendr(idx) => self.read_ispendr(idx),
                _ => {
                    // error!("Read register address: {:#x}", addr);
                    Ok(0)
                }
            },
            None => {
                // error!("Invalid read register address: {addr:#x}");
                Ok(0)
            }
        }
    }

    /// Handles 8-bit write access to VGIC registers.
    pub fn handle_write8(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    /// Handles 16-bit write access to VGIC registers.
    pub fn handle_write16(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    /// Handles 32-bit write access to VGIC registers.
    pub fn handle_write32(&self, addr: usize, value: usize) {
        if self.gicv3 && addr < self.dist_offset {
            return;
        }
        let addr = addr - self.dist_offset;
        if let Some(reg) = GicRegister::from_addr(addr as u32) {
            match reg {
                GicRegister::GicdCtlr => self.vgicd.lock().vgicd_ctrlr_write(value),
                // GicRegister::GicdIsenabler(idx) => self.write_isenabler(idx, value),
                GicRegister::GicdIsenabler(idx) => {
                    self.vgicd.lock().vgicd_isenabler_write(idx, value)
                }
                _ => {
                    // error!("Write register address: {:#x}", addr);
                }
            }
        }
    }

    // Removed, interrupt injection in arm_vcpu
    // pub fn inject_irq(&self, irq: u32) {
    //     self.vgicd.lock().inject_irq(irq);
    // }

    /// Fetches interrupt information for the given IRQ number.
    pub fn fetch_irq(&self, irq: u32) -> VgicInt {
        self.vgicd.lock().fetch_irq(irq)
    }

    /// Placeholder method for unused operations.
    pub fn nothing(&self, _value: u32) {}

    pub(crate) fn base(&self) -> usize {
        self.base
    }

    pub(crate) fn size(&self) -> usize {
        self.size
    }

    fn gicr_read(&self, addr: usize) -> usize {
        match addr {
            0x0004 => 0x43b,
            // One redistributor, affinity 0, and the Last bit set.
            0x0008 => 1 << 4,
            0xffe8 | 0x1ffe8 => 0x30,
            _ => 0,
        }
    }
}
