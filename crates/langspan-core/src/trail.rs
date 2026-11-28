//! Trails: what an ended session read, edited and left, told once to the
//! next session of its checkout (roadmap C9, [ADR 0023](../../../docs/adr/0023-messages-presence-and-session-continuity-after-launch.md)).
//!
//! At a session's end the core builds a trail from recorded events only:
//! the owner's task line (unless the prompt summary is off), the files the
//! session and its subagents read and edited, check results recorded after
//! its last write, what it still held, the messages it sent that no one has
//! read, and its last declared intent. The end hook adds, from the line
//! journal on this machine, the line ranges of its edits and the files it
//! left uncommitted (`TrailFacts`). Nothing here is model output: no
//! transcript, no final reply, no file content.
//!
//! The next session of the same checkout to start or be prompted takes the
//! trail, once, under the rule a handoff follows (`takes_handoff`): a
//! subagent never takes one, and a headless session takes only a headless
//! session's trail.

use serde::{Deserialize, Serialize};

use crate::{
    CheckResult, CoreError, LineRange, PresenceKind, Recipient, Session, State, error, is_false,
    is_zero, live_lease, validate_relative_path,
};

/// Files per list of a trail, at most; the rest are counted.
pub const TRAIL_FILES: usize = 8;
/// Line ranges kept per edited file.
pub const TRAIL_RANGES: usize = 4;
/// Check results kept per trail.
pub const TRAIL_CHECKS: usize = 2;
/// Unread sent messages named per trail.
pub const TRAIL_WAITING: usize = 4;
/// Trails kept per workspace, newest first; the oldest is dropped.
pub const MAX_TRAILS: usize = 16;
/// The most bytes a trail takes in a briefing, in at most three lines.
pub const TRAIL_BYTES: usize = 600;
/// The most bytes a trail takes encoded as JSON in a reply. Escaped paths
/// and texts can take several times their length, so a trail is cut to
/// this, oldest items first, when it is recorded and again when it is shown.
pub const TRAIL_JSON_BYTES: usize = 12 * 1024;
/// Files the end hook may report per list.
pub const MAX_TRAIL_FACTS: usize = 64;

/// What the end hook read from the line journal for the ending session and
/// its subagents. Paths and line numbers only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrailFacts {
    /// Journaled files and where the lines they wrote are now, newest write
    /// first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited: Vec<TrailFile>,
    /// Files whose journaled lines are still among the uncommitted changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncommitted: Vec<String>,
    /// The hook's prompt summary is off: the trail has no task line.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_task: bool,
    /// The journal could not be read in the hook's time: the line ranges
    /// and uncommitted files are missing or incomplete.
    #[serde(default, skip_serializing_if = "is_false")]
    pub partial: bool,
}

/// One edited file and the lines of it the session wrote, numbered in the
/// file as the session left it. Empty when they are not known.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrailFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<LineRange>,
    /// More ranges than `TRAIL_RANGES`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub more_lines: bool,
}

/// A check recorded in the workspace after the session's last write.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrailCheck {
    pub name: String,
    pub result: CheckResult,
}

/// What one ended session left, from recorded events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Trail {
    pub session_id: String,
    pub workspace_id: String,
    /// The owner's task line, data from that session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The session's last declared intent, data from that session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Files it read, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_read: u32,
    /// Files it edited, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited: Vec<TrailFile>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_edited: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<TrailCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncommitted: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_uncommitted: u32,
    /// Files it still held when it ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<String>,
    /// IDs of messages it sent that a recipient has not read yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
    /// Line ranges and uncommitted files are missing or incomplete: the end
    /// hook ran out of time, or an earlier client sent no journal facts.
    #[serde(default, skip_serializing_if = "is_false")]
    pub partial: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub headless: bool,
    /// The daemon's clock, within `generation`.
    pub at_ms: u64,
    pub generation: u64,
    /// The session that was told it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_by: Option<String>,
}

/// A trail as a briefing or a listing shows it: with its age, when it was
/// recorded in the current daemon run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrailView {
    pub trail: Trail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
}

pub(crate) fn validate_facts(facts: &TrailFacts) -> Result<(), CoreError> {
    if facts.edited.len() > MAX_TRAIL_FACTS || facts.uncommitted.len() > MAX_TRAIL_FACTS {
        return Err(error(
            "invalid_limit",
            "A session end reports at most 64 edited and 64 uncommitted files",
        ));
    }
    for file in &facts.edited {
        validate_relative_path(&file.path)?;
        if file.lines.len() > TRAIL_RANGES
            || file
                .lines
                .iter()
                .any(|range| range.start_line == 0 || range.start_line > range.end_line)
        {
            return Err(error(
                "invalid_lines",
                "An edited file names at most 4 one-based line ranges, each starting at or before its end",
            ));
        }
    }
    for path in &facts.uncommitted {
        validate_relative_path(path)?;
    }
    Ok(())
}

pub(crate) fn validate_trail(state: &State, trail: &Trail) -> Result<(), CoreError> {
    let session = state
        .sessions
        .get(&trail.session_id)
        .ok_or_else(|| error("invalid_event", "A trail names a session that is not known"))?;
    if session.workspace_id != trail.workspace_id || session.parent.is_some() {
        return Err(error(
            "invalid_event",
            "A trail belongs to a top-level session of its own workspace",
        ));
    }
    if trail.read.len() > TRAIL_FILES
        || trail.edited.len() > TRAIL_FILES
        || trail.uncommitted.len() > TRAIL_FILES
        || trail.held.len() > TRAIL_FILES
        || trail.checks.len() > TRAIL_CHECKS
        || trail.waiting_on.len() > TRAIL_WAITING
    {
        return Err(error("invalid_event", "A trail is over its bounds"));
    }
    Ok(())
}

/// The session and its subagents, at any depth the parent links record.
fn own_sessions<'a>(state: &'a State, session_id: &'a str) -> Vec<&'a str> {
    let mut own = vec![session_id];
    let mut index = 0;
    while index < own.len() {
        let parent = own[index];
        let children: Vec<&str> = state
            .sessions
            .values()
            .filter(|session| session.parent.as_deref() == Some(parent))
            .map(|session| session.session_id.as_str())
            .filter(|id| !own.contains(id))
            .collect();
        own.extend(children);
        index += 1;
    }
    own
}

/// Distinct items, newest first, at most `limit`, and how many more.
fn newest<T: Clone + PartialEq>(mut items: Vec<(u64, T)>, limit: usize) -> (Vec<T>, u32) {
    items.sort_by_key(|(at, _)| core::cmp::Reverse(*at));
    let mut kept: Vec<T> = Vec::new();
    for (_, item) in items {
        if !kept.contains(&item) {
            kept.push(item);
        }
    }
    let more = kept.len().saturating_sub(limit);
    kept.truncate(limit);
    (kept, u32::try_from(more).unwrap_or(u32::MAX))
}

/// The trail `session` leaves as it ends now, or none: a subagent leaves
/// none of its own (its parent's trail has its work), and neither does a
/// session that read and wrote nothing.
pub(crate) fn build(
    state: &State,
    session: &Session,
    facts: Option<&TrailFacts>,
    now_ms: u64,
) -> Option<Trail> {
    if session.parent.is_some() {
        return None;
    }
    let own = own_sessions(state, &session.session_id);
    // Times compare only within one generation: a newer one sorts first.
    let order = |generation: u64, at_ms: u64| (generation << 48) | (at_ms & ((1 << 48) - 1));
    let activities: Vec<_> = own
        .iter()
        .filter_map(|id| state.activity.get(*id))
        .collect();
    let (read, more_read) = newest(
        activities
            .iter()
            .flat_map(|activity| {
                activity
                    .looking
                    .iter()
                    .filter(|looked| looked.kind == PresenceKind::Read)
                    .map(|looked| {
                        (
                            order(activity.generation, looked.at_ms),
                            looked.path.clone(),
                        )
                    })
            })
            .collect(),
        TRAIL_FILES,
    );
    let journaled = facts.map(|facts| facts.edited.as_slice()).unwrap_or(&[]);
    // Reported edits first, newest first; then files only the journal
    // knows (a shell command's), in its order.
    let (mut edited, _) = newest(
        activities
            .iter()
            .flat_map(|activity| {
                activity
                    .touched
                    .iter()
                    .map(|touch| (order(activity.generation, touch.at_ms), touch.path.clone()))
            })
            .collect(),
        usize::MAX,
    );
    for file in journaled {
        if !edited.contains(&file.path) {
            edited.push(file.path.clone());
        }
    }
    let more_edited = u32::try_from(edited.len().saturating_sub(TRAIL_FILES)).unwrap_or(u32::MAX);
    edited.truncate(TRAIL_FILES);
    if read.is_empty() && edited.is_empty() {
        return None;
    }
    let edited: Vec<TrailFile> = edited
        .into_iter()
        .map(|path| {
            journaled
                .iter()
                .find(|file| file.path == path)
                .cloned()
                .unwrap_or(TrailFile {
                    path,
                    lines: Vec::new(),
                    more_lines: false,
                })
        })
        .collect();
    let no_task = facts.is_some_and(|facts| facts.no_task);
    let summary = state
        .activity
        .get(&session.session_id)
        .and_then(|activity| activity.summary.clone())
        .filter(|_| !no_task);
    // Checks recorded in this run after the last write of this run.
    let last_write = activities
        .iter()
        .filter(|activity| activity.generation == state.generation)
        .flat_map(|activity| activity.touched.iter().map(|touch| touch.at_ms))
        .max();
    let mut checks: Vec<TrailCheck> = Vec::new();
    if let Some(last_write) = last_write {
        for record in state
            .checks
            .get(&session.workspace_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter(|record| {
                record.generation == state.generation
                    && record.recorded_ms >= last_write
                    && record.inputs.is_empty()
            })
        {
            if checks.len() == TRAIL_CHECKS {
                break;
            }
            if !checks.iter().any(|check| check.name == record.name) {
                checks.push(TrailCheck {
                    name: record.name.clone(),
                    result: record.result.clone(),
                });
            }
        }
    }
    let (uncommitted, more_uncommitted) = facts.map_or((Vec::new(), 0), |facts| {
        let mut paths: Vec<String> = Vec::new();
        for path in &facts.uncommitted {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        let more = paths.len().saturating_sub(TRAIL_FILES);
        paths.truncate(TRAIL_FILES);
        (paths, u32::try_from(more).unwrap_or(u32::MAX))
    });
    let mut held: Vec<String> = state
        .leases
        .values()
        .filter(|lease| {
            own.contains(&lease.session_id.as_str()) && live_lease(state, lease, now_ms)
        })
        .flat_map(|lease| lease.paths.iter().cloned())
        .collect();
    held.sort();
    held.dedup();
    held.truncate(TRAIL_FILES);
    // Messages it sent that someone still has to read; a handoff is
    // shown on its own, before the trail.
    let waiting_on: Vec<String> = state
        .messages
        .get(&session.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|message| {
            message.from.as_deref() == Some(session.session_id.as_str())
                && !message.retracted
                && !matches!(message.to, Recipient::NextSession {})
        })
        .take(TRAIL_WAITING)
        .map(|message| message.message_id.clone())
        .collect();
    let mut trail = Trail {
        session_id: session.session_id.clone(),
        workspace_id: session.workspace_id.clone(),
        summary,
        intent: state
            .intents
            .get(&session.session_id)
            .map(|intent| intent.summary.clone()),
        read,
        more_read,
        edited,
        more_edited,
        checks,
        uncommitted,
        more_uncommitted,
        held,
        waiting_on,
        partial: facts.is_none_or(|facts| facts.partial),
        headless: session.headless,
        at_ms: now_ms,
        generation: state.generation,
        taken_by: None,
    };
    fit(&mut trail);
    Some(trail)
}

/// A trail's JSON beside its strings: field names, punctuation, numbers
/// and flags, and a view's `age_ms`. More than any trail uses.
const TRAIL_JSON_ROOM: usize = 512;
/// One edited file's JSON beside its path and line ranges.
const FILE_JSON_ROOM: usize = 48;
/// One line range's JSON with the largest numbers, and its comma.
const RANGE_JSON: usize = 56;
/// One check's JSON beside its name, the longest result included.
const CHECK_JSON_ROOM: usize = 64;
/// The most a task line or an intent keeps of a trail's encoding, so that
/// one long escaped text cannot take the room of every file.
const TEXT_JSON_BYTES: usize = TRAIL_JSON_BYTES / 4;

/// At least the bytes `trail` takes as JSON, inside a view or alone.
pub(crate) fn json_cost(trail: &Trail) -> usize {
    let text = |value: &str| crate::recent::json_len(value) + 1;
    let texts = |values: &[String]| values.iter().map(|value| text(value)).sum::<usize>();
    TRAIL_JSON_ROOM
        + text(&trail.session_id)
        + text(&trail.workspace_id)
        + [&trail.summary, &trail.intent, &trail.taken_by]
            .into_iter()
            .flatten()
            .map(|value| text(value))
            .sum::<usize>()
        + texts(&trail.read)
        + texts(&trail.uncommitted)
        + texts(&trail.held)
        + texts(&trail.waiting_on)
        + trail
            .edited
            .iter()
            .map(|file| FILE_JSON_ROOM + text(&file.path) + file.lines.len() * RANGE_JSON)
            .sum::<usize>()
        + trail
            .checks
            .iter()
            .map(|check| CHECK_JSON_ROOM + text(&check.name))
            .sum::<usize>()
}

/// Halve `text` at a character boundary, or drop it once nothing is left.
/// False when there is no text.
fn shorten(text: &mut Option<String>) -> bool {
    let Some(value) = text else {
        return false;
    };
    let mut end = value.len() / 2;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        *text = None;
    } else {
        value.truncate(end);
    }
    true
}

/// Cut `trail` until its JSON takes at most `TRAIL_JSON_BYTES`: a long task
/// line and intent to their share first, then the oldest files read, left
/// uncommitted, edited and held, in that order. Dropped read, uncommitted
/// and edited files are counted in the trail's `more_` fields.
pub(crate) fn fit(trail: &mut Trail) {
    for text in [&mut trail.summary, &mut trail.intent] {
        while text
            .as_deref()
            .is_some_and(|value| crate::recent::json_len(value) > TEXT_JSON_BYTES)
        {
            shorten(text);
        }
    }
    let count = |more: &mut u32| *more = more.saturating_add(1);
    while json_cost(trail) > TRAIL_JSON_BYTES {
        if trail.read.pop().is_some() {
            count(&mut trail.more_read);
        } else if trail.uncommitted.pop().is_some() {
            count(&mut trail.more_uncommitted);
        } else if trail.edited.pop().is_some() {
            count(&mut trail.more_edited);
        } else if trail.held.pop().is_none()
            && !shorten(&mut trail.intent)
            && !shorten(&mut trail.summary)
        {
            break;
        }
    }
}

/// A trail as shown, within `TRAIL_JSON_BYTES` however it was stored.
pub(crate) fn view(state: &State, trail: &Trail, now_ms: u64) -> TrailView {
    let mut shown = trail.clone();
    fit(&mut shown);
    TrailView {
        trail: shown,
        age_ms: (trail.generation == state.generation).then(|| now_ms.saturating_sub(trail.at_ms)),
    }
}

/// The newest trail `me` may take now, if any: at a start or a prompt
/// (`handoffs`), in its own checkout, from another top-level session that
/// is still ended, under the rule a handoff follows.
pub(crate) fn owed<'a>(state: &'a State, me: &Session, handoffs: bool) -> Option<&'a Trail> {
    if !handoffs || me.parent.is_some() {
        return None;
    }
    state.trails.get(&me.workspace_id)?.iter().find(|trail| {
        trail.taken_by.is_none()
            && trail.session_id != me.session_id
            && (!me.headless || trail.headless)
            && state
                .activity
                .get(&trail.session_id)
                .is_some_and(|activity| activity.ended)
    })
}

/// The newest `limit` trails of a workspace.
pub(crate) fn listed(
    state: &State,
    workspace_id: &str,
    limit: usize,
    now_ms: u64,
) -> Vec<TrailView> {
    state
        .trails
        .get(workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(limit)
        .map(|trail| view(state, trail, now_ms))
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::trail::*;

    /// Every field at its largest: full lists of escaped paths, the widest
    /// numbers and results, and multi-byte texts.
    fn largest(kind: char) -> Trail {
        let path = |n: usize| format!("{}{n:03}", kind.to_string().repeat(509));
        let paths = |from: usize| (from..from + TRAIL_FILES).map(path).collect::<Vec<_>>();
        let range = LineRange {
            start_line: u32::MAX,
            end_line: u32::MAX,
        };
        Trail {
            session_id: path(900),
            workspace_id: path(901),
            summary: Some("é\u{1}".repeat(170)),
            intent: Some("é\u{1}".repeat(680)),
            read: paths(0),
            more_read: u32::MAX,
            edited: paths(100)
                .into_iter()
                .map(|path| TrailFile {
                    path,
                    lines: vec![range; TRAIL_RANGES],
                    more_lines: true,
                })
                .collect(),
            more_edited: u32::MAX,
            checks: (0..TRAIL_CHECKS)
                .map(|n| TrailCheck {
                    name: path(200 + n),
                    result: CheckResult::Failed {
                        exit_code: i32::MIN,
                    },
                })
                .collect(),
            uncommitted: paths(300),
            more_uncommitted: u32::MAX,
            held: paths(400),
            waiting_on: (500..500 + TRAIL_WAITING).map(path).collect(),
            partial: true,
            headless: true,
            at_ms: u64::MAX,
            generation: u64::MAX,
            taken_by: Some(path(902)),
        }
    }

    fn encoded(trail: &Trail) -> usize {
        serde_json::to_vec(&TrailView {
            trail: trail.clone(),
            age_ms: Some(u64::MAX),
        })
        .unwrap()
        .len()
    }

    #[test]
    fn the_cost_of_a_trail_is_never_below_its_encoding() {
        for kind in ['a', '"', '\u{1}', 'é'] {
            let full = largest(kind);
            assert!(json_cost(&full) >= encoded(&full), "{kind:?}");
            let mut fitted = full.clone();
            fit(&mut fitted);
            assert!(json_cost(&fitted) >= encoded(&fitted), "{kind:?}");
        }
    }

    #[test]
    fn a_fitted_trail_keeps_within_its_bound_and_counts_what_it_dropped() {
        for kind in ['"', '\u{1}', 'é'] {
            let mut trail = largest(kind);
            // Short identifiers, as the core admits them.
            trail.session_id = "claude-code.s1".into();
            trail.workspace_id = "w1".into();
            trail.taken_by = Some("codex.s2".into());
            trail.waiting_on = vec!["m1".into()];
            trail.more_read = 0;
            trail.more_edited = 0;
            trail.more_uncommitted = 0;
            fit(&mut trail);
            assert!(encoded(&trail) <= TRAIL_JSON_BYTES, "{kind:?}");
            for (kept, more) in [
                (trail.read.len(), trail.more_read),
                (trail.edited.len(), trail.more_edited),
                (trail.uncommitted.len(), trail.more_uncommitted),
            ] {
                assert_eq!(kept + more as usize, TRAIL_FILES, "{kind:?}");
            }
            assert!(trail.read.len() < TRAIL_FILES, "{kind:?}");
            // A long text is cut to its share, on a character boundary.
            let intent = trail.intent.as_deref().unwrap_or_default();
            assert!(crate::recent::json_len(intent) <= TEXT_JSON_BYTES);
            assert!(!intent.is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn a_trail_within_its_bound_is_left_whole() {
        let mut trail = largest('a');
        for path in trail
            .read
            .iter_mut()
            .chain(&mut trail.uncommitted)
            .chain(&mut trail.held)
            .chain(&mut trail.waiting_on)
            .chain(trail.edited.iter_mut().map(|file| &mut file.path))
            .chain(trail.checks.iter_mut().map(|check| &mut check.name))
        {
            path.truncate(40);
        }
        trail.session_id.truncate(40);
        trail.workspace_id.truncate(40);
        trail.taken_by = None;
        trail.summary = Some("add retry to the uploader".into());
        trail.intent = Some("i".repeat(2048));
        let before = trail.clone();
        fit(&mut trail);
        assert_eq!(trail, before);
    }
}
