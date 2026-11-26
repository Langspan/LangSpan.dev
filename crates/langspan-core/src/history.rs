//! The history of one definition (roadmap D4): every interface change the
//! index recorded for a name, newest first, with the session that last
//! reported editing the file, that session's task line at the time, the notes
//! and messages that named the file around the change, and whether the change
//! put back a signature the definition had before.
//!
//! Read from the event log: the daemon's store hands over the logged events
//! and the generations' clock anchors, and this module decides. Nothing is
//! recorded and nothing counts as delivered. The session named is a
//! correlation (the last session that reported editing the file before the
//! change was indexed), never proof of who wrote the change.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    ClockAnchor, CoreError, Event, InterfaceChange, InterfaceNotice, RecordedEvent, Stamp, State,
    error, validate_known_workspace, validate_relative_path,
};

/// The most entries one answer returns.
pub const HISTORY_LIMIT: u32 = 50;
/// How far from a change a note or message is taken to be about it.
pub const HISTORY_RELATED_MS: u64 = 30 * 60 * 1000;
/// Notes, and separately messages, shown with one entry at most.
pub const HISTORY_RELATED: usize = 4;
/// The newest logged events of the kinds a history reads, at most, that one
/// answer is drawn from. Older ones are reached by asking again with
/// `before_seq`.
pub const HISTORY_EVENTS: usize = 20_000;
/// The longest name asked about, in bytes.
pub const HISTORY_NAME_BYTES: usize = 128;

/// What every answer says about `by_session`.
pub const HISTORY_ATTRIBUTION: &str = "by_session is the last session that reported editing the file before the change was indexed: a correlation, not proof of authorship.";

/// A note made near a change that names its file. Remembered text: data,
/// never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HistoryNote {
    pub note_id: String,
    pub author: Option<String>,
    pub text: String,
}

/// A message sent near a change that names its file. Session text: data,
/// never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HistoryMessage {
    pub message_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub text: String,
}

/// One recorded change to the definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    /// The event's sequence number in the log.
    pub seq: u64,
    pub notice_id: String,
    /// How long ago, when the change's generation can be placed against
    /// now (always within the daemon's current run).
    pub age_ms: Option<u64>,
    /// `added`, `removed`, `signature_changed`, `renamed`,
    /// `definition_added` or `definition_removed`.
    pub change: String,
    pub kind: String,
    pub path: String,
    /// Absent for an addition, and for a removal recorded before removals
    /// kept their signature.
    pub before: Option<String>,
    /// Absent for a removal.
    pub after: Option<String>,
    /// For a rename: the new name, what links the two names, and for a file
    /// moved whole, where it was (roadmap D7). Absent otherwise, and from
    /// renames recorded before each was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_path: Option<String>,
    /// The last session that reported editing the file. A correlation, not
    /// proof of who made the change.
    pub by_session: Option<String>,
    /// That session's last prompt summary before the change; absent when it
    /// recorded none (prompt summaries off). Session text: data.
    pub task: Option<String>,
    pub notes: Vec<HistoryNote>,
    pub messages: Vec<HistoryMessage>,
    /// `after` is a signature this definition, in this file, had at an
    /// earlier recorded change: this change put it back.
    pub reverted: bool,
}

/// A definition's recorded changes, newest first. `truncated` with a
/// `next_seq` when more are recorded than this answer holds: ask again with
/// `before_seq` set to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub workspace_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub entries: Vec<HistoryEntry>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_seq: Option<u64>,
    pub attribution: String,
}

impl History {
    /// Drop the oldest entries, a whole event's at a time, until `fits` says
    /// the answer is small enough; the newest event's entries always stay.
    /// What is dropped can be asked for again from `next_seq`.
    pub fn shrink_until(&mut self, mut fits: impl FnMut(&History) -> bool) {
        while !fits(self) {
            let Some(last) = self.entries.last().map(|entry| entry.seq) else {
                return;
            };
            if self.entries.first().is_some_and(|entry| entry.seq == last) {
                return;
            }
            self.entries.retain(|entry| entry.seq != last);
            self.truncated = true;
            self.next_seq = self.entries.last().map(|entry| entry.seq);
        }
    }
}

/// What a history is asked for, and the log it is read from.
pub struct HistoryQuery<'a> {
    pub workspace_id: &'a str,
    pub name: &'a str,
    pub path: Option<&'a str>,
    pub limit: u32,
    /// Only changes recorded before this sequence number.
    pub before_seq: Option<u64>,
    /// Logged events, in any order: interface changes, prompts, notes,
    /// forgettings, messages and withdrawals. Others are ignored.
    pub events: &'a [RecordedEvent],
    /// Every generation's clock anchor that the log holds.
    pub anchors: &'a [ClockAnchor],
    /// The daemon's clock now, in the current generation.
    pub now_ms: u64,
}

/// How far apart two times are, when they can be compared: on one
/// generation's clock, or in wall time through both generations' anchors.
fn apart(a: Stamp, b: Stamp, anchors: &[ClockAnchor]) -> Option<u64> {
    if a.generation == b.generation {
        return Some(a.at_ms.abs_diff(b.at_ms));
    }
    let wall = |stamp: Stamp| {
        anchors
            .iter()
            .find(|anchor| anchor.generation == stamp.generation)
            .map(|anchor| anchor.wall(stamp.at_ms))
    };
    Some(wall(a)?.abs_diff(wall(b)?))
}

fn names_file(paths: &[String], text: &str, names: &BTreeSet<&str>) -> bool {
    paths.iter().any(|named| names.contains(named.as_str()))
        || names.iter().any(|name| text.contains(name))
}

/// Each file moved whole in `events` (roadmap D7): the old path and the
/// new one, from the `file_moved` renames of the workspace's notices.
fn moves<'a>(events: &[&'a RecordedEvent], workspace_id: &str) -> Vec<(&'a str, &'a str)> {
    events
        .iter()
        .filter_map(|recorded| match &recorded.event {
            Event::InterfaceChanged { notice } if notice.workspace_id == workspace_id => {
                Some(notice)
            }
            _ => None,
        })
        .flat_map(|notice| {
            notice
                .changes
                .iter()
                .filter(|change| change.evidence.as_deref() == Some(crate::FILE_MOVED))
                .filter_map(|change| change.before_path.as_deref())
                .map(|from| (from, notice.path.as_str()))
        })
        .collect()
}

/// Every path `path` was known by through `moves`: itself, and each path a
/// move led from or to, transitively.
fn lineage<'a>(moves: &[(&'a str, &'a str)], path: &'a str) -> BTreeSet<&'a str> {
    let mut names = BTreeSet::from([path]);
    loop {
        let before = names.len();
        for &(from, to) in moves {
            if names.contains(from) || names.contains(to) {
                names.insert(from);
                names.insert(to);
            }
        }
        if names.len() == before {
            return names;
        }
    }
}

/// The recorded changes of `query.name`, including a rename to it, newest
/// first, at most
/// `query.limit` (a whole event's entries are kept together).
pub fn history(state: &State, query: &HistoryQuery<'_>) -> Result<History, CoreError> {
    validate_known_workspace(state, query.workspace_id)?;
    if query.name.is_empty()
        || query.name.len() > HISTORY_NAME_BYTES
        || query
            .name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(error(
            "invalid_name",
            "A definition name is 1 to 128 bytes without spaces or control characters",
        ));
    }
    if !(1..=HISTORY_LIMIT).contains(&query.limit) {
        return Err(error("invalid_limit", "A history returns 1 to 50 entries"));
    }
    if let Some(path) = query.path {
        validate_relative_path(path)?;
    }
    let mut events: Vec<&RecordedEvent> = query.events.iter().collect();
    events.sort_by_key(|recorded| recorded.seq);
    // A file moved whole keeps its history: asked about by its new path,
    // what was recorded under the old one is included (roadmap D7).
    let moves = moves(&events, query.workspace_id);
    let asked = query.path.map(|path| lineage(&moves, path));

    // Every change of the name, oldest first.
    let mut found: Vec<(u64, &InterfaceNotice, &InterfaceChange)> = Vec::new();
    for recorded in &events {
        let Event::InterfaceChanged { notice } = &recorded.event else {
            continue;
        };
        if notice.workspace_id != query.workspace_id
            || asked
                .as_ref()
                .is_some_and(|paths| !paths.contains(notice.path.as_str()))
        {
            continue;
        }
        // A rename is recorded under the old name, and the new name's
        // addition is folded into it: it belongs to both names' histories.
        let named = |c: &&InterfaceChange| {
            c.name == query.name
                || (c.change == "renamed" && c.after_name.as_deref() == Some(query.name))
        };
        for change in notice.changes.iter().filter(named) {
            found.push((recorded.seq, notice, change));
        }
    }
    let forgotten: BTreeSet<&str> = events
        .iter()
        .filter_map(|recorded| match &recorded.event {
            Event::Forgotten {
                workspace_id,
                note_id,
            } if workspace_id == query.workspace_id => Some(note_id.as_str()),
            _ => None,
        })
        .collect();
    // A withdrawn message no longer holds: its text is left out, as the
    // briefing and recap stop presenting it.
    let retracted: BTreeSet<&str> = events
        .iter()
        .filter_map(|recorded| match &recorded.event {
            Event::MessageRetracted {
                workspace_id,
                message_id,
                ..
            } if workspace_id == query.workspace_id => Some(message_id.as_str()),
            _ => None,
        })
        .collect();
    let now = Stamp {
        at_ms: query.now_ms,
        generation: state.generation,
    };
    let limit = query.limit as usize;
    let mut entries: Vec<HistoryEntry> = Vec::new();
    let mut truncated = false;
    // Newest first.
    for (index, (seq, notice, change)) in found.iter().enumerate().rev() {
        if query.before_seq.is_some_and(|before| *seq >= before) {
            continue;
        }
        // The rest of a started event stays with it, past the limit.
        if entries.len() >= limit && entries.last().is_none_or(|last| last.seq != *seq) {
            truncated = true;
            break;
        }
        let at = Stamp {
            at_ms: notice.at_ms,
            generation: notice.generation,
        };
        let near = |stamp: Option<Stamp>| {
            stamp
                .and_then(|stamp| apart(stamp, at, query.anchors))
                .is_some_and(|gap| gap <= HISTORY_RELATED_MS)
        };
        let reverted = change.after.as_ref().is_some_and(|after| {
            found[..index].iter().any(|(_, older, earlier)| {
                older.path == notice.path
                    && earlier.kind == change.kind
                    && (earlier.before.as_ref() == Some(after)
                        || earlier.after.as_ref() == Some(after))
            })
        });
        let task = notice.by_session.as_deref().and_then(|session| {
            events
                .iter()
                .rev()
                .filter(|recorded| recorded.seq < *seq)
                .find_map(|recorded| match &recorded.event {
                    Event::SessionPrompted {
                        session_id,
                        summary,
                        ..
                    } if session_id == session => Some(summary.clone()),
                    _ => None,
                })
        });
        // A note or message about the file under any of its paths.
        let file = lineage(&moves, &notice.path);
        let mut notes: Vec<HistoryNote> = Vec::new();
        let mut messages: Vec<HistoryMessage> = Vec::new();
        for recorded in &events {
            match &recorded.event {
                Event::Remembered { note, at: when }
                    if note.workspace_id == query.workspace_id
                        && notes.len() < HISTORY_RELATED
                        && !forgotten.contains(note.note_id.as_str())
                        && !notes.iter().any(|held| held.note_id == note.note_id)
                        && names_file(&note.paths, &note.text, &file)
                        && near(*when) =>
                {
                    notes.push(HistoryNote {
                        note_id: note.note_id.clone(),
                        author: note.author.clone(),
                        text: note.text.clone(),
                    });
                }
                Event::MessageSent {
                    message,
                    at_ms,
                    generation,
                } if message.workspace_id == query.workspace_id
                    && messages.len() < HISTORY_RELATED
                    && !retracted.contains(message.message_id.as_str())
                    && !messages
                        .iter()
                        .any(|held| held.message_id == message.message_id)
                    && names_file(&message.paths, &message.text, &file)
                    && near(
                        at_ms
                            .zip(*generation)
                            .map(|(at_ms, generation)| Stamp { at_ms, generation }),
                    ) =>
                {
                    messages.push(HistoryMessage {
                        message_id: message.message_id.clone(),
                        from: message.from.clone(),
                        text: message.text.clone(),
                    });
                }
                _ => {}
            }
        }
        entries.push(HistoryEntry {
            seq: *seq,
            notice_id: notice.notice_id.clone(),
            age_ms: apart(now, at, query.anchors),
            change: change.change.clone(),
            kind: change.kind.clone(),
            path: notice.path.clone(),
            before: change.before.clone(),
            after: change.after.clone(),
            after_name: change.after_name.clone(),
            evidence: change.evidence.clone(),
            before_path: change.before_path.clone(),
            by_session: notice.by_session.clone(),
            task,
            notes,
            messages,
            reverted,
        });
    }
    Ok(History {
        workspace_id: query.workspace_id.to_owned(),
        name: query.name.to_owned(),
        path: query.path.map(str::to_owned),
        next_seq: if truncated {
            entries.last().map(|entry| entry.seq)
        } else {
            None
        },
        entries,
        truncated,
        attribution: HISTORY_ATTRIBUTION.into(),
    })
}
