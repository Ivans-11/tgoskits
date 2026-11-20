//! CMOS RAM and RTC calendar shared by x86 control frontends.
//! Alarm and periodic interrupt generation are not implemented.

use ax_errno::{AxError, AxResult};
use chrono::{DateTime, Datelike, Timelike, Utc};

/// MC146818-compatible CMOS index/data registers and wall clock.
pub struct Rtc {
    nvram: [u8; 128],
    index: usize,
    epoch: i64,
    epoch_ns: u64,
}

impl Default for Rtc {
    fn default() -> Self {
        let mut nvram = [0; 128];
        nvram[10] = 0x26;
        nvram[11] = 2; // 24-hour BCD.
        nvram[13] = 0x80; // Valid RAM/time.
        Self {
            nvram,
            index: 0,
            epoch: 0,
            epoch_ns: 0,
        }
    }
}

impl Rtc {
    /// Set the wall clock reference against the host monotonic clock.
    pub fn set_time(&mut self, seconds: i64, now_ns: u64) -> AxResult {
        DateTime::from_timestamp(seconds, 0).ok_or(AxError::InvalidInput)?;
        self.epoch = seconds;
        self.epoch_ns = now_ns;
        Ok(())
    }

    /// Current wall clock in seconds from the Unix epoch.
    pub fn time(&self, now_ns: u64) -> i64 {
        self.epoch
            .saturating_add((now_ns.saturating_sub(self.epoch_ns) / 1_000_000_000) as i64)
    }

    /// Read one CMOS byte, with calendar fields derived from the same clock.
    pub fn read(&self, index: usize, now_ns: u64) -> AxResult<u8> {
        if index >= 128 {
            return Err(AxError::InvalidInput);
        }
        if matches!(index, 0 | 2 | 4 | 6..=9 | 0x32) && self.nvram[11] & 0x80 == 0 {
            let date: DateTime<Utc> =
                DateTime::from_timestamp(self.time(now_ns), 0).ok_or(AxError::InvalidInput)?;
            let value = match index {
                0 => date.second(),
                2 => date.minute(),
                4 => date.hour(),
                6 => date.weekday().num_days_from_sunday() + 1,
                7 => date.day(),
                8 => date.month(),
                9 => date.year().rem_euclid(100) as u32,
                _ => date.year().div_euclid(100) as u32,
            } as u8;
            let binary = self.nvram[11] & 4 != 0;
            if index == 4 && self.nvram[11] & 2 == 0 {
                let hour = (value + 11) % 12 + 1;
                return Ok(Self::encode(hour, binary) | if value >= 12 { 0x80 } else { 0 });
            }
            return Ok(Self::encode(value, binary));
        }
        Ok(self.nvram[index])
    }

    fn encode(value: u8, binary: bool) -> u8 {
        if binary {
            value
        } else {
            (value / 10) << 4 | (value % 10)
        }
    }

    /// Write CMOS RAM or RTC mode. Unsupported IRQ modes fail explicitly.
    pub fn write(&mut self, index: usize, value: u8, now_ns: u64) -> AxResult {
        if index >= 128 {
            return Err(AxError::InvalidInput);
        }
        if index == 11 {
            if value & 0x70 != 0 {
                return Err(AxError::Unsupported);
            }
            if value & 0x80 != 0 && self.nvram[11] & 0x80 == 0 {
                for i in [0, 2, 4, 6, 7, 8, 9, 0x32] {
                    self.nvram[i] = self.read(i, now_ns)?;
                }
            } else if value & 0x80 == 0 && self.nvram[11] & 0x80 != 0 {
                let binary = self.nvram[11] & 4 != 0;
                let decode = |v: u8| {
                    if binary {
                        u32::from(v)
                    } else {
                        u32::from(v >> 4) * 10 + u32::from(v & 15)
                    }
                };
                let hour = self.nvram[4];
                let hour = if self.nvram[11] & 2 != 0 {
                    decode(hour)
                } else {
                    decode(hour & 0x7f) % 12 + if hour & 0x80 != 0 { 12 } else { 0 }
                };
                let year = decode(self.nvram[0x32]) * 100 + decode(self.nvram[9]);
                let date = chrono::NaiveDate::from_ymd_opt(
                    year as i32,
                    decode(self.nvram[8]),
                    decode(self.nvram[7]),
                )
                .and_then(|d| d.and_hms_opt(hour, decode(self.nvram[2]), decode(self.nvram[0])))
                .ok_or(AxError::InvalidInput)?;
                self.set_time(date.and_utc().timestamp(), now_ns)?;
            }
        }
        if !matches!(index, 12 | 13) {
            self.nvram[index] = if index == 10 { value & 0x7f } else { value };
        }
        Ok(())
    }

    /// Read CMOS index (0x70) or data (0x71).
    pub fn read_port(&self, port: u16, now_ns: u64) -> AxResult<u8> {
        match port {
            0x70 => Ok(self.index as u8),
            0x71 => self.read(self.index, now_ns),
            _ => Err(AxError::InvalidInput),
        }
    }

    /// Write CMOS index (0x70) or data (0x71).
    pub fn write_port(&mut self, port: u16, value: u8, now_ns: u64) -> AxResult {
        match port {
            0x70 => {
                self.index = usize::from(value & 0x7f);
                Ok(())
            }
            0x71 => self.write(self.index, value, now_ns),
            _ => Err(AxError::InvalidInput),
        }
    }
}
