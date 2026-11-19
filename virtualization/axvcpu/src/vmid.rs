//! Shared tag allocation policy; ISA backends own register and fence operations.
use ax_kspin::SpinNoIrq;

use crate::tag_pool::{Tag, TagPool};
// ARM's baseline VMID field has eight bits. Wider ISA tag spaces can be used
// later without changing the protocol; exhaustion uses uncached VMID zero.
static POOL: SpinNoIrq<TagPool<256>> = SpinNoIrq::new(TagPool::new());
/// Holds the tag reservation through bind, all guest accesses, and unbind.
#[derive(Debug)]
pub struct Lease(pub Tag);
impl Lease {
    pub fn acquire(vm: usize, root: usize, maximum: usize) -> Option<Self> {
        POOL.lock().acquire(vm, root, maximum).map(Self)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        POOL.lock().release(self.0);
    }
}
