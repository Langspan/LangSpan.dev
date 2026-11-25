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
        Ok(Self { connection })
    }

    /// A reader for the request path. It never creates or changes the file.
    pub fn open_reader(path: &Path) -> Result<Self, Failure> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_millis(100))?;
        Ok(Self { connection })
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
            "SELECT name, kind, signature FROM baselines WHERE workspace_id = ?1 AND path = ?2",
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
        let changes: Vec<InterfaceChange> = langspan_graph::diff(&before, after)
            .into_iter()
            .filter_map(|change| {
                let (label, before, after, number) = match change.change {
                    Change::Removed => ("removed", None, None, None),
                    Change::SignatureChanged { before, after } => {
                        ("signature_changed", Some(before), Some(after), None)
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
                    Change::Added => return None,
                };
                let arity = |signature| {
                    if number.is_some() {
                        None
                    } else {
                        counts(signature)
                    }
                };
                Some(InterfaceChange {
                    name: change.name,
                    kind: change.kind.name().into(),
                    change: label.into(),
                    before_arity: arity(&before),
                    after_arity: arity(&after),
                    before_count: number.map(|(before, _)| before),
                    after_count: number.map(|(_, after)| after),
                    before,
                    after,
                })
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

    /// Bring one workspace's index up to `scan.files`, the indexable paths
    /// of its latest complete scan with their content digests. A path in
    /// `scan.unobserved` was listed but not versioned by that scan (a
    /// partial scan, a file over the size bound): it keeps what the index
    /// already holds for it and is not treated as removed. Parses at most
    /// `FILES_PER_PASS` files and stops at `deadline`; what is left is picked
    /// up by the next call. `now_ms` (wall clock) dates a file that becomes
    /// partial. Returns the interface changes it found.
    pub fn update(
        &mut self,
        workspace_id: &str,
        root: &Path,
        scan: &ScanView,
        now_ms: u64,
        deadline: Instant,
    ) -> Result<Vec<Notice>, Failure> {
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
            for table in ["files", "symbols", "refs", "imports", "baselines", "calls"] {
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

        let pending: Vec<(&String, &String)> = files
            .iter()
            .filter(|(path, digest)| {
                let (indexed, oversize) = indexed.get(*path).cloned().unwrap_or_default();
                indexed.as_ref() != Some(digest) && oversize.as_ref() != Some(digest)
            })
            .take(FILES_PER_PASS)
            .collect();
        for (path, digest) in pending {
            if Instant::now() >= deadline {
                break;
            }
            let Some(language) = Language::for_path(path) else {
                continue;
            };
            // The file must still be exactly what the scan saw; otherwise
            // the next scan brings its new digest. Read up to the scan's own
            // bound, so a version the scan digested can be confirmed.
            let scanned = crate::observer::Limits::default().max_file_bytes;
            let Some(bytes) = read_exact_version(root, path, digest, scanned) else {
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
                continue;
            }
            let Ok(graph) = langspan_graph::extract(language, &bytes) else {
                continue;
            };
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
                        "INSERT INTO baselines VALUES (?1, ?2, ?3, ?4, ?5)",
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
        Ok(notices)
    }

    /// Run `reads` and measure coverage as one answer, from one read
    /// snapshot of the index. The indexer commits one file at a time, so
    /// separate reads could pair results from before a commit with coverage
    /// from after it and claim more than the results hold. This is the only
    /// way to get coverage, so every answer's coverage describes its rows.
    pub fn with_coverage<T>(
        &self,
        workspace_id: &str,
        reads: impl FnOnce(&Self) -> Result<T, Failure>,
    ) -> Result<(T, GraphCoverage), Failure> {
        // A deferred transaction in WAL mode reads one snapshot, fixed at
        // its first read, until it ends. It only reads, so ending it on an
        // error by dropping it (a rollback) loses nothing.
        let snapshot = self.connection.unchecked_transaction()?;
        let answer = reads(self)?;
        let coverage = self.coverage(workspace_id)?;
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
}

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
    "groovy", "h", "hh", "hpp", "hrl", "hs", "java", "jl", "kt", "kts", "lua", "m", "ml", "mli",
    "mm", "nim", "php", "pl", "pm", "r", "rb", "scala", "svelte", "swift", "vue", "zig",
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
        Self {
            files: indexable(snapshot),
            unobserved: snapshot.unobserved.clone(),
            blind,
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
            .update("w", &dir.0, &scan, now_ms, Instant::now() + budget)
            .unwrap()
    }

    fn blind(index: &Index, limit: u32) -> (langspan_core::BlindSpots, GraphCoverage) {
        index
            .with_coverage("w", |index| index.blind_spots("w", limit))
            .unwrap()
    }

    const BROKEN: (&str, &str) = ("src/broken.rs", "pub fn half( {{{\n");

    #[test]
    fn blind_spots_name_partial_never_clean_and_waiting_files() {
        let (dir, mut index) = setup("blind");
        let api = ("src/api.rs", "pub fn upload() {}\n");
        pass(&dir, &mut index, &[api, BROKEN, CALLER]);
        let (spots, coverage) = blind(&index, 10);
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
        assert_eq!(blind(&index, 10).0.partial[0].since_ms, NOW);

        // Fixed: in no list, and the count says so.
        let fixed = ("src/broken.rs", "pub fn half() {}\n");
        pass(&dir, &mut index, &[api, fixed, CALLER]);
        let (spots, coverage) = blind(&index, 10);
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
        let (spots, coverage) = blind(&index, 10);
        assert_eq!(spots.waiting, ["src/api.rs"]);
        assert_eq!((spots.totals.waiting, coverage.blind_in_scope), (1, 1));
        pass(&dir, &mut index, &[changed, fixed, CALLER]);
        assert!(blind(&index, 10).0.waiting.is_empty());
    }

    #[test]
    fn a_file_over_the_parse_bound_is_too_large_and_not_waiting() {
        let (dir, mut index) = setup("oversize");
        let big = format!("// {}\n", "x".repeat(MAX_SOURCE_BYTES));
        pass(&dir, &mut index, &[("src/big.rs", &big), CALLER]);
        let (spots, coverage) = blind(&index, 10);
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
        let (spots, coverage) = blind(&index, 10);
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
        index.update("w", &dir.0, &scan, NOW, far).unwrap();
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
        index.update("w", &dir.0, &scan, NOW, far).unwrap();
        let (spots, coverage) = blind(&index, 10);
        assert_eq!(spots, langspan_core::BlindSpots::default());
        assert_eq!(coverage.blind_in_scope, 0);
    }

    #[test]
    fn a_capped_list_is_the_same_on_every_call() {
        let (dir, mut index) = setup("cap");
        let names: Vec<String> = (0..5).map(|n| format!("src/b{n}.rs")).collect();
        let files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), BROKEN.1)).collect();
        pass(&dir, &mut index, &files);
        let first = blind(&index, 2).0;
        assert_eq!(first, blind(&index, 2).0);
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
            .with_coverage("w", |index| index.impact("w", "src/api.rs", 10))
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
            fs::write(root.join(format!("z/C{n}.java")), "class C {}\n").unwrap();
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
                &scan,
                NOW,
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        let (spots, coverage) = blind(&index, 10);
        assert_eq!(spots.too_large, ["src/0huge.rs"]);
        assert_eq!(spots.unscanned, ["src/c.rs"]);
        assert_eq!(
            spots.unsupported_languages,
            [langspan_core::LanguageCount {
                extension: ".java".into(),
                files: 10
            }]
        );
        assert_eq!((spots.totals.too_large, spots.totals.unscanned), (1, 1));
        assert_eq!(coverage.blind_in_scope, 2);
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
            .with_coverage("w", |reader| {
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
            .with_coverage("w", |reader| reader.symbol("w", "upload", 10))
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
}
