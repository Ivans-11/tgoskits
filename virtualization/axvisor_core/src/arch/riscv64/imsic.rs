//! Host-independent discovery of the physical IMSIC guest file.

use axvisor_api::memory::{PhysAddr, phys_to_virt};
use fdt_parser::{Fdt, Node};

/// Initializes hardware AIA from the existing host FDT and memory APIs.
///
/// Called once from the core's per-CPU initialization on a pinned CPU, after
/// core per-CPU storage is ready. The architecture API supplies the hardware
/// hart ID. Building with `aia-hw` opts into core ownership of guest file 1;
/// the host must reserve it and make its MMIO mapping available for the core's
/// lifetime. Without the feature, the core uses the software AIA backend.
pub(crate) fn init_on_cpu() -> bool {
    let Some(hart_id) = axvisor_api::arch::current_hart_id() else {
        return false;
    };
    let Some(bytes) = crate::vmm::fdt::try_get_host_fdt() else {
        return false;
    };
    let Ok(fdt) = Fdt::from_bytes(bytes) else {
        return false;
    };
    let Some((address, ids)) = guest_file(&fdt, hart_id) else {
        return false;
    };
    let address = phys_to_virt(PhysAddr::from_usize(address)).as_usize();
    riscv_vcpu::aia::init_hardware_on_cpu(address, ids)
}

fn u32_property(node: &Node<'_>, name: &str) -> Option<usize> {
    let bytes = node.find_property(name)?.raw_value();
    Some(u32::from_be_bytes(bytes.try_into().ok()?) as usize)
}

fn enabled(node: &Node<'_>) -> bool {
    node.find_property("status")
        .is_none_or(|property| matches!(property.raw_value(), b"okay\0" | b"ok\0"))
}

fn hart_phandle(fdt: &Fdt<'_>, hart_id: usize) -> Option<usize> {
    let mut cpu_level = None;
    for node in fdt.all_nodes() {
        if cpu_level.is_some_and(|level| node.level <= level) {
            cpu_level = None;
        }
        if node
            .find_property("device_type")
            .is_some_and(|p| p.raw_value() == b"cpu\0")
            && enabled(&node)
        {
            let reg = node.find_property("reg")?;
            let id = match reg.raw_value().len() {
                4 => u32::from_be_bytes(reg.raw_value().try_into().ok()?) as usize,
                8 => usize::try_from(u64::from_be_bytes(reg.raw_value().try_into().ok()?)).ok()?,
                _ => continue,
            };
            if id == hart_id {
                cpu_level = Some(node.level);
            }
        } else if cpu_level.is_some_and(|level| node.level == level + 1)
            && node.compatibles().any(|c| c == "riscv,cpu-intc")
            && u32_property(&node, "#interrupt-cells") == Some(1)
        {
            return node.phandle().map(|p| p.as_usize());
        }
    }
    None
}

fn guest_file(fdt: &Fdt<'_>, hart_id: usize) -> Option<(usize, usize)> {
    let phandle = hart_phandle(fdt, hart_id)?;
    for node in fdt
        .all_nodes()
        .filter(|node| enabled(node) && node.compatibles().any(|c| c == "riscv,imsics"))
    {
        if let Some(file) = controller_file(&node, phandle) {
            return Some(file);
        }
    }
    None
}

fn controller_file(node: &Node<'_>, phandle: usize) -> Option<(usize, usize)> {
    let guest_bits = u32_property(node, "riscv,guest-index-bits")?;
    let ids = u32_property(node, "riscv,num-guest-ids")
        .or_else(|| u32_property(node, "riscv,num-ids"))?;
    if !(1..=7).contains(&guest_bits) || !(63..=2047).contains(&ids) || ids & 63 != 63 {
        return None;
    }
    let parents = node.find_property("interrupts-extended")?;
    let parents = parents.raw_value();
    if !parents.len().is_multiple_of(8) {
        return None;
    }
    let mut parents = parents.chunks_exact(8);
    let stride = 4096usize << guest_bits;
    let mut selected = None;
    for region in node.reg()? {
        let start = usize::try_from(region.address).ok()?;
        let size = region.size?;
        start.checked_add(size)?;
        if start & 4095 != 0 || size == 0 || !size.is_multiple_of(stride) {
            return None;
        }
        for offset in (0..size).step_by(stride) {
            let parent = parents.next()?;
            let handle = u32::from_be_bytes(parent[..4].try_into().ok()?) as usize;
            let irq = u32::from_be_bytes(parent[4..].try_into().ok()?);
            if handle == phandle && irq == 9 {
                if selected.is_some() {
                    return None;
                }
                selected = Some((start + offset + 4096, ids));
            }
        }
    }
    if parents.next().is_some() {
        return None;
    }
    selected
}
