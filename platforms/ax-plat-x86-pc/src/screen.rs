//! Allocation-free Multiboot RGB framebuffer console.

use ax_kspin::SpinNoIrq;
use font8x8::legacy::BASIC_LEGACY;
use multiboot::information::{ColorInfoType, FramebufferTable};

use crate::config::plat::PHYS_VIRT_OFFSET;

const CELL_WIDTH: usize = 8;
const CELL_HEIGHT: usize = 16;
const MAX_COLUMNS: usize = 256;
const MAX_ROWS: usize = 128;
static SCREEN: SpinNoIrq<Option<Screen>> = SpinNoIrq::new(None);

struct Screen {
    address: usize,
    pitch: usize,
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    x: usize,
    y: usize,
    escape: bool,
    cells: [[u8; MAX_COLUMNS]; MAX_ROWS],
}

/// Initializes output and returns the page-aligned physical mapping it needs.
pub(crate) fn init(table: &FramebufferTable) -> Option<(usize, usize)> {
    if !matches!(table.bpp, 24 | 32) || !matches!(table.color_info(), Some(ColorInfoType::Rgb(_))) {
        return None;
    }
    init_rgb(
        table.addr,
        table.pitch,
        table.width,
        table.height,
        table.bpp,
    )
}

pub(crate) fn init_rgb(
    addr: u64,
    pitch: u32,
    width: u32,
    height: u32,
    bpp: u8,
) -> Option<(usize, usize)> {
    if !matches!(bpp, 24 | 32) {
        return None;
    }
    let address = usize::try_from(addr).ok()?;
    let pitch = pitch as usize;
    let width = width as usize;
    let height = height as usize;
    let end = address.checked_add(pitch.checked_mul(height)?)?;
    // The bootstrap page tables cover the first 512 GiB.
    let bytes_per_pixel = usize::from(bpp / 8);
    if address == 0
        || width < CELL_WIDTH
        || height < CELL_HEIGHT
        || pitch < width.checked_mul(bytes_per_pixel)?
        || end > 512 * 1024 * 1024 * 1024
    {
        return None;
    }
    let mut screen = Screen {
        address: address + PHYS_VIRT_OFFSET,
        pitch,
        // Bound the early console's RAM shadow without requiring an allocator.
        width: width.min(MAX_COLUMNS * CELL_WIDTH),
        height: (height / CELL_HEIGHT).min(MAX_ROWS) * CELL_HEIGHT,
        bytes_per_pixel,
        x: 0,
        y: 0,
        escape: false,
        cells: [[b' '; MAX_COLUMNS]; MAX_ROWS],
    };
    screen.clear_rows(0, screen.height);
    *SCREEN.lock() = Some(screen);
    let start = address & !4095;
    Some((start, (end.checked_add(4095)? & !4095) - start))
}

pub(crate) fn write(bytes: &[u8]) {
    // Avoid a recursive panic deadlock if rendering itself faults.
    let Some(mut guard) = SCREEN.try_lock() else {
        return;
    };
    if let Some(screen) = guard.as_mut() {
        for &byte in bytes {
            screen.put_byte(byte);
        }
    }
}

impl Screen {
    fn pixel(&self, x: usize, y: usize) -> *mut u8 {
        (self.address + y * self.pitch + x * self.bytes_per_pixel) as *mut u8
    }

    fn clear_rows(&mut self, start: usize, end: usize) {
        for y in start..end {
            for x in 0..self.width {
                // SAFETY: dimensions and mapping bounds were checked at initialization;
                // the platform retains this mapping and serializes access with SCREEN.
                for byte in 0..self.bytes_per_pixel {
                    unsafe { self.pixel(x, y).add(byte).write_volatile(0) };
                }
            }
        }
    }

    fn newline(&mut self) {
        self.x = 0;
        self.y += CELL_HEIGHT;
        if self.y + CELL_HEIGHT <= self.height {
            return;
        }
        let rows = self.height / CELL_HEIGHT;
        for row in 0..rows {
            for col in 0..self.width / CELL_WIDTH {
                let next = if row + 1 < rows {
                    self.cells[row + 1][col]
                } else {
                    b' '
                };
                if self.cells[row][col] != next {
                    self.draw_cell(col, row, next);
                }
            }
        }
        self.y -= CELL_HEIGHT;
    }

    fn draw_cell(&mut self, col: usize, row: usize, byte: u8) {
        let previous = self.cells[row][col];
        for y in 0..CELL_HEIGHT {
            let old_bits = BASIC_LEGACY[previous as usize][y / 2];
            let bits = BASIC_LEGACY[byte as usize][y / 2];
            for x in 0..CELL_WIDTH {
                if (old_bits ^ bits) & (1 << x) == 0 {
                    continue;
                }
                let color = if bits & (1 << x) != 0 { 0xff } else { 0 };
                for channel in 0..self.bytes_per_pixel {
                    // SAFETY: callers use cells inside the validated viewport.
                    unsafe {
                        self.pixel(col * CELL_WIDTH + x, row * CELL_HEIGHT + y)
                            .add(channel)
                            .write_volatile(color);
                    }
                }
            }
        }
        self.cells[row][col] = byte;
    }

    fn put_byte(&mut self, byte: u8) {
        // Strip CSI sequences (including logger colours); preserve state across writes.
        if self.escape {
            if (0x40..=0x7e).contains(&byte) && byte != b'[' {
                self.escape = false;
            }
            return;
        }
        match byte {
            0x1b => self.escape = true,
            b'\n' => self.newline(),
            b'\r' => self.x = 0,
            b'\t' => {
                for _ in 0..4 {
                    self.put_byte(b' ');
                }
            }
            8 | 127 => {
                self.x = self.x.saturating_sub(CELL_WIDTH);
            }
            0x20..=0x7e => {
                if self.x + CELL_WIDTH > self.width {
                    self.newline();
                }
                self.draw_cell(self.x / CELL_WIDTH, self.y / CELL_HEIGHT, byte);
                self.x += CELL_WIDTH;
            }
            _ => {}
        }
    }
}
