//! The monthly refusal count (roadmap H5): every time is injected, the
//! daemon's clock and the wall clock, and each day's start is given.
use langspan_core::{
    COUNT_KINDS, COUNT_WORKSPACES, ClockAnchor, CountQuery, Event, RecordedEvent, RefusalCount,
    RefusalKind, count_refusals, validate_count_days,
};

const DAY: u64 = 86_400_000;
const HOUR: u64 = 3_600_000;
/// 2027-03-01T00:00:00Z.
const MARCH_1: u64 = 1_803_859_200_000;
/// 2027-04-01T00:00:00Z.
const APRIL_1: u64 = MARCH_1 + 31 * DAY;

/// March 2027 in UTC: 31 midnights and April's first.
fn march() -> Vec<u64> {
    (0..=31).map(|day| MARCH_1 + day * DAY).collect()
}

struct Log {
    events: Vec<RecordedEvent>,
    anchors: Vec<ClockAnchor>,
}

impl Log {
    /// One generation anchored so that its clock's zero is `wall_zero`.
    fn new(wall_zero: u64) -> Self {
        Self {
            events: Vec::new(),
            anchors: vec![ClockAnchor {
                generation: 1,
                clock_ms: 0,
                wall_ms: wall_zero,
            }],
        }
    }
    fn refused(&mut self, session: &str, kind: RefusalKind, paths: &[&str], at_ms: u64) {
        self.refused_in(1, "work", session, kind, paths, at_ms);
    }
    fn refused_in(
        &mut self,
        generation: u64,
        workspace: &str,
        session: &str,
        kind: RefusalKind,
        paths: &[&str],
        at_ms: u64,
    ) {
        let seq = self.events.len() as u64 * 10 + 10;
        self.events.push(RecordedEvent {
            seq,
            event: Event::WriteRefused {
                session_id: session.into(),
                workspace_id: workspace.into(),
                kind,
                paths: paths.iter().map(|path| (*path).into()).collect(),
                peer_session_id: None,
                input_digest: None,
                at_ms,
                generation,
            },
        });
    }
    fn count(&self, days: &[u64]) -> RefusalCount {
        self.count_with(days, false, |_, _, _, _| false)
    }
    fn count_with(
        &self,
        days: &[u64],
        earlier: bool,
        mut retried: impl FnMut(&str, &str, u64, &[String]) -> bool,
    ) -> RefusalCount {
        count_refusals(
            &CountQuery {
                days,
                refusals: &self.events,
                anchors: &self.anchors,
                earlier,
                retained_since_ms: None,
            },
            |session, workspace, after, paths| {
                Ok::<_, ()>(retried(session, workspace, after, paths))
            },
        )
        .unwrap()
    }
}

fn kinds(count: &RefusalCount) -> Vec<(String, u32)> {
    count
        .by_kind
        .iter()
        .map(|(kind, n)| (kind.clone(), *n))
        .collect()
}

#[test]
fn three_refusals_of_one_session_file_and_kind_in_a_day_count_as_one() {
    let mut log = Log::new(MARCH_1);
    for minute in [1, 2, 3] {
        log.refused(
            "claude-code.alice",
            RefusalKind::Conflict,
            &["src/lib.rs"],
            10 * HOUR + minute * 60_000,
        );
    }
    let count = log.count(&march());
    assert_eq!(count.refusals, 1, "{count:?}");
    assert_eq!(count.repeats, 2, "{count:?}");
    assert_eq!(count.by_kind["conflict"], 1);
    assert_eq!(count.by_workspace["work"], 1);
}

#[test]
fn each_part_of_the_key_makes_its_own_refusal() {
    let mut log = Log::new(MARCH_1);
    let at = 10 * HOUR;
    log.refused("claude-code.alice", RefusalKind::Conflict, &["a.rs"], at);
    // Another session, kind, path set and day each count again.
    log.refused("codex.bob", RefusalKind::Conflict, &["a.rs"], at);
    log.refused("claude-code.alice", RefusalKind::StaleBase, &["a.rs"], at);
    log.refused(
        "claude-code.alice",
        RefusalKind::Conflict,
        &["a.rs", "b.rs"],
        at,
    );
    log.refused(
        "claude-code.alice",
        RefusalKind::Conflict,
        &["a.rs"],
        at + DAY,
    );
    let count = log.count(&march());
    assert_eq!(count.refusals, 5, "{count:?}");
    assert_eq!(count.repeats, 0);
}

#[test]
fn a_day_is_the_given_local_day_not_the_utc_one() {
    // New York in March 2027 before the change (EST, -05:00): March 2
    // starts at 05:00 UTC.
    let days: Vec<u64> = (0..=31).map(|day| MARCH_1 + 5 * HOUR + day * DAY).collect();
    let mut log = Log::new(MARCH_1);
    // 23:00 and 01:00 UTC on March 2: both 18:00 and 20:00 local on March 1.
    let late = DAY - HOUR + 5 * HOUR;
    log.refused("s", RefusalKind::UnreadLines, &["a.rs"], DAY - HOUR);
    log.refused("s", RefusalKind::UnreadLines, &["a.rs"], DAY + HOUR);
    assert_eq!(log.count(&days).refusals, 1);
    // In UTC they are on two days.
    assert_eq!(log.count(&march()).refusals, 2);
    // 05:00 UTC on March 2 is local midnight: a new day.
    log.refused("s", RefusalKind::UnreadLines, &["a.rs"], late + HOUR);
    assert_eq!(log.count(&days).refusals, 2);
}

#[test]
fn refusals_in_other_months_are_left_to_their_own_month() {
    let mut log = Log::new(MARCH_1 - DAY);
    // February 28 at 23:59:59.999, March 31 at 23:59:59.999, April 1 at
    // 00:00.
    log.refused("s", RefusalKind::Conflict, &["a.rs"], DAY - 1);
    log.refused("s", RefusalKind::Conflict, &["b.rs"], DAY);
    log.refused("s", RefusalKind::Conflict, &["c.rs"], 32 * DAY - 1);
    log.refused("s", RefusalKind::Conflict, &["d.rs"], 32 * DAY);
    let count = log.count(&march());
    assert_eq!(count.refusals, 2, "{count:?}");
    assert_eq!((count.from_ms, count.to_ms), (MARCH_1, APRIL_1));
    let april: Vec<u64> = (0..=30).map(|day| APRIL_1 + day * DAY).collect();
    assert_eq!(log.count(&april).refusals, 1);
}

#[test]
fn kinds_always_add_up_to_the_total() {
    let mut log = Log::new(MARCH_1);
    let all = [
        RefusalKind::Conflict,
        RefusalKind::StaleBase,
        RefusalKind::UnreadLines,
        RefusalKind::CommitHeld,
    ];
    for (index, kind) in all.iter().cycle().take(23).enumerate() {
        let index = index as u64;
        log.refused(
            &format!("s{}", index % 3),
            *kind,
            &[&format!("f{}.rs", index % 5)],
            index * 7 * HOUR,
        );
    }
    let count = log.count(&march());
    assert!(count.refusals > 0);
    assert_eq!(count.by_kind.values().sum::<u32>(), count.refusals);
    assert_eq!(count.by_workspace.values().sum::<u32>(), count.refusals);
    // Every kind is named, with zero when there was none.
    let empty = Log::new(MARCH_1).count(&march());
    let mut every: Vec<(String, u32)> = COUNT_KINDS
        .iter()
        .map(|kind| (kind.name().to_owned(), 0))
        .collect();
    every.sort();
    assert_eq!(kinds(&empty), every);
    assert_eq!(empty.refusals, 0);
}

#[test]
fn a_refusal_is_retried_through_when_its_session_was_next_admitted_on_the_same_files() {
    let mut log = Log::new(MARCH_1);
    log.refused("alice", RefusalKind::StaleBase, &["a.rs"], HOUR);
    log.refused("alice", RefusalKind::StaleBase, &["a.rs"], 2 * HOUR);
    log.refused("bob", RefusalKind::Conflict, &["b.rs"], 3 * HOUR);
    log.refused("carol", RefusalKind::CommitHeld, &["c.rs"], 4 * HOUR);
    let mut asked = Vec::new();
    let count = log.count_with(&march(), false, |session, workspace, after, paths| {
        asked.push((session.to_owned(), workspace.to_owned(), after));
        session == "alice" && paths == ["a.rs".to_owned()]
    });
    assert_eq!(count.refusals, 3);
    assert_eq!(count.retried_through, 1, "{count:?}");
    // Asked once per refusal counted, after its last repeat; a held commit
    // has no admission to look for.
    assert_eq!(
        asked,
        vec![
            ("alice".to_owned(), "work".to_owned(), 20),
            ("bob".to_owned(), "work".to_owned(), 30),
        ]
    );
}

#[test]
fn unanchored_generations_and_workspaces_are_reported_apart() {
    let mut log = Log::new(MARCH_1);
    log.refused_in(1, "one", "s", RefusalKind::Conflict, &["a.rs"], HOUR);
    log.refused_in(1, "two", "t", RefusalKind::Conflict, &["a.rs"], HOUR);
    log.refused_in(1, "two", "u", RefusalKind::Conflict, &["a.rs"], HOUR);
    // Generation 2 was started by a build that recorded no anchor.
    log.refused_in(2, "one", "s", RefusalKind::Conflict, &["a.rs"], HOUR);
    let count = log.count(&march());
    assert_eq!(count.refusals, 3);
    assert_eq!(count.undated, 1);
    assert_eq!(count.by_workspace["one"], 1);
    assert_eq!(count.by_workspace["two"], 2);
}

#[test]
fn a_read_that_starts_inside_the_month_says_truncated() {
    let mut log = Log::new(MARCH_1);
    log.refused("s", RefusalKind::Conflict, &["a.rs"], 2 * DAY);
    assert!(log.count_with(&march(), true, |_, _, _, _| false).truncated);
    assert!(
        !log.count_with(&march(), false, |_, _, _, _| false)
            .truncated
    );
    // The oldest refusal read is before the month: nothing was missed.
    let mut log = Log::new(MARCH_1 - DAY);
    log.refused("s", RefusalKind::Conflict, &["a.rs"], 0);
    log.refused("s", RefusalKind::Conflict, &["a.rs"], 3 * DAY);
    assert!(!log.count_with(&march(), true, |_, _, _, _| false).truncated);
}

#[test]
fn the_days_are_checked() {
    assert!(validate_count_days(&march()).is_ok());
    assert!(validate_count_days(&[MARCH_1]).is_err(), "one instant");
    assert!(validate_count_days(&[]).is_err());
    assert!(
        validate_count_days(&[MARCH_1, MARCH_1]).is_err(),
        "not increasing"
    );
    let too_many: Vec<u64> = (0..40).map(|day| MARCH_1 + day * DAY).collect();
    assert!(validate_count_days(&too_many).is_err());
}

#[test]
fn the_answer_round_trips_and_refuses_unknown_fields() {
    let mut log = Log::new(MARCH_1);
    log.refused("s", RefusalKind::Conflict, &["a.rs"], HOUR);
    let count = log.count(&march());
    let json = serde_json::to_string(&count).unwrap();
    assert_eq!(serde_json::from_str::<RefusalCount>(&json).unwrap(), count);
    let extra = json.replacen('{', r#"{"saved":1,"#, 1);
    assert!(serde_json::from_str::<RefusalCount>(&extra).is_err());
}

#[test]
fn workspaces_past_the_cap_are_counted_not_named() {
    let mut log = Log::new(MARCH_1);
    let workspaces = COUNT_WORKSPACES as u64 + 6;
    for index in 0..workspaces {
        let workspace = format!("w{index:03}");
        // The first workspace has two refusals, the rest one each.
        for file in if index == 0 {
            &["a.rs", "b.rs"][..]
        } else {
            &["a.rs"][..]
        } {
            log.refused_in(1, &workspace, "s", RefusalKind::Conflict, &[file], HOUR);
        }
    }
    let count = log.count(&march());
    assert_eq!(count.refusals as u64, workspaces + 1);
    assert_eq!(count.by_workspace.len(), COUNT_WORKSPACES);
    assert_eq!(count.by_workspace["w000"], 2, "the most refused is kept");
    assert_eq!(count.omitted_workspaces, 6);
    assert_eq!(
        count.by_workspace.values().sum::<u32>() + count.omitted_workspaces,
        count.refusals
    );
}

/// Review 4704296756: a window that starts before retention's cutoff may
/// be missing refusals and says so; one that starts at or after it lost
/// nothing and does not.
#[test]
fn a_window_reaching_before_the_retention_cutoff_is_marked_incomplete() {
    let mut log = Log::new(MARCH_1);
    log.refused("alice", RefusalKind::Conflict, &["a.rs"], 20 * DAY);
    let count = |since: u64| {
        count_refusals(
            &CountQuery {
                days: &march(),
                refusals: &log.events,
                anchors: &log.anchors,
                earlier: false,
                retained_since_ms: Some(since),
            },
            |_, _, _, _| Ok::<_, ()>(false),
        )
        .unwrap()
    };
    let cut = count(MARCH_1 + 10 * DAY);
    assert_eq!(cut.refusals, 1);
    assert_eq!(cut.retained_since_ms, Some(MARCH_1 + 10 * DAY));
    assert!(!cut.truncated, "the cap was not reached");
    assert_eq!(count(MARCH_1).retained_since_ms, None);
    assert_eq!(count(MARCH_1 - DAY).retained_since_ms, None);
    assert_eq!(log.count(&march()).retained_since_ms, None);
}
