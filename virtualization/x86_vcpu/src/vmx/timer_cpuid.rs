//! Describe the virtual LAPIC's 1 GHz clock without changing the TSC rate.
use raw_cpuid::CpuIdResult;

pub(super) fn virtual_timer_leaf(host: CpuIdResult) -> CpuIdResult {
    let unavailable = CpuIdResult {
        eax: 0,
        ebx: 0,
        ecx: 0,
        edx: 0,
    };
    if host.eax == 0 || host.ebx == 0 || host.ecx == 0 {
        return unavailable;
    }
    let numerator = u64::from(host.ecx) * u64::from(host.ebx);
    let denominator = 1_000_000_000u64 * u64::from(host.eax);
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let (Ok(ebx), Ok(eax)) = (u32::try_from(numerator / a), u32::try_from(denominator / a)) else {
        return unavailable;
    };
    CpuIdResult {
        eax,
        ebx,
        ecx: 1_000_000_000,
        edx: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_tsc_rate_with_virtual_lapic_clock() {
        for host in [
            CpuIdResult {
                eax: 1,
                ebx: 96,
                ecx: 38_400_000,
                edx: 0,
            },
            CpuIdResult {
                eax: 2,
                ebx: 300,
                ecx: 24_000_000,
                edx: 0,
            },
        ] {
            let guest = virtual_timer_leaf(host);
            assert_eq!(guest.ecx, 1_000_000_000);
            assert_eq!(
                u128::from(guest.ecx) * u128::from(guest.ebx) * u128::from(host.eax),
                u128::from(host.ecx) * u128::from(host.ebx) * u128::from(guest.eax)
            );
        }
    }

    #[test]
    fn unknown_crystal_requires_guest_calibration() {
        let guest = virtual_timer_leaf(CpuIdResult {
            eax: 1,
            ebx: 96,
            ecx: 0,
            edx: 0,
        });
        assert_eq!((guest.eax, guest.ebx, guest.ecx), (0, 0, 0));
    }
}
