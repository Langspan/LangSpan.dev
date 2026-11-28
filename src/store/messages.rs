//! Messages read back from the event log: the outbound audit, a sent
//! message, and who was given it.

use super::{Store, StoreResult, retention};
use langspan_core::{Event, Outcome, SentPublication};
use rusqlite::{OptionalExtension, params};

impl Store {
    /// The outbound audit: what left for a workspace, newest first, read
    /// from the `remote_published` events. Each payload is at most 16 KiB,
    /// which JSON escaping can at most double, so one record always fits a
    /// frame; the answer stops before 48 KiB and says whether it did.
    pub(super) fn sent(&self, workspace_id: &str, limit: u32) -> StoreResult<Outcome> {
        if !self.state.remote_links.contains_key(workspace_id) {
            return Ok(Outcome::Error {
                code: "remote_not_enabled".into(),
                message: "this workspace has never published; nothing has left the machine for it"
                    .into(),
            });
        }
        if !(1..=100).contains(&limit) {
            return Ok(Outcome::Error {
                code: "invalid_limit".into(),
                message: "limit must be between 1 and 100".into(),
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT seq, event_json FROM events
             WHERE json_valid(event_json)
               AND json_extract(event_json, '$.type') = 'remote_published'
               AND json_extract(event_json, '$.workspace_id') = ?1
             ORDER BY seq DESC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![workspace_id, i64::from(limit)])?;
        let mut sent = Vec::new();
        let mut bytes = 0;
        let mut more = false;
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            let Event::RemotePublished {
                sequence,
                payload,
                published_at_ms,
                ..
            } = serde_json::from_str(&json)?
            else {
                return Err("a remote_published row holds another event".into());
            };
            let record = SentPublication {
                seq: u64::try_from(seq)?,
                sequence,
                published_at_ms,
                payload,
            };
            let size = serde_json::to_vec(&record)?.len() + 16;
            if bytes + size > 48 * 1024 {
                more = true;
                break;
            }
            bytes += size;
            sent.push(record);
        }
        Ok(Outcome::RemoteSent {
            sent,
            more,
            since_ms: retention::since_ms(&self.connection)?,
        })
    }

    /// One message as it was sent, read from its `message_sent` event, so
    /// it is there after every recipient has acknowledged it and the
    /// projection has dropped it, and marked retracted when a later
    /// `message_retracted` event withdrew it. Read-only and never recorded. Messages
    /// are rare, so the scan reads few rows past the cheap prefix test; a
    /// sent message is at most 400 bytes of text and 8 paths, so the answer
    /// fits a frame.
    pub(super) fn show_message(
        &self,
        workspace_id: &str,
        message_id: &str,
    ) -> StoreResult<Outcome> {
        if !self.state.workspaces.contains_key(workspace_id) {
            return Ok(Outcome::Error {
                code: "unknown_workspace".into(),
                message: format!(
                    "no workspace {workspace_id:?} is registered; `langspan status` lists them"
                ),
            });
        }
        if let Err(error) = langspan_core::validate_request_id(message_id) {
            return Ok(Outcome::Error {
                code: error.code,
                message: error.message,
            });
        }
        let stored = self.sent_event(workspace_id, message_id)?;
        let Some((sent_seq, stored)) = stored else {
            // Retention deletes the oldest events. Once any were deleted, an
            // ID not found may be a message whose event went with them, and
            // the log cannot tell that from one never sent: say so plainly.
            if let Some(first_kept) = self.first_kept_event()? {
                return Ok(Outcome::Error {
                    code: "message_not_kept".into(),
                    message: format!(
                        "message {message_id:?} is not in the event log of workspace {workspace_id:?}, which keeps events from sequence {first_kept} on; if it was sent before then, its full text is no longer kept. `langspan recap {workspace_id}` lists the messages still pending"
                    ),
                });
            }
            return Ok(Outcome::Error {
                code: "unknown_message".into(),
                message: format!(
                    "no message {message_id:?} was sent in workspace {workspace_id:?}; `langspan recap {workspace_id}` lists the messages still pending"
                ),
            });
        };
        let Event::MessageSent { mut message, .. } = serde_json::from_str(&stored)? else {
            return Err("a message_sent row holds another event".into());
        };
        message.retracted = self.retracted_after(sent_seq, workspace_id, message_id)?;
        Ok(Outcome::MessageShown {
            message: message.into(),
        })
    }

    /// The newest `message_sent` event for the message: its sequence and
    /// JSON. Messages are rare, so the scan reads few rows past the cheap
    /// prefix test.
    fn sent_event(
        &self,
        workspace_id: &str,
        message_id: &str,
    ) -> StoreResult<Option<(i64, String)>> {
        Ok(self
            .connection
            .query_row(
                r#"SELECT seq, event_json FROM events
                 WHERE event_json LIKE '{"type":"message_sent",%'
                   AND json_valid(event_json)
                   AND json_extract(event_json, '$.type') = 'message_sent'
                   AND json_extract(event_json, '$.message.workspace_id') = ?1
                   AND json_extract(event_json, '$.message.message_id') = ?2
                 ORDER BY seq DESC LIMIT 1"#,
                params![workspace_id, message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Whether a `message_retracted` event after sequence `after` withdrew
    /// the message. A withdrawal is its own later event; the sent one never
    /// changes.
    fn retracted_after(
        &self,
        after: i64,
        workspace_id: &str,
        message_id: &str,
    ) -> StoreResult<bool> {
        Ok(self.connection.query_row(
            r#"SELECT EXISTS (SELECT 1 FROM events
                 WHERE seq > ?1
                   AND event_json LIKE '{"type":"message_retracted",%'
                   AND json_valid(event_json)
                   AND json_extract(event_json, '$.type') = 'message_retracted'
                   AND json_extract(event_json, '$.workspace_id') = ?2
                   AND json_extract(event_json, '$.message_id') = ?3)"#,
            params![after, workspace_id, message_id],
            |row| row.get(0),
        )?)
    }

    /// Who was given a sent message (roadmap E12): the core reads its held
    /// and history records with the `message_sent`, `message_acknowledged`
    /// and `message_retracted` events the log still keeps. Read-only and
    /// never recorded. At most `ACK_ROWS` acknowledgements are read (a
    /// session acknowledges a message once), so the work is bounded; past
    /// that the answer is `partial`.
    pub(super) fn message_status(
        &self,
        workspace_id: &str,
        from: Option<&str>,
        message_id: Option<&str>,
        now_ms: u64,
    ) -> StoreResult<Outcome> {
        const ACK_ROWS: i64 = 4096;
        let message_id = match langspan_core::status_message_id(
            &self.state,
            workspace_id,
            from,
            message_id,
        ) {
            Ok(id) => id,
            Err(error) if error.code == "workspace_not_found" => {
                return Ok(Outcome::Error {
                    code: "unknown_workspace".into(),
                    message: format!(
                        "no workspace {workspace_id:?} is registered; `langspan status` lists them"
                    ),
                });
            }
            Err(error) => {
                return Ok(Outcome::Error {
                    code: error.code,
                    message: error.message,
                });
            }
        };
        let (after, sent) = match self.sent_event(workspace_id, &message_id)? {
            Some((seq, json)) => {
                let Event::MessageSent {
                    message,
                    at_ms,
                    generation,
                } = serde_json::from_str(&json)?
                else {
                    return Err("a message_sent row holds another event".into());
                };
                (
                    seq,
                    Some(langspan_core::SentRecord {
                        message,
                        at_ms,
                        generation,
                    }),
                )
            }
            None => (0, None),
        };
        let mut statement = self.connection.prepare(
            r#"SELECT event_json FROM events
             WHERE seq > ?1
               AND event_json LIKE '{"type":"message_acknowledged",%'
               AND json_valid(event_json)
               AND json_extract(event_json, '$.type') = 'message_acknowledged'
               AND json_extract(event_json, '$.workspace_id') = ?2
               AND json_extract(event_json, '$.message_id') = ?3
             ORDER BY seq LIMIT ?4"#,
        )?;
        let mut rows = statement.query(params![after, workspace_id, message_id, ACK_ROWS + 1])?;
        let mut acks = Vec::new();
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            let Event::MessageAcknowledged {
                session_id,
                via,
                at_ms,
                generation,
                ..
            } = serde_json::from_str(&json)?
            else {
                return Err("a message_acknowledged row holds another event".into());
            };
            acks.push(langspan_core::AckRecord {
                session_id,
                via,
                at_ms,
                generation,
            });
        }
        let cut = acks.len() as i64 > ACK_ROWS;
        acks.truncate(ACK_ROWS as usize);
        let retracted = self.retracted_after(after, workspace_id, &message_id)?;
        match langspan_core::message_status(
            &self.state,
            workspace_id,
            &message_id,
            sent.as_ref(),
            &acks,
            retracted,
            now_ms,
        ) {
            Some(mut status) => {
                status.partial |= cut;
                Ok(Outcome::MessageStatus { status })
            }
            None if self.first_kept_event()?.is_some() => Ok(Outcome::Error {
                code: "message_not_kept".into(),
                message: format!(
                    "message {message_id:?} is neither held nor in the event log of workspace {workspace_id:?}; if it was sent before the log's oldest kept event, who received it is no longer kept"
                ),
            }),
            None => Ok(Outcome::Error {
                code: "unknown_message".into(),
                message: format!(
                    "no message {message_id:?} was sent in workspace {workspace_id:?}; `langspan recap {workspace_id}` lists the messages still pending"
                ),
            }),
        }
    }

    /// `Some(N)` once events before sequence N have been deleted (by
    /// retention): the first kept event is past 1, or every row is gone
    /// while the sequence counter has advanced. `None` while the log is
    /// whole.
    fn first_kept_event(&self) -> StoreResult<Option<i64>> {
        let first: Option<i64> =
            self.connection
                .query_row("SELECT MIN(seq) FROM events", [], |row| row.get(0))?;
        let counter: Option<i64> = self
            .connection
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'events'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match (first, counter) {
            (Some(first), _) if first > 1 => Some(first),
            (None, Some(last)) if last > 0 => Some(last + 1),
            _ => None,
        })
    }
}
