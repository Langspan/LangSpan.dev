//! The system's time-zone rules, read in-process: a compiled zone file
//! (TZif, RFC 8536) named by `TZ` or `/etc/localtime`, or a POSIX `TZ`
//! rule such as `EST5EDT,M3.2.0,M11.1.0`. No child process is started, so
//! a month's hundred offsets cost microseconds on any Unix and none
//! depends on a `date` program's arguments.
//!
//! `TZ` is resolved as the C library resolves it: unset is
//! `/etc/localtime` (UTC when that file does not exist), empty is UTC, a
//! leading `:` is dropped, an absolute path is that file, and a relative
//! name is a file under `TZDIR` or the usual zone directories. A name that
//! names no file is read as a POSIX rule. Unlike the C library, a value
//! that is neither is refused (`None`) rather than taken as UTC, so a
//! mistyped zone is reported instead of counted in the wrong day.
//! Leap-second ("right/") zones are read as if their transition times were
//! plain Unix times: at most half a minute off, at the changes only.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest zone file read. Real ones are under 4 KiB.
const MAX_FILE: u64 = 256 * 1024;
/// Where zone names are looked up when `TZDIR` is unset or misses.
const ZONE_DIRS: [&str; 3] = [
    "/usr/share/zoneinfo",
    "/var/db/timezone/zoneinfo",
    "/usr/share/lib/zoneinfo",
];

/// One zone's rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    /// Transition instants (Unix seconds, ascending) and the offset (seconds
    /// east of UTC) from each.
    transitions: Vec<(i64, i32)>,
    /// The offset before the first transition.
    initial: i32,
    /// The rule after the last transition, when the file or `TZ` gives one.
    rule: Option<Rule>,
}

impl Zone {
    /// The zone `tz` names (`None`: the inherited `TZ`), or `None` when it
    /// cannot be read.
    pub fn load(tz: Option<&str>) -> Option<Self> {
        let inherited;
        let tz = match tz {
            Some(tz) => Some(tz),
            None => {
                inherited = std::env::var_os("TZ");
                match &inherited {
                    Some(value) => Some(value.to_str()?),
                    None => None,
                }
            }
        };
        let Some(tz) = tz else {
            return match read_bounded(Path::new("/etc/localtime")) {
                Ok(bytes) => parse_tzif(&bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Self::utc()),
                Err(_) => None,
            };
        };
        if tz.is_empty() {
            return Some(Self::utc());
        }
        let (name, file_only) = match tz.strip_prefix(':') {
            Some(name) => (name, true),
            None => (tz, false),
        };
        if let Some(path) = zone_file(name) {
            return read_bounded(&path)
                .ok()
                .and_then(|bytes| parse_tzif(&bytes));
        }
        if matches!(name, "UTC" | "UCT" | "GMT" | "Zulu" | "Etc/UTC" | "Etc/GMT") {
            return Some(Self::utc());
        }
        if file_only {
            return None;
        }
        let rule = parse_rule(tz)?;
        Some(Self {
            transitions: Vec::new(),
            initial: rule.std_offset,
            rule: Some(rule),
        })
    }

    fn utc() -> Self {
        Self {
            transitions: Vec::new(),
            initial: 0,
            rule: None,
        }
    }

    /// The offset in minutes east of UTC at `at_ms` (Unix milliseconds),
    /// truncated toward zero as `%z` shows it.
    pub fn offset_minutes(&self, at_ms: u64) -> i32 {
        let at = i64::try_from(at_ms / 1000).unwrap_or(i64::MAX);
        self.offset_seconds(at) / 60
    }

    fn offset_seconds(&self, at: i64) -> i32 {
        let after = self.transitions.partition_point(|&(when, _)| when <= at);
        match (after, &self.rule) {
            (_, Some(rule)) if after == self.transitions.len() => rule.offset_at(at),
            (0, _) => self.initial,
            _ => self.transitions[after - 1].1,
        }
    }
}

/// The file a zone name refers to, when one exists.
fn zone_file(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        return Some(PathBuf::from(name));
    }
    let safe = !name.is_empty()
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_-+".contains(&byte));
    if !safe {
        return None;
    }
    let tzdir = std::env::var_os("TZDIR").map(PathBuf::from);
    tzdir
        .into_iter()
        .chain(ZONE_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(std::io::Error::other("zone file too large"));
    }
    Ok(bytes)
}

/// A TZif file's rules: its 64-bit data and footer rule when the version
/// has them, else its 32-bit data.
pub fn parse_tzif(bytes: &[u8]) -> Option<Zone> {
    let first = header(bytes)?;
    if first.version == 0 {
        return block(bytes.get(44..)?, &first, 4).map(|(zone, _)| zone);
    }
    let rest = bytes.get(44 + first.len_for(4)?..)?;
    let second = header(rest)?;
    let (mut zone, end) = block(rest.get(44..)?, &second, 8)?;
    let footer = rest.get(44 + end..)?;
    if let Some(text) = footer
        .strip_prefix(b"\n")
        .and_then(|footer| footer.split(|&byte| byte == b'\n').next())
        && !text.is_empty()
    {
        zone.rule = Some(parse_rule(std::str::from_utf8(text).ok()?)?);
    }
    Some(zone)
}

#[derive(Debug, Clone, Copy)]
struct Header {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl Header {
    /// The data block's length with `time_size`-byte times.
    fn len_for(&self, time_size: usize) -> Option<usize> {
        let parts = [
            self.timecnt.checked_mul(time_size + 1)?,
            self.typecnt.checked_mul(6)?,
            self.charcnt,
            self.leapcnt.checked_mul(time_size + 4)?,
            self.isstdcnt,
            self.isutcnt,
        ];
        parts
            .iter()
            .try_fold(0usize, |sum, &part| sum.checked_add(part))
    }
}

fn header(bytes: &[u8]) -> Option<Header> {
    if bytes.get(..4)? != b"TZif" {
        return None;
    }
    let version = match *bytes.get(4)? {
        0 => 0,
        byte @ b'2'..=b'9' => byte - b'0',
        _ => return None,
    };
    let count = |index: usize| -> Option<usize> {
        let at = 20 + index * 4;
        let raw = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        usize::try_from(raw).ok()
    };
    let header = Header {
        version,
        isutcnt: count(0)?,
        isstdcnt: count(1)?,
        leapcnt: count(2)?,
        timecnt: count(3)?,
        typecnt: count(4)?,
        charcnt: count(5)?,
    };
    if header.typecnt == 0 {
        return None;
    }
    Some(header)
}

/// The zone in one data block, and the block's length.
fn block(data: &[u8], header: &Header, time_size: usize) -> Option<(Zone, usize)> {
    let len = header.len_for(time_size)?;
    let data = data.get(..len)?;
    let times = data.get(..header.timecnt * time_size)?;
    let indices = data.get(times.len()..times.len() + header.timecnt)?;
    let types_at = times.len() + indices.len();
    let types = data.get(types_at..types_at + header.typecnt * 6)?;
    let offset = |index: usize| -> Option<i32> {
        let raw = types.get(index * 6..index * 6 + 4)?;
        let offset = i32::from_be_bytes(raw.try_into().ok()?);
        // RFC 8536: -89999 to 93599 seconds.
        (-89_999..=93_599).contains(&offset).then_some(offset)
    };
    let mut transitions = Vec::with_capacity(header.timecnt);
    for (chunk, &index) in times.chunks_exact(time_size).zip(indices) {
        let when = if time_size == 4 {
            i64::from(i32::from_be_bytes(chunk.try_into().ok()?))
        } else {
            i64::from_be_bytes(chunk.try_into().ok()?)
        };
        if transitions.last().is_some_and(|&(last, _)| last >= when) {
            return None;
        }
        transitions.push((when, offset(usize::from(index))?));
    }
    let zone = Zone {
        transitions,
        initial: offset(0)?,
        rule: None,
    };
    Some((zone, len))
}

/// A POSIX `TZ` rule: a standard offset and, optionally, daylight saving
/// time between two yearly dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rule {
    /// Seconds east of UTC.
    std_offset: i32,
    dst: Option<Dst>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dst {
    offset: i32,
    /// When daylight saving starts, in standard local time.
    start: (Date, i32),
    /// When it ends, in daylight saving local time.
    end: (Date, i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Date {
    /// `Jn`: day 1 to 365, February 29 never counted.
    Julian(i64),
    /// `n`: day 0 to 365, February 29 counted.
    Ordinal(i64),
    /// `Mm.w.d`: weekday `d` (0 is Sunday) of week `w` (5 is the last) of
    /// month `m`.
    Month(i64, i64, i64),
}

const DAY: i64 = 86_400;

impl Rule {
    fn offset_at(&self, at: i64) -> i32 {
        let Some(dst) = self.dst else {
            return self.std_offset;
        };
        let local = at.saturating_add(i64::from(self.std_offset));
        let (year, _, _) = crate::today::civil_from_days(local.div_euclid(DAY));
        let start =
            dst.start.0.day(year) * DAY + i64::from(dst.start.1) - i64::from(self.std_offset);
        let end = dst.end.0.day(year) * DAY + i64::from(dst.end.1) - i64::from(dst.offset);
        let in_dst = if start < end {
            start <= at && at < end
        } else {
            at < end || start <= at
        };
        if in_dst { dst.offset } else { self.std_offset }
    }
}

impl Date {
    /// The date's days since 1970-01-01 in `year`.
    fn day(self, year: i64) -> i64 {
        let jan_1 = crate::today::days_from_civil(year, 1, 1);
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        match self {
            Date::Julian(n) => jan_1 + n - 1 + i64::from(leap && n >= 60),
            Date::Ordinal(n) => jan_1 + n,
            Date::Month(month, week, weekday) => {
                let first = crate::today::days_from_civil(year, month, 1);
                let (next_year, next_month) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                let next = crate::today::days_from_civil(next_year, next_month, 1);
                // 1970-01-01 was a Thursday.
                let first_weekday = (first + 4).rem_euclid(7);
                let mut day = first + (weekday - first_weekday).rem_euclid(7) + (week - 1) * 7;
                while day >= next {
                    day -= 7;
                }
                day
            }
        }
    }
}

/// `std offset [dst [offset] [,start[/time],end[/time]]]`.
fn parse_rule(text: &str) -> Option<Rule> {
    let mut rest = text.as_bytes();
    name(&mut rest)?;
    let std_offset = -hms(&mut rest, 24)?;
    if rest.is_empty() {
        return Some(Rule {
            std_offset,
            dst: None,
        });
    }
    name(&mut rest)?;
    let offset = if rest.first().is_some_and(|&byte| byte != b',') {
        -hms(&mut rest, 24)?
    } else {
        std_offset + 3600
    };
    let (start, end) = if rest.is_empty() {
        // The C library's default: the United States rules since 2007.
        ((Date::Month(3, 2, 0), 7200), (Date::Month(11, 1, 0), 7200))
    } else {
        rest = rest.strip_prefix(b",")?;
        let start = transition(&mut rest)?;
        rest = rest.strip_prefix(b",")?;
        let end = transition(&mut rest)?;
        (start, end)
    };
    rest.is_empty().then_some(Rule {
        std_offset,
        dst: Some(Dst { offset, start, end }),
    })
}

/// A zone abbreviation: three or more letters, or `<...>`.
fn name(rest: &mut &[u8]) -> Option<()> {
    let len = if rest.first() == Some(&b'<') {
        let close = rest.iter().position(|&byte| byte == b'>')?;
        let inner = &rest[1..close];
        if inner.len() < 3
            || !inner
                .iter()
                .all(|&byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'-')
        {
            return None;
        }
        close + 1
    } else {
        let len = rest
            .iter()
            .take_while(|byte| byte.is_ascii_alphabetic())
            .count();
        if len < 3 {
            return None;
        }
        len
    };
    *rest = &rest[len..];
    Some(())
}

/// `[+-]hh[:mm[:ss]]` as seconds, hours at most `max_hours`.
fn hms(rest: &mut &[u8], max_hours: i64) -> Option<i32> {
    let sign = match rest.first() {
        Some(b'-') => {
            *rest = &rest[1..];
            -1
        }
        Some(b'+') => {
            *rest = &rest[1..];
            1
        }
        _ => 1,
    };
    let mut total = 0;
    for (part, max, limit) in [(0, max_hours, 3), (1, 59, 2), (2, 59, 2)] {
        if part > 0 {
            match rest.strip_prefix(b":") {
                Some(after) => *rest = after,
                None => break,
            }
        }
        let value = number(rest, limit)?;
        if value > max {
            return None;
        }
        total += value * [3600, 60, 1][part];
    }
    i32::try_from(sign * total).ok()
}

/// One to `limit` decimal digits.
fn number(rest: &mut &[u8], limit: usize) -> Option<i64> {
    let len = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if len == 0 || len > limit {
        return None;
    }
    let value = std::str::from_utf8(&rest[..len]).ok()?.parse().ok()?;
    *rest = &rest[len..];
    Some(value)
}

/// `date[/time]`, the time 02:00 when absent.
fn transition(rest: &mut &[u8]) -> Option<(Date, i32)> {
    let date = match rest.first()? {
        b'J' => {
            *rest = &rest[1..];
            let n = number(rest, 3)?;
            (1..=365).contains(&n).then_some(Date::Julian(n))?
        }
        b'M' => {
            *rest = &rest[1..];
            let month = number(rest, 2)?;
            *rest = rest.strip_prefix(b".")?;
            let week = number(rest, 1)?;
            *rest = rest.strip_prefix(b".")?;
            let weekday = number(rest, 1)?;
            ((1..=12).contains(&month) && (1..=5).contains(&week) && weekday <= 6)
                .then_some(Date::Month(month, week, weekday))?
        }
        _ => {
            let n = number(rest, 3)?;
            (n <= 365).then_some(Date::Ordinal(n))?
        }
    };
    let time = match rest.strip_prefix(b"/") {
        Some(after) => {
            *rest = after;
            hms(rest, 167)?
        }
        None => 7200,
    };
    Some((date, time))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-03-08T00:00:00Z: New York puts its clocks forward at 07:00 UTC.
    const MARCH_8: i64 = 1_772_928_000;
    /// 2026-11-01T00:00:00Z: New York puts its clocks back at 06:00 UTC.
    const NOVEMBER_1: i64 = 1_793_491_200;
    const HOUR: i64 = 3600;

    fn rule(text: &str) -> Rule {
        parse_rule(text).unwrap_or_else(|| panic!("{text}"))
    }

    #[test]
    fn a_posix_rule_changes_at_its_local_times() {
        for text in [
            "EST5EDT,M3.2.0,M11.1.0",
            "EST5EDT,M3.2.0/2,M11.1.0/2:00",
            "EST5EDT",
        ] {
            let new_york = rule(text);
            assert_eq!(
                new_york.offset_at(MARCH_8 + 7 * HOUR - 1),
                -5 * 3600,
                "{text}"
            );
            assert_eq!(new_york.offset_at(MARCH_8 + 7 * HOUR), -4 * 3600, "{text}");
            assert_eq!(
                new_york.offset_at(NOVEMBER_1 + 6 * HOUR - 1),
                -4 * 3600,
                "{text}"
            );
            assert_eq!(
                new_york.offset_at(NOVEMBER_1 + 6 * HOUR),
                -5 * 3600,
                "{text}"
            );
        }
        // Southern hemisphere: daylight saving across the new year.
        let sydney = rule("AEST-10AEDT,M10.1.0,M4.1.0/3");
        let january = 1_767_225_600; // 2026-01-01T00:00:00Z
        assert_eq!(sydney.offset_at(january), 11 * 3600);
        assert_eq!(sydney.offset_at(january + 180 * 24 * HOUR), 10 * 3600);
        // Fixed offsets, quoted names, minutes and Julian days.
        assert_eq!(rule("<+0530>-5:30").offset_at(0), 5 * 3600 + 1800);
        assert_eq!(rule("UTC0").offset_at(MARCH_8), 0);
        let julian = rule("XST3XDT,J60/0,300");
        assert_eq!(
            Date::Julian(60).day(2028),
            crate::today::days_from_civil(2028, 3, 1)
        );
        assert_eq!(
            Date::Ordinal(59).day(2028),
            crate::today::days_from_civil(2028, 2, 29)
        );
        assert_eq!(julian.dst.unwrap().offset, -2 * 3600);
        for bad in [
            "",
            "E5",
            "EST",
            "EST5EDT,M3.2.0",
            "EST5EDT,M13.1.0,M11.1.0",
            "EST25",
            "EST5x",
        ] {
            assert_eq!(parse_rule(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_last_week_is_the_months_last_such_weekday() {
        // The last Sunday of March 2026 is the 29th; of October, the 25th.
        assert_eq!(
            Date::Month(3, 5, 0).day(2026),
            crate::today::days_from_civil(2026, 3, 29)
        );
        assert_eq!(
            Date::Month(10, 5, 0).day(2026),
            crate::today::days_from_civil(2026, 10, 25)
        );
        // The second Sunday of March 2026 is the 8th.
        assert_eq!(
            Date::Month(3, 2, 0).day(2026),
            crate::today::days_from_civil(2026, 3, 8)
        );
    }

    /// A version 2 TZif file: one 64-bit transition from `-05:00` to
    /// `-04:00` at `MARCH_8 + 7h` and New York's rule after it.
    fn tzif() -> Vec<u8> {
        let header = |timecnt: u32, typecnt: u32, charcnt: u32| {
            let mut bytes = b"TZif2".to_vec();
            bytes.extend([0; 15]);
            for count in [0, 0, 0, timecnt, typecnt, charcnt] {
                bytes.extend(u32::to_be_bytes(count));
            }
            bytes
        };
        let mut bytes = header(0, 1, 4);
        bytes.extend(i32::to_be_bytes(-5 * 3600));
        bytes.extend([0, 0]);
        bytes.extend(b"EST\0");
        bytes.extend(header(1, 2, 8));
        bytes.extend(i64::to_be_bytes(MARCH_8 + 7 * HOUR));
        bytes.push(1);
        for (offset, dst, name) in [(-5 * 3600, 0, 0), (-4 * 3600, 1, 4)] {
            bytes.extend(i32::to_be_bytes(offset));
            bytes.extend([dst, name]);
        }
        bytes.extend(b"EST\0EDT\0");
        bytes.extend(b"\nEST5EDT,M3.2.0,M11.1.0\n");
        bytes
    }

    #[test]
    fn a_tzif_file_uses_its_transitions_then_its_footer_rule() {
        let zone = parse_tzif(&tzif()).unwrap();
        let at = |seconds: i64| zone.offset_minutes(u64::try_from(seconds * 1000).unwrap());
        assert_eq!(at(0), -300);
        assert_eq!(at(MARCH_8 + 7 * HOUR - 1), -300);
        assert_eq!(at(MARCH_8 + 7 * HOUR), -240);
        // Past the last transition the footer rule decides.
        assert_eq!(at(NOVEMBER_1 + 6 * HOUR - 1), -240);
        assert_eq!(at(NOVEMBER_1 + 6 * HOUR), -300);
    }

    #[test]
    fn truncated_or_malformed_tzif_files_are_refused_without_panicking() {
        let whole = tzif();
        for len in 0..whole.len() - 1 {
            // Every cut either fails or still parses the 64-bit block.
            let _ = parse_tzif(&whole[..len]);
        }
        assert_eq!(parse_tzif(&whole[..60]), None);
        // The one transition's type index: after both headers, the 32-bit
        // block (54 bytes in all) and the 8-byte time.
        let mut bad_type = whole.clone();
        assert_eq!(bad_type[54 + 44 + 8], 1);
        bad_type[54 + 44 + 8] = 9;
        assert_eq!(parse_tzif(&bad_type), None);
        assert_eq!(parse_tzif(b"TZif"), None);
        assert_eq!(
            parse_tzif(b"not a zone file at all, but long enough to read a header"),
            None
        );
        // Huge counts overflow nothing.
        let mut huge = whole;
        huge[32..36].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(parse_tzif(&huge), None);
    }

    #[test]
    fn the_systems_new_york_file_agrees_with_its_rules() {
        let Some(zone) = Zone::load(Some("America/New_York")) else {
            // The fast suite runs where zone files are installed; say so.
            panic!("no America/New_York zone file under TZDIR or {ZONE_DIRS:?}");
        };
        let at = |seconds: i64| zone.offset_minutes(u64::try_from(seconds * 1000).unwrap());
        assert_eq!(at(MARCH_8 + 7 * HOUR - 1), -300);
        assert_eq!(at(MARCH_8 + 7 * HOUR), -240);
        assert_eq!(at(NOVEMBER_1 + 6 * HOUR - 1), -240);
        assert_eq!(at(NOVEMBER_1 + 6 * HOUR), -300);
        assert_eq!(Zone::load(Some("")), Some(Zone::utc()));
        assert_eq!(
            Zone::load(Some("UTC")).map(|zone| zone.offset_minutes(0)),
            Some(0)
        );
        assert_eq!(Zone::load(Some("No/Such_Zone")), None);
        assert_eq!(Zone::load(Some(":EST5EDT,M3.2.0,M11.1.0")), None);
        assert_eq!(Zone::load(Some("../../etc/passwd")), None);
    }

    /// Every zone and rule here agrees with the system's own GNU `date`
    /// (one child for all instants, `date -f`) every 5 hours of 2024 to
    /// 2027 (2,800 instants).
    #[test]
    #[cfg(target_os = "linux")]
    fn offsets_agree_with_gnu_date_across_many_zones() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let start = 1_704_067_200; // 2024-01-01T00:00:00Z
        let instants: Vec<i64> = (0..2_800).map(|step| start + step * 5 * HOUR).collect();
        let input: String = instants.iter().map(|at| format!("@{at}\n")).collect();
        for tz in [
            "America/New_York",
            "Europe/London",
            "Australia/Sydney",
            "Asia/Kolkata",
            "America/Sao_Paulo",
            "Africa/Casablanca",
            "Pacific/Chatham",
            "EST5EDT,M3.2.0,M11.1.0",
            "AEST-10AEDT,M10.1.0,M4.1.0/3",
            "<+0545>-5:45",
        ] {
            let zone = Zone::load(Some(tz)).unwrap_or_else(|| panic!("{tz}"));
            let mut child = Command::new("date")
                .args(["-f", "-", "+%z"])
                .env("TZ", tz)
                .env("LC_ALL", "C")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{tz}");
            let text = String::from_utf8(output.stdout).unwrap();
            let expected: Vec<i32> = text
                .lines()
                .map(|line| crate::local_day::parse_offset(line).unwrap())
                .collect();
            let actual: Vec<i32> = instants
                .iter()
                .map(|&at| zone.offset_minutes(u64::try_from(at * 1000).unwrap()))
                .collect();
            assert_eq!(actual, expected, "{tz}");
        }
    }
}
