//! Per-vCPU publication handshake. Exit closes admission before draining senders.
use core::sync::atomic::{AtomicU8, Ordering};

const RUNNING: u8 = 1;
const POSTING: u8 = 2;

pub(super) struct Publication(AtomicU8);

impl Publication {
    pub(super) const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    pub(super) fn enter(&self) {
        self.0.store(RUNNING, Ordering::Release);
    }

    pub(super) fn try_post(&self) -> Option<Posting<'_>> {
        self.0
            .compare_exchange(
                RUNNING,
                RUNNING | POSTING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()
            .map(|_| Posting(self))
    }

    pub(super) fn exit(&self) {
        // Clearing RUNNING prevents new senders while the current sender
        // finishes publishing PIR and writing the notification ICR.
        let previous = self.0.fetch_and(!RUNNING, Ordering::AcqRel);
        if previous & POSTING != 0 {
            while self.0.load(Ordering::Acquire) & POSTING != 0 {
                core::hint::spin_loop();
            }
        }
    }
}

pub(super) struct Posting<'a>(&'a Publication);

impl Drop for Posting<'_> {
    fn drop(&mut self) {
        self.0.0.fetch_and(!POSTING, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn exit_waits_for_sender_and_closes_admission() {
        let p = std::sync::Arc::new(Publication::new());
        assert!(p.try_post().is_none());
        p.enter();
        let posting = p.try_post().unwrap();
        let exit = p.clone();
        let thread = std::thread::spawn(move || exit.exit());
        while p.0.load(Ordering::Acquire) & RUNNING != 0 {
            std::thread::yield_now();
        }
        assert!(p.try_post().is_none());
        drop(posting);
        thread.join().unwrap();
        assert_eq!(p.0.load(Ordering::Acquire), 0);
        p.enter();
        assert!(p.try_post().is_some());
        p.exit();
    }
}
