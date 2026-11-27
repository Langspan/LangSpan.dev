//! The head graph: each changed file's exported symbols compared with
//! its version at `HEAD` (roadmap C7), and the bounded retries of reads
//! that may pass later.

use super::*;
use langspan_graph::MAX_SOURCE_BYTES;
use langspan_graph::{Change, FileGraph, Language, Symbol, SymbolKind};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

impl Index {
    /// Bring the workspace's in-flight set up to the scan (roadmap C7): for
    /// each file that differs from `HEAD` and is indexed at its scanned
    /// content, the exported symbols that `HEAD`'s version of the file
    /// lacks or has with another signature. A file the scan found clean,
    /// or no longer lists, has none. A file parsed partially, at either
    /// version, says nothing. Reads at most `FILES_PER_PASS` blobs from Git
    /// and stops at `deadline`; the rest wait for the next pass.
    pub(super) fn update_inflight(
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
                    Some(head) => {
                        let renamed = self.renamed_since(workspace_id, path, &head)?;
                        self.added_or_changed(workspace_id, path, &head)?
                            .into_iter()
                            .map(|symbol| {
                                let from =
                                    renamed.get(&(symbol.name.clone(), symbol.kind)).cloned();
                                (symbol, from)
                            })
                            .collect()
                    }
                    None => Vec::new(),
                }
            };
            let transaction = self.connection.transaction()?;
            transaction.execute(
                "DELETE FROM inflight WHERE workspace_id = ?1 AND path = ?2",
                params![workspace_id, path],
            )?;
            for (symbol, renamed_from) in &inflight {
                transaction.execute(
                    "INSERT INTO inflight(workspace_id, path, name, kind, signature, renamed_from)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        workspace_id,
                        path,
                        symbol.name,
                        symbol.kind.name(),
                        symbol.signature,
                        renamed_from
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

    /// The renames from `head` to the indexed version of `path`, as an
    /// interface notice finds them (roadmap D7): each new name and kind with
    /// the name `head` gave it. A file moved whole is compared at its new
    /// path, where `HEAD` has no such file, so it renames nothing here.
    fn renamed_since(
        &self,
        workspace_id: &str,
        path: &str,
        head: &FileGraph,
    ) -> Result<BTreeMap<(String, SymbolKind), String>, Failure> {
        let Some((_, now)) = self.baseline(workspace_id, path)? else {
            return Ok(BTreeMap::new());
        };
        Ok(langspan_graph::diff(head, &now)
            .into_iter()
            .filter_map(|change| match change.change {
                Change::Renamed { to, .. } => Some(((to, change.kind), change.name)),
                _ => None,
            })
            .collect())
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
            "SELECT path, name, kind, signature, renamed_from FROM inflight WHERE workspace_id = ?1
                AND (?2 IS NULL OR (path, name, kind, signature) > (?2, ?3, ?4, ?5))
             ORDER BY path, name, kind, signature LIMIT ?6",
        )?;
        let (path, name, kind, signature) = after.map_or((None, None, None, None), |row| {
            (Some(&row.0), Some(&row.1), Some(&row.2), Some(&row.3))
        });
        let rows = statement
            .query_map(
                params![workspace_id, path, name, kind, signature, limit as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
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
/// is split into words: Go, Java and C# by the TypeScript and JavaScript rules
/// (words, quoted and backtick literals, `//` and `/* */` comments), as
/// the rules for other files drop all whitespace, and `chan int` would
/// then equal `chanint` (review comment 4704678366).
pub(super) fn signature_tokens(path: &str, signature: &str) -> Vec<Vec<u8>> {
    use crate::reformat::Language as Tokens;
    let language = match Language::for_path(path) {
        Some(Language::Rust) => Tokens::Rust,
        Some(Language::Python) => Tokens::Python,
        Some(
            Language::TypeScript | Language::Tsx | Language::Go | Language::Java | Language::CSharp,
        ) => Tokens::Script,
        None => crate::reformat::language(path),
    };
    crate::reformat::tokens(signature.as_bytes(), language)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect()
}

/// One in-flight symbol as the index holds it: `(path, name, kind,
/// signature)`.
pub type InflightRow = (String, String, String, String, Option<String>);

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
pub(super) struct Retries {
    workspace_id: String,
    stage: &'static str,
    pass: i64,
    /// Each path's failing version, its failures and the pass it may next
    /// be tried at.
    by_path: BTreeMap<String, (String, u32, i64)>,
}

impl Retries {
    /// Reading the working file to index it.
    pub(super) const INDEX: &'static str = "index";
    /// Reading the `HEAD` blob to compare it with the indexed version.
    pub(super) const COMPARE: &'static str = "compare";
    /// Failures after which a version is given up on until it changes.
    /// With the waits below, about 160 passes after the first failure.
    pub(super) const ATTEMPTS: u32 = 10;
    /// The longest wait between two tries, in passes.
    const MAX_WAIT: i64 = 32;

    pub(super) fn load(
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
    pub(super) fn order<'a, T>(
        &self,
        items: impl IntoIterator<Item = (T, &'a str, String)>,
    ) -> Vec<(u32, T)> {
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
    pub(super) fn failed(
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
    pub(super) fn give_up(
        &self,
        connection: &Connection,
        path: &str,
        version: &str,
    ) -> Result<(), Failure> {
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
    pub(super) fn clear(&self, connection: &Connection, path: &str) -> Result<(), Failure> {
        if self.by_path.contains_key(path) {
            connection.execute(
                "DELETE FROM retries WHERE workspace_id = ?1 AND path = ?2 AND stage = ?3",
                params![self.workspace_id, path, self.stage],
            )?;
        }
        Ok(())
    }
}
