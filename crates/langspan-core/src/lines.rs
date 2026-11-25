//! The admission rule that protects lines other sessions wrote recently from
//! a session that has not read the file
//! ([ADR 0028](../../../docs/adr/0028-line-ownership.md)). The hook finds
//! the lines in its line journal (ADR 0026); the core decides with what it
//! alone knows: which versions the session has seen and which writers are
//! still live.

use crate::{
    CoreError, FileState, LINE_HORIZON_MS, LivenessStatus, MAX_REGION_LINE, MAX_WRITTEN,
    MAX_WRITTEN_RANGES, ObservedFile, Session, State, WrittenLines, error, known_versions,
    validate_id,
};

/// At most `MAX_WRITTEN` entries, each for a requested path, with at most
/// `MAX_WRITTEN_RANGES` ordered, disjoint, one-based ranges.
pub(crate) fn validate_overwrites(
    overwrites: &[WrittenLines],
    paths: &[String],
) -> Result<(), CoreError> {
    let invalid = |message: &str| Err(error("invalid_overwrites", message));
    if overwrites.len() > MAX_WRITTEN {
        return invalid("An admission can name at most 64 written-lines entries");
    }
    for entry in overwrites {
        if paths.binary_search(&entry.path).is_err() {
            return invalid("Written lines name a path this admission does not request");
        }
        validate_id(&entry.session_id, "writer session ID")?;
        if entry.lines.len() > MAX_WRITTEN_RANGES {
            return invalid("Written lines are at most 32 ranges per entry");
        }
        let mut previous = 0;
        for range in &entry.lines {
            if range.start_line <= previous
                || range.start_line > range.end_line
                || range.end_line > MAX_REGION_LINE
            {
                return invalid(
                    "Written line ranges are one-based, ordered, disjoint and at most 2000000",
                );
            }
            previous = range.end_line;
        }
    }
    Ok(())
}

/// A writer whose lines still count: registered, not ended, not known to
/// have exited.
fn live_writer(state: &State, session_id: &str) -> bool {
    state.sessions.contains_key(session_id)
        && !state
            .activity
            .get(session_id)
            .is_some_and(|activity| activity.ended)
        && !state
            .session_liveness
            .get(session_id)
            .is_some_and(|liveness| liveness.status == LivenessStatus::Exited)
}

/// The supplied entries that refuse this edit: for an existing file of which
/// the session has no recorded version, written by another live session
/// within `LINE_HORIZON_MS`. A session that read or wrote the file is
/// governed by the stale-source rule instead, so a change is never refused
/// twice.
pub(crate) fn unread_lines(
    state: &State,
    session: &Session,
    base: &[ObservedFile],
    overwrites: &[WrittenLines],
) -> Vec<WrittenLines> {
    let known = known_versions(state, &session.session_id);
    overwrites
        .iter()
        .filter(|entry| {
            base.iter().any(|file| {
                file.path == entry.path && matches!(file.state, FileState::Present { .. })
            }) && !known.iter().any(|version| version.path == entry.path)
                && entry.session_id != session.session_id
                && entry.age_ms <= LINE_HORIZON_MS
                && live_writer(state, &entry.session_id)
        })
        .cloned()
        .collect()
}
