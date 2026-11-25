//! The start of the local day: the one place LangSpan turns "today" into a
//! wall-clock instant, from the system's own time-zone rules.
//!
//! The offset from UTC is asked of `date` for each instant that matters
//! (`date -r SECONDS +%z` on BSD and macOS, `date -d @SECONDS +%z` on Linux),
//! honouring `TZ`. One offset is not enough: on a day the clocks change, the
//! offset now differs from the offset at midnight, and applying the current
//! one to the whole day puts midnight an hour off.

use std::time::{Duration, Instant};

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;

/// The start of the local day containing `now_ms` (Unix milliseconds), given
/// the zone's offset in minutes east of UTC at any instant (`None` when it
/// cannot be told).
///
/// The local date is taken at the offset now. Its midnight is the instant
/// `t` with `t + offset_at(t)` on that date's 00:00. Offsets are sampled at
/// the first guess and three hours before it, which covers every
/// daylight-saving change (at most two hours) near midnight. When midnight
/// happens twice (clocks put back after it) the first is taken; when it is
/// skipped (clocks put forward at it) the day starts at the change. Without
/// the offset now, midnight UTC is used; without the other samples, the
/// offset now is applied to the whole day.
pub fn local_midnight_ms(now_ms: u64, mut offset_at: impl FnMut(u64) -> Option<i32>) -> u64 {
    let now = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let Some(now_offset) = offset_at(now_ms) else {
        return fixed_midnight(now, 0);
    };
    let local_now = now.saturating_add(minutes_ms(now_offset));
    let date_start = local_now - local_now.rem_euclid(DAY_MS);
    let instant = |offset: i32| date_start - minutes_ms(offset);
    let mut asked: Vec<(i64, Option<i32>)> = Vec::new();
    let mut ask = |at: i64| {
        if let Some(&(_, answer)) = asked.iter().find(|(seen, _)| *seen == at) {
            return answer;
        }
        let answer = u64::try_from(at).ok().and_then(&mut offset_at);
        asked.push((at, answer));
        answer
    };
    let guess = instant(now_offset);
    let mut offsets = vec![now_offset];
    for at in [guess, guess - 3 * HOUR_MS] {
        if let Some(offset) = ask(at)
            && !offsets.contains(&offset)
        {
            offsets.push(offset);
        }
    }
    let candidates: Vec<i64> = offsets.iter().map(|&offset| instant(offset)).collect();
    let consistent = offsets
        .iter()
        .zip(&candidates)
        .filter(|&(&offset, &at)| ask(at) == Some(offset))
        .map(|(_, &at)| at)
        .min();
    let midnight = consistent
        .or_else(|| candidates.iter().copied().max())
        .unwrap_or(guess)
        .min(now);
    u64::try_from(midnight).unwrap_or(0)
}

/// The start of the local day at `deadline`'s wall clock, asking the
/// system's `date` (each child stopped at `deadline`).
pub fn midnight_now_ms(now_ms: u64, deadline: Instant) -> u64 {
    local_midnight_ms(now_ms, |at| offset_at(at, deadline, None))
}

/// The zone's offset at `at_ms`, from `date`, in `tz` when given (else the
/// inherited `TZ`). `None` when `date` fails, prints anything other than
/// `+HHMM` or `-HHMM`, or does not finish by `deadline`.
pub fn offset_at(at_ms: u64, deadline: Instant, tz: Option<&str>) -> Option<i32> {
    let seconds = at_ms / 1000;
    let mut command = std::process::Command::new("date");
    if cfg!(any(target_os = "linux", target_os = "android")) {
        command.arg(format!("-d@{seconds}"));
    } else {
        command.arg("-r").arg(seconds.to_string());
    }
    command
        .arg("+%z")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(tz) = tz {
        command.env("TZ", tz);
    }
    let mut child = command.spawn().ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut text).ok()?;
    parse_offset(text.trim())
}

/// `+HHMM` or `-HHMM` as minutes east of UTC.
pub fn parse_offset(text: &str) -> Option<i32> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 60 + minutes))
}

fn minutes_ms(offset_minutes: i32) -> i64 {
    i64::from(offset_minutes) * 60_000
}

/// The last midnight at or before `now` at one fixed offset.
fn fixed_midnight(now: i64, offset_minutes: i32) -> u64 {
    let local = now.saturating_add(minutes_ms(offset_minutes));
    let midnight = local - local.rem_euclid(DAY_MS) - minutes_ms(offset_minutes);
    u64::try_from(midnight).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;
    /// 2026-03-08T00:00:00Z: New York puts its clocks forward at 07:00 UTC.
    const MARCH_8: u64 = 1_772_928_000_000;
    /// 2026-11-01T00:00:00Z: New York puts its clocks back at 06:00 UTC.
    const NOVEMBER_1: u64 = 1_793_491_200_000;

    /// America/New_York's 2026 rules: EDT (-04:00) from the March change to
    /// the November change, EST (-05:00) otherwise.
    fn new_york(at: u64) -> Option<i32> {
        Some(
            if (MARCH_8 + 7 * HOUR..NOVEMBER_1 + 6 * HOUR).contains(&at) {
                -240
            } else {
                -300
            },
        )
    }

    #[test]
    fn midnight_on_a_daylight_saving_change_uses_the_offset_at_midnight() {
        // Noon EST on 1 November: the day began at 00:00 EDT, 04:00 UTC.
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 17 * HOUR, new_york),
            NOVEMBER_1 + 4 * HOUR
        );
        // Noon EDT on 8 March: the day began at 00:00 EST, 05:00 UTC.
        assert_eq!(
            local_midnight_ms(MARCH_8 + 16 * HOUR, new_york),
            MARCH_8 + 5 * HOUR
        );
        // Ordinary days either side keep their own offset.
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 24 * HOUR + 17 * HOUR, new_york),
            NOVEMBER_1 + 29 * HOUR
        );
        assert_eq!(
            local_midnight_ms(MARCH_8 - 24 * HOUR + 17 * HOUR, new_york),
            MARCH_8 - 19 * HOUR
        );
        // Just after midnight, and in the repeated hour after the change.
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 4 * HOUR, new_york),
            NOVEMBER_1 + 4 * HOUR
        );
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 4 * HOUR - 1, new_york),
            NOVEMBER_1 - 20 * HOUR
        );
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 6 * HOUR + 1, new_york),
            NOVEMBER_1 + 4 * HOUR
        );
    }

    #[test]
    fn a_repeated_midnight_takes_the_first_and_a_skipped_one_the_change() {
        // Clocks put back from 01:00 to 00:00 local (-04:00 to -05:00) at
        // 05:00 UTC: midnight happens at 04:00 UTC and again at 05:00 UTC.
        let back = |at: u64| {
            Some(if at < NOVEMBER_1 + 5 * HOUR {
                -240
            } else {
                -300
            })
        };
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 17 * HOUR, back),
            NOVEMBER_1 + 4 * HOUR
        );
        // Clocks put forward at midnight local (-04:00 to -03:00) at 04:00
        // UTC: 00:00 never shows, and the day starts at the change.
        let forward = |at: u64| {
            Some(if at < NOVEMBER_1 + 4 * HOUR {
                -240
            } else {
                -180
            })
        };
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 15 * HOUR, forward),
            NOVEMBER_1 + 4 * HOUR
        );
    }

    #[test]
    fn missing_offsets_fall_back_to_utc_or_the_offset_now() {
        let now = NOVEMBER_1 + 17 * HOUR;
        assert_eq!(local_midnight_ms(now, |_| None), NOVEMBER_1);
        let only_now = |at: u64| (at == now).then_some(-300);
        assert_eq!(local_midnight_ms(now, only_now), NOVEMBER_1 + 5 * HOUR);
        // At UTC+14:00, 10:00 UTC is exactly the next day's midnight.
        let kiribati = |_| Some(840);
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 10 * HOUR, kiribati),
            NOVEMBER_1 + 10 * HOUR
        );
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 10 * HOUR - 1, kiribati),
            NOVEMBER_1 - 14 * HOUR
        );
    }

    #[test]
    fn the_system_date_gives_new_york_midnight_on_both_changes() {
        let deadline = Instant::now() + Duration::from_secs(10);
        let system = |at| offset_at(at, deadline, Some("America/New_York"));
        assert_eq!(system(MARCH_8), Some(-300));
        assert_eq!(system(MARCH_8 + 8 * HOUR), Some(-240));
        assert_eq!(
            local_midnight_ms(NOVEMBER_1 + 17 * HOUR, system),
            NOVEMBER_1 + 4 * HOUR
        );
        assert_eq!(
            local_midnight_ms(MARCH_8 + 16 * HOUR, system),
            MARCH_8 + 5 * HOUR
        );
    }

    #[test]
    fn offsets_parse() {
        assert_eq!(parse_offset("+0100"), Some(60));
        assert_eq!(parse_offset("-0530"), Some(-330));
        assert_eq!(parse_offset("+0000"), Some(0));
        for bad in ["", "0100", "+01", "+01000", "+2460", "+0160", "+01a0"] {
            assert_eq!(parse_offset(bad), None, "{bad:?}");
        }
    }
}
