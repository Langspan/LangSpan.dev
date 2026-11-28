//! Same task (issue #537): whether two sessions' recorded work shows one
//! task. Decided from prompt targets and held files alone, never from model
//! judgment, so both sessions reach the same answer.

use std::collections::BTreeSet;

use crate::{LivenessStatus, State};

/// That peer `session_id` is on the asking session's task: told once per
/// peer.
pub(super) fn key(session_id: &str) -> u64 {
    super::item_key(&["peer", session_id, "same_task"])
}

/// `keys`, with each same-task key in `told` of a peer still running.
/// Told once per peer, it outlasts a briefing in which the match lapsed (a
/// hold expired, or a prompt pointed elsewhere) and goes when the peer ends.
pub(super) fn keep_told(
    state: &State,
    session_id: &str,
    told: &BTreeSet<u64>,
    mut keys: Vec<u64>,
) -> Vec<u64> {
    for peer in state.sessions.values() {
        let ended = state
            .activity
            .get(&peer.session_id)
            .is_some_and(|activity| activity.ended)
            || state
                .session_liveness
                .get(&peer.session_id)
                .is_some_and(|liveness| liveness.status == LivenessStatus::Exited);
        let key = key(&peer.session_id);
        if peer.session_id != session_id
            && peer.generation == state.generation
            && !ended
            && told.contains(&key)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    keys
}

/// One session's side: where its last recent prompt points (its prompt
/// targets), and the files it holds (its claims).
pub(super) type Work<'a> = (&'a BTreeSet<String>, &'a BTreeSet<String>);

/// Whether `a` and `b` are on the same task: a file one's prompt points at
/// is one the other's prompt points at or holds. Symmetric. Files both only
/// hold or edit are an overlap, not the same task.
pub(super) fn same_task((a_targets, a_claims): Work, (b_targets, b_claims): Work) -> bool {
    !a_targets.is_disjoint(b_targets)
        || !a_targets.is_disjoint(b_claims)
        || !b_targets.is_disjoint(a_claims)
}

#[cfg(test)]
mod tests {
    use crate::briefing::same_task::same_task;
    use std::collections::BTreeSet;

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn targets_meeting_targets_or_claims_match_both_ways_and_nothing_else_does() {
        let (http, footer, none) = (set(&["src/http.rs"]), set(&["src/footer.rs"]), set(&[]));
        let cases = [
            ((&http, &none), (&http, &none), true),
            ((&http, &none), (&none, &http), true),
            ((&http, &footer), (&footer, &none), true),
            // Disjoint targets, and claims alone, never match.
            ((&http, &none), (&footer, &none), false),
            ((&none, &http), (&none, &http), false),
            ((&none, &none), (&none, &none), false),
        ];
        for (a, b, expected) in cases {
            assert_eq!(same_task(a, b), expected, "{a:?} {b:?}");
            assert_eq!(same_task(b, a), expected, "not symmetric: {a:?} {b:?}");
        }
    }
}
