//! The write side of the index: opening it, interface notices, export,
//! import and the scan the indexer reads.

use super::head::Retries;
use super::*;
use langspan_core::InterfaceChange;
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

/// The name a file with no definitions is recorded by when it moves whole
/// (roadmap D7): its file name, cut at a character boundary to the 128
/// bytes a change's name may hold, or `file` for a name the core would
/// refuse (control characters).
fn moved_file_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.is_empty() || name.chars().any(char::is_control) {
        return "file".into();
    }
    let mut end = name.len().min(128);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

/// The exported signatures a version gives one name and kind, as a notice
/// shows several: each as written, joined by " | ", as many as fit a
/// notice's 256 bytes (one signature is at most 240), so that they never
/// make the core refuse the notice and the changes in it.
fn written(graph: &FileGraph, name: &str, kind: SymbolKind) -> Option<String> {
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
}

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
        // The first and last line of each baseline definition, which link a
        // removal and an addition with a changed body as a rename (roadmap
        // D7). Added the same way: a baseline from before has none, so its
        // next such change is told as a removal and an addition.
        for (column, kind) in [
            ("block_digest", "TEXT"),
            ("line", "INTEGER"),
            ("end_line", "INTEGER"),
        ] {
            let has: bool = connection.query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('baselines') WHERE name = ?1",
                [column],
                |row| row.get(0),
            )?;
            if !has {
                connection.execute_batch(&format!(
                    "ALTER TABLE baselines ADD COLUMN {column} {kind};"
                ))?;
            }
        }
        // The name `HEAD` gave an in-flight symbol that it renames (roadmap
        // D7 with C7), so that files still using the old name hear of it.
        // Added the same way: a row from before has none until its file is
        // compared again.
        let has: bool = connection.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('inflight') WHERE name = 'renamed_from'",
            [],
            |row| row.get(0),
        )?;
        if !has {
            connection.execute_batch("ALTER TABLE inflight ADD COLUMN renamed_from TEXT;")?;
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
    pub(super) fn baseline(
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
            "SELECT name, kind, signature, block_digest, line, end_line FROM baselines
             WHERE workspace_id = ?1 AND path = ?2",
        )?;
        let symbols = statement
            .query_map(params![workspace_id, path], |row| {
                Ok(Symbol {
                    name: row.get(0)?,
                    kind: SymbolKind::parse(&row.get::<_, String>(1)?)
                        .unwrap_or(SymbolKind::Function),
                    // 0: not recorded, which links nothing.
                    line: row.get::<_, Option<u32>>(4)?.unwrap_or(0),
                    end_line: row.get::<_, Option<u32>>(5)?.unwrap_or(0),
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

    pub(super) fn notice(
        &self,
        workspace_id: &str,
        path: &str,
        (before_digest, before): (String, FileGraph),
        (digest, after): (String, &FileGraph),
    ) -> Result<Option<Notice>, Failure> {
        // Parameter counts for the write-time break check (ADR 0029).
        let counts = |signature: &Option<String>| {
            crate::definitions::arity(Language::for_path(path)?, signature.as_deref()?)
        };
        let changes: Vec<InterfaceChange> = langspan_graph::diff(&before, after)
            .into_iter()
            .map(|change| {
                let (mut after_name, mut evidence) = (None, None);
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
                    Change::Renamed {
                        to,
                        before,
                        after,
                        evidence: why,
                    } => {
                        after_name = Some(to);
                        evidence = Some(why.name().to_owned());
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
                    before_name: after_name.is_some().then(|| change.name.clone()),
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
                    evidence,
                    before_path: None,
                    private: false,
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
            pass: 0,
            before_digest,
            digest,
            dependents: self.dependents(workspace_id, Some(path), &names)?,
            changes,
        }))
    }

    /// Files moved whole in this scan (roadmap D7), old path to new, found as
    /// Git finds an exact rename: a removed path whose indexed content
    /// parsed cleanly, and a path new to the index with exactly that
    /// content and the same language. A content shared by two removed or
    /// two new paths links none of them, and a file changed as well as
    /// moved is not found: both stay a removal and a first indexing.
    pub(super) fn moves(
        &self,
        workspace_id: &str,
        indexed: &BTreeMap<String, (Option<String>, Option<String>)>,
        removed: &[String],
        files: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, Failure> {
        let mut gone: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for path in removed {
            let Some((Some(digest), _)) = indexed.get(path) else {
                continue;
            };
            let clean: Option<Option<String>> = self
                .connection
                .query_row(
                    "SELECT baseline_digest FROM files WHERE workspace_id = ?1 AND path = ?2",
                    params![workspace_id, path],
                    |row| row.get(0),
                )
                .optional()?;
            if clean.flatten().as_ref() == Some(digest) {
                gone.entry(digest).or_default().push(path);
            }
        }
        let mut came: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (path, digest) in files
            .iter()
            .filter(|(path, _)| !indexed.contains_key(*path))
        {
            came.entry(digest).or_default().push(path);
        }
        let mut moved = BTreeMap::new();
        for (digest, from) in gone {
            if let ([from], Some([to])) = (from.as_slice(), came.get(digest).map(Vec::as_slice))
                && Language::for_path(from).is_some()
                && Language::for_path(from) == Language::for_path(to)
            {
                moved.insert((*from).to_owned(), (*to).to_owned());
            }
        }
        Ok(moved)
    }

    /// The notice for a file moved whole from `from` to `to`: each exported
    /// definition, by name and kind, kept its name and signature and moved
    /// with it, for the files that mention it. A file with no exports lists
    /// its private definitions instead, as indexed (still at `from`),
    /// marked `private` and told to no dependent, so that its notes and
    /// history follow it too. A file with no definitions at all is listed
    /// as itself: one private change of kind `file`, named by its file name.
    pub(super) fn moved_notice(
        &self,
        workspace_id: &str,
        from: &str,
        to: &str,
        (digest, before): (String, FileGraph),
    ) -> Result<Option<Notice>, Failure> {
        let exports = !before.symbols.is_empty();
        let mut definitions: Vec<(String, String, String)> = if exports {
            let mut seen = BTreeSet::new();
            before
                .symbols
                .iter()
                .filter(|symbol| seen.insert((symbol.name.as_str(), symbol.kind)))
                .filter_map(|symbol| {
                    let signature = written(&before, &symbol.name, symbol.kind)?;
                    Some((symbol.name.clone(), symbol.kind.name().into(), signature))
                })
                .collect()
        } else {
            let mut seen = BTreeSet::new();
            self.symbols_where("s.path = ?2", workspace_id, from, u32::MAX)?
                .into_iter()
                .filter(|symbol| {
                    !symbol.exported
                        && symbol.signature.len() <= 256
                        && !symbol.signature.chars().any(char::is_control)
                        && seen.insert((symbol.name.clone(), symbol.kind.clone()))
                })
                .map(|symbol| (symbol.name, symbol.kind, symbol.signature))
                .collect()
        };
        if definitions.is_empty() {
            // No definitions at all (an import-only module): the file itself,
            // by its name, so that the move is still recorded.
            let name = moved_file_name(from);
            definitions.push((name.clone(), "file".into(), name));
        }
        let changes: Vec<InterfaceChange> = definitions
            .into_iter()
            .map(|(name, kind, signature)| InterfaceChange {
                name: name.clone(),
                kind,
                change: "renamed".into(),
                before: Some(signature.clone()),
                after: Some(signature),
                after_name: Some(name.clone()),
                before_name: Some(name),
                evidence: Some(langspan_core::FILE_MOVED.into()),
                before_path: Some(from.into()),
                private: !exports,
                before_arity: None,
                after_arity: None,
                before_count: None,
                after_count: None,
            })
            .take(langspan_core::NOTICE_CHANGES)
            .collect();
        let names: Vec<&str> = changes.iter().map(|change| change.name.as_str()).collect();
        Ok(Some(Notice {
            path: to.into(),
            pass: 0,
            dependents: if exports {
                self.dependents(workspace_id, Some(from), &names)?
            } else {
                Vec::new()
            },
            digest: format!("moved from {from}"),
            before_digest: digest,
            changes,
        }))
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
    pub(super) fn start_again_at(
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
    pub(super) fn indexed_as(
        &self,
        workspace_id: &str,
        root: &Path,
        revision: u32,
    ) -> Result<bool, Failure> {
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

    /// Count one more indexing pass of the workspace and return its number,
    /// the clock `Retries` waits by.
    pub(super) fn next_pass(&self, workspace_id: &str) -> Result<i64, Failure> {
        Ok(self.connection.query_row(
            "INSERT INTO passes(workspace_id, count) VALUES (?1, 1)
             ON CONFLICT(workspace_id) DO UPDATE SET count = count + 1 RETURNING count",
            [workspace_id],
            |row| row.get(0),
        )?)
    }

    /// Write what the index holds for `of` to `out` in the export format
    /// (`crate::index_file`): only files indexed at some version, in path
    /// order, rows in the order they were stored. Signatures are the only
    /// source text in it. Stops with an error past `index_file::MAX_FILE_BYTES`
    /// or at `deadline`.
    pub fn export(
        &self,
        of: &crate::registration::Registration,
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
        of: &crate::registration::Registration,
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
}

/// For each call, in the same order, the name of the innermost definition
/// whose lines hold its line. Definitions nest, so one sweep in line order
/// with a stack of the open ones finds it.
pub(super) fn innermost<'a>(
    symbols: &'a [Symbol],
    calls: &[langspan_graph::Call],
) -> Vec<Option<&'a str>> {
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
pub(super) fn read_exact_version(
    root: &Path,
    path: &str,
    digest: &str,
    max: usize,
) -> Option<Vec<u8>> {
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
pub(super) const OTHER_SOURCE: &[&str] = &[
    "c", "cc", "clj", "cljs", "cpp", "cxx", "dart", "elm", "erl", "ex", "exs", "fs", "fsx",
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
pub(super) fn write_scan_blind(
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
