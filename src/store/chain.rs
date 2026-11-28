//! The event log's hash chain (roadmap J9, issue #91).
//!
//! Each event row's `digest` is the SHA-256, in lowercase hex, of the
//! previous row's digest in hex followed by this row's `event_json`, so
//! `printf '%s%s' PREV JSON | shasum -a 256` recomputes it. The first row
//! chains from [`GENESIS`]. A compaction that deletes rows records, in
//! `chain_gaps`, the digest of the last row of each run it deleted, keyed by
//! that row's sequence, so the rows it keeps still check. The chain shows
//! that the log changed after a person wrote its head down; it does not
//! stop the owner's account from changing it.

use super::StoreResult;
use crate::sha256;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The digest the first chained row chains from.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The digest of a row holding `event_json` after a row whose digest is `previous`.
pub fn link(previous: &str, event_json: &str) -> String {
    sha256::hex(&sha256::digest(
        [previous.as_bytes(), event_json.as_bytes()]
            .concat()
            .as_slice(),
    ))
}

/// What `events verify` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub ok: bool,
    /// Rows that checked, before the first that did not.
    pub events: u64,
    /// The digest the next event chains from; `None` when the chain broke.
    pub head: Option<String>,
    /// The first sequence whose digest is missing or wrong.
    pub broken_at: Option<u64>,
}

impl Verdict {
    pub fn text(&self) -> String {
        match (&self.head, self.broken_at) {
            (Some(head), _) => format!("ok: {} events, head {head}", self.events),
            (None, Some(seq)) => format!(
                "broken at event {seq}: its digest does not match the events before it ({} events before it checked)",
                self.events
            ),
            (None, None) => "broken".into(),
        }
    }
}

/// Add the chain's column and table if this store lacks them. A column and
/// a table, not a schema change (ADR 0038): an earlier build writes rows
/// without a digest, which [`extend`] chains the next time this build opens.
pub(super) fn migrate(connection: &Connection) -> StoreResult<()> {
    if !has_digests(connection)? {
        connection.execute_batch("ALTER TABLE events ADD COLUMN digest TEXT")?;
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS chain_gaps (
            seq INTEGER PRIMARY KEY NOT NULL,
            digest TEXT NOT NULL
         )",
    )?;
    Ok(())
}

fn has_digests(connection: &Connection) -> StoreResult<bool> {
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM pragma_table_info('events') WHERE name = 'digest'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// The greatest gap digest recorded after `after` and before `before`.
fn gap(gaps: &BTreeMap<u64, String>, after: u64, before: u64) -> Option<&String> {
    gaps.range(after.saturating_add(1)..before)
        .next_back()
        .map(|(_, digest)| digest)
}

fn gaps(connection: &Connection) -> StoreResult<BTreeMap<u64, String>> {
    let mut statement = connection.prepare("SELECT seq, digest FROM chain_gaps")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get(1)?)))?;
    let mut gaps = BTreeMap::new();
    for row in rows {
        let (seq, digest) = row?;
        gaps.insert(u64::try_from(seq)?, digest);
    }
    Ok(gaps)
}

/// Chain the rows after the newest chained row, which are rows from before
/// the upgrade or rows an earlier build wrote, and return the head. A row
/// without a digest before a chained one stays as it is, for `verify`.
pub(super) fn extend(connection: &Connection) -> StoreResult<String> {
    // One transaction: one sync for an upgrade's pass over the whole log.
    let connection = connection.unchecked_transaction()?;
    let gaps = gaps(&connection)?;
    let last: Option<(i64, String)> = connection
        .query_row(
            "SELECT seq, digest FROM events WHERE digest IS NOT NULL ORDER BY seq DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (mut after, mut head) = match last {
        Some((seq, digest)) => (u64::try_from(seq)?, digest),
        None => (0, GENESIS.to_string()),
    };
    let rows: Vec<(i64, String)> = connection
        .prepare("SELECT seq, event_json FROM events WHERE seq > ?1 ORDER BY seq")?
        .query_map([i64::try_from(after)?], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    for (seq, json) in rows {
        let seq_u = u64::try_from(seq)?;
        let previous = gap(&gaps, after, seq_u).unwrap_or(&head);
        head = link(previous, &json);
        connection.execute(
            "UPDATE events SET digest = ?1 WHERE seq = ?2",
            params![head, seq],
        )?;
        after = seq_u;
    }
    connection.commit()?;
    Ok(gap(&gaps, after, u64::MAX).cloned().unwrap_or(head))
}

/// Before a compaction deletes the rows `removed` selects, record the
/// digest of the last row of each run of them. `removed` selects no row
/// above its `?1`, the snapshot's sequence, so only rows up to it are read.
pub(super) fn record_gaps(
    transaction: &Transaction<'_>,
    removed: &str,
    arguments: impl rusqlite::Params,
) -> StoreResult<()> {
    let mut statement = transaction.prepare(&format!(
        "SELECT seq, digest, ({removed}) FROM events WHERE seq <= ?1 ORDER BY seq"
    ))?;
    // A NULL condition deletes nothing, as in the DELETE itself.
    let rows: Vec<(i64, Option<String>, Option<bool>)> = statement
        .query_map(arguments, |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut rows = rows.into_iter().peekable();
    while let Some((seq, digest, gone)) = rows.next() {
        let run_ends = rows
            .peek()
            .is_none_or(|(_, _, next_gone)| *next_gone != Some(true));
        if let (Some(true), true, Some(digest)) = (gone, run_ends, digest) {
            transaction.execute(
                "INSERT OR REPLACE INTO chain_gaps(seq, digest) VALUES (?1, ?2)",
                params![seq, digest],
            )?;
        }
    }
    Ok(())
}

/// Recompute the chain of the store at `path`, read-only, and report the
/// first row whose digest is missing or wrong.
pub fn verify(path: &Path) -> StoreResult<Verdict> {
    if !path.is_file() {
        return Err(format!(
            "no coordination store at {}: the coordinator has not run with this state directory",
            path.display()
        )
        .into());
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_millis(250))?;
    if !has_digests(&connection)? {
        return Err("this store's events carry no digests yet: start the coordinator with this build once, then verify again".into());
    }
    let read = connection.unchecked_transaction()?;
    let gaps = gaps(&read)?;
    let mut statement = read.prepare("SELECT seq, event_json, digest FROM events ORDER BY seq")?;
    let mut rows = statement.query([])?;
    let (mut after, mut head, mut events) = (0u64, GENESIS.to_string(), 0u64);
    while let Some(row) = rows.next()? {
        let seq = u64::try_from(row.get::<_, i64>(0)?)?;
        let json: String = row.get(1)?;
        let stored: Option<String> = row.get(2)?;
        let expected = link(gap(&gaps, after, seq).unwrap_or(&head), &json);
        if stored.as_deref() != Some(expected.as_str()) {
            return Ok(Verdict {
                ok: false,
                events,
                head: None,
                broken_at: Some(seq),
            });
        }
        (after, head, events) = (seq, expected, events + 1);
    }
    Ok(Verdict {
        ok: true,
        events,
        head: Some(gap(&gaps, after, u64::MAX).cloned().unwrap_or(head)),
        broken_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest is reproducible outside LangSpan: SHA-256 (checked against
    /// the FIPS vectors in `ids/sha256.rs`) of the previous digest's hex
    /// followed by the JSON, with no separator.
    #[test]
    fn a_link_is_the_sha256_of_the_previous_hex_and_the_json() {
        let json = r#"{"type":"booted","generation":1}"#;
        let input = format!("{}{json}", "0".repeat(64));
        let first = link(GENESIS, json);
        assert_eq!(first, sha256::hex(&sha256::digest(input.as_bytes())));
        assert_eq!(first.len(), 64);
        assert_ne!(link(&first, json), first, "each link depends on the last");
    }
}
