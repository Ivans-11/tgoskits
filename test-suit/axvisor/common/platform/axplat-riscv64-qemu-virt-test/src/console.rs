use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::console::ConsoleIf;
use uart_16550::{Config, Uart16550, backend::MmioBackend};

use crate::config::{devices::UART_PADDR, plat::PHYS_VIRT_OFFSET};

static UART: LazyInit<SpinNoIrq<Uart16550<MmioBackend>>> = LazyInit::new();

pub(crate) fn init_early() {
    UART.init_once({
        let mut uart =
            unsafe { Uart16550::new_mmio((UART_PADDR + PHYS_VIRT_OFFSET) as *mut u8, 1) }.unwrap();
        // Test guests still use the UART path, but they may share the
        // passthrough UART with the host shell, so skip the intrusive
        // loopback self-test here.
        uart.init(Config::default())
            .expect("Failed to initialize UART");
        SpinNoIrq::new(uart)
    });
}

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    fn write_bytes(bytes: &[u8]) {
        for &c in bytes {
            let mut uart = UART.lock();
            match c {
                b'\n' => uart.send_bytes_exact(b"\r\n"),
                c => uart.send_bytes_exact(&[c]),
            }
        }
    }

    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut uart = UART.lock();
        uart.try_receive_bytes(bytes)
    }

    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        Some(crate::config::devices::UART_IRQ)
    }
}
