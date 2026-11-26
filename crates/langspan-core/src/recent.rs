//! What changed recently in a repository, told to a session as it starts
//! (roadmap C3, [ADR 0023](../../../docs/adr/0023-messages-presence-and-session-continuity-after-launch.md)).
//!
//! A session that starts after a busy hour hears from the ordinary briefing
//! only about peers active now. This section adds what the current daemon
//! run saw within a window: which files other sessions edited and how many
//! sessions each, interface changes in this checkout, files that were
//! refused or edited from two sides, and the ended sessions of this checkout
//! whose edits may still be uncommitted. It is derived on request from core
//! state, itself derived from the event log; nothing new is stored, nothing
//! is held or refused because of it, and it carries no prompt text and no
//! file content.
//!
//! The window is measured on the daemon's own clock and only within the
//! current generation: events from before the daemon's latest boot record
//! never count, whatever their age, and a restart cannot stretch or shift
//! the window.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    BRIEF_PATHS, CoreError, InterfaceChange, LivenessStatus, Session, State, error, is_false,
    is_zero,
};

/// The window when the daemon's configuration names none: four hours.
pub const RECENT_WINDOW_MS: u64 = 4 * 60 * 60 * 1000;
/// The shortest and longest window a configuration may name.
pub const MIN_RECENT_WINDOW_MS: u64 = 60 * 1000;
pub const MAX_RECENT_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// Items per list, at most; the rest are counted in `omitted`.
pub const RECENT_CHANGED: usize = 8;
pub const RECENT_INTERFACES: usize = 4;
pub const RECENT_CONTESTED: usize = 4;
pub const RECENT_LEFT: usize = 4;
/// Ended sessions proposed for `left_uncommitted`, at most, before the
/// caller confirms them against the line journal and keeps `RECENT_LEFT`.
pub const RECENT_LEFT_CANDIDATES: usize = 8;
/// Changes listed per interface notice, at most.
pub const RECENT_NOTICE_CHANGES: usize = 4;
/// The most bytes the section takes in a JSON reply, every string escaped,
/// so a briefing carrying it stays within one protocol frame beside the
/// peers' own budget. Items that do not fit are counted in `omitted`.
pub const RECENT_BYTES: usize = 12 * 1024;
/// Refusals with their paths kept for this section, newest first.
pub const MAX_PATH_REFUSALS: usize = 64;

/// What changed recently in the asking session's repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecentChanges {
    /// How far back this looked, on the daemon's clock, within the current
    /// daemon run.
    pub window_ms: u64,
    /// The asking session's checkout, where `left_uncommitted` is confirmed.
    pub root: String,
    /// Files other sessions of this repository edited, most sessions first,
    /// then newest.
    pub changed_paths: Vec<ChangedPath>,
    /// Interface changes in this checkout, newest first, whatever files the
    /// asking session works on.
    pub interface_changes: Vec<RecentInterfaceChange>,
    /// Files with a refused write, or edited by two or more sessions.
    pub contested_paths: Vec<ContestedPath>,
    /// Ended sessions of this checkout and the files they edited. The core
    /// proposes candidates; the caller keeps only those whose journaled
    /// lines are still uncommitted, and drops the rest.
    pub left_uncommitted: Vec<LeftUncommitted>,
    /// Items left out by the per-list caps. Counted, never silently dropped.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: u32,
}

impl RecentChanges {
    pub fn is_empty(&self) -> bool {
        self.changed_paths.is_empty()
            && self.interface_changes.is_empty()
            && self.contested_paths.is_empty()
            && self.left_uncommitted.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChangedPath {
    pub path: String,
    /// Distinct sessions that edited it within the window.
    pub sessions: u32,
    /// Age of the newest of those edits.
    pub age_ms: u64,
    /// Every counted edit was in another worktree of this repository, so
    /// the change is on another branch, not in this checkout.
    #[serde(default, skip_serializing_if = "is_false")]
    pub elsewhere: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecentInterfaceChange {
    pub path: String,
    /// The session that last reported editing `path`: a correlation.
    pub by_session: Option<String>,
    pub changes: Vec<InterfaceChange>,
    /// Files of this checkout that mention a changed name, by name match.
    pub dependents: Vec<String>,
    pub age_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContestedPath {
    pub path: String,
    /// Writes or commits of it refused within the window.
    pub refusals: u32,
    /// Distinct sessions whose edits of it landed within the window.
    pub sessions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeftUncommitted {
    pub session_id: String,
    /// Files it edited within the window, newest first: from the core every
    /// retained edit that fits `RECENT_BYTES`; once the caller confirms
    /// them, the uncommitted ones, at most `BRIEF_PATHS`.
    pub paths: Vec<String>,
    /// Age of its newest edit.
    pub age_ms: u64,
}

/// A refused write with its paths, kept for this section. Derived on
/// replay from `WriteRefused` events; never stored or sent on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRefusal {
    pub workspace_id: String,
    pub paths: Vec<String>,
    pub at_ms: u64,
    pub generation: u64,
}

/// Remember a refusal of the current run, newest first; another run's go.
pub(crate) fn remember_refusal(
    state: &mut State,
    workspace_id: &str,
    paths: &[String],
    at_ms: u64,
    generation: u64,
) {
    state
        .path_refusals
        .retain(|kept| kept.generation == generation);
    state.path_refusals.insert(
        0,
        PathRefusal {
            workspace_id: workspace_id.to_owned(),
            paths: paths.to_vec(),
            at_ms,
            generation,
        },
    );
    state.path_refusals.truncate(MAX_PATH_REFUSALS);
}

/// The window asked for, or the default; outside the accepted range is an
/// error, never a silently changed window.
pub(crate) fn window(asked: Option<u64>) -> Result<u64, CoreError> {
    let window = asked.unwrap_or(RECENT_WINDOW_MS);
    if !(MIN_RECENT_WINDOW_MS..=MAX_RECENT_WINDOW_MS).contains(&window) {
        return Err(error(
            "invalid_limit",
            "A recent window is between 60000 ms (one minute) and 86400000 ms (one day); set recent_window_ms in config.json within it",
        ));
    }
    Ok(window)
}

/// Whether `other` is the asking session, a subagent it started, or the
/// session that started it: their work is not news to it.
fn own(me: &Session, other: &str) -> bool {
    other == me.session_id
        || other
            .strip_prefix(me.session_id.as_str())
            .is_some_and(|rest| rest.starts_with('.'))
        || me.parent.as_deref() == Some(other)
}

/// What changed recently, for `me`, within `window_ms` before `now_ms`.
pub(crate) fn recent(state: &State, me: &Session, window_ms: u64, now_ms: u64) -> RecentChanges {
    let since = now_ms.saturating_sub(window_ms);
    let fresh = |at_ms: u64| at_ms >= since && at_ms <= now_ms;
    let mut omitted = 0_usize;
    // Path -> (sessions, newest edit, edited in this checkout).
    let mut edits: BTreeMap<&str, (BTreeSet<&str>, u64, bool)> = BTreeMap::new();
    let mut left = Vec::new();
    for session in state.sessions.values() {
        if session.generation != state.generation
            || own(me, &session.session_id)
            || !crate::related_workspace(state, &me.workspace_id, &session.workspace_id)
        {
            continue;
        }
        let Some(activity) = state
            .activity
            .get(&session.session_id)
            .filter(|activity| activity.generation == state.generation)
        else {
            continue;
        };
        let here = session.workspace_id == me.workspace_id;
        let touched: Vec<_> = activity
            .touched
            .iter()
            .filter(|touch| fresh(touch.at_ms))
            .collect();
        for touch in &touched {
            let entry = edits
                .entry(touch.path.as_str())
                .or_insert_with(|| (BTreeSet::new(), 0, false));
            entry.0.insert(session.session_id.as_str());
            entry.1 = entry.1.max(touch.at_ms);
            entry.2 |= here;
        }
        let exited = state
            .session_liveness
            .get(&session.session_id)
            .is_some_and(|liveness| liveness.status == LivenessStatus::Exited);
        if here && (activity.ended || exited) && !touched.is_empty() {
            left.push(LeftUncommitted {
                session_id: session.session_id.clone(),
                // Every retained edit: the caller confirms them against
                // the journal first and only then keeps `BRIEF_PATHS`, so
                // newer committed files never hide an older uncommitted one.
                paths: touched.iter().map(|touch| touch.path.clone()).collect(),
                age_ms: now_ms.saturating_sub(touched[0].at_ms),
            });
        }
    }
    let mut refusals: BTreeMap<&str, u32> = BTreeMap::new();
    for refusal in &state.path_refusals {
        if refusal.generation == state.generation
            && fresh(refusal.at_ms)
            && crate::related_workspace(state, &me.workspace_id, &refusal.workspace_id)
        {
            for path in &refusal.paths {
                *refusals.entry(path.as_str()).or_default() += 1;
            }
        }
    }
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let mut changed: Vec<(ChangedPath, u64)> = edits
        .iter()
        .map(|(path, (sessions, at_ms, here))| {
            (
                ChangedPath {
                    path: (*path).to_owned(),
                    sessions: count(sessions.len()),
                    age_ms: now_ms.saturating_sub(*at_ms),
                    elsewhere: !here,
                },
                *at_ms,
            )
        })
        .collect();
    changed.sort_by(|(a, a_at), (b, b_at)| {
        (b.sessions, b_at, &a.path).cmp(&(a.sessions, a_at, &b.path))
    });
    let mut contested: Vec<(ContestedPath, u64)> = edits
        .keys()
        .chain(refusals.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|path| {
            let (sessions, at_ms) = edits
                .get(path)
                .map(|(sessions, at_ms, _)| (sessions.len(), *at_ms))
                .unwrap_or((0, 0));
            let refused = refusals.get(path).copied().unwrap_or(0);
            (refused > 0 || sessions >= 2).then(|| {
                (
                    ContestedPath {
                        path: (*path).to_owned(),
                        refusals: refused,
                        sessions: count(sessions),
                    },
                    at_ms,
                )
            })
        })
        .collect();
    contested.sort_by(|(a, a_at), (b, b_at)| {
        (b.refusals, b.sessions, b_at, &a.path).cmp(&(a.refusals, a.sessions, a_at, &b.path))
    });
    let interfaces: Vec<RecentInterfaceChange> = state
        .interface_notices
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|notice| {
            notice.generation == state.generation
                && fresh(notice.at_ms)
                && !notice
                    .by_session
                    .as_deref()
                    .is_some_and(|session| own(me, session))
        })
        .map(|notice| {
            omitted += notice.changes.len().saturating_sub(RECENT_NOTICE_CHANGES);
            RecentInterfaceChange {
                path: notice.path.clone(),
                by_session: notice.by_session.clone(),
                changes: notice
                    .changes
                    .iter()
                    .take(RECENT_NOTICE_CHANGES)
                    .cloned()
                    .collect(),
                dependents: notice
                    .dependents
                    .iter()
                    .take(BRIEF_PATHS)
                    .cloned()
                    .collect(),
                age_ms: now_ms.saturating_sub(notice.at_ms),
            }
        })
        .collect();
    left.sort_by(|a, b| (a.age_ms, &a.session_id).cmp(&(b.age_ms, &b.session_id)));
    let mut keep = |items: usize, cap: usize| {
        omitted += items.saturating_sub(cap);
        cap
    };
    let changed_cap = keep(changed.len(), RECENT_CHANGED);
    let contested_cap = keep(contested.len(), RECENT_CONTESTED);
    let interfaces_cap = keep(interfaces.len(), RECENT_INTERFACES);
    let left_cap = keep(left.len(), RECENT_LEFT_CANDIDATES);
    let mut recent = RecentChanges {
        window_ms,
        root: state
            .workspaces
            .get(&me.workspace_id)
            .map(|workspace| workspace.root.clone())
            .unwrap_or_default(),
        changed_paths: changed
            .into_iter()
            .take(changed_cap)
            .map(|(path, _)| path)
            .collect(),
        interface_changes: interfaces.into_iter().take(interfaces_cap).collect(),
        contested_paths: contested
            .into_iter()
            .take(contested_cap)
            .map(|(path, _)| path)
            .collect(),
        left_uncommitted: left.into_iter().take(left_cap).collect(),
        omitted: 0,
    };
    omitted += within_bytes(&mut recent, RECENT_BYTES);
    recent.omitted = count(omitted);
    recent
}

/// The length of `value` as a JSON string, quotes and escapes included, as
/// the protocol encodes it.
pub(crate) fn json_len(value: &str) -> usize {
    2 + value
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            c if c < ' ' => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}

/// Room for a JSON object's field names, punctuation and numbers, beyond
/// its strings: more than any of these objects uses.
const OBJECT_ROOM: usize = 160;
const INTERFACE_CHANGE_ROOM: usize = 320;

fn interface_len(change: &RecentInterfaceChange) -> usize {
    OBJECT_ROOM
        + json_len(&change.path)
        + change.by_session.as_deref().map_or(0, json_len)
        + change
            .changes
            .iter()
            .map(|change| {
                INTERFACE_CHANGE_ROOM
                    + [&change.name, &change.kind, &change.change]
                        .into_iter()
                        .chain(change.before.as_ref())
                        .chain(change.after.as_ref())
                        .map(|text| json_len(text))
                        .sum::<usize>()
            })
            .sum::<usize>()
        + change
            .dependents
            .iter()
            .map(|path| json_len(path) + 1)
            .sum::<usize>()
}

/// Keep the section within `budget` encoded bytes, in its own order, and
/// return how many items did not fit. An ended session keeps the files
/// that fit, newest first, and is counted only when none do.
fn within_bytes(recent: &mut RecentChanges, budget: usize) -> usize {
    let mut used = OBJECT_ROOM + json_len(&recent.root);
    let mut omitted = 0;
    let mut fits = |cost: usize| {
        if used + cost > budget {
            omitted += 1;
            false
        } else {
            used += cost;
            true
        }
    };
    recent
        .changed_paths
        .retain(|path| fits(OBJECT_ROOM + json_len(&path.path)));
    recent
        .interface_changes
        .retain(|change| fits(interface_len(change)));
    recent
        .contested_paths
        .retain(|path| fits(OBJECT_ROOM + json_len(&path.path)));
    let mut omitted_sessions = 0;
    recent.left_uncommitted.retain_mut(|left| {
        let session = OBJECT_ROOM + json_len(&left.session_id);
        if used + session > budget {
            omitted_sessions += 1;
            return false;
        }
        let mut cost = session;
        left.paths.retain(|path| {
            let next = json_len(path) + 1;
            if used + cost + next > budget {
                return false;
            }
            cost += next;
            true
        });
        if left.paths.is_empty() {
            omitted_sessions += 1;
            return false;
        }
        used += cost;
        true
    });
    omitted + omitted_sessions
}

#[cfg(test)]
mod tests {
    use crate::recent::*;

    fn awkward(n: usize) -> String {
        format!("{n}/\"\\\n\u{1}\u{7f}é{}", "\"".repeat(n % 300))
    }

    #[test]
    fn the_byte_budget_bounds_the_encoded_section_and_counts_the_rest() {
        let changes = |n: usize| InterfaceChange {
            name: awkward(n),
            kind: "function".into(),
            change: "signature_changed".into(),
            before: Some(awkward(n + 1)),
            after: Some(awkward(n + 2)),
            before_arity: Some(crate::Arity {
                min: u32::MAX,
                max: Some(u32::MAX),
                receiver: true,
            }),
            after_arity: Some(crate::Arity {
                min: u32::MAX,
                max: Some(u32::MAX),
                receiver: true,
            }),
            before_count: Some(u32::MAX),
            after_count: Some(u32::MAX),
        };
        let mut recent = RecentChanges {
            window_ms: u64::MAX,
            root: awkward(500),
            changed_paths: (0..RECENT_CHANGED)
                .map(|n| ChangedPath {
                    path: awkward(n * 37),
                    sessions: u32::MAX,
                    age_ms: u64::MAX,
                    elsewhere: true,
                })
                .collect(),
            interface_changes: (0..RECENT_INTERFACES)
                .map(|n| RecentInterfaceChange {
                    path: awkward(n),
                    by_session: Some(awkward(n + 3)),
                    changes: (0..RECENT_NOTICE_CHANGES).map(changes).collect(),
                    dependents: (0..BRIEF_PATHS).map(awkward).collect(),
                    age_ms: u64::MAX,
                })
                .collect(),
            contested_paths: (0..RECENT_CONTESTED)
                .map(|n| ContestedPath {
                    path: awkward(n + 11),
                    refusals: u32::MAX,
                    sessions: u32::MAX,
                })
                .collect(),
            left_uncommitted: (0..RECENT_LEFT_CANDIDATES)
                .map(|n| LeftUncommitted {
                    session_id: awkward(n),
                    paths: (0..crate::MAX_TOUCHED).map(|p| awkward(p * 13)).collect(),
                    age_ms: u64::MAX,
                })
                .collect(),
            omitted: u32::MAX,
        };
        let before = serde_json::to_vec(&recent).unwrap().len();
        assert!(before > RECENT_BYTES, "{before}");
        let sessions = recent.left_uncommitted.len();
        let omitted = within_bytes(&mut recent, RECENT_BYTES);
        let after = serde_json::to_vec(&recent).unwrap().len();
        assert!(after <= RECENT_BYTES, "{after} bytes");
        assert!(omitted > 0);
        assert!(!recent.changed_paths.is_empty());
        assert!(recent.left_uncommitted.len() < sessions);
        // Each estimate is at least what the encoding takes.
        for text in [awkward(0), awkward(299), String::new()] {
            assert!(json_len(&text) >= serde_json::to_string(&text).unwrap().len());
        }
    }
}
