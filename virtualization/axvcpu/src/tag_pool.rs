//! Bounded VM-scoped address-space tags with active-context reservations.
//! The caller serializes this pool. A recycled tag has a new incarnation;
//! every CPU must invalidate that tag before using the new incarnation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    pub id: usize,
    pub incarnation: u64,
}
#[derive(Clone, Copy)]
struct Slot {
    owner: Option<(usize, usize)>,
    incarnation: u64,
    active: usize,
}
const EMPTY: Slot = Slot {
    owner: None,
    incarnation: 0,
    active: 0,
};
/// Tag zero is the untagged fallback. Inactive slots can be recycled without
/// stealing a tag from a CPU still executing/decoding in the old context.
pub struct TagPool<const N: usize> {
    slots: [Slot; N],
    cursor: usize,
    serial: u64,
}
impl<const N: usize> Default for TagPool<N> {
    fn default() -> Self {
        Self::new()
    }
}
impl<const N: usize> TagPool<N> {
    pub const fn new() -> Self {
        assert!(N > 1);
        Self {
            slots: [EMPTY; N],
            cursor: 1,
            serial: 0,
        }
    }
    pub fn acquire(&mut self, vm: usize, root: usize, maximum: usize) -> Option<Tag> {
        let limit = maximum.min(N - 1);
        if limit == 0 {
            return None;
        }
        let owner = Some((vm, root));
        for id in 1..=limit {
            if self.slots[id].owner == owner {
                self.slots[id].active = self.slots[id].active.checked_add(1)?;
                return Some(Tag {
                    id,
                    incarnation: self.slots[id].incarnation,
                });
            }
        }
        for offset in 0..limit {
            let id = (self.cursor - 1 + offset) % limit + 1;
            if self.slots[id].active == 0 {
                self.serial = self.serial.checked_add(1)?;
                self.slots[id] = Slot {
                    owner,
                    incarnation: self.serial,
                    active: 1,
                };
                self.cursor = id % limit + 1;
                return Some(Tag {
                    id,
                    incarnation: self.serial,
                });
            }
        }
        None
    }
    pub fn release(&mut self, tag: Tag) {
        let slot = &mut self.slots[tag.id];
        assert_eq!(slot.incarnation, tag.incarnation);
        assert!(slot.active != 0);
        slot.active -= 1;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_vm_shares_a_tag_across_live_vcpus() {
        let mut p = TagPool::<4>::new();
        let a = p.acquire(1, 0x1000, 3).unwrap();
        let b = p.acquire(1, 0x1000, 3).unwrap();
        assert_eq!(a, b);
        p.release(a);
        assert!(p.acquire(2, 0x2000, 1).is_none());
        p.release(b);
        let c = p.acquire(2, 0x2000, 1).unwrap();
        assert_eq!(a.id, c.id);
        assert_ne!(a.incarnation, c.incarnation);
    }
    #[test]
    fn rollover_preserves_active_contexts() {
        let mut p = TagPool::<4>::new();
        let active = p.acquire(1, 1, 3).unwrap();
        let mut recycled = 0;
        for vm in 2..1000 {
            let tag = p.acquire(vm, vm, 3).unwrap();
            assert_ne!(tag.id, active.id);
            assert!(tag.incarnation > recycled);
            recycled = tag.incarnation;
            p.release(tag);
        }
        assert_eq!(p.acquire(1, 1, 3), Some(active));
    }
    #[test]
    fn no_tag_and_exhaustion_are_explicit() {
        let mut p = TagPool::<3>::new();
        assert!(p.acquire(1, 1, 0).is_none());
        let a = p.acquire(1, 1, 2).unwrap();
        let b = p.acquire(2, 2, 2).unwrap();
        assert!(p.acquire(3, 3, 2).is_none());
        p.release(a);
        p.release(b);
        assert!(p.acquire(3, 3, 2).is_some());
    }
    #[test]
    fn root_replacement_and_generation_overflow_do_not_alias() {
        let mut p = TagPool::<2>::new();
        let a = p.acquire(1, 1, 1).unwrap();
        p.release(a);
        let b = p.acquire(1, 2, 1).unwrap();
        p.release(b);
        assert_ne!(a.incarnation, b.incarnation);
        p.serial = u64::MAX;
        assert!(p.acquire(2, 2, 1).is_none());
    }
}
