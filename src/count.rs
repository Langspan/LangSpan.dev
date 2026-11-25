//! `langspan count` (roadmap H5): how often LangSpan refused a write or a
//! commit in one local month. The count itself is the core's
//! (`langspan_core::count_refusals`), read by the daemon from the event log
//! at query time; this adapter turns a month into its local days' starts and
//! the answer into text.
//!
//! Each local midnight is found by `local_day` from the system's own zone
//! rules for that date, so a month with a daylight-saving change has days
//! of 23 and 25 hours, each starting at its own 00:00.
//!
//! The wording says what LangSpan did, never what it is assumed to have
//! saved: every text carries `DISCLAIMER`.

use std::path::Path;
use std::time::{Duration, Instant};

use langspan_core::{COUNT_KINDS, Command, Outcome, RefusalCount};
use serde::Serialize;

use crate::{
    client, local_day,
    protocol::{PROTOCOL_VERSION, Request},
    today,
};

/// The line every count carries until refusals are tied to outcomes (H9)
/// and a measurement.
pub const DISCLAIMER: &str = "A refusal is not proof of avoided rework.";
/// How long the `date` calls for one month may take together.
const DATE_BUDGET: Duration = Duration::from_secs(2);
/// Workspaces named in the text; the rest are counted.
const TEXT_WORKSPACES: usize = 8;
const DAY_MS: i64 = 86_400_000;
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A calendar month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Month {
    pub year: i64,
    /// 1 to 12.
    pub month: i64,
}

impl Month {
    /// `YYYY-MM`, 1970-01 to 9999-12.
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || format!("--month {text:?} is not a month; write it as 2027-03");
        let bytes = text.as_bytes();
        if bytes.len() != 7 || bytes[4] != b'-' || !text.is_ascii() {
            return Err(invalid());
        }
        let digits = |part: &str| -> Result<i64, String> {
            if part.bytes().all(|byte| byte.is_ascii_digit()) {
                part.parse().map_err(|_| invalid())
            } else {
                Err(invalid())
            }
        };
        let (year, month) = (digits(&text[..4])?, digits(&text[5..])?);
        if year < 1970 || !(1..=12).contains(&month) {
            return Err(invalid());
        }
        Ok(Self { year, month })
    }

    /// The month holding `at_ms` at the zone's offset then.
    pub fn of(at_ms: u64, offset_minutes: i32) -> Self {
        let local = i64::try_from(at_ms).unwrap_or(i64::MAX) / 60_000 + i64::from(offset_minutes);
        let (year, month, _) = today::civil_from_days(local.div_euclid(1440));
        Self { year, month }
    }

    fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    fn days(self) -> i64 {
        today::days_from_civil(self.next().year, self.next().month, 1)
            - today::days_from_civil(self.year, self.month, 1)
    }

    /// `2027-03`.
    pub fn key(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    /// `March 2027`.
    pub fn label(self) -> String {
        let name = usize::try_from(self.month - 1)
            .ok()
            .and_then(|index| MONTHS.get(index))
            .copied()
            .unwrap_or("?");
        format!("{name} {}", self.year)
    }

    /// Each local day's start and the next month's first: the window the
    /// daemon counts. `offset_at` gives the zone's offset (minutes east of
    /// UTC) at an instant; `None` when any offset the month needs cannot be
    /// read, so no count is built from a partial or guessed month.
    pub fn day_starts(self, mut offset_at: impl FnMut(u64) -> Option<i32>) -> Option<Vec<u64>> {
        let mut asked: Vec<(u64, Option<i32>)> = Vec::new();
        let mut failed = false;
        let mut ask = |at: u64| {
            if let Some(&(_, answer)) = asked.iter().find(|(seen, _)| *seen == at) {
                return answer;
            }
            let answer = offset_at(at);
            failed |= answer.is_none();
            asked.push((at, answer));
            answer
        };
        let first = today::days_from_civil(self.year, self.month, 1);
        // Noon UTC of the first day at the offset found there: within that
        // local date for every zone (offsets are within 14 hours) and every
        // change of up to two hours.
        let first_noon = u64::try_from(first * DAY_MS + DAY_MS / 2).ok()?;
        let offset = i64::from(ask(first_noon)?) * 60_000;
        let mut starts = Vec::new();
        for day in first..=first + self.days() {
            // 1970-01-01 east of UTC would be before the epoch: start at it.
            let noon = u64::try_from((day * DAY_MS + DAY_MS / 2 - offset).max(0)).ok()?;
            // `local_midnight_ms` falls back to UTC or another sampled
            // offset when a lookup fails; `failed` refuses that fallback.
            starts.push(local_day::local_midnight_ms(noon, &mut ask));
        }
        if failed {
            return None;
        }
        // A zone that skipped a whole day has two days with one start.
        starts.dedup();
        Some(starts)
    }
}

/// What `count --json` prints: the month and the daemon's count.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MonthCount {
    pub month: String,
    #[serde(flatten)]
    pub count: RefusalCount,
}

impl MonthCount {
    /// `March 2027: 17 refusals (9 conflict, 5 stale_base, 2 unread_lines,
    /// 1 commit_held)`.
    pub fn headline(&self, label: &str) -> String {
        let kinds: Vec<String> = COUNT_KINDS
            .iter()
            .map(|kind| {
                format!(
                    "{} {}",
                    self.count.by_kind.get(kind.name()).copied().unwrap_or(0),
                    kind.name()
                )
            })
            .collect();
        format!(
            "{label}: {} ({})",
            refusals(self.count.refusals),
            kinds.join(", ")
        )
    }

    /// `truncated: counted from ...` when the daemon counted only the
    /// newest records: shown next to the count wherever it appears.
    pub fn truncation(&self) -> Option<String> {
        self.count.truncated.then(|| {
            format!(
                "truncated: counted from the newest {} refusal records only",
                langspan_core::COUNT_REFUSALS
            )
        })
    }

    /// The headline and, when the count is partial, its truncation: the
    /// one line `today` and the local page show.
    pub fn summary(&self, label: &str) -> String {
        match self.truncation() {
            Some(truncation) => format!("{} ({truncation})", self.headline(label)),
            None => self.headline(label),
        }
    }

    /// The text for a person.
    pub fn text(&self, label: &str) -> String {
        let count = &self.count;
        let mut lines = vec![self.headline(label)];
        if !count.by_workspace.is_empty() {
            let mut ranked: Vec<(&String, &u32)> = count.by_workspace.iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            let shown: Vec<String> = ranked
                .iter()
                .take(TEXT_WORKSPACES)
                .map(|(workspace, n)| format!("{workspace} {n}"))
                .collect();
            let left: u32 = ranked
                .iter()
                .skip(TEXT_WORKSPACES)
                .map(|(_, n)| **n)
                .sum::<u32>()
                .saturating_add(count.omitted_workspaces);
            let more = if left > 0 {
                format!(", other workspaces {left}")
            } else {
                String::new()
            };
            lines.push(format!("By workspace: {}{more}", shown.join(", ")));
        }
        if count.refusals > 0 {
            lines.push(format!(
                "Retried and let through: {} (the same session was admitted on the same files afterwards).",
                count.retried_through
            ));
        }
        if count.repeats > 0 {
            lines.push(format!(
                "{} repeat(s) of a refusal by the same session, kind and files on the same day are not counted again.",
                count.repeats
            ));
        }
        if let Some(truncation) = self.truncation() {
            let (first, rest) = truncation.split_at(1);
            lines.push(format!("{}{rest}.", first.to_uppercase()));
        }
        if count.undated > 0 {
            lines.push(format!(
                "{} refusal record(s) from an earlier build have no time and are not counted.",
                count.undated
            ));
        }
        lines.push(DISCLAIMER.into());
        lines.join("\n")
    }
}

fn refusals(n: u32) -> String {
    if n == 1 {
        "1 refusal".into()
    } else {
        format!("{n} refusals")
    }
}

/// The month `--month` names, or the local month now.
pub fn month(text: Option<&str>, now_ms: u64) -> Result<Month, String> {
    match text {
        Some(text) => Month::parse(text),
        None => {
            let deadline = Instant::now() + DATE_BUDGET;
            let offset = local_day::offset_at(now_ms, deadline, None).ok_or_else(|| {
                "could not read the local time zone (`date` gave no offset within 2 s); pass --month"
                    .to_owned()
            })?;
            Ok(Month::of(now_ms, offset))
        }
    }
}

/// The month's local day starts, from the system's zone rules.
pub fn day_starts(month: Month) -> Result<Vec<u64>, String> {
    let deadline = Instant::now() + DATE_BUDGET;
    month
        .day_starts(|at| local_day::offset_at(at, deadline, None))
        .ok_or_else(|| {
            format!(
                "could not read the local time zone for every day of {} (`date` failed or gave no offset within 2 s); no count is shown from a partial month",
                month.key()
            )
        })
}

/// Ask the daemon for one month's count. An unreachable daemon is an
/// error; no earlier figure is kept to show instead.
pub fn ask(state_dir: &Path, month: Month, days: Vec<u64>) -> Result<MonthCount, crate::Error> {
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Count { days },
    };
    match client::call(state_dir, &request)?.outcome {
        Outcome::Count { count } => Ok(MonthCount {
            month: month.key(),
            count,
        }),
        Outcome::Error { code, message } => Err(format!("{code}: {message}").into()),
        _ => Err(
            "the coordinator answered with something other than a count; restart the daemon after upgrading LangSpan"
                .into(),
        ),
    }
}

/// The one line `langspan today` shows: this month's count, every
/// workspace, and the named workspace's part of it.
pub fn today_line(state_dir: &Path, workspace_id: Option<&str>, now_ms: u64) -> String {
    match this_month(state_dir, now_ms) {
        Ok((month, count)) => {
            let part = match workspace_id {
                Some(id) => format!(
                    " in every workspace, {} in {id}",
                    count.count.by_workspace.get(id).copied().unwrap_or(0)
                ),
                None => String::new(),
            };
            let truncation = count
                .truncation()
                .map(|truncation| format!(" ({truncation})"))
                .unwrap_or_default();
            format!(
                "This month, {}{part}{truncation}. {DISCLAIMER}",
                count.headline(&month.label())
            )
        }
        Err(error) => format!("This month: the refusal count could not be read ({error})."),
    }
}

/// The local month now and its count, for `today` and the local page.
pub fn this_month(state_dir: &Path, now_ms: u64) -> Result<(Month, MonthCount), crate::Error> {
    let month = month(None, now_ms)?;
    let count = ask(state_dir, month, day_starts(month)?)?;
    Ok((month, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;

    #[test]
    fn months_are_read_and_named() {
        assert_eq!(
            Month::parse("2027-03"),
            Ok(Month {
                year: 2027,
                month: 3
            })
        );
        for bad in [
            "2027-3",
            "2027-13",
            "2027-00",
            "1969-12",
            "27-03",
            "2027/03",
            "２027-03",
        ] {
            assert!(Month::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Month::parse("2027-03").unwrap().label(), "March 2027");
        assert_eq!(Month::parse("2028-02").unwrap().days(), 29);
        assert_eq!(Month::parse("2027-12").unwrap().next().key(), "2028-01");
        // 2027-03-31T23:30:00Z is in April at +01:00 and March at -05:00.
        let at = 1_806_535_800_000;
        assert_eq!(Month::of(at, 60).key(), "2027-04");
        assert_eq!(Month::of(at, -300).key(), "2027-03");
    }

    #[test]
    fn a_month_with_a_clock_change_has_each_day_start_at_its_own_midnight() {
        // March 2027 in New York: EST (-05:00) until 2027-03-14T07:00:00Z,
        // EDT (-04:00) after.
        let change = 1_805_007_600_000;
        let new_york = |at: u64| Some(if at < change { -300 } else { -240 });
        let march = Month::parse("2027-03").unwrap();
        let starts = march.day_starts(new_york).unwrap();
        assert_eq!(starts.len(), 32);
        // 2027-03-01T05:00:00Z, and April 1 at 04:00 UTC.
        assert_eq!(starts[0], 1_803_877_200_000);
        assert_eq!(starts[31], 1_806_552_000_000);
        let lengths: Vec<u64> = starts.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert_eq!(lengths.iter().filter(|&&ms| ms == 23 * HOUR).count(), 1);
        assert_eq!(lengths.iter().filter(|&&ms| ms == 24 * HOUR).count(), 30);
        // Without the first day's offset there is no window.
        assert!(march.day_starts(|_| None).is_none());
        // Nor when any later lookup fails: no month from fallback offsets.
        for answered in [1, 2, 10, 60] {
            let mut calls = 0;
            let partial = march.day_starts(|at| {
                calls += 1;
                if calls > answered { None } else { new_york(at) }
            });
            assert!(partial.is_none(), "after {answered} answers: {partial:?}");
        }
    }

    #[test]
    fn the_text_claims_no_outcome() {
        let mut by_kind = std::collections::BTreeMap::new();
        for (kind, n) in [
            ("conflict", 9),
            ("stale_base", 5),
            ("unread_lines", 2),
            ("commit_held", 1),
        ] {
            by_kind.insert(kind.to_owned(), n);
        }
        let count = MonthCount {
            month: "2027-03".into(),
            count: RefusalCount {
                from_ms: 0,
                to_ms: 1,
                refusals: 17,
                by_kind,
                by_workspace: [("app".to_owned(), 12), ("web".to_owned(), 5)].into(),
                omitted_workspaces: 0,
                retried_through: 3,
                repeats: 4,
                truncated: true,
                undated: 2,
            },
        };
        let text = count.text("March 2027");
        assert!(
            text.starts_with(
                "March 2027: 17 refusals (9 conflict, 5 stale_base, 2 unread_lines, 1 commit_held)\nBy workspace: app 12, web 5\n"
            ),
            "{text}"
        );
        assert!(text.ends_with(DISCLAIMER), "{text}");
        assert!(
            text.contains("\nTruncated: counted from the newest 20000 refusal records only.\n"),
            "{text}"
        );
        assert_no_outcome_claimed(&text);
        let summary = count.summary("March 2027");
        assert!(
            summary.ends_with(
                "1 commit_held) (truncated: counted from the newest 20000 refusal records only)"
            ),
            "{summary}"
        );
        let whole = MonthCount {
            count: RefusalCount {
                truncated: false,
                ..count.count.clone()
            },
            ..count.clone()
        };
        assert_eq!(whole.summary("March 2027"), whole.headline("March 2027"));
        assert!(!whole.text("March 2027").contains("runcated"));
    }

    /// Words that would claim what a refusal achieved. The disclaimer's own
    /// "avoided" is the one use allowed.
    pub(crate) fn assert_no_outcome_claimed(text: &str) {
        let text = text.replace(DISCLAIMER, "").to_lowercase();
        for word in [
            "prevent",
            "saved",
            "saving",
            "saves",
            "avoid",
            "collision",
            "caught",
            "catch",
            "protect",
            "rescue",
            "spared",
            "stopped",
            "averted",
            "blocked",
        ] {
            assert!(!text.contains(word), "{word:?} in {text}");
        }
    }
}
