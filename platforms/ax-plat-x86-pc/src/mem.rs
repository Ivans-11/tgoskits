//! Physical memory information.

use ax_lazyinit::LazyInit;
use ax_plat::mem::{MemIf, PhysAddr, RawRange, VirtAddr, pa, va};
use heapless::Vec;
use multiboot::information::{MemoryManagement, MemoryType, Multiboot, PAddr};

use crate::config::{devices::MMIO_RANGES, plat::PHYS_VIRT_OFFSET};

const MAX_REGIONS: usize = 128;

static RAM_REGIONS: LazyInit<Vec<RawRange, MAX_REGIONS>> = LazyInit::new();
static DEVICE_REGIONS: LazyInit<Vec<RawRange, MAX_REGIONS>> = LazyInit::new();

pub fn init(multiboot_info_ptr: usize) {
    let mut mm = MemIfImpl;
    let mut available = Vec::<RawRange, MAX_REGIONS>::new();
    let mut framebuffer = None;
    let mut rsdp = None;
    if multiboot_info_ptr & 1 != 0 {
        use crate::bootinfo::{tags, u32_at, u64_at};
        for (kind, data) in tags(multiboot_info_ptr & !1) {
            match kind {
                6 if data.len() >= 8 => {
                    let stride = u32_at(data, 0) as usize;
                    assert!(stride >= 24, "invalid Multiboot2 memory map stride");
                    for entry in data[8..].chunks_exact(stride) {
                        if u32_at(entry, 16) == 1 {
                            available
                                .push((u64_at(entry, 0) as usize, u64_at(entry, 8) as usize))
                                .unwrap();
                        }
                    }
                }
                8 if data.len() >= 30 && data[21] == 1 => {
                    framebuffer = crate::screen::init_rgb(
                        u64_at(data, 0),
                        u32_at(data, 8),
                        u32_at(data, 12),
                        u32_at(data, 16),
                        data[20],
                    );
                }
                14 if rsdp.is_none() => rsdp = Some(data),
                15 => rsdp = Some(data),
                _ => {}
            }
        }
    } else {
        let info = unsafe { Multiboot::from_ptr(multiboot_info_ptr as _, &mut mm).unwrap() };
        framebuffer = info.framebuffer_table().and_then(crate::screen::init);
        for r in info.memory_regions().unwrap() {
            if r.memory_type() == MemoryType::Available {
                available
                    .push((r.base_address() as usize, r.length() as usize))
                    .unwrap();
            }
        }
    }
    crate::console::init_early();
    crate::topology::init(rsdp);
    let mut devices = Vec::<RawRange, MAX_REGIONS>::new();
    devices.extend_from_slice(MMIO_RANGES).unwrap();
    if let Some(range) = framebuffer {
        devices.push(range).unwrap();
    }
    devices.sort_unstable_by_key(|range| range.0);
    // The framebuffer may lie inside a preconfigured PCI MMIO window.
    let mut merged = Vec::<RawRange, MAX_REGIONS>::new();
    for (start, size) in devices {
        if let Some((previous, length)) = merged.last_mut()
            && start <= *previous + *length
        {
            *length = (*previous + *length).max(start + size) - *previous;
            continue;
        }
        merged.push((start, size)).unwrap();
    }
    DEVICE_REGIONS.init_once(merged);

    let mut regions = Vec::new();
    for range in available {
        let ram = [range];
        // Some firmware includes the framebuffer in its available RAM map.
        ax_plat::mem::ranges_difference(&ram, DEVICE_REGIONS.as_slice(), |range| {
            regions.push(range).unwrap();
        })
        .unwrap();
    }
    RAM_REGIONS.init_once(regions);
}

struct MemIfImpl;

impl MemoryManagement for MemIfImpl {
    unsafe fn paddr_to_slice(&self, addr: PAddr, size: usize) -> Option<&'static [u8]> {
        let ptr = Self::phys_to_virt(pa!(addr as usize)).as_ptr();
        Some(unsafe { core::slice::from_raw_parts(ptr, size) })
    }

    unsafe fn allocate(&mut self, _length: usize) -> Option<(PAddr, &mut [u8])> {
        None
    }

    unsafe fn deallocate(&mut self, _addr: PAddr) {}
}

#[impl_plat_interface]
impl MemIf for MemIfImpl {
    /// Returns all physical memory (RAM) ranges on the platform.
    fn phys_ram_ranges() -> &'static [RawRange] {
        RAM_REGIONS.as_slice()
    }

    /// Returns all reserved physical memory ranges on the platform.
    ///
    /// Lower 1MiB memory is reserved and not allocatable.
    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        &[(0, 0x100000)]
    }

    /// Returns all device memory (MMIO) ranges on the platform.
    fn mmio_ranges() -> &'static [RawRange] {
        DEVICE_REGIONS.as_slice()
    }

    /// Translates a physical address to a virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        va!(paddr.as_usize() + PHYS_VIRT_OFFSET)
    }

    /// Translates a virtual address to a physical address.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        pa!(vaddr.as_usize() - PHYS_VIRT_OFFSET)
    }

    /// Returns the kernel address space base virtual address and size.
    fn kernel_aspace() -> (VirtAddr, usize) {
        (
            va!(crate::config::plat::KERNEL_ASPACE_BASE),
            crate::config::plat::KERNEL_ASPACE_SIZE,
        )
    }
}
