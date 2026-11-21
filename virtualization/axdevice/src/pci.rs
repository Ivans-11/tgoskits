// SPDX-License-Identifier: Apache-2.0

//! PCI configuration mechanism 1 and fixed-aperture endpoint models.
//!
//! The backend supplies BAR accesses and guest MSI delivery. This module owns
//! guest-visible configuration semantics, independently of host assignment APIs.

use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axaddrspace::{
    GuestPhysAddr, GuestPhysAddrRange,
    device::{AccessWidth, Port, PortRange},
};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axvisor_api::sync::WaitQueue;

/// Identity exposed by a virtual PCI endpoint.
pub struct PciIdentity {
    /// Vendor ID in the low half and device ID in the high half.
    pub vendor_device: u32,
    /// Revision, programming interface, subclass and class.
    pub class_revision: u32,
    /// Subsystem vendor ID and subsystem ID.
    pub subsystem: u32,
}

/// Guest-programmed MSI message; its address never denotes host memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsiMessage {
    /// Guest MSI destination address.
    pub address: u64,
    /// Guest MSI payload.
    pub data: u32,
}

/// Resource access and interrupt delivery supplied by the owning frontend.
pub trait PciBackend: Send + Sync {
    /// Read a BAR at a checked offset.
    fn read_bar(&self, index: u32, offset: u64, width: AccessWidth) -> AxResult<u64>;
    /// Write a BAR at a checked offset.
    fn write_bar(&self, index: u32, offset: u64, width: AccessWidth, value: u64) -> AxResult;
    /// Deliver a guest MSI. False keeps the interrupt pending for a later
    /// configuration update. Called without the configuration lock held.
    fn send_msi(&self, message: MsiMessage) -> bool;
    /// Update an accelerated MSI route, or restore software delivery for
    /// disabled/masked MSI. Runs in sleepable context, serialized with other
    /// configuration writes and without the state lock. Failure leaves the
    /// previous route intact and rejects the configuration write.
    fn configure_msi(&self, _message: Option<MsiMessage>) -> AxResult {
        Ok(())
    }
}

const MSI: usize = 0x40;
const PCIE: usize = 0x60;

struct Bar {
    aperture: GuestPhysAddrRange,
    size: u64,
    flags: u32,
    address: u64,
    probe: [bool; 2],
}

struct Config {
    bytes: [u8; 256],
    bars: BTreeMap<u32, Bar>,
    pending: bool,
}

fn get32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

impl Config {
    fn memory_enabled(&self) -> bool {
        self.bytes[4] & 2 != 0
    }

    fn msi(&self) -> Option<MsiMessage> {
        if self.bytes[MSI + 2] & 1 == 0 || get32(&self.bytes, MSI + 16) & 1 != 0 {
            return None;
        }
        Some(MsiMessage {
            address: u64::from(get32(&self.bytes, MSI + 4))
                | (u64::from(get32(&self.bytes, MSI + 8)) << 32),
            data: u16::from_le_bytes(self.bytes[MSI + 12..MSI + 14].try_into().unwrap()).into(),
        })
    }

    fn read_byte(&self, offset: usize) -> u8 {
        if (0x10..0x28).contains(&offset) {
            let index = ((offset - 0x10) / 4) as u32;
            let (bar, high) = if let Some(bar) = self.bars.get(&index) {
                (bar, false)
            } else if let Some(bar) = index
                .checked_sub(1)
                .and_then(|i| self.bars.get(&i))
                .filter(|b| b.flags & 4 != 0)
            {
                (bar, true)
            } else {
                return 0;
            };
            let value = if bar.probe[usize::from(high)] {
                !(bar.size - 1)
            } else {
                bar.address
            };
            let word = if high {
                (value >> 32) as u32
            } else {
                value as u32 | bar.flags
            };
            return (word >> ((offset & 3) * 8)) as u8;
        }
        if offset == MSI + 20 {
            return u8::from(self.pending);
        }
        self.bytes[offset]
    }

    fn write(&mut self, offset: usize, width: usize, value: u64) {
        if (0x10..0x28).contains(&offset) {
            // BARs are dword registers. Byte/word writes cannot create an
            // out-of-aperture mapping; unsupported widths are ignored.
            if width != 4 || offset & 3 != 0 {
                return;
            }
            let index = ((offset - 0x10) / 4) as u32;
            let high = !self.bars.contains_key(&index);
            let key = if high { index.wrapping_sub(1) } else { index };
            let Some(bar) = self.bars.get_mut(&key) else {
                return;
            };
            if high && bar.flags & 4 == 0 {
                return;
            }
            bar.probe[usize::from(high)] = value as u32 == u32::MAX;
            if !bar.probe[usize::from(high)] {
                if high {
                    bar.address =
                        (bar.address & u64::from(u32::MAX)) | ((value as u32 as u64) << 32);
                } else {
                    bar.address =
                        (bar.address & !u64::from(u32::MAX)) | (value as u32 & !0xf) as u64;
                }
                bar.address &= !(bar.size - 1);
            }
            return;
        }
        for byte in 0..width {
            let i = offset + byte;
            let value = (value >> (byte * 8)) as u8;
            match i {
                4 => self.bytes[i] = value & 6, // memory decoding and bus master
                5 => self.bytes[i] = value & 4, // INTx disable (INTx is not advertised)
                i if i == MSI + 2 => self.bytes[i] = (self.bytes[i] & !1) | (value & 1),
                i if (MSI + 4..MSI + 14).contains(&i) => self.bytes[i] = value,
                i if i == MSI + 16 => self.bytes[i] = value & 1,
                // Immutable IDs, capabilities, pending bit and reserved fields.
                _ => (),
            }
        }
    }
}

/// A PCIe endpoint with fixed MMIO apertures and one maskable MSI vector.
pub struct PciFunction {
    state: SpinNoIrq<Config>,
    backend: Arc<dyn PciBackend>,
    configuration_busy: Arc<AtomicBool>,
    configuration_wait: WaitQueue,
}

struct ConfigurationGuard<'a>(&'a PciFunction);

impl Drop for ConfigurationGuard<'_> {
    fn drop(&mut self) {
        self.0.configuration_busy.store(false, Ordering::Release);
        self.0.configuration_wait.wake_all();
    }
}

impl PciFunction {
    /// Build a configuration header from identity only. Physical control
    /// registers, capabilities and routing are not copied into the guest.
    pub fn new(identity: PciIdentity, backend: Arc<dyn PciBackend>) -> Self {
        let mut bytes = [0; 256];
        for (offset, value) in [
            (0, identity.vendor_device),
            (8, identity.class_revision),
            (0x2c, identity.subsystem),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[6] = 0x10; // capabilities list
        bytes[0x34] = MSI as u8;
        bytes[MSI] = 5;
        bytes[MSI + 1] = PCIE as u8;
        bytes[MSI + 2] = 0x80; // 64-bit MSI, one vector
        bytes[MSI + 3] = 1; // per-vector mask and pending bits
        bytes[MSI + 16] = 1;
        bytes[PCIE] = 0x10;
        bytes[PCIE + 2] = 0x92; // v2 root-complex integrated endpoint
        // No FLR, ATS, PCI PM or extended capabilities are advertised. Those
        // physical facilities belong to the host assignment lifecycle.
        Self {
            state: SpinNoIrq::new(Config {
                bytes,
                bars: BTreeMap::new(),
                pending: false,
            }),
            backend,
            configuration_busy: Arc::new(AtomicBool::new(false)),
            configuration_wait: WaitQueue::new(),
        }
    }

    /// Add a full BAR aperture and return its MMIO device. The frontend must
    /// authorize this range and supply the physical BAR's low four flag bits.
    pub fn add_bar(
        self: &Arc<Self>,
        index: u32,
        range: GuestPhysAddrRange,
        flags: u32,
    ) -> AxResult<PciWindow> {
        let size = range.size() as u64;
        if index >= 6
            || !size.is_power_of_two()
            || size < 16
            || range.start.as_usize() as u64 & (size - 1) != 0
            || flags & !0xf != 0
        {
            return Err(AxError::InvalidInput);
        }
        if flags & 1 != 0
            || !matches!(flags & 6, 0 | 4)
            || (flags & 4 != 0 && index == 5)
            || (flags & 4 == 0 && range.end.as_usize() as u64 > 1u64 << 32)
        {
            return Err(AxError::Unsupported);
        }
        let mut config = self.state.lock();
        if config.bars.contains_key(&index)
            || (flags & 4 != 0 && config.bars.contains_key(&(index + 1)))
            || index
                .checked_sub(1)
                .and_then(|i| config.bars.get(&i))
                .is_some_and(|b| b.flags & 4 != 0)
        {
            return Err(AxError::InvalidInput);
        }
        config.bars.insert(
            index,
            Bar {
                aperture: range,
                size,
                flags,
                address: range.start.as_usize() as u64,
                probe: [false; 2],
            },
        );
        Ok(PciWindow {
            range,
            region: index,
            function: self.clone(),
        })
    }

    fn read_config(&self, offset: usize, width: usize) -> usize {
        let config = self.state.lock();
        (0..width).fold(0, |value, i| {
            value | ((config.read_byte(offset + i) as usize) << (i * 8))
        })
    }

    fn write_config(&self, offset: usize, width: usize, value: usize) -> AxResult {
        // Host remapping may sleep. Serialize writers without retaining the
        // spinlock acquired by physical interrupt callbacks.
        while self
            .configuration_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            let busy = self.configuration_busy.clone();
            self.configuration_wait
                .wait_until(move || !busy.load(Ordering::Acquire));
        }
        let _guard = ConfigurationGuard(self);
        let (old_msi, new_msi, bytes) = {
            let mut config = self.state.lock();
            let previous = config.bytes;
            let old_msi = config.msi();
            config.write(offset, width, value as u64);
            let next = config.bytes;
            let new_msi = config.msi();
            config.bytes = previous;
            (old_msi, new_msi, next)
        };
        if old_msi != new_msi {
            self.backend.configure_msi(new_msi)?;
        }
        self.state.lock().bytes = bytes;
        self.deliver_pending();
        Ok(())
    }

    /// Signal an interrupt, retaining it while MSI is disabled or masked.
    pub fn interrupt(&self) {
        self.state.lock().pending = true;
        self.deliver_pending();
    }

    fn deliver_pending(&self) {
        let message = {
            let mut config = self.state.lock();
            if !config.pending {
                return;
            }
            let Some(message) = config.msi() else {
                return;
            };
            config.pending = false;
            message
        };
        if !self.backend.send_msi(message) {
            self.state.lock().pending = true;
        }
    }
}

/// A BAR window created by `PciFunction::add_bar`.
pub struct PciWindow {
    range: GuestPhysAddrRange,
    region: u32,
    function: Arc<PciFunction>,
}

impl PciWindow {
    fn offset(&self, addr: GuestPhysAddr, width: AccessWidth) -> Option<u64> {
        let config = self.function.state.lock();
        let bar = config.bars.get(&self.region)?;
        let offset = (addr.as_usize() as u64).checked_sub(bar.address)?;
        (config.memory_enabled()
            && bar.address == bar.aperture.start.as_usize() as u64
            && offset.checked_add(width.size() as u64)? <= bar.size)
            .then_some(offset)
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for PciWindow {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }
    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        match self.offset(addr, width) {
            Some(offset) => self
                .function
                .backend
                .read_bar(self.region, offset, width)
                .map(|v| v as usize),
            None => Ok(usize::MAX >> ((8 - width.size()) * 8)),
        }
    }
    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, value: usize) -> AxResult {
        if let Some(offset) = self.offset(addr, width) {
            self.function
                .backend
                .write_bar(self.region, offset, width, value as u64)?;
        }
        Ok(())
    }
}

/// PCI configuration mechanism 1. Guest BDFs are independent of host BDFs.
pub struct PciBus {
    address: SpinNoIrq<u32>,
    functions: BTreeMap<u16, Arc<PciFunction>>,
}

impl PciBus {
    /// Build bus 0 with single-function endpoints at slots 1 through 31.
    /// Keys encode bus, device and function as in a PCI requester ID.
    pub fn new(functions: BTreeMap<u16, Arc<PciFunction>>) -> AxResult<Self> {
        if functions
            .keys()
            .any(|&bdf| bdf == 0 || bdf > 31 << 3 || bdf & 7 != 0)
        {
            return Err(AxError::InvalidInput);
        }
        Ok(Self {
            address: SpinNoIrq::new(0),
            functions,
        })
    }

    fn data_address(&self, port: Port, width: AccessWidth) -> Option<(u16, usize)> {
        let address = *self.address.lock();
        if address & 0x8000_0000 == 0
            || port.0 < 0xcfc
            || usize::from(port.0) + width.size() > 0xd00
            || usize::from(port.0) % width.size() != 0
        {
            return None;
        }
        Some((
            (address >> 8) as u16,
            (address & 0xfc) as usize + usize::from(port.0 - 0xcfc),
        ))
    }
}

impl BaseDeviceOps<PortRange> for PciBus {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }
    fn address_range(&self) -> PortRange {
        PortRange::new(Port(0xcf8), Port(0xcff))
    }
    fn handle_read(&self, addr: Port, width: AccessWidth) -> AxResult<usize> {
        if addr.0 == 0xcf8 && width.size() == 4 {
            return Ok(*self.address.lock() as usize);
        }
        if let Some((bdf, offset)) = self.data_address(addr, width) {
            if let Some(function) = self.functions.get(&bdf) {
                return Ok(function.read_config(offset, width.size()));
            }
            if bdf == 0 {
                // A synthetic host bridge lets conventional PCI probing find bus 0.
                let mut header = [0u8; 256];
                header[..4].copy_from_slice(&0x1237_8086u32.to_le_bytes());
                header[0x0b] = 6;
                return Ok((0..width.size())
                    .fold(0, |v, i| v | ((header[offset + i] as usize) << (8 * i))));
            }
        }
        Ok(usize::MAX >> ((8 - width.size()) * 8))
    }
    fn handle_write(&self, addr: Port, width: AccessWidth, value: usize) -> AxResult {
        if addr.0 == 0xcf8 && width.size() == 4 {
            *self.address.lock() = value as u32 & !3;
        } else if let Some((bdf, offset)) = self.data_address(addr, width)
            && let Some(function) = self.functions.get(&bdf)
        {
            function.write_config(offset, width.size(), value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use alloc::{boxed::Box, vec::Vec};

    use super::*;

    struct TestSync;
    #[axvisor_api::api_impl]
    impl axvisor_api::sync::SyncIf for TestSync {
        fn create_wait_queue() -> usize {
            0
        }
        fn destroy_wait_queue(_queue: usize) {}
        fn wait_queue_wait(_queue: usize) {
            panic!("unexpected unconditional wait")
        }
        fn wait_queue_wait_until(_queue: usize, condition: Box<dyn Fn() -> bool + Send + 'static>) {
            while !condition() {
                std::thread::yield_now();
            }
        }
        fn wait_queue_wake_one(_queue: usize) {}
        fn wait_queue_wake_all(_queue: usize) {}
    }

    #[derive(Default)]
    struct Backend {
        reject: AtomicBool,
        updating: AtomicBool,
        routes: std::sync::Mutex<Vec<Option<MsiMessage>>>,
        delivered: std::sync::Mutex<Vec<MsiMessage>>,
    }

    impl PciBackend for Backend {
        fn read_bar(&self, _: u32, _: u64, _: AccessWidth) -> AxResult<u64> {
            Ok(0)
        }
        fn write_bar(&self, _: u32, _: u64, _: AccessWidth, _: u64) -> AxResult {
            Ok(())
        }
        fn send_msi(&self, message: MsiMessage) -> bool {
            self.delivered.lock().unwrap().push(message);
            true
        }
        fn configure_msi(&self, message: Option<MsiMessage>) -> AxResult {
            assert!(!self.updating.swap(true, Ordering::SeqCst));
            std::thread::yield_now();
            let result = if self.reject.load(Ordering::Relaxed) {
                Err(AxError::Io)
            } else {
                self.routes.lock().unwrap().push(message);
                Ok(())
            };
            self.updating.store(false, Ordering::SeqCst);
            result
        }
    }

    #[test]
    fn route_updates_serialize_and_failed_remapping_preserves_guest_config() {
        let backend = Arc::new(Backend::default());
        let function = Arc::new(PciFunction::new(
            PciIdentity {
                vendor_device: 0x1234_8086,
                class_revision: 0,
                subsystem: 0,
            },
            backend.clone(),
        ));
        function.write_config(MSI + 4, 4, 0xfee0_0000).unwrap();
        function.write_config(MSI + 12, 2, 69).unwrap();
        function.write_config(MSI + 2, 2, 1).unwrap();
        function.interrupt();
        assert!(backend.routes.lock().unwrap().is_empty());
        assert_eq!(function.read_config(MSI + 20, 4), 1);
        function.write_config(MSI + 20, 4, 0).unwrap();
        assert_eq!(function.read_config(MSI + 20, 4), 1);
        function.write_config(MSI + 8, 4, 1).unwrap();
        function.write_config(MSI + 16, 4, 0).unwrap();
        assert_eq!(
            *backend.delivered.lock().unwrap(),
            [MsiMessage {
                address: 0x1_fee0_0000,
                data: 69
            }]
        );
        assert_eq!(function.read_config(MSI + 20, 4), 0);
        backend.reject.store(true, Ordering::Relaxed);
        assert_eq!(function.write_config(MSI + 16, 4, 1), Err(AxError::Io));
        assert_eq!(function.read_config(MSI + 16, 4), 0);
        backend.reject.store(false, Ordering::Relaxed);
        std::thread::scope(|scope| {
            for base in [80, 100] {
                let function = &function;
                scope.spawn(move || {
                    for vector in base..base + 20 {
                        function.write_config(MSI + 12, 2, vector).unwrap();
                    }
                });
            }
        });
        let last = *backend.routes.lock().unwrap().last().unwrap();
        assert_eq!(
            last.unwrap().data as usize,
            function.read_config(MSI + 12, 2)
        );
        function.write_config(MSI + 16, 4, 1).unwrap();
        assert_eq!(*backend.routes.lock().unwrap().last().unwrap(), None);
    }

    fn config() -> Config {
        let mut config = Config {
            bytes: [0; 256],
            bars: BTreeMap::new(),
            pending: false,
        };
        config.bytes[MSI + 2] = 0x80;
        config.bytes[MSI + 3] = 1;
        config.bars.insert(
            0,
            Bar {
                aperture: (0x1_0000_0000usize..0x1_0800_0000usize).try_into().unwrap(),
                size: 0x800_0000,
                flags: 4,
                address: 0x1_0000_0000,
                probe: [false; 2],
            },
        );
        config
    }

    fn read32(config: &Config, offset: usize) -> u32 {
        (0..4).fold(0, |value, i| {
            value | (u32::from(config.read_byte(offset + i)) << (8 * i))
        })
    }

    #[test]
    fn bar_size_probe_preserves_64bit_address_and_flags() {
        let mut config = config();
        config.write(0x10, 4, u32::MAX.into());
        config.write(0x14, 4, u32::MAX.into());
        assert_eq!(read32(&config, 0x10), 0xf800_0004);
        assert_eq!(read32(&config, 0x14), u32::MAX);
        assert_eq!(config.bars[&0].address, 0x1_0000_0000);
        config.write(0x10, 4, 4);
        config.write(0x14, 4, 1);
        assert_eq!(read32(&config, 0x10), 4);
        assert_eq!(read32(&config, 0x14), 1);
        config.write(0x10, 2, 0xffff);
        assert_eq!(read32(&config, 0x10), 4);
        assert_eq!(read32(&config, 0x18), 0);
    }

    #[test]
    fn only_guest_controls_are_mutable() {
        let mut config = config();
        config.bytes[0] = 0x86;
        config.write(0, 4, 0);
        assert_eq!(config.bytes[0], 0x86);
        config.write(4, 4, u32::MAX.into());
        assert_eq!(read32(&config, 4), 0x406);
        assert!(config.memory_enabled());
        config.write(4, 2, 0);
        assert!(!config.memory_enabled());
        config.write(MSI + 2, 2, 0xffff);
        assert_eq!(config.bytes[MSI + 2], 0x81);
        assert_eq!(config.bytes[MSI + 3], 1);
    }
}
