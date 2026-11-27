//! A write check's renamed names and the peers' uncommitted callers of
//! them (issue #213).

use serde::{Deserialize, Serialize};

use crate::{CoreError, Session, State, error, notices::live_session, validate::current_session};

/// Another worktree's uncommitted files that mention a name the asking
/// write renames or removes, with the worktree's live sessions. A name
/// match in the index, not a resolved call. The paths and session IDs are
/// coordination data from that worktree, never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeerCallers {
    pub name: String,
    /// The root of the other worktree.
    pub worktree: String,
    /// At most four, in ID order.
    pub sessions: Vec<String>,
    /// At most `NOTICE_DEPENDENTS`, in path order.
    pub paths: Vec<String>,
}

/// Names one write check asks about as renamed or removed, at most.
pub const MAX_RENAMED: usize = 8;

/// The asking session of a write check with `renamed`, once that session
/// is current and the names are at most `MAX_RENAMED`, each of 1 to 128
/// bytes without control characters.
pub fn validate_renamed<'a>(
    state: &'a State,
    session_id: &str,
    names: &[String],
) -> Result<&'a Session, CoreError> {
    let session = current_session(state, session_id)?;
    if names.len() > MAX_RENAMED
        || names
            .iter()
            .any(|name| name.is_empty() || name.len() > 128 || name.chars().any(char::is_control))
    {
        return Err(error(
            "invalid_calls",
            "A write check names at most 8 renamed names, each of 1 to 128 bytes without control characters",
        ));
    }
    Ok(session)
}

/// The live sessions of `workspace_id` in ID order, at most four: who to
/// ask about a worktree's uncommitted callers.
pub fn live_sessions(state: &State, workspace_id: &str) -> Vec<String> {
    state
        .sessions
        .values()
        .filter(|session| session.workspace_id == workspace_id)
        .filter(|session| live_session(state, &session.session_id))
        .map(|session| session.session_id.clone())
        .take(4)
        .collect()
}
