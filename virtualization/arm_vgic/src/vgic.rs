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

/// Size of one GICv3 redistributor frame, including its SGI/PPI frame.
pub const GICV3_REDIST_SIZE: usize = 0x20000;

#[derive(Clone, Copy)]
struct GicV3Layout {
    redist_base: usize,
    redist_count: usize,
    dist_base: usize,
    dist_size: usize,
}

/// Virtual Generic Interrupt Controller.
///
/// Manages virtual interrupt distribution for guest VMs.
pub struct Vgic {
    vgicd: Mutex<Vgicd>,
    base: usize,
    size: usize,
    gicv3_layout: Option<Mutex<GicV3Layout>>,
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
            gicv3_layout: None,
        }
    }

    /// Creates a GICv3 MMIO model with a contiguous redistributor region.
    pub fn new_gicv3(base: usize, size: usize, dist_offset: usize) -> Vgic {
        assert_eq!(dist_offset % GICV3_REDIST_SIZE, 0);
        Vgic {
            vgicd: Mutex::new(Vgicd::new()),
            base,
            size,
            gicv3_layout: Some(Mutex::new(GicV3Layout {
                redist_base: base,
                redist_count: dist_offset / GICV3_REDIST_SIZE,
                dist_base: base + dist_offset,
                dist_size: size - dist_offset,
            })),
        }
    }

    /// Updates the guest-visible redistributor region for a host-controlled VM.
    pub fn configure_gicv3(&self, redist_base: usize, redist_count: usize) -> AxResult {
        let Some(layout) = &self.gicv3_layout else {
            return ax_errno::ax_err!(Unsupported);
        };
        let redist_size = redist_count.checked_mul(GICV3_REDIST_SIZE);
        if redist_count == 0
            || redist_size.and_then(|size| redist_base.checked_add(size))
                != Some(layout.lock().dist_base)
        {
            return ax_errno::ax_err!(InvalidInput);
        }
        let mut layout = layout.lock();
        layout.redist_base = redist_base;
        layout.redist_count = redist_count;
        Ok(())
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
        let (addr, gicv3) = if let Some(layout) = &self.gicv3_layout {
            let layout = *layout.lock();
            let dist_offset = layout.redist_count * GICV3_REDIST_SIZE;
            if addr < dist_offset {
                return Ok(self.gicr_read(addr, layout.redist_count));
            }
            (addr - dist_offset, true)
        } else {
            (addr, false)
        };
        if gicv3 && addr == 0xffe8 {
            return Ok(0x30);
        }
        match GicRegister::from_addr(addr as u32) {
            Some(reg) => match reg {
                GicRegister::GicdCtlr => Ok(self.vgicd.lock().ctrlr as usize),
                GicRegister::GicdTyper if gicv3 => Ok(7),
                GicRegister::GicdTyper => Ok(self.vgicd.lock().typer as usize),
                GicRegister::GicdIidr => Ok(self.vgicd.lock().iidr as usize),
                // // GicRegister::GicdStatusr => self.read_statusr(),
                // // GicRegister::GicdIgroupr(idx) => self.read_igroupr(idx),
                GicRegister::GicdIsenabler(idx) => Ok(self.vgicd.lock().vgicd_isenabler_read(idx)),
                GicRegister::GicdIcfgr(idx) => Ok(self.vgicd.lock().vgicd_icfgr_read(idx)),
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
        let addr = if let Some(layout) = &self.gicv3_layout {
            let dist_offset = layout.lock().redist_count * GICV3_REDIST_SIZE;
            if addr < dist_offset {
                return;
            }
            addr - dist_offset
        } else {
            addr
        };
        if let Some(reg) = GicRegister::from_addr(addr as u32) {
            match reg {
                GicRegister::GicdCtlr => self.vgicd.lock().vgicd_ctrlr_write(value),
                // GicRegister::GicdIsenabler(idx) => self.write_isenabler(idx, value),
                GicRegister::GicdIsenabler(idx) => {
                    self.vgicd.lock().vgicd_isenabler_write(idx, value)
                }
                GicRegister::GicdIcfgr(idx) => self.vgicd.lock().vgicd_icfgr_write(idx, value),
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
        self.gicv3_layout
            .as_ref()
            .map(|layout| layout.lock().redist_base)
            .unwrap_or(self.base)
    }

    pub(crate) fn size(&self) -> usize {
        self.gicv3_layout
            .as_ref()
            .map(|layout| {
                let layout = layout.lock();
                layout.redist_count * GICV3_REDIST_SIZE + layout.dist_size
            })
            .unwrap_or(self.size)
    }

    fn gicr_read(&self, addr: usize, redist_count: usize) -> usize {
        let redist = addr / GICV3_REDIST_SIZE;
        if redist >= redist_count {
            return 0;
        }
        match addr % GICV3_REDIST_SIZE {
            0x0004 => 0x43b,
            0x0008 => (redist << 8) | (usize::from(redist + 1 == redist_count) * (1 << 4)),
            0x000c => redist,
            0xffe8 | 0x1ffe8 => 0x30,
            _ => 0,
        }
    }
}
