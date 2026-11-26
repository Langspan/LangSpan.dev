//! The daemon's single SQLite owner. The caller holds the lifetime OS lock.
//!
//! Decisions are applied to a private candidate state, persisted with their
//! receipts, and published only after commit. Receipts retain original outcomes;
//! replay validity is calculated from the current recovered state.

use langspan_core::{
    ClockAnchor, Command, DASHBOARD_ITEMS, DENIAL_WINDOW, DenialRecord, Denials, Event,
    HISTORY_EVENTS, HistoryQuery, LeaseStatus, Outcome, RecordedEvent, SentPublication, State,
    StatusRecord, TODAY_EVENTS, TodayQuery, TodayWindow, apply, dashboard_denials, decide,
    definition_history, expiry_events, shrink_status, status_records,
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

/// The kinds of event a definition's history reads (roadmap D4). One owner
/// for the partial index over them and the lookups that must match it.
macro_rules! history_kinds {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         IN ('interface_changed', 'session_prompted', 'remembered', 'forgotten', 'message_sent')"
    };
}

/// A partial index of those events, so a history reads them and not the
/// rest of the log.
const HISTORY_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_history ON events(seq) WHERE ",
    history_kinds!()
);

/// The newest of those events before a sequence number.
const HISTORY_BEFORE: &str = concat!(
    "SELECT seq, event_json FROM events WHERE seq < ?1 AND ",
    history_kinds!(),
    " ORDER BY seq DESC LIMIT ?2"
);

/// Notes, forgettings and messages from a sequence number on, oldest first:
/// those that follow a page's changes.
const HISTORY_FOLLOWING: &str = concat!(
    "SELECT seq, event_json FROM events WHERE seq >= ?1 AND ",
    history_kinds!(),
    " AND json_extract(event_json, '$.type') IN ('remembered', 'forgotten', 'message_sent')
     ORDER BY seq LIMIT ?2"
);

/// The forgettings, one row per forgotten note.
macro_rules! forgotten_kind {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END = 'forgotten'"
    };
}

/// A partial index of the forgettings by note, so a history finds whether a
/// note it would show was forgotten however far after it that happened.
const FORGOTTEN_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_forgotten ON events(json_extract(event_json, '$.note_id')) WHERE ",
    forgotten_kind!()
);

/// The forgettings of one note.
const FORGETTING_LOOKUP: &str = concat!(
    "SELECT seq, event_json FROM events WHERE ",
    forgotten_kind!(),
    " AND json_extract(event_json, '$.note_id') = ?1"
);

/// The withdrawals, one row per withdrawn message.
macro_rules! retracted_kind {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END = 'message_retracted'"
    };
}

/// A partial index of the withdrawals by message, so a history finds
/// whether a message it would show was withdrawn however far after it that
/// happened.
const RETRACTED_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_retracted ON events(json_extract(event_json, '$.message_id')) WHERE ",
    retracted_kind!()
);

/// The withdrawals of one message.
const RETRACTION_LOOKUP: &str = concat!(
    "SELECT seq, event_json FROM events WHERE ",
    retracted_kind!(),
    " AND json_extract(event_json, '$.message_id') = ?1"
);

/// Every generation's clock anchor.
const ANCHORS: &str = concat!(
    "SELECT event_json FROM events WHERE ",
    generation_marker!(),
    " ORDER BY seq"
);

/// A history's answer stops before this many bytes: the MCP result carries
/// it twice, once escaped again as text, inside one 64 KiB frame.
pub const HISTORY_BUDGET_BYTES: usize = 28 * 1024;

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
    if let Event::CheckRecorded { check, .. } = event {
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
        connection.execute_batch(HISTORY_INDEX)?;
        connection.execute_batch(FORGOTTEN_INDEX)?;
        connection.execute_batch(RETRACTED_INDEX)?;

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
        if workspace.root != snapshot.root
            && !crate::observer::same_root(
                std::path::Path::new(&workspace.root),
                std::path::Path::new(&snapshot.root),
            )
        {
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
        let mut previous: Option<crate::observer::Snapshot> = previous_json
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        if previous.as_ref().is_some_and(|old| {
            if old.root == snapshot.root && old.common_dir == snapshot.common_dir {
                return false;
            }
            // Reached through a link left where the checkout used to be,
            // both paths now name the same directories. A link to any other
            // checkout would too, so the checkout reached must also hold the
            // commit the previous scan saw. One bounded Git call, once per
            // change of path: the next scan compares equal paths.
            let same = |before: &str, now: &str| {
                crate::observer::same_root(std::path::Path::new(before), std::path::Path::new(now))
            };
            !(same(&old.root, &snapshot.root)
                && same(&old.common_dir, &snapshot.common_dir)
                && old.head.as_deref().is_some_and(|head| {
                    crate::observer::holds_commit(
                        std::path::Path::new(&snapshot.root),
                        head,
                        std::time::Instant::now() + std::time::Duration::from_millis(500),
                    )
                    .unwrap_or(false)
                }))
        }) {
            return Err(IDENTITY_CHANGED.into());
        }
        // Accepted as the same checkout: the previous scan stays the
        // baseline under the paths it is reached by now, so a change made
        // while it moved is still observed with its earlier version.
        if let Some(old) = previous.as_mut() {
            old.root.clone_from(&snapshot.root);
            old.common_dir.clone_from(&snapshot.common_dir);
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
                | Command::MessageStatus { .. }
                | Command::CheckStatus { .. }
                | Command::FlakyTests { .. }
                | Command::LandingReadiness { .. }
                | Command::TurnQueue { .. }
                | Command::Recap { .. }
                | Command::Recall { .. }
                | Command::Today { .. }
                | Command::History { .. }
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
        if let Some(workspace_id) = workspace_id
            && !self.state.workspaces.contains_key(workspace_id)
        {
            return Ok(Outcome::Error {
                code: "unknown_workspace".into(),
                message: "no workspace has this ID; `langspan status` lists them".into(),
            });
        }
        if from_ms > wall_ms {
            return Ok(Outcome::Error {
                code: "invalid_window".into(),
                message: "the window starts after now; give a --since time in the past".into(),
            });
        }
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
    fn message_status(
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
                            }],
                            dependents: Vec::new(),
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
                                }],
                                dependents: Vec::new(),
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
