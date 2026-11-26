//! The daemon's single SQLite owner. The caller holds the lifetime OS lock.
//!
//! Decisions are applied to a private candidate state, persisted with their
//! receipts, and published only after commit. Receipts retain original outcomes;
//! replay validity is calculated from the current recovered state.

use langspan_core::{
    ClockAnchor, Command, DASHBOARD_ITEMS, DENIAL_WINDOW, DenialRecord, Denials, Event,
    LeaseStatus, Outcome, RecordedEvent, ReportQuery, SentPublication, State, StatusRecord,
    TODAY_EVENTS, TodayQuery, TodayWindow, apply, dashboard_denials, decide, expiry_events,
    shrink_status, status_records,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

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

/// A generation marker: a `booted` or `clock_anchored` event. One owner for
/// the index over them and the lookup that must match it to use it.
macro_rules! generation_marker {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         IN ('booted', 'clock_anchored')"
    };
}

/// A partial index of the generation markers, a few rows per daemon start,
/// so the anchor lookup does not read the generation's other events.
const GENERATION_MARKER_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_generation_marker ON events(seq) WHERE ",
    generation_marker!()
);

/// The newest generation marker before a sequence number: the anchor of the
/// generation the event there belongs to. Read through the partial index.
const ANCHOR_LOOKUP: &str = concat!(
    "SELECT event_json FROM events WHERE seq < ?1 AND ",
    generation_marker!(),
    " ORDER BY seq DESC LIMIT 1"
);

/// An event that sets or ends a session's intent.
macro_rules! intent_event {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         IN ('intent_set', 'intent_ended', 'session_ended')"
    };
}

/// A partial index of the intent events, so the report can find which
/// sessions held an intent before the events it reads without reading the
/// rest of the older log.
const INTENT_EVENT_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_intent ON events(seq) WHERE ",
    intent_event!()
);

/// The intent events before a sequence number, oldest first. Read through
/// the partial index.
const INTENT_EVENTS_BEFORE: &str = concat!(
    "SELECT event_json FROM events WHERE seq < ?1 AND ",
    intent_event!(),
    " ORDER BY seq"
);

/// Bytes a status part adds around its text: the record's fields with the
/// largest offset, and the separator.
const PART_OVERHEAD_BYTES: usize = 96;

/// Bytes `text` takes as a JSON string's contents, at most: a quote or
/// backslash is escaped in two, and a control character in at most six.
fn escaped_len(text: &str) -> usize {
    text.chars().map(escaped_char_len).sum()
}

fn escaped_char_len(c: char) -> usize {
    match c {
        '"' | '\\' => 2,
        c if u32::from(c) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// Where a part of `text` that starts at `start` ends, on a character
/// boundary, so that the part fits in `room` bytes of a page.
fn part_end(text: &str, start: usize, room: usize) -> usize {
    let mut left = room.saturating_sub(PART_OVERHEAD_BYTES);
    let mut end = start;
    for c in text[start..].chars() {
        let size = escaped_char_len(c);
        if size > left {
            break;
        }
        left -= size;
        end += c.len_utf8();
    }
    end
}

/// A `workspace list` page within `PAGE_BUDGET_BYTES`: the core bounds it
/// by count, and a root can be 4 KiB, so it may end sooner here, with the
/// cursor moved back to its last workspace. It always keeps one, which a
/// root's bound lets fit.
fn workspace_page(
    mut workspaces: Vec<langspan_core::WorkspaceSummary>,
    mut next: Option<String>,
) -> Outcome {
    let mut bytes = 0usize;
    let keep = workspaces
        .iter()
        .take_while(|workspace| {
            let size = serde_json::to_vec(workspace).map_or(usize::MAX, |json| json.len() + 1);
            let first = bytes == 0;
            bytes = bytes.saturating_add(size);
            first || bytes <= PAGE_BUDGET_BYTES
        })
        .count();
    if keep < workspaces.len() {
        workspaces.truncate(keep);
        next = workspaces.last().map(|last| last.workspace_id.clone());
    }
    Outcome::Workspaces { workspaces, next }
}

/// Identifies one record's JSON across pages, so that a page resumes inside
/// a record only if the record has not changed. Not a security control: a
/// mismatch (or a different build) starts the record again from its start.
fn digest(text: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
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
    tap: Option<Tap>,
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
    if let Event::CheckRecorded { check } = event {
        crate::hook::redact_check(check);
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
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !(0..=3).contains(&version) {
            return Err(
                format!("unsupported SQLite schema version {version}; expected at most 3").into(),
            );
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

        let state = Self::recover(&connection)?;
        let generation = state
            .generation
            .checked_add(1)
            .ok_or("authority generation exhausted; refusing to reuse a generation")?;
        let mut store = Self {
            connection,
            state,
            tap: None,
        };
        // The generation's clock against the wall clock, so times can be
        // placed on a day (the status line and `today`).
        let anchor = ClockAnchor {
            generation,
            clock_ms: crate::clock::now_ms(),
            wall_ms: wall_now_ms(),
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

    /// Persist a complete read-only Git scan and its derived facts atomically.
    pub fn record_scan(
        &mut self,
        workspace_id: &str,
        snapshot: &crate::observer::Snapshot,
        now_ms: u64,
    ) -> StoreResult<usize> {
        let workspace = self
            .state
            .workspaces
            .get(workspace_id)
            .ok_or("cannot store a scan for an unregistered workspace")?;
        if workspace.root != snapshot.root {
            return Err("scan root differs from the registered physical workspace".into());
        }
        let limits = crate::observer::Limits::default();
        if snapshot.files.len() > limits.max_files || snapshot.branches.len() > limits.max_branches
        {
            return Err("complete scan exceeds the persistence bounds".into());
        }
        for (path, version) in &snapshot.files {
            let state = observed_file_state(Some(version))?;
            langspan_core::validate_observed_file(path, &state)?;
        }
        let branches = observed_branches(snapshot);
        if branches.len() != snapshot.branches.len() + 1 {
            return Err("Git snapshot contains duplicate branch identities".into());
        }
        for (name, (head_oid, tree_oid)) in &branches {
            let command = Command::ObserveBranch {
                workspace_id: workspace_id.into(),
                branch_ref: name.clone(),
                head_oid: head_oid.clone(),
                tree_oid: tree_oid.clone(),
            };
            if let Outcome::Error { code, message } =
                decide(&self.state, &command, "scan-validation", now_ms).outcome
            {
                return Err(format!("invalid complete Git snapshot: {code}: {message}").into());
            }
        }
        let snapshot_json = serde_json::to_string(snapshot)?;
        if snapshot_json.len() > 4 * 1024 * 1024 {
            return Err("serialized Git snapshot exceeds 4 MiB".into());
        }
        let previous_json: Option<String> = self
            .connection
            .query_row(
                "SELECT snapshot_json FROM git_snapshots WHERE workspace_id=?1",
                [workspace_id],
                |row| row.get(0),
            )
            .optional()?;
        let previous: Option<crate::observer::Snapshot> = previous_json
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        if previous
            .as_ref()
            .is_some_and(|old| old.root != snapshot.root || old.common_dir != snapshot.common_dir)
        {
            return Err("Git identity changed since the previous scan; register the replacement as a new workspace".into());
        }
        let changes = crate::observer::diff(&previous, snapshot);
        let sequence: i64 =
            self.connection
                .query_row("SELECT coalesce(max(seq), 0) FROM events", [], |row| {
                    row.get(0)
                })?;
        let scan_id = format!("scan-{}-{sequence}", self.state.generation);
        let mut commands = Vec::new();
        // Files whose content changed since a complete earlier scan, at their
        // new versions: the core keeps those no session reported (issue #129).
        let mut content_changes = Vec::new();
        // Several changed files back at their committed version at once,
        // with HEAD where it was: a reset, stash or checkout of files.
        let restored = changes
            .iter()
            .filter(|change| {
                change.before.as_ref().is_some_and(|before| before.dirty)
                    && change.after.as_ref().is_some_and(|after| !after.dirty)
            })
            .count();
        let file_changes = u32::try_from(changes.len()).unwrap_or(u32::MAX);
        // A changed file now at a version a session of this workspace wrote
        // or read is explained by that session, not by the checkout: an edit
        // no scan saw before it was committed moves HEAD "with the files".
        let known = |path: &str, after: &langspan_core::FileState| {
            self.state.activity.iter().any(|(session, activity)| {
                self.state
                    .sessions
                    .get(session)
                    .is_some_and(|s| s.workspace_id == workspace_id)
                    && activity
                        .known
                        .iter()
                        .any(|version| version.path == path && version.state == *after)
            })
        };
        let unexplained = changes
            .iter()
            .filter(|change| {
                observed_file_state(change.after.as_ref())
                    .map_or(true, |after| !known(&change.path, &after))
            })
            .count();
        for change in changes {
            // A new path absent from an existing complete baseline was missing.
            // No previous baseline, or a previous scan that did not version
            // the path, is deliberately recorded as unknown instead.
            let before = if previous.is_some() && !change.before_unknown {
                Some(observed_file_state(change.before.as_ref())?)
            } else {
                None
            };
            let after = observed_file_state(change.after.as_ref())?;
            if before.as_ref().is_some_and(|before| *before != after)
                && content_changes.len() < langspan_core::MAX_UNCOORDINATED_OBSERVED
            {
                content_changes.push(langspan_core::ObservedFile {
                    path: change.path.clone(),
                    state: after.clone(),
                });
            }
            commands.push(Command::ObserveDisk {
                workspace_id: workspace_id.into(),
                path: change.path,
                before,
                after,
                related_operation_ids: Vec::new(),
            });
        }
        let previous_branches = previous.as_ref().map(observed_branches).unwrap_or_default();
        let names: std::collections::BTreeSet<_> =
            branches.keys().chain(previous_branches.keys()).collect();
        for name in names {
            let current = branches.get(name);
            let changed = current != previous_branches.get(name)
                || name == "HEAD"
                    && previous
                        .as_ref()
                        .is_some_and(|old| old.branch != snapshot.branch);
            if changed {
                let (head_oid, tree_oid) = current.cloned().unwrap_or_default();
                commands.push(Command::ObserveBranch {
                    workspace_id: workspace_id.into(),
                    branch_ref: name.clone(),
                    head_oid,
                    tree_oid,
                });
            }
        }
        let facts = commands.len();
        let mut checkout_changed = false;
        // The checkout changed under the sessions in it: another branch, a
        // HEAD that moved with files no session put there, or a bulk
        // restore. A commit moves HEAD without changing any file, or with
        // only files at versions their sessions wrote, and is not one.
        if let Some(old) = &previous {
            let head_moved = old.head != snapshot.head;
            if old.branch != snapshot.branch
                || head_moved && unexplained > 0
                || !head_moved && restored >= CHECKOUT_RESTORED_FILES
            {
                let branch =
                    |name: &Option<String>| name.as_ref().map(|name| format!("refs/heads/{name}"));
                commands.push(Command::ObserveCheckout {
                    workspace_id: workspace_id.into(),
                    from_branch: branch(&old.branch),
                    to_branch: branch(&snapshot.branch),
                    from_head: old.head.clone(),
                    to_head: snapshot.head.clone(),
                    files: file_changes,
                });
                checkout_changed = true;
            }
        }
        // Changed files no checkout change accounts for. Which of them no
        // session reported, and whom to name, is the core's decision.
        if !checkout_changed && !content_changes.is_empty() {
            commands.push(Command::ObserveUncoordinated {
                workspace_id: workspace_id.into(),
                files: content_changes,
            });
        }
        // Worktrees of one repository share a Git common directory. Only its
        // digest is recorded; it groups workspaces without merging them.
        commands.push(Command::ObserveWorkspaceGit {
            workspace_id: workspace_id.into(),
            repository_id: format!(
                "local-{}",
                &crate::sha256::hex(&crate::sha256::digest(snapshot.common_dir.as_bytes()))[..32]
            ),
            branch_ref: snapshot
                .branch
                .as_ref()
                .map(|name| format!("refs/heads/{name}")),
        });
        let mut events = Vec::new();
        // The returned count is of disk and branch facts, not the grouping record.
        let mut observed = 0;
        for (index, command) in commands.iter().enumerate() {
            let id = format!("{scan_id}-{index}");
            let decision = decide(&self.state, command, &id, now_ms);
            if let Outcome::Error { code, message } = decision.outcome {
                return Err(format!("invalid Git observation: {code}: {message}").into());
            }
            events.extend(decision.events);
            if index + 1 == facts {
                observed = events.len();
            }
        }
        let summary = langspan_core::ScanSummary {
            workspace_id: workspace_id.into(),
            observed_at_ms: now_ms,
            generation: self.state.generation,
            file_count: snapshot.files.len() as u64,
            branch_count: snapshot.branches.len() as u64,
            head_oid: snapshot.head.clone(),
            branch_ref: snapshot
                .branch
                .as_ref()
                .map(|name| format!("refs/heads/{name}")),
            coverage: (snapshot.coverage != langspan_core::ScanCoverage::default())
                .then(|| snapshot.coverage.clone()),
        };
        self.commit_with_snapshot(
            &events,
            None,
            Some((
                workspace_id,
                &snapshot_json,
                &serde_json::to_string(&summary)?,
            )),
        )?;
        Ok(observed)
    }

    pub fn replay(
        &mut self,
        request_id: &str,
        original: &Command,
        now_ms: u64,
    ) -> StoreResult<Option<Execution>> {
        self.refresh(now_ms)?;
        if matches!(
            original,
            Command::Status
                | Command::StatusRecords { .. }
                | Command::Events { .. }
                | Command::Changes { .. }
                | Command::RemoteOutbox { .. }
                | Command::RemoteSent { .. }
                | Command::ShowMessage { .. }
                | Command::CheckStatus { .. }
                | Command::FlakyTests { .. }
                | Command::LandingReadiness { .. }
                | Command::TurnQueue { .. }
                | Command::Recap { .. }
                | Command::Recall { .. }
                | Command::Today { .. }
                | Command::Report { .. }
                | Command::StatusLine { .. }
                | Command::Dashboard
                | Command::WorkspaceList { .. }
                | Command::WorkspaceRemove { apply: false, .. }
        ) {
            return Ok(None);
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
        crate::hook::redact_command(&mut recorded);
        let mut retry = original.clone();
        crate::hook::redact_command(&mut retry);
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
            crate::hook::redact_check(check);
        }
        Ok(Some(self.execution(outcome, true, now_ms)))
    }

    pub fn execute(
        &mut self,
        request_id: &str,
        original: &Command,
        prepared: &Command,
        now_ms: u64,
    ) -> StoreResult<Execution> {
        // Refused before a receipt or an event could hold it.
        if let Some(refused) = crate::hook::refuse_secret(original) {
            return Ok(self.execution(refused, false, now_ms));
        }
        if let Some(replayed) = self.replay(request_id, original, now_ms)? {
            return Ok(replayed);
        }
        let outcome = match original {
            // What matters now, in one frame however long the history.
            Command::Status => match decide(&self.state, prepared, request_id, now_ms).outcome {
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
                outcome => outcome,
            },
            Command::StatusRecords { after, page_bytes } => self.status_records(
                after.as_deref(),
                page_bytes.map_or(PAGE_BUDGET_BYTES, |bytes| {
                    usize::try_from(bytes)
                        .unwrap_or(PAGE_BUDGET_BYTES)
                        .clamp(MIN_PAGE_BYTES, PAGE_BUDGET_BYTES)
                }),
            ),
            // A read: decided by the core, never recorded.
            Command::RemoteOutbox { .. }
            | Command::CheckStatus { .. }
            | Command::FlakyTests { .. }
            | Command::LandingReadiness { .. }
            | Command::TurnQueue { .. }
            | Command::StatusLine { .. }
            | Command::WorkspaceList { .. }
            | Command::WorkspaceRemove { apply: false, .. }
            | Command::Recap { .. }
            | Command::Recall { .. } => {
                match decide(&self.state, prepared, request_id, now_ms).outcome {
                    Outcome::Workspaces { workspaces, next } => workspace_page(workspaces, next),
                    // A recap or a recall must fit one frame with room for
                    // the envelope.
                    Outcome::Recap { mut recap } => {
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
            Command::Dashboard => {
                let mut outcome = decide(&self.state, prepared, request_id, now_ms).outcome;
                if let Outcome::Dashboard { dashboard } = &mut outcome {
                    dashboard.denials = Some(self.denials(now_ms)?);
                }
                outcome
            }
            Command::Events { after, limit } => self.events(*after, *limit)?,
            Command::Today {
                workspace_id,
                from_ms,
            } => self.today(workspace_id.as_deref(), *from_ms, wall_now_ms(), now_ms)?,
            Command::Report {
                workspace_id,
                from_ms,
            } => self.report(workspace_id, *from_ms, wall_now_ms())?,
            Command::RemoteSent {
                workspace_id,
                limit,
            } => self.sent(workspace_id, *limit)?,
            Command::ShowMessage {
                workspace_id,
                message_id,
            } => self.show_message(workspace_id, message_id)?,
            Command::Changes {
                workspace_id,
                after,
                limit,
            } => self.changes(workspace_id, *after, *limit)?,
            // A hook run inside the interval writes nothing, not even a
            // receipt, so a burst of them costs no storage (roadmap J12).
            Command::SessionSeen { .. } => {
                let decision = decide(&self.state, prepared, request_id, now_ms);
                if !decision.events.is_empty() {
                    self.commit(
                        &decision.events,
                        Some((request_id, original, &decision.outcome)),
                    )?;
                }
                decision.outcome
            }
            _ => {
                let decision = decide(&self.state, prepared, request_id, now_ms);
                self.commit(
                    &decision.events,
                    Some((request_id, original, &decision.outcome)),
                )?;
                decision.outcome
            }
        };
        Ok(self.execution(outcome, false, now_ms))
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

    /// Recent refusals for the dashboard. Conflicts are receipts, not
    /// events, and carry no time; stale-source refusals are the events that
    /// recorded what a refused session was told. Only the newest
    /// `DENIAL_WINDOW` rows of each table are searched, so the cost of a read
    /// does not grow with history.
    fn denials(&self, now_ms: u64) -> StoreResult<Denials> {
        let page = i64::try_from(DASHBOARD_ITEMS / 2 + 1)?;
        let window = i64::from(DENIAL_WINDOW);
        let mut statement = self.connection.prepare(
            "SELECT command_json, outcome_json FROM receipts
             WHERE rowid > (SELECT coalesce(max(rowid), 0) FROM receipts) - ?1
               AND json_extract(outcome_json, '$.type') = 'conflict'
             ORDER BY rowid DESC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![window, page])?;
        let mut conflicts = Vec::new();
        while let Some(row) = rows.next()? {
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

    /// One page of state records from `after`, within `budget` bytes
    /// (`PAGE_BUDGET_BYTES` unless the reader asked for less, never under
    /// `MIN_PAGE_BYTES`; `workspace_page` bounds a `workspace list` page by
    /// `PAGE_BUDGET_BYTES`). A record that fits a page is never split; one
    /// that does not (an entry at its limits can be larger than a frame) is
    /// given as parts of its JSON, so that every valid state can be read. A
    /// page holds at least one record or part when any remain.
    fn status_records(&self, after: Option<&str>, budget: usize) -> Outcome {
        let (mut resume, records) = match status_records(&self.state, after) {
            Ok(read) => read,
            Err(error) => {
                return Outcome::Error {
                    code: error.code,
                    message: error.message,
                };
            }
        };
        let mut page = Vec::new();
        let mut bytes = 0;
        let mut next = None;
        for (position, record) in records {
            let text = match serde_json::to_string(&record) {
                Ok(text) => text,
                Err(error) => {
                    return Outcome::Error {
                        code: "internal".into(),
                        message: format!("a state record could not be encoded: {error}"),
                    };
                }
            };
            let digest = digest(&text);
            // The rest of a record the last page began, if it is unchanged.
            let start = match resume.take() {
                Some((at, point))
                    if at == position
                        && point.digest == digest
                        && point.offset < text.len()
                        && text.is_char_boundary(point.offset) =>
                {
                    point.offset
                }
                _ => 0,
            };
            if start == 0 && text.len() < budget {
                if bytes + text.len() + 1 > budget {
                    return Outcome::StatusRecords {
                        records: page,
                        next,
                    };
                }
                bytes += text.len() + 1;
                page.push(record);
                next = Some(position.after());
                continue;
            }
            let end = part_end(&text, start, budget.saturating_sub(bytes));
            if end == start {
                return Outcome::StatusRecords {
                    records: page,
                    next,
                };
            }
            let last = end == text.len();
            page.push(StatusRecord::Part {
                text: text[start..end].into(),
                offset: start as u64,
                last,
            });
            if !last {
                return Outcome::StatusRecords {
                    records: page,
                    next: Some(position.within(end, digest)),
                };
            }
            bytes += PART_OVERHEAD_BYTES + escaped_len(&text[start..end]);
            next = Some(position.after());
        }
        Outcome::StatusRecords {
            records: page,
            next: None,
        }
    }

    fn events(&self, after: u64, limit: u32) -> StoreResult<Outcome> {
        if !(1..=1000).contains(&limit) {
            return Ok(Outcome::Error {
                code: "invalid_limit".into(),
                message: "event limit must be between 1 and 1000".into(),
            });
        }
        let after = i64::try_from(after).unwrap_or(i64::MAX);
        let mut statement = self
            .connection
            .prepare("SELECT seq, event_json FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
        let mut rows = statement.query(params![after, i64::from(limit)])?;
        let mut events = Vec::new();
        let mut page_bytes = 0;
        while let Some(row) = rows.next()? {
            let sequence: i64 = row.get(0)?;
            let stored: String = row.get(1)?;
            let (event, json) = shown_event(serde_json::from_str(&stored)?, stored)?;
            if page_bytes + json.len() + 64 > 48 * 1024 {
                if events.is_empty() {
                    return Err("one event exceeds the transport page budget".into());
                }
                break;
            }
            page_bytes += json.len() + 64;
            events.push(RecordedEvent {
                seq: u64::try_from(sequence)?,
                event,
            });
        }
        Ok(Outcome::Events { events })
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
        let (events, anchor, earlier) = self.newest_events()?;
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

    /// The newest `TODAY_EVENTS` events, oldest first, with the anchor of
    /// the generation the first belongs to (when it was anchored) and
    /// whether older events exist. `today` and `report` read the same.
    fn newest_events(&self) -> StoreResult<(Vec<RecordedEvent>, Option<ClockAnchor>, bool)> {
        let mut statement = self
            .connection
            .prepare("SELECT seq, event_json FROM events ORDER BY seq DESC LIMIT ?1")?;
        let mut rows = statement.query([i64::try_from(TODAY_EVENTS)?])?;
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
        let earlier: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE seq < ?1)",
            [first],
            |row| row.get(0),
        )?;
        // The generation the first event read belongs to, if it was anchored.
        let before: Option<String> = self
            .connection
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

    /// The sessions holding an intent just before sequence number `seq`,
    /// from the intent events before it.
    fn open_intents_before(&self, seq: u64) -> StoreResult<BTreeSet<String>> {
        let mut statement = self.connection.prepare(INTENT_EVENTS_BEFORE)?;
        let mut rows = statement.query([i64::try_from(seq)?])?;
        let mut open = BTreeSet::new();
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            langspan_core::open_intents(&mut open, &serde_json::from_str(&json)?);
        }
        Ok(open)
    }

    /// The coordination report (roadmap H8): the newest `TODAY_EVENTS`
    /// events at most, placed in wall time as `today` places them, counted
    /// by the core. Reads only; it writes nothing and acknowledges no
    /// message.
    pub fn report(&self, workspace_id: &str, from_ms: u64, wall_ms: u64) -> StoreResult<Outcome> {
        if let Some(refused) = self.refused_window(Some(workspace_id), from_ms, wall_ms) {
            return Ok(refused);
        }
        let (events, anchor, earlier) = self.newest_events()?;
        let open_intents = self.open_intents_before(events.first().map_or(0, |first| first.seq))?;
        let mut report = langspan_core::report(
            &self.state,
            &ReportQuery {
                workspace_id,
                window: TodayWindow {
                    from_ms,
                    to_ms: wall_ms,
                },
                events: &events,
                anchor,
                earlier,
                open_intents: &open_intents,
            },
        );
        report.shrink_until(|report| {
            serde_json::to_vec(report).is_ok_and(|bytes| bytes.len() <= PAGE_BUDGET_BYTES)
        });
        Ok(Outcome::Report { report })
    }

    /// The outbound audit: what left for a workspace, newest first, read
    /// from the `remote_published` events. Each payload is at most 16 KiB,
    /// which JSON escaping can at most double, so one record always fits a
    /// frame; the answer stops before 48 KiB and says whether it did.
    fn sent(&self, workspace_id: &str, limit: u32) -> StoreResult<Outcome> {
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
        Ok(Outcome::RemoteSent { sent, more })
    }

    /// One message as it was sent, read from its `message_sent` event, so
    /// it is there after every recipient has acknowledged it and the
    /// projection has dropped it, and marked retracted when a later
    /// `message_retracted` event withdrew it. Read-only and never recorded. Messages
    /// are rare, so the scan reads few rows past the cheap prefix test; a
    /// sent message is at most 400 bytes of text and 8 paths, so the answer
    /// fits a frame.
    fn show_message(&self, workspace_id: &str, message_id: &str) -> StoreResult<Outcome> {
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
        let stored: Option<(i64, String)> = self
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
            .optional()?;
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
        let Event::MessageSent { mut message } = serde_json::from_str(&stored)? else {
            return Err("a message_sent row holds another event".into());
        };
        // A withdrawal is its own later event; the sent one never changes.
        message.retracted = self.connection.query_row(
            r#"SELECT EXISTS (SELECT 1 FROM events
                 WHERE seq > ?1
                   AND event_json LIKE '{"type":"message_retracted",%'
                   AND json_valid(event_json)
                   AND json_extract(event_json, '$.type') = 'message_retracted'
                   AND json_extract(event_json, '$.workspace_id') = ?2
                   AND json_extract(event_json, '$.message_id') = ?3)"#,
            params![sent_seq, workspace_id, message_id],
            |row| row.get(0),
        )?;
        Ok(Outcome::MessageShown {
            message: message.into(),
        })
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

    fn changes(&self, workspace_id: &str, after: u64, limit: u32) -> StoreResult<Outcome> {
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
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for event in events {
            transaction.execute(
                "INSERT INTO events(event_json) VALUES (?1)",
                [serde_json::to_string(event)?],
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
                "INSERT INTO receipts(request_id, command_json, outcome_json) VALUES (?1, ?2, ?3)",
                params![
                    request_id,
                    serde_json::to_string(command)?,
                    serde_json::to_string(outcome)?,
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

    fn recover(connection: &Connection) -> StoreResult<State> {
        let mut state = State::default();
        let mut statement =
            connection.prepare("SELECT seq, event_json FROM events ORDER BY seq")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let sequence: i64 = row.get(0)?;
            if sequence <= 0 {
                return Err("event history contains a non-positive sequence".into());
            }
            let json: String = row.get(1)?;
            let mut event: Event = serde_json::from_str(&json).map_err(|error| {
                format!("invalid persisted event at sequence {sequence}: {error}")
            })?;
            redact_event(&mut event);
            apply(&mut state, &event).map_err(|error| {
                format!("invalid persisted transition at sequence {sequence}: {error}")
            })?;
        }

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
        Ok(state)
    }
}

/// Changed files that went back to their committed version in one scan,
/// with HEAD unmoved, before it counts as the checkout changing. One file is
/// as likely an edit undone by hand.
const CHECKOUT_RESTORED_FILES: usize = 2;

fn observed_file_state(
    version: Option<&crate::observer::FileVersion>,
) -> StoreResult<langspan_core::FileState> {
    use crate::observer::FileKind;
    match version {
        None
        | Some(crate::observer::FileVersion {
            kind: FileKind::Missing,
            digest: None,
            ..
        }) => Ok(langspan_core::FileState::Missing),
        Some(crate::observer::FileVersion {
            kind,
            digest: Some(digest),
            ..
        }) if *kind != FileKind::Missing => Ok(langspan_core::FileState::Present {
            digest: digest.clone(),
            kind: if *kind == FileKind::Symlink {
                langspan_core::FileKind::Symlink
            } else {
                langspan_core::FileKind::Regular
            },
        }),
        _ => Err("inconsistent file kind and digest in complete Git snapshot".into()),
    }
}

fn observed_branches(
    snapshot: &crate::observer::Snapshot,
) -> std::collections::BTreeMap<String, (Option<String>, Option<String>)> {
    let mut branches = std::collections::BTreeMap::from([(
        "HEAD".into(),
        (snapshot.head.clone(), snapshot.head_tree.clone()),
    )]);
    for branch in &snapshot.branches {
        branches.insert(
            format!("refs/heads/{}", branch.name),
            (Some(branch.commit.clone()), Some(branch.tree.clone())),
        );
    }
    branches
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Review 4703461855: an intent set before the newest `TODAY_EVENTS`
    /// events, whose session ends inside the window, is counted as left
    /// open. Real SQLite: the intent is only in the older part of the log.
    #[test]
    fn an_intent_set_before_the_events_read_is_left_open_when_its_session_ends() {
        let directory = std::env::temp_dir().join(format!(
            "langspan-report-intent-{}-{}",
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
        assert_eq!(alice.counts.intents_left_open, 1, "{report:#?}");
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
            .query_map([1_000_000i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(store);
        std::fs::remove_dir_all(&directory).unwrap();
        assert!(
            plan.iter().any(|step| step.contains("events_intent")),
            "{plan:?}"
        );
    }
}
