//! Hardware PI descriptor layout and atomic publication protocol.

use core::sync::atomic::{AtomicU64, Ordering};

const ON: u64 = 1;

#[repr(C, align(64))]
pub(super) struct Descriptor {
    pir: [AtomicU64; 4],
    control: AtomicU64,
    reserved: [u64; 3],
}

impl Descriptor {
    pub(super) const fn new(vector: u8) -> Self {
        Self {
            pir: [const { AtomicU64::new(0) }; 4],
            control: AtomicU64::new((vector as u64) << 16),
            reserved: [0; 3],
        }
    }

    /// Publish PIR before ON; true means a notification must be sent.
    pub(super) fn post(&self, vector: u8) -> bool {
        let vector = vector as usize;
        self.pir[vector / 64].fetch_or(1u64 << (vector % 64), Ordering::SeqCst);
        self.control.fetch_or(ON, Ordering::SeqCst) & ON == 0
    }

    /// Called after hardware execution and senders have both stopped.
    pub(super) fn reclaim(&self) -> [u64; 4] {
        self.control.fetch_and(!ON, Ordering::SeqCst);
        core::array::from_fn(|i| self.pir[i].swap(0, Ordering::SeqCst))
    }
}

const _: () = assert!(core::mem::size_of::<Descriptor>() == 64);

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn descriptor_layout_and_notification_coalescing() {
        assert_eq!(core::mem::align_of::<Descriptor>(), 64);
        assert_eq!(core::mem::offset_of!(Descriptor, control), 32);
        let d = Descriptor::new(0xf2);
        assert!(d.post(69));
        assert!(!d.post(70));
        assert!(!d.post(69));
        assert_eq!(d.reclaim(), [0, (1 << 5) | (1 << 6), 0, 0]);
        assert_eq!(d.control.load(Ordering::SeqCst), 0xf2 << 16);
        assert!(d.post(69));
    }

    #[test]
    fn new_event_notifies_after_hardware_clears_on() {
        let d = Descriptor::new(0xf2);
        assert!(d.post(69));
        // Hardware clears ON before consuming PIR. A repeated vector during
        // that window must still set ON and send another notification.
        d.control.fetch_and(!ON, Ordering::SeqCst);
        assert!(d.post(69));
        assert_eq!(d.reclaim(), [0, 1 << 5, 0, 0]);
    }

    #[test]
    fn concurrent_senders_preserve_every_vector() {
        let d = std::sync::Arc::new(Descriptor::new(0xf2));
        let threads: std::vec::Vec<_> = (0..4)
            .map(|word| {
                let d = d.clone();
                std::thread::spawn(move || {
                    for bit in 0..64 {
                        d.post((word * 64 + bit) as u8);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(d.reclaim(), [u64::MAX; 4]);
    }
}
