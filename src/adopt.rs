//! Taking over an ended session's uncommitted work (issue #161,
//! [ADR 0028](../docs/adr/0028-line-ownership.md)).
//!
//! A session that ends before it commits (its process crashed, the disk
//! filled) leaves its edits in the checkout, credited in the line journal to
//! the ended session. A session that carries on the same task runs
//! `langspan adopt SESSION`: the coordinator checks that the named session
//! has ended and worked in the same workspace, and records the transfer as a
//! `session_adopted` event; then the journal entries of the ended session
//! are credited to the adopter, so that `status --mine` and `commit --mine`
//! treat those lines as its own. Lines another session also wrote stay
//! shared, as before.
//!
//! At a session's start, the hook names the ended sessions whose journaled
//! lines are still uncommitted in its checkout (`left_sessions`); the
//! coordinator keeps those nobody adopted, and the briefing suggests
//! `langspan adopt` once.

use crate::{
    journal,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{AdoptReason, Command, MAX_LEFT, Outcome};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Instant,
};

/// Distinct files whose uncommitted change `left_sessions` reads, at most:
/// two bounded Git reads each.
const LEFT_PATHS: usize = 32;

/// What `langspan adopt` did.
#[derive(Debug, Clone, Serialize)]
pub struct AdoptReport {
    /// `adopted`, or `already_adopted` when this session had adopted the
    /// same session before (any entries left were credited now).
    pub result: &'static str,
    pub session_id: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<AdoptReason>,
    /// Files whose journal entries were credited to this session by this
    /// call, in path order.
    pub paths: Vec<String>,
    /// Subagents of the ended session that have not ended: their lines
    /// stay their own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub live: Vec<String>,
}

impl AdoptReport {
    pub fn text(&self) -> String {
        let mut text = match self.result {
            "adopted" => format!(
                "Session {} adopted the uncommitted work of ended session {}.",
                self.session_id, self.from
            ),
            _ => format!(
                "Session {} had already adopted the work of session {}.",
                self.session_id, self.from
            ),
        };
        text.push_str(&match &self.reason {
            Some(AdoptReason::Handoff { message_id }) => {
                format!(" Reason: its message {message_id} to this session.")
            }
            Some(AdoptReason::SameTask {}) => {
                " Reason: both sessions have the same task line.".into()
            }
            None => " No handoff or shared task line links the two sessions; the choice was yours."
                .into(),
        });
        if self.paths.is_empty() {
            text.push_str(" No journaled lines of it were left to credit.");
        } else {
            text.push_str(&format!(
                " Its lines in {} file(s) are now this session's: {}. `langspan status --mine` lists them, and `langspan commit --mine` commits them; lines another session also wrote stay shared.",
                self.paths.len(),
                self.paths.join(", ")
            ));
        }
        if !self.live.is_empty() {
            text.push_str(&format!(
                " Its subagent(s) {} have not ended, so their lines stay theirs.",
                self.live.join(", ")
            ));
        }
        text
    }
}

/// `session_id` adopts `from`'s uncommitted work: the coordinator records
/// the transfer, then the line journal credits `from`'s entries to
/// `session_id`. Running it again after a failure part way finishes it.
pub fn adopt(
    state_dir: &Path,
    request_id: &str,
    session_id: &str,
    from: &str,
    deadline: Instant,
) -> Result<AdoptReport, crate::Error> {
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: request_id.into(),
        command: Command::Adopt {
            session_id: session_id.into(),
            from: from.into(),
        },
    };
    let response = crate::client::call_until(state_dir, &request, deadline)?;
    let (adoption, root, recorded, live) = match response.outcome {
        Outcome::Adopted {
            adoption,
            root,
            recorded,
            live,
        } => (adoption, root, recorded, live),
        Outcome::Error { code, message } => return Err(format!("{code}: {message}").into()),
        other => return Err(format!("the coordinator answered {other:?}").into()),
    };
    let moved = journal::reassign(state_dir, &PathBuf::from(&root), from, &live, session_id).map_err(
        |error| {
            format!(
                "the adoption is recorded, but its lines could not all be credited ({error}); run `langspan adopt {from}` again"
            )
        },
    )?;
    Ok(AdoptReport {
        result: if recorded {
            "adopted"
        } else {
            "already_adopted"
        },
        session_id: adoption.session_id,
        from: adoption.from,
        reason: adoption.reason,
        paths: moved.into_iter().collect(),
        live,
    })
}

/// The sessions other than `me` whose journaled lines are still among the
/// uncommitted changes of the checkout at `root`, at most `MAX_LEFT`. The
/// coordinator decides which of them have ended. Bounded by `LEFT_PATHS`
/// files read and by `deadline`; a file that cannot be read in time counts
/// as still changed.
pub fn left_sessions(state_dir: &Path, root: &Path, me: &str, deadline: Instant) -> Vec<String> {
    let entries = journal::entries(state_dir, root);
    let writers = journal::writers(&entries);
    if writers
        .keys()
        .all(|session| crate::commit::is_own(me, session))
    {
        return Vec::new();
    }
    let head = crate::git::output(
        root,
        &["rev-parse", "--verify", "-q", "HEAD"],
        deadline,
        256,
    )
    .ok()
    .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
    .filter(|head| !head.is_empty());
    let mut changes: BTreeMap<&str, Option<journal::Lines>> = BTreeMap::new();
    let mut left: BTreeSet<String> = BTreeSet::new();
    for (session, paths) in &writers {
        if crate::commit::is_own(me, session) || left.len() >= MAX_LEFT {
            continue;
        }
        for path in paths {
            if !changes.contains_key(path) {
                if changes.len() >= LEFT_PATHS || Instant::now() >= deadline {
                    break;
                }
                let lines = journal::uncommitted_lines(root, path, head.as_deref(), deadline);
                changes.insert(path, lines);
            }
            let changed = changes.get(path).and_then(Option::as_ref);
            if crate::formatter::still_changed(me, session, path, &entries, changed) {
                left.insert((*session).to_owned());
                break;
            }
        }
    }
    left.into_iter().collect()
}
