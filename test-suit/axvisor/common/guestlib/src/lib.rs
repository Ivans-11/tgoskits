#![cfg_attr(feature = "ax-std", no_std)]

#[cfg(all(target_arch = "riscv64", feature = "riscv64-qemu-virt-test"))]
extern crate ax_plat_riscv64_qemu_virt_test;
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use core::fmt;

pub const RESULT_BEGIN_MARKER: &str = "AXTEST_RESULT_BEGIN";
pub const RESULT_END_MARKER: &str = "AXTEST_RESULT_END";

/// Emit one structured result record for the host-side runner.
///
/// The record is framed by fixed begin/end markers so the runner can extract it
/// from mixed console output. `details_json` is embedded as raw JSON and should
/// therefore already be a valid JSON object/array/value.
pub fn emit_case_result(
    case_id: &str,
    status: &str,
    message: Option<&str>,
    details_json: Option<&str>,
) {
    #[cfg(feature = "ax-std")]
    {
        use std::os::arceos::api;

        let record = CaseResultRecord {
            case_id,
            status,
            message,
            details_json,
        };
        api::stdio::ax_console_write_fmt(format_args!(
            "{RESULT_BEGIN_MARKER}\n{record}\n{RESULT_END_MARKER}\n"
        ))
        .unwrap();
    }

    #[cfg(not(feature = "ax-std"))]
    {
        let _ = (case_id, status, message, details_json);
    }
}

#[cfg(feature = "ax-std")]
struct CaseResultRecord<'a> {
    case_id: &'a str,
    status: &'a str,
    message: Option<&'a str>,
    details_json: Option<&'a str>,
}

#[cfg(feature = "ax-std")]
impl fmt::Display for CaseResultRecord<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{\"case_id\":")?;
        JsonEscaped(self.case_id).fmt(f)?;
        f.write_str(",\"status\":")?;
        JsonEscaped(self.status).fmt(f)?;

        if let Some(message) = self.message {
            f.write_str(",\"message\":")?;
            JsonEscaped(message).fmt(f)?;
        }
        if let Some(details_json) = self.details_json {
            f.write_str(",\"details\":")?;
            f.write_str(details_json)?;
        }

        f.write_str("}")
    }
}

#[cfg(feature = "ax-std")]
struct JsonEscaped<'a>(&'a str);

#[cfg(feature = "ax-std")]
impl fmt::Display for JsonEscaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for ch in self.0.chars() {
            match ch {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                ch if ch.is_control() => {
                    write!(f, "\\u{:04x}", ch as u32)?;
                }
                ch => write!(f, "{ch}")?,
            }
        }
        f.write_str("\"")
    }
}

/// Convenience wrapper for a passing guest case.
pub fn emit_case_pass(case_id: &str, message: &str, details_json: Option<&str>) {
    emit_case_result(case_id, "pass", Some(message), details_json);
}

/// Convenience wrapper for a failing guest case.
pub fn emit_case_fail(case_id: &str, message: &str, details_json: Option<&str>) {
    emit_case_result(case_id, "fail", Some(message), details_json);
}

/// Convenience wrapper for a skipped guest case.
pub fn emit_case_skip(case_id: &str, message: &str, details_json: Option<&str>) {
    emit_case_result(case_id, "skip", Some(message), details_json);
}

/// Emit an error result and terminate the guest immediately afterwards.
pub fn emit_case_error(case_id: &str, message: &str, details_json: Option<&str>) -> ! {
    emit_case_result(case_id, "error", Some(message), details_json);
    power_off_or_hang();
}

/// Terminate the guest if the runtime can power off cleanly; otherwise spin.
///
/// The non-`ax-std` fallback keeps the CPU busy so the function still has a
/// well-defined diverging behavior in minimal environments.
pub fn power_off_or_hang() -> ! {
    #[cfg(feature = "ax-std")]
    {
        use std::os::arceos::modules::ax_hal;
        ax_hal::power::system_off();
    }

    #[cfg(not(feature = "ax-std"))]
    loop {
        core::hint::spin_loop();
    }
}
