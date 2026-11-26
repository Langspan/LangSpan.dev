//! A derived, rebuildable index of symbols and references per workspace.
//!
//! It lives in its own SQLite file beside the event store. Deleting the file
//! loses nothing: it is rebuilt from the working trees. Every row is keyed
//! by workspace and path and carries the digest of the exact content it was
//! extracted from, so each worktree is indexed as it is on disk. One thread
//! writes; the daemon's request path reads. See ADR 0008.

use langspan_core::{GraphCoverage, InterfaceChange, NameRef, SymbolRef};
use langspan_graph::MAX_SOURCE_BYTES;
use langspan_graph::{Change, FileGraph, Language, Symbol, SymbolKind};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
    time::Instant,
};

const SCHEMA: i64 = 4;
/// Files parsed in one pass. The rest wait for the next.
pub const FILES_PER_PASS: usize = 200;
pub const MAX_RESULTS: u32 = 200;

pub struct Index {
    connection: Connection,
    /// `FILES_PER_PASS`, except where a test sets another.
    files_per_pass: usize,
}

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exported {
    pub files: u32,
    pub partial_files: u32,
    pub bytes: u64,
}

/// What an import found, one count per file line after the header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Imported {
    pub loaded: u32,
    pub different: u32,
    pub kept: u32,
    pub over_limits: u32,
    pub malformed: u32,
}

/// An exported symbol of `path` changed between two indexed versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub path: String,
    /// The content the comparison started from and ended at. Together they
    /// name one transition, so A→B and a later C→B are different notices.
    pub before_digest: String,
    pub digest: String,
    pub changes: Vec<InterfaceChange>,
    pub dependents: Vec<String>,
}

type Failure = Box<dyn std::error::Error + Send + Sync>;

impl Index {
    /// The single writer. A file from another schema version is discarded
    /// and rebuilt: it holds nothing that cannot be derived again.
    pub fn open(path: &Path) -> Result<Self, Failure> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        let _: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != SCHEMA {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS symbols;
                 DROP TABLE IF EXISTS refs; DROP TABLE IF EXISTS imports;
                 DROP TABLE IF EXISTS baselines; DROP TABLE IF EXISTS calls;
                 DROP TABLE IF EXISTS unread; DROP TABLE IF EXISTS scan_summary;
                 DROP TABLE IF EXISTS inflight_files; DROP TABLE IF EXISTS inflight;
                 DROP TABLE IF EXISTS retries; DROP TABLE IF EXISTS passes;
                 CREATE TABLE files (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL,
                    scan_digest TEXT NOT NULL, digest TEXT, partial INTEGER NOT NULL DEFAULT 0,
                    baseline_digest TEXT, partial_since INTEGER, oversize_digest TEXT,
                    PRIMARY KEY (workspace_id, path)) WITHOUT ROWID;
                 CREATE TABLE unread (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, reason TEXT NOT NULL,
                    PRIMARY KEY (workspace_id, path)) WITHOUT ROWID;
                 CREATE TABLE scan_summary (
                    workspace_id TEXT NOT NULL PRIMARY KEY, too_large INTEGER NOT NULL,
                    unscanned INTEGER NOT NULL, languages TEXT NOT NULL) WITHOUT ROWID;
                 CREATE TABLE symbols (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                    kind TEXT NOT NULL, line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    signature TEXT NOT NULL, exported INTEGER NOT NULL,
                    body_digest TEXT NOT NULL);
                 CREATE INDEX symbols_by_name ON symbols(workspace_id, name);
                 CREATE INDEX symbols_by_path ON symbols(workspace_id, path);
                 CREATE TABLE refs (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                    line INTEGER NOT NULL);
                 CREATE INDEX refs_by_name ON refs(workspace_id, name, path);
                 CREATE INDEX refs_by_path ON refs(workspace_id, path);
                 CREATE TABLE imports (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, target TEXT NOT NULL,
                    line INTEGER NOT NULL);
                 CREATE INDEX imports_by_path ON imports(workspace_id, path);
                 CREATE TABLE baselines (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                    kind TEXT NOT NULL, signature TEXT NOT NULL);
                 CREATE INDEX baselines_by_path ON baselines(workspace_id, path);
                 CREATE TABLE calls (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, caller TEXT,
                    callee TEXT NOT NULL, line INTEGER NOT NULL, args INTEGER);
                 CREATE INDEX calls_by_callee ON calls(workspace_id, callee, path, line);
                 CREATE INDEX calls_by_path ON calls(workspace_id, path, line);
                 PRAGMA user_version = 4;",
            )?;
            transaction.commit()?;
        }
        // The root each workspace was last indexed at, and its registration
        // revision then: how many times its ID had been moved or removed
        // (roadmap G6). Added without a schema change: an index without it
        // is kept, and each workspace in it starts again at its next pass
        // (see `start_again_at`).
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS roots (
                workspace_id TEXT NOT NULL PRIMARY KEY, root TEXT NOT NULL,
                revision INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;",
        )?;
        // Each file's in-flight set (roadmap C7): the exported symbols its
        // indexed content adds or changes against the worktree's `HEAD`,
        // and the indexed digest and `HEAD` blob it was compared at. Added
        // without a schema change: an index without them computes them at
        // its next pass.
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS inflight_files (
                workspace_id TEXT NOT NULL, path TEXT NOT NULL, digest TEXT NOT NULL,
                head_object TEXT, PRIMARY KEY (workspace_id, path)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS inflight (
                workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                kind TEXT NOT NULL, signature TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS inflight_by_path ON inflight(workspace_id, path);",
        )?;
        // Each file version whose read for the index or for the in-flight
        // comparison failed in a way that may pass, with how often and the
        // pass it may next be tried at (see `Retry`), and each workspace's
        // count of passes. Added without a schema change: an index without
        // them starts every such version afresh.
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS retries (
                workspace_id TEXT NOT NULL, path TEXT NOT NULL, stage TEXT NOT NULL,
                version TEXT NOT NULL, failures INTEGER NOT NULL, next_pass INTEGER NOT NULL,
                PRIMARY KEY (workspace_id, path, stage)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS passes (
                workspace_id TEXT NOT NULL PRIMARY KEY, count INTEGER NOT NULL) WITHOUT ROWID;",
        )?;
        // The digest of each baseline function's body, which links a removal
        // and an addition as a rename (roadmap B10). Added without a schema
        // change: a baseline from before has none, so its next change is
        // told as a removal and an addition, as it was.
        let has_block: bool = connection.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('baselines') WHERE name = 'block_digest'",
            [],
            |row| row.get(0),
        )?;
        if !has_block {
            connection.execute_batch("ALTER TABLE baselines ADD COLUMN block_digest TEXT;")?;
        }
        Ok(Self {
            connection,
            files_per_pass: FILES_PER_PASS,
        })
    }

    /// Parse at most `files` per pass from now on. Only tests set it.
    pub fn set_files_per_pass(&mut self, files: usize) {
        self.files_per_pass = files;
    }

    /// A reader for the request path. It never creates or changes the file.
    pub fn open_reader(path: &Path) -> Result<Self, Failure> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_millis(100))?;
        Ok(Self {
            connection,
            files_per_pass: FILES_PER_PASS,
        })
    }

    /// The exported symbols of the last version of `path` that parsed
    /// cleanly, and that version's digest. Interface comparisons start from
    /// here, so a partial parse in between cannot hide a removal.
    fn baseline(
        &self,
        workspace_id: &str,
        path: &str,
    ) -> Result<Option<(String, FileGraph)>, Failure> {
        let digest: Option<String> = self
            .connection
            .query_row(
                "SELECT baseline_digest FROM files WHERE workspace_id = ?1 AND path = ?2",
                params![workspace_id, path],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let Some(digest) = digest else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare_cached(
            "SELECT name, kind, signature, block_digest FROM baselines
             WHERE workspace_id = ?1 AND path = ?2",
        )?;
        let symbols = statement
            .query_map(params![workspace_id, path], |row| {
                Ok(Symbol {
                    name: row.get(0)?,
                    kind: SymbolKind::parse(&row.get::<_, String>(1)?)
                        .unwrap_or(SymbolKind::Function),
                    line: 0,
                    end_line: 0,
                    signature: row.get(2)?,
                    exported: true,
                    bytes: 0..0,
                    block: None,
                    block_digest: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some((
            digest,
            FileGraph {
                symbols,
                ..FileGraph::default()
            },
        )))
    }

    fn notice(
        &self,
        workspace_id: &str,
        path: &str,
        (before_digest, before): (String, FileGraph),
        (digest, after): (String, &FileGraph),
    ) -> Result<Option<Notice>, Failure> {
        // Parameter counts for the write-time break check (ADR 0029).
        let counts = |signature: &Option<String>| {
            let language = Language::for_path(path)?;
            let arity = langspan_graph::arity(language, signature.as_deref()?)?;
            Some(langspan_core::Arity {
                min: arity.min,
                max: arity.max,
                receiver: arity.receiver,
            })
            .filter(|arity| {
                arity.min <= langspan_core::MAX_ARITY
                    && arity.max.is_none_or(|max| max <= langspan_core::MAX_ARITY)
            })
        };
        // The exported signatures a version gives one name and kind, as a
        // notice shows several: each as written, joined by " | ", as many as
        // fit a notice's 256 bytes (one signature is at most 240), so that
        // they never make the core refuse the notice and the changes in it.
        let written = |graph: &FileGraph, name: &str, kind| {
            let mut signatures: Vec<&str> = graph
                .symbols
                .iter()
                .filter(|symbol| symbol.exported && symbol.name == name && symbol.kind == kind)
                .map(|symbol| symbol.signature.as_str())
                .filter(|signature| !signature.chars().any(char::is_control))
                .collect();
            signatures.sort_unstable();
            signatures.dedup();
            let mut shown = String::new();
            for signature in signatures {
                let joined = if shown.is_empty() {
                    signature.to_owned()
                } else {
                    format!("{shown} | {signature}")
                };
                if joined.len() > 256 {
                    break;
                }
                shown = joined;
            }
            (!shown.is_empty()).then_some(shown)
        };
        let changes: Vec<InterfaceChange> = langspan_graph::diff(&before, after)
            .into_iter()
            .map(|change| {
                let mut after_name = None;
                let (label, before, after, number) = match change.change {
                    // The signature that went, for a definition's history
                    // (roadmap D4) to see it come back and for a write
                    // calling it to be shown (roadmap B10).
                    Change::Removed => (
                        "removed",
                        written(&before, &change.name, change.kind),
                        None,
                        None,
                    ),
                    Change::SignatureChanged { before, after } => {
                        ("signature_changed", Some(before), Some(after), None)
                    }
                    // Told at a write calling the old name (roadmap B10).
                    Change::Renamed { to, before, after } => {
                        after_name = Some(to);
                        ("renamed", Some(before), Some(after), None)
                    }
                    // Only how many definitions share the name changed: no
                    // parameter counts, so no break alert (ADR 0029).
                    Change::DefinitionsAdded {
                        signatures,
                        before,
                        after,
                    } => (
                        "definition_added",
                        None,
                        Some(signatures),
                        Some((before, after)),
                    ),
                    Change::DefinitionsRemoved {
                        signatures,
                        before,
                        after,
                    } => (
                        "definition_removed",
                        Some(signatures),
                        None,
                        Some((before, after)),
                    ),
                    // A new name: history only, never told to anyone
                    // (roadmap D4), so no parameter counts.
                    Change::Added => (
                        "added",
                        None,
                        written(after, &change.name, change.kind),
                        None,
                    ),
                };
                // Counts for the changes the write-time break check reads
                // (ADR 0029): never for a new name or a count change.
                let arity = |signature| {
                    if label == "added" || number.is_some() {
                        None
                    } else {
                        counts(signature)
                    }
                };
                InterfaceChange {
                    name: change.name,
                    kind: change.kind.name().into(),
                    change: label.into(),
                    before_arity: arity(&before),
                    after_arity: arity(&after),
                    before_count: number.map(|(before, _)| before),
                    after_count: number.map(|(_, after)| after),
                    before,
                    after,
                    after_name,
                }
            })
            .take(langspan_core::NOTICE_CHANGES)
            .collect();
        if changes.is_empty() {
            return Ok(None);
        }
        let names: Vec<&str> = changes.iter().map(|change| change.name.as_str()).collect();
        Ok(Some(Notice {
            path: path.into(),
            before_digest,
            digest,
            dependents: self.dependents(workspace_id, path, &names)?,
            changes,
        }))
    }

    fn dependents(
        &self,
        workspace_id: &str,
        path: &str,
        names: &[&str],
    ) -> Result<Vec<String>, Failure> {
        let mut found = std::collections::BTreeSet::new();
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT path FROM refs
             WHERE workspace_id = ?1 AND name = ?2 AND path != ?3 ORDER BY path LIMIT ?4",
        )?;
        for name in names {
            let limit = langspan_core::NOTICE_DEPENDENTS as i64;
            for row in statement.query_map(params![workspace_id, name, path, limit], |row| {
                row.get::<_, String>(0)
            })? {
                found.insert(row?);
            }
        }
        Ok(found
            .into_iter()
            .take(langspan_core::NOTICE_DEPENDENTS)
            .collect())
    }

    /// A workspace now at another root than it was indexed at (`workspace
    /// move`) is another checkout: what was indexed for the old one is
    /// forgotten, so differences between the two checkouts are not reported
    /// as interface changes. So is a workspace in a later registration
    /// revision, even at the same root: removed and registered again, or
    /// switched away and back. What changed while that root was not this
    /// registration's is not news from the earlier one. A workspace
    /// with no root record (indexed by a build before roots were kept) is of
    /// an unknown checkout: what it holds is forgotten too, so its first
    /// pass after the upgrade is a baseline that reports nothing.
    fn start_again_at(
        &mut self,
        workspace_id: &str,
        root: &Path,
        revision: u32,
    ) -> Result<(), Failure> {
        if self.indexed_as(workspace_id, root, revision)? {
            return Ok(());
        }
        let root = root.to_string_lossy();
        let transaction = self.connection.transaction()?;
        for table in [
            "files",
            "unread",
            "scan_summary",
            "symbols",
            "refs",
            "imports",
            "baselines",
            "calls",
            "inflight_files",
            "inflight",
            "retries",
            "passes",
        ] {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE workspace_id = ?1"),
                [workspace_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO roots(workspace_id, root, revision) VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace_id) DO UPDATE SET root = excluded.root,
                revision = excluded.revision",
            params![workspace_id, root, revision],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Whether what the index holds for the workspace was indexed at this
    /// root and registration revision: of this registration, not an
    /// earlier one.
    fn indexed_as(&self, workspace_id: &str, root: &Path, revision: u32) -> Result<bool, Failure> {
        let indexed: Option<(String, u32)> = self
            .connection
            .query_row(
                "SELECT root, revision FROM roots WHERE workspace_id = ?1",
                [workspace_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(indexed.is_some_and(|(at, indexed_revision)| {
            at == root.to_string_lossy().as_ref() && indexed_revision == revision
        }))
    }

    /// Bring one workspace's index up to `scan.files`, the indexable paths
    /// of its latest complete scan with their content digests. A path in
    /// `scan.unobserved` was listed but not versioned by that scan (a
    /// partial scan, a file over the size bound): it keeps what the index
    /// already holds for it and is not treated as removed. Parses at most
    /// `FILES_PER_PASS` files and stops at `deadline`; what is left is picked
    /// up by the next call. `now_ms` (wall clock) dates a file that becomes
    /// partial. `revision` is the workspace's registration revision when
    /// the scan was taken (`daemon::Registration`). Returns the interface
    /// changes it found.
    pub fn update(
        &mut self,
        workspace_id: &str,
        root: &Path,
        revision: u32,
        scan: &ScanView,
        now_ms: u64,
        deadline: Instant,
    ) -> Result<Vec<Notice>, Failure> {
        self.start_again_at(workspace_id, root, revision)?;
        let (files, unobserved) = (&scan.files, &scan.unobserved);
        // Each path's indexed digest, and the scanned digest at which it was
        // found too large to parse: that version is not tried again.
        type Indexed = BTreeMap<String, (Option<String>, Option<String>)>;
        let indexed: Indexed = {
            let mut statement = self.connection.prepare_cached(
                "SELECT path, digest, oversize_digest FROM files WHERE workspace_id = ?1",
            )?;
            statement
                .query_map([workspace_id], |row| {
                    Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
                })?
                .collect::<Result<_, _>>()?
        };
        // A deleted or renamed file removes everything it exported. Say so,
        // with the files that still mention those names, before forgetting it.
        let removed = |path: &&String| !files.contains_key(*path) && !unobserved.contains(*path);
        let mut notices = Vec::new();
        for path in indexed.keys().filter(removed) {
            if let Some(baseline) = self.baseline(workspace_id, path)? {
                notices.extend(self.notice(
                    workspace_id,
                    path,
                    baseline,
                    ("deleted".into(), &FileGraph::default()),
                )?);
            }
        }
        let transaction = self.connection.transaction()?;
        for path in indexed.keys().filter(removed) {
            for table in [
                "files",
                "symbols",
                "refs",
                "imports",
                "baselines",
                "calls",
                "inflight_files",
                "inflight",
                "retries",
            ] {
                transaction.execute(
                    &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND path = ?2"),
                    params![workspace_id, path],
                )?;
            }
        }
        for (path, digest) in files {
            transaction.execute(
                "INSERT INTO files(workspace_id, path, scan_digest) VALUES (?1, ?2, ?3)
                 ON CONFLICT(workspace_id, path) DO UPDATE SET scan_digest = excluded.scan_digest",
                params![workspace_id, path, digest],
            )?;
        }
        write_scan_blind(&transaction, workspace_id, &scan.blind)?;
        transaction.commit()?;

        let pass = self.next_pass(workspace_id)?;
        let retries = Retries::load(&self.connection, workspace_id, Retries::INDEX, pass)?;
        // Versions not tried yet come first, then the ones whose retry is
        // due, so files that keep failing never hold the pass from others.
        let pending = retries.order(
            files
                .iter()
                .filter(|(path, digest)| {
                    let (indexed, oversize) = indexed.get(*path).cloned().unwrap_or_default();
                    indexed.as_ref() != Some(digest) && oversize.as_ref() != Some(digest)
                })
                .map(|(path, digest)| ((path, digest), path.as_str(), digest.clone())),
        );
        for (failures, (path, digest)) in pending.into_iter().take(self.files_per_pass) {
            if Instant::now() >= deadline {
                break;
            }
            // No parser for it, or the parser gives no tree: that holds for
            // this version on every later pass too.
            let Some(language) = Language::for_path(path) else {
                retries.give_up(&self.connection, path, digest)?;
                continue;
            };
            // The file must still be exactly what the scan saw. Read up to
            // the scan's own bound, so a version the scan digested can be
            // confirmed. Not read (removed, not a regular file any more, not
            // readable, changed or grown since the scan): tried again with a
            // bound, as the next scan usually brings its new state.
            let scanned = crate::observer::Limits::default().max_file_bytes;
            let Some(bytes) = read_exact_version(root, path, digest, scanned) else {
                retries.failed(&self.connection, path, digest, failures)?;
                continue;
            };
            // Too large to parse: remembered for this version, so it neither
            // waits forever nor takes a place in every later pass. Only the
            // confirmed scanned bytes say so; a file that grew after the
            // scan says nothing about the scanned version.
            if bytes.len() > MAX_SOURCE_BYTES {
                self.connection.execute(
                    "UPDATE files SET oversize_digest = ?3 WHERE workspace_id = ?1 AND path = ?2",
                    params![workspace_id, path, digest],
                )?;
                retries.clear(&self.connection, path)?;
                continue;
            }
            let Ok(mut graph) = langspan_graph::extract(language, &bytes) else {
                retries.give_up(&self.connection, path, digest)?;
                continue;
            };
            retries.clear(&self.connection, path)?;
            langspan_graph::digest_blocks(&mut graph, &bytes, |block| {
                crate::sha256::hex(&crate::sha256::digest(block))
            });
            // Same bound and parser as `extract`, so it fails only where that
            // did; a file without calls stores none.
            let calls = langspan_graph::calls(language, &bytes).unwrap_or_default();
            let before = self.baseline(workspace_id, path)?;
            let transaction = self.connection.transaction()?;
            for table in ["symbols", "refs", "imports", "calls"] {
                transaction.execute(
                    &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND path = ?2"),
                    params![workspace_id, path],
                )?;
            }
            for symbol in &graph.symbols {
                let body = bytes.get(symbol.bytes.clone()).unwrap_or_default();
                transaction.execute(
                    "INSERT INTO symbols VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        workspace_id,
                        path,
                        symbol.name,
                        symbol.kind.name(),
                        symbol.line,
                        symbol.end_line,
                        symbol.signature,
                        symbol.exported,
                        crate::sha256::hex(&crate::sha256::digest(body))
                    ],
                )?;
            }
            for (call, caller) in calls.iter().zip(innermost(&graph.symbols, &calls)) {
                transaction.execute(
                    "INSERT INTO calls VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![workspace_id, path, caller, call.name, call.line, call.args],
                )?;
            }
            for reference in &graph.references {
                transaction.execute(
                    "INSERT INTO refs VALUES (?1, ?2, ?3, ?4)",
                    params![workspace_id, path, reference.name, reference.line],
                )?;
            }
            for import in &graph.imports {
                transaction.execute(
                    "INSERT INTO imports VALUES (?1, ?2, ?3, ?4)",
                    params![workspace_id, path, import.name, import.line],
                )?;
            }
            // A partial file keeps the time it first became partial across
            // further partial versions; a clean parse clears it.
            transaction.execute(
                "UPDATE files SET digest = ?3, partial = ?4, oversize_digest = NULL,
                    partial_since = CASE WHEN NOT ?4 THEN NULL
                        WHEN partial = 1 AND partial_since IS NOT NULL THEN partial_since
                        ELSE ?5 END
                 WHERE workspace_id = ?1 AND path = ?2",
                params![workspace_id, path, digest, graph.partial, now_ms as i64],
            )?;
            // Queries see partial results; the interface baseline moves only
            // on a clean parse.
            if !graph.partial {
                transaction.execute(
                    "DELETE FROM baselines WHERE workspace_id = ?1 AND path = ?2",
                    params![workspace_id, path],
                )?;
                for symbol in graph.symbols.iter().filter(|symbol| symbol.exported) {
                    transaction.execute(
                        "INSERT INTO baselines(workspace_id, path, name, kind, signature,
                            block_digest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            workspace_id,
                            path,
                            symbol.name,
                            symbol.kind.name(),
                            symbol.signature,
                            symbol.block_digest
                        ],
                    )?;
                }
                transaction.execute(
                    "UPDATE files SET baseline_digest = ?3 WHERE workspace_id = ?1 AND path = ?2",
                    params![workspace_id, path, digest],
                )?;
            }
            transaction.commit()?;
            // A first clean indexing has no earlier version to differ from,
            // and a partial parse is not trusted to say a symbol is gone.
            if graph.partial {
                continue;
            }
            if let Some(before) = before {
                notices.extend(self.notice(
                    workspace_id,
                    path,
                    before,
                    (digest.clone(), &graph),
                )?);
            }
        }
        self.update_inflight(workspace_id, root, scan, pass, deadline)?;
        Ok(notices)
    }

    /// Bring the workspace's in-flight set up to the scan (roadmap C7): for
    /// each file that differs from `HEAD` and is indexed at its scanned
    /// content, the exported symbols that `HEAD`'s version of the file
    /// lacks or has with another signature. A file the scan found clean,
    /// or no longer lists, has none. A file parsed partially, at either
    /// version, says nothing. Reads at most `FILES_PER_PASS` blobs from Git
    /// and stops at `deadline`; the rest wait for the next pass.
    fn update_inflight(
        &mut self,
        workspace_id: &str,
        root: &Path,
        scan: &ScanView,
        pass: i64,
        deadline: Instant,
    ) -> Result<(), Failure> {
        type Compared = BTreeMap<String, (String, Option<String>)>;
        let compared: Compared = self
            .connection
            .prepare_cached(
                "SELECT path, digest, head_object FROM inflight_files WHERE workspace_id = ?1",
            )?
            .query_map([workspace_id], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
            })?
            .collect::<Result<_, _>>()?;
        let retries = Retries::load(&self.connection, workspace_id, Retries::COMPARE, pass)?;
        // Not read by this scan: what was said of it still holds.
        let kept = |path: &String| scan.dirty.contains_key(path) || scan.unobserved.contains(path);
        let transaction = self.connection.transaction()?;
        for path in compared.keys().filter(|path| !kept(path)) {
            for table in ["inflight_files", "inflight"] {
                transaction.execute(
                    &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND path = ?2"),
                    params![workspace_id, path],
                )?;
            }
        }
        for path in retries.paths().filter(|path| !kept(path)) {
            transaction.execute(
                "DELETE FROM retries WHERE workspace_id = ?1 AND path = ?2 AND stage = ?3",
                params![workspace_id, path, Retries::COMPARE],
            )?;
        }
        transaction.commit()?;
        // The files to compare: each indexed at its scanned content (until
        // then the index cannot say what it defines) and not yet compared
        // at that content and `HEAD` blob.
        let mut due = Vec::new();
        for (path, head_object) in &scan.dirty {
            let indexed: Option<(Option<String>, bool)> = self
                .connection
                .query_row(
                    "SELECT digest, partial FROM files WHERE workspace_id = ?1 AND path = ?2",
                    params![workspace_id, path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((Some(digest), partial)) = indexed else {
                continue;
            };
            if scan.files.get(path) != Some(&digest) {
                continue;
            }
            if compared.get(path) == Some(&(digest.clone(), head_object.clone())) {
                continue;
            }
            let version = format!("{digest} {}", head_object.as_deref().unwrap_or("-"));
            due.push((
                (path, head_object, digest, partial, version.clone()),
                path.as_str(),
                version,
            ));
        }
        let mut reads = 0;
        for (failures, (path, head_object, digest, partial, version)) in retries.order(due) {
            if Instant::now() >= deadline || reads >= FILES_PER_PASS {
                break;
            }
            let inflight = if partial {
                Vec::new()
            } else {
                let head = match head_object {
                    Some(object) => {
                        reads += 1;
                        match head_graph(root, path, object, deadline) {
                            Ok(head) => head,
                            // Not read (a timeout, a missing object): tried
                            // again at a later pass, up to `Retries::ATTEMPTS`
                            // times; after that nothing is said of this
                            // version, as for one that cannot be parsed.
                            Err(HeadUnread) => {
                                if retries.failed(&self.connection, path, &version, failures)? {
                                    None
                                } else {
                                    continue;
                                }
                            }
                        }
                    }
                    None => Some(FileGraph::default()),
                };
                match head {
                    Some(head) => self.added_or_changed(workspace_id, path, &head)?,
                    None => Vec::new(),
                }
            };
            let transaction = self.connection.transaction()?;
            transaction.execute(
                "DELETE FROM inflight WHERE workspace_id = ?1 AND path = ?2",
                params![workspace_id, path],
            )?;
            for symbol in &inflight {
                transaction.execute(
                    "INSERT INTO inflight VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        workspace_id,
                        path,
                        symbol.name,
                        symbol.kind.name(),
                        symbol.signature
                    ],
                )?;
            }
            transaction.execute(
                "INSERT INTO inflight_files VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(workspace_id, path) DO UPDATE SET digest = excluded.digest,
                    head_object = excluded.head_object",
                params![workspace_id, path, digest, head_object],
            )?;
            transaction.execute(
                "DELETE FROM retries WHERE workspace_id = ?1 AND path = ?2 AND stage = ?3",
                params![workspace_id, path, Retries::COMPARE],
            )?;
            transaction.commit()?;
        }
        Ok(())
    }

    /// Count one more indexing pass of the workspace and return its number,
    /// the clock `Retries` waits by.
    fn next_pass(&self, workspace_id: &str) -> Result<i64, Failure> {
        Ok(self.connection.query_row(
            "INSERT INTO passes(workspace_id, count) VALUES (?1, 1)
             ON CONFLICT(workspace_id) DO UPDATE SET count = count + 1 RETURNING count",
            [workspace_id],
            |row| row.get(0),
        )?)
    }

    /// The exported symbols indexed for `path` that `head` does not export
    /// with the same name, kind and signature. Signatures are compared by
    /// their tokens, as a reformat is (`crate::reformat::tokens`): spacing
    /// between tokens aside, but not the whitespace in a string literal.
    fn added_or_changed(
        &self,
        workspace_id: &str,
        path: &str,
        head: &FileGraph,
    ) -> Result<Vec<Symbol>, Failure> {
        let compared = |signature: &str| signature_tokens(path, signature);
        let before: BTreeSet<(&str, SymbolKind, Vec<Vec<u8>>)> = head
            .symbols
            .iter()
            .filter(|symbol| symbol.exported)
            .map(|symbol| {
                (
                    symbol.name.as_str(),
                    symbol.kind,
                    compared(&symbol.signature),
                )
            })
            .collect();
        let mut statement = self.connection.prepare_cached(
            "SELECT name, kind, signature FROM symbols
             WHERE workspace_id = ?1 AND path = ?2 AND exported = 1 ORDER BY line",
        )?;
        let symbols = statement
            .query_map(params![workspace_id, path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(symbols
            .into_iter()
            .filter_map(|(name, kind, signature)| {
                let kind = SymbolKind::parse(&kind)?;
                if before.contains(&(name.as_str(), kind, compared(&signature))) {
                    return None;
                }
                Some(Symbol {
                    name,
                    kind,
                    line: 0,
                    end_line: 0,
                    signature,
                    exported: true,
                    bytes: 0..0,
                    block: None,
                    block_digest: None,
                })
            })
            .collect())
    }

    /// The workspace's in-flight symbols, in path, name, kind and signature
    /// order, at most `limit`: `(path, name, kind, signature)`.
    pub fn inflight(&self, workspace_id: &str, limit: usize) -> Result<Vec<InflightRow>, Failure> {
        self.inflight_after(workspace_id, None, limit)
    }

    /// As `inflight`, from the first row after `after` in that order.
    pub fn inflight_after(
        &self,
        workspace_id: &str,
        after: Option<&InflightRow>,
        limit: usize,
    ) -> Result<Vec<InflightRow>, Failure> {
        let mut statement = self.connection.prepare_cached(
            "SELECT path, name, kind, signature FROM inflight WHERE workspace_id = ?1
                AND (?2 IS NULL OR (path, name, kind, signature) > (?2, ?3, ?4, ?5))
             ORDER BY path, name, kind, signature LIMIT ?6",
        )?;
        let (path, name, kind, signature) = after.map_or((None, None, None, None), |row| {
            (Some(&row.0), Some(&row.1), Some(&row.2), Some(&row.3))
        });
        let rows = statement
            .query_map(
                params![workspace_id, path, name, kind, signature, limit as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The workspace's files that mention `name`, or that import (in
    /// `imports`, from `imports_of`) a path whose last part is `name` or
    /// the module of `path` (see `module_name`). Only files in `among` when
    /// given. A name match, at most `limit`, in path order.
    pub fn mentioning(
        &self,
        workspace_id: &str,
        (path, name): (&str, &str),
        imports: &[(String, String)],
        among: Option<&BTreeSet<String>>,
        limit: usize,
    ) -> Result<Vec<String>, Failure> {
        let module = module_name(path);
        let mut found = BTreeSet::new();
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT path FROM refs WHERE workspace_id = ?1 AND name = ?2
             ORDER BY path LIMIT 4096",
        )?;
        for row in
            statement.query_map(params![workspace_id, name], |row| row.get::<_, String>(0))?
        {
            found.insert(row?);
        }
        for (file, target) in imports {
            let last = import_tail(target);
            if last.is_some_and(|last| last == name || Some(last) == module) {
                found.insert(file.clone());
            }
        }
        Ok(found
            .into_iter()
            .filter(|file| among.is_none_or(|among| among.contains(file)))
            .take(limit)
            .collect())
    }

    /// The import targets of the workspace's files in `among`, or of all of
    /// them (the first 20,000), as `(path, target)`.
    pub fn imports_of(
        &self,
        workspace_id: &str,
        among: Option<&BTreeSet<String>>,
    ) -> Result<Vec<(String, String)>, Failure> {
        Ok(match among {
            Some(among) => {
                let mut statement = self.connection.prepare_cached(
                    "SELECT path, target FROM imports WHERE workspace_id = ?1 AND path = ?2",
                )?;
                let mut imports = Vec::new();
                for file in among {
                    for row in statement.query_map(params![workspace_id, file], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })? {
                        imports.push(row?);
                    }
                }
                imports
            }
            None => self
                .connection
                .prepare_cached(
                    "SELECT path, target FROM imports WHERE workspace_id = ?1
                     ORDER BY path LIMIT 20000",
                )?
                .query_map([workspace_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?,
        })
    }

    /// Write what the index holds for `of` to `out` in the export format
    /// (`crate::index_file`): only files indexed at some version, in path
    /// order, rows in the order they were stored. Signatures are the only
    /// source text in it. Stops with an error past `index_file::MAX_FILE_BYTES`
    /// or at `deadline`.
    pub fn export(
        &self,
        of: &crate::daemon::Registration,
        out: &mut impl std::io::Write,
        now_ms: u64,
        deadline: Instant,
    ) -> Result<Exported, Failure> {
        use crate::index_file::{CallRow, FileRecord, Header, ImportRow, NameLine, SymbolRow};
        let workspace_id = of.workspace_id.as_str();
        // One read snapshot. This connection is the only writer, and it is
        // busy here, so the snapshot is also the latest state.
        let snapshot = self.connection.unchecked_transaction()?;
        if !self.indexed_as(workspace_id, &of.root, of.revision)? {
            return Err(format!(
                "the index holds nothing for workspace {workspace_id} yet; wait for its first scan and try again"
            )
            .into());
        }
        let mut exported = Exported {
            files: 0,
            partial_files: 0,
            bytes: 0,
        };
        write_line(
            out,
            &mut exported.bytes,
            &Header {
                format: crate::index_file::FORMAT.into(),
                schema: SCHEMA,
                workspace_root: of
                    .root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                created_at: now_ms,
            },
        )?;
        let files: Vec<(String, String, bool)> = self
            .connection
            .prepare_cached(
                "SELECT path, digest, partial FROM files
                 WHERE workspace_id = ?1 AND digest IS NOT NULL ORDER BY path",
            )?
            .query_map([workspace_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        let (mut files_written, mut partial_files) = (0u32, 0u32);
        for (path, digest, partial) in files {
            if Instant::now() >= deadline {
                return Err("the export did not finish in time; nothing usable was written".into());
            }
            let key = params![workspace_id, path];
            let symbols = self
                .connection
                .prepare_cached(
                    "SELECT name, kind, line, end_line, signature, exported, body_digest
                     FROM symbols WHERE workspace_id = ?1 AND path = ?2 ORDER BY rowid",
                )?
                .query_map(key, |row| {
                    Ok(SymbolRow {
                        name: row.get(0)?,
                        kind: row.get(1)?,
                        line: row.get(2)?,
                        end_line: row.get(3)?,
                        signature: row.get(4)?,
                        exported: row.get(5)?,
                        body_digest: row.get(6)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            let refs = self
                .connection
                .prepare_cached(
                    "SELECT name, line FROM refs WHERE workspace_id = ?1 AND path = ?2 ORDER BY rowid",
                )?
                .query_map(key, |row| {
                    Ok(NameLine {
                        name: row.get(0)?,
                        line: row.get(1)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            let imports = self
                .connection
                .prepare_cached(
                    "SELECT target, line FROM imports
                     WHERE workspace_id = ?1 AND path = ?2 ORDER BY rowid",
                )?
                .query_map(key, |row| {
                    Ok(ImportRow {
                        target: row.get(0)?,
                        line: row.get(1)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            let calls = self
                .connection
                .prepare_cached(
                    "SELECT caller, callee, line, args FROM calls
                     WHERE workspace_id = ?1 AND path = ?2 ORDER BY rowid",
                )?
                .query_map(key, |row| {
                    Ok(CallRow {
                        caller: row.get(0)?,
                        callee: row.get(1)?,
                        line: row.get(2)?,
                        args: row.get(3)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            write_line(
                out,
                &mut exported.bytes,
                &FileRecord {
                    path,
                    digest,
                    partial,
                    symbols,
                    refs,
                    imports,
                    calls,
                },
            )?;
            files_written += 1;
            partial_files += u32::from(partial);
        }
        snapshot.commit()?;
        exported.files = files_written;
        exported.partial_files = partial_files;
        Ok(exported)
    }

    /// Read an export from `input` into the index for `of`. A file's rows
    /// are loaded only when its exported digest is the SHA-256 of the file
    /// on disk now, the last complete scan `scan` lists it, and the index
    /// holds no version of it yet: an imported row never describes content
    /// the local copy does not have, and never replaces what this index
    /// already parsed. Everything else is left to the indexer. Loading is
    /// a first indexing: it reports no interface change. Without `apply`
    /// nothing is written. A malformed header refuses the whole import; a
    /// bad file line is skipped and counted. All or nothing: an error, or
    /// `deadline`, leaves the index as it was.
    pub fn import(
        &mut self,
        of: &crate::daemon::Registration,
        scan: &ScanView,
        input: &mut impl std::io::BufRead,
        apply: bool,
        now_ms: u64,
        deadline: Instant,
    ) -> Result<Imported, Failure> {
        use crate::index_file::{
            Line, MAX_LINE_BYTES, Seen, Skip, parse_header, parse_record, read_line,
        };
        let workspace_id = of.workspace_id.as_str();
        let mut line = Vec::new();
        match read_line(input, &mut line, MAX_LINE_BYTES)? {
            Line::Read => parse_header(&line, SCHEMA)?,
            Line::TooLong => {
                return Err("the first line is not an index export header: it is too long".into());
            }
            Line::End => return Err("the file is empty: it has no index export header".into()),
        };
        if apply {
            self.start_again_at(workspace_id, &of.root, of.revision)?;
        }
        // What this registration's index already holds a version of. Before
        // its first pass (only possible in a dry run), nothing.
        let held: BTreeSet<String> = if self.indexed_as(workspace_id, &of.root, of.revision)? {
            self.connection
                .prepare_cached(
                    "SELECT path FROM files WHERE workspace_id = ?1 AND digest IS NOT NULL",
                )?
                .query_map([workspace_id], |row| row.get(0))?
                .collect::<Result<_, _>>()?
        } else {
            BTreeSet::new()
        };
        let transaction = self.connection.transaction()?;
        let mut counts = Imported::default();
        let mut seen = Seen::default();
        loop {
            if Instant::now() >= deadline {
                return Err("the import did not finish in time; the index is unchanged".into());
            }
            let record = match read_line(input, &mut line, MAX_LINE_BYTES)? {
                Line::End => break,
                Line::TooLong => {
                    counts.over_limits += 1;
                    continue;
                }
                Line::Read if line.iter().all(u8::is_ascii_whitespace) => continue,
                Line::Read => match parse_record(&line) {
                    Ok(record) => record,
                    Err(Skip::OverLimits(_)) => {
                        counts.over_limits += 1;
                        continue;
                    }
                    Err(Skip::Malformed(_)) => {
                        counts.malformed += 1;
                        continue;
                    }
                },
            };
            if !seen.first(&record.path) {
                counts.malformed += 1;
                continue;
            }
            if held.contains(&record.path) {
                counts.kept += 1;
                continue;
            }
            let Some(scanned) = scan.files.get(&record.path) else {
                counts.different += 1;
                continue;
            };
            if read_exact_version(&of.root, &record.path, &record.digest, MAX_SOURCE_BYTES)
                .is_none()
            {
                counts.different += 1;
                continue;
            }
            counts.loaded += 1;
            if apply {
                load(&transaction, workspace_id, scanned, &record, now_ms)?;
            }
        }
        if apply {
            transaction.commit()?;
        }
        Ok(counts)
    }

    /// Run `reads` and measure coverage as one answer, from one read
    /// snapshot of the index. The indexer commits one file at a time, so
    /// separate reads could pair results from before a commit with coverage
    /// from after it and claim more than the results hold. This is the only
    /// way to get coverage, so every answer's coverage describes its rows.
    ///
    /// The answer is for `of`, the workspace's current registration. Until
    /// the indexer has started that registration's index, what the index
    /// holds is of an earlier one (another root, or before a removal):
    /// `reads` is not run, and the answer is empty and not scanned.
    pub fn with_coverage<T: Default>(
        &self,
        of: &crate::daemon::Registration,
        reads: impl FnOnce(&Self) -> Result<T, Failure>,
    ) -> Result<(T, GraphCoverage), Failure> {
        // A deferred transaction in WAL mode reads one snapshot, fixed at
        // its first read, until it ends. It only reads, so ending it on an
        // error by dropping it (a rollback) loses nothing.
        let snapshot = self.connection.unchecked_transaction()?;
        if !self.indexed_as(&of.workspace_id, &of.root, of.revision)? {
            snapshot.commit()?;
            return Ok((T::default(), GraphCoverage::default()));
        }
        let answer = reads(self)?;
        let coverage = self.coverage(&of.workspace_id)?;
        snapshot.commit()?;
        Ok((answer, coverage))
    }

    fn coverage(&self, workspace_id: &str) -> Result<GraphCoverage, Failure> {
        // Blind: partial, or not indexed at its scanned content (waiting or
        // too large), or not read by the last scan at all. A file the scan
        // did not read keeps its earlier row, so it is counted once.
        let (indexable, current, partial, blind_indexed): (u32, u32, u32, u32) = self
            .connection
            .query_row(
            "SELECT count(*), coalesce(sum(digest IS scan_digest), 0), coalesce(sum(partial), 0),
                    coalesce(sum((partial = 1 OR digest IS NOT scan_digest) AND NOT EXISTS (
                        SELECT 1 FROM unread u WHERE u.workspace_id = f.workspace_id
                        AND u.path = f.path)), 0)
                 FROM files f WHERE workspace_id = ?1",
            [workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let unread: Option<(u32, u32)> = self
            .connection
            .query_row(
                "SELECT too_large, unscanned FROM scan_summary WHERE workspace_id = ?1",
                [workspace_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (too_large, unscanned) = unread.unwrap_or_default();
        Ok(GraphCoverage {
            indexable_files: indexable,
            current_files: current,
            partial_files: partial,
            // A complete scan with no supported file is still a scan.
            scanned: indexable > 0 || unread.is_some(),
            blind_in_scope: blind_indexed
                .saturating_add(too_large)
                .saturating_add(unscanned),
        })
    }

    /// Where the index is blind, from the same snapshot as its coverage
    /// when called inside `with_coverage`. At most `limit` paths per list,
    /// in path order, so a capped list is the same on every call.
    pub fn blind_spots(
        &self,
        workspace_id: &str,
        limit: u32,
    ) -> Result<langspan_core::BlindSpots, Failure> {
        let connection = &self.connection;
        let paths = |sql: &str| -> Result<(Vec<String>, u32), Failure> {
            let listed = connection
                .prepare_cached(&format!("SELECT path FROM ({sql}) ORDER BY path LIMIT ?2"))?
                .query_map(params![workspace_id, limit], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let count = connection
                .prepare_cached(&format!("SELECT count(*) FROM ({sql})"))?
                .query_row(params![workspace_id], |row| row.get(0))?;
            Ok((listed, count))
        };
        // Rows the last scan still read; a file it did not read is listed
        // by why it was not read, not by what its earlier row says.
        const READ: &str = "workspace_id = ?1 AND path NOT IN
            (SELECT path FROM unread WHERE workspace_id = ?1)";
        let (never_clean, never_clean_count) = paths(&format!(
            "SELECT path FROM files WHERE {READ} AND digest IS NOT NULL AND baseline_digest IS NULL"
        ))?;
        let (waiting, waiting_count) = paths(&format!(
            "SELECT path FROM files WHERE {READ} AND digest IS NOT scan_digest
             AND oversize_digest IS NOT scan_digest"
        ))?;
        let (too_large, too_large_count) = paths(&format!(
            "SELECT path FROM files WHERE {READ} AND oversize_digest IS scan_digest
             AND digest IS NOT scan_digest
             UNION ALL SELECT path FROM unread WHERE workspace_id = ?1 AND reason = 'too_large'"
        ))?;
        let (unscanned, _) =
            paths("SELECT path FROM unread WHERE workspace_id = ?1 AND reason = 'unscanned'")?;
        let partial: Vec<langspan_core::PartialFile> = connection
            .prepare_cached(&format!(
                "SELECT path, partial_since FROM files WHERE {READ} AND partial = 1
                 ORDER BY path LIMIT ?2"
            ))?
            .query_map(params![workspace_id, limit], |row| {
                Ok(langspan_core::PartialFile {
                    path: row.get(0)?,
                    since_ms: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u64,
                })
            })?
            .collect::<Result<_, _>>()?;
        let partial_count: u32 = connection
            .prepare_cached(&format!(
                "SELECT count(*) FROM files WHERE {READ} AND partial = 1"
            ))?
            .query_row(params![workspace_id], |row| row.get(0))?;
        // Past the stored paths, the scan's full counts.
        let summary: Option<(u32, u32, String)> = connection
            .query_row(
                "SELECT too_large, unscanned, languages FROM scan_summary WHERE workspace_id = ?1",
                [workspace_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (scan_too_large, scan_unscanned, languages) = summary.unwrap_or_default();
        let stored_too_large: u32 = connection.query_row(
            "SELECT count(*) FROM unread WHERE workspace_id = ?1 AND reason = 'too_large'",
            [workspace_id],
            |row| row.get(0),
        )?;
        let mut unsupported_languages: Vec<langspan_core::LanguageCount> =
            serde_json::from_str::<BTreeMap<String, u32>>(&languages)
                .unwrap_or_default()
                .into_iter()
                .map(|(extension, files)| langspan_core::LanguageCount { extension, files })
                .collect();
        unsupported_languages.sort_by(|a, b| {
            b.files
                .cmp(&a.files)
                .then_with(|| a.extension.cmp(&b.extension))
        });
        Ok(langspan_core::BlindSpots {
            partial,
            never_clean,
            waiting,
            too_large,
            unscanned,
            unsupported_languages,
            totals: langspan_core::BlindTotals {
                partial: partial_count,
                never_clean: never_clean_count,
                waiting: waiting_count,
                too_large: too_large_count
                    .saturating_sub(stored_too_large)
                    .saturating_add(scan_too_large),
                unscanned: scan_unscanned,
            },
        })
    }

    fn symbols_where(
        &self,
        clause: &str,
        workspace_id: &str,
        value: &str,
        limit: u32,
    ) -> Result<Vec<SymbolRef>, Failure> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT s.path, s.name, s.kind, s.line, s.end_line, s.signature, s.exported, f.digest
             FROM symbols s JOIN files f ON f.workspace_id = s.workspace_id AND f.path = s.path
             WHERE s.workspace_id = ?1 AND {clause} ORDER BY s.path, s.line LIMIT ?3"
        ))?;
        let rows = statement
            .query_map(params![workspace_id, value, limit], |row| {
                Ok(SymbolRef {
                    path: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    line: row.get(3)?,
                    end_line: row.get(4)?,
                    signature: row.get(5)?,
                    exported: row.get(6)?,
                    digest: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    ..SymbolRef::default()
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Add what the index knows around one definition: the digest of its
    /// own bytes, the names called within its lines, and the call sites of
    /// its name outside it. All by name; nothing is resolved.
    pub fn add_detail(&self, workspace_id: &str, symbol: &mut SymbolRef) -> Result<(), Failure> {
        let connection = &self.connection;
        let (path, line, end) = (&symbol.path, symbol.line, symbol.end_line);
        symbol.body_digest = connection
            .prepare_cached(
                "SELECT body_digest FROM symbols WHERE workspace_id = ?1 AND path = ?2
                 AND line = ?3 AND end_line = ?4 AND name = ?5 AND kind = ?6",
            )?
            .query_row(
                params![workspace_id, path, line, end, symbol.name, symbol.kind],
                |row| row.get(0),
            )
            .optional()?;
        let calls = connection
            .prepare_cached(
                "SELECT callee, count(*), min(line) FROM calls
                 WHERE workspace_id = ?1 AND path = ?2 AND line BETWEEN ?3 AND ?4
                 GROUP BY callee ORDER BY min(line), callee LIMIT ?5",
            )?
            .query_map(
                params![
                    workspace_id,
                    path,
                    line,
                    end,
                    langspan_core::MAX_SYMBOL_CALLS
                ],
                |row| {
                    Ok(langspan_core::CallCount {
                        name: row.get(0)?,
                        count: row.get(1)?,
                        line: row.get(2)?,
                    })
                },
            )?
            .collect::<Result<_, _>>()?;
        // A call within the definition itself (recursion) is not a caller.
        const OUTSIDE: &str = "FROM calls WHERE workspace_id = ?1 AND callee = ?5
             AND NOT (path = ?2 AND line BETWEEN ?3 AND ?4)";
        let count: u32 = connection
            .prepare_cached(&format!("SELECT count(*) {OUTSIDE}"))?
            .query_row(params![workspace_id, path, line, end, symbol.name], |row| {
                row.get(0)
            })?;
        let limit = langspan_core::MAX_CALLER_SITES;
        let sites = connection
            .prepare_cached(&format!(
                "SELECT path, line {OUTSIDE} ORDER BY path, line LIMIT ?6"
            ))?
            .query_map(
                params![workspace_id, path, line, end, symbol.name, limit],
                |row| {
                    Ok(format!(
                        "{}:{}",
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?
                    ))
                },
            )?
            .collect::<Result<_, _>>()?;
        symbol.calls = Some(calls);
        symbol.callers = Some(langspan_core::Callers { count, sites });
        Ok(())
    }

    /// Definitions named `name`, at most `limit + 1` so truncation shows.
    pub fn symbol(
        &self,
        workspace_id: &str,
        name: &str,
        limit: u32,
    ) -> Result<Vec<SymbolRef>, Failure> {
        self.symbols_where("s.name = ?2", workspace_id, name, limit + 1)
    }

    /// Definitions in the file `path`, in line order, at most `limit`.
    pub fn symbols_in(
        &self,
        workspace_id: &str,
        path: &str,
        limit: u32,
    ) -> Result<Vec<SymbolRef>, Failure> {
        self.symbols_where("s.path = ?2", workspace_id, path, limit)
    }

    /// Mentions of `name`. `except` leaves one file out.
    pub fn references(
        &self,
        workspace_id: &str,
        name: &str,
        except: Option<&str>,
        limit: u32,
    ) -> Result<Vec<NameRef>, Failure> {
        let mut statement = self.connection.prepare_cached(
            "SELECT r.path, r.name, r.line, f.digest FROM refs r
             JOIN files f ON f.workspace_id = r.workspace_id AND f.path = r.path
             WHERE r.workspace_id = ?1 AND r.name = ?2 AND r.path != ?3
             ORDER BY r.path, r.line LIMIT ?4",
        )?;
        let rows = statement
            .query_map(
                params![workspace_id, name, except.unwrap_or(""), limit + 1],
                |row| {
                    Ok(NameRef {
                        path: row.get(0)?,
                        name: row.get(1)?,
                        line: row.get(2)?,
                        digest: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Distinct files that mention `name`, in path order, at most `limit + 1`
    /// so truncation shows.
    pub fn mentioned_in(
        &self,
        workspace_id: &str,
        name: &str,
        limit: u32,
    ) -> Result<Vec<String>, Failure> {
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT path FROM refs WHERE workspace_id = ?1 AND name = ?2
             ORDER BY path LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![workspace_id, name, limit + 1], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(rows)
    }

    /// What `path` exports, and where else those names are mentioned.
    pub fn impact(
        &self,
        workspace_id: &str,
        path: &str,
        limit: u32,
    ) -> Result<(Vec<SymbolRef>, Vec<NameRef>), Failure> {
        let symbols: Vec<SymbolRef> = self.symbols_where(
            "s.path = ?2 AND s.exported = 1",
            workspace_id,
            path,
            limit + 1,
        )?;
        let mut references = Vec::new();
        let mut names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            let room = (limit + 1).saturating_sub(references.len() as u32);
            if room == 0 {
                break;
            }
            references.extend(self.references(workspace_id, name, Some(path), room - 1)?);
        }
        Ok((symbols, references))
    }

    /// The files reached from `path` level by level, up to `depth` levels
    /// (roadmap D2). Level n+1 is the files mentioning an exported name of
    /// a level-n file; each file appears once, at its first level, with the
    /// name (`via`) and the previous-level file (`from`) that reached it.
    /// At most `MAX_RESULTS` files in all and `NAMES_PER_LEVEL` names
    /// expanded per level; a level a bound cut is `truncated`. Past
    /// `deadline` the levels so far are returned, the last one `truncated`.
    /// Call it inside `with_coverage` so the levels and the coverage are one
    /// snapshot.
    pub fn impact_levels(
        &self,
        workspace_id: &str,
        path: &str,
        depth: u32,
        deadline: Instant,
    ) -> Result<Vec<langspan_core::ImpactLevel>, Failure> {
        let limit = MAX_RESULTS as usize;
        let mut seen = BTreeSet::from([path.to_owned()]);
        let mut frontier = vec![path.to_owned()];
        let mut levels = Vec::new();
        for level in 1..=depth {
            let mut files: Vec<langspan_core::ImpactFile> = Vec::new();
            let mut truncated = false;
            let mut late = false;
            let mut expanded = BTreeSet::new();
            'level: for from in &frontier {
                for name in self.exported_names(workspace_id, from)? {
                    if expanded.contains(&name) {
                        continue;
                    }
                    if expanded.len() == NAMES_PER_LEVEL {
                        truncated = true;
                        break 'level;
                    }
                    if Instant::now() >= deadline {
                        (truncated, late) = (true, true);
                        break 'level;
                    }
                    let mentions = self.mentioned_in(workspace_id, &name, MAX_RESULTS)?;
                    truncated |= mentions.len() > limit;
                    for mention in mentions.into_iter().take(limit) {
                        if seen.contains(&mention) {
                            continue;
                        }
                        // `seen` holds the queried file too: `limit` files
                        // have been reached.
                        if seen.len() > limit {
                            truncated = true;
                            break 'level;
                        }
                        seen.insert(mention.clone());
                        files.push(langspan_core::ImpactFile {
                            path: mention,
                            via: name.clone(),
                            from: from.clone(),
                        });
                    }
                    expanded.insert(name);
                }
            }
            frontier = files.iter().map(|file| file.path.clone()).collect();
            levels.push(langspan_core::ImpactLevel {
                level,
                files,
                truncated,
            });
            if late {
                break;
            }
        }
        Ok(levels)
    }

    /// The distinct exported names of `path`, in name order.
    fn exported_names(&self, workspace_id: &str, path: &str) -> Result<Vec<String>, Failure> {
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT name FROM symbols WHERE workspace_id = ?1 AND path = ?2
             AND exported = 1 ORDER BY name",
        )?;
        let rows = statement
            .query_map(params![workspace_id, path], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(rows)
    }
}

/// Exported names one `impact_levels` level expands at most.
pub const NAMES_PER_LEVEL: usize = 50;

/// For each call, in the same order, the name of the innermost definition
/// whose lines hold its line. Definitions nest, so one sweep in line order
/// with a stack of the open ones finds it.
fn innermost<'a>(symbols: &'a [Symbol], calls: &[langspan_graph::Call]) -> Vec<Option<&'a str>> {
    let mut definitions: Vec<&Symbol> = symbols.iter().collect();
    definitions.sort_by_key(|symbol| (symbol.line, std::cmp::Reverse(symbol.end_line)));
    let mut definitions = definitions.into_iter().peekable();
    // A callee's name can sit on a later line than a call nested in its
    // receiver, so calls are visited in line order.
    let mut order: Vec<usize> = (0..calls.len()).collect();
    order.sort_by_key(|&index| calls[index].line);
    let mut open: Vec<&Symbol> = Vec::new();
    let mut found = vec![None; calls.len()];
    for index in order {
        let line = calls[index].line;
        while let Some(symbol) = definitions.next_if(|symbol| symbol.line <= line) {
            while open.last().is_some_and(|top| top.end_line < symbol.line) {
                open.pop();
            }
            open.push(symbol);
        }
        while open.last().is_some_and(|top| top.end_line < line) {
            open.pop();
        }
        found[index] = open.last().map(|symbol| symbol.name.as_str());
    }
    found
}

/// Check records a detailed lookup compares, newest conclusive first.
pub const LAST_CHECK_RECORDS: usize = 8;

/// For each of `files` (path to indexed digest), the newest conclusive check
/// record whose tree holds exactly those bytes at that path. The bytes are
/// read again from `root` and used only if they still have the indexed
/// digest; they are compared as a raw Git blob, so a file Git stores through
/// a clean filter is never matched. One read-only `git ls-tree` per record,
/// at most `LAST_CHECK_RECORDS`, stopping at `deadline`. A path not found
/// by then has no entry: absence claims nothing.
pub fn last_checks(
    root: &Path,
    files: &BTreeMap<String, String>,
    records: &[langspan_core::CheckRecord],
    deadline: Instant,
) -> BTreeMap<String, langspan_core::LastCheck> {
    use langspan_core::CheckResult;
    // Blob IDs in both object formats, so the file is read only once.
    let blobs: BTreeMap<&str, [Option<String>; 2]> = files
        .iter()
        .take_while(|_| Instant::now() < deadline)
        .filter_map(|(path, digest)| {
            let bytes = read_exact_version(root, path, digest, MAX_SOURCE_BYTES)?;
            let id = |length| crate::observer::blob_id(&bytes, length);
            Some((path.as_str(), [id(40), id(64)]))
        })
        .collect();
    let mut found = BTreeMap::new();
    let conclusive = records.iter().filter_map(|record| {
        let result = match record.result {
            CheckResult::Passed {} => "passed",
            CheckResult::Failed { .. } => "failed",
            CheckResult::Inconclusive {} => return None,
        };
        Some((record, result))
    });
    for (record, result) in conclusive.take(LAST_CHECK_RECORDS) {
        let tree = record.tree_oid.as_str();
        let pending: Vec<&str> = blobs
            .keys()
            .copied()
            .filter(|path| !found.contains_key(*path))
            .collect();
        if pending.is_empty() || Instant::now() >= deadline {
            break;
        }
        let format = match tree.len() {
            40 => 0,
            64 => 1,
            _ => continue,
        };
        // A tree ID from the record is passed to Git only as hex.
        if !tree.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let mut args = vec![
            "--literal-pathspecs",
            "ls-tree",
            "--full-tree",
            "-z",
            tree,
            "--",
        ];
        args.extend(&pending);
        let Ok(listing) = crate::git::output(root, &args, deadline, 1 << 20) else {
            continue;
        };
        // Each entry: `MODE SP TYPE SP OID TAB PATH NUL`.
        for entry in listing.split(|byte| *byte == 0) {
            let entry = String::from_utf8_lossy(entry);
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            let mut meta = meta.split(' ');
            let (Some(_mode), Some("blob"), Some(oid)) = (meta.next(), meta.next(), meta.next())
            else {
                continue;
            };
            let matches = blobs
                .get(path)
                .is_some_and(|ids| ids[format].as_deref() == Some(oid));
            if matches && !found.contains_key(path) {
                found.insert(
                    path.to_owned(),
                    langspan_core::LastCheck {
                        name: record.name.clone(),
                        result: result.into(),
                        tree: tree.into(),
                    },
                );
            }
        }
    }
    found
}

/// Write `value` as one JSON line, counting `bytes`, and refuse past the
/// most an import reads.
fn write_line(
    out: &mut impl std::io::Write,
    bytes: &mut u64,
    value: &impl serde::Serialize,
) -> Result<(), Failure> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    *bytes += line.len() as u64;
    if *bytes > crate::index_file::MAX_FILE_BYTES {
        return Err(format!(
            "the export would be larger than {} MiB, the most an import reads",
            crate::index_file::MAX_FILE_BYTES / (1024 * 1024)
        )
        .into());
    }
    out.write_all(&line)?;
    Ok(())
}

/// Write one checked file record as a first indexing of `path` would: its
/// rows, its version, and for a clean parse its interface baseline.
/// `scanned` is the digest the last scan recorded for the path.
fn load(
    transaction: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    scanned: &str,
    record: &crate::index_file::FileRecord,
    now_ms: u64,
) -> Result<(), Failure> {
    let path = record.path.as_str();
    for table in ["symbols", "refs", "imports", "calls", "baselines"] {
        transaction.execute(
            &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND path = ?2"),
            params![workspace_id, path],
        )?;
    }
    // Indexed now: no read of it waits for a retry.
    transaction.execute(
        "DELETE FROM retries WHERE workspace_id = ?1 AND path = ?2 AND stage = ?3",
        params![workspace_id, path, Retries::INDEX],
    )?;
    transaction.execute(
        "INSERT INTO files(workspace_id, path, scan_digest, digest, partial, baseline_digest,
            partial_since, oversize_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, CASE WHEN ?5 THEN NULL ELSE ?4 END,
            CASE WHEN ?5 THEN ?6 ELSE NULL END, NULL)
         ON CONFLICT(workspace_id, path) DO UPDATE SET scan_digest = excluded.scan_digest,
            digest = excluded.digest, partial = excluded.partial,
            baseline_digest = excluded.baseline_digest, partial_since = excluded.partial_since,
            oversize_digest = NULL",
        params![
            workspace_id,
            path,
            scanned,
            record.digest,
            record.partial,
            now_ms as i64
        ],
    )?;
    for symbol in &record.symbols {
        transaction.execute(
            "INSERT INTO symbols VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                workspace_id,
                path,
                symbol.name,
                symbol.kind,
                symbol.line,
                symbol.end_line,
                symbol.signature,
                symbol.exported,
                symbol.body_digest
            ],
        )?;
        if !record.partial && symbol.exported {
            transaction.execute(
                // An export carries no block digest, so an imported
                // baseline has none: its next rename is told as a removal
                // and an addition, as for a baseline from before B10.
                "INSERT INTO baselines(workspace_id, path, name, kind, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    workspace_id,
                    path,
                    symbol.name,
                    symbol.kind,
                    symbol.signature
                ],
            )?;
        }
    }
    for call in &record.calls {
        transaction.execute(
            "INSERT INTO calls VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                workspace_id,
                path,
                call.caller,
                call.callee,
                call.line,
                call.args
            ],
        )?;
    }
    for reference in &record.refs {
        transaction.execute(
            "INSERT INTO refs VALUES (?1, ?2, ?3, ?4)",
            params![workspace_id, path, reference.name, reference.line],
        )?;
    }
    for import in &record.imports {
        transaction.execute(
            "INSERT INTO imports VALUES (?1, ?2, ?3, ?4)",
            params![workspace_id, path, import.target, import.line],
        )?;
    }
    Ok(())
}

/// The file's bytes, only if they are still exactly the scanned version and
/// at most `max` long.
fn read_exact_version(root: &Path, path: &str, digest: &str, max: usize) -> Option<Vec<u8>> {
    let full = root.join(path);
    if !fs::symlink_metadata(&full).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&full)
        .ok()?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= max && crate::sha256::hex(&crate::sha256::digest(&bytes)) == digest)
        .then_some(bytes)
}

/// The structure of `path`'s blob `object` at the worktree's `HEAD`, read
/// raw from Git (never through filters), within the parse bound. `None`
/// when it cannot be parsed completely, including a blob over the parse
/// bound: that holds for this blob on every pass, so nothing is said of the
/// file at this version (review comment 4705138339). `HeadUnread` when Git
/// did not give the blob for another reason: then it is tried again.
fn head_graph(
    root: &Path,
    path: &str,
    object: &str,
    deadline: Instant,
) -> Result<Option<FileGraph>, HeadUnread> {
    let Some(language) = Language::for_path(path) else {
        return Ok(None);
    };
    if object.len() < 40 || object.len() > 64 || !object.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let cat_file = |args: &[&str], limit| {
        let mut command = vec![
            "--no-replace-objects",
            "-c",
            "core.fsmonitor=false",
            "cat-file",
        ];
        command.extend(args);
        crate::git::output(root, &command, deadline, limit)
    };
    let bytes = match cat_file(&["blob", object], MAX_SOURCE_BYTES) {
        Ok(bytes) => bytes,
        Err(_) => {
            // Only Git's own word on the blob's size makes it terminal: a
            // read that failed for another reason may pass later.
            let size = cat_file(&["-s", object], 64).map_err(|_| HeadUnread)?;
            let size = String::from_utf8_lossy(&size).trim().parse::<u64>();
            return match size {
                Ok(size) if size > MAX_SOURCE_BYTES as u64 => Ok(None),
                _ => Err(HeadUnread),
            };
        }
    };
    let graph = langspan_graph::extract(language, &bytes).ok();
    Ok(graph.filter(|graph| !graph.partial))
}

/// A signature of `path`'s language as `added_or_changed` compares it: its
/// tokens, as a reformat is (`crate::reformat::tokens`), so spacing between
/// two tokens never counts and never joins them. Every indexed language
/// is split into words: Go and Java by the TypeScript and JavaScript rules
/// (words, quoted and backtick literals, `//` and `/* */` comments), as
/// the rules for other files drop all whitespace, and `chan int` would
/// then equal `chanint` (review comment 4704678366).
fn signature_tokens(path: &str, signature: &str) -> Vec<Vec<u8>> {
    use crate::reformat::Language as Tokens;
    let language = match Language::for_path(path) {
        Some(Language::Rust) => Tokens::Rust,
        Some(Language::Python) => Tokens::Python,
        Some(Language::TypeScript | Language::Tsx | Language::Go | Language::Java) => {
            Tokens::Script
        }
        None => crate::reformat::language(path),
    };
    crate::reformat::tokens(signature.as_bytes(), language)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect()
}

/// One in-flight symbol as the index holds it: `(path, name, kind,
/// signature)`.
pub type InflightRow = (String, String, String, String);

/// Git did not give `head_graph` the blob: it timed out, failed or the
/// object is missing. Unlike a version that parses partially, this may
/// pass, so the comparison is tried again, within `Retries`' bound.
struct HeadUnread;

/// The file versions of one workspace whose read for one stage failed in
/// a way that may pass. Every outcome of reading a file is either terminal
/// for that version (indexed, parsed partially, too large, no parser: told
/// or remembered, and not read again until the file or `HEAD` changes) or
/// recorded here. A recorded version is tried again after a wait that
/// doubles with each failure, counted in the workspace's passes, and given
/// up after `ATTEMPTS` failures until it changes. Versions not tried yet go
/// before every retry, so no set of failing files holds a pass's budget
/// from the others (review comment 4705138339).
struct Retries {
    workspace_id: String,
    stage: &'static str,
    pass: i64,
    /// Each path's failing version, its failures and the pass it may next
    /// be tried at.
    by_path: BTreeMap<String, (String, u32, i64)>,
}

impl Retries {
    /// Reading the working file to index it.
    const INDEX: &'static str = "index";
    /// Reading the `HEAD` blob to compare it with the indexed version.
    const COMPARE: &'static str = "compare";
    /// Failures after which a version is given up on until it changes.
    /// With the waits below, about 160 passes after the first failure.
    const ATTEMPTS: u32 = 10;
    /// The longest wait between two tries, in passes.
    const MAX_WAIT: i64 = 32;

    fn load(
        connection: &Connection,
        workspace_id: &str,
        stage: &'static str,
        pass: i64,
    ) -> Result<Self, Failure> {
        let by_path = connection
            .prepare_cached(
                "SELECT path, version, failures, next_pass FROM retries
                 WHERE workspace_id = ?1 AND stage = ?2",
            )?
            .query_map(params![workspace_id, stage], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(Self {
            workspace_id: workspace_id.into(),
            stage,
            pass,
            by_path,
        })
    }

    fn paths(&self) -> impl Iterator<Item = &String> {
        self.by_path.keys()
    }

    /// The items to try at this pass, each with its version's failures so
    /// far: first those never tried, in their given order, then the due
    /// retries, fewest failures first. A version waiting or given up on is
    /// left out; a path whose recorded version is another is new.
    fn order<'a, T>(&self, items: impl IntoIterator<Item = (T, &'a str, String)>) -> Vec<(u32, T)> {
        let mut due: Vec<(u32, T)> = items
            .into_iter()
            .filter_map(|(item, path, version)| match self.by_path.get(path) {
                Some((failed, failures, next)) if *failed == version => {
                    (*failures < Self::ATTEMPTS && *next <= self.pass).then_some((*failures, item))
                }
                _ => Some((0, item)),
            })
            .collect();
        due.sort_by_key(|(failures, _)| *failures);
        due
    }

    /// Record one more failure of `version`, which had `failures` before.
    /// Returns whether that was the last try: the version is given up on.
    fn failed(
        &self,
        connection: &Connection,
        path: &str,
        version: &str,
        failures: u32,
    ) -> Result<bool, Failure> {
        let failures = failures.saturating_add(1);
        let wait = (1_i64 << failures.min(16).saturating_sub(1)).min(Self::MAX_WAIT);
        self.write(connection, path, version, failures, self.pass + wait)?;
        Ok(failures >= Self::ATTEMPTS)
    }

    /// Remember `version` as not readable at all, until it changes.
    fn give_up(&self, connection: &Connection, path: &str, version: &str) -> Result<(), Failure> {
        self.write(connection, path, version, Self::ATTEMPTS, self.pass)
    }

    fn write(
        &self,
        connection: &Connection,
        path: &str,
        version: &str,
        failures: u32,
        next_pass: i64,
    ) -> Result<(), Failure> {
        connection.execute(
            "INSERT INTO retries VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace_id, path, stage) DO UPDATE SET version = excluded.version,
                failures = excluded.failures, next_pass = excluded.next_pass",
            params![
                self.workspace_id,
                path,
                self.stage,
                version,
                failures,
                next_pass
            ],
        )?;
        Ok(())
    }

    /// The path was read: nothing of it waits for a retry.
    fn clear(&self, connection: &Connection, path: &str) -> Result<(), Failure> {
        if self.by_path.contains_key(path) {
            connection.execute(
                "DELETE FROM retries WHERE workspace_id = ?1 AND path = ?2 AND stage = ?3",
                params![self.workspace_id, path, self.stage],
            )?;
        }
        Ok(())
    }
}

/// The module name a file is imported by: its file name without the
/// extension, or its directory's name for a Go file and for `mod.rs`,
/// `lib.rs`, `main.rs`, `index.*` and `__init__.py`.
fn module_name(path: &str) -> Option<&str> {
    let mut parts = path.rsplit('/');
    let file = parts.next()?;
    let stem = file.split_once('.').map_or(file, |(stem, _)| stem);
    let by_directory = matches!(stem, "mod" | "lib" | "main" | "index" | "__init__")
        || Language::for_path(path) == Some(Language::Go);
    if by_directory {
        parts.next().filter(|directory| !directory.is_empty())
    } else {
        Some(stem).filter(|stem| !stem.is_empty())
    }
}

/// The last part of an import target as written: `crate::quota`,
/// `"./quota.js"`, `app.quota` and `"example.com/app/quota"` all end in
/// `quota`.
fn import_tail(target: &str) -> Option<&str> {
    const SOURCE: &[&str] = &[
        "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "py", "go", "rs",
    ];
    let target = target.trim().trim_matches(['"', '\'', '`']);
    let parts: Vec<&str> = target
        .split(['/', ':', '.', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        [.., last, extension]
            if SOURCE.contains(extension) && target.ends_with(&format!(".{extension}")) =>
        {
            Some(last)
        }
        [.., last] => Some(last),
        [] => None,
    }
}

/// The indexable part of a complete scan: regular files in a supported
/// language, with the digest the scan recorded.
pub fn indexable(snapshot: &crate::observer::Snapshot) -> BTreeMap<String, String> {
    snapshot
        .files
        .iter()
        .filter(|(path, version)| {
            version.kind == crate::observer::FileKind::Regular && Language::for_path(path).is_some()
        })
        .filter_map(|(path, version)| Some((path.clone(), version.digest.clone()?)))
        .collect()
}

/// Unread files kept by path, per workspace. Past this the scan's counts
/// still hold, but those paths are not named.
pub const MAX_UNREAD: usize = 4096;

/// Extensions of source languages the index has no grammar for. Other
/// files (documentation, data, images) are not counted.
const OTHER_SOURCE: &[&str] = &[
    "c", "cc", "clj", "cljs", "cpp", "cs", "cxx", "dart", "elm", "erl", "ex", "exs", "fs", "fsx",
    "groovy", "h", "hh", "hpp", "hrl", "hs", "jl", "kt", "kts", "lua", "m", "ml", "mli", "mm",
    "nim", "php", "pl", "pm", "r", "rb", "scala", "svelte", "swift", "vue", "zig",
];

/// What the last complete scan listed but the index cannot read. It is kept
/// in the index file, so one read snapshot answers for it and the rows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanBlind {
    /// Supported-language files the scan did not read, the first
    /// `MAX_UNREAD` in path order.
    pub unread: BTreeMap<String, crate::observer::Unread>,
    /// All of them, by reason.
    pub too_large: u32,
    pub unscanned: u32,
    /// Other source files the scan listed, by extension.
    pub languages: BTreeMap<String, u32>,
}

/// One complete scan as the indexer needs it.
#[derive(Debug, Default, Clone)]
pub struct ScanView {
    pub files: BTreeMap<String, String>,
    pub unobserved: BTreeSet<String>,
    pub blind: ScanBlind,
    /// The indexable files that differ from the worktree's `HEAD`, each with
    /// its blob at `HEAD` when `HEAD` holds it (roadmap C7).
    pub dirty: BTreeMap<String, Option<String>>,
}

impl ScanView {
    pub fn of(snapshot: &crate::observer::Snapshot) -> Self {
        use crate::observer::{FileKind, Unread};
        let mut blind = ScanBlind::default();
        for (path, reason) in &snapshot.unread {
            if Language::for_path(path).is_none() {
                continue;
            }
            match reason {
                Unread::TooLarge => blind.too_large += 1,
                Unread::Omitted => blind.unscanned += 1,
            }
            if blind.unread.len() < MAX_UNREAD {
                blind.unread.insert(path.clone(), *reason);
            }
        }
        let listed = snapshot
            .files
            .iter()
            .filter(|(_, version)| version.kind == FileKind::Regular)
            .map(|(path, _)| path)
            .chain(snapshot.unread.keys());
        for path in listed {
            let name = path.rsplit('/').next().unwrap_or(path);
            let Some((_, extension)) = name.rsplit_once('.') else {
                continue;
            };
            let extension = extension.to_ascii_lowercase();
            if OTHER_SOURCE.contains(&extension.as_str()) {
                *blind.languages.entry(format!(".{extension}")).or_default() += 1;
            }
        }
        let files = indexable(snapshot);
        let dirty = snapshot
            .files
            .iter()
            .filter(|(path, version)| version.dirty && files.contains_key(*path))
            .map(|(path, _)| (path.clone(), snapshot.head_objects.get(path).cloned()))
            .collect();
        Self {
            files,
            unobserved: snapshot.unobserved.clone(),
            blind,
            dirty,
        }
    }
}

/// Replace the workspace's record of its last scan's blind spots. The paths
/// are rewritten only when they changed, so a steady workspace costs one
/// read per pass.
fn write_scan_blind(
    transaction: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    blind: &ScanBlind,
) -> Result<(), Failure> {
    use crate::observer::Unread;
    let reason = |reason: &Unread| match reason {
        Unread::TooLarge => "too_large",
        Unread::Omitted => "unscanned",
    };
    let stored: BTreeMap<String, String> = transaction
        .prepare_cached("SELECT path, reason FROM unread WHERE workspace_id = ?1")?
        .query_map([workspace_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let same = stored.len() == blind.unread.len()
        && stored
            .iter()
            .zip(&blind.unread)
            .all(|((a, ra), (b, rb))| a == b && ra == reason(rb));
    if !same {
        transaction.execute("DELETE FROM unread WHERE workspace_id = ?1", [workspace_id])?;
        let mut insert = transaction.prepare_cached("INSERT INTO unread VALUES (?1, ?2, ?3)")?;
        for (path, why) in &blind.unread {
            insert.execute(params![workspace_id, path, reason(why)])?;
        }
    }
    transaction.execute(
        "INSERT INTO scan_summary VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(workspace_id) DO UPDATE SET too_large = excluded.too_large,
            unscanned = excluded.unscanned, languages = excluded.languages",
        params![
            workspace_id,
            blind.too_large,
            blind.unscanned,
            serde_json::to_string(&blind.languages)?
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    use std::time::Duration;

    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn setup(name: &str) -> (Dir, Index) {
        let dir = std::env::temp_dir().join(format!("ls-graph-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        let index = Index::open(&dir.join("graph.sqlite3")).unwrap();
        (Dir(dir), index)
    }

    /// Write files and index the whole set, as a scan would.
    fn pass(dir: &Dir, index: &mut Index, files: &[(&str, &str)]) -> Vec<Notice> {
        pass_at(dir, index, files, NOW, Duration::from_secs(10))
    }

    fn pass_at(
        dir: &Dir,
        index: &mut Index,
        files: &[(&str, &str)],
        now_ms: u64,
        budget: Duration,
    ) -> Vec<Notice> {
        let mut digests = BTreeMap::new();
        for (path, text) in files {
            fs::write(dir.0.join(path), text).unwrap();
            digests.insert(
                (*path).to_owned(),
                crate::sha256::hex(&crate::sha256::digest(text.as_bytes())),
            );
        }
        let scan = ScanView {
            files: digests,
            ..ScanView::default()
        };
        index
            .update("w", &dir.0, 0, &scan, now_ms, Instant::now() + budget)
            .unwrap()
    }

    /// Workspace `w` at `root`, never moved or removed: what `pass` indexes.
    fn of(root: &Path) -> crate::daemon::Registration {
        crate::daemon::Registration {
            workspace_id: "w".into(),
            root: root.to_path_buf(),
            revision: 0,
        }
    }

    fn blind(index: &Index, root: &Path, limit: u32) -> (langspan_core::BlindSpots, GraphCoverage) {
        index
            .with_coverage(&of(root), |index| index.blind_spots("w", limit))
            .unwrap()
    }

    /// Answers are for the workspace's current registration: until the
    /// indexer starts its index, the index of another root or of the
    /// registration before a removal is not read.
    #[test]
    fn an_answer_for_another_registration_reads_nothing() {
        let (dir, mut index) = setup("registration");
        pass(&dir, &mut index, &[("src/api.rs", "pub fn upload() {}\n")]);
        let (symbols, coverage) = index
            .with_coverage(&of(&dir.0), |index| index.symbol("w", "upload", 10))
            .unwrap();
        assert_eq!((symbols.len(), coverage.scanned), (1, true));
        let moved = crate::daemon::Registration {
            root: dir.0.join("elsewhere"),
            ..of(&dir.0)
        };
        // Removed and registered again, or switched away and back.
        let removed = crate::daemon::Registration {
            revision: 1,
            ..of(&dir.0)
        };
        for other in [moved, removed] {
            let (symbols, coverage) = index
                .with_coverage(&other, |index| index.symbol("w", "upload", 10))
                .unwrap();
            assert!(symbols.is_empty(), "{other:?}: {symbols:?}");
            assert_eq!(coverage, GraphCoverage::default(), "{other:?}");
        }
    }

    const BROKEN: (&str, &str) = ("src/broken.rs", "pub fn half( {{{\n");

    #[test]
    fn a_workspace_indexed_at_another_root_starts_again_without_notices() {
        // Roadmap G6: `workspace move --switch` binds the ID to another
        // checkout. Its files differ from the old one's without anyone
        // changing an interface, so the index starts that root afresh.
        let (dir, mut index) = setup("moved-root");
        let api = ("src/api.rs", "pub fn upload() {}\n");
        assert!(pass(&dir, &mut index, &[api, CALLER]).is_empty());
        let other = setup("moved-root-other").0;
        let changed = ("src/api.rs", "pub fn upload(x: u8) {}\n");
        let notices = pass(&other, &mut index, &[changed, CALLER]);
        assert!(notices.is_empty(), "{notices:?}");
        let found = index.symbol("w", "upload", 10).unwrap();
        assert_eq!(found[0].signature, "pub fn upload(x: u8)");
        // At the same root, a changed signature is still a notice.
        let again = ("src/api.rs", "pub fn upload(x: u16) {}\n");
        assert_eq!(pass(&other, &mut index, &[again, CALLER]).len(), 1);
    }

    #[test]
    fn an_index_written_before_roots_were_kept_starts_again_at_its_next_pass() {
        // The previous version's schema-4 file: the same tables without
        // `roots`, holding one checkout's files and baselines.
        let (dir, mut index) = setup("unrooted");
        let api = ("src/api.rs", "pub fn upload() {}\n");
        assert!(pass(&dir, &mut index, &[api, CALLER]).is_empty());
        index.connection.execute_batch("DROP TABLE roots;").unwrap();
        drop(index);
        let mut index = Index::open(&dir.0.join("graph.sqlite3")).unwrap();
        // The workspace was moved before the first pass after the upgrade:
        // the other checkout's differences are not interface changes.
        let other = setup("unrooted-other").0;
        let changed = ("src/api.rs", "pub fn upload(x: u8) {}\n");
        let notices = pass(&other, &mut index, &[changed, CALLER]);
        assert!(notices.is_empty(), "{notices:?}");
        let found = index.symbol("w", "upload", 10).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].signature, "pub fn upload(x: u8)");
        // From then on, a changed signature at that root is a notice.
        let again = ("src/api.rs", "pub fn upload(x: u16) {}\n");
        assert_eq!(pass(&other, &mut index, &[again, CALLER]).len(), 1);
    }

    #[test]
    fn blind_spots_name_partial_never_clean_and_waiting_files() {
        let (dir, mut index) = setup("blind");
        let api = ("src/api.rs", "pub fn upload() {}\n");
        pass(&dir, &mut index, &[api, BROKEN, CALLER]);
        let (spots, coverage) = blind(&index, &dir.0, 10);
        assert_eq!(
            spots.partial,
            [langspan_core::PartialFile {
                path: "src/broken.rs".into(),
                since_ms: NOW
            }]
        );
        assert_eq!(spots.never_clean, ["src/broken.rs"], "no clean parse yet");
        assert!(spots.waiting.is_empty() && spots.too_large.is_empty());
        assert_eq!(coverage.blind_in_scope, 1);

        // Still partial at a later version: partial since the first one.
        let later = ("src/broken.rs", "pub fn half( {{{ x\n");
        pass_at(
            &dir,
            &mut index,
            &[api, later, CALLER],
            NOW + 60_000,
            Duration::from_secs(10),
        );
        assert_eq!(blind(&index, &dir.0, 10).0.partial[0].since_ms, NOW);

        // Fixed: in no list, and the count says so.
        let fixed = ("src/broken.rs", "pub fn half() {}\n");
        pass(&dir, &mut index, &[api, fixed, CALLER]);
        let (spots, coverage) = blind(&index, &dir.0, 10);
        assert_eq!(spots, langspan_core::BlindSpots::default());
        assert_eq!(coverage.blind_in_scope, 0);

        // Changed since the last pass: waiting until a pass indexes it.
        let changed = ("src/api.rs", "pub fn upload(x: u8) {}\n");
        pass_at(
            &dir,
            &mut index,
            &[changed, fixed, CALLER],
            NOW,
            Duration::ZERO,
        );
        let (spots, coverage) = blind(&index, &dir.0, 10);
        assert_eq!(spots.waiting, ["src/api.rs"]);
        assert_eq!((spots.totals.waiting, coverage.blind_in_scope), (1, 1));
        pass(&dir, &mut index, &[changed, fixed, CALLER]);
        assert!(blind(&index, &dir.0, 10).0.waiting.is_empty());
    }

    #[test]
    fn a_file_over_the_parse_bound_is_too_large_and_not_waiting() {
        let (dir, mut index) = setup("oversize");
        let big = format!("// {}\n", "x".repeat(MAX_SOURCE_BYTES));
        pass(&dir, &mut index, &[("src/big.rs", &big), CALLER]);
        let (spots, coverage) = blind(&index, &dir.0, 10);
        assert_eq!(spots.too_large, ["src/big.rs"]);
        assert!(spots.waiting.is_empty(), "{spots:?}");
        assert_eq!(coverage.blind_in_scope, 1);
        // Remembered for that version, so later passes do not read it again.
        let oversize: Option<String> = index
            .connection
            .query_row(
                "SELECT oversize_digest FROM files WHERE path = 'src/big.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(oversize.map(|digest| digest.len()), Some(64));
        // Small again: indexed, and no longer too large.
        pass(
            &dir,
            &mut index,
            &[("src/big.rs", "pub fn small() {}\n"), CALLER],
        );
        let (spots, coverage) = blind(&index, &dir.0, 10);
        assert!(spots.too_large.is_empty(), "{spots:?}");
        assert_eq!(coverage.blind_in_scope, 0);
    }

    #[test]
    fn a_small_scanned_version_is_not_marked_too_large_by_a_later_large_file() {
        let (dir, mut index) = setup("oversize-race");
        let small = "pub fn small() {}\n";
        let digest = crate::sha256::hex(&crate::sha256::digest(small.as_bytes()));
        // Scanned small, then grew past the bound before the index ran.
        let big = format!("// {}\n", "x".repeat(MAX_SOURCE_BYTES));
        fs::write(dir.0.join("src/api.rs"), &big).unwrap();
        let scan = ScanView {
            files: BTreeMap::from([("src/api.rs".to_owned(), digest.clone())]),
            ..ScanView::default()
        };
        let far = Instant::now() + Duration::from_secs(10);
        index.update("w", &dir.0, 0, &scan, NOW, far).unwrap();
        let oversize: Option<String> = index
            .connection
            .query_row(
                "SELECT oversize_digest FROM files WHERE path = 'src/api.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            oversize, None,
            "the large bytes are not the scanned version"
        );
        // Back to the scanned content: that version is indexed.
        fs::write(dir.0.join("src/api.rs"), small).unwrap();
        let far = Instant::now() + Duration::from_secs(10);
        index.update("w", &dir.0, 0, &scan, NOW, far).unwrap();
        let (spots, coverage) = blind(&index, &dir.0, 10);
        assert_eq!(spots, langspan_core::BlindSpots::default());
        assert_eq!(coverage.blind_in_scope, 0);
    }

    #[test]
    fn a_capped_list_is_the_same_on_every_call() {
        let (dir, mut index) = setup("cap");
        let names: Vec<String> = (0..5).map(|n| format!("src/b{n}.rs")).collect();
        let files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), BROKEN.1)).collect();
        pass(&dir, &mut index, &files);
        let first = blind(&index, &dir.0, 2).0;
        assert_eq!(first, blind(&index, &dir.0, 2).0);
        let paths: Vec<&str> = first
            .partial
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["src/b0.rs", "src/b1.rs"]);
        assert_eq!(first.totals.partial, 5);
    }

    #[test]
    fn impact_on_a_file_mentioned_only_by_a_partial_file_counts_one_blind_file() {
        let (dir, mut index) = setup("scope");
        let api = ("src/api.rs", "pub fn lonely() {}\n");
        let user = ("src/user.rs", "fn caller() { lonely(); }\nfn half( {{{\n");
        pass(&dir, &mut index, &[api, user]);
        let ((_, references), coverage) = index
            .with_coverage(&of(&dir.0), |index| index.impact("w", "src/api.rs", 10))
            .unwrap();
        assert_eq!(coverage.partial_files, 1);
        assert_eq!(coverage.blind_in_scope, 1);
        assert!(
            references
                .iter()
                .all(|reference| reference.path == "src/user.rs")
        );
    }

    #[test]
    fn files_the_scan_did_not_read_and_other_languages_come_from_a_real_scan() {
        let (dir, mut index) = setup("scan");
        let root = fs::canonicalize(&dir.0).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&root)
            .status()
            .unwrap();
        assert!(status.success());
        // Path order: the large file first, then two read files, then one
        // past the file bound, then other languages and documentation.
        fs::write(root.join("src/0huge.rs"), "x".repeat(200)).unwrap();
        for name in ["a", "b", "c"] {
            fs::write(root.join(format!("src/{name}.rs")), "pub fn f() {}\n").unwrap();
        }
        fs::create_dir_all(root.join("z")).unwrap();
        for n in 0..10 {
            fs::write(root.join(format!("z/C{n}.kt")), "class C {}\n").unwrap();
        }
        fs::write(root.join("z/README.md"), "notes\n").unwrap();
        let limits = crate::observer::Limits {
            max_files: 2,
            max_file_bytes: 100,
            ..crate::observer::Limits::default()
        };
        let snapshot = crate::observer::scan_with_limits(
            &root,
            Instant::now() + Duration::from_secs(10),
            limits,
        )
        .unwrap();
        let scan = ScanView::of(&snapshot);
        index
            .update(
                "w",
                &root,
                0,
                &scan,
                NOW,
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        let (spots, coverage) = blind(&index, &root, 10);
        assert_eq!(spots.too_large, ["src/0huge.rs"]);
        assert_eq!(spots.unscanned, ["src/c.rs"]);
        assert_eq!(
            spots.unsupported_languages,
            [langspan_core::LanguageCount {
                extension: ".kt".into(),
                files: 10
            }]
        );
        assert_eq!((spots.totals.too_large, spots.totals.unscanned), (1, 1));
        assert_eq!(coverage.blind_in_scope, 2);
    }

    /// A language gains a grammar: its files are indexed and counted in
    /// coverage, so they are no longer reported as another language.
    #[test]
    fn no_indexed_extension_is_reported_as_another_language() {
        for extension in OTHER_SOURCE {
            assert_eq!(
                Language::for_path(&format!("src/a.{extension}")),
                None,
                ".{extension} is indexed"
            );
        }
        assert_eq!(
            Language::for_path("src/main/java/shop/Orders.java"),
            Some(Language::Java)
        );
    }

    const NOW: u64 = 1_700_000_000_000;

    const CALLER: (&str, &str) = ("src/main.rs", "fn main() { api::upload(); api::gate(); }\n");

    #[test]
    fn impact_finds_exports_after_many_private_definitions() {
        let (dir, mut index) = setup("impact");
        pass(
            &dir,
            &mut index,
            &[
                (
                    "src/api.rs",
                    "fn a() {}\nfn b() {}\nfn c() {}\npub fn upload() {}\n",
                ),
                CALLER,
            ],
        );
        let (symbols, references) = index.impact("w", "src/api.rs", 1).unwrap();
        assert_eq!(symbols[0].name, "upload");
        assert_eq!(references[0].path, "src/main.rs");
    }

    fn levels(index: &Index, path: &str, depth: u32) -> Vec<langspan_core::ImpactLevel> {
        index
            .impact_levels("w", path, depth, Instant::now() + Duration::from_secs(10))
            .unwrap()
    }

    fn reached(level: &langspan_core::ImpactLevel) -> Vec<(&str, &str, &str)> {
        level
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.via.as_str(), file.from.as_str()))
            .collect()
    }

    /// `a.rs` exports `alpha`, `b.rs` mentions it and exports `beta`,
    /// `c.rs` mentions `beta` and exports `gamma`, which `a.rs` mentions.
    const CHAIN: [(&str, &str); 3] = [
        ("src/a.rs", "pub fn alpha() { gamma(); }\n"),
        ("src/b.rs", "pub fn beta() { alpha(); }\n"),
        ("src/c.rs", "pub fn gamma() { beta(); }\n"),
    ];

    #[test]
    fn impact_levels_follow_a_caller_of_a_caller_and_name_the_way() {
        let (dir, mut index) = setup("levels");
        pass(&dir, &mut index, &CHAIN);
        let two = levels(&index, "src/a.rs", 2);
        assert_eq!(two.len(), 2);
        assert_eq!((two[0].level, two[1].level), (1, 2));
        assert_eq!(reached(&two[0]), [("src/b.rs", "alpha", "src/a.rs")]);
        assert_eq!(reached(&two[1]), [("src/c.rs", "beta", "src/b.rs")]);
        assert!(!two[0].truncated && !two[1].truncated);

        // `c.rs` exports `gamma`, which `a.rs` mentions: the cycle does
        // not bring the queried file back, and nothing appears twice.
        let three = levels(&index, "src/a.rs", 3);
        assert_eq!(three.len(), 3);
        assert_eq!(three[2].files, []);
        assert!(!three[2].truncated);
        let one = levels(&index, "src/a.rs", 1);
        assert_eq!(one.len(), 1);
        assert_eq!(reached(&one[0]), [("src/b.rs", "alpha", "src/a.rs")]);
    }

    #[test]
    fn a_file_appears_once_at_its_first_level() {
        let (dir, mut index) = setup("levels-once");
        // `d.rs` mentions both `alpha` (level 1) and `beta` (level 2).
        let both = ("src/d.rs", "fn d() { alpha(); beta(); }\n");
        pass(&dir, &mut index, &[CHAIN[0], CHAIN[1], CHAIN[2], both]);
        let two = levels(&index, "src/a.rs", 2);
        assert_eq!(
            reached(&two[0]),
            [
                ("src/b.rs", "alpha", "src/a.rs"),
                ("src/d.rs", "alpha", "src/a.rs")
            ]
        );
        assert_eq!(reached(&two[1]), [("src/c.rs", "beta", "src/b.rs")]);
    }

    #[test]
    fn impact_levels_expand_at_most_fifty_names_per_level_and_say_so() {
        let (dir, mut index) = setup("levels-names");
        let exports: String = (0..NAMES_PER_LEVEL + 1)
            .map(|n| format!("pub fn n{n:02}() {{}}\n"))
            .collect();
        // Mentions only the last name in path order, which is past the bound.
        let last = format!("fn x() {{ n{NAMES_PER_LEVEL:02}(); }}\n");
        let first = "fn y() { n00(); }\n";
        pass(
            &dir,
            &mut index,
            &[
                ("src/api.rs", &exports),
                ("src/first.rs", first),
                ("src/last.rs", &last),
            ],
        );
        let one = levels(&index, "src/api.rs", 2);
        assert_eq!(reached(&one[0]), [("src/first.rs", "n00", "src/api.rs")]);
        assert!(one[0].truncated, "the 51st name was not expanded");
        assert!(!one[1].truncated);
    }

    #[test]
    fn impact_levels_stop_at_two_hundred_files_in_total() {
        let (dir, mut index) = setup("levels-total");
        let mut files: Vec<(String, String)> = vec![
            ("src/api.rs".into(), "pub fn root() {}\n".into()),
            ("src/hub.rs".into(), "pub fn hub() { root(); }\n".into()),
        ];
        // 150 files at level 1, 150 more at level 2: 100 are left out.
        for n in 0..150 {
            files.push((format!("src/l1_{n:03}.rs"), "fn f() { root(); }\n".into()));
            files.push((format!("src/l2_{n:03}.rs"), "fn f() { hub(); }\n".into()));
        }
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        // One pass parses at most `FILES_PER_PASS`; the second the rest.
        pass(&dir, &mut index, &borrowed);
        pass(&dir, &mut index, &borrowed);
        let two = levels(&index, "src/api.rs", 2);
        assert_eq!(two[0].files.len(), 151, "hub and 150 more");
        assert!(!two[0].truncated);
        assert_eq!(
            two[0].files.len() + two[1].files.len(),
            MAX_RESULTS as usize
        );
        assert!(two[1].truncated);
    }

    #[test]
    fn a_passed_deadline_returns_the_levels_so_far_marked_truncated() {
        let (dir, mut index) = setup("levels-deadline");
        pass(&dir, &mut index, &CHAIN);
        let late = index
            .impact_levels("w", "src/a.rs", 3, Instant::now())
            .unwrap();
        assert!(late.len() <= 3);
        assert!(late.last().is_some_and(|level| level.truncated), "{late:?}");
    }

    #[test]
    fn levels_and_coverage_come_from_one_snapshot() {
        let (dir, mut index) = setup("levels-snapshot");
        pass(&dir, &mut index, &CHAIN);
        let reader = Index::open_reader(&dir.0.join("graph.sqlite3")).unwrap();
        // The writer adds a file between the reader's levels and its
        // coverage: neither the levels nor the coverage see it.
        let (two, coverage) = reader
            .with_coverage(&of(&dir.0), |reader| {
                let two = reader.impact_levels(
                    "w",
                    "src/a.rs",
                    2,
                    Instant::now() + Duration::from_secs(10),
                )?;
                let more = ("src/d.rs", "fn d() { beta(); }\n");
                pass(&dir, &mut index, &[CHAIN[0], CHAIN[1], CHAIN[2], more]);
                Ok(two)
            })
            .unwrap();
        assert_eq!(reached(&two[1]), [("src/c.rs", "beta", "src/b.rs")]);
        assert_eq!(coverage.indexable_files, 3, "{coverage:?}");
        let (after, coverage) = reader
            .with_coverage(&of(&dir.0), |reader| {
                reader.impact_levels("w", "src/a.rs", 2, Instant::now() + Duration::from_secs(10))
            })
            .unwrap();
        assert_eq!(after[1].files.len(), 2);
        assert_eq!(coverage.indexable_files, 4);
    }

    /// A file of each indexed language, for `signature_tokens`.
    fn language_path(language: langspan_graph::Language) -> &'static str {
        match language {
            langspan_graph::Language::Rust => "src/a.rs",
            langspan_graph::Language::Python => "src/a.py",
            langspan_graph::Language::TypeScript => "src/a.ts",
            langspan_graph::Language::Tsx => "src/a.tsx",
            langspan_graph::Language::Go => "src/a.go",
            langspan_graph::Language::Java => "src/A.java",
        }
    }

    /// Whitespace between two words is never dropped from a signature
    /// comparison, in any indexed language: `chan int` and `chanint` are
    /// different Go types (review comment 4704678366), as `impl Trait` and
    /// `implTrait` are different Rust ones. Words meet next to keywords,
    /// inside generics and after pointer, reference and channel markers.
    #[test]
    fn signature_comparison_never_joins_two_words_in_any_indexed_language() {
        const WORDS: &[&str] = &[
            "fn",
            "impl",
            "dyn",
            "mut",
            "const",
            "unsafe",
            "chan",
            "func",
            "map",
            "interface",
            "struct",
            "final",
            "extends",
            "super",
            "typeof",
            "keyof",
            "readonly",
            "lambda",
            "async",
            "def",
            "int",
            "string",
            "T",
            "x",
        ];
        const PLACES: &[&str] = &[
            "{a} {b}",
            "f({a} {b})",
            "f<{a} {b}>",
            "f[{a} {b}]",
            "f(x *{a} {b})",
            "f(x <-{a} {b})",
            "f(x: &{a} {b})",
            "f(x: Vec<Box<{a} {b}>>)",
        ];
        for language in langspan_graph::Language::ALL {
            let path = language_path(language);
            for place in PLACES {
                for a in WORDS {
                    for b in WORDS {
                        let spaced = place.replace("{a}", a).replace("{b}", b);
                        let joined = place.replace("{a} {b}", &format!("{a}{b}"));
                        let joined = joined.replace("{a}", a).replace("{b}", b);
                        assert_ne!(
                            signature_tokens(path, &spaced),
                            signature_tokens(path, &joined),
                            "{path}: {spaced:?} and {joined:?}"
                        );
                    }
                }
            }
        }
    }

    /// Each indexed language's own declarations: a change where two tokens
    /// meet is a changed signature; spacing between tokens, and a line
    /// break, are not; whitespace inside a string literal is part of it.
    #[test]
    fn signature_comparison_keeps_each_languages_token_boundaries() {
        use langspan_graph::Language::*;
        let changed: &[(langspan_graph::Language, &str, &str)] = &[
            (Rust, "pub fn f(x: impl Trait)", "pub fn f(x: implTrait)"),
            (Rust, "pub fn f(x: &dyn Fn())", "pub fn f(x: &dynFn())"),
            (Rust, "pub fn f(x: *const u8)", "pub fn f(x: *constu8)"),
            (Rust, "pub fn f<'a>(x: &'a str)", "pub fn f<'a>(x: &'astr)"),
            (
                Rust,
                "pub fn f(x: Vec<Box<dyn Any>>)",
                "pub fn f(x: Vec<Box<dynAny>>)",
            ),
            (
                Rust,
                "pub fn f(x: &str) -> &'static str",
                "pub fn f(x: &str) -> &'staticstr",
            ),
            (
                Rust,
                r#"pub const A: &str = "a b""#,
                r#"pub const A: &str = "ab""#,
            ),
            (Python, "def f(x=lambda y: y)", "def f(x=lambday: y)"),
            (Python, "async def f(x)", "asyncdef f(x)"),
            (Python, r#"def f(x="a b")"#, r#"def f(x="ab")"#),
            (
                TypeScript,
                "export function f(x: typeof y)",
                "export function f(x: typeofy)",
            ),
            (
                TypeScript,
                "export function f(x: keyof T)",
                "export function f(x: keyofT)",
            ),
            (
                TypeScript,
                "export function f(x: readonly string[])",
                "export function f(x: readonlystring[])",
            ),
            (
                TypeScript,
                "export function f<T extends U>(x: T)",
                "export function f<TextendsU>(x: T)",
            ),
            (
                TypeScript,
                "export function f(x = 'a b')",
                "export function f(x = 'ab')",
            ),
            (
                Tsx,
                "export function F(p: Props<keyof T>)",
                "export function F(p: Props<keyofT>)",
            ),
            (Go, "func F(x chan int)", "func F(x chanint)"),
            (Go, "func F(x <-chan int)", "func F(x <-chanint)"),
            (Go, "func F(x *pkg.T)", "func F(x *pkgT)"),
            (Go, "func F[T any](x T)", "func F[Tany](x T)"),
            (
                Go,
                "func F(f func(x int) error)",
                "func F(f func(xint) error)",
            ),
            (
                Go,
                "func (s *S) F(x struct{ A int })",
                "func (s *S) F(x struct{ Aint })",
            ),
            (
                Go,
                r#"func F(x string) (y string, err error)"#,
                r#"func F(x string) (ystring, err error)"#,
            ),
            (Go, r#"const A = "a b""#, r#"const A = "ab""#),
            (Go, "const A = `a b`", "const A = `ab`"),
            (
                Java,
                "public static int f(final Foo x)",
                "public static int f(finalFoo x)",
            ),
            (
                Java,
                "public <T extends Comparable<T>> T max(List<? extends T> xs)",
                "public <T extends Comparable<T>> T max(List<?extendsT> xs)",
            ),
            (
                Java,
                "public static void f(int[] x)",
                "public staticvoid f(int[] x)",
            ),
            (
                Java,
                r#"@Named("a b") public void f()"#,
                r#"@Named("ab") public void f()"#,
            ),
        ];
        for (language, before, after) in changed {
            let path = language_path(*language);
            assert_ne!(
                signature_tokens(path, before),
                signature_tokens(path, after),
                "{path}: {before:?} and {after:?}"
            );
        }
        let respaced: &[(langspan_graph::Language, &str, &str)] = &[
            (
                Rust,
                "pub fn f( x : Vec< u8 > ) -> u8",
                "pub fn f(x: Vec<u8>) -> u8",
            ),
            (Python, "def f( x : int = 1 )", "def f(x: int = 1)"),
            (
                TypeScript,
                "export function f( x : T )",
                "export function f(x: T)",
            ),
            (
                Tsx,
                "export function F( p : Props )",
                "export function F(p: Props)",
            ),
            (
                Go,
                "func F( x  * T ,\n\ty map[ string ]int )",
                "func F(x *T, y map[string]int)",
            ),
            (Go, "func F(x <-chan int)", "func F(x <- chan int)"),
            (Go, "func F(x chan<- int)", "func F(x chan <-int)"),
            (Java, "public  int  f( int[]  x )", "public int f(int[] x)"),
        ];
        for (language, before, after) in respaced {
            let path = language_path(*language);
            assert_eq!(
                signature_tokens(path, before),
                signature_tokens(path, after),
                "{path}: {before:?} and {after:?}"
            );
        }
    }

    #[test]
    fn a_rename_with_an_unchanged_body_is_one_renamed_change_for_the_old_names_dependents() {
        let (dir, mut index) = setup("rename");
        let caller = ("src/main.rs", "fn main() { api::fetch_user(1); }\n");
        let before = (
            "src/api.rs",
            "pub fn fetch_user(id: u32) -> u32 {\n    id\n}\n",
        );
        pass(&dir, &mut index, &[before, caller]);
        let after = (
            "src/api.rs",
            "pub fn load_user(id: u32) -> u32 {\n    id\n}\n",
        );
        let notices = pass(&dir, &mut index, &[after, caller]);
        assert_eq!(notices.len(), 1, "{notices:?}");
        let change = &notices[0].changes;
        assert_eq!(change.len(), 1, "{change:?}");
        assert_eq!(
            (
                change[0].name.as_str(),
                change[0].change.as_str(),
                change[0].after_name.as_deref(),
                change[0].after.as_deref()
            ),
            (
                "fetch_user",
                "renamed",
                Some("load_user"),
                Some("pub fn load_user(id: u32) -> u32")
            )
        );
        assert!(change[0].after_arity.is_some());
        assert_eq!(notices[0].dependents, ["src/main.rs"]);
    }

    #[test]
    fn an_index_from_before_block_digests_is_kept_and_tells_its_next_rename_as_a_removal() {
        let (dir, mut index) = setup("blocks");
        let before = (
            "src/api.rs",
            "pub fn fetch_user(id: u32) -> u32 {\n    id\n}\n",
        );
        pass(&dir, &mut index, &[before, CALLER]);
        // The baselines as an earlier build wrote them: no block digests.
        index
            .connection
            .execute_batch(
                "ALTER TABLE baselines DROP COLUMN block_digest;
                 INSERT INTO files(workspace_id, path, scan_digest) VALUES ('w', 'kept', 'x');",
            )
            .unwrap();
        drop(index);
        let mut index = Index::open(&dir.0.join("graph.sqlite3")).unwrap();
        let kept: i64 = index
            .connection
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = 'kept'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "the index was kept, not rebuilt");
        let after = (
            "src/api.rs",
            "pub fn load_user(id: u32) -> u32 {\n    id\n}\n",
        );
        let notices = pass(&dir, &mut index, &[after, CALLER]);
        let labels: Vec<&str> = notices[0]
            .changes
            .iter()
            .map(|change| change.change.as_str())
            .collect();
        // The new name is also recorded, for its history (roadmap D4).
        assert_eq!(labels, ["removed", "added"], "{notices:?}");
        // From then on the baseline has them.
        let again = (
            "src/api.rs",
            "pub fn get_user(id: u32) -> u32 {\n    id\n}\n",
        );
        let notices = pass(&dir, &mut index, &[again, CALLER]);
        assert_eq!(notices[0].changes[0].change, "renamed", "{notices:?}");
    }

    #[test]
    fn deleting_a_file_reports_its_exports_as_removed() {
        let (dir, mut index) = setup("delete");
        let api = ("src/api.rs", "pub fn upload() {}\npub fn gate() {}\n");
        pass(&dir, &mut index, &[api, CALLER]);
        fs::remove_file(dir.0.join("src/api.rs")).unwrap();
        let notices = pass(&dir, &mut index, &[CALLER]);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].path, "src/api.rs");
        assert_eq!(notices[0].changes.len(), 2);
        assert!(
            notices[0]
                .changes
                .iter()
                .all(|change| change.change == "removed")
        );
        assert_eq!(notices[0].dependents, ["src/main.rs"]);
    }

    #[test]
    fn a_partial_parse_in_between_cannot_hide_a_removal() {
        let (dir, mut index) = setup("partial");
        pass(
            &dir,
            &mut index,
            &[
                ("src/api.rs", "pub fn upload() {}\npub fn gate() {}\n"),
                CALLER,
            ],
        );
        // Caught mid-edit: gate is not seen because the file does not parse.
        let notices = pass(
            &dir,
            &mut index,
            &[
                ("src/api.rs", "pub fn upload() {}\npub fn gate( {{{\n"),
                CALLER,
            ],
        );
        assert!(
            notices.is_empty(),
            "a partial parse is not trusted: {notices:?}"
        );
        // The clean result drops gate. It is compared with the last clean
        // version, so the removal is reported.
        let notices = pass(
            &dir,
            &mut index,
            &[("src/api.rs", "pub fn upload() {}\n"), CALLER],
        );
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].changes[0].name, "gate");
    }

    #[test]
    fn coverage_describes_the_same_snapshot_as_the_results_it_comes_with() {
        // Found on CI: a query read its rows, the indexer committed the next
        // file, and the coverage read afterwards claimed that file too.
        let (dir, mut writer) = setup("snapshot");
        pass(&dir, &mut writer, &[CALLER]);
        let reader = Index::open_reader(&dir.0.join("graph.sqlite3")).unwrap();
        let (symbols, coverage) = reader
            .with_coverage(&of(&dir.0), |reader| {
                let symbols = reader.symbol("w", "upload", 10)?;
                pass(
                    &dir,
                    &mut writer,
                    &[("src/api.rs", "pub fn upload() {}\n"), CALLER],
                );
                Ok(symbols)
            })
            .unwrap();
        assert!(symbols.is_empty(), "read before api.rs was indexed");
        assert_eq!(
            (coverage.indexable_files, coverage.current_files),
            (1, 1),
            "coverage must not include api.rs, which the results do not"
        );
        // A later answer sees both.
        let (symbols, coverage) = reader
            .with_coverage(&of(&dir.0), |reader| reader.symbol("w", "upload", 10))
            .unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!((coverage.indexable_files, coverage.current_files), (2, 2));
    }

    const UPLOAD: (&str, &str) = (
        "src/api.rs",
        "pub fn upload(path: &str) -> u8 {\n    let a = open(path);\n    let b = open(path);\n    hash(a, b)\n}\n\npub fn other() -> u8 { 1 }\n",
    );
    const CLI: (&str, &str) = ("src/cli.rs", "fn main() {\n    api::upload(\"a\");\n}\n");
    const WEB: (&str, &str) = (
        "src/web.rs",
        "fn serve() {\n\n    upload(\"b\");\n    upload(\"c\");\n}\n",
    );

    fn upload_with_detail(index: &Index) -> SymbolRef {
        let mut symbol = index.symbol("w", "upload", 10).unwrap().remove(0);
        index.add_detail("w", &mut symbol).unwrap();
        symbol
    }

    #[test]
    fn detail_counts_the_calls_inside_a_definition_and_its_callers_elsewhere() {
        let (dir, mut index) = setup("detail");
        pass(&dir, &mut index, &[UPLOAD, CLI, WEB]);
        let upload = upload_with_detail(&index);
        let call = |name: &str, count, line| langspan_core::CallCount {
            name: name.into(),
            count,
            line,
        };
        assert_eq!(
            upload.calls.as_deref(),
            Some(&[call("open", 2, 2), call("hash", 1, 4)][..])
        );
        assert_eq!(
            upload.callers,
            Some(langspan_core::Callers {
                count: 3,
                sites: vec![
                    "src/cli.rs:2".into(),
                    "src/web.rs:3".into(),
                    "src/web.rs:4".into()
                ],
            })
        );
        // Each call is stored with the innermost definition it is in.
        let caller: String = index
            .connection
            .query_row(
                "SELECT caller FROM calls WHERE callee = 'hash'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(caller, "upload");
        // Without detail nothing is added.
        let plain = index.symbol("w", "upload", 10).unwrap().remove(0);
        assert_eq!(
            (plain.body_digest, plain.calls, plain.callers),
            (None, None, None)
        );
    }

    #[test]
    fn a_body_digest_changes_only_with_the_definitions_own_bytes() {
        let (dir, mut index) = setup("body");
        pass(&dir, &mut index, &[UPLOAD, CLI]);
        let before = upload_with_detail(&index);
        let elsewhere = UPLOAD.1.replace("{ 1 }", "{ 2 }");
        pass(&dir, &mut index, &[(UPLOAD.0, &elsewhere), CLI]);
        let after = upload_with_detail(&index);
        assert_ne!(before.digest, after.digest, "the file changed");
        assert_eq!(before.body_digest, after.body_digest, "upload did not");
        assert_eq!(before.body_digest.as_ref().unwrap().len(), 64);
        let inside = UPLOAD.1.replace("hash(a, b)", "hash(b, a)");
        pass(&dir, &mut index, &[(UPLOAD.0, &inside), CLI]);
        assert_ne!(upload_with_detail(&index).body_digest, after.body_digest);
    }

    #[test]
    fn a_deleted_index_is_rebuilt_with_the_same_calls() {
        let (dir, mut index) = setup("rebuild");
        pass(&dir, &mut index, &[UPLOAD, CLI, WEB]);
        let before = upload_with_detail(&index);
        drop(index);
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(dir.0.join(format!("graph.sqlite3{suffix}")));
        }
        let mut index = Index::open(&dir.0.join("graph.sqlite3")).unwrap();
        pass(&dir, &mut index, &[UPLOAD, CLI, WEB]);
        assert_eq!(upload_with_detail(&index), before);
    }

    #[test]
    fn an_index_of_the_previous_schema_is_discarded() {
        let (dir, index) = setup("schema");
        index
            .connection
            .execute_batch("DROP TABLE calls; PRAGMA user_version = 2;")
            .unwrap();
        drop(index);
        let mut index = Index::open(&dir.0.join("graph.sqlite3")).unwrap();
        pass(&dir, &mut index, &[UPLOAD, CLI]);
        assert_eq!(upload_with_detail(&index).callers.unwrap().count, 1);
    }

    #[test]
    fn returning_to_earlier_content_is_a_distinct_transition() {
        let (dir, mut index) = setup("cycle");
        let a = ("src/api.rs", "pub fn upload() {}\n");
        let b = ("src/api.rs", "pub fn upload(x: u8) {}\n");
        let c = ("src/api.rs", "pub fn upload(x: u16) {}\n");
        pass(&dir, &mut index, &[a, CALLER]);
        let first = pass(&dir, &mut index, &[b, CALLER]);
        pass(&dir, &mut index, &[c, CALLER]);
        let again = pass(&dir, &mut index, &[b, CALLER]);
        assert_eq!(first[0].digest, again[0].digest, "same destination");
        assert_ne!(
            first[0].before_digest, again[0].before_digest,
            "different transition"
        );
        assert_eq!(
            again[0].changes[0].before.as_deref(),
            Some("pub fn upload(x: u16)")
        );
    }

    // Every outcome of reading a file for the in-flight comparison is
    // terminal for that version or retried with a bound, and failing files
    // never hold a pass from the others (review comment 4705138339).

    /// Run Git in `dir` and return its trimmed standard output.
    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A Git repository whose `HEAD` holds `committed`.
    fn repo(name: &str, committed: &[(&str, &[u8])]) -> (Dir, Index) {
        let (dir, index) = setup(name);
        git(&dir.0, &["init", "--quiet"]);
        for (key, value) in [
            ("user.name", "LangSpan test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
            ("gc.auto", "0"),
        ] {
            git(&dir.0, &["config", key, value]);
        }
        fs::write(dir.0.join(".gitignore"), "graph.sqlite3*\n").unwrap();
        for (path, bytes) in committed {
            fs::write(dir.0.join(path), bytes).unwrap();
        }
        git(&dir.0, &["add", "--all"]);
        git(&dir.0, &["commit", "--quiet", "-m", "base"]);
        (dir, index)
    }

    /// What a scan of `paths` would hand the index now: each file's digest
    /// and, as every one of them is taken to differ from `HEAD`, its blob
    /// there if `HEAD` has one.
    fn head_scan(dir: &Dir, paths: &[String]) -> ScanView {
        let listing = git(&dir.0, &["ls-tree", "-r", "HEAD"]);
        let at_head: BTreeMap<&str, &str> = listing
            .lines()
            .filter_map(|line| {
                let (meta, path) = line.split_once('\t')?;
                Some((path, meta.split(' ').nth(2)?))
            })
            .collect();
        let mut scan = ScanView::default();
        for path in paths {
            let bytes = fs::read(dir.0.join(path)).unwrap();
            let digest = crate::sha256::hex(&crate::sha256::digest(&bytes));
            scan.files.insert(path.clone(), digest);
            let head = at_head
                .get(path.as_str())
                .map(|object| (*object).to_owned());
            scan.dirty.insert(path.clone(), head);
        }
        scan
    }

    fn update(dir: &Dir, index: &mut Index, scan: &ScanView) {
        let far = Instant::now() + Duration::from_secs(30);
        index.update("w", &dir.0, 0, scan, NOW, far).unwrap();
    }

    /// The in-flight set, as `path: name`.
    fn in_flight(index: &Index) -> Vec<String> {
        index
            .connection
            .prepare("SELECT path || ': ' || name FROM inflight ORDER BY 1")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Whether the comparison of `path` is recorded: it is not read again
    /// until the file or its `HEAD` blob changes.
    fn compared(index: &Index, path: &str) -> bool {
        index
            .connection
            .query_row(
                "SELECT COUNT(*) > 0 FROM inflight_files WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Whether `path` is indexed at the scanned version.
    fn indexed(index: &Index, path: &str) -> bool {
        index
            .connection
            .query_row(
                "SELECT COUNT(*) > 0 FROM files WHERE path = ?1 AND digest IS scan_digest",
                [path],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// The failures recorded for `path` at `stage`.
    fn failures(index: &Index, stage: &str, path: &str) -> Option<u32> {
        index
            .connection
            .query_row(
                "SELECT failures FROM retries WHERE path = ?1 AND stage = ?2",
                [path, stage],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    /// Move `path`'s `HEAD` blob out of the object store; returns where it
    /// was and where it is now.
    fn hide_head_blob(dir: &Dir, path: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let object = git(&dir.0, &["rev-parse", &format!("HEAD:{path}")]);
        let loose = dir
            .0
            .join(".git/objects")
            .join(&object[..2])
            .join(&object[2..]);
        let aside = dir.0.join(format!("aside-{object}"));
        fs::rename(&loose, &aside).unwrap();
        (loose, aside)
    }

    const GATE: &str = "pub fn upload() {}\npub fn gate(role: &str) -> bool { role.is_empty() }\n";

    fn oversized() -> Vec<u8> {
        format!(
            "// {}\npub fn upload() {{}}\n",
            "x".repeat(MAX_SOURCE_BYTES)
        )
        .into_bytes()
    }

    #[test]
    fn an_oversized_head_blob_is_remembered_for_that_version_and_not_read_again() {
        let big = oversized();
        let (dir, mut index) = repo("head-oversize", &[("src/api.rs", &big)]);
        fs::write(dir.0.join("src/api.rs"), GATE).unwrap();
        let scan = head_scan(&dir, &["src/api.rs".into()]);
        update(&dir, &mut index, &scan);
        assert!(indexed(&index, "src/api.rs"));
        // Uncomparable: nothing is said of it, and that is remembered.
        assert!(compared(&index, "src/api.rs"), "left pending");
        assert_eq!(in_flight(&index), Vec::<String>::new());
        assert_eq!(failures(&index, Retries::COMPARE, "src/api.rs"), None);
        // A new HEAD blob is a new version: compared again.
        git(&dir.0, &["commit", "--quiet", "--all", "-m", "small"]);
        fs::write(
            dir.0.join("src/api.rs"),
            format!("{GATE}pub fn quota() {{}}\n"),
        )
        .unwrap();
        update(&dir, &mut index, &head_scan(&dir, &["src/api.rs".into()]));
        assert_eq!(in_flight(&index), ["src/api.rs: quota"]);
    }

    #[test]
    fn a_binary_head_blob_is_remembered_for_that_version() {
        let binary: &[u8] = b"\x00\xff\xfe pub fn \x00 {{{ \x01\x02";
        let (dir, mut index) = repo("head-binary", &[("src/api.rs", binary)]);
        fs::write(dir.0.join("src/api.rs"), GATE).unwrap();
        update(&dir, &mut index, &head_scan(&dir, &["src/api.rs".into()]));
        assert!(compared(&index, "src/api.rs"), "left pending");
        assert_eq!(failures(&index, Retries::COMPARE, "src/api.rs"), None);
    }

    #[test]
    fn a_binary_working_file_is_indexed_partially_and_says_nothing_in_flight() {
        let (dir, mut index) = repo("work-binary", &[("src/api.rs", b"pub fn upload() {}\n")]);
        fs::write(
            dir.0.join("src/api.rs"),
            b"\x00\xff\xfe pub fn \x00 {{{ \x01",
        )
        .unwrap();
        update(&dir, &mut index, &head_scan(&dir, &["src/api.rs".into()]));
        assert!(indexed(&index, "src/api.rs"), "left pending");
        assert!(compared(&index, "src/api.rs"), "left pending");
        assert_eq!(in_flight(&index), Vec::<String>::new());
        assert_eq!(failures(&index, Retries::INDEX, "src/api.rs"), None);
    }

    #[test]
    fn a_missing_head_blob_is_compared_once_it_is_back() {
        let (dir, mut index) = repo("head-missing", &[("src/api.rs", b"pub fn upload() {}\n")]);
        let (loose, aside) = hide_head_blob(&dir, "src/api.rs");
        fs::write(dir.0.join("src/api.rs"), GATE).unwrap();
        let scan = head_scan(&dir, &["src/api.rs".into()]);
        update(&dir, &mut index, &scan);
        assert!(
            !compared(&index, "src/api.rs"),
            "an unread blob is not compared"
        );
        assert_eq!(failures(&index, Retries::COMPARE, "src/api.rs"), Some(1));
        fs::rename(&aside, &loose).unwrap();
        update(&dir, &mut index, &scan);
        assert_eq!(in_flight(&index), ["src/api.rs: gate"]);
        assert_eq!(failures(&index, Retries::COMPARE, "src/api.rs"), None);
    }

    #[test]
    fn a_head_blob_that_stays_unreadable_is_tried_a_bounded_number_of_times() {
        let (dir, mut index) = repo("head-bound", &[("src/api.rs", b"pub fn upload() {}\n")]);
        let (loose, aside) = hide_head_blob(&dir, "src/api.rs");
        fs::write(dir.0.join("src/api.rs"), GATE).unwrap();
        let scan = head_scan(&dir, &["src/api.rs".into()]);
        let mut passes = 0;
        while !compared(&index, "src/api.rs") {
            update(&dir, &mut index, &scan);
            passes += 1;
            assert!(passes <= 200, "still retried after {passes} passes");
        }
        // Waits double: far fewer tries than passes.
        assert!(passes > 100, "given up after only {passes} passes");
        assert_eq!(in_flight(&index), Vec::<String>::new());
        assert_eq!(failures(&index, Retries::COMPARE, "src/api.rs"), None);
        // Given up on for this version: back or not, it is not read again.
        fs::rename(&aside, &loose).unwrap();
        update(&dir, &mut index, &scan);
        assert_eq!(in_flight(&index), Vec::<String>::new());
        // A new version of the file is compared afresh.
        fs::write(
            dir.0.join("src/api.rs"),
            format!("{GATE}pub fn quota() {{}}\n"),
        )
        .unwrap();
        update(&dir, &mut index, &head_scan(&dir, &["src/api.rs".into()]));
        assert_eq!(in_flight(&index), ["src/api.rs: gate", "src/api.rs: quota"]);
    }

    /// A working file that cannot be read for indexing (`break_it` makes it
    /// so after the scan) is tried again with a bound, then given up on
    /// for that version; a new version is indexed and compared.
    fn unreadable_working_file_is_bounded(name: &str, break_it: impl Fn(&Path)) {
        let (dir, mut index) = repo(name, &[("src/api.rs", b"pub fn upload() {}\n")]);
        let full = dir.0.join("src/api.rs");
        fs::write(&full, GATE).unwrap();
        let scan = head_scan(&dir, &["src/api.rs".into()]);
        break_it(&full);
        update(&dir, &mut index, &scan);
        assert_eq!(failures(&index, Retries::INDEX, "src/api.rs"), Some(1));
        let mut passes = 1;
        while failures(&index, Retries::INDEX, "src/api.rs") < Some(Retries::ATTEMPTS) {
            update(&dir, &mut index, &scan);
            passes += 1;
            assert!(passes <= 200, "still retried after {passes} passes");
        }
        assert!(!indexed(&index, "src/api.rs") && !compared(&index, "src/api.rs"));
        // Readable again at a new version: indexed and compared.
        let _ = fs::remove_dir_all(&full);
        let _ = fs::remove_file(&full);
        fs::write(&full, format!("{GATE}pub fn quota() {{}}\n")).unwrap();
        update(&dir, &mut index, &head_scan(&dir, &["src/api.rs".into()]));
        assert_eq!(in_flight(&index), ["src/api.rs: gate", "src/api.rs: quota"]);
        assert_eq!(failures(&index, Retries::INDEX, "src/api.rs"), None);
    }

    #[test]
    fn a_working_file_deleted_after_the_scan_is_retried_with_a_bound() {
        unreadable_working_file_is_bounded("work-deleted", |full| fs::remove_file(full).unwrap());
    }

    #[test]
    fn a_working_file_that_is_no_longer_a_regular_file_is_retried_with_a_bound() {
        unreadable_working_file_is_bounded("work-not-file", |full| {
            fs::remove_file(full).unwrap();
            fs::create_dir(full).unwrap();
        });
    }

    #[test]
    fn a_working_file_changed_after_the_scan_is_retried_with_a_bound() {
        unreadable_working_file_is_bounded("work-changed", |full| {
            fs::write(full, "pub fn other() {}\n").unwrap();
        });
    }

    #[test]
    fn a_working_file_without_read_permission_is_retried_with_a_bound() {
        // The superuser reads any file: the denial cannot be made.
        if rustix::process::geteuid().is_root() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        unreadable_working_file_is_bounded("work-denied", |full| {
            fs::set_permissions(full, fs::Permissions::from_mode(0o000)).unwrap();
        });
    }

    /// More files than one pass reads whose `HEAD` blobs fail, some
    /// terminally (too large) and some for now (missing), sort before a
    /// good file: it still gets its in-flight entry.
    #[test]
    fn failing_head_blobs_beyond_the_pass_budget_do_not_starve_a_later_file() {
        let paths: Vec<String> = (0..=FILES_PER_PASS)
            .map(|n| format!("src/a{n:03}.rs"))
            .chain(["src/zz.rs".into()])
            .collect();
        let last = paths.len() - 1;
        // Every other failing file's blob too large, the rest to be hidden;
        // each blob distinct, so hiding one hides no other file's.
        let versions: Vec<Vec<u8>> = (0..paths.len())
            .map(|n| {
                let mut bytes = if n % 2 == 0 && n != last {
                    oversized()
                } else {
                    Vec::new()
                };
                bytes.extend(format!("pub fn upload() {{}}\n// {n}\n").bytes());
                bytes
            })
            .collect();
        let committed: Vec<(&str, &[u8])> = paths
            .iter()
            .zip(&versions)
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        let (dir, mut index) = repo("head-starve", &committed);
        for (n, path) in paths.iter().enumerate() {
            if n % 2 == 1 && n != last {
                hide_head_blob(&dir, path);
            }
            fs::write(dir.0.join(path), GATE).unwrap();
        }
        let scan = head_scan(&dir, &paths);
        for _ in 0..4 {
            update(&dir, &mut index, &scan);
        }
        assert!(
            in_flight(&index).contains(&"src/zz.rs: gate".to_owned()),
            "starved"
        );
    }

    /// More working files than one pass reads that cannot be read sort
    /// before a good file: it is still indexed and gets its entry.
    #[test]
    fn unreadable_working_files_beyond_the_pass_budget_do_not_starve_a_later_file() {
        let paths: Vec<String> = (0..=FILES_PER_PASS)
            .map(|n| format!("src/a{n:03}.rs"))
            .chain(["src/zz.rs".into()])
            .collect();
        let committed: Vec<(&str, &[u8])> = paths
            .iter()
            .map(|path| (path.as_str(), b"pub fn upload() {}\n".as_slice()))
            .collect();
        let (dir, mut index) = repo("work-starve", &committed);
        for path in &paths {
            fs::write(dir.0.join(path), GATE).unwrap();
        }
        let scan = head_scan(&dir, &paths);
        for path in paths.iter().filter(|path| *path != "src/zz.rs") {
            fs::remove_file(dir.0.join(path)).unwrap();
        }
        for _ in 0..4 {
            update(&dir, &mut index, &scan);
        }
        assert_eq!(in_flight(&index), ["src/zz.rs: gate"], "starved");
    }

    fn scan_of(files: &[(&str, &str)]) -> ScanView {
        ScanView {
            files: files
                .iter()
                .map(|(path, text)| {
                    (
                        (*path).to_owned(),
                        crate::sha256::hex(&crate::sha256::digest(text.as_bytes())),
                    )
                })
                .collect(),
            ..ScanView::default()
        }
    }

    fn transfer(from: &Index, root: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        from.export(
            &of(root),
            &mut out,
            NOW,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        out
    }

    #[test]
    fn an_import_never_replaces_a_version_this_index_parsed_and_sets_the_baseline() {
        let (dir, mut source) = setup("import-source");
        let a = ("src/api.rs", "pub fn upload() {}\n");
        let b = ("src/api.rs", "pub fn upload(x: u8) {}\n");
        pass(&dir, &mut source, &[a, CALLER]);
        let exported = transfer(&source, &dir.0);

        // Here the index already parsed another version of api.rs: it is
        // kept, and its later change is still compared with it.
        let local = dir.0.join("local.sqlite3");
        let mut index = Index::open(&local).unwrap();
        pass(&dir, &mut index, &[b, CALLER]);
        fs::write(dir.0.join(a.0), a.1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let counts = index
            .import(
                &of(&dir.0),
                &scan_of(&[a, CALLER]),
                &mut &exported[..],
                true,
                NOW,
                deadline,
            )
            .unwrap();
        assert_eq!((counts.loaded, counts.kept), (0, 2), "{counts:?}");
        let notices = pass(&dir, &mut index, &[a, CALLER]);
        assert_eq!(notices.len(), 1, "b to a is still a change: {notices:?}");

        // A fresh index loads both, reports nothing, and compares the next
        // version with the imported one.
        let fresh = dir.0.join("fresh.sqlite3");
        let mut index = Index::open(&fresh).unwrap();
        let counts = index
            .import(
                &of(&dir.0),
                &scan_of(&[a, CALLER]),
                &mut &exported[..],
                true,
                NOW,
                deadline,
            )
            .unwrap();
        assert_eq!(counts.loaded, 2, "{counts:?}");
        assert!(
            pass(&dir, &mut index, &[a, CALLER]).is_empty(),
            "no parse, no notice"
        );
        assert_eq!(upload_with_detail(&index), upload_with_detail(&source));
        let notices = pass(&dir, &mut index, &[b, CALLER]);
        assert_eq!(notices.len(), 1, "a to b against the imported baseline");
        assert_eq!(notices[0].dependents, ["src/main.rs"]);
    }

    #[test]
    fn a_path_missing_from_the_scan_is_not_imported() {
        let (dir, mut source) = setup("import-unscanned");
        let a = ("src/api.rs", "pub fn upload() {}\n");
        pass(&dir, &mut source, &[a, CALLER]);
        let exported = transfer(&source, &dir.0);
        let mut index = Index::open(&dir.0.join("fresh.sqlite3")).unwrap();
        let counts = index
            .import(
                &of(&dir.0),
                &scan_of(&[CALLER]),
                &mut &exported[..],
                true,
                NOW,
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!((counts.loaded, counts.different), (1, 1), "{counts:?}");
        // So the next pass cannot report its exports removed.
        assert!(pass(&dir, &mut index, &[CALLER]).is_empty());
    }
}
