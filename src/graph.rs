//! A derived, rebuildable index of symbols and references per workspace.
//!
//! It lives in its own SQLite file beside the event store. Deleting the file
//! loses nothing: it is rebuilt from the working trees. Every row is keyed
//! by workspace and path and carries the digest of the exact content it was
//! extracted from, so each worktree is indexed as it is on disk. One thread
//! writes; the daemon's request path reads. See ADR 0008.

use head::Retries;
use langspan_core::InterfaceChange;
use langspan_graph::MAX_SOURCE_BYTES;
use langspan_graph::{FileGraph, Language};
use rusqlite::{Connection, params};
use std::{collections::BTreeMap, path::Path, time::Instant};
use write::{innermost, read_exact_version, write_scan_blind};

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
    /// The workspace's index pass that found it. A transition can repeat
    /// (a file moved `a → b → a → b`, content `A → B → A → B`); the pass
    /// tells each occurrence apart, and stays the same for every delivery
    /// of one.
    pub pass: i64,
    pub changes: Vec<InterfaceChange>,
    pub dependents: Vec<String>,
}

type Failure = Box<dyn std::error::Error + Send + Sync>;

mod head;
mod query;
mod write;

pub use head::InflightRow;
pub use query::{LAST_CHECK_RECORDS, MAP_ROWS, NAMES_PER_LEVEL, last_checks};
pub use write::{MAX_UNREAD, ScanBlind, ScanView, indexable};

impl Index {
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
        let mut indexed: Indexed = {
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
        // A file moved whole keeps it all under its new path instead.
        let removed: Vec<String> = indexed
            .keys()
            .filter(|path| !files.contains_key(*path) && !unobserved.contains(*path))
            .cloned()
            .collect();
        let moved = self.moves(workspace_id, &indexed, &removed, files)?;
        let mut notices = Vec::new();
        for path in &removed {
            let Some(baseline) = self.baseline(workspace_id, path)? else {
                continue;
            };
            notices.extend(match moved.get(path) {
                Some(to) => self.moved_notice(workspace_id, path, to, baseline)?,
                None => self.notice(
                    workspace_id,
                    path,
                    baseline,
                    ("deleted".into(), &FileGraph::default()),
                )?,
            });
        }
        let transaction = self.connection.transaction()?;
        for path in &removed {
            for table in ["files", "symbols", "refs", "imports", "baselines", "calls"] {
                match moved.get(path) {
                    Some(to) => transaction.execute(
                        &format!(
                            "UPDATE {table} SET path = ?3 WHERE workspace_id = ?1 AND path = ?2"
                        ),
                        params![workspace_id, path, to],
                    )?,
                    None => transaction.execute(
                        &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND path = ?2"),
                        params![workspace_id, path],
                    )?,
                };
            }
            // Compared against `HEAD` and retried at the old path: a moved
            // file is compared afresh at its new one.
            for table in ["inflight_files", "inflight", "retries"] {
                transaction.execute(
                    &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND path = ?2"),
                    params![workspace_id, path],
                )?;
            }
        }
        // A moved file's content is indexed already, under its new path.
        for (from, to) in &moved {
            if let Some(version) = indexed.remove(from) {
                indexed.insert(to.clone(), version);
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
                            block_digest, line, end_line)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            workspace_id,
                            path,
                            symbol.name,
                            symbol.kind.name(),
                            symbol.signature,
                            symbol.block_digest,
                            symbol.line,
                            symbol.end_line
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
        for notice in &mut notices {
            notice.pass = pass;
        }
        Ok(notices)
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    use super::{head::*, query::*, write::*};
    use langspan_core::{GraphCoverage, SymbolRef};
    use rusqlite::OptionalExtension;
    use std::fs;
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
            let file = dir.0.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
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
    fn of(root: &Path) -> crate::registration::Registration {
        crate::registration::Registration {
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
        let moved = crate::registration::Registration {
            root: dir.0.join("elsewhere"),
            ..of(&dir.0)
        };
        // Removed and registered again, or switched away and back.
        let removed = crate::registration::Registration {
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
        assert_eq!(
            Language::for_path("src/Shop/Orders.cs"),
            Some(Language::CSharp)
        );
    }

    const NOW: u64 = 1_700_000_000_000;

    const CALLER: (&str, &str) = ("src/main.rs", "fn main() { api::upload(); api::gate(); }\n");

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

    /// `symbol_paths` from `from` to `to` in workspace `w`, with the real
    /// bounds and a generous deadline: (chains as `PATH:LINE NAME` steps,
    /// edges visited, truncated).
    fn chains(index: &Index, from: &str, to: &str, hops: u32) -> (Vec<Vec<String>>, u32, bool) {
        let (paths, edges, truncated) = index
            .symbol_paths(
                "w",
                (from, to),
                hops,
                langspan_core::MAX_PATH_EDGES,
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        let steps = paths
            .iter()
            .map(|path| {
                path.steps
                    .iter()
                    .map(|step| format!("{}:{} {}", step.path, step.line, step.name))
                    .collect()
            })
            .collect();
        (steps, edges, truncated)
    }

    #[test]
    fn a_deadline_passed_while_expanding_callees_stops_the_search_and_says_so() {
        let (dir, mut index) = setup("paths-late-callee");
        pass(
            &dir,
            &mut index,
            &[
                (
                    "src/a.rs",
                    "fn handler() {\n    one();\n    two();\n    three();\n}\n",
                ),
                ("src/b.rs", "fn one() {}\nfn two() {}\nfn three() {}\n"),
            ],
        );
        // In time for the definition, late from its first callee on: the
        // search stops before reading any callee, even at its last hop.
        let mut asked = 0;
        let (paths, edges, truncated) = index
            .symbol_paths_by(
                "w",
                ("handler", "three"),
                1,
                langspan_core::MAX_PATH_EDGES,
                || {
                    asked += 1;
                    asked > 1
                },
            )
            .unwrap();
        assert!(paths.is_empty(), "{paths:?}");
        assert_eq!(edges, 0);
        assert!(truncated, "the deadline cut the search");
        // In time throughout, the same search finds the chain.
        let (found, edges, truncated) = chains(&index, "handler", "three", 1);
        assert_eq!(found, [["src/a.rs:4 handler", "src/b.rs:3 three"]]);
        assert!(edges == 3 && !truncated);
    }

    /// Issue #52, sixth criterion, with the clock held still: a search with
    /// more edges than `MAX_PATH_EDGES` stops at exactly that many and says
    /// `truncated`. The daemon's time budget can stop the same search sooner
    /// on a loaded machine, so the exact count is checked here.
    #[test]
    fn a_search_never_late_stops_at_exactly_its_edge_bound() {
        let (dir, mut index) = setup("paths-edge-bound");
        let mut hub = String::from("def hub():\n");
        for n in 0..=langspan_core::MAX_PATH_EDGES {
            hub.push_str(&format!("    f{n}()\n"));
        }
        let files = [
            ("hub.py", hub.as_str()),
            ("goal.py", "def goal():\n    return 1\n"),
        ];
        pass(&dir, &mut index, &files);
        let (paths, edges, truncated) = index
            .symbol_paths_by(
                "w",
                ("hub", "goal"),
                4,
                langspan_core::MAX_PATH_EDGES,
                || false,
            )
            .unwrap();
        assert!(paths.is_empty(), "{paths:?}");
        assert_eq!(edges, langspan_core::MAX_PATH_EDGES);
        assert!(truncated, "the edge bound cut the search");
    }

    #[test]
    fn a_map_area_is_a_module_path_cut_to_its_depth() {
        for (path, depth, area) in [
            ("src/api.ts", 1, "src/api"),
            ("src/api/index.ts", 1, "src/api"),
            ("src/api/handler.ts", 1, "src/api"),
            ("src/api/handler.ts", 2, "src/api/handler"),
            ("src/lib.rs", 1, "src"),
            ("src/store/mod.rs", 3, "src/store"),
            ("pkg/store/store.go", 1, "pkg/store"),
            ("app/__init__.py", 1, "app"),
            ("main.go", 1, "."),
            ("setup.py", 2, "setup"),
            // Only the last extension goes: dotted basenames stay apart.
            ("src/api.v1.ts", 1, "src/api.v1"),
            ("src/api.v2.ts", 1, "src/api.v2"),
            ("src/api.test.ts", 1, "src/api.test"),
            ("src/.eslintrc.js", 1, "src/.eslintrc"),
            // A declaration file is its module's.
            ("src/api.d.ts", 1, "src/api"),
            ("src/api/index.d.ts", 1, "src/api"),
            ("src/api/index.test.ts", 2, "src/api/index.test"),
            ("src/README", 1, "src/README"),
            // Each name stands for its directory only in its own language.
            ("src/lib.ts", 1, "src/lib"),
            ("tools/main.py", 1, "tools/main"),
            ("src/index.py", 1, "src/index"),
            ("src/main.rs", 1, "src"),
        ] {
            assert_eq!(map_area(path, depth), area, "{path} at {depth}");
        }
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
            langspan_graph::Language::CSharp => "src/A.cs",
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
            (
                CSharp,
                "public static int F(ref Foo x)",
                "public static int F(refFoo x)",
            ),
            (
                CSharp,
                "public async Task<T> F<T>(T x) where T : class",
                "public asyncTask<T> F<T>(T x) where T : class",
            ),
            (
                CSharp,
                r#"public string F(string x = @"a b")"#,
                r#"public string F(string x = @"ab")"#,
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
            (
                CSharp,
                "public  int  F( int[]  x ,  ref  T  y )",
                "public int F(int[] x, ref T y)",
            ),
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
    fn an_index_from_before_baseline_lines_tells_a_same_signature_rename_as_a_removal() {
        let (dir, mut index) = setup("lines");
        let before = (
            "src/api.rs",
            "pub fn fetch_user(id: u32) -> u32 {\n    id\n}\n",
        );
        pass(&dir, &mut index, &[before, CALLER]);
        // The baselines as the build before roadmap D7 wrote them.
        index
            .connection
            .execute_batch(
                "ALTER TABLE baselines DROP COLUMN line;
                 ALTER TABLE baselines DROP COLUMN end_line;",
            )
            .unwrap();
        drop(index);
        let mut index = Index::open(&dir.0.join("graph.sqlite3")).unwrap();
        let after = (
            "src/api.rs",
            "pub fn load_user(id: u32) -> u32 {\n    id + 1\n}\n",
        );
        let notices = pass(&dir, &mut index, &[after, CALLER]);
        let labels: Vec<&str> = notices[0]
            .changes
            .iter()
            .map(|change| change.change.as_str())
            .collect();
        assert_eq!(labels, ["removed", "added"], "{notices:?}");
    }

    #[test]
    fn an_index_from_before_block_digests_is_kept_and_tells_its_next_rename_as_a_removal() {
        let (dir, mut index) = setup("blocks");
        let before = (
            "src/api.rs",
            "pub fn fetch_user(id: u32) -> u32 {\n    id\n}\n",
        );
        pass(&dir, &mut index, &[before, CALLER]);
        // The baselines as an earlier build wrote them: no block digests,
        // and no lines (roadmap D7).
        index
            .connection
            .execute_batch(
                "ALTER TABLE baselines DROP COLUMN block_digest;
                 ALTER TABLE baselines DROP COLUMN line;
                 ALTER TABLE baselines DROP COLUMN end_line;
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

    /// Roadmap D7 with C7: a file moved whole keeps its definitions under
    /// the new path, while its in-flight set is compared afresh against the
    /// new path's `HEAD` and nothing is left under the old path.
    #[test]
    fn a_moved_file_keeps_its_definitions_and_is_compared_again_at_its_new_path() {
        let (dir, mut index) = repo("moved-inflight", &[("src/api.rs", b"pub fn upload() {}\n")]);
        fs::write(dir.0.join("src/api.rs"), GATE).unwrap();
        update(&dir, &mut index, &head_scan(&dir, &["src/api.rs".into()]));
        assert_eq!(in_flight(&index), ["src/api.rs: gate"]);
        fs::rename(dir.0.join("src/api.rs"), dir.0.join("src/client.rs")).unwrap();
        update(
            &dir,
            &mut index,
            &head_scan(&dir, &["src/client.rs".into()]),
        );
        let paths: Vec<String> = index
            .symbol("w", "upload", 10)
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.path)
            .collect();
        assert_eq!(paths, ["src/client.rs"]);
        // `HEAD` has no `src/client.rs`: both definitions are new there.
        assert_eq!(
            in_flight(&index),
            ["src/client.rs: gate", "src/client.rs: upload"]
        );
        assert!(!compared(&index, "src/api.rs"));
        assert!(compared(&index, "src/client.rs"));
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
