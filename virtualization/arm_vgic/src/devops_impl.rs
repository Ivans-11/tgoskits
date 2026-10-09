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
use axaddrspace::{
    GuestPhysAddrRange,
    device::{AccessWidth, DeviceAddrRange},
};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};

use crate::vgic::Vgic;

impl BaseDeviceOps<GuestPhysAddrRange> for Vgic {
    /// Gets the emulator type of the current device.
    ///
    /// This function returns the emulator device type of the current instance. Specifically, it always returns `EmuDeviceType::EmuDeviceTGicdV2`,
    /// indicating that the emulator device is of type `EmuDeviceTGicdV2`.
    ///
    /// # Returns
    /// - Returns an instance of the `EmuDeviceType` enum, representing the specific type of the emulator device.
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::InterruptController
    }

    /// Returns the configured MMIO range for the device.
    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base().into(), self.size())
    }

    /// Handles memory read operations.
    ///
    /// Based on the given physical address and read width, performs the corresponding read operation.
    /// Supports reading 1 byte, 2 bytes, and 4 bytes. This function dereferences the provided physical
    /// address and calls the specific read function based on the width parameter.
    ///
    /// Parameters:
    /// - `addr`: The physical address to read from.
    /// - `width`: The width of the data to be read, determining the size of the read operation.
    ///
    /// Returns:
    /// - `AxResult<usize>`: The result of the read operation, including any errors and the size of the data read.
    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as DeviceAddrRange>::Addr,
        width: AccessWidth,
    ) -> AxResult<usize> {
        let addr = addr.as_usize() - self.base();

        // Match different read operations based on the width parameter
        match width {
            AccessWidth::Byte => {
                // Handle 1-byte read
                self.handle_read8(addr)
            }
            AccessWidth::Word => {
                // Handle 2-byte read
                self.handle_read16(addr)
            }
            AccessWidth::Dword => {
                // Handle 4-byte read
                self.handle_read32(addr)
            }
            AccessWidth::Qword => {
                let low = self.handle_read32(addr & !0x7)? as u64;
                let high = self.handle_read32((addr & !0x7) + 4)? as u64;
                Ok((low | (high << 32)) as usize)
            }
        }
    }
    /// Handles write operations of different widths.
    ///
    /// This function performs a write operation based on the given physical address, width, and value.
    /// It converts the guest physical address to a device-relative offset and calls the corresponding
    /// write handler.
    ///
    /// Parameters:
    /// - `addr`: The physical address to write to.
    /// - `width`: The byte width of the data to be written (1, 2, 4 for 8-bit, 16-bit, and 32-bit data respectively).
    /// - `val`: The value to be written.
    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as DeviceAddrRange>::Addr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        let addr = addr.as_usize() - self.base();

        // Depending on the width parameter, perform the corresponding write operation
        match width {
            AccessWidth::Byte => {
                // Handle 8-bit write operation
                self.handle_write8(addr, val);
                Ok(())
            }
            AccessWidth::Word => {
                // Handle 16-bit write operation
                self.handle_write16(addr, val);
                Ok(())
            }
            AccessWidth::Dword => {
                // Handle 32-bit write operation
                self.handle_write32(addr, val);
                Ok(())
            }
            AccessWidth::Qword => {
                self.handle_write32(addr & !0x7, val as u32 as usize);
                self.handle_write32((addr & !0x7) + 4, val >> 32);
                Ok(())
            }
        }
    }
}
