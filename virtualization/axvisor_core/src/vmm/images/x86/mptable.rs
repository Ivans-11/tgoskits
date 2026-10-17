//! Minimal Intel MultiProcessor table for x86 Linux direct boot.

use alloc::vec::Vec;

use super::linux::X86LinuxRange;

pub const MP_TABLE_GPA: usize = 0x9f800;
pub const MP_TABLE_SIZE: usize = 0x800;

const MP_CONFIG_GPA: usize = MP_TABLE_GPA;
const MP_FLOATING_POINTER_GPA: usize = 0x9fc00;
const MP_FLOATING_POINTER_OFFSET: usize = MP_FLOATING_POINTER_GPA - MP_TABLE_GPA;

const LOCAL_APIC_ADDR: u32 = 0xfee0_0000;
const IO_APIC_ADDR: u32 = 0xfec0_0000;

const BSP_APIC_ID: u8 = 0;
const IO_APIC_ID: u8 = 0x0f;
const APIC_VERSION: u8 = 0x14;
const IO_APIC_VERSION: u8 = 0x11;

const BUS_ID_PCI: u8 = 0;
const BUS_ID_ISA: u8 = 1;

/// Returns the reserved guest physical range occupied by the MP table.
pub const fn reserved_range() -> X86LinuxRange {
    X86LinuxRange::new(MP_TABLE_GPA, MP_TABLE_SIZE)
}

/// Builds a minimal MP floating pointer and MP config table.
pub fn build(cpu_count: usize, pci_routes: &[[u8; 4]]) -> Option<[u8; MP_TABLE_SIZE]> {
    let mut image = [0u8; MP_TABLE_SIZE];
    let cpu_count = cpu_count.max(1);
    if cpu_count > IO_APIC_ID as usize
        || pci_routes.iter().any(|&[bus, dev, pin, gsi]| {
            bus == u8::MAX || dev > 31 || !(1..=4).contains(&pin) || gsi >= 24
        })
        || pci_routes.iter().enumerate().any(|(index, route)| {
            pci_routes[..index]
                .iter()
                .any(|other| other[..3] == route[..3])
        })
    {
        return None;
    }

    let config = build_config_table(cpu_count, pci_routes);
    if config.len() > MP_FLOATING_POINTER_OFFSET {
        return None;
    }
    image[..config.len()].copy_from_slice(&config);

    let floating = build_floating_pointer();
    image[MP_FLOATING_POINTER_OFFSET..MP_FLOATING_POINTER_OFFSET + floating.len()]
        .copy_from_slice(&floating);

    Some(image)
}

fn build_floating_pointer() -> [u8; 16] {
    let mut data = [0u8; 16];
    data[0..4].copy_from_slice(b"_MP_");
    data[4..8].copy_from_slice(&(MP_CONFIG_GPA as u32).to_le_bytes());
    data[8] = 1; // length in 16-byte units
    data[9] = 4; // MP spec revision 1.4
    data[10] = checksum(&data);
    data
}

fn build_config_table(cpu_count: usize, pci_routes: &[[u8; 4]]) -> Vec<u8> {
    let entries = config_entries(cpu_count, pci_routes);
    let entries_len: usize = entries.iter().map(Vec::len).sum();

    let mut table = Vec::with_capacity(44 + entries_len);
    table.extend_from_slice(b"PCMP");
    table.extend_from_slice(&0u16.to_le_bytes());
    table.push(4); // MP spec revision 1.4
    table.push(0); // checksum, patched below
    table.extend_from_slice(b"AXVISOR ");
    table.extend_from_slice(b"X86LINUX    ");
    table.extend_from_slice(&0u32.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    table.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    table.extend_from_slice(&LOCAL_APIC_ADDR.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    table.push(0);
    table.push(0);
    for entry in &entries {
        table.extend_from_slice(entry);
    }

    let len = table.len() as u16;
    table[4..6].copy_from_slice(&len.to_le_bytes());
    table[7] = checksum(&table);
    table
}

fn config_entries(cpu_count: usize, pci_routes: &[[u8; 4]]) -> Vec<Vec<u8>> {
    let mut entries = Vec::new();
    for apic_id in 0..cpu_count {
        entries.push(processor_entry(apic_id as u8));
    }
    // Keep the ISA bus separate from every explicitly described PCI bus.
    // Linux accepts the MP table's 8-bit bus number; using 0xff here avoids
    // colliding with the passthrough device's bus 1.
    let isa_bus = if pci_routes.is_empty() {
        BUS_ID_ISA
    } else {
        u8::MAX
    };
    entries.extend([
        bus_entry(BUS_ID_PCI, b"PCI   "),
        bus_entry(isa_bus, b"ISA   "),
        io_apic_entry(),
    ]);
    push_isa_interrupt_entries(&mut entries, isa_bus);
    if pci_routes.is_empty() {
        push_pci_interrupt_entries(&mut entries);
    } else {
        let mut buses = alloc::vec![BUS_ID_PCI];
        for &[bus, dev, pin, gsi] in pci_routes {
            if !buses.contains(&bus) {
                entries.push(bus_entry(bus, b"PCI   "));
                buses.push(bus);
            }
            entries.push(interrupt_entry(0, bus, (dev << 2) | (pin - 1), gsi));
        }
    }
    entries
}

fn processor_entry(apic_id: u8) -> Vec<u8> {
    let mut entry = Vec::with_capacity(20);
    entry.push(0);
    entry.push(apic_id);
    entry.push(APIC_VERSION);
    let flags = if apic_id == BSP_APIC_ID { 0x03 } else { 0x01 };
    entry.push(flags); // enabled, plus BSP for CPU0
    entry.extend_from_slice(&0x0000_0600u32.to_le_bytes());
    entry.extend_from_slice(&0x0000_0201u32.to_le_bytes());
    entry.extend_from_slice(&[0; 8]);
    entry
}

fn bus_entry(bus_id: u8, bus_type: &[u8; 6]) -> Vec<u8> {
    let mut entry = Vec::with_capacity(8);
    entry.push(1);
    entry.push(bus_id);
    entry.extend_from_slice(bus_type);
    entry
}

fn io_apic_entry() -> Vec<u8> {
    let mut entry = Vec::with_capacity(8);
    entry.push(2);
    entry.push(IO_APIC_ID);
    entry.push(IO_APIC_VERSION);
    entry.push(0x01); // enabled
    entry.extend_from_slice(&IO_APIC_ADDR.to_le_bytes());
    entry
}

fn push_isa_interrupt_entries(entries: &mut Vec<Vec<u8>>, isa_bus: u8) {
    // Keep legacy ISA IRQs identity-routed to the IOAPIC. IRQ0 is timer and
    // IRQ4 is COM1; both are useful during early Linux bring-up diagnostics.
    for irq in 0u8..16 {
        entries.push(interrupt_entry(0, isa_bus, irq, irq));
    }
}

fn push_pci_interrupt_entries(entries: &mut Vec<Vec<u8>>) {
    // QEMU q35 exposes the host rootfs virtio-blk as 00:03.0 and the static
    // guest network device as 00:04.0. Add enough INTx routing for Linux to
    // build the PCI IRQ table before a fuller virtual PCI IRQ router exists.
    for dev in 0u8..5 {
        for pin in 0u8..4 {
            let source_irq = (dev << 2) | pin;
            let intin = pci_intx_gsi(dev, pin);
            entries.push(interrupt_entry(0, BUS_ID_PCI, source_irq, intin));
        }
    }
}

const fn pci_intx_gsi(dev: u8, pin: u8) -> u8 {
    // Keep the guest MP table aligned with the q35 PCI interrupt line exposed
    // for the current rootfs device. This is intentionally narrow; a later PCI
    // IRQ router should derive it from platform/device topology instead.
    match (dev, pin) {
        (3, 0) => 11,
        (4, 0) => 10,
        _ => 16 + ((dev + pin) & 3),
    }
}

fn interrupt_entry(
    interrupt_type: u8,
    source_bus_id: u8,
    source_bus_irq: u8,
    dest_io_apic_intin: u8,
) -> Vec<u8> {
    let mut entry = Vec::with_capacity(8);
    entry.push(3);
    entry.push(interrupt_type);
    entry.extend_from_slice(&0u16.to_le_bytes());
    entry.push(source_bus_id);
    entry.push(source_bus_irq);
    entry.push(IO_APIC_ID);
    entry.push(dest_io_apic_intin);
    entry
}

fn checksum(bytes: &[u8]) -> u8 {
    0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_mp_table_checksums() {
        let image = build(2, &[]).unwrap();
        let config_len = u16::from_le_bytes([image[4], image[5]]) as usize;
        assert_eq!(&image[..4], b"PCMP");
        assert_eq!(
            image[..config_len]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );

        let fp = &image[MP_FLOATING_POINTER_OFFSET..MP_FLOATING_POINTER_OFFSET + 16];
        assert_eq!(&fp[..4], b"_MP_");
        assert_eq!(fp.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)), 0);
        assert_eq!(
            u32::from_le_bytes([fp[4], fp[5], fp[6], fp[7]]) as usize,
            MP_CONFIG_GPA
        );
    }

    #[test]
    fn routes_static_virtio_devices_to_q35_intx_lines() {
        assert_eq!(pci_intx_gsi(3, 0), 11);
        assert_eq!(pci_intx_gsi(4, 0), 10);
    }

    #[test]
    fn keeps_isa_bus_separate_from_downstream_pci_bus() {
        let entries = config_entries(1, &[[1, 0, 1, 16]]);
        assert!(entries.contains(&bus_entry(1, b"PCI   ")));
        assert!(entries.contains(&bus_entry(u8::MAX, b"ISA   ")));
        assert!(entries.contains(&interrupt_entry(0, u8::MAX, 0, 0)));
        assert!(entries.contains(&interrupt_entry(0, 1, 0, 16)));
        assert!(build(1, &[[1, 0, 1, 16]]).is_some());
    }

    #[test]
    fn rejects_invalid_and_oversized_routes() {
        for route in [
            [255, 0, 1, 16],
            [1, 32, 1, 16],
            [1, 0, 0, 16],
            [1, 0, 1, 24],
        ] {
            assert!(build(1, &[route]).is_none());
        }
        assert!(build(1, &[[1, 0, 1, 16], [1, 0, 1, 17]]).is_none());
        let routes: Vec<_> = (0..128).map(|bus| [bus, 0, 1, 16]).collect();
        assert!(build(1, &routes).is_none());
    }
}
