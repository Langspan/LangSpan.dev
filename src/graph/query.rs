//! Read queries over the index: symbols, references, impact, paths
//! between symbols and the repository map.

use super::write::read_exact_version;
use super::*;
use langspan_core::{GraphCoverage, NameRef, SymbolRef};
use langspan_graph::Language;
use langspan_graph::MAX_SOURCE_BYTES;
use rusqlite::{OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

impl Index {
    /// The files of `workspace_id` that mention any of `names`, other than
    /// `except`, in path order, at most `NOTICE_DEPENDENTS`: an interface
    /// notice's dependents in its own workspace, and in another worktree of
    /// the repository (issue #194).
    pub fn dependents(
        &self,
        workspace_id: &str,
        except: Option<&str>,
        names: &[&str],
    ) -> Result<Vec<String>, Failure> {
        let mut found = std::collections::BTreeSet::new();
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT path FROM refs
             WHERE workspace_id = ?1 AND name = ?2 AND path != ?3 ORDER BY path LIMIT ?4",
        )?;
        // No indexed path is empty, so `""` excludes nothing.
        let except = except.unwrap_or("");
        for name in names {
            let limit = langspan_core::NOTICE_DEPENDENTS as i64;
            for row in statement.query_map(params![workspace_id, name, except, limit], |row| {
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
        of: &crate::registration::Registration,
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

    pub(super) fn symbols_where(
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
            .query_map(params![workspace_id, value, limit], symbol_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Definitions of `name` in the file `path`, in line order, at most
    /// `limit`.
    pub fn symbols_at(
        &self,
        workspace_id: &str,
        path: &str,
        name: &str,
        limit: u32,
    ) -> Result<Vec<SymbolRef>, Failure> {
        let mut statement = self.connection.prepare_cached(
            "SELECT s.path, s.name, s.kind, s.line, s.end_line, s.signature, s.exported, f.digest
             FROM symbols s JOIN files f ON f.workspace_id = s.workspace_id AND f.path = s.path
             WHERE s.workspace_id = ?1 AND s.path = ?2 AND s.name = ?3
             ORDER BY s.line LIMIT ?4",
        )?;
        let rows = statement
            .query_map(params![workspace_id, path, name, limit], symbol_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Definitions named `name` with the digest of each one's own bytes, in
    /// the file `path` or, when `None`, the whole workspace: at most
    /// `limit + 1`, in path and line order (roadmap E1). The path is
    /// applied before the limit, so definitions elsewhere never crowd out
    /// the file's own (review comment 4713922965).
    pub fn anchor_definitions(
        &self,
        workspace_id: &str,
        path: Option<&str>,
        name: &str,
        limit: u32,
    ) -> Result<Vec<SymbolRef>, Failure> {
        let mut statement = self.connection.prepare_cached(match path {
            Some(_) => {
                "SELECT path, name, kind, line, end_line, body_digest FROM symbols
                 WHERE workspace_id = ?1 AND name = ?2 AND path = ?4
                 ORDER BY line LIMIT ?3"
            }
            None => {
                "SELECT path, name, kind, line, end_line, body_digest FROM symbols
                 WHERE workspace_id = ?1 AND name = ?2 AND ?4 IS NULL
                 ORDER BY path, line LIMIT ?3"
            }
        })?;
        let rows = statement
            .query_map(params![workspace_id, name, limit + 1, path], |row| {
                Ok(SymbolRef {
                    path: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    line: row.get(3)?,
                    end_line: row.get(4)?,
                    body_digest: Some(row.get(5)?),
                    ..SymbolRef::default()
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Of `paths`, those indexed at the content the last scan read, without
    /// a syntax error or a bound reached: the files whose definitions the
    /// index can vouch for now (roadmap E1).
    pub fn fresh(
        &self,
        workspace_id: &str,
        paths: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, Failure> {
        let mut statement = self.connection.prepare_cached(
            "SELECT 1 FROM files WHERE workspace_id = ?1 AND path = ?2
             AND digest IS scan_digest AND partial = 0 AND NOT EXISTS (
                SELECT 1 FROM unread u WHERE u.workspace_id = ?1 AND u.path = ?2)",
        )?;
        let mut fresh = BTreeSet::new();
        for path in paths {
            if statement
                .query_row(params![workspace_id, path], |_| Ok(()))
                .optional()?
                .is_some()
            {
                fresh.insert(path.clone());
            }
        }
        Ok(fresh)
    }

    /// Of `paths`, those the index holds no file row for: deleted, moved
    /// out of scope or never read (roadmap E1).
    pub fn absent(
        &self,
        workspace_id: &str,
        paths: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, Failure> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT 1 FROM files WHERE workspace_id = ?1 AND path = ?2")?;
        let mut absent = BTreeSet::new();
        for path in paths {
            if statement
                .query_row(params![workspace_id, path], |_| Ok(()))
                .optional()?
                .is_none()
            {
                absent.insert(path.clone());
            }
        }
        Ok(absent)
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

    /// The definitions of `path` for its outline (roadmap A11): how many
    /// the index holds, and the `limit` largest by lines spanned (the
    /// earlier of equal ones), in line order. Only when the index holds
    /// that file whole at `digest`; `None` when it holds another version, a
    /// partial parse, or no row for it.
    pub fn outline(
        &self,
        workspace_id: &str,
        path: &str,
        digest: &str,
        limit: u32,
    ) -> Result<Option<(u32, Vec<SymbolRef>)>, Failure> {
        let whole: bool = self
            .connection
            .prepare_cached(
                "SELECT count(*) > 0 FROM files WHERE workspace_id = ?1 AND path = ?2
                 AND digest = ?3 AND partial = 0",
            )?
            .query_row(params![workspace_id, path, digest], |row| row.get(0))?;
        if !whole {
            return Ok(None);
        }
        let total: u32 = self
            .connection
            .prepare_cached("SELECT count(*) FROM symbols WHERE workspace_id = ?1 AND path = ?2")?
            .query_row(params![workspace_id, path], |row| row.get(0))?;
        let mut symbols = self
            .connection
            .prepare_cached(
                "SELECT s.path, s.name, s.kind, s.line, s.end_line, s.signature, s.exported, f.digest
                 FROM symbols s JOIN files f ON f.workspace_id = s.workspace_id AND f.path = s.path
                 WHERE s.workspace_id = ?1 AND s.path = ?2
                 ORDER BY s.end_line - s.line DESC, s.line, s.end_line LIMIT ?3",
            )?
            .query_map(params![workspace_id, path, limit], symbol_row)?
            .collect::<Result<Vec<_>, _>>()?;
        symbols.sort_by_key(|symbol| (symbol.line, symbol.end_line));
        Ok(Some((total, symbols)))
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

    /// Definitions named `name` to rank (roadmap C2): the first
    /// `RANK_CANDIDATES` in path order, and the first `RANK_CANDIDATES` in
    /// the asking session's and its peers' files in rank order (its own
    /// files, then its peers', then exported ones, then path order), with
    /// how many definitions the name has in all. Path order alone would let
    /// a file of many definitions crowd out a later-sorting own file
    /// (review comment 4709803063).
    pub fn rank_candidates(
        &self,
        workspace_id: &str,
        name: &str,
        inputs: &langspan_core::RankInputs,
    ) -> Result<(Vec<SymbolRef>, u32), Failure> {
        let mut found = self.symbol(
            workspace_id,
            name,
            langspan_core::RANK_CANDIDATES.saturating_sub(1),
        )?;
        // One bound value carries the name and both lists of paths.
        let own: Vec<&String> = inputs.own.keys().collect();
        let peers: Vec<&String> = inputs.peers.keys().collect();
        let asked = serde_json::json!({"name": name, "own": own, "peers": peers}).to_string();
        let mut statement = self.connection.prepare_cached(
            "SELECT s.path, s.name, s.kind, s.line, s.end_line, s.signature, s.exported, f.digest
             FROM symbols s JOIN files f ON f.workspace_id = s.workspace_id AND f.path = s.path
             WHERE s.workspace_id = ?1 AND s.name = json_extract(?2, '$.name')
             AND (s.path IN (SELECT value FROM json_each(?2, '$.own'))
                  OR s.path IN (SELECT value FROM json_each(?2, '$.peers')))
             ORDER BY s.path IN (SELECT value FROM json_each(?2, '$.own')) DESC,
                      s.path IN (SELECT value FROM json_each(?2, '$.peers')) DESC,
                      s.exported DESC, s.path, s.line
             LIMIT ?3",
        )?;
        let more = statement
            .query_map(
                params![workspace_id, asked, langspan_core::RANK_CANDIDATES],
                symbol_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        // A definition read by both queries is one row twice: drop the copy.
        found.extend(more);
        let key = |s: &SymbolRef| {
            let (path, kind, signature) = (s.path.clone(), s.kind.clone(), s.signature.clone());
            (path, s.line, s.end_line, kind, signature)
        };
        found.sort_by_key(key);
        found.dedup();
        let total = self
            .connection
            .prepare_cached("SELECT count(*) FROM symbols WHERE workspace_id = ?1 AND name = ?2")?;
        let total = { total }.query_row(params![workspace_id, name], |row| row.get(0))?;
        Ok((found, total))
    }

    /// Files that mention `name`, each with its count of mentions: the
    /// `RANK_CANDIDATES` with the most (then in path order) and every one
    /// in `paths`, with how many files mention it in all (roadmap C2).
    pub fn mention_counts(
        &self,
        workspace_id: &str,
        name: &str,
        paths: &[String],
    ) -> Result<(Vec<(String, u32)>, u32), Failure> {
        let listed = serde_json::to_string(paths)?;
        let mut statement = self.connection.prepare_cached(
            "SELECT path, count(*) AS mentions FROM refs WHERE workspace_id = ?1 AND name = ?2
             GROUP BY path ORDER BY mentions DESC, path LIMIT ?3",
        )?;
        let mut files: BTreeMap<String, u32> = statement
            .query_map(
                params![workspace_id, name, langspan_core::RANK_CANDIDATES],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
            .collect::<Result<_, _>>()?;
        let mut statement = self.connection.prepare_cached(
            "SELECT path, count(*) FROM refs WHERE workspace_id = ?1 AND name = ?2
             AND path IN (SELECT value FROM json_each(?3)) GROUP BY path",
        )?;
        let listed = statement
            .query_map(params![workspace_id, name, listed], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<Vec<(String, u32)>, _>>()?;
        files.extend(listed);
        let total = self
            .connection
            .prepare_cached(
                "SELECT count(DISTINCT path) FROM refs WHERE workspace_id = ?1 AND name = ?2",
            )?
            .query_row(params![workspace_id, name], |row| row.get(0))?;
        Ok((files.into_iter().collect(), total))
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

    /// The shortest chains of calls from a definition named `from` to one
    /// named `to`, at most `max_hops` calls long (roadmap D3), breadth first.
    /// A call in a definition's lines of a name reaches that name's
    /// definitions in the files the calling file imports by path, when there
    /// are any (`resolution: "path"`), and otherwise every definition of the
    /// name (`"name"`); more than one is `ambiguous`. Each definition is
    /// visited once, at its first hop. At most `MAX_PATHS` chains, in step
    /// order. The search stops after `max_edges` edges (each definition a
    /// call reaches, or the call alone when it reaches none) or at
    /// `deadline`, and returns what it found with `truncated`. Returns the
    /// chains, the edges visited and `truncated`. Call it inside
    /// `with_coverage`.
    pub fn symbol_paths(
        &self,
        workspace_id: &str,
        (from, to): (&str, &str),
        max_hops: u32,
        max_edges: u32,
        deadline: Instant,
    ) -> Result<(Vec<langspan_core::SymbolPath>, u32, bool), Failure> {
        self.symbol_paths_by(workspace_id, (from, to), max_hops, max_edges, || {
            Instant::now() >= deadline
        })
    }

    /// `symbol_paths`, with `late` saying whether the deadline has passed.
    pub(super) fn symbol_paths_by(
        &self,
        workspace_id: &str,
        (from, to): (&str, &str),
        max_hops: u32,
        max_edges: u32,
        mut late: impl FnMut() -> bool,
    ) -> Result<(Vec<langspan_core::SymbolPath>, u32, bool), Failure> {
        let mut search = PathSearch::default();
        let starts = self.definitions(workspace_id, from, MAX_RESULTS + 1)?;
        search.truncated = starts.len() > MAX_RESULTS as usize;
        search.start_ambiguous = starts.len() > 1;
        let mut frontier: Vec<usize> = starts
            .into_iter()
            .take(MAX_RESULTS as usize)
            .map(|definition| search.reach(definition, 0, None))
            .collect();
        let mut imports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut edges = 0u32;
        let mut stopped = false;
        let mut found = Vec::new();
        for hop in 0..=max_hops {
            found = frontier
                .iter()
                .copied()
                .filter(|&node| search.nodes[node].name == to)
                .collect();
            if !found.is_empty() || hop == max_hops || stopped {
                break;
            }
            let mut next = Vec::new();
            'expand: for &node in &frontier {
                if late() {
                    (search.truncated, stopped) = (true, true);
                    break;
                }
                let at = search.nodes[node].clone();
                if !imports.contains_key(&at.path) {
                    let resolved = self.resolved_imports(workspace_id, &at.path)?;
                    imports.insert(at.path.clone(), resolved);
                }
                for (callee, line) in self.callees(workspace_id, &at)? {
                    if edges >= max_edges || late() {
                        (search.truncated, stopped) = (true, true);
                        break 'expand;
                    }
                    let room = max_edges - edges;
                    // The imported files' definitions are read on their own,
                    // so the bound never drops one of them for another
                    // file's definition of the name.
                    let imported = &imports[&at.path];
                    let by_path =
                        self.definitions_in(workspace_id, &callee, imported, MAX_RESULTS + 1)?;
                    let (mut reached, resolution) = if by_path.is_empty() {
                        let any = self.definitions(workspace_id, &callee, MAX_RESULTS + 1)?;
                        (any, "name")
                    } else {
                        (by_path, "path")
                    };
                    let ambiguous = reached.len() > 1;
                    if reached.len() > room.min(MAX_RESULTS) as usize {
                        reached.truncate(room.min(MAX_RESULTS) as usize);
                        search.truncated = true;
                    }
                    edges += u32::try_from(reached.len()).unwrap_or(u32::MAX).max(1);
                    for definition in reached {
                        let via = Via {
                            from: node,
                            line,
                            resolution,
                            ambiguous,
                        };
                        if let Some(new) = search.reach_by(definition, hop + 1, via) {
                            next.push(new);
                        }
                    }
                }
            }
            frontier = next;
        }
        let (paths, more) = search.chains(&found, langspan_core::MAX_PATHS);
        Ok((paths, edges, search.truncated || more))
    }

    /// Definitions named `name` that a call can reach (functions, methods
    /// and types, whose call is a constructor's), in path and line order, at
    /// most `limit`. A module, constant or interface of the name is none.
    fn definitions(
        &self,
        workspace_id: &str,
        name: &str,
        limit: u32,
    ) -> Result<Vec<Definition>, Failure> {
        let rows = self
            .connection
            .prepare_cached(
                "SELECT path, line, end_line, name FROM symbols
                 WHERE workspace_id = ?1 AND name = ?2
                 AND kind IN ('function', 'method', 'type')
                 ORDER BY path, line LIMIT ?3",
            )?
            .query_map(params![workspace_id, name, limit], |row| {
                Ok(Definition {
                    path: row.get(0)?,
                    line: row.get(1)?,
                    end_line: row.get(2)?,
                    name: row.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// `definitions` of `name` in `paths` only, in path and line order, at
    /// most `limit`.
    fn definitions_in(
        &self,
        workspace_id: &str,
        name: &str,
        paths: &BTreeSet<String>,
        limit: u32,
    ) -> Result<Vec<Definition>, Failure> {
        let mut statement = self.connection.prepare_cached(
            "SELECT line, end_line FROM symbols
             WHERE workspace_id = ?1 AND path = ?2 AND name = ?3
             AND kind IN ('function', 'method', 'type')
             ORDER BY line LIMIT ?4",
        )?;
        let mut found = Vec::new();
        for path in paths {
            let room = limit.saturating_sub(u32::try_from(found.len()).unwrap_or(u32::MAX));
            if room == 0 {
                break;
            }
            let rows = statement
                .query_map(params![workspace_id, path, name, room], |row| {
                    Ok(Definition {
                        path: path.clone(),
                        line: row.get(0)?,
                        end_line: row.get(1)?,
                        name: name.to_owned(),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            found.extend(rows);
        }
        Ok(found)
    }

    /// The names called within `definition`'s lines, each with its first
    /// call's line, in line order.
    fn callees(
        &self,
        workspace_id: &str,
        definition: &Definition,
    ) -> Result<Vec<(String, u32)>, Failure> {
        let rows = self
            .connection
            .prepare_cached(
                "SELECT callee, min(line) FROM calls
                 WHERE workspace_id = ?1 AND path = ?2 AND line BETWEEN ?3 AND ?4
                 GROUP BY callee ORDER BY min(line), callee",
            )?
            .query_map(
                params![
                    workspace_id,
                    definition.path,
                    definition.line,
                    definition.end_line
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The workspace files that `path`'s imports name, resolved by path
    /// (`langspan_graph::resolve_import`) against the files the last scan
    /// listed.
    fn resolved_imports(
        &self,
        workspace_id: &str,
        path: &str,
    ) -> Result<BTreeSet<String>, Failure> {
        let targets: Vec<String> = self
            .connection
            .prepare_cached("SELECT target FROM imports WHERE workspace_id = ?1 AND path = ?2")?
            .query_map(params![workspace_id, path], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        let mut exists = self
            .connection
            .prepare_cached("SELECT 1 FROM files WHERE workspace_id = ?1 AND path = ?2")?;
        let mut resolved = BTreeSet::new();
        let mut failed = None;
        for target in targets {
            let found = langspan_graph::resolve_import(path, &target, |candidate| {
                match exists.exists(params![workspace_id, candidate]) {
                    Ok(found) => found,
                    Err(error) => {
                        failed.get_or_insert(error);
                        false
                    }
                }
            });
            resolved.extend(found);
        }
        match failed {
            Some(error) => Err(error.into()),
            None => Ok(resolved),
        }
    }

    /// The workspace's areas at `depth` and the edges between them (roadmap
    /// D3). A file's area is its module path's first `depth + 1` parts (see
    /// `map_area`). Edges are `imports` resolved by path and `calls` of a
    /// name with exactly one definition, counted per pair of different
    /// areas and kind. Reads at most `MAP_ROWS` imports and as many calls;
    /// past that, or past `MAX_MAP_AREAS` areas or `MAX_MAP_EDGES` edges, the
    /// answer is `truncated`. Areas in path order; edges by count, most
    /// first, then by area and kind. Call it inside `with_coverage`.
    pub fn map(
        &self,
        workspace_id: &str,
        depth: u32,
    ) -> Result<
        (
            Vec<langspan_core::MapArea>,
            Vec<langspan_core::MapEdge>,
            bool,
        ),
        Failure,
    > {
        let connection = &self.connection;
        let files: BTreeSet<String> = connection
            .prepare_cached("SELECT path FROM files WHERE workspace_id = ?1")?
            .query_map([workspace_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        let exported: BTreeMap<String, u32> = connection
            .prepare_cached(
                "SELECT path, count(*) FROM symbols WHERE workspace_id = ?1 AND exported = 1
                 GROUP BY path",
            )?
            .query_map([workspace_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut areas: BTreeMap<String, langspan_core::MapArea> = BTreeMap::new();
        for path in &files {
            let area = areas.entry(map_area(path, depth)).or_default();
            area.files += 1;
            area.exported += exported.get(path).copied().unwrap_or(0);
            if let Some(language) = Language::for_path(path).map(Language::name)
                && !area.languages.iter().any(|known| known == language)
            {
                area.languages.push(language.into());
                area.languages.sort();
            }
        }
        let mut counts: BTreeMap<(String, String, &str), u32> = BTreeMap::new();
        let mut count = |from: &str, to: &str, kind, n: u32| {
            let (from, to) = (map_area(from, depth), map_area(to, depth));
            if from != to {
                *counts.entry((from, to, kind)).or_default() += n;
            }
        };
        let imports: Vec<(String, String)> = connection
            .prepare_cached(
                "SELECT path, target FROM imports WHERE workspace_id = ?1
                 ORDER BY path, line, target LIMIT ?2",
            )?
            .query_map(params![workspace_id, MAP_ROWS + 1], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut truncated = imports.len() > MAP_ROWS as usize;
        for (path, target) in imports.iter().take(MAP_ROWS as usize) {
            if let Some(to) =
                langspan_graph::resolve_import(path, target, |candidate| files.contains(candidate))
            {
                count(path, &to, "imports", 1);
            }
        }
        let calls: Vec<(String, String, u32)> = connection
            .prepare_cached(
                "SELECT c.path, s.path, count(*)
                 FROM (SELECT path, callee FROM calls WHERE workspace_id = ?1
                       ORDER BY path, line LIMIT ?2) c
                 JOIN (SELECT name, min(path) AS path FROM symbols WHERE workspace_id = ?1
                       AND kind IN ('function', 'method', 'type')
                       GROUP BY name HAVING count(*) = 1) s ON s.name = c.callee
                 GROUP BY c.path, s.path",
            )?
            .query_map(params![workspace_id, MAP_ROWS], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        for (from, to, n) in &calls {
            count(from, to, "calls", *n);
        }
        truncated |= connection
            .prepare_cached(
                "SELECT count(*) > ?2 FROM (SELECT 1 FROM calls WHERE workspace_id = ?1 LIMIT ?2 + 1)",
            )?
            .query_row(params![workspace_id, MAP_ROWS], |row| row.get::<_, bool>(0))?;
        let mut edges: Vec<langspan_core::MapEdge> = counts
            .into_iter()
            .map(|((from, to, kind), count)| langspan_core::MapEdge {
                from,
                to,
                kind: kind.into(),
                resolution: if kind == "imports" { "path" } else { "name" }.into(),
                count,
            })
            .collect();
        edges.sort_by(|a, b| {
            (b.count, &a.from, &a.to, &a.kind).cmp(&(a.count, &b.from, &b.to, &b.kind))
        });
        truncated |= areas.len() > langspan_core::MAX_MAP_AREAS
            || edges.len() > langspan_core::MAX_MAP_EDGES;
        edges.truncate(langspan_core::MAX_MAP_EDGES);
        let areas = areas
            .into_iter()
            .take(langspan_core::MAX_MAP_AREAS)
            .map(|(path, area)| langspan_core::MapArea { path, ..area })
            .collect();
        Ok((areas, edges, truncated))
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

/// Import rows, and call rows, one `map` reads at most.
pub const MAP_ROWS: u32 = 200_000;

/// The area of `path` at `depth` in a `map`: its module path cut to its
/// first `depth + 1` parts, or `.` for the root. A file's module path is its
/// path without the extension, or its directory for a Go file (a package is
/// a directory) and, each in its own language, for `mod.rs`, `lib.rs`,
/// `main.rs`, `index.*` and `__init__.py`, so `src/api.ts`, `src/api/index.ts` and
/// `src/api/handler.ts` are all in `src/api` at depth 1. Only the last
/// extension goes (a `.d.ts` declaration file loses both), so `src/api.v1.ts`
/// and `src/api.v2.ts` are two areas.
pub(super) fn map_area(path: &str, depth: u32) -> String {
    let mut parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let file = parts.pop().unwrap_or_default();
    let stem = file
        .strip_suffix(".d.ts")
        .or_else(|| file.rsplit_once('.').map(|(stem, _)| stem))
        .unwrap_or(file);
    let by_directory = match Language::for_path(path) {
        Some(Language::Go) => true,
        Some(Language::Rust) => matches!(stem, "mod" | "lib" | "main"),
        Some(Language::TypeScript | Language::Tsx) => stem == "index",
        Some(Language::Python) => stem == "__init__",
        _ => false,
    };
    if !by_directory && !stem.is_empty() {
        parts.push(stem);
    }
    parts.truncate(depth as usize + 1);
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// A definition a `symbol_paths` search can stand at.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Definition {
    path: String,
    line: u32,
    end_line: u32,
    name: String,
}

/// How one definition was reached: by a call in `from` at `line`.
#[derive(Debug, Clone, Copy)]
struct Via {
    from: usize,
    line: u32,
    resolution: &'static str,
    ambiguous: bool,
}

/// The state of one `symbol_paths` search: every definition reached, the
/// hop it was first reached at, and each call that reached it at that hop.
#[derive(Default)]
struct PathSearch {
    nodes: Vec<Definition>,
    hops: Vec<u32>,
    vias: Vec<Vec<Via>>,
    index: BTreeMap<Definition, usize>,
    start_ambiguous: bool,
    truncated: bool,
}

impl PathSearch {
    /// Record `definition` at `hop`; returns its node.
    fn reach(&mut self, definition: Definition, hop: u32, via: Option<Via>) -> usize {
        let node = self.nodes.len();
        self.index.insert(definition.clone(), node);
        self.nodes.push(definition);
        self.hops.push(hop);
        self.vias.push(via.into_iter().collect());
        node
    }

    /// Record that `via` reaches `definition` at `hop`. Returns the node when
    /// it is new; a definition first reached at this hop keeps `via` as one
    /// more way in, and one reached earlier ignores it.
    fn reach_by(&mut self, definition: Definition, hop: u32, via: Via) -> Option<usize> {
        match self.index.get(&definition) {
            None => Some(self.reach(definition, hop, Some(via))),
            Some(&node) => {
                if self.hops[node] == hop {
                    self.vias[node].push(via);
                }
                None
            }
        }
    }

    /// Up to `limit` chains ending at `targets`, sorted by their steps, and
    /// whether there were more.
    fn chains(&self, targets: &[usize], limit: usize) -> (Vec<langspan_core::SymbolPath>, bool) {
        // Each chain as its nodes and the vias between them, built backwards
        // from its target. Collect one past the limit to know there is more.
        let mut found: Vec<Vec<(usize, Option<Via>)>> = Vec::new();
        let mut stack: Vec<Vec<(usize, Option<Via>)>> =
            targets.iter().map(|&node| vec![(node, None)]).collect();
        stack.reverse();
        while let Some(chain) = stack.pop() {
            if found.len() > limit {
                break;
            }
            let (node, _) = chain[chain.len() - 1];
            if self.hops[node] == 0 {
                found.push(chain);
                continue;
            }
            for via in self.vias[node].iter().rev() {
                let mut longer = chain.clone();
                let last = longer.len() - 1;
                longer[last].1 = Some(*via);
                longer.push((via.from, None));
                stack.push(longer);
            }
        }
        let more = found.len() > limit;
        let mut paths: Vec<langspan_core::SymbolPath> = found
            .into_iter()
            .map(|mut chain| {
                chain.reverse();
                self.chain(&chain)
            })
            .collect();
        paths.sort_by(|a, b| {
            let key = |path: &langspan_core::SymbolPath| {
                path.steps
                    .iter()
                    .map(|step| (step.path.clone(), step.line, step.name.clone()))
                    .collect::<Vec<_>>()
            };
            key(a).cmp(&key(b))
        });
        paths.truncate(limit);
        (paths, more)
    }

    /// One chain, first node first, each with the via that reached it.
    fn chain(&self, chain: &[(usize, Option<Via>)]) -> langspan_core::SymbolPath {
        // The via into node i is stored with node i: the first has none.
        let how = |via: Option<Via>| match via {
            Some(via) => (via.resolution, via.ambiguous),
            None => ("name", self.start_ambiguous),
        };
        let mut steps = Vec::new();
        for (index, &(node, via)) in chain.iter().enumerate() {
            let definition = &self.nodes[node];
            let (resolution, ambiguous) = how(via);
            let step = match chain.get(index + 1) {
                Some(&(next, Some(into))) => langspan_core::PathStep {
                    path: definition.path.clone(),
                    line: into.line,
                    name: definition.name.clone(),
                    edge: "calls".into(),
                    callee: Some(self.nodes[next].name.clone()),
                    resolution: resolution.into(),
                    ambiguous,
                },
                _ => langspan_core::PathStep {
                    path: definition.path.clone(),
                    line: definition.line,
                    name: definition.name.clone(),
                    edge: "defines".into(),
                    callee: None,
                    resolution: resolution.into(),
                    ambiguous,
                },
            };
            steps.push(step);
        }
        langspan_core::SymbolPath {
            ambiguous: steps.iter().any(|step| step.ambiguous),
            steps,
        }
    }
}

/// One definition row: path, name, kind, line, end line, signature,
/// exported and the file's digest, in that order.
fn symbol_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolRef> {
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
