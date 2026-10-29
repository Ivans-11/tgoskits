//! Time management.
//!
//! Currently, the TSC is used as the clock source.

use ax_int_ratio::Ratio;
use ax_plat::time::TimeIf;
use raw_cpuid::CpuId;

#[cfg(feature = "irq")]
static mut NANOS_TO_LAPIC_TICKS_RATIO: Ratio = Ratio::zero();

static mut INIT_TICK: u64 = 0;
static mut TSC_FREQ_HZ: u64 = crate::config::devices::TIMER_FREQUENCY as u64;
static mut TICKS_TO_NANOS_RATIO: Ratio = Ratio::zero();
static mut NANOS_TO_TICKS_RATIO: Ratio = Ratio::zero();

/// RTC wall time offset in nanoseconds at monotonic time base.
static mut RTC_EPOCHOFFSET_NANOS: u64 = 0;

pub fn init_early() {
    if let Some(freq) = CpuId::new()
        .get_tsc_info()
        .and_then(|info| info.tsc_frequency())
    {
        unsafe { TSC_FREQ_HZ = freq }
    } else if let Some(freq) = CpuId::new()
        .get_processor_frequency_info()
        .map(|info| info.processor_base_frequency())
        && freq > 0
    {
        unsafe { TSC_FREQ_HZ = u64::from(freq) * 1_000_000 }
    }

    ax_plat::console_println!("TSC frequency: {} Hz", unsafe { TSC_FREQ_HZ });

    unsafe {
        // kHz preserves sub-MHz TSC rates and fits the conversion ratios.
        let khz = u32::try_from(TSC_FREQ_HZ / 1_000).expect("TSC frequency too high");
        assert!(khz > 0, "TSC frequency unavailable");
        TICKS_TO_NANOS_RATIO = Ratio::new(1_000_000, khz);
        NANOS_TO_TICKS_RATIO = Ratio::new(khz, 1_000_000);
        INIT_TICK = core::arch::x86_64::_rdtsc();
    }

    #[cfg(feature = "rtc")]
    {
        use x86_rtc::Rtc;

        // Get the current time in microseconds since the epoch (1970-01-01) from the x86 RTC.
        // Subtract the timer ticks to get the actual time when ArceOS was booted.
        let eopch_time_nanos = Rtc::new().get_unix_timestamp() * 1_000_000_000;
        unsafe {
            RTC_EPOCHOFFSET_NANOS = eopch_time_nanos - ax_plat::time::ticks_to_nanos(INIT_TICK);
        }
    }
}

pub fn init_primary() {
    #[cfg(feature = "irq")]
    unsafe {
        use x2apic::lapic::{TimerDivide, TimerMode};
        let lapic = super::apic::local_apic();
        lapic.set_timer_mode(TimerMode::OneShot);
        lapic.set_timer_divide(TimerDivide::Div1);
        // Calibrate the physical countdown against the TSC before starting APs.
        // Masking the interrupt does not stop the LAPIC counter.
        lapic.disable_timer();
        lapic.set_timer_initial(u32::MAX);
        let start = core::arch::x86_64::_rdtsc();
        let initial = lapic.timer_current();
        let interval = TSC_FREQ_HZ / 100;
        while core::arch::x86_64::_rdtsc().wrapping_sub(start) < interval {
            core::hint::spin_loop();
        }
        let remaining = lapic.timer_current();
        let elapsed = core::arch::x86_64::_rdtsc().wrapping_sub(start);
        lapic.set_timer_initial(0);
        let ticks = initial - remaining;
        let nanos = (u128::from(elapsed) * 1_000_000_000 / u128::from(TSC_FREQ_HZ)) as u64;
        assert!(
            ticks > 0 && remaining > 0,
            "LAPIC calibration counter did not run"
        );
        NANOS_TO_LAPIC_TICKS_RATIO = Ratio::new(
            ticks,
            u32::try_from(nanos).expect("LAPIC calibration exceeded four seconds"),
        );
        ax_plat::console_println!(
            "LAPIC timer frequency: {} Hz",
            u64::from(ticks) * 1_000_000_000 / nanos
        );
        lapic.enable_timer();
    }
}

#[cfg(feature = "smp")]
pub fn init_secondary() {
    #[cfg(feature = "irq")]
    unsafe {
        use x2apic::lapic::{TimerDivide, TimerMode};
        let lapic = crate::apic::local_apic();
        lapic.set_timer_mode(TimerMode::OneShot);
        lapic.set_timer_divide(TimerDivide::Div1);
        lapic.set_timer_initial(0);
        lapic.enable_timer();
    }
}

struct TimeIfImpl;

#[impl_plat_interface]
impl TimeIf for TimeIfImpl {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64 {
        unsafe { core::arch::x86_64::_rdtsc() - INIT_TICK }
    }

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64 {
        unsafe { TICKS_TO_NANOS_RATIO }.mul_trunc(ticks)
    }

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64 {
        unsafe { NANOS_TO_TICKS_RATIO }.mul_trunc(nanos)
    }

    /// Return epoch offset in nanoseconds (wall time offset to monotonic
    /// clock start).
    fn epochoffset_nanos() -> u64 {
        unsafe { RTC_EPOCHOFFSET_NANOS }
    }

    /// Returns the IRQ number for the timer interrupt.
    #[cfg(feature = "irq")]
    fn irq_num() -> usize {
        crate::config::devices::TIMER_IRQ
    }

    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time
    /// deadline (in nanoseconds).
    #[cfg(feature = "irq")]
    fn set_oneshot_timer(deadline_ns: u64) {
        let lapic = super::apic::local_apic();
        let now_ns = Self::ticks_to_nanos(Self::current_ticks());
        unsafe {
            if now_ns < deadline_ns {
                let apic_ticks = NANOS_TO_LAPIC_TICKS_RATIO.mul_trunc(deadline_ns - now_ns);
                lapic.set_timer_initial(apic_ticks.clamp(1, u32::MAX as u64) as u32);
            } else {
                lapic.set_timer_initial(1);
            }
        }
    }
}
