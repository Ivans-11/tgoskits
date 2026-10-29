//! CPUID table lookup and software XSAVE sizing for VMX guests.

use alloc::vec::Vec;

use axvcpu::AxVCpuCpuidEntry;
use raw_cpuid::CpuIdResult;

const ZERO: CpuIdResult = CpuIdResult {
    eax: 0,
    ebx: 0,
    ecx: 0,
    edx: 0,
};
const INDEX_SIGNIFICANT: u32 = 1;

/// Final virtualized results; invalidated on CPUID-table, CR4 or XCR0 changes.
#[derive(Debug)]
pub(super) struct GuestCpuidCache(Vec<(u32, u32, CpuIdResult)>);

impl GuestCpuidCache {
    pub fn new() -> Self {
        Self(Vec::with_capacity(64))
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn get(&self, leaf: u32, index: u32) -> Option<CpuIdResult> {
        self.0
            .iter()
            .find(|(l, i, _)| *l == leaf && *i == index)
            .map(|e| e.2)
    }

    pub fn insert(&mut self, leaf: u32, index: u32, value: CpuIdResult) {
        // Do not memoize unknown leaves which may describe changing hardware
        // state. The known dynamic leaves below are invalidated by the vCPU.
        if matches!(
            leaf,
            0 | 1
                | 2
                | 4
                | 5
                | 7
                | 0xa
                | 0xb
                | 0xd
                | 0x15
                | 0x16
                | 0x1f
                | 0x4000_0000
                | 0x4000_0001
                | 0x8000_0000..=0x8000_0008
        ) && self.0.len() < 64
        {
            self.0.push((leaf, index, value));
        }
    }
}

fn entry(entries: &[AxVCpuCpuidEntry], leaf: u32, index: u32) -> Option<CpuIdResult> {
    entries
        .iter()
        .find(|e| e.function == leaf && (e.flags & INDEX_SIGNIFICANT == 0 || e.index == index))
        .map(|e| CpuIdResult {
            eax: e.eax,
            ebx: e.ebx,
            ecx: e.ecx,
            edx: e.edx,
        })
}

/// Bounds cache storage even if a guest issues arbitrary CPUID inputs.
#[derive(Debug)]
pub(super) struct CpuidCache(Vec<(u32, u32, CpuIdResult)>);

impl CpuidCache {
    pub fn new() -> Self {
        Self(Vec::with_capacity(64))
    }

    pub fn host(&mut self, leaf: u32, index: u32) -> CpuIdResult {
        // Cache only known static descriptions. Dynamic fields in leaves 1,
        // 7 and D are supplied from guest state by the caller.
        let index = match leaf {
            0 | 1 | 2 | 0x15 | 0x16 | 0x8000_0000..=0x8000_0008 => 0,
            4 | 7 | 0xd | 0x14 | 0x19 => index,
            _ => return raw_cpuid::cpuid!(leaf, index),
        };
        if let Some((_, _, value)) = self.0.iter().find(|(l, i, _)| *l == leaf && *i == index) {
            return *value;
        }
        let value = raw_cpuid::cpuid!(leaf, index);
        if self.0.len() < 64 {
            self.0.push((leaf, index, value));
        }
        value
    }

    pub fn query(&mut self, entries: &[AxVCpuCpuidEntry], leaf: u32, index: u32) -> CpuIdResult {
        if entries.is_empty() {
            return self.host(leaf, index);
        }
        configured(entries, leaf, index)
    }

    pub fn xsave(
        &mut self,
        entries: &[AxVCpuCpuidEntry],
        index: u32,
        xcr0: u64,
        xss: u64,
    ) -> CpuIdResult {
        let mut value = self.query(entries, 0xd, index);
        let supported = self.query(entries, 0xd, 0);
        // Match the backend's existing exclusion of AMX state.
        let user =
            (u64::from(supported.edx) << 32 | u64::from(supported.eax)) & !((1 << 17) | (1 << 18));
        match index {
            0 => {
                value.eax = user as u32;
                value.edx = (user >> 32) as u32;
                if user != 0 {
                    value.ebx = xsave_size(xcr0 & user, false, |i| self.query(entries, 0xd, i));
                    value.ecx = xsave_size(user, false, |i| self.query(entries, 0xd, i));
                }
            }
            1 if value.eax & ((1 << 1) | (1 << 3)) != 0 => {
                let supervisor = if value.eax & (1 << 3) != 0 {
                    xss & (u64::from(value.edx) << 32 | u64::from(value.ecx))
                } else {
                    0
                };
                value.ebx = xsave_size((xcr0 & user) | supervisor, true, |i| {
                    self.query(entries, 0xd, i)
                });
            }
            17 | 18 => value = ZERO,
            _ => {}
        }
        value
    }
}

fn configured(entries: &[AxVCpuCpuidEntry], leaf: u32, index: u32) -> CpuIdResult {
    if let Some(value) = entry(entries, leaf, index) {
        return value;
    }
    // Intel's out-of-range leaves alias the maximum basic leaf. Missing
    // in-range leaves/subleaves return zero, never leak physical capabilities.
    let class = if (0x4000_0000..0x5000_0000).contains(&leaf) {
        leaf & 0xffff_ff00
    } else {
        leaf & 0x8000_0000
    };
    let max = entry(entries, class, 0).map_or(0, |e| e.eax);
    if leaf > max
        && let Some(basic) = entry(entries, 0, 0)
    {
        // Only Intel has the maximum-basic-leaf fallback.
        if basic.ebx == 0x756e_6547 && basic.edx == 0x4965_6e69 && basic.ecx == 0x6c65_746e {
            return entry(entries, basic.eax, index).unwrap_or(ZERO);
        }
    }
    ZERO
}

fn xsave_size(mask: u64, compacted: bool, mut component: impl FnMut(u32) -> CpuIdResult) -> u32 {
    let mut size: u32 = 576; // Legacy region plus XSAVE header.
    for index in 2..64 {
        if mask & (1u64 << index) == 0 {
            continue;
        }
        let value = component(index);
        let offset = if compacted {
            if value.ecx & 2 != 0 {
                size.saturating_add(63) & !63
            } else {
                size
            }
        } else {
            value.ebx
        };
        size = size.max(offset.saturating_add(value.eax));
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_cache_invalidation_subleaves_and_bound() {
        let mut cache = GuestCpuidCache::new();
        cache.insert(7, 0, CpuIdResult { eax: 42, ..ZERO });
        cache.insert(7, 1, CpuIdResult { eax: 99, ..ZERO });
        assert_eq!(cache.get(7, 0).unwrap().eax, 42);
        assert_eq!(cache.get(7, 1).unwrap().eax, 99);
        assert!(cache.get(7, 2).is_none());
        cache.clear();
        assert!(cache.get(7, 0).is_none());
        cache.insert(7, 0, ZERO);
        assert_eq!(cache.get(7, 0).unwrap().eax, 0);
        cache.insert(0xdead, 0, ZERO);
        assert!(cache.get(0xdead, 0).is_none());
        for index in 1..100 {
            cache.insert(7, index, ZERO);
        }
        assert_eq!(cache.0.len(), 64);
    }

    #[test]
    fn matches_kvm_index_flags_and_missing_subleaf() {
        let table = [
            AxVCpuCpuidEntry {
                function: 0,
                eax: 0xd,
                ..Default::default()
            },
            AxVCpuCpuidEntry {
                function: 1,
                eax: 42,
                ..Default::default()
            },
            AxVCpuCpuidEntry {
                function: 4,
                index: 1,
                flags: 1,
                eax: 99,
                ..Default::default()
            },
        ];
        assert_eq!(configured(&table, 1, 0xdead).eax, 42);
        assert_eq!(configured(&table, 4, 1).eax, 99);
        assert_eq!(configured(&table, 4, 0).eax, 0);
        assert_eq!(configured(&table, 7, 0).eax, 0);
    }

    #[test]
    fn standard_and_compacted_xsave_layouts() {
        let component = |index| match index {
            2 => CpuIdResult {
                eax: 256,
                ebx: 576,
                ..ZERO
            },
            5 => CpuIdResult {
                eax: 33,
                ebx: 1088,
                ecx: 2,
                ..ZERO
            },
            6 => CpuIdResult {
                eax: 512,
                ebx: 1152,
                ecx: 2,
                ..ZERO
            },
            _ => panic!("unexpected component"),
        };
        assert_eq!(xsave_size(3, false, component), 576);
        assert_eq!(xsave_size(7, false, component), 832);
        assert_eq!(xsave_size(0x67, false, component), 1664);
        assert_eq!(xsave_size(0x67, true, component), 1408);
    }
}
