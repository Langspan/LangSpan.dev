//! The daemon's clock: milliseconds on a monotonic clock that keeps counting
//! while the machine sleeps.
//!
//! `std::time::Instant` does not promise that. Its documentation names
//! `clock_gettime` with `CLOCK_MONOTONIC` on Unix and with `CLOCK_UPTIME_RAW`
//! on Darwin, and says "it is also not specified whether system suspends
//! count as elapsed time or not". The clocks themselves are documented:
//!
//! - Linux, clock_gettime(2): `CLOCK_MONOTONIC` (and `CLOCK_MONOTONIC_RAW`)
//!   "does not count time that the system is suspended"; `CLOCK_BOOTTIME` is
//!   identical to `CLOCK_MONOTONIC` "except that it also includes any time
//!   that the system is suspended".
//! - Windows: `QueryInterruptTime` counts 100-nanosecond intervals since
//!   boot, and Microsoft documents that it includes time spent in sleep and
//!   hibernation, while `QueryUnbiasedInterruptTime` leaves that time out.
//!   It is called through `langspan-windows`, which keeps this crate free of
//!   `unsafe`.
//! - macOS, clock_gettime(3): `CLOCK_UPTIME_RAW` "does not increment while
//!   the system is asleep"; `CLOCK_MONOTONIC` "will continue to increment
//!   while the system is asleep". (`CLOCK_MONOTONIC_RAW` is described only as
//!   "like CLOCK_MONOTONIC" but unadjusted, and rustix does not name it on
//!   Apple targets, so the explicitly documented one is used.)
//!
//! So `Instant` stops during a laptop's sleep on Linux and macOS, and this clock does
//! not: ages shown to people, the activity, teammate and briefing horizons,
//! and lease expiry all count a night with the lid closed.
//!
//! The value is time since boot. It is only ever compared with other values
//! from this clock on this machine, never with another machine's clock and
//! never with wall time. A reboot starts it again from zero.

#[cfg(unix)]
use rustix::time::{ClockId, clock_gettime};

/// The sleep-inclusive monotonic clock this platform documents.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SOURCE: ClockId = ClockId::Boottime;
#[cfg(target_vendor = "apple")]
pub const SOURCE: ClockId = ClockId::Monotonic;
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    windows
)))]
compile_error!("no sleep-inclusive monotonic clock is known for this platform");

/// Milliseconds since boot on `SOURCE`, sleep included.
#[cfg(unix)]
pub fn now_ms() -> u64 {
    let now = clock_gettime(SOURCE);
    let seconds = u64::try_from(now.tv_sec).unwrap_or(0);
    let millis = u64::try_from(now.tv_nsec).unwrap_or(0) / 1_000_000;
    seconds.saturating_mul(1000).saturating_add(millis)
}

/// Milliseconds since boot on the interrupt-time clock, sleep included.
#[cfg(windows)]
pub fn now_ms() -> u64 {
    langspan_windows::interrupt_time_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemon_clock_is_the_documented_sleep_inclusive_one() {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        assert_eq!(
            SOURCE,
            ClockId::Boottime,
            "CLOCK_MONOTONIC stops in suspend"
        );
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            SOURCE,
            ClockId::Monotonic,
            "CLOCK_UPTIME_RAW stops in sleep"
        );
    }

    #[test]
    fn the_daemon_clock_never_goes_back() {
        let before = now_ms();
        let after = now_ms();
        assert!(after >= before && before > 0);
        // On Linux the suspend-blind clock can only be behind it.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let awake = clock_gettime(ClockId::Monotonic);
            let awake_ms = u64::try_from(awake.tv_sec).unwrap() * 1000;
            assert!(now_ms() + 1 >= awake_ms);
        }
    }
}
