//! A report read on a connection of its own (issue #581), so a report of
//! a long history never holds the state owner while an admission waits.

use super::{Store, StoreResult};
use langspan_core::{Outcome, REPORT_HISTORY_EVENTS, TODAY_EVENTS};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A read-only connection to the store the state owner writes. In WAL
/// mode its reads see one committed snapshot and never block a write.
pub struct ReportReader {
    connection: Connection,
    path: PathBuf,
}

impl ReportReader {
    /// Opens `path`, a store the state owner has already opened, read-only.
    pub fn open(path: &Path) -> StoreResult<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_millis(250))?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }

    /// The report the owner's [`Store::report`] would give, from one
    /// snapshot of the log. Refusals that need the state are the owner's,
    /// asked first with [`Store::report_refused`].
    pub fn report(
        &mut self,
        workspace_id: &str,
        from_ms: u64,
        wall_ms: u64,
    ) -> StoreResult<Outcome> {
        let read = self.connection.transaction()?;
        let snapshot_seq: i64 =
            read.query_row("SELECT coalesce(max(seq), 0) FROM snapshots", [], |row| {
                row.get(0)
            })?;
        pause_for_test(&self.path);
        Store::read_report(
            &read,
            u64::try_from(snapshot_seq)?,
            (TODAY_EVENTS, REPORT_HISTORY_EVENTS),
            workspace_id,
            from_ms,
            wall_ms,
        )
    }
}

/// For tests only, in a debug build: `LANGSPAN_TEST_REPORT_PAUSE_MS`, at
/// most 10 s, holds each report inside its read, standing in for a slow
/// read of a long history. Before it holds, it writes an empty
/// `report-reading` beside the store at `path`, inside the owner-only state
/// directory, so a test waits for the read to begin instead of sleeping.
/// A release build never reads the variable.
#[cfg(debug_assertions)]
fn pause_for_test(path: &Path) {
    if let Some(ms) = std::env::var("LANGSPAN_TEST_REPORT_PAUSE_MS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
    {
        let _ = std::fs::write(path.with_file_name("report-reading"), b"");
        std::thread::sleep(Duration::from_millis(ms.min(10_000)));
    }
}
#[cfg(not(debug_assertions))]
fn pause_for_test(_path: &Path) {}
