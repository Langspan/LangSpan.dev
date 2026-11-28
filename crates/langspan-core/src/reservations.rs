//! Reserved numbers (issue #299, ADR 0040): a session takes the next number
//! of a named counter, such as the next ADR or migration, before it uses
//! it, so that a session in another worktree of the repository takes
//! another. Advisory, like an intent: it holds and refuses no file.

use serde::{Deserialize, Serialize};

use crate::{
    BTreeMap, CoreError, Decision, ErrorCode, Event, Outcome, State, current_session, error,
};

/// Keys counted per scope. A new key past this is refused.
pub const MAX_RESERVATION_KEYS: usize = 256;
/// Claims kept per key, newest first. The highest number is kept apart.
const MAX_RESERVATION_CLAIMS: usize = 16;
/// Other sessions' claims a briefing names, newest first.
const BRIEF_RESERVATIONS: usize = 8;
/// Bytes in a counter's key.
const RESERVATION_KEY_BYTES: usize = 64;

/// One number a session took.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    /// Where the counter counts: the repository ID of the session's
    /// checkout, or its workspace ID outside Git.
    pub scope: String,
    pub key: String,
    pub number: u64,
    pub session_id: String,
    pub workspace_id: String,
    pub at_ms: u64,
}

/// A counter: the highest number it handed out, and its newest claims.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Counter {
    pub(crate) highest: u64,
    pub(crate) claims: Vec<Reservation>,
}

fn validate_key(key: &str) -> Result<(), CoreError> {
    if key.is_empty()
        || key.len() > RESERVATION_KEY_BYTES
        || !key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        return Err(error(
            ErrorCode::InvalidKey.as_str(),
            "A reservation key is 1 to 64 lowercase ASCII letters, digits, dots, underscores or hyphens, such as adr or protocol-version",
        ));
    }
    Ok(())
}

/// The scope of `workspace_id`'s counters: every worktree of its repository.
fn scope(state: &State, workspace_id: &str) -> String {
    state
        .workspace_git
        .get(workspace_id)
        .map_or_else(|| workspace_id.to_owned(), |git| git.repository_id.clone())
}

/// Whether `session_id` is running in this generation.
fn live(state: &State, session_id: &str) -> bool {
    current_session(state, session_id).is_ok()
        && state
            .activity
            .get(session_id)
            .is_none_or(|activity| !activity.ended)
}

pub(crate) fn decide_reserve(
    state: &State,
    session_id: &str,
    key: &str,
    after: u64,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let session = current_session(state, session_id)?;
    validate_key(key)?;
    let scope = scope(state, &session.workspace_id);
    let counters = state.reservations.get(&scope);
    let counter = counters.and_then(|counters| counters.get(key));
    if counter.is_none() && counters.is_some_and(|counters| counters.len() >= MAX_RESERVATION_KEYS)
    {
        return Err(error(
            ErrorCode::TooManyKeys.as_str(),
            "This repository already counts 256 reservation keys; reserve under an existing key",
        ));
    }
    let number = after
        .max(counter.map_or(0, |counter| counter.highest))
        .checked_add(1)
        .ok_or_else(|| {
            error(
                ErrorCode::InvalidNumber.as_str(),
                "The number to count after is the largest there is; give the highest number your checkout uses instead",
            )
        })?;
    let held = counter
        .map(|counter| {
            counter
                .claims
                .iter()
                .filter(|claim| claim.session_id != session_id && live(state, &claim.session_id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let reservation = Reservation {
        scope,
        key: key.into(),
        number,
        session_id: session_id.into(),
        workspace_id: session.workspace_id.clone(),
        at_ms: now_ms,
    };
    Ok(Decision {
        events: vec![Event::Reserved {
            reservation: reservation.clone(),
        }],
        outcome: Outcome::Reserved { reservation, held },
    })
}

pub(crate) fn apply_reserved(
    state: &mut State,
    reservation: &Reservation,
) -> Result<(), CoreError> {
    let session = current_session(state, &reservation.session_id)?;
    validate_key(&reservation.key)?;
    if session.workspace_id != reservation.workspace_id {
        return Err(error(
            ErrorCode::InvalidEvent.as_str(),
            "A reservation must name its session's workspace",
        ));
    }
    let counters = state
        .reservations
        .entry(reservation.scope.clone())
        .or_default();
    if !counters.contains_key(&reservation.key) && counters.len() >= MAX_RESERVATION_KEYS {
        return Err(error(
            ErrorCode::InvalidEvent.as_str(),
            "A reservation exceeds the key bound",
        ));
    }
    let counter = counters.entry(reservation.key.clone()).or_default();
    if reservation.number <= counter.highest {
        return Err(error(
            ErrorCode::InvalidEvent.as_str(),
            "A reservation must take a number above every earlier one of its key",
        ));
    }
    counter.highest = reservation.number;
    counter.claims.insert(0, reservation.clone());
    counter.claims.truncate(MAX_RESERVATION_CLAIMS);
    Ok(())
}

/// The live claims of sessions other than `session_id` in its scope, newest
/// first, at most `BRIEF_RESERVATIONS`.
pub(crate) fn peer_claims(state: &State, session_id: &str, workspace_id: &str) -> Vec<Reservation> {
    let mut claims: Vec<&Reservation> = state
        .reservations
        .get(&scope(state, workspace_id))
        .map(BTreeMap::values)
        .into_iter()
        .flatten()
        .flat_map(|counter| &counter.claims)
        .filter(|claim| claim.session_id != session_id && live(state, &claim.session_id))
        .collect();
    claims.sort_by(|a, b| (b.at_ms, b.number, &b.key).cmp(&(a.at_ms, a.number, &a.key)));
    claims
        .into_iter()
        .take(BRIEF_RESERVATIONS)
        .cloned()
        .collect()
}
