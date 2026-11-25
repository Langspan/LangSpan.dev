//! A derived, rebuildable index of symbols and references per workspace.
//!
//! It lives in its own SQLite file beside the event store. Deleting the file
//! loses nothing: it is rebuilt from the working trees. Every row is keyed
//! by workspace and path and carries the digest of the exact content it was
//! extracted from, so each worktree is indexed as it is on disk. One thread
//! writes; the daemon's request path reads. See ADR 0008.

use langspan_core::{GraphCoverage, InterfaceChange, NameRef, SymbolRef};
use langspan_graph::{Change, FileGraph, Language, Symbol, SymbolKind};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{collections::BTreeMap, fs, io::Read, path::Path, time::Instant};

const SCHEMA: i64 = 2;
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
                 DROP TABLE IF EXISTS baselines;
                 CREATE TABLE files (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL,
                    scan_digest TEXT NOT NULL, digest TEXT, partial INTEGER NOT NULL DEFAULT 0,
                    baseline_digest TEXT,
                    PRIMARY KEY (workspace_id, path)) WITHOUT ROWID;
                 CREATE TABLE symbols (
                    workspace_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                    kind TEXT NOT NULL, line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    signature TEXT NOT NULL, exported INTEGER NOT NULL);
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
                 PRAGMA user_version = 2;",
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
                let (label, before, after) = match change.change {
                    Change::Removed => ("removed", None, None),
                    Change::SignatureChanged { before, after } => {
                        ("signature_changed", Some(before), Some(after))
                    }
                    Change::Added => return None,
                };
                Some(InterfaceChange {
                    name: change.name,
                    kind: change.kind.name().into(),
                    change: label.into(),
                    before_arity: counts(&before),
                    after_arity: counts(&after),
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

    /// Bring one workspace's index up to `files`, the indexable paths of its
    /// latest complete scan with their content digests. A path in
    /// `unobserved` was listed but not versioned by that scan (a partial
    /// scan, a file over the size bound): it keeps what the index already
    /// holds for it and is not treated as removed. Parses at most
    /// `FILES_PER_PASS` files and stops at `deadline`; what is left is picked
    /// up by the next call. Returns the interface changes it found.
    pub fn update(
        &mut self,
        workspace_id: &str,
        root: &Path,
        files: &BTreeMap<String, String>,
        unobserved: &std::collections::BTreeSet<String>,
        deadline: Instant,
    ) -> Result<Vec<Notice>, Failure> {
        let indexed: BTreeMap<String, Option<String>> = {
            let mut statement = self
                .connection
                .prepare_cached("SELECT path, digest FROM files WHERE workspace_id = ?1")?;
            statement
                .query_map([workspace_id], |row| Ok((row.get(0)?, row.get(1)?)))?
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
            for table in ["files", "symbols", "refs", "imports", "baselines"] {
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
        transaction.commit()?;

        let pending: Vec<(&String, &String)> = files
            .iter()
            .filter(|(path, digest)| indexed.get(*path).and_then(Option::as_ref) != Some(digest))
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
            // the next scan brings its new digest.
            let Some(bytes) = read_exact_version(root, path, digest) else {
                continue;
            };
            let Ok(graph) = langspan_graph::extract(language, &bytes) else {
                continue;
            };
            let before = self.baseline(workspace_id, path)?;
            let transaction = self.connection.transaction()?;
            for table in ["symbols", "refs", "imports"] {
                transaction.execute(
                    &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND path = ?2"),
                    params![workspace_id, path],
                )?;
            }
            for symbol in &graph.symbols {
                transaction.execute(
                    "INSERT INTO symbols VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        workspace_id,
                        path,
                        symbol.name,
                        symbol.kind.name(),
                        symbol.line,
                        symbol.end_line,
                        symbol.signature,
                        symbol.exported
                    ],
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
            transaction.execute(
                "UPDATE files SET digest = ?3, partial = ?4 WHERE workspace_id = ?1 AND path = ?2",
                params![workspace_id, path, digest, graph.partial],
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
        let (indexable, current, partial): (u32, u32, u32) = self.connection.query_row(
            "SELECT count(*), coalesce(sum(digest IS scan_digest), 0), coalesce(sum(partial), 0)
             FROM files WHERE workspace_id = ?1",
            [workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(GraphCoverage {
            indexable_files: indexable,
            current_files: current,
            partial_files: partial,
            scanned: indexable > 0,
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
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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

/// The file's bytes, only if they are still exactly the scanned version.
fn read_exact_version(root: &Path, path: &str, digest: &str) -> Option<Vec<u8>> {
    let full = root.join(path);
    if !fs::symlink_metadata(&full).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&full)
        .ok()?
        .take(langspan_graph::MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= langspan_graph::MAX_SOURCE_BYTES
        && crate::sha256::hex(&crate::sha256::digest(&bytes)) == digest)
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
        let mut digests = BTreeMap::new();
        for (path, text) in files {
            fs::write(dir.0.join(path), text).unwrap();
            digests.insert(
                (*path).to_owned(),
                crate::sha256::hex(&crate::sha256::digest(text.as_bytes())),
            );
        }
        index
            .update(
                "w",
                &dir.0,
                &digests,
                &std::collections::BTreeSet::new(),
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap()
    }

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
