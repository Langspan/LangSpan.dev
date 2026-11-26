//! Retention for the event log (ADR 0025, J7).
//!
//! A compaction writes the derived state at sequence N into `snapshots` and
//! deletes, in the same transaction, the events at or below N and the
//! receipts recorded before the window. The store then starts from the
//! newest snapshot and replays only the events after it. The snapshot is
//! checked first: the snapshot read back from its JSON, with the events
//! after N applied, must equal the state the daemon holds, or nothing is
//! deleted.

use super::{Store, StoreResult, redact_event};
use langspan_core::{
    ClockAnchor, CompactionReport, Event, KeptTests, LeaseStatus, MAX_RETENTION_DAYS,
    MIN_RETENTION_DAYS, Outcome, PathRefusal, RecentRefusal, Rename, State, WriteRun, apply,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

const DAY_MS: u64 = 86_400_000;

/// A snapshot as stored: the state, and the derived fields the state leaves
/// out of every reply (`#[serde(skip)]`), which a start from the snapshot
/// must still have. A field skipped in the state and missing here makes the
/// snapshot check refuse the compaction.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRecord {
    state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clock_anchor: Option<ClockAnchor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refusals: Vec<RecentRefusal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    path_refusals: Vec<PathRefusal>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspace_revisions: BTreeMap<String, u32>,
    /// The test results kept per workspace and check, for classifying a
    /// later run's failures (ADR 0030).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    kept_tests: BTreeMap<String, BTreeMap<String, KeptTests>>,
    /// Each session's write count before its first write (ADR 0030).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    first_writes: BTreeMap<String, u64>,
    /// Each workspace's runs of consecutive writes (ADR 0030).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    write_runs: BTreeMap<String, Vec<WriteRun>>,
    /// Each workspace's remembered renames (roadmap D7).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    renames: BTreeMap<String, Vec<Rename>>,
}

impl SnapshotRecord {
    fn of(state: &State) -> Self {
        Self {
            state: state.clone(),
            clock_anchor: state.clock_anchor,
            refusals: state.refusals.clone(),
            path_refusals: state.path_refusals.clone(),
            workspace_revisions: state.workspace_revisions.clone(),
            kept_tests: state.kept_tests.clone(),
            first_writes: state.first_writes.clone(),
            write_runs: state.write_runs.clone(),
            renames: state.renames.clone(),
        }
    }
    fn into_state(self) -> State {
        let mut state = self.state;
        state.clock_anchor = self.clock_anchor;
        state.refusals = self.refusals;
        state.path_refusals = self.path_refusals;
        state.workspace_revisions = self.workspace_revisions;
        state.kept_tests = self.kept_tests;
        state.first_writes = self.first_writes;
        state.write_runs = self.write_runs;
        state.renames = self.renames;
        state
    }
}

/// The newest snapshot and its sequence, or the empty state at zero.
pub(super) fn snapshot(connection: &Connection) -> StoreResult<(State, u64)> {
    let newest: Option<(i64, String)> = connection
        .query_row(
            "SELECT seq, state_json FROM snapshots ORDER BY seq DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((sequence, json)) = newest else {
        return Ok((State::default(), 0));
    };
    let record: SnapshotRecord = serde_json::from_str(&json).map_err(|error| {
        format!("the state snapshot at event {sequence} cannot be read by this build: {error}")
    })?;
    Ok((record.into_state(), u64::try_from(sequence)?))
}

/// Apply the events after `from` to `state` in order, calling `before` with
/// each event's sequence and the state just before that event is applied.
pub(super) fn replay_after(
    connection: &Connection,
    state: &mut State,
    from: u64,
    mut before: impl FnMut(u64, &mut State) -> StoreResult<()>,
) -> StoreResult<()> {
    let mut statement =
        connection.prepare("SELECT seq, event_json FROM events WHERE seq > ?1 ORDER BY seq")?;
    let mut rows = statement.query([i64::try_from(from)?])?;
    while let Some(row) = rows.next()? {
        let sequence: i64 = row.get(0)?;
        if sequence <= 0 {
            return Err("event history contains a non-positive sequence".into());
        }
        let json: String = row.get(1)?;
        let mut event: Event = serde_json::from_str(&json)
            .map_err(|error| format!("invalid persisted event at sequence {sequence}: {error}"))?;
        redact_event(&mut event);
        before(u64::try_from(sequence)?, state)?;
        apply(state, &event).map_err(|error| {
            format!("invalid persisted transition at sequence {sequence}: {error}")
        })?;
    }
    Ok(())
}

/// The wall-clock time from which every publication is still listed, or
/// `None` when no event was ever removed.
pub(super) fn since_ms(connection: &Connection) -> StoreResult<Option<u64>> {
    let since: Option<i64> =
        connection.query_row("SELECT max(cutoff_ms) FROM snapshots", [], |row| row.get(0))?;
    Ok(since
        .and_then(|ms| u64::try_from(ms).ok())
        .filter(|ms| *ms > 0))
}

/// What is open now, keyed by the kind and ID its opening event carries,
/// with how a person would name it. A snapshot never cuts inside one.
fn open_items(state: &State) -> BTreeMap<(&'static str, String), String> {
    let mut open = BTreeMap::new();
    for lease in state.leases.values() {
        if lease.status == LeaseStatus::Active {
            open.insert(
                ("lease", lease.lease_id.clone()),
                format!(
                    "the open admission {} of session {}",
                    lease.lease_id, lease.session_id
                ),
            );
        }
    }
    for turn in state.turns.values() {
        open.insert(
            ("turn", turn.turn_id.clone()),
            format!("the push turn {} onto {}", turn.turn_id, turn.onto),
        );
    }
    for waiter in state.turn_queues.values().flatten() {
        open.insert(
            ("waiter", waiter.ticket.clone()),
            format!("the landing {} waiting for {}", waiter.ticket, waiter.onto),
        );
    }
    for message in state.messages.values().flatten() {
        open.insert(
            ("message", message.message_id.clone()),
            format!("the pending message {}", message.message_id),
        );
    }
    open
}

/// The kind and ID of what `event` opens, if it opens something.
fn opening(event: &Event) -> Option<(&'static str, String)> {
    match event {
        Event::LeaseGranted { lease } => Some(("lease", lease.lease_id.clone())),
        Event::TurnTaken { turn } => Some(("turn", turn.turn_id.clone())),
        Event::TurnQueueJoined { waiter } => Some(("waiter", waiter.ticket.clone())),
        Event::MessageSent { message, .. } => Some(("message", message.message_id.clone())),
        _ => None,
    }
}

/// `state` written as a snapshot and read back, so that what is checked is
/// what a later start would load.
fn restored(state: &mut State) -> StoreResult<String> {
    let json = serde_json::to_string(&SnapshotRecord::of(state))?;
    *state = serde_json::from_str::<SnapshotRecord>(&json)?.into_state();
    Ok(json)
}

/// The top-level state fields in which `left` and `right` differ, as a
/// snapshot records them; a field no snapshot records is named as such.
fn differences(left: &State, right: &State) -> Vec<String> {
    let fields = |state: &State| {
        let Ok(serde_json::Value::Object(mut record)) =
            serde_json::to_value(SnapshotRecord::of(state))
        else {
            return None;
        };
        let Some(serde_json::Value::Object(mut fields)) = record.remove("state") else {
            return None;
        };
        fields.extend(record);
        Some(fields)
    };
    let (Some(left), Some(right)) = (fields(left), fields(right)) else {
        return vec!["state".into()];
    };
    let named: Vec<String> = left
        .keys()
        .chain(right.keys().filter(|key| !left.contains_key(*key)))
        .filter(|key| left.get(*key) != right.get(*key))
        .cloned()
        .collect();
    if named.is_empty() {
        vec!["a field no snapshot records".into()]
    } else {
        named
    }
}

/// Which events a compaction deletes: those at or below the snapshot
/// sequence `?1` recorded before the cutoff `?2`, except the generation
/// markers from `?3` on, which date the events left (see `anchor_from`).
const REMOVED: &str = concat!(
    "seq <= ?1 AND recorded_at_ms < ?2 AND NOT (seq >= ?3 AND ",
    generation_marker!(),
    ")"
);

/// The first generation marker a compaction keeps: the start of the
/// generation of the oldest event it leaves, or of the newest generation
/// when it leaves none. `today` places an event on a day by the clock anchor
/// of its generation, so that start and every later one stay; older starts
/// go. `i64::MAX` when there is no start to keep.
fn anchor_from(connection: &Connection, target: i64, cutoff: i64) -> StoreResult<i64> {
    let first_left: Option<i64> = connection.query_row(
        "SELECT min(seq) FROM events WHERE seq > ?1 OR recorded_at_ms >= ?2",
        params![target, cutoff],
        |row| row.get(0),
    )?;
    let start: Option<i64> = connection.query_row(
        "SELECT max(seq) FROM events WHERE seq < ?1
           AND json_valid(event_json) AND json_extract(event_json, '$.type') = 'booted'",
        [first_left.unwrap_or(i64::MAX)],
        |row| row.get(0),
    )?;
    Ok(start.unwrap_or(i64::MAX))
}

impl Store {
    /// Hold the compaction transaction open this long before it commits.
    /// Only for the test that kills the daemon mid-compaction.
    pub fn set_compaction_pause(&mut self, pause: Option<Duration>) {
        self.compaction_pause = pause;
    }

    /// Compact the log to the last `keep_days` days; with `apply` false,
    /// report what would go and delete nothing. Takes as long as replaying
    /// the events after the previous snapshot, once to find the new one
    /// and check it.
    pub fn compact(&mut self, keep_days: u32, apply: bool) -> StoreResult<Outcome> {
        if !(MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&keep_days) {
            return Ok(Outcome::Error {
                code: "invalid_retention".into(),
                message: format!(
                    "keep_days must be between {MIN_RETENTION_DAYS} and {MAX_RETENTION_DAYS}: a retry whose receipt was removed is answered as a new request"
                ),
            });
        }
        let cutoff = (self.wall_ms)().saturating_sub(u64::from(keep_days) * DAY_MS);
        let cutoff_sql = i64::try_from(cutoff).unwrap_or(i64::MAX);
        let previous = self.snapshot_seq;
        // The newest event after the snapshot that is older than the window.
        let candidate: Option<i64> = self.connection.query_row(
            "SELECT max(seq) FROM events WHERE seq > ?1 AND recorded_at_ms < ?2",
            params![i64::try_from(previous)?, cutoff_sql],
            |row| row.get(0),
        )?;
        let mut target = candidate.map_or(Ok(previous), u64::try_from)?.max(previous);
        let mut held_by = None;
        let open = open_items(&self.state);
        if target > previous && !open.is_empty() {
            let mut statement = self.connection.prepare(
                "SELECT seq, event_json FROM events
                 WHERE seq > ?1 AND seq <= ?2
                   AND json_extract(event_json, '$.type')
                       IN ('lease_granted', 'turn_taken', 'turn_queue_joined', 'message_sent')
                 ORDER BY seq",
            )?;
            let mut rows =
                statement.query(params![i64::try_from(previous)?, i64::try_from(target)?])?;
            while let Some(row) = rows.next()? {
                let sequence = u64::try_from(row.get::<_, i64>(0)?)?;
                let event: Event = serde_json::from_str(&row.get::<_, String>(1)?)?;
                if let Some(name) = opening(&event).and_then(|key| open.get(&key)) {
                    target = sequence - 1;
                    held_by = Some(format!("{name}, opened at event {sequence}"));
                    break;
                }
            }
        }
        let target = target.max(previous);
        let snapshot = if target > previous {
            match self.verified_snapshot(target)? {
                Ok(json) => Some(json),
                Err(fields) => {
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "level": "error",
                            "event": "compaction_aborted",
                            "snapshot_seq": target,
                            "differs": fields,
                            "detail": "the snapshot and the events after it do not give the state full replay gives; nothing was deleted",
                        })
                    );
                    return Ok(Outcome::Error {
                        code: "snapshot_mismatch".into(),
                        message: format!(
                            "the snapshot at event {target} and the events after it do not give the state the daemon holds (it differs in {}); nothing was deleted",
                            fields.join(", ")
                        ),
                    });
                }
            }
        } else {
            None
        };
        let target_sql = i64::try_from(target)?;
        let anchor_sql = anchor_from(&self.connection, target_sql, cutoff_sql)?;
        let (events, event_bytes, first_seq, last_seq): (i64, i64, Option<i64>, Option<i64>) =
            self.connection.query_row(
                &format!(
                    "SELECT count(*), coalesce(sum(length(event_json)), 0), min(seq), max(seq)
                     FROM events WHERE {REMOVED}"
                ),
                params![target_sql, cutoff_sql, anchor_sql],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let (receipts, receipt_bytes): (i64, i64) = self.connection.query_row(
            "SELECT count(*),
                    coalesce(sum(length(request_id) + length(command_json) + length(outcome_json)), 0)
             FROM receipts WHERE recorded_at_ms < ?1",
            [cutoff_sql],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let report = CompactionReport {
            applied: apply,
            keep_days,
            cutoff_ms: cutoff,
            snapshot_seq: (target > 0).then_some(target),
            events: u64::try_from(events)?,
            first_seq: first_seq.map(u64::try_from).transpose()?,
            last_seq: last_seq.map(u64::try_from).transpose()?,
            receipts: u64::try_from(receipts)?,
            bytes: u64::try_from(event_bytes)?.saturating_add(u64::try_from(receipt_bytes)?),
            held_by,
        };
        if !apply || (snapshot.is_none() && events == 0 && receipts == 0) {
            return Ok(Outcome::Compaction { report });
        }
        let since = since_ms(&self.connection)?.unwrap_or(0);
        // From here on publications are listed from the cutoff, if any
        // event goes; otherwise from where they were.
        let since = if events > 0 { since.max(cutoff) } else { since };
        let since_sql = i64::try_from(since).unwrap_or(i64::MAX);
        let created = i64::try_from((self.wall_ms)()).unwrap_or(i64::MAX);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(json) = &snapshot {
            transaction.execute(
                "INSERT INTO snapshots(seq, state_json, cutoff_ms, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![target_sql, json, since_sql, created],
            )?;
            transaction.execute("DELETE FROM snapshots WHERE seq < ?1", [target_sql])?;
        } else {
            transaction.execute(
                "UPDATE snapshots SET cutoff_ms = ?1 WHERE seq = ?2",
                params![since_sql, target_sql],
            )?;
        }
        transaction.execute(
            &format!("DELETE FROM events WHERE {REMOVED}"),
            params![target_sql, cutoff_sql, anchor_sql],
        )?;
        transaction.execute(
            "DELETE FROM receipts WHERE recorded_at_ms < ?1",
            [cutoff_sql],
        )?;
        if let Some(pause) = self.compaction_pause {
            std::thread::sleep(pause);
        }
        transaction.commit()?;
        self.snapshot_seq = target;
        Ok(Outcome::Compaction { report })
    }

    /// The state at `target` as snapshot JSON, if that snapshot read back
    /// with the events after it applied gives the state the daemon holds;
    /// otherwise the fields that differ.
    fn verified_snapshot(&self, target: u64) -> StoreResult<Result<String, Vec<String>>> {
        let (mut state, from) = snapshot(&self.connection)?;
        let mut taken = None;
        replay_after(&self.connection, &mut state, from, |sequence, state| {
            if taken.is_none() && sequence > target {
                taken = Some(restored(state)?);
            }
            Ok(())
        })?;
        let json = match taken {
            Some(json) => json,
            None => restored(&mut state)?,
        };
        if state == self.state {
            Ok(Ok(json))
        } else {
            Ok(Err(differences(&state, &self.state)))
        }
    }
}
