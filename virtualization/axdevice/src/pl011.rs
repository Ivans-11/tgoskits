// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");

use ax_errno::AxResult;
use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange, device::AccessWidth};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axvisor_api::console;

const UART_DR: usize = 0x000;
const UART_FR: usize = 0x018;
const UART_FR_RXFE: usize = 1 << 4;
const UART_FR_TXFE: usize = 1 << 7;

/// Minimal PL011 console backed by the AxVisor host console.
pub struct EmulatedPl011 {
    base: GuestPhysAddr,
    size: usize,
}

impl EmulatedPl011 {
    /// Creates a PL011 console at the configured guest address.
    pub const fn new(base: GuestPhysAddr, size: usize) -> Self {
        Self { base, size }
    }

    fn identification(offset: usize) -> Option<usize> {
        Some(match offset {
            0xfe0 => 0x11,
            0xfe4 => 0x10,
            0xfe8 => 0x14,
            0xfec => 0x00,
            0xff0 => 0x0d,
            0xff4 => 0xf0,
            0xff8 => 0x05,
            0xffc => 0xb1,
            _ => return None,
        })
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for EmulatedPl011 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn handle_read(&self, addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        let offset = addr.as_usize() - self.base.as_usize();
        Ok(match offset {
            UART_FR => UART_FR_RXFE | UART_FR_TXFE,
            _ => Self::identification(offset).unwrap_or(0),
        })
    }

    fn handle_write(&self, addr: GuestPhysAddr, _width: AccessWidth, val: usize) -> AxResult {
        if addr.as_usize() - self.base.as_usize() == UART_DR {
            console::write_bytes(&[val as u8]);
        }
        Ok(())
    }
}
