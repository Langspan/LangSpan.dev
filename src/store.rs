//! The daemon's single SQLite owner. The caller holds the lifetime OS lock.
//!
//! Decisions are applied to a private candidate state, persisted with their
//! receipts, and published only after commit. Receipts retain original outcomes;
//! replay validity is calculated from the current recovered state.

use langspan_core::{
    ClockAnchor, Command, Event, LeaseStatus, Outcome, Route, State, TODAY_EVENTS, apply, decide,
    expiry_events, shrink_status,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;
use std::time::Duration;

/// The scan failure recorded when a workspace's root or Git directory now
/// names another checkout. Admission reads it back from the state.
pub const IDENTITY_CHANGED: &str =
    "Git identity changed since the previous scan; register the replacement as a new workspace";

/// The wall clock in Unix milliseconds; 0 if it reads before the epoch.
pub fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// A read answer's body stops before this many bytes, leaving the rest of
/// the 64 KiB frame for the envelope and JSON escaping of the envelope.
pub const PAGE_BUDGET_BYTES: usize = 48 * 1024;

/// The smallest status-records page a reader can ask for. It leaves room for
/// at least one part of a record however its text escapes, so every page
/// moves the cursor on.
pub const MIN_PAGE_BYTES: usize = 1024;

/// A session's admissions a retry is looked for in, after a refusal: it
/// may run other edits of the same reply first.
pub const RETRY_LOOKAHEAD: u32 = 16;
/// A history's answer stops before this many bytes: the MCP result carries
/// it twice, once escaped again as text, inside one 64 KiB frame.
pub const HISTORY_BUDGET_BYTES: usize = 28 * 1024;

#[macro_use]
mod sql;
pub mod chain;
mod messages;
mod paging;
mod reads;
mod replay;
mod report_reader;
mod retention;
mod scan;

use paging::workspace_page;
pub use report_reader::ReportReader;
use sql::{
    COUNT_INDEXES, FORGOTTEN_INDEX, GENERATION_MARKER_INDEX, HISTORY_INDEX, INTENT_EVENT_INDEX,
    MOVES_INDEX, RETRACTED_INDEX,
};

/// The SQLite schema marker this build writes.
const SCHEMA_VERSION: i64 = 4;

/// The newest build that has opened the store (ADR 0038): one row, raised by
/// a later build and never lowered, so that an earlier build that cannot read
/// the store names the build that can. A table, not a schema change: builds
/// without it ignore it.
const NEWEST_WRITER: &str = "CREATE TABLE IF NOT EXISTS newest_writer (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version TEXT NOT NULL
)";

/// A release version's three numbers; `None` for anything else. A
/// pre-release or build suffix is ignored.
fn release(version: &str) -> Option<[u64; 3]> {
    if version.len() > 32 {
        return None;
    }
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let numbers = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(numbers)
}

pub type StoreResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone)]
pub struct Execution {
    pub outcome: Outcome,
    pub replayed: bool,
    pub generation: u64,
    pub lease_valid: Option<bool>,
}

pub struct Store {
    connection: Connection,
    state: State,
    /// The newest snapshot's sequence: events at or below it are not
    /// replayed at start. Zero when there is none.
    snapshot_seq: u64,
    /// Wall-clock Unix milliseconds, stamped on every event and receipt so
    /// that retention can tell their age. Not the daemon's monotonic clock,
    /// which starts again at every boot.
    wall_ms: Box<dyn Fn() -> u64 + Send>,
    /// A test-only pause inside the compaction transaction, before it
    /// commits, so that a test can kill the daemon there.
    compaction_pause: Option<Duration>,
    tap: Option<Tap>,
    /// The digest the next event chains from (`chain`).
    head: String,
    /// The newest events `today` and `report` read: `TODAY_EVENTS`, which
    /// a test lowers to cross it with a short log.
    today_events: usize,
    /// The older intent and registration events a report folds at most:
    /// `REPORT_HISTORY_EVENTS`, which a test lowers likewise.
    report_history_events: usize,
}

/// Events committed since the tap was last drained that its filter keeps,
/// at most `TAP_LIMIT`: a consumer reads facts already recorded, after they
/// are durable, and never changes what is committed.
struct Tap {
    keep: fn(&Event) -> bool,
    events: Vec<Event>,
}

/// The most events a tap holds between drains; later ones are not kept.
const TAP_LIMIT: usize = 64;

/// A check record written before check commands were redacted can still
/// hold a command line as typed. It is redacted wherever it is read back:
/// into state, into an event page and from a receipt.
fn redact_event(event: &mut Event) {
    if let Event::CheckRecorded { check, .. } = event {
        crate::text::redact::redact_check(check);
    }
}

/// `event` as it may be shown, with the JSON its page budget is measured by.
fn shown_event(mut event: Event, stored: String) -> StoreResult<(Event, String)> {
    if !matches!(event, Event::CheckRecorded { .. }) {
        return Ok((event, stored));
    }
    redact_event(&mut event);
    let json = serde_json::to_string(&event)?;
    Ok((event, json))
}

impl Store {
    pub fn open(path: &Path) -> StoreResult<Self> {
        Self::open_with_wall_clock(path, Box::new(wall_now_ms))
    }

    /// Open with the wall clock that stamps records' ages for retention.
    pub fn open_with_wall_clock(
        path: &Path,
        wall_ms: Box<dyn Fn() -> u64 + Send>,
    ) -> StoreResult<Self> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !(0..=SCHEMA_VERSION).contains(&version) {
            return Err(format!(
                "unsupported SQLite schema version {version}; expected at most {SCHEMA_VERSION}"
            )
            .into());
        }
        if version == 0 {
            let tables: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )?;
            if tables != 0 {
                return Err(
                    "unversioned database contains tables; refusing to reset history".into(),
                );
            }
        }

        let journal: String =
            connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !journal.eq_ignore_ascii_case("wal") {
            return Err(
                "SQLite WAL could not be enabled; use a supported file-backed database".into(),
            );
        }
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        if version == 0 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "CREATE TABLE events (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_json TEXT NOT NULL
                 );
                 CREATE TABLE receipts (
                    request_id TEXT PRIMARY KEY NOT NULL,
                    command_json TEXT NOT NULL,
                    outcome_json TEXT NOT NULL
                 );
                 PRAGMA user_version = 1;",
            )?;
            transaction.commit()?;
        }

        if version < 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "CREATE TABLE git_snapshots (
                    workspace_id TEXT PRIMARY KEY NOT NULL,
                    snapshot_json TEXT NOT NULL,
                    summary_json TEXT NOT NULL
                 );
                 CREATE INDEX observation_workspace_sequence ON events(
                    CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.observation.workspace_id') END, seq
                 );
                 PRAGMA user_version = 2;"
            )?;
            transaction.commit()?;
        }

        if version < 3 {
            // Version 3 adds event kinds, not tables. The marker makes an older
            // binary refuse this history instead of failing partway through it.
            connection.pragma_update(None, "user_version", 3)?;
        }
        // An index, not a format change: a store of any version 3 gains it
        // when opened (one pass over the log), and a build without it
        // reads and writes the same rows.
        connection.execute_batch(GENERATION_MARKER_INDEX)?;
        connection.execute_batch(INTENT_EVENT_INDEX)?;
        // The same for the monthly count (H5): two partial indexes.
        connection.execute_batch(COUNT_INDEXES)?;
        connection.execute_batch(HISTORY_INDEX)?;
        connection.execute_batch(FORGOTTEN_INDEX)?;
        connection.execute_batch(RETRACTED_INDEX)?;
        connection.execute_batch(MOVES_INDEX)?;

        if version < 4 {
            // Version 4 adds retention: each record's wall-clock time and the
            // snapshots compaction writes. Records from before it have no
            // time of their own, so they count from this upgrade.
            let now = i64::try_from(wall_ms()).unwrap_or(i64::MAX);
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "ALTER TABLE events ADD COLUMN recorded_at_ms INTEGER;
                 ALTER TABLE receipts ADD COLUMN recorded_at_ms INTEGER;
                 CREATE TABLE snapshots (
                    seq INTEGER PRIMARY KEY NOT NULL,
                    state_json TEXT NOT NULL,
                    cutoff_ms INTEGER NOT NULL,
                    created_at_ms INTEGER NOT NULL
                 );",
            )?;
            transaction.execute("UPDATE events SET recorded_at_ms = ?1", [now])?;
            transaction.execute("UPDATE receipts SET recorded_at_ms = ?1", [now])?;
            transaction.execute_batch("PRAGMA user_version = 4;")?;
            transaction.commit()?;
        }

        let (state, snapshot_seq) = Self::recover_as_newest_writer(&connection)?;
        // After the store proved readable, so a store this build refuses
        // is left as it was (ADR 0038).
        chain::migrate(&connection)?;
        let head = chain::extend(&connection)?;
        let generation = state
            .generation
            .checked_add(1)
            .ok_or("authority generation exhausted; refusing to reuse a generation")?;
        let mut store = Self {
            connection,
            state,
            snapshot_seq,
            wall_ms,
            compaction_pause: None,
            tap: None,
            head,
            today_events: TODAY_EVENTS,
            report_history_events: langspan_core::REPORT_HISTORY_EVENTS,
        };
        // The generation's clock against the wall clock, so times can be
        // placed on a day (the status line and `today`). The same wall clock
        // stamps records' ages, so a retention cutoff and a placed time are
        // on one clock.
        let anchor = ClockAnchor {
            generation,
            clock_ms: crate::clock::now_ms(),
            wall_ms: (store.wall_ms)(),
        };
        store.commit(
            &[
                Event::Booted { generation },
                Event::ClockAnchored { anchor },
            ],
            None,
        )?;
        Ok(store)
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// From now on, keep each committed event `keep` accepts, for
    /// `take_tapped`. Events replayed when the store opened are not kept.
    pub fn tap(&mut self, keep: fn(&Event) -> bool) {
        self.tap = Some(Tap {
            keep,
            events: Vec::new(),
        });
    }

    /// The events kept since the last call, oldest first.
    pub fn take_tapped(&mut self) -> Vec<Event> {
        self.tap
            .as_mut()
            .map(|tap| std::mem::take(&mut tap.events))
            .unwrap_or_default()
    }

    pub fn refresh(&mut self, now_ms: u64) -> StoreResult<()> {
        let events = expiry_events(&self.state, now_ms);
        if !events.is_empty() {
            self.commit(&events, None)?;
        }
        Ok(())
    }

    pub fn execute(
        &mut self,
        request_id: &str,
        original: &Command,
        prepared: &Command,
        now_ms: u64,
    ) -> StoreResult<Execution> {
        // Refused before a receipt or an event could hold it.
        if let Some(refused) = crate::text::redact::refuse_secret(original) {
            return Ok(self.execution(refused, false, now_ms));
        }
        if let Some(replayed) = self.replay(request_id, original, now_ms)? {
            return Ok(replayed);
        }
        let outcome = match original.route() {
            // A read: decided by the core, never recorded.
            Route::Read => {
                match decide(&self.state, prepared, request_id, now_ms).outcome {
                    // What matters now, in one frame however long the history.
                    Outcome::Status {
                        mut state,
                        summary: Some(mut summary),
                    } => {
                        shrink_status(&mut state, &mut summary, |state, summary| {
                            serde_json::to_vec(&(state, summary))
                                .is_ok_and(|bytes| bytes.len() <= PAGE_BUDGET_BYTES)
                        });
                        Outcome::Status {
                            state,
                            summary: Some(summary),
                        }
                    }
                    Outcome::Dashboard { mut dashboard } => {
                        dashboard.denials = Some(self.denials(now_ms)?);
                        Outcome::Dashboard { dashboard }
                    }
                    Outcome::Workspaces { workspaces, next } => workspace_page(workspaces, next),
                    // Oldest trails first out of an answer past one frame,
                    // counted in `omitted`.
                    Outcome::Trails {
                        workspace_id,
                        mut trails,
                        mut omitted,
                    } => {
                        while trails.len() > 1
                            && serde_json::to_vec(&trails).is_ok_and(|b| b.len() > 48 * 1024)
                        {
                            trails.pop();
                            omitted += 1;
                        }
                        Outcome::Trails {
                            workspace_id,
                            trails,
                            omitted,
                        }
                    }
                    // A recap or a recall must fit one frame with room for
                    // the envelope.
                    Outcome::Recap { mut recap } => {
                        recap.since = self.since(prepared)?;
                        recap.shrink_until(|recap| {
                            serde_json::to_vec(recap).is_ok_and(|bytes| bytes.len() <= 48 * 1024)
                        });
                        Outcome::Recap { recap }
                    }
                    // Within the daemon's frame. The MCP result carries the
                    // answer twice, once escaped again as text, so the MCP
                    // adapter trims it once more against its encoded result.
                    Outcome::Recall { mut recall } => {
                        recall.shrink_until(|recall| {
                            serde_json::to_vec(recall).is_ok_and(|bytes| bytes.len() <= 28 * 1024)
                        });
                        Outcome::Recall { recall }
                    }
                    outcome => outcome,
                }
            }
            Route::Store => self.stored(original, prepared, request_id, now_ms)?,
            Route::Decide | Route::Index | Route::Internal => {
                let decision = decide(&self.state, prepared, request_id, now_ms);
                // A hook run inside the interval writes nothing, not even a
                // receipt, so a burst of them costs no storage (roadmap J12).
                // Nor does a between-step watcher's ask with nothing to tell,
                // made about once a second while a turn runs (issue #214), or
                // the indexer's anchor recheck after a pass that changed no
                // note's state (roadmap E1).
                let quiet = matches!(
                    original,
                    Command::SessionSeen { .. }
                        | Command::BetweenSteps { .. }
                        | Command::ObserveNoteAnchors { .. }
                ) && decision.events.is_empty();
                if !quiet {
                    self.commit(
                        &decision.events,
                        Some((request_id, original, &decision.outcome)),
                    )?;
                }
                decision.outcome
            }
        };
        Ok(self.execution(outcome, false, now_ms))
    }

    /// A command answered from the event log, not decided by the core.
    fn stored(
        &mut self,
        command: &Command,
        prepared: &Command,
        request_id: &str,
        now_ms: u64,
    ) -> StoreResult<Outcome> {
        Ok(match command {
            Command::StatusRecords { after, page_bytes } => {
                self.status_page(after.as_deref(), *page_bytes, |record| {
                    serde_json::to_string(record).map_err(|error| error.to_string())
                })
            }
            Command::Events { after, limit } => self.events(*after, *limit)?,
            Command::Compact { keep_days, apply } => self.compact(*keep_days, *apply)?,
            Command::Today {
                workspace_id,
                from_ms,
            } => self.today(workspace_id.as_deref(), *from_ms, wall_now_ms(), now_ms)?,
            Command::Report {
                workspace_id,
                from_ms,
            } => self.report(workspace_id, *from_ms, wall_now_ms())?,
            Command::Count { days } => self.count(days)?,
            Command::History {
                workspace_id,
                name,
                path,
                limit,
                before_seq,
            } => self.history(
                (workspace_id, name, path.as_deref()),
                *limit,
                *before_seq,
                now_ms,
            )?,
            Command::RemoteSent {
                workspace_id,
                limit,
            } => self.sent(workspace_id, *limit)?,
            Command::ShowMessage {
                workspace_id,
                message_id,
            } => self.show_message(workspace_id, message_id)?,
            Command::MessageStatus {
                workspace_id,
                from,
                message_id,
            } => {
                self.message_status(workspace_id, from.as_deref(), message_id.as_deref(), now_ms)?
            }
            Command::Changes {
                workspace_id,
                after,
                limit,
            } => self.changes(workspace_id, *after, *limit)?,
            // Nothing else routes here; the core refuses it.
            _ => decide(&self.state, prepared, request_id, now_ms).outcome,
        })
    }

    fn execution(&self, outcome: Outcome, replayed: bool, now_ms: u64) -> Execution {
        let lease_id = match &outcome {
            Outcome::Admitted { lease } => Some(&lease.lease_id),
            Outcome::Completed { lease_id } => Some(lease_id),
            Outcome::Observed { observation } => observation.lease_id.as_ref(),
            _ => None,
        };
        let lease_valid = lease_id.map(|id| {
            self.state.leases.get(id).is_some_and(|lease| {
                lease.status == LeaseStatus::Active
                    && lease.generation == self.state.generation
                    && lease.expires_at_ms > now_ms
            })
        });
        Execution {
            outcome,
            replayed,
            generation: self.state.generation,
            lease_valid,
        }
    }

    fn commit(
        &mut self,
        events: &[Event],
        receipt: Option<(&str, &Command, &Outcome)>,
    ) -> StoreResult<()> {
        self.commit_with_snapshot(events, receipt, None)
    }

    fn commit_with_snapshot(
        &mut self,
        events: &[Event],
        receipt: Option<(&str, &Command, &Outcome)>,
        snapshot: Option<(&str, &str, &str)>,
    ) -> StoreResult<()> {
        let mut next_state = self.state.clone();
        for event in events {
            apply(&mut next_state, event)?;
        }
        let recorded_at = i64::try_from((self.wall_ms)()).unwrap_or(i64::MAX);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut head = self.head.clone();
        for event in events {
            let json = serde_json::to_string(event)?;
            head = chain::link(&head, &json);
            transaction.execute(
                "INSERT INTO events(event_json, recorded_at_ms, digest) VALUES (?1, ?2, ?3)",
                params![json, recorded_at, head],
            )?;
            // The last scan was of the old checkout: the next scan of the
            // new root starts a fresh baseline, so files that differ between
            // the two checkouts are not reported as changes.
            if let Event::WorkspaceMoved {
                workspace: langspan_core::Workspace { workspace_id, .. },
                ..
            }
            | Event::WorkspaceRemoved { workspace_id } = event
            {
                transaction.execute(
                    "DELETE FROM git_snapshots WHERE workspace_id = ?1",
                    [workspace_id],
                )?;
            }
        }
        if let Some((request_id, command, outcome)) = receipt {
            transaction.execute(
                "INSERT INTO receipts(request_id, command_json, outcome_json, recorded_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    request_id,
                    serde_json::to_string(command)?,
                    serde_json::to_string(outcome)?,
                    recorded_at,
                ],
            )?;
        }
        if let Some((workspace_id, snapshot_json, summary_json)) = snapshot {
            transaction.execute(
                "INSERT INTO git_snapshots(workspace_id,snapshot_json,summary_json) VALUES (?1,?2,?3)
                 ON CONFLICT(workspace_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,summary_json=excluded.summary_json",
                params![workspace_id, snapshot_json, summary_json],
            )?;
        }
        transaction.commit()?;
        self.state = next_state;
        self.head = head;
        if let Some(tap) = &mut self.tap {
            let room = TAP_LIMIT.saturating_sub(tap.events.len());
            let keep = tap.keep;
            tap.events.extend(
                events
                    .iter()
                    .filter(|event| keep(event))
                    .take(room)
                    .cloned(),
            );
        }
        Ok(())
    }

    /// `recover`, then record this build as the newest writer if it is. When
    /// a newer build wrote the store and this one cannot read it, the error
    /// is one line naming that build (ADR 0038).
    fn recover_as_newest_writer(connection: &Connection) -> StoreResult<(State, u64)> {
        connection.execute_batch(NEWEST_WRITER)?;
        let newest = connection
            .query_row("SELECT version FROM newest_writer", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .and_then(|version| release(&version));
        let this = release(env!("CARGO_PKG_VERSION"));
        let recovered = Self::recover(connection).map_err(|error| match (newest, this) {
            (Some(newer), Some(this)) if newer > this => {
                let [major, minor, patch] = newer;
                let newer = format!("{major}.{minor}.{patch}");
                format!(
                    "this store was last written by LangSpan {newer} and holds records this build ({}) cannot read; start LangSpan {newer} or later on this state directory, or move the state directory aside to start with an empty store",
                    env!("CARGO_PKG_VERSION")
                )
                .into()
            }
            _ => error,
        })?;
        // `None` orders first: a build of no release version never records.
        if this > newest {
            connection.execute(
                "INSERT INTO newest_writer (id, version) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET version = excluded.version",
                [env!("CARGO_PKG_VERSION")],
            )?;
        }
        Ok(recovered)
    }

    /// The state the newest snapshot and the events after it give, and the
    /// snapshot's sequence.
    fn recover(connection: &Connection) -> StoreResult<(State, u64)> {
        let (mut state, snapshot_seq) = retention::snapshot(connection)?;
        retention::replay_after(connection, &mut state, snapshot_seq, |_, _| Ok(()))?;

        // Receipts only answer retries. One this build cannot read (written
        // by another version, or damaged) does not stop the store opening:
        // it is counted, and its request is refused if it is ever retried.
        let mut statement =
            connection.prepare("SELECT command_json, outcome_json FROM receipts")?;
        let mut rows = statement.query([])?;
        let mut unreadable = 0u64;
        while let Some(row) = rows.next()? {
            let command: String = row.get(0)?;
            let outcome: String = row.get(1)?;
            if serde_json::from_str::<Command>(&command).is_err()
                || serde_json::from_str::<Outcome>(&outcome).is_err()
            {
                unreadable += 1;
            }
        }
        if unreadable > 0 {
            eprintln!(
                "{}",
                serde_json::json!({
                    "level": "warn",
                    "event": "receipts_unreadable",
                    "count": unreadable,
                    "detail": "receipts this build cannot read were skipped; their requests cannot be replayed and a retry of one is refused",
                })
            );
        }
        Ok((state, snapshot_seq))
    }
}

#[cfg(test)]
mod tests {
    use super::sql::*;
    use super::*;
    use langspan_core::HISTORY_EVENTS;

    /// Issue #259, at the store: events the client does not read count
    /// against neither the page's bytes nor its limit, and those after the
    /// last event it reads are left to the next page.
    #[test]
    fn an_event_page_does_not_spend_its_budget_on_unread_events() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-event-page-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut store = Store::open(&directory.join("state.sqlite")).unwrap();
        // Thirty workspaces with 2 KB roots: 60 KB, past one page's bytes.
        let mut commands: Vec<Command> = (0..30)
            .map(|n| Command::WorkspaceRegister {
                workspace_id: format!("w{n}"),
                root: format!("/{n}{}", "a".repeat(2000)),
            })
            .collect();
        let register = |session: &str| Command::SessionRegister {
            session_id: session.into(),
            workspace_id: "w0".into(),
            parent: None,
            headless: false,
        };
        commands.extend([register("one"), register("two")]);
        commands.push(Command::WorkspaceRegister {
            workspace_id: "late".into(),
            root: "/late".into(),
        });
        for (index, command) in commands.iter().enumerate() {
            let execution = store
                .execute(&format!("fixture-{index}"), command, command, 1)
                .unwrap();
            assert!(!matches!(execution.outcome, Outcome::Error { .. }));
        }
        let sessions = |event: &Event| matches!(event, Event::SessionRegistered { .. });
        let page = |after, limit| match store.event_page(after, limit, sessions).unwrap() {
            Outcome::Events { events } => events,
            other => panic!("{other:?}"),
        };
        let first = page(0, 1);
        let last = first.last().unwrap();
        assert!(
            sessions(&last.event),
            "the page reached a session: {first:?}"
        );
        assert!(first.len() > 30, "the workspaces were held for the reply");
        // The second session ends the page: the workspace after it is the
        // next page's, with nothing the client reads.
        let second = page(last.seq, 1);
        assert!(sessions(&second.last().unwrap().event));
        let rest = page(second.last().unwrap().seq, 1);
        assert_eq!(rest.len(), 1, "{rest:?}");
        assert!(!sessions(&rest[0].event));
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// `count` runs in the daemon's request handler too: reading refusals
    /// and a session's later admissions must use their partial indexes, not
    /// read the whole log.
    #[test]
    fn the_count_lookups_read_their_indexes() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-count-plan-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let store = Store::open(&directory.join("state.sqlite")).unwrap();
        let plan = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Vec<String> {
            store
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params, |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let refusals = plan(REFUSAL_ROWS, &[&10i64]);
        let earlier = plan(REFUSAL_EARLIER, &[&10i64]);
        let grants = plan(GRANTS_AFTER, &[&"session", &1i64, &16i64]);
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        for (steps, index) in [
            (&refusals, "events_write_refused"),
            (&earlier, "events_write_refused"),
            (&grants, "events_grant_session"),
        ] {
            assert!(steps.iter().any(|step| step.contains(index)), "{steps:?}");
        }
    }

    /// `today` runs in the daemon's request handler: finding the anchor must
    /// read the generation markers only, not every older event of a long
    /// generation.
    #[test]
    fn the_anchor_lookup_reads_only_generation_markers() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-anchor-plan-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let store = Store::open(&directory.join("state.sqlite")).unwrap();
        let plan: Vec<String> = store
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {ANCHOR_LOOKUP}"))
            .unwrap()
            .query_map([1_000_000i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        assert!(
            plan.iter()
                .any(|step| step.contains("events_generation_marker")),
            "{plan:?}"
        );
    }

    /// Review 4704154106: Alice registers in `work` before the newest
    /// events the report reads; among them she reads a file, `work` is
    /// removed and she registers the same session in `elsewhere`. The read
    /// counts for `work` (registered again), the workspace she was in then,
    /// and not for `elsewhere`. Real SQLite: her first registration is only in the
    /// older part of the log.
    #[test]
    fn a_session_registered_again_elsewhere_keeps_its_earlier_read_in_its_old_workspace() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-report-members-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut store = Store::open(&directory.join("state.sqlite")).unwrap();
        // The report reads the newest 8 events, so a short log crosses it.
        store.today_events = 8;
        let run = |store: &mut Store, id: &str, command: Command| {
            let now = crate::clock::now_ms();
            let outcome = store.execute(id, &command, &command, now).unwrap().outcome;
            assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
        };
        let alice = "claude-code.alice";
        for workspace in ["work", "elsewhere"] {
            run(
                &mut store,
                &format!("w-{workspace}"),
                Command::WorkspaceRegister {
                    workspace_id: workspace.into(),
                    root: format!("/repos/{workspace}"),
                },
            );
        }
        let register = |session: &str, workspace: &str| Command::SessionRegister {
            session_id: session.into(),
            workspace_id: workspace.into(),
            parent: None,
            headless: false,
        };
        run(&mut store, "s-alice", register(alice, "work"));
        run(
            &mut store,
            "s-filler",
            register("claude-code.filler", "elsewhere"),
        );
        let from_ms = wall_now_ms();
        // Enough events after the registration that it is not among the
        // newest `today_events` read.
        let generation = store.state.generation;
        let filler: Vec<Event> = (0..store.today_events)
            .map(|_| Event::SessionSeen {
                session_id: "claude-code.filler".into(),
                at_ms: crate::clock::now_ms(),
                generation,
            })
            .collect();
        store.commit(&filler, None).unwrap();
        // A prompt carries a time of its own, which places the read after it.
        run(
            &mut store,
            "p",
            Command::SessionPrompt {
                session_id: alice.into(),
                summary: "carry on".into(),
                line_digests: Vec::new(),
            },
        );
        run(
            &mut store,
            "read",
            Command::ObserveSource {
                session_id: alice.into(),
                files: vec![langspan_core::ObservedFile {
                    path: "src/lib.rs".into(),
                    state: langspan_core::FileState::Missing,
                }],
                origin: langspan_core::SourceOrigin::Read,
            },
        );
        run(
            &mut store,
            "r",
            Command::WorkspaceRemove {
                workspace_id: "work".into(),
                apply: true,
            },
        );
        run(&mut store, "s-again", register(alice, "elsewhere"));
        // Registered again, so its report can be asked for.
        run(
            &mut store,
            "w-work-again",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/repos/work".into(),
            },
        );
        let to_ms = wall_now_ms() + 1;
        let reads = |store: &Store, workspace: &str| {
            let outcome = store.report(workspace, from_ms, to_ms).unwrap();
            let Outcome::Report { report } = outcome else {
                panic!("{outcome:?}");
            };
            assert!(report.truncated, "{report:#?}");
            report
                .sessions
                .iter()
                .find(|row| row.session_id == alice)
                .map(|row| row.counts.reads_reported)
        };
        let (work, elsewhere) = (reads(&store, "work"), reads(&store, "elsewhere"));
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(work, Some(1));
        assert_eq!(elsewhere, Some(0));
    }

    /// Review 4703461855: an intent set before the newest `TODAY_EVENTS`
    /// events, whose session ends inside the window, is counted as left
    /// open. Real SQLite: the intent is only in the older part of the log.
    #[test]
    fn an_intent_set_before_the_events_read_is_left_open_when_its_session_ends() {
        assert_eq!(intents_left_open_after_older_log(false), 1);
    }

    /// Review 4703929181: removing the workspace ends the intent with it, so
    /// the session registered again under the same ID and ended without a
    /// new intent left none open, though the removal is in the older log.
    #[test]
    fn a_workspace_removed_before_the_events_read_ends_its_intents() {
        assert_eq!(intents_left_open_after_older_log(true), 0);
    }

    /// Alice sets an intent, then (when `remove`) the workspace is removed
    /// and registered again with her session, all before the newest
    /// `TODAY_EVENTS` events; she ends her session inside the window. Her
    /// `intents_left_open`.
    fn intents_left_open_after_older_log(remove: bool) -> u32 {
        let directory = std::env::temp_dir().join(format!(
            "langspan-report-intent-{remove}-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut store = Store::open(&directory.join("state.sqlite")).unwrap();
        let run = |store: &mut Store, id: &str, command: Command| {
            let now = crate::clock::now_ms();
            store.execute(id, &command, &command, now).unwrap();
        };
        run(
            &mut store,
            "w",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/repos/work".into(),
            },
        );
        for session in ["claude-code.alice", "claude-code.filler"] {
            run(
                &mut store,
                &format!("s-{session}"),
                Command::SessionRegister {
                    session_id: session.into(),
                    workspace_id: "work".into(),
                    parent: None,
                    headless: false,
                },
            );
        }
        run(
            &mut store,
            "i",
            Command::IntentSet {
                session_id: "claude-code.alice".into(),
                paths: vec!["src/lib.rs".into()],
                summary: "rework the parser".into(),
            },
        );
        if remove {
            run(
                &mut store,
                "r",
                Command::WorkspaceRemove {
                    workspace_id: "work".into(),
                    apply: true,
                },
            );
            run(
                &mut store,
                "w2",
                Command::WorkspaceRegister {
                    workspace_id: "work".into(),
                    root: "/repos/work".into(),
                },
            );
            for session in ["claude-code.alice", "claude-code.filler"] {
                run(
                    &mut store,
                    &format!("s2-{session}"),
                    Command::SessionRegister {
                        session_id: session.into(),
                        workspace_id: "work".into(),
                        parent: None,
                        headless: false,
                    },
                );
            }
        }
        let from_ms = wall_now_ms();
        // Enough events after the intent that it is not among those read.
        let generation = store.state.generation;
        let filler: Vec<Event> = (0..TODAY_EVENTS)
            .map(|_| Event::SessionSeen {
                session_id: "claude-code.filler".into(),
                at_ms: crate::clock::now_ms(),
                generation,
            })
            .collect();
        store.commit(&filler, None).unwrap();
        // A prompt carries a time of its own, which places the end after it.
        run(
            &mut store,
            "p",
            Command::SessionPrompt {
                session_id: "claude-code.alice".into(),
                summary: "carry on".into(),
                line_digests: Vec::new(),
            },
        );
        run(
            &mut store,
            "e",
            Command::SessionEnd {
                session_id: "claude-code.alice".into(),
                trail: None,
            },
        );
        let outcome = store.report("work", from_ms, wall_now_ms() + 1).unwrap();
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        let Outcome::Report { report } = outcome else {
            panic!("{outcome:?}");
        };
        let alice = report
            .sessions
            .iter()
            .find(|row| row.session_id == "claude-code.alice")
            .unwrap_or_else(|| panic!("{report:#?}"));
        assert_eq!(alice.counts.intents_set, 0, "{report:#?}");
        alice.counts.intents_left_open
    }

    /// Review 4705125393: with no compaction snapshot, a report folds at
    /// most the newest `REPORT_HISTORY_EVENTS` intent and registration
    /// events before those it reads, however long the log, and says
    /// `partial` when an event it counts needed an older one. Real SQLite
    /// with more intent events than the window and the budget together:
    /// Alice registered before all of them and prompts inside the window.
    /// Reading every older event would find her workspace and count her;
    /// the bounded fold cannot, and says so.
    #[test]
    fn a_report_on_a_long_history_folds_at_most_the_budget_and_says_partial() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-report-budget-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut store = Store::open(&directory.join("state.sqlite")).unwrap();
        // The product's bounds, lowered below so a short log crosses both.
        assert_eq!(
            (store.today_events, store.report_history_events),
            (TODAY_EVENTS, langspan_core::REPORT_HISTORY_EVENTS)
        );
        store.today_events = 4;
        store.report_history_events = 4;
        let run = |store: &mut Store, id: &str, command: Command| {
            let now = crate::clock::now_ms();
            store.execute(id, &command, &command, now).unwrap();
        };
        run(
            &mut store,
            "w",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/repos/work".into(),
            },
        );
        for session in ["claude-code.alice", "claude-code.filler"] {
            run(
                &mut store,
                &format!("s-{session}"),
                Command::SessionRegister {
                    session_id: session.into(),
                    workspace_id: "work".into(),
                    parent: None,
                    headless: false,
                },
            );
        }
        let generation = store.state.generation;
        // The window holds the prompt and at most the newest 3 of these;
        // the 5 or more older are past the budget of 4, so the fold stops
        // short of Alice's registration.
        let history: Vec<Event> = (0..8)
            .map(|index| {
                if index % 2 == 0 {
                    Event::IntentSet {
                        intent: langspan_core::Intent {
                            session_id: "claude-code.filler".into(),
                            workspace_id: "work".into(),
                            paths: vec!["src/lib.rs".into()],
                            summary: "filler".into(),
                            generation,
                        },
                    }
                } else {
                    Event::IntentEnded {
                        session_id: "claude-code.filler".into(),
                    }
                }
            })
            .collect();
        store.commit(&history, None).unwrap();
        // The window starts now as the report places events: on the
        // daemon's clock through this generation's anchor, which a long test
        // may see drift from the wall clock.
        let anchor = store.state.clock_anchor().unwrap();
        let from_ms = anchor.wall_ms + (crate::clock::now_ms() - anchor.clock_ms);
        run(
            &mut store,
            "p",
            Command::SessionPrompt {
                session_id: "claude-code.alice".into(),
                summary: "carry on".into(),
                line_digests: Vec::new(),
            },
        );
        let outcome = store.report("work", from_ms, from_ms + 60_000).unwrap();
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        let Outcome::Report { report } = outcome else {
            panic!("{outcome:?}");
        };
        // Through JSON, so the check reads what a client is sent.
        let report = serde_json::to_value(&report).unwrap();
        assert_eq!(report["partial"], serde_json::json!(true), "{report:#}");
        let rows = report["sessions"].as_array().unwrap();
        assert!(
            rows.iter()
                .all(|row| row["session_id"] != "claude-code.alice"),
            "{report:#}"
        );
    }

    /// The intent lookup reads only intent events, not the rest of the log.
    #[test]
    fn the_intent_lookup_reads_only_intent_events() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-intent-plan-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let store = Store::open(&directory.join("state.sqlite")).unwrap();
        let plan: Vec<String> = store
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {INTENT_EVENTS_BEFORE}"))
            .unwrap()
            .query_map([0i64, 1_000_000i64, 10i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        assert!(
            plan.iter().any(|step| step.contains("events_report_state")),
            "{plan:?}"
        );
    }

    /// A definition's history (roadmap D4) also runs in the request handler:
    /// its reads go through the partial index of the five kinds it uses,
    /// and the anchors through the generation markers.
    #[test]
    fn history_reads_only_the_events_it_uses() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-history-plan-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let store = Store::open(&directory.join("state.sqlite")).unwrap();
        let plan = |sql: &str, arguments: &[i64]| -> Vec<String> {
            store
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(rusqlite::params_from_iter(arguments), |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let plans = [
            (plan(HISTORY_BEFORE, &[1_000_000, 10]), "events_history"),
            (plan(HISTORY_FOLLOWING, &[1, 10]), "events_history"),
            (plan(ANCHORS, &[]), "events_generation_marker"),
            (plan(MOVES_LOOKUP, &[10]), "events_moves"),
        ];
        let forgetting: Vec<String> = store
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {FORGETTING_LOOKUP}"))
            .unwrap()
            .query_map(["note-1"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let retraction: Vec<String> = store
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {RETRACTION_LOOKUP}"))
            .unwrap()
            .query_map(["message-1"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let plans = plans.into_iter().chain([
            (forgetting, "events_forgotten"),
            (retraction, "events_retracted"),
        ]);
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        for (steps, index) in plans {
            assert!(
                steps.iter().any(|step| step.contains(index)),
                "{index}: {steps:?}"
            );
        }
    }

    /// A forgotten note's text never comes back in a history, even when
    /// paging into old changes puts its forgetting behind more following
    /// events than one answer reads.
    #[test]
    fn paging_into_old_history_never_shows_a_forgotten_note() {
        use langspan_core::{InterfaceChange, InterfaceNotice, Message, Note, Recipient, Stamp};
        let directory = std::env::temp_dir().join(format!(
            "langspan-history-forgotten-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut store = Store::open(&directory.join("state.sqlite")).unwrap();
        let register = Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: directory.to_string_lossy().into_owned(),
        };
        store.execute("w", &register, &register, 0).unwrap();
        let at = Stamp {
            at_ms: 1_000,
            generation: store.state.generation,
        };
        let text = "the retry loop deadlocks under load";
        let kept = "retries are capped at three";
        let remembered = |note_id: &str, text: &str| Event::Remembered {
            note: Note {
                note_id: note_id.into(),
                workspace_id: "work".into(),
                author: None,
                text: text.into(),
                paths: vec!["src/save.rs".into()],
                anchor: None,
                anchor_state: None,
                tags: Vec::new(),
            },
            at: Some(at),
        };
        store
            .commit(
                &[
                    remembered("note-1", text),
                    remembered("note-2", kept),
                    Event::InterfaceChanged {
                        notice: InterfaceNotice {
                            notice_id: "n-1".into(),
                            workspace_id: "work".into(),
                            path: "src/save.rs".into(),
                            changes: vec![InterfaceChange {
                                name: "save".into(),
                                kind: "function".into(),
                                change: "signature_changed".into(),
                                before: Some("fn save()".into()),
                                after: Some("fn save(retries: u32)".into()),
                                before_arity: None,
                                after_arity: None,
                                before_count: None,
                                after_count: None,
                                after_name: None,
                                before_name: None,
                                evidence: None,
                                before_path: None,
                                private: false,
                            }],
                            dependents: Vec::new(),
                            elsewhere: Vec::new(),
                            by_session: None,
                            at_ms: at.at_ms,
                            generation: at.generation,
                        },
                    },
                ],
                None,
            )
            .unwrap();
        let changed: i64 = store
            .connection
            .query_row("SELECT max(seq) FROM events", [], |row| row.get(0))
            .unwrap();
        // More following messages than one answer reads, then the forget.
        let messages: Vec<Event> = (0..HISTORY_EVENTS)
            .map(|index| Event::MessageSent {
                message: Message {
                    message_id: format!("m-{index}"),
                    workspace_id: "work".into(),
                    from: None,
                    to: Recipient::Everyone {},
                    text: "unrelated".into(),
                    paths: Vec::new(),
                    acknowledged: Vec::new(),
                    retracted: false,
                },
                at_ms: Some(at.at_ms),
                generation: Some(at.generation),
            })
            .collect();
        store.commit(&messages, None).unwrap();
        store
            .commit(
                &[Event::Forgotten {
                    workspace_id: "work".into(),
                    note_id: "note-1".into(),
                }],
                None,
            )
            .unwrap();

        let before = u64::try_from(changed).unwrap() + 1;
        let outcome = store
            .history(("work", "save", None), 10, Some(before), at.at_ms)
            .unwrap();
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        let Outcome::History { history } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(history.entries.len(), 1, "{history:?}");
        // The note never forgotten still shows; the forgotten one does not.
        let notes: Vec<&str> = history.entries[0]
            .notes
            .iter()
            .map(|note| note.note_id.as_str())
            .collect();
        assert_eq!(notes, ["note-2"], "{history:?}");
        assert!(
            !serde_json::to_string(&history).unwrap().contains(text),
            "{history:?}"
        );
    }

    /// Roadmap D7: the next page of a moved file's history, asked by its new
    /// path from before the move, still includes what was recorded under
    /// the old path: the move is read whatever the page's cursor.
    #[test]
    fn the_next_page_of_a_moved_files_history_keeps_its_old_path() {
        use langspan_core::{InterfaceChange, InterfaceNotice};
        let directory = std::env::temp_dir().join(format!(
            "langspan-history-moved-page-{}-{}",
            std::process::id(),
            wall_now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut store = Store::open(&directory.join("state.sqlite")).unwrap();
        let register = Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: directory.to_string_lossy().into_owned(),
        };
        store.execute("w", &register, &register, 0).unwrap();
        let generation = store.state.generation;
        let notice = |id: &str, path: &str, change: InterfaceChange| Event::InterfaceChanged {
            notice: InterfaceNotice {
                notice_id: id.into(),
                workspace_id: "work".into(),
                path: path.into(),
                changes: vec![change],
                dependents: Vec::new(),
                elsewhere: Vec::new(),
                by_session: None,
                at_ms: 1_000,
                generation,
            },
        };
        let changed = |after: &str| InterfaceChange {
            name: "save".into(),
            kind: "function".into(),
            change: "signature_changed".into(),
            before: Some("fn save()".into()),
            after: Some(after.into()),
            before_arity: None,
            after_arity: None,
            before_count: None,
            after_count: None,
            after_name: None,
            before_name: None,
            evidence: None,
            before_path: None,
            private: false,
        };
        let moved = InterfaceChange {
            change: "renamed".into(),
            before: Some("fn save(b: u32)".into()),
            after_name: Some("save".into()),
            before_name: Some("save".into()),
            evidence: Some("file_moved".into()),
            before_path: Some("src/a.rs".into()),
            ..changed("fn save(b: u32)")
        };
        store
            .commit(
                &[
                    notice("n-1", "src/a.rs", changed("fn save(a: u32)")),
                    notice("n-2", "src/a.rs", changed("fn save(b: u32)")),
                    notice("n-3", "src/b.rs", moved),
                ],
                None,
            )
            .unwrap();
        let page = |before_seq| match store
            .history(("work", "save", Some("src/b.rs")), 1, before_seq, 1_000)
            .unwrap()
        {
            Outcome::History { history } => history,
            outcome => panic!("{outcome:?}"),
        };
        let mut seen = Vec::new();
        let mut before_seq = None;
        loop {
            let history = page(before_seq);
            seen.extend(history.entries.iter().map(|entry| entry.notice_id.clone()));
            match history.next_seq {
                Some(next) if seen.len() < 10 => before_seq = Some(next),
                _ => break,
            }
        }
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(seen, ["n-3", "n-2", "n-1"]);
    }

    /// A withdrawn message's text never comes back in a history, whether
    /// its withdrawal is within the events one answer reads or behind more
    /// following events than that.
    #[test]
    fn history_never_shows_a_withdrawn_message() {
        use langspan_core::{InterfaceChange, InterfaceNotice, Message, Recipient, Stamp};
        for filler in [0, HISTORY_EVENTS] {
            let directory = std::env::temp_dir().join(format!(
                "langspan-history-retracted-{}-{}-{filler}",
                std::process::id(),
                wall_now_ms()
            ));
            std::fs::create_dir(&directory).unwrap();
            let mut store = Store::open(&directory.join("state.sqlite")).unwrap();
            let register = Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: directory.to_string_lossy().into_owned(),
            };
            store.execute("w", &register, &register, 0).unwrap();
            let at = Stamp {
                at_ms: 1_000,
                generation: store.state.generation,
            };
            let withdrawn = "save now takes the retry count";
            let kept = "callers of save pass three";
            let sent = |message_id: &str, text: &str, paths: Vec<String>| Event::MessageSent {
                message: Message {
                    message_id: message_id.into(),
                    workspace_id: "work".into(),
                    from: None,
                    to: Recipient::Everyone {},
                    text: text.into(),
                    paths,
                    acknowledged: Vec::new(),
                    retracted: false,
                },
                at_ms: Some(at.at_ms),
                generation: Some(at.generation),
            };
            store
                .commit(
                    &[
                        sent("m-withdrawn", withdrawn, vec!["src/save.rs".into()]),
                        sent("m-kept", kept, vec!["src/save.rs".into()]),
                        Event::InterfaceChanged {
                            notice: InterfaceNotice {
                                notice_id: "n-1".into(),
                                workspace_id: "work".into(),
                                path: "src/save.rs".into(),
                                changes: vec![InterfaceChange {
                                    name: "save".into(),
                                    kind: "function".into(),
                                    change: "signature_changed".into(),
                                    before: Some("fn save()".into()),
                                    after: Some("fn save(retries: u32)".into()),
                                    before_arity: None,
                                    after_arity: None,
                                    before_count: None,
                                    after_count: None,
                                    after_name: None,
                                    before_name: None,
                                    evidence: None,
                                    before_path: None,
                                    private: false,
                                }],
                                dependents: Vec::new(),
                                elsewhere: Vec::new(),
                                by_session: None,
                                at_ms: at.at_ms,
                                generation: at.generation,
                            },
                        },
                    ],
                    None,
                )
                .unwrap();
            let changed: i64 = store
                .connection
                .query_row("SELECT max(seq) FROM events", [], |row| row.get(0))
                .unwrap();
            let unrelated: Vec<Event> = (0..filler)
                .map(|index| sent(&format!("m-{index}"), "unrelated", Vec::new()))
                .collect();
            if !unrelated.is_empty() {
                store.commit(&unrelated, None).unwrap();
            }
            store
                .commit(
                    &[Event::MessageRetracted {
                        workspace_id: "work".into(),
                        message_id: "m-withdrawn".into(),
                        by: None,
                    }],
                    None,
                )
                .unwrap();

            let before = u64::try_from(changed).unwrap() + 1;
            let paged = store
                .history(("work", "save", None), 10, Some(before), at.at_ms)
                .unwrap();
            // Unpaged, the change is within the events read only without
            // the filler.
            let newest = (filler == 0).then(|| {
                store
                    .history(("work", "save", None), 10, None, at.at_ms)
                    .unwrap()
            });
            drop(store);
            std::fs::remove_dir_all(&directory).unwrap();
            for outcome in std::iter::once(paged).chain(newest) {
                let Outcome::History { history } = outcome else {
                    panic!("{outcome:?}");
                };
                assert_eq!(history.entries.len(), 1, "{filler}: {history:?}");
                let messages: Vec<&str> = history.entries[0]
                    .messages
                    .iter()
                    .map(|message| message.message_id.as_str())
                    .collect();
                assert_eq!(messages, ["m-kept"], "{filler}: {history:?}");
                assert!(
                    !serde_json::to_string(&history).unwrap().contains(withdrawn),
                    "{filler}: {history:?}"
                );
            }
        }
    }
}
