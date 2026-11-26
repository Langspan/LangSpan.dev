//! Bounded, read-only observations of physical Git workspaces.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
    time::Instant,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub root: String,
    pub common_dir: String,
    pub head: Option<String>,
    pub head_tree: Option<String>,
    pub branch: Option<String>,
    pub files: BTreeMap<String, FileVersion>,
    pub branches: Vec<BranchRecord>,
    /// What this scan listed but did not version, counted by reason.
    #[serde(default)]
    pub coverage: langspan_core::ScanCoverage,
    /// Every listed path this scan did not version, for the comparison with
    /// the previous scan only: none of them is a deletion. Not persisted.
    #[serde(skip)]
    pub unobserved: BTreeSet<String>,
    /// The working-tree files among `unobserved` that this scan did not
    /// read, and why: for the index's blind-spots answer. Not persisted.
    #[serde(skip)]
    pub unread: BTreeMap<String, Unread>,
    /// The blob at `head` of each dirty regular file that `head` holds, for
    /// the index's in-flight comparison (roadmap C7). Not persisted.
    #[serde(skip)]
    pub head_objects: BTreeMap<String, String>,
    /// When the scan began reading, on the clock the core is told: a session
    /// that reported a file since then is newer than this scan's read of it.
    /// Set by the daemon's reader; not persisted.
    #[serde(skip)]
    pub scanned_from_ms: Option<u64>,
}

/// Why a scan listed a working-tree file but did not read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// Over the scan's per-file byte bound.
    TooLarge,
    /// Past the scan's file, byte or time bound.
    Omitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Regular,
    Symlink,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub digest: Option<String>,
    pub kind: FileKind,
    pub dirty: bool,
    /// Unmerged in the index: a merge, rebase, cherry-pick or stash pop
    /// stopped on this path.
    #[serde(default, skip_serializing_if = "is_false")]
    pub conflicted: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchRecord {
    pub name: String,
    pub commit: String,
    pub tree: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub before: Option<FileVersion>,
    pub after: Option<FileVersion>,
    /// No earlier version is known: there is no previous scan, or the
    /// previous scan was partial and did not version this path.
    pub before_unknown: bool,
}

/// The first three bound what a scan versions: past them it is partial and
/// says so. The last two bound what it lists: past them it fails.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Files versioned per scan, changed ones first.
    pub max_files: usize,
    /// A larger file is listed as too large and not read.
    pub max_file_bytes: usize,
    /// Content read per scan; clean files may use half of it.
    pub max_total_bytes: u64,
    pub max_git_output_bytes: usize,
    pub max_branches: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_files: 4096,
            max_file_bytes: 4 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
            max_git_output_bytes: 4 * 1024 * 1024,
            max_branches: 1024,
        }
    }
}

/// Whether two workspace roots name the same directory now: equal once
/// symbolic links are resolved, and the same device and inode. A registered
/// root is kept as given, so a checkout reached through a link (a moved
/// folder with a link left at its old path, or a home directory reached
/// through one) matches the canonical root Git reports. A root that does
/// not resolve matches nothing. Every comparison of a registered root with
/// a resolved one uses this.
pub fn same_root(a: &Path, b: &Path) -> bool {
    let (Ok(resolved_a), Ok(resolved_b)) = (std::fs::canonicalize(a), std::fs::canonicalize(b))
    else {
        return false;
    };
    if resolved_a != resolved_b {
        return false;
    }
    let (Ok(meta_a), Ok(meta_b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta_a.is_dir() && meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino()
    }
    #[cfg(not(unix))]
    {
        meta_a.is_dir() && meta_b.is_dir()
    }
}

/// Whether the object database of the checkout at `root` holds the commit
/// `oid`, read as the observer runs Git (no fsmonitor program, no replace
/// refs). An error is the reason Git could not be asked.
pub fn holds_commit(root: &Path, oid: &str, deadline: Instant) -> Result<bool, String> {
    let object = format!("{oid}^{{commit}}");
    let arguments = [
        "--no-replace-objects",
        "-c",
        "core.fsmonitor=false",
        "cat-file",
        "-e",
        &object,
    ];
    Ok(
        crate::git::run_with_env(root, &arguments, &[], deadline, 256)?
            .0
            .success(),
    )
}

pub fn scan(root: &Path, deadline: Instant) -> Result<Snapshot, String> {
    scan_with_limits(root, deadline, Limits::default())
}

pub fn scan_with_limits(
    root: &Path,
    deadline: Instant,
    limits: Limits,
) -> Result<Snapshot, String> {
    check_deadline(deadline)?;
    #[cfg(not(unix))]
    {
        let _ = (root, limits);
        Err("Git observer currently requires Unix".into())
    }
    #[cfg(unix)]
    {
        let identity = crate::git::identity(root, deadline)?;
        let registered = std::fs::canonicalize(root)
            .map_err(|error| format!("cannot resolve observed workspace: {error}"))?;
        if registered != identity.root {
            return Err("Git observation requires the registered physical workspace root to be the Git toplevel".into());
        }
        let root_text = identity
            .root
            .to_str()
            .ok_or("workspace root is not UTF-8")?;
        let common_dir = identity
            .common_dir
            .to_str()
            .ok_or("Git common directory is not UTF-8")?;
        let started = Instant::now();
        // Clean files are versioned only in the first half of the budget, so
        // the changed ones and the final consistency check still fit in it.
        let clean_until = started + deadline.saturating_duration_since(started) / 2;
        let listing = listing(&identity.root, deadline, limits)?;
        let index = parse_index(&listing.index)?;
        let head_files = parse_tree(&listing.tree)?;
        let untracked = parse_paths(&listing.untracked)?;
        let mut coverage = langspan_core::ScanCoverage::default();
        let mut unobserved = BTreeSet::new();
        let mut candidates = Vec::new();
        for path in index
            .keys()
            .chain(head_files.keys())
            .chain(untracked.iter())
            .collect::<BTreeSet<_>>()
        {
            let indexed = index.get(path);
            // A gitlink is another repository: present, never read into.
            if indexed.is_some_and(|indexed| indexed.entry.gitlink)
                || head_files.get(path).is_some_and(|head| head.gitlink)
            {
                coverage.submodules += 1;
                if coverage.submodule_paths.len() < langspan_core::COVERAGE_PATHS {
                    coverage.submodule_paths.push(path.clone());
                }
                unobserved.insert(path.clone());
                continue;
            }
            // Outside a sparse checkout: not in the working tree, and not
            // deleted from it.
            if indexed.is_some_and(|indexed| indexed.skip_worktree) {
                coverage.skip_worktree += 1;
                unobserved.insert(path.clone());
                continue;
            }
            candidates.push(path.clone());
        }
        // Changed by the listings alone: untracked, staged, unmerged or
        // staged for deletion. `git status` adds working-tree edits, and is
        // asked only once some file has to be left out.
        let listed_change = |path: &String| match index.get(path) {
            None => true,
            Some(indexed) => {
                indexed.conflicted
                    || head_files
                        .get(path)
                        .is_none_or(|head| head != &indexed.entry)
            }
        };
        let mut changed: Option<BTreeSet<String>> = None;
        if candidates.len() > limits.max_files {
            let reported = changed_paths(&identity.root, deadline, limits)?;
            // Changed files first, then clean ones, each in path order.
            candidates.sort_by_key(|path| !(listed_change(path) || reported.contains(path)));
            changed = Some(reported);
        }
        let root_fd = rustix::fs::open(
            &identity.root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| format!("cannot open physical workspace: {error}"))?;
        // Clean files may use half of the byte budget, so changed files
        // always have at least the other half.
        let clean_budget = limits.max_total_bytes / 2;
        let mut total_bytes = 0;
        let mut clean_bytes = 0;
        let mut skip_clean = false;
        let mut files = BTreeMap::new();
        let mut unread = BTreeMap::new();
        let mut head_objects = BTreeMap::new();
        for path in candidates {
            let indexed = index.get(&path);
            let mut too_large = false;
            let observed = loop {
                check_deadline(deadline)?;
                if !skip_clean && Instant::now() >= clean_until {
                    skip_clean = true;
                }
                if skip_clean && changed.is_none() {
                    changed = Some(changed_paths(&identity.root, deadline, limits)?);
                }
                let is_changed = listed_change(&path)
                    || changed.as_ref().map(|changed| changed.contains(&path)) != Some(false);
                if files.len() >= limits.max_files || skip_clean && !is_changed {
                    break None;
                }
                // A staged deletion may leave an ignored backing file.
                // Represent the removed source path without opening it.
                if indexed.is_none() && !untracked.contains(&path) {
                    break Some(Observed::missing());
                }
                // Not known to be changed: counted as clean.
                let maybe_clean = !is_changed || changed.is_none() && !listed_change(&path);
                let mut budget = limits.max_total_bytes.saturating_sub(total_bytes);
                if maybe_clean {
                    budget = budget.min(clean_budget.saturating_sub(clean_bytes));
                }
                let before = total_bytes;
                match observe_file(
                    &root_fd,
                    &path,
                    deadline,
                    limits,
                    budget,
                    indexed.map(|indexed| indexed.entry.object.len()),
                    &mut total_bytes,
                )? {
                    Reading::File(observed) => {
                        if maybe_clean {
                            clean_bytes += total_bytes - before;
                        }
                        break Some(observed);
                    }
                    Reading::TooLarge => {
                        too_large = true;
                        coverage.too_large += 1;
                        if coverage.too_large_paths.len() < langspan_core::COVERAGE_PATHS {
                            coverage.too_large_paths.push(path.clone());
                        }
                        break None;
                    }
                    // Ask Git which files changed, then decide this one again.
                    Reading::OverBudget if changed.is_none() => skip_clean = true,
                    Reading::OverBudget => {
                        skip_clean = true;
                        break None;
                    }
                }
            };
            let Some(mut observed) = observed else {
                if !too_large {
                    coverage.omitted += 1;
                }
                let reason = if too_large {
                    Unread::TooLarge
                } else {
                    Unread::Omitted
                };
                unread.insert(path.clone(), reason);
                unobserved.insert(path);
                continue;
            };
            let mut dirty = observed.version.kind == FileKind::Missing || indexed.is_none();
            if let Some(indexed) = indexed {
                dirty |= listed_change(&path)
                    || observed.version.kind != indexed.entry.kind
                    || observed.version.kind == FileKind::Regular
                        && observed.executable != (indexed.entry.mode == "100755");
                // The working bytes as a Git blob ID, compared with the
                // indexed blob: the raw content, never filtered.
                if !dirty {
                    dirty = observed.blob.as_deref() != Some(&indexed.entry.object);
                }
                observed.version.conflicted = indexed.conflicted;
            }
            observed.version.dirty = dirty;
            if dirty
                && observed.version.kind == FileKind::Regular
                && let Some(head) = head_files.get(&path)
                && head.kind == FileKind::Regular
                && !head.gitlink
            {
                head_objects.insert(path.clone(), head.object.clone());
            }
            files.insert(path, observed.version);
        }
        // Do not turn partial enumeration or index/ref movement into deletions.
        // Working file contents are point-in-time reads, not an atomic tree.
        if listing != self::listing(&identity.root, deadline, limits)? {
            return Err("Git repository metadata changed during observation; observation is incomplete; retry".into());
        }
        check_deadline(deadline)?;
        Ok(Snapshot {
            root: root_text.into(),
            common_dir: common_dir.into(),
            head: listing.head,
            head_tree: listing.head_tree,
            branch: listing.branch,
            files,
            branches: listing.branches,
            coverage,
            unobserved,
            unread,
            head_objects,
            scanned_from_ms: None,
        })
    }
}

/// The paths `git status` reports as changed: staged, modified in the
/// working tree, unmerged or untracked. Submodules are ignored and every
/// configured filter is switched off, so no repository program runs.
fn changed_paths(
    root: &Path,
    deadline: Instant,
    limits: Limits,
) -> Result<BTreeSet<String>, String> {
    let overrides = crate::git::filter_overrides(root, deadline)?;
    let mut arguments: Vec<&str> = overrides.iter().map(String::as_str).collect();
    arguments.extend([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=all",
        "--no-renames",
    ]);
    let bytes = git_output(root, &arguments, deadline, limits.max_git_output_bytes)?;
    nul_records(&bytes)?
        .map(|record| {
            if record.len() < 4 || record[2] != b' ' {
                return Err("Git status enumeration is malformed".to_owned());
            }
            path_text(&record[3..])
        })
        .collect()
}

/// Content observations carry no author. A hook can be related evidence only;
/// its presence must not change this diff into an authorship assertion.
pub fn diff(previous: &Option<Snapshot>, current: &Snapshot) -> Vec<Change> {
    let Some(previous) = previous.as_ref().filter(|previous| {
        previous.root == current.root && previous.common_dir == current.common_dir
    }) else {
        return current
            .files
            .iter()
            .filter(|(_, version)| version.dirty || version.kind == FileKind::Missing)
            .map(|(path, version)| Change {
                path: path.clone(),
                before: None,
                after: Some(version.clone()),
                before_unknown: true,
            })
            .collect();
    };
    let paths: BTreeSet<_> = previous.files.keys().chain(current.files.keys()).collect();
    let previous_partial = previous.coverage.is_partial();
    paths
        .into_iter()
        .filter_map(|path| {
            let before = previous.files.get(path);
            let after = current.files.get(path);
            match (before, after) {
                // Listed but not versioned now (left out by a bound, too
                // large, outside a sparse checkout): unknown, not deleted.
                (Some(_), None) if current.unobserved.contains(path) => None,
                // A partial previous scan may simply not have versioned it.
                // Like a first scan, only a dirty file is worth reporting,
                // and its earlier version is unknown.
                (None, Some(after)) if previous_partial => {
                    (after.dirty || after.kind == FileKind::Missing).then(|| Change {
                        path: path.clone(),
                        before: None,
                        after: Some(after.clone()),
                        before_unknown: true,
                    })
                }
                // A formerly missing path leaving the index is still absent,
                // not a second deletion. Staging/committing alone is not a
                // content change.
                _ => (content_identity(before) != content_identity(after)).then(|| Change {
                    path: path.clone(),
                    before: before.cloned(),
                    after: after.cloned(),
                    before_unknown: false,
                }),
            }
        })
        .collect()
}

fn content_identity(version: Option<&FileVersion>) -> Option<(FileKind, Option<&str>)> {
    version
        .filter(|version| version.kind != FileKind::Missing)
        .map(|version| (version.kind, version.digest.as_deref()))
}

fn check_deadline(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        Err("Git observation deadline elapsed; observation is incomplete".into())
    } else {
        Ok(())
    }
}

fn content_digest(bytes: &[u8], deadline: Instant) -> Result<String, String> {
    check_deadline(deadline)?;
    let digest = crate::sha256::hex(&crate::sha256::digest(bytes));
    check_deadline(deadline)?;
    Ok(digest)
}

fn add_bytes(total: &mut u64, count: usize, limits: Limits) -> Result<(), String> {
    *total = total
        .checked_add(count as u64)
        .ok_or("Git observation byte bound exceeded")?;
    if *total > limits.max_total_bytes {
        return Err("Git observation total-byte bound exceeded; observation is incomplete".into());
    }
    Ok(())
}

fn git_output(
    root: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>, String> {
    // Repository configuration must not execute fsmonitor or clean filters.
    // cat-file below reads raw blobs, never --filters/--textconv.
    let mut safe_args = vec![
        "--no-replace-objects",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ];
    safe_args.extend_from_slice(args);
    crate::git::output(root, &safe_args, deadline, limit)
}

#[derive(PartialEq, Eq)]
struct Listing {
    head: Option<String>,
    head_tree: Option<String>,
    branch: Option<String>,
    index: Vec<u8>,
    tree: Vec<u8>,
    untracked: Vec<u8>,
    branches: Vec<BranchRecord>,
}

fn listing(root: &Path, deadline: Instant, limits: Limits) -> Result<Listing, String> {
    let output = |args: &[&str]| git_output(root, args, deadline, limits.max_git_output_bytes);
    // These forms succeed with empty output for unborn/detached states, so no
    // failed Git command is accidentally reinterpreted as a missing HEAD.
    let head = optional_line(output(&["rev-parse", "--revs-only", "HEAD"])?)?;
    if let Some(head) = &head {
        validate_object(head)?;
    }
    let head_tree = match &head {
        Some(head) => {
            let revision = format!("{head}^{{tree}}");
            let tree = optional_line(output(&["rev-parse", "--verify", &revision])?)?
                .ok_or("Git HEAD tree is missing; observation is incomplete")?;
            validate_object(&tree)?;
            Some(tree)
        }
        None => None,
    };
    let branch = optional_line(output(&["branch", "--show-current"])?)?;
    let branches = parse_branches(
        &output(&[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname)%00%(objectname)%00%(tree)%00",
            "refs/heads/",
        ])?,
        limits.max_branches,
    )?;
    let index = output(&["ls-files", "--cached", "--stage", "-t", "-z"])?;
    let untracked = output(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    let tree = match &head {
        Some(head) => output(&["ls-tree", "-r", "-z", head])?,
        None => Vec::new(),
    };
    Ok(Listing {
        head,
        head_tree,
        branch,
        index,
        tree,
        untracked,
        branches,
    })
}

fn optional_line(bytes: Vec<u8>) -> Result<Option<String>, String> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| "Git metadata is not UTF-8; observation is incomplete")?;
    let line = text
        .strip_suffix('\n')
        .ok_or("Git metadata line is incomplete")?;
    if line.contains(['\n', '\0']) {
        return Err("Git metadata line is malformed".into());
    }
    Ok((!line.is_empty()).then(|| line.to_owned()))
}

fn validate_object(object: &str) -> Result<(), String> {
    if !matches!(object.len(), 40 | 64) || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Git object identity is malformed; observation is incomplete".into());
    }
    Ok(())
}

fn path_text(bytes: &[u8]) -> Result<String, String> {
    let path = std::str::from_utf8(bytes)
        .map_err(|_| "Git file path is not UTF-8; observation is incomplete")?;
    if path.is_empty()
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(
            "Git file path is not a relative workspace path; observation is incomplete".into(),
        );
    }
    Ok(path.into())
}

fn nul_records(bytes: &[u8]) -> Result<impl Iterator<Item = &[u8]>, String> {
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        return Err("Git NUL enumeration is incomplete".into());
    }
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty()))
}

fn parse_paths(bytes: &[u8]) -> Result<BTreeSet<String>, String> {
    nul_records(bytes)?.map(path_text).collect()
}

#[derive(PartialEq, Eq)]
struct Entry {
    object: String,
    mode: String,
    kind: FileKind,
    /// A submodule commit (mode 160000). `kind` means nothing for it.
    gitlink: bool,
}

struct Indexed {
    entry: Entry,
    conflicted: bool,
    /// Outside a sparse checkout (`S` in `ls-files -t`).
    skip_worktree: bool,
}

fn entry(mode: &str, object: &str) -> Result<Entry, String> {
    validate_object(object)?;
    let kind = match mode {
        "100644" | "100755" | "160000" => FileKind::Regular,
        "120000" => FileKind::Symlink,
        _ => {
            return Err("Git index entry type is unsupported; observation is incomplete".into());
        }
    };
    Ok(Entry {
        object: object.into(),
        mode: mode.into(),
        kind,
        gitlink: mode == "160000",
    })
}

fn parse_index(bytes: &[u8]) -> Result<BTreeMap<String, Indexed>, String> {
    let mut entries: BTreeMap<String, Indexed> = BTreeMap::new();
    for record in nul_records(bytes)? {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("Git index enumeration is malformed")?;
        let metadata =
            std::str::from_utf8(&record[..tab]).map_err(|_| "Git index metadata is malformed")?;
        let fields: Vec<_> = metadata.split_ascii_whitespace().collect();
        if fields.len() != 4 || !matches!(fields[3], "0" | "1" | "2" | "3") {
            return Err("Git index enumeration is malformed".into());
        }
        // H: cached; M: unmerged; S: skip-worktree (sparse checkout).
        if !matches!(fields[0], "H" | "M" | "S") {
            return Err("Git index entry status is unsupported; observation is incomplete".into());
        }
        let path = path_text(&record[tab + 1..])?;
        let entry = entry(fields[1], fields[2])?;
        match entries.get_mut(&path) {
            Some(existing) => existing.conflicted = true,
            None => {
                entries.insert(
                    path,
                    Indexed {
                        entry,
                        conflicted: fields[3] != "0",
                        skip_worktree: fields[0] == "S",
                    },
                );
            }
        }
    }
    Ok(entries)
}

fn parse_tree(bytes: &[u8]) -> Result<BTreeMap<String, Entry>, String> {
    let mut entries = BTreeMap::new();
    for record in nul_records(bytes)? {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("Git tree enumeration is malformed")?;
        let metadata =
            std::str::from_utf8(&record[..tab]).map_err(|_| "Git tree metadata is malformed")?;
        let fields: Vec<_> = metadata.split_ascii_whitespace().collect();
        let gitlink = fields.len() == 3 && fields[0] == "160000" && fields[1] == "commit";
        if fields.len() != 3 || !(fields[1] == "blob" || gitlink) {
            return Err("Git tree entry type is unsupported; observation is incomplete".into());
        }
        let path = path_text(&record[tab + 1..])?;
        if entries.insert(path, entry(fields[0], fields[2])?).is_some() {
            return Err("Git tree contains duplicate paths; observation is incomplete".into());
        }
    }
    Ok(entries)
}

fn parse_branches(bytes: &[u8], limit: usize) -> Result<Vec<BranchRecord>, String> {
    let mut branches = Vec::new();
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err("Git branch enumeration is incomplete".into());
    }
    for record in bytes
        .split(|byte| *byte == b'\n')
        .filter(|record| !record.is_empty())
    {
        let fields: Vec<_> = record.split(|byte| *byte == 0).collect();
        if fields.len() != 4 || !fields[3].is_empty() {
            return Err("Git branch enumeration is malformed".into());
        }
        let text =
            |bytes| std::str::from_utf8(bytes).map_err(|_| "Git branch metadata is not UTF-8");
        let name = text(fields[0])?
            .strip_prefix("refs/heads/")
            .ok_or("Git returned a non-local branch")?;
        let commit = text(fields[1])?;
        let tree = text(fields[2])?;
        validate_object(commit)?;
        validate_object(tree)?;
        if name.is_empty() {
            return Err("Git branch name is empty".into());
        }
        branches.push(BranchRecord {
            name: name.into(),
            commit: commit.into(),
            tree: tree.into(),
        });
        if branches.len() > limit {
            return Err("Git branch-count bound exceeded; observation is incomplete".into());
        }
    }
    Ok(branches)
}

#[cfg(unix)]
struct Observed {
    version: FileVersion,
    executable: bool,
    /// The bytes as a Git blob ID, when the caller asked for one.
    blob: Option<String>,
}

#[cfg(unix)]
enum Reading {
    File(Observed),
    /// Over the per-file bound: not read.
    TooLarge,
    /// Within the per-file bound, but more than the scan's remaining byte
    /// budget: not read.
    OverBudget,
}

/// The Git blob ID of `bytes` in a repository whose object IDs have
/// `object_length` hexadecimal characters: SHA-1 (40) or SHA-256 (64).
pub(crate) fn blob_id(bytes: &[u8], object_length: usize) -> Option<String> {
    let header = format!("blob {}\0", bytes.len());
    match object_length {
        40 => Some(crate::sha256::hex(&crate::sha1::digest(&[
            header.as_bytes(),
            bytes,
        ]))),
        64 => {
            let mut object = header.into_bytes();
            object.extend_from_slice(bytes);
            Some(crate::sha256::hex(&crate::sha256::digest(&object)))
        }
        _ => None,
    }
}

#[cfg(unix)]
impl Observed {
    fn missing() -> Self {
        Self {
            version: FileVersion {
                digest: None,
                kind: FileKind::Missing,
                dirty: true,
                conflicted: false,
            },
            executable: false,
            blob: None,
        }
    }
}

#[cfg(unix)]
fn observe_file(
    root_fd: &rustix::fd::OwnedFd,
    path: &str,
    deadline: Instant,
    limits: Limits,
    budget: u64,
    blob_object_length: Option<usize>,
    total: &mut u64,
) -> Result<Reading, String> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, readlinkat_raw, statat};
    use std::{fs, io::Read, os::unix::fs::MetadataExt};
    let mut components: Vec<_> = Path::new(path).components().collect();
    let name = components.pop().ok_or("empty observed path")?.as_os_str();
    let mut directory = rustix::io::dup(root_fd)
        .map_err(|error| format!("cannot open observer directory: {error}"))?;
    for component in components {
        check_deadline(deadline)?;
        directory = match openat(
            &directory,
            component.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(rustix::io::Errno::NOENT) => return Ok(Reading::File(Observed::missing())),
            Err(error) => {
                return Err(format!(
                    "cannot traverse observed path ancestor without following symlinks: {error}; observation is incomplete"
                ));
            }
        };
    }
    let before = match statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(Reading::File(Observed::missing())),
        Err(error) => return Err(format!("cannot inspect observed file: {error}")),
    };
    if before.st_size < 0 {
        return Err("observed file has a negative size; observation is incomplete".into());
    }
    let size = before.st_size as u64;
    if size > limits.max_file_bytes as u64 {
        return Ok(Reading::TooLarge);
    }
    if size > budget {
        return Ok(Reading::OverBudget);
    }
    let blob = |bytes: &[u8]| blob_object_length.and_then(|length| blob_id(bytes, length));
    let kind = FileType::from_raw_mode(before.st_mode);
    match kind {
        FileType::Symlink => {
            let mut buffer = vec![0_u8; limits.max_file_bytes.saturating_add(1).min(8192)];
            let length = readlinkat_raw(&directory, name, &mut buffer[..])
                .map_err(|error| format!("cannot read observed symlink text: {error}"))?;
            if length == buffer.len() || length > limits.max_file_bytes {
                return Err(
                    "Git observation symlink byte bound exceeded; observation is incomplete".into(),
                );
            }
            buffer.truncate(length);
            add_bytes(total, length, limits)?;
            let after = statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| "symlink changed during observation; observation is incomplete")?;
            if before.st_dev != after.st_dev
                || before.st_ino != after.st_ino
                || before.st_size != after.st_size
                || before.st_mode != after.st_mode
            {
                return Err("symlink changed during observation; observation is incomplete".into());
            }
            Ok(Reading::File(Observed {
                version: FileVersion {
                    digest: Some(content_digest(&buffer, deadline)?),
                    kind: FileKind::Symlink,
                    dirty: false,
                    conflicted: false,
                },
                executable: false,
                blob: blob(&buffer),
            }))
        }
        FileType::RegularFile => {
            let fd = openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| {
                format!("cannot open observed file without following symlinks: {error}")
            })?;
            let mut file = fs::File::from(fd);
            let initial = file
                .metadata()
                .map_err(|error| format!("cannot inspect observed file: {error}"))?;
            if !initial.is_file()
                || initial.dev() != stat_device(&before)
                || initial.ino() != before.st_ino
            {
                return Err("file changed during observation; observation is incomplete".into());
            }
            if initial.nlink() != 1 {
                return Err("hard-linked file is unsupported; observation is incomplete".into());
            }
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 8192];
            loop {
                check_deadline(deadline)?;
                let count = match file.read(&mut chunk) {
                    Ok(count) => count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(format!("cannot read observed file: {error}")),
                };
                if count == 0 {
                    break;
                }
                if bytes.len().saturating_add(count) > limits.max_file_bytes {
                    return Err(
                        "Git observation per-file byte bound exceeded; observation is incomplete"
                            .into(),
                    );
                }
                add_bytes(total, count, limits)?;
                bytes.extend_from_slice(&chunk[..count]);
            }
            let after = file
                .metadata()
                .map_err(|error| format!("cannot recheck observed file: {error}"))?;
            let current_path = statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| "file changed during observation; observation is incomplete")?;
            if initial.len() != after.len()
                || initial.mtime() != after.mtime()
                || initial.mtime_nsec() != after.mtime_nsec()
                || initial.ctime() != after.ctime()
                || initial.ctime_nsec() != after.ctime_nsec()
                || initial.mode() != after.mode()
                || initial.dev() != stat_device(&current_path)
                || initial.ino() != current_path.st_ino
            {
                return Err("file changed during observation; observation is incomplete".into());
            }
            Ok(Reading::File(Observed {
                version: FileVersion {
                    digest: Some(content_digest(&bytes, deadline)?),
                    kind: FileKind::Regular,
                    dirty: false,
                    conflicted: false,
                },
                executable: initial.mode() & 0o111 != 0,
                blob: blob(&bytes),
            }))
        }
        _ => Err("special file is unsupported; observation is incomplete".into()),
    }
}

/// `st_dev` is `u64` on Linux and `i32` on macOS; std's `MetadataExt::dev`
/// widens it the same way on both.
#[allow(clippy::unnecessary_cast)]
fn stat_device(stat: &rustix::fs::Stat) -> u64 {
    stat.st_dev as u64
}
