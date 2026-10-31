use std::{
    alloc::{self, Layout},
    cell::RefCell,
    collections::{HashMap, HashSet},
    marker::PhantomData,
};

use ax_memory_addr::{PhysAddr, VirtAddr};
use ax_page_table_entry::{GenericPTE, MappingFlags};
use ax_page_table_multiarch::{PageSize, PageTable64, PagingHandler, PagingMetaData, PagingResult};
use rand::{RngExt, SeedableRng, rngs::SmallRng};

/// Creates a layout for allocating `num` pages with alignment of `2^align_pow2`
/// pages.
const fn pages_layout(num: usize, align: usize) -> Layout {
    if !align.is_power_of_two() {
        panic!("alignment must be a power of two");
    }
    if align % 4096 != 0 {
        panic!("alignment must be a multiple of 4K");
    }
    unsafe { Layout::from_size_align_unchecked(4096 * num, align) }
}

const PAGE_LAYOUT: Layout = pages_layout(1, 4096);

thread_local! {
    static ALLOCATED: RefCell<HashSet<usize>> = RefCell::default();
    static ALIGN: RefCell<HashMap<usize, usize>> = RefCell::default();
}

struct TrackPagingHandler<M: PagingMetaData>(PhantomData<M>);

impl<M: PagingMetaData> PagingHandler for TrackPagingHandler<M> {
    fn alloc_frame() -> Option<PhysAddr> {
        let ptr = unsafe { alloc::alloc(PAGE_LAYOUT) } as usize;
        assert!(
            ptr <= M::PA_MAX_ADDR,
            "allocated frame address exceeds PA_MAX_ADDR"
        );
        ALLOCATED.with_borrow_mut(|it| it.insert(ptr));
        Some(PhysAddr::from_usize(ptr))
    }

    fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr> {
        let layout = pages_layout(num, align);
        let ptr = unsafe { alloc::alloc(layout) } as usize;
        assert!(
            ptr <= M::PA_MAX_ADDR,
            "allocated frame address exceeds PA_MAX_ADDR"
        );
        ALLOCATED.with_borrow_mut(|it| {
            for i in 0..num {
                it.insert(ptr + i * 4096);
            }
        });
        ALIGN.with_borrow_mut(|it| {
            it.insert(ptr, align);
        });
        Some(PhysAddr::from_usize(ptr))
    }

    fn dealloc_frame(paddr: PhysAddr) {
        let ptr = paddr.as_usize();
        ALLOCATED.with_borrow_mut(|it| {
            assert!(it.remove(&ptr), "dealloc a frame that was not allocated");
        });
        unsafe {
            alloc::dealloc(ptr as _, PAGE_LAYOUT);
        }
    }

    fn dealloc_frames(paddr: PhysAddr, num: usize) {
        let ptr = paddr.as_usize();
        ALLOCATED.with_borrow_mut(|it| {
            for i in 0..num {
                let addr = ptr + i * 4096;
                assert!(it.remove(&addr), "dealloc a frame that was not allocated");
            }
        });
        let align = ALIGN.with_borrow_mut(|it| {
            it.remove(&ptr)
                .expect("dealloc frames that were not allocated")
        });
        let layout = pages_layout(num, align);
        unsafe {
            alloc::dealloc(ptr as _, layout);
        }
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        assert!(paddr.as_usize() > 0);
        VirtAddr::from_usize(paddr.as_usize())
    }
}

fn run_test_for<M: PagingMetaData<VirtAddr = VirtAddr>, PTE: GenericPTE>() -> PagingResult<()> {
    ALLOCATED.with_borrow_mut(|it| {
        it.clear();
    });

    let vaddr_mask = ((1u64 << M::VA_MAX_BITS) - 1) & !0xfff;

    let mut table = PageTable64::<M, PTE, TrackPagingHandler<M>>::try_new().unwrap();
    let mut pages = HashSet::new();
    let mut rng = SmallRng::seed_from_u64(1234);

    for _ in 0..2048 {
        let mut cursor = table.cursor();
        if rng.random_ratio(3, 4) || pages.is_empty() {
            // insert a mapping
            let addr = loop {
                let addr = rng.random::<u64>() & vaddr_mask;
                if pages.insert(addr) {
                    break addr;
                }
            };
            cursor.map(
                VirtAddr::from_usize(addr as usize),
                PhysAddr::from_usize((rng.random::<u64>() & vaddr_mask) as usize),
                PageSize::Size4K,
                MappingFlags::READ | MappingFlags::WRITE,
            )?;
        } else {
            // remove a mapping
            let addr = *pages.iter().next().unwrap();
            cursor.unmap(VirtAddr::from_usize(addr as usize))?;
            pages.remove(&addr);
        }
    }

    drop(table);
    assert_eq!(
        ALLOCATED.with_borrow(|it| it.len()),
        0,
        "Some frames were not deallocated"
    );

    Ok(())
}

#[cfg(target_pointer_width = "32")]
fn run_test_for_32bit<M: PagingMetaData<VirtAddr = VirtAddr>, PTE: GenericPTE>() -> PagingResult<()>
{
    use ax_page_table_multiarch::PageTable32;
    ALLOCATED.with_borrow_mut(|it| {
        it.clear();
    });

    let vaddr_mask = ((1u64 << M::VA_MAX_BITS) - 1) & !0xfff;

    let mut table = PageTable32::<M, PTE, TrackPagingHandler<M>>::try_new().unwrap();
    let mut pages = HashSet::new();
    let mut rng = SmallRng::seed_from_u64(5678);
    for _ in 0..512 {
        // Fewer iterations for 32-bit to avoid address space exhaustion
        if rng.random_ratio(3, 4) || pages.is_empty() {
            // insert a mapping
            let addr = loop {
                let addr = rng.random::<u32>() & (vaddr_mask as u32);
                if pages.insert(addr as u64) {
                    break addr as u64;
                }
            };
            table
                .map(
                    VirtAddr::from_usize(addr as usize),
                    PhysAddr::from_usize((rng.random::<u32>() & (vaddr_mask as u32)) as usize),
                    PageSize::Size4K,
                    MappingFlags::READ | MappingFlags::WRITE,
                )?
                .ignore();
        } else {
            // remove a mapping
            let addr = *pages.iter().next().unwrap();
            table.unmap(VirtAddr::from_usize(addr as usize))?.2.ignore();
            pages.remove(&addr);
        }
    }

    drop(table);
    assert_eq!(
        ALLOCATED.with_borrow(|it| it.len()),
        0,
        "Some frames were not deallocated"
    );

    Ok(())
}

#[test]
#[cfg(any(target_arch = "arm", docsrs))]
#[cfg(target_pointer_width = "32")]
fn test_dealloc_arm32() -> PagingResult<()> {
    run_test_for_32bit::<
        ax_page_table_multiarch::arm::A32PagingMetaData,
        ax_page_table_entry::arm::A32PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "x86_64", docsrs))]
fn test_dealloc_x86() -> PagingResult<()> {
    run_test_for::<
        ax_page_table_multiarch::x86_64::X64PagingMetaData,
        ax_page_table_entry::x86_64::X64PTE,
    >()?;
    Ok(())
}

// Exercise the generic x4-root machinery on the build host, including all
// four root pages and allocation/deallocation pairing.
#[test]
#[cfg(target_arch = "x86_64")]
fn test_x4_root() -> PagingResult<()> {
    struct X4<const LEVELS: usize>;
    impl<const LEVELS: usize> PagingMetaData for X4<LEVELS> {
        const LEVELS: usize = LEVELS;
        const PA_MAX_BITS: usize = 52;
        const VA_MAX_BITS: usize = 12 + LEVELS * 9 + 2;
        const ROOT_ENTRY_COUNT: usize = 2048;
        type VirtAddr = VirtAddr;
        fn flush_tlb(_: Option<VirtAddr>) {}
    }
    fn check<const LEVELS: usize>() -> PagingResult<()> {
        type Pte = ax_page_table_entry::x86_64::X64PTE;
        let mut table =
            PageTable64::<X4<LEVELS>, Pte, TrackPagingHandler<X4<LEVELS>>, 4>::try_new()?;
        assert_eq!(table.root_paddr().as_usize() % 16384, 0);
        let stride = 1usize << (12 + LEVELS * 9);
        for i in 0..4 {
            table.cursor().map(
                (i * stride + 0x1000).into(),
                (0x1000 + i * 4096).into(),
                PageSize::Size4K,
                MappingFlags::READ,
            )?;
        }
        for i in 0..4 {
            assert_eq!(
                table.query((i * stride + 0x1000).into())?.0.as_usize(),
                0x1000 + i * 4096
            );
        }
        #[cfg(feature = "copy-from")]
        {
            let mut borrowed =
                PageTable64::<X4<LEVELS>, Pte, TrackPagingHandler<X4<LEVELS>>, 4>::try_new()?;
            borrowed.cursor().copy_from(&table, 0.into(), 4 * stride);
            for i in 0..4 {
                assert_eq!(
                    borrowed.query((i * stride + 0x1000).into())?.0.as_usize(),
                    0x1000 + i * 4096
                );
            }
            // Dropping the borrower must not free any of the source's subtrees.
            drop(borrowed);
            assert!(table.query((3 * stride + 0x1000).into()).is_ok());
        }
        drop(table);
        assert_eq!(ALLOCATED.with_borrow(|it| it.len()), 0);
        assert_eq!(ALIGN.with_borrow(|it| it.len()), 0);
        Ok(())
    }
    check::<3>()?;
    check::<4>()
}

#[test]
#[cfg(all(target_arch = "x86_64", feature = "copy-from"))]
fn test_ordinary_root_bitmap_size() {
    use ax_page_table_entry::x86_64::X64PTE;
    use ax_page_table_multiarch::x86_64::X64PagingMetaData;
    type Table = PageTable64<X64PagingMetaData, X64PTE, TrackPagingHandler<X64PagingMetaData>>;
    // Original layout: one physical address and one 512-bit bitmap, including
    // the bitmap's alignment padding.
    assert_eq!(
        core::mem::size_of::<Table>(),
        core::mem::size_of::<(PhysAddr, bitmaps::Bitmap<512>)>()
    );
}

#[test]
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", docsrs))]
fn test_dealloc_riscv() -> PagingResult<()> {
    run_test_for::<
        ax_page_table_multiarch::riscv::Sv39MetaData<VirtAddr>,
        ax_page_table_entry::riscv::Rv64PTE,
    >()?;
    run_test_for::<
        ax_page_table_multiarch::riscv::Sv48MetaData<VirtAddr>,
        ax_page_table_entry::riscv::Rv64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "aarch64", docsrs))]
fn test_dealloc_aarch64() -> PagingResult<()> {
    run_test_for::<
        ax_page_table_multiarch::aarch64::A64PagingMetaData,
        ax_page_table_entry::aarch64::A64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "loongarch64", docsrs))]
fn test_dealloc_loongarch64() -> PagingResult<()> {
    run_test_for::<
        ax_page_table_multiarch::loongarch64::LA64MetaData,
        ax_page_table_entry::loongarch64::LA64PTE,
    >()?;
    Ok(())
}
