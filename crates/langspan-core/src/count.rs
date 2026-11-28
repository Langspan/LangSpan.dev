//! A monthly count of refusals (roadmap H5), read from `WriteRefused`
//! events at query time: no counter is kept.
//!
//! A refusal is counted once per session, workspace, kind and path set per
//! local day, so a session refused again and again for the same edit is one
//! refusal. Local days are given by the caller as their starting instants;
//! this module knows no time zone. A refusal is placed in wall time by its
//! generation's `ClockAnchor`; a refusal of a generation without one (a
//! build before anchors) is `undated` and in no count.
//!
//! The count says what LangSpan did, never what it saved: a refusal is not
//! proof of avoided rework.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{ClockAnchor, Event, RecordedEvent, RefusalKind, is_zero};

/// Every kind, in the order counts are shown.
pub const COUNT_KINDS: [RefusalKind; 4] = [
    RefusalKind::Conflict,
    RefusalKind::StaleBase,
    RefusalKind::UnreadLines,
    RefusalKind::CommitHeld,
];
/// The most `WriteRefused` events one count reads, newest first; an older
/// part of the month is left out and the count says `truncated`.
pub const COUNT_REFUSALS: usize = 20_000;
/// The most day starts a window has: 31 days and the next day's start.
pub const COUNT_DAYS: usize = 32;
/// Workspaces named in `by_workspace`; the rest are counted.
pub const COUNT_WORKSPACES: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefusalCount {
    /// Wall-clock milliseconds since the Unix epoch: the first day's start,
    /// included, and the day after the last one's start, excluded.
    pub from_ms: u64,
    pub to_ms: u64,
    /// Refusals once per session, workspace, kind and path set per day.
    pub refusals: u32,
    /// Every kind of `COUNT_KINDS`, zero included: they add up to
    /// `refusals`.
    pub by_kind: BTreeMap<String, u32>,
    /// The workspaces with a refusal, at most `COUNT_WORKSPACES`.
    pub by_workspace: BTreeMap<String, u32>,
    /// Refusals of the workspaces not named in `by_workspace`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_workspaces: u32,
    /// Refusals whose session was admitted on the same paths after its last
    /// refusal of them: a retry let through.
    pub retried_through: u32,
    /// Refusals folded into one already counted (the same session,
    /// workspace, kind, paths and day).
    pub repeats: u32,
    /// The log holds more `WriteRefused` events than `COUNT_REFUSALS` and
    /// the oldest read may not be before the window: the count is of the
    /// newest ones only.
    pub truncated: bool,
    /// Refusals read that have no wall time; in no count.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub undated: u32,
    /// Retention removed the records written before this wall time, and
    /// the window starts before it: refusals of that part may be missing,
    /// so the count is not the window's complete total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_since_ms: Option<u64>,
}

/// What a count is asked to cover.
pub struct CountQuery<'a> {
    /// The local days' starts, earliest first, and the next day's start:
    /// at least two and at most `COUNT_DAYS + 1`.
    pub days: &'a [u64],
    /// The newest `WriteRefused` events, oldest first, at most
    /// `COUNT_REFUSALS`. Other events are ignored.
    pub refusals: &'a [RecordedEvent],
    /// The anchors of the generations they belong to.
    pub anchors: &'a [ClockAnchor],
    /// The log holds `WriteRefused` events older than the first given.
    pub earlier: bool,
    /// Retention removed the records written before this wall time.
    pub retained_since_ms: Option<u64>,
}

/// The day starts a count accepts: strictly increasing, between two and
/// `COUNT_DAYS + 1` of them.
pub fn validate_count_days(days: &[u64]) -> Result<(), String> {
    if days.len() < 2 || days.len() > COUNT_DAYS + 1 {
        return Err(format!(
            "a count needs between 2 and {} day starts, the last being the end; got {}",
            COUNT_DAYS + 1,
            days.len()
        ));
    }
    if days.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("the day starts must be strictly increasing".into());
    }
    Ok(())
}

/// One refusal as counted: who, where, what kind, which files, which day.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Key<'a> {
    workspace_id: &'a str,
    session_id: &'a str,
    kind: RefusalKind,
    paths: BTreeSet<&'a str>,
    day: usize,
}

/// Count the refusals in the window. `retried(session, workspace, after,
/// paths)` (paths sorted, without repeats) says whether the session was
/// admitted on exactly `paths` after
/// the event at sequence number `after`; it is asked once per refusal
/// counted, after its last repeat, and never for a held commit, which no
/// admission follows. Pure: every time and event is supplied.
pub fn count_refusals<E>(
    query: &CountQuery<'_>,
    mut retried: impl FnMut(&str, &str, u64, &[String]) -> Result<bool, E>,
) -> Result<RefusalCount, E> {
    let days = query.days;
    let (from_ms, to_ms) = (
        days.first().copied().unwrap_or(0),
        days.last().copied().unwrap_or(0),
    );
    let anchors: BTreeMap<u64, &ClockAnchor> = query
        .anchors
        .iter()
        .map(|anchor| (anchor.generation, anchor))
        .collect();
    // Each refusal counted, with the sequence number of its last repeat.
    let mut units: BTreeMap<Key<'_>, u64> = BTreeMap::new();
    let mut repeats = 0u32;
    let mut undated = 0u32;
    // The wall time of the oldest refusal read: `None` when it has none.
    let mut oldest: Option<Option<u64>> = None;
    for recorded in query.refusals {
        let Event::WriteRefused {
            session_id,
            workspace_id,
            kind,
            paths,
            at_ms,
            generation,
            ..
        } = &recorded.event
        else {
            continue;
        };
        let wall = anchors.get(generation).map(|anchor| anchor.wall(*at_ms));
        oldest.get_or_insert(wall);
        let Some(wall) = wall else {
            undated = undated.saturating_add(1);
            continue;
        };
        if wall < from_ms || wall >= to_ms {
            continue;
        }
        let key = Key {
            workspace_id,
            session_id,
            kind: *kind,
            paths: paths.iter().map(String::as_str).collect(),
            day: days
                .partition_point(|start| *start <= wall)
                .saturating_sub(1),
        };
        if units.insert(key, recorded.seq).is_some() {
            repeats = repeats.saturating_add(1);
        }
    }
    let mut by_kind: BTreeMap<String, u32> = COUNT_KINDS
        .iter()
        .map(|kind| (kind.name().to_owned(), 0))
        .collect();
    let mut by_workspace: BTreeMap<String, u32> = BTreeMap::new();
    let mut refusals = 0u32;
    let mut retried_through = 0u32;
    for (key, seq) in &units {
        refusals = refusals.saturating_add(1);
        let count = by_kind.entry(key.kind.name().into()).or_default();
        *count = count.saturating_add(1);
        let count = by_workspace.entry(key.workspace_id.into()).or_default();
        *count = count.saturating_add(1);
        if key.kind != RefusalKind::CommitHeld
            && retried(
                key.session_id,
                key.workspace_id,
                *seq,
                &key.paths
                    .iter()
                    .map(|path| (*path).to_owned())
                    .collect::<Vec<_>>(),
            )?
        {
            retried_through = retried_through.saturating_add(1);
        }
    }
    let mut omitted_workspaces = 0u32;
    if by_workspace.len() > COUNT_WORKSPACES {
        let mut ranked: Vec<(String, u32)> = by_workspace.into_iter().collect();
        // The most refused first; ties by name.
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (_, count) in ranked.drain(COUNT_WORKSPACES..) {
            omitted_workspaces = omitted_workspaces.saturating_add(count);
        }
        by_workspace = ranked.into_iter().collect();
    }
    let truncated = query.earlier
        && match oldest {
            Some(Some(first)) => first >= from_ms,
            _ => true,
        };
    Ok(RefusalCount {
        from_ms,
        to_ms,
        refusals,
        by_kind,
        by_workspace,
        omitted_workspaces,
        retried_through,
        repeats,
        truncated,
        undated,
        retained_since_ms: crate::today::retention_cut(query.retained_since_ms, from_ms),
    })
}
