//! Answering a retried request from its stored receipt.

use super::{Execution, Store, StoreResult};
use langspan_core::{Command, Outcome, Route, decide};
use rusqlite::OptionalExtension;

impl Store {
    pub fn replay(
        &mut self,
        request_id: &str,
        original: &Command,
        now_ms: u64,
    ) -> StoreResult<Option<Execution>> {
        self.refresh(now_ms)?;
        match original.route() {
            Route::Read | Route::Store | Route::Internal => return Ok(None),
            Route::Decide | Route::Index => {}
        }
        let receipt: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT command_json, outcome_json FROM receipts WHERE request_id = ?1",
                [request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((command_json, outcome_json)) = receipt else {
            return Ok(None);
        };
        let unreadable = || {
            Ok(Some(self.execution(
                Outcome::Error {
                    code: "receipt_unreadable".into(),
                    message: "this request ID's earlier outcome was written in a form this build cannot read, so it cannot be answered again; send the operation with a new request ID".into(),
                },
                false,
                now_ms,
            )))
        };
        let Ok(mut recorded) = serde_json::from_str::<Command>(&command_json) else {
            return unreadable();
        };
        // An earlier build may have stored a field the daemon now redacts on
        // receipt (a check's command line), so the identical retry arrives in
        // a form its receipt does not hold. Both sides are compared as the
        // daemon now records them; redaction is a fixed point on its output.
        crate::text::redact::redact_command(&mut recorded);
        let mut retry = original.clone();
        crate::text::redact::redact_command(&mut retry);
        if !recorded.same_request(&retry) {
            return Ok(Some(self.execution(
                Outcome::Error {
                    code: "request_id_conflict".into(),
                    message:
                        "request_id already belongs to a different command; use a new ID".into(),
                },
                false,
                now_ms,
            )));
        }
        let Ok(mut outcome) = serde_json::from_str(&outcome_json) else {
            return unreadable();
        };
        if let Outcome::CheckRecorded { check } = &mut outcome {
            crate::text::redact::redact_check(check);
        }
        // A retried adoption rewrites the line journal from its answer, so it
        // is decided again: the ended session may have resumed since, or a
        // subagent may have started, and the cached answer would hand their
        // new lines over. Only an answer that records nothing new is used.
        if let Outcome::Adopted { recorded, .. } = outcome {
            let decision = decide(&self.state, original, request_id, now_ms);
            outcome = match decision.outcome {
                Outcome::Adopted {
                    adoption,
                    root,
                    live,
                    earlier,
                    kept,
                    ..
                } if decision.events.is_empty() => Outcome::Adopted {
                    adoption,
                    root,
                    recorded,
                    live,
                    earlier,
                    kept,
                },
                refused @ Outcome::Error { .. } => refused,
                _ => Outcome::Error {
                    code: "adoption_changed".into(),
                    message: "this adoption's record no longer matches the coordinator's state; run `langspan adopt` again with a new request ID".into(),
                },
            };
        }
        Ok(Some(self.execution(outcome, true, now_ms)))
    }
}
