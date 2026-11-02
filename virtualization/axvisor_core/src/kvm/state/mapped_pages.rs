//! Bound each page-record allocation independently of guest RAM size.
//!
//! Page faults hold the control-state lock with IRQs disabled. A single large
//! hash table can exceed the host's atomic allocation limit during rehashing.
//! Partition by 4 MiB GPA ranges: each inner table holds at most 1024 pages.

use alloc::collections::BTreeMap;

use hashbrown::HashMap;

const RANGE_SHIFT: u32 = 22;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct MappedPages<V> {
    ranges: BTreeMap<u64, HashMap<u64, V>>,
}

impl<V> Default for MappedPages<V> {
    fn default() -> Self {
        Self {
            ranges: BTreeMap::new(),
        }
    }
}

impl<V> MappedPages<V> {
    pub(in crate::kvm) fn get(&self, gpa: &u64) -> Option<&V> {
        self.ranges.get(&(gpa >> RANGE_SHIFT))?.get(gpa)
    }

    pub(in crate::kvm) fn contains_key(&self, gpa: &u64) -> bool {
        self.get(gpa).is_some()
    }

    pub(in crate::kvm) fn insert(&mut self, gpa: u64, value: V) -> Option<V> {
        debug_assert_eq!(gpa & 0xfff, 0);
        self.ranges
            .entry(gpa >> RANGE_SHIFT)
            .or_default()
            .insert(gpa, value)
    }

    pub(in crate::kvm) fn remove(&mut self, gpa: &u64) -> Option<V> {
        let key = gpa >> RANGE_SHIFT;
        let range = self.ranges.get_mut(&key)?;
        let value = range.remove(gpa);
        if range.is_empty() {
            self.ranges.remove(&key);
        }
        value
    }
}

impl<V> IntoIterator for MappedPages<V> {
    type Item = (u64, V);
    type IntoIter =
        core::iter::Flatten<alloc::collections::btree_map::IntoValues<u64, HashMap<u64, V>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.ranges.into_values().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_guest_replacement_removal_and_teardown() {
        let mut pages = MappedPages::default();
        // 1 GiB touched RAM crosses the former contiguous-table limit.
        for page in 0..262144u64 {
            assert_eq!(pages.insert(page << 12, page), None);
        }
        assert_eq!(pages.ranges.len(), 256);
        assert!(pages.ranges.values().all(|range| range.len() == 1024));
        assert_eq!(pages.insert(0x400000, 7), Some(1024));
        assert_eq!(pages.get(&0x400000), Some(&7));
        assert_eq!(pages.remove(&0x400000), Some(7));
        assert!(!pages.contains_key(&0x400000));
        assert_eq!(pages.into_iter().count(), 262143);
    }
}
