#![no_std]

//! Software AIA interrupt state. Synchronization belongs to the caller.
//! Register behavior follows the AIA specification; hardware backing is separate.

pub const MAX_IDS: usize = 2047;

/// APLIC MSI-mode source state. The caller forwards returned MSI targets.
#[derive(Clone, Debug)]
pub struct Aplic {
    sources: usize,
    config: [u32; 64],
    target: [u32; 64],
    pending: u64,
    enabled: u64,
    input: u64,
    delivery: bool,
}

impl Aplic {
    pub fn new(sources: usize) -> Option<Self> {
        if sources > 63 {
            return None;
        }
        Some(Self {
            sources,
            config: [0; 64],
            target: [0; 64],
            pending: 0,
            enabled: 0,
            input: 0,
            delivery: false,
        })
    }

    pub fn set_level(&mut self, source: usize, high: bool) -> bool {
        if source == 0 || source > self.sources {
            return false;
        }
        let bit = 1u64 << source;
        let old = self.input & bit != 0;
        if high {
            self.input |= bit;
        } else {
            self.input &= !bit;
        }
        match self.config[source] & 7 {
            4 if high && !old => self.pending |= bit,
            5 if !high && old => self.pending |= bit,
            6 if high => self.pending |= bit,
            7 if !high => self.pending |= bit,
            6 | 7 => self.pending &= !bit,
            _ => {}
        }
        true
    }

    pub fn drain(&mut self, mut send: impl FnMut(usize, usize)) {
        if !self.delivery {
            return;
        }
        let mut ready = self.pending & self.enabled & !1;
        while ready != 0 {
            let source = ready.trailing_zeros() as usize;
            let target = self.target[source];
            self.pending &= !(1u64 << source);
            ready &= ready - 1;
            send((target >> 18) as usize, (target & 0x7ff) as usize);
        }
    }

    pub fn read(&self, offset: usize) -> u32 {
        match offset {
            0 => 0x8000_0004 | (u32::from(self.delivery) << 8),
            4..=0xfc if offset / 4 <= self.sources => self.config[offset / 4],
            0x3004..=0x30fc if (offset - 0x3000) / 4 <= self.sources => {
                self.target[(offset - 0x3000) / 4]
            }
            0x1c00 | 0x1c04 => (self.pending >> ((offset - 0x1c00) * 8)) as u32,
            0x1d00 | 0x1d04 => (self.input >> ((offset - 0x1d00) * 8)) as u32,
            0x1e00 | 0x1e04 => (self.enabled >> ((offset - 0x1e00) * 8)) as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: usize, value: u32) {
        match offset {
            0 => self.delivery = value & 0x100 != 0,
            4..=0xfc if offset / 4 <= self.sources => {
                let source = offset / 4;
                self.config[source] = match value & 7 {
                    mode @ (1 | 4..=7) => mode,
                    _ => 0,
                };
                if self.config[source] == 0 {
                    self.pending &= !(1u64 << source);
                    self.enabled &= !(1u64 << source);
                }
            }
            0x3004..=0x30fc if (offset - 0x3000) / 4 <= self.sources => {
                self.target[(offset - 0x3000) / 4] = value & 0xfffc_07ff
            }
            0x1c00 | 0x1c04 => self.pending |= (value as u64) << ((offset - 0x1c00) * 8),
            0x1d00 | 0x1d04 => self.pending &= !((value as u64) << ((offset - 0x1d00) * 8)),
            0x1e00 | 0x1e04 => self.enabled |= (value as u64) << ((offset - 0x1e00) * 8),
            0x1f00 | 0x1f04 => self.enabled &= !((value as u64) << ((offset - 0x1f00) * 8)),
            0x1cdc | 0x2000 if value > 0 && value as usize <= self.sources => {
                self.pending |= 1u64 << value
            }
            0x2004 => self.write(0x2000, value.swap_bytes()),
            0x1ddc if value > 0 && value as usize <= self.sources => {
                self.pending &= !(1u64 << value)
            }
            0x1edc if value > 0 && value as usize <= self.sources => self.enabled |= 1u64 << value,
            0x1fdc if value > 0 && value as usize <= self.sources => {
                self.enabled &= !(1u64 << value)
            }
            _ => {}
        }
        let mask = u64::MAX >> (63 - self.sources);
        self.pending &= mask & !1;
        self.enabled &= mask & !1;
    }
}

#[derive(Clone, Debug)]
pub struct Imsic {
    ids: usize,
    pending: [u64; 32],
    enabled: [u64; 32],
    delivery: bool,
    threshold: usize,
}

impl Imsic {
    pub fn new(ids: usize) -> Option<Self> {
        if !(63..=MAX_IDS).contains(&ids) || ids & 63 != 63 {
            return None;
        }
        Some(Self {
            ids,
            pending: [0; 32],
            enabled: [0; 32],
            delivery: false,
            threshold: 0,
        })
    }

    pub fn inject(&mut self, id: usize) -> bool {
        if id == 0 || id > self.ids {
            return false;
        }
        self.pending[id / 64] |= 1 << (id % 64);
        true
    }

    pub fn top(&self) -> usize {
        for word in 0..=self.ids / 64 {
            let bits = self.pending[word] & self.enabled[word];
            if bits != 0 {
                let id = word * 64 + bits.trailing_zeros() as usize;
                if self.threshold == 0 || id < self.threshold {
                    return id;
                }
                return 0;
            }
        }
        0
    }

    pub fn external_pending(&self) -> bool {
        self.delivery && self.top() != 0
    }

    pub fn topei(&mut self, claim: bool) -> usize {
        let id = self.top();
        if claim && id != 0 {
            self.pending[id / 64] &= !(1 << (id % 64));
        }
        (id << 16) | id
    }

    pub fn read(&self, selector: usize) -> Option<usize> {
        match selector {
            0x70 => Some(usize::from(self.delivery)),
            0x72 => Some(self.threshold),
            0x80..=0xbf if selector & 1 == 0 && (selector - 0x80) / 2 <= self.ids / 64 => {
                Some(self.pending[(selector - 0x80) / 2] as usize)
            }
            0xc0..=0xff if selector & 1 == 0 && (selector - 0xc0) / 2 <= self.ids / 64 => {
                Some(self.enabled[(selector - 0xc0) / 2] as usize)
            }
            _ => None,
        }
    }

    pub fn write(&mut self, selector: usize, value: usize) -> bool {
        match selector {
            0x70 => self.delivery = value & 1 != 0,
            0x72 => self.threshold = value & 0x7ff,
            0x80..=0xbf if selector & 1 == 0 && (selector - 0x80) / 2 <= self.ids / 64 => {
                self.pending[(selector - 0x80) / 2] = value as u64
            }
            0xc0..=0xff if selector & 1 == 0 && (selector - 0xc0) / 2 <= self.ids / 64 => {
                self.enabled[(selector - 0xc0) / 2] = value as u64
            }
            _ => return false,
        }
        self.pending[0] &= !1;
        self.enabled[0] &= !1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_input_requires_a_complete_pulse() {
        let mut aplic = Aplic::new(63).unwrap();
        aplic.write(4, 4);
        aplic.write(0x3004, (1 << 18) | 7);
        aplic.write(0x1edc, 1);
        aplic.write(0, 0x104);
        let mut count = 0;
        for _ in 0..3 {
            assert!(aplic.set_level(1, true));
            aplic.drain(|hart, id| {
                assert_eq!((hart, id), (1, 7));
                count += 1;
            });
            aplic.set_level(1, false);
        }
        assert_eq!(count, 3);
        assert!(!aplic.set_level(64, true));
    }

    #[test]
    fn masked_interrupt_survives_until_enabled_and_claimed() {
        let mut file = Imsic::new(63).unwrap();
        assert!(file.inject(7));
        assert!(!file.external_pending());
        assert!(file.write(0xc0, 1 << 7));
        assert!(!file.external_pending());
        file.write(0x70, 1);
        assert!(file.external_pending());
        assert_eq!(file.topei(false), (7 << 16) | 7);
        assert_eq!(file.topei(true), (7 << 16) | 7);
        assert!(!file.external_pending());
        file.inject(7);
        assert!(file.external_pending());
    }

    #[test]
    fn priority_threshold_and_identity_zero() {
        let mut file = Imsic::new(127).unwrap();
        assert!(!file.inject(0));
        assert!(!file.inject(128));
        file.write(0xc0, usize::MAX);
        file.write(0xc2, usize::MAX);
        file.inject(70);
        file.inject(3);
        assert_eq!(file.top(), 3);
        file.write(0x72, 3);
        assert_eq!(file.top(), 0);
        file.write(0x72, 4);
        assert_eq!(file.topei(true), (3 << 16) | 3);
        assert_eq!(file.top(), 0);
        file.write(0x72, 0);
        assert_eq!(file.top(), 70);
        assert_eq!(file.read(0xc0).unwrap() & 1, 0);
        assert_eq!(file.read(0x81), None);
    }
}
