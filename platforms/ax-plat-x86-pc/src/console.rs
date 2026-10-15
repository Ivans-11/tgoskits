use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::console::ConsoleIf;
use uart_16550::{Config, Uart16550, backend::PioBackend};

const COM1_PORT: u16 = 0x3F8;
// Early console output must not depend on a functioning legacy serial port.
const UART_POLL_LIMIT: usize = 100_000;

static UART: LazyInit<SpinNoIrq<Option<Uart16550<PioBackend>>>> = LazyInit::new();

pub(crate) fn init_early() {
    UART.init_once({
        // SAFETY: the platform owns the legacy COM1 I/O ports.
        let uart = unsafe { Uart16550::new_port(COM1_PORT) }
            .ok()
            .and_then(|mut uart| {
                uart.init(Config::default()).ok()?;
                uart.test_loopback().ok()?;
                Some(uart)
            });
        SpinNoIrq::new(uart)
    });
}

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    /// Writes bytes to the console from input u8 slice.
    fn write_bytes(bytes: &[u8]) {
        crate::screen::write(bytes);
        let mut guard = UART.lock();
        if let Some(uart) = guard.as_mut() {
            let mut remaining = bytes;
            let mut stalled = 0;
            while !remaining.is_empty() {
                match uart.try_send_bytes(remaining) {
                    Ok(sent) if sent > 0 => {
                        remaining = &remaining[sent..];
                        stalled = 0;
                    }
                    _ => {
                        stalled += 1;
                        if stalled == UART_POLL_LIMIT {
                            *guard = None;
                            break;
                        }
                        core::hint::spin_loop();
                    }
                }
            }
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut uart = UART.lock();
        uart.as_mut()
            .map_or(0, |uart| uart.try_receive_bytes(bytes))
    }

    /// Returns the IRQ number for the console, if applicable.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        None
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(_enabled: bool) {}

    #[cfg(feature = "irq")]
    fn handle_irq() -> ax_plat::console::ConsoleIrqEvent {
        ax_plat::console::ConsoleIrqEvent::empty()
    }
}
