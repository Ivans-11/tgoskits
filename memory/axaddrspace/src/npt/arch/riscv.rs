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

use core::sync::atomic::{AtomicUsize, Ordering};

pub use ax_page_table_entry::riscv::Rv64PTE;
use ax_page_table_multiarch::PagingMetaData;

static GUEST_TLB_GENERATION: AtomicUsize = AtomicUsize::new(1);

/// Generation of guest translation changes, including remote VS-stage fences.
pub fn guest_tlb_generation() -> usize {
    GUEST_TLB_GENERATION.load(Ordering::Acquire)
}

/// Makes retained guest translations stale on their next entry.
///
/// This supplements, rather than replaces, the caller's local/remote fence.
pub fn invalidate_guest_tlbs() {
    let _ = GUEST_TLB_GENERATION.fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
        Some(v.saturating_add(1))
    });
}

/// RISC-V G-stage table: unsigned GPAs and a four-page root table.
pub struct GStageMetaData<const LEVELS: usize>;

impl<const LEVELS: usize> PagingMetaData for GStageMetaData<LEVELS> {
    const LEVELS: usize = LEVELS;
    const PA_MAX_BITS: usize = 56;
    const VA_MAX_BITS: usize = 12 + LEVELS * 9 + 2;
    const ROOT_ENTRY_COUNT: usize = 2048;

    type VirtAddr = crate::GuestPhysAddr;

    fn vaddr_is_valid(vaddr: usize) -> bool {
        vaddr < (1usize << Self::VA_MAX_BITS)
    }

    fn flush_tlb(_vaddr: Option<Self::VirtAddr>) {
        invalidate_guest_tlbs();
        // G-stage mappings require GVMA, not the VS-stage VVMA fence.
        // Keep a memory clobber so PTE stores complete before invalidation.
        unsafe {
            core::arch::asm!(".insn r 0x73, 0, 0x31, x0, x0, x0", options(nostack));
        }
    }
}
