//! Reads answered from the event log: the dashboard's refusals, the daily
//! account, a recap's catch-up, the report, the refusal count, a
//! definition's history and a workspace's changes.

use super::sql::{
    ANCHOR_LOOKUP, ANCHORS, FORGETTING_LOOKUP, GRANTS_AFTER, HISTORY_BEFORE, HISTORY_FOLLOWING,
    INTENT_EVENTS_BEFORE, MOVES_LOOKUP, REFUSAL_EARLIER, REFUSAL_ROWS, RETRACTION_LOOKUP,
};
use super::{
    HISTORY_BUDGET_BYTES, PAGE_BUDGET_BYTES, RETRY_LOOKAHEAD, Store, StoreResult, retention,
    wall_now_ms,
};
use langspan_core::{
    ClockAnchor, Command, DASHBOARD_ITEMS, DENIAL_WINDOW, DenialRecord, Denials, Event,
    HISTORY_EVENTS, HISTORY_MOVES, HistoryQuery, Outcome, RecordedEvent, ReportQuery, TodayQuery,
    TodayWindow, dashboard_denials, definition_history,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;

/// Before the events a report reads: sessions holding an intent, each with
/// its intent's workspace, each session's workspace, and what of these is
/// known when only part of the older history was folded (`None`: all).
type ReportHistory = (
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    Option<langspan_core::ReportKnown>,
);

/// The report's refusal window (H9): `REFUSAL_WINDOW_MS`, or for tests only
/// `LANGSPAN_TEST_REFUSAL_WINDOW_MS`, between 100 ms and that, so a test can
/// see a refusal abandoned without waiting 30 minutes. Not a setting: it is
/// undocumented for users and can only shorten the window.
fn refusal_window_ms() -> u64 {
    std::env::var("LANGSPAN_TEST_REFUSAL_WINDOW_MS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .map(|ms| ms.clamp(100, langspan_core::REFUSAL_WINDOW_MS))
        .unwrap_or(langspan_core::REFUSAL_WINDOW_MS)
}

impl Store {
    /// Recent refusals for the dashboard. Conflicts are receipts, not
    /// events, and carry no time; stale-source refusals are the events that
    /// recorded what a refused session was told. Only the newest
    /// `DENIAL_WINDOW` rows of each table are searched, so the cost of a read
    /// does not grow with history.
    pub(super) fn denials(&self, now_ms: u64) -> StoreResult<Denials> {
        let page = i64::try_from(DASHBOARD_ITEMS / 2 + 1)?;
        let window = i64::from(DENIAL_WINDOW);
        let mut statement = self.connection.prepare(
            "SELECT command_json, outcome_json, request_id FROM receipts
             WHERE rowid > (SELECT coalesce(max(rowid), 0) FROM receipts) - ?1
               AND json_extract(outcome_json, '$.type') = 'conflict'
             ORDER BY rowid DESC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![window, page])?;
        let mut conflicts = Vec::new();
        while let Some(row) = rows.next()? {
            // A conflict the hook waited out (ADR 0036) is a refusal only if
            // the wait ended in it: an admission asked for after the wait is
            // listed by its own outcome, and a stale source found during it
            // with the stale refusals.
            let first = row.get::<_, String>(2)?;
            let waited: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM receipts WHERE request_id = ?1
                   OR (request_id = ?2
                       AND json_extract(command_json, '$.result') != 'refused_conflict'))",
                params![
                    crate::wait::admission_id(&first),
                    crate::wait::record_id(&first)
                ],
                |found| found.get(0),
            )?;
            if waited {
                continue;
            }
            // A receipt this build cannot read is no denial it can show.
            let (Ok(command), Ok(outcome)) = (
                serde_json::from_str::<Command>(&row.get::<_, String>(0)?),
                serde_json::from_str::<Outcome>(&row.get::<_, String>(1)?),
            ) else {
                continue;
            };
            let (Command::Admit {
                session_id, paths, ..
            }
            | Command::AdmitTool {
                session_id, paths, ..
            }) = command
            else {
                continue;
            };
            let Outcome::Conflict { holders } = outcome else {
                continue;
            };
            conflicts.push(DenialRecord {
                session_id,
                paths,
                holders: holders
                    .into_iter()
                    .map(|holder| holder.session_id)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                at_ms: None,
                generation: None,
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT event_json FROM events
             WHERE seq > (SELECT coalesce(max(seq), 0) FROM events) - ?1
               AND json_extract(event_json, '$.type') = 'source_observed'
               AND json_extract(event_json, '$.origin') = 'refused'
             ORDER BY seq DESC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![window, page])?;
        let mut refusals = Vec::new();
        while let Some(row) = rows.next()? {
            if let Event::SourceObserved {
                session_id,
                files,
                at_ms,
                generation,
                ..
            } = serde_json::from_str(&row.get::<_, String>(0)?)?
            {
                refusals.push(DenialRecord {
                    session_id,
                    paths: files.into_iter().map(|file| file.path).collect(),
                    holders: Vec::new(),
                    at_ms: Some(at_ms),
                    generation: Some(generation),
                });
            }
        }
        let limit = DASHBOARD_ITEMS / 2;
        let more = |records: &mut Vec<DenialRecord>| {
            let more = records.len() > limit;
            records.truncate(limit);
            more
        };
        let more_conflicts = more(&mut conflicts);
        let more_refusals = more(&mut refusals);
        Ok(dashboard_denials(
            &self.state,
            now_ms,
            conflicts,
            more_conflicts,
            refusals,
            more_refusals,
        ))
    }

    /// The daily account: the newest `TODAY_EVENTS` events at most, placed
    /// in wall time by their generation's anchor, summarised by the core.
    /// Reads only; it writes nothing and acknowledges no message.
    pub fn today(
        &self,
        workspace_id: Option<&str>,
        from_ms: u64,
        wall_ms: u64,
        now_ms: u64,
    ) -> StoreResult<Outcome> {
        if let Some(refused) = self.refused_window(workspace_id, from_ms, wall_ms) {
            return Ok(refused);
        }
        let (events, anchor, earlier) = Self::newest_events(&self.connection, self.today_events)?;
        let retained_from_ms = retention::since_ms(&self.connection)?;
        let mut today = langspan_core::today(
            &self.state,
            &TodayQuery {
                workspace_id,
                window: TodayWindow {
                    from_ms,
                    to_ms: wall_ms,
                },
                events: &events,
                anchor,
                earlier,
                retained_from_ms,
                now_ms,
            },
        );
        today.shrink_until(|today| {
            serde_json::to_vec(today).is_ok_and(|bytes| bytes.len() <= PAGE_BUDGET_BYTES)
        });
        Ok(Outcome::Today { today })
    }

    /// Why a window of the event log cannot be read: an unknown workspace,
    /// or a start after now.
    fn refused_window(
        &self,
        workspace_id: Option<&str>,
        from_ms: u64,
        wall_ms: u64,
    ) -> Option<Outcome> {
        if let Some(workspace_id) = workspace_id
            && !self.state.workspaces.contains_key(workspace_id)
        {
            return Some(Outcome::Error {
                code: "unknown_workspace".into(),
                message: "no workspace has this ID; `langspan status` lists them".into(),
            });
        }
        (from_ms > wall_ms).then(|| Outcome::Error {
            code: "invalid_window".into(),
            message: "the window starts after now; give a --since time in the past".into(),
        })
    }

    /// What happened in a workspace since the point a recap asked for
    /// (roadmap C10), from the newest `SINCE_EVENTS` events; `None` when it
    /// asked for none.
    pub(super) fn since(&self, recap: &Command) -> StoreResult<Option<langspan_core::Since>> {
        let Command::Recap {
            workspace_id,
            since: Some(from),
            ..
        } = recap
        else {
            return Ok(None);
        };
        let (events, anchor, earlier) =
            Self::newest_events(&self.connection, langspan_core::SINCE_EVENTS)?;
        let query = langspan_core::SinceQuery {
            workspace_id,
            from: *from,
            events: &events,
            anchor,
            earlier,
            retained_from_ms: retention::since_ms(&self.connection)?,
            wall_now_ms: wall_now_ms(),
        };
        Ok(langspan_core::since(&self.state, &query))
    }

    /// The newest `limit` events, oldest first, with the anchor of the
    /// generation the first belongs to (when it was anchored) and whether
    /// older events exist. `today` and `report` read `today_events`.
    fn newest_events(
        connection: &Connection,
        limit: usize,
    ) -> StoreResult<(Vec<RecordedEvent>, Option<ClockAnchor>, bool)> {
        let mut statement =
            connection.prepare("SELECT seq, event_json FROM events ORDER BY seq DESC LIMIT ?1")?;
        let mut rows = statement.query([i64::try_from(limit)?])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            events.push(RecordedEvent {
                seq: u64::try_from(seq)?,
                event: serde_json::from_str(&json)?,
            });
        }
        events.reverse();
        let first = events.first().map_or(0, |recorded| recorded.seq);
        let first = i64::try_from(first)?;
        // Older events exist when the log holds some before the first one
        // read. What retention removed is the caller's to pass separately,
        // as its cutoff: the kept generation markers are older than it.
        let earlier: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE seq < ?1)",
            [first],
            |row| row.get(0),
        )?;
        // The generation the first event read belongs to, if it was anchored.
        let before: Option<String> = connection
            .query_row(ANCHOR_LOOKUP, [first], |row| row.get(0))
            .optional()?;
        let anchor = match before
            .map(|json| serde_json::from_str::<Event>(&json))
            .transpose()?
        {
            Some(Event::ClockAnchored { anchor }) => Some(anchor),
            _ => None,
        };
        Ok((events, anchor, earlier))
    }

    /// Just before sequence number `seq`: the sessions holding an intent,
    /// each with its intent's workspace, and each session's workspace, from
    /// the newest snapshot (a compaction removed the events before it) and
    /// the intent events, session registrations and workspace removals
    /// after it and before `seq`. At most `REPORT_HISTORY_EVENTS` of those
    /// are read, the newest: when more exist, the snapshot is not used and
    /// the third value says which sessions the events read made known, so a
    /// report's cost stays bounded however long the log grows.
    fn report_history_before(
        connection: &Connection,
        from: u64,
        budget: usize,
        seq: u64,
    ) -> StoreResult<ReportHistory> {
        let mut statement = connection.prepare_cached(INTENT_EVENTS_BEFORE)?;
        let mut rows = statement.query(params![
            i64::try_from(from)?,
            i64::try_from(seq)?,
            i64::try_from(budget + 1)?
        ])?;
        let mut newest_first = Vec::new();
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            newest_first.push(json);
        }
        let complete = newest_first.len() <= budget;
        newest_first.truncate(budget);
        let (mut open, mut members, mut known) = if !complete {
            (
                BTreeMap::new(),
                BTreeMap::new(),
                Some(langspan_core::ReportKnown::default()),
            )
        } else if from > 0 {
            let (snapshot, _) = retention::snapshot(connection)?;
            (
                snapshot
                    .intents
                    .iter()
                    .map(|(session, intent)| (session.clone(), intent.workspace_id.clone()))
                    .collect(),
                snapshot
                    .sessions
                    .iter()
                    .map(|(id, session)| (id.clone(), session.workspace_id.clone()))
                    .collect(),
                None,
            )
        } else {
            (BTreeMap::new(), BTreeMap::new(), None)
        };
        for json in newest_first.iter().rev() {
            let event = serde_json::from_str(json)?;
            langspan_core::open_intents(&mut open, &event);
            langspan_core::session_workspaces(&mut members, &event);
            if let Some(known) = known.as_mut() {
                langspan_core::known_sessions(known, &event);
            }
        }
        Ok((open, members, known))
    }

    /// The coordination report (roadmap H8, with H9's refusal outcomes):
    /// the newest `TODAY_EVENTS` events at most, placed in wall time as
    /// `today` places them, counted by the core. Reads only; it writes
    /// nothing and acknowledges no message.
    pub fn report(&self, workspace_id: &str, from_ms: u64, wall_ms: u64) -> StoreResult<Outcome> {
        if let Some(refused) = self.report_refused(workspace_id, from_ms, wall_ms) {
            return Ok(refused);
        }
        let limits = (self.today_events, self.report_history_events);
        Self::read_report(
            &self.connection,
            self.snapshot_seq,
            limits,
            workspace_id,
            from_ms,
            wall_ms,
        )
    }

    /// Why a report of `workspace_id` from `from_ms` cannot be read, from
    /// the state; the daemon asks this before a reader reads it.
    pub(crate) fn report_refused(
        &self,
        workspace_id: &str,
        from_ms: u64,
        wall_ms: u64,
    ) -> Option<Outcome> {
        self.refused_window(Some(workspace_id), from_ms, wall_ms)
    }

    /// The report from the log on `connection`, whose newest snapshot is
    /// at `snapshot_seq`, reading at most `limits` (`today_events`, then
    /// the older history events folded): on the state owner's connection,
    /// or a [`super::ReportReader`]'s own (issue #581).
    pub(super) fn read_report(
        connection: &Connection,
        snapshot_seq: u64,
        (today_events, history_events): (usize, usize),
        workspace_id: &str,
        from_ms: u64,
        wall_ms: u64,
    ) -> StoreResult<Outcome> {
        let (events, anchor, earlier) = Self::newest_events(connection, today_events)?;
        let retained_from_ms = retention::since_ms(connection)?;
        let (open_intents, session_workspaces, known) = Self::report_history_before(
            connection,
            snapshot_seq,
            history_events,
            events.first().map_or(0, |first| first.seq),
        )?;
        let mut report = langspan_core::report(&ReportQuery {
            workspace_id,
            window: TodayWindow {
                from_ms,
                to_ms: wall_ms,
            },
            events: &events,
            anchor,
            earlier,
            retained_from_ms,
            open_intents: &open_intents,
            session_workspaces: &session_workspaces,
            known: known.as_ref(),
            refusal_window_ms: refusal_window_ms(),
        });
        report.shrink_until(|report| {
            serde_json::to_vec(report).is_ok_and(|bytes| bytes.len() <= PAGE_BUDGET_BYTES)
        });
        Ok(Outcome::Report { report })
    }

    /// The refusal count (H5): the newest `COUNT_REFUSALS` refusals at
    /// most, placed by their generation's anchor and counted by the core.
    /// Reads only; it writes nothing.
    pub fn count(&self, days: &[u64]) -> StoreResult<Outcome> {
        if let Err(message) = langspan_core::validate_count_days(days) {
            return Ok(Outcome::Error {
                code: "invalid_window".into(),
                message,
            });
        }
        let mut statement = self.connection.prepare(REFUSAL_ROWS)?;
        let mut rows = statement.query([i64::try_from(langspan_core::COUNT_REFUSALS)?])?;
        let mut refusals = Vec::new();
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            refusals.push(RecordedEvent {
                seq: u64::try_from(seq)?,
                event: serde_json::from_str(&json)?,
            });
        }
        refusals.reverse();
        let earlier: bool = match refusals.first() {
            Some(first) => {
                self.connection
                    .query_row(REFUSAL_EARLIER, [i64::try_from(first.seq)?], |row| {
                        row.get(0)
                    })?
            }
            None => false,
        };
        let mut anchors = Vec::new();
        let mut statement = self.connection.prepare_cached(ANCHORS)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            if let Event::ClockAnchored { anchor } = serde_json::from_str(&json)? {
                anchors.push(anchor);
            }
        }
        let mut grants = self.connection.prepare(GRANTS_AFTER)?;
        let count = langspan_core::count_refusals(
            &langspan_core::CountQuery {
                days,
                refusals: &refusals,
                anchors: &anchors,
                earlier,
                retained_since_ms: retention::since_ms(&self.connection)?,
            },
            |session, workspace, after, paths| -> StoreResult<bool> {
                let mut rows =
                    grants.query(params![session, i64::try_from(after)?, RETRY_LOOKAHEAD])?;
                while let Some(row) = rows.next()? {
                    let json: String = row.get(0)?;
                    if let Event::LeaseGranted { lease } = serde_json::from_str(&json)? {
                        let mut granted: Vec<&String> = lease.paths.iter().collect();
                        granted.sort();
                        granted.dedup();
                        // A refusal keeps the first `REFUSAL_PATHS` paths,
                        // sorted: the grant's, cut the same way, must match.
                        if lease.workspace_id == workspace
                            && granted
                                .iter()
                                .take(langspan_core::REFUSAL_PATHS)
                                .copied()
                                .eq(paths.iter())
                        {
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            },
        )?;
        Ok(Outcome::Count { count })
    }

    /// A definition's history (roadmap D4): the newest `HISTORY_EVENTS`
    /// events of the kinds it reads before `before_seq`, the notes and
    /// messages after that point, and every generation's anchor, decided by
    /// the core. Reads only, through the `events_history` index, the
    /// forgettings of the notes read through `events_forgotten` and the
    /// withdrawals of the messages read through `events_retracted`, by note
    /// or message and not within a capped window; it writes nothing and
    /// delivers nothing.
    /// When the log holds older events than those read, the answer says
    /// `truncated` with a `next_seq` that reads on from there.
    pub fn history(
        &self,
        (workspace_id, name, path): (&str, &str, Option<&str>),
        limit: u32,
        before_seq: Option<u64>,
        now_ms: u64,
    ) -> StoreResult<Outcome> {
        let rows_of = |mut rows: rusqlite::Rows<'_>| -> StoreResult<Vec<RecordedEvent>> {
            let mut events = Vec::new();
            while let Some(row) = rows.next()? {
                let seq: i64 = row.get(0)?;
                let json: String = row.get(1)?;
                events.push(RecordedEvent {
                    seq: u64::try_from(seq)?,
                    event: serde_json::from_str(&json)?,
                });
            }
            Ok(events)
        };
        let read = |sql: &str, from: i64, count: usize| -> StoreResult<Vec<RecordedEvent>> {
            let mut statement = self.connection.prepare_cached(sql)?;
            rows_of(statement.query(params![from, i64::try_from(count)?])?)
        };
        let before = before_seq.map_or(i64::MAX, |seq| i64::try_from(seq).unwrap_or(i64::MAX));
        let mut events = read(HISTORY_BEFORE, before, HISTORY_EVENTS)?;
        // The oldest event read, when the log may hold older ones.
        let older = (events.len() == HISTORY_EVENTS)
            .then(|| events.last().map(|recorded| recorded.seq))
            .flatten();
        if before_seq.is_some() {
            events.extend(read(HISTORY_FOLLOWING, before, HISTORY_EVENTS)?);
        }
        // A note read here may have been forgotten after every event read:
        // its forgetting is looked up by note, never within a capped window,
        // so a forgotten note's text cannot come back.
        let read_seqs: std::collections::BTreeSet<u64> =
            events.iter().map(|recorded| recorded.seq).collect();
        let notes: std::collections::BTreeSet<String> = events
            .iter()
            .filter_map(|recorded| match &recorded.event {
                Event::Remembered { note, .. } if note.workspace_id == workspace_id => {
                    Some(note.note_id.clone())
                }
                _ => None,
            })
            .collect();
        let mut statement = self.connection.prepare_cached(FORGETTING_LOOKUP)?;
        for note_id in &notes {
            let forgettings = rows_of(statement.query([note_id])?)?;
            events.extend(
                forgettings
                    .into_iter()
                    .filter(|recorded| !read_seqs.contains(&recorded.seq)),
            );
        }
        drop(statement);
        // Likewise a message read here may have been withdrawn after every
        // event read; withdrawals are not among the kinds read above, so
        // each is looked up by message.
        let messages: std::collections::BTreeSet<String> = events
            .iter()
            .filter_map(|recorded| match &recorded.event {
                Event::MessageSent { message, .. } if message.workspace_id == workspace_id => {
                    Some(message.message_id.clone())
                }
                _ => None,
            })
            .collect();
        let mut statement = self.connection.prepare_cached(RETRACTION_LOOKUP)?;
        for message_id in &messages {
            events.extend(rows_of(statement.query([message_id])?)?);
        }
        drop(statement);
        // A moved file's earlier paths hold on every page (roadmap D7).
        let mut statement = self.connection.prepare_cached(MOVES_LOOKUP)?;
        let moves = rows_of(statement.query([i64::try_from(HISTORY_MOVES)?])?)?;
        drop(statement);
        let mut anchors = Vec::new();
        let mut statement = self.connection.prepare_cached(ANCHORS)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            if let Event::ClockAnchored { anchor } = serde_json::from_str(&json)? {
                anchors.push(anchor);
            }
        }
        let query = HistoryQuery {
            workspace_id,
            name,
            path,
            limit,
            before_seq,
            events: &events,
            moves: &moves,
            anchors: &anchors,
            now_ms,
        };
        let mut history = match definition_history(&self.state, &query) {
            Ok(history) => history,
            Err(failure) => {
                return Ok(Outcome::Error {
                    code: failure.code,
                    message: failure.message,
                });
            }
        };
        if let (Some(oldest), false) = (older, history.truncated) {
            history.truncated = true;
            history.next_seq = Some(oldest);
        }
        history.shrink_until(|history| {
            serde_json::to_vec(history).is_ok_and(|bytes| bytes.len() <= HISTORY_BUDGET_BYTES)
        });
        Ok(Outcome::History { history })
    }

    pub(super) fn changes(
        &self,
        workspace_id: &str,
        after: u64,
        limit: u32,
    ) -> StoreResult<Outcome> {
        if !self.state.workspaces.contains_key(workspace_id) {
            return Ok(Outcome::Error {
                code: "unknown_workspace".into(),
                message: "register this workspace before reading its changes".into(),
            });
        }
        if !(1..=1000).contains(&limit) {
            return Ok(Outcome::Error {
                code: "invalid_limit".into(),
                message: "change limit must be between 1 and 1000".into(),
            });
        }
        let summary: Option<String> = self
            .connection
            .query_row(
                "SELECT summary_json FROM git_snapshots WHERE workspace_id=?1",
                [workspace_id],
                |row| row.get(0),
            )
            .optional()?;
        let scan = summary
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        // Every client reads both types, so the page needs no client's
        // filter before its limit, as `event_page` has (tests/protocol.rs).
        let mut statement = self.connection.prepare(
            "SELECT seq,event_json FROM events
             WHERE json_extract(event_json,'$.type') IN ('disk_observed','branch_observed')
               AND (CASE WHEN json_valid(event_json) THEN json_extract(event_json,'$.observation.workspace_id') END)=?1 AND seq>?2
             ORDER BY seq LIMIT ?3"
        )?;
        let mut rows = statement.query(params![
            workspace_id,
            i64::try_from(after).unwrap_or(i64::MAX),
            i64::from(limit)
        ])?;
        let mut events = Vec::new();
        let mut bytes = 0;
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            if bytes + json.len() + 64 > 48 * 1024 {
                if events.is_empty() {
                    return Err("one observation exceeds the transport page budget".into());
                }
                break;
            }
            bytes += json.len() + 64;
            events.push(RecordedEvent {
                seq: u64::try_from(seq)?,
                event: serde_json::from_str(&json)?,
            });
        }
        Ok(Outcome::Changes { events, scan })
    }
}
