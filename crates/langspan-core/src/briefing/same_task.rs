//! Same task (issue #537): whether two sessions' recorded work shows one
//! task. Decided from prompt targets and held files alone, never from model
//! judgment, so both sessions reach the same answer.

use std::collections::BTreeSet;

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
