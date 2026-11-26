#![cfg(unix)]

use langspan::observer::{self, FileKind, Limits, Snapshot};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

struct Repo {
    base: PathBuf,
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "langspan-git-observer-{}-{}",
            std::process::id(),
            NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let root = base.join("checkout");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        Self { base, root }
    }

    fn write(&self, path: &str, content: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn commit(&self) {
        git(&self.root, &["add", "--all"]);
        git(
            &self.root,
            &[
                "-c",
                "user.name=Observer Test",
                "-c",
                "user.email=observer@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "--message",
                "synthetic observer fixture",
            ],
        );
    }

    fn scan(&self) -> Snapshot {
        observer::scan(&self.root, Instant::now() + Duration::from_secs(10)).unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}

fn git(root: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    output.stdout
}

fn changed_paths(previous: &Option<Snapshot>, current: &Snapshot) -> Vec<String> {
    observer::diff(previous, current)
        .into_iter()
        .map(|change| change.path)
        .collect()
}

#[test]
fn a_clean_baseline_is_silent_and_contains_only_needed_metadata() {
    let repo = Repo::new();
    repo.write("tracked.txt", b"abc");
    repo.write("binary.dat", &[0, 255, 128, 7]);
    repo.commit();
    let snapshot = repo.scan();
    assert_eq!(
        snapshot.root,
        fs::canonicalize(&repo.root).unwrap().to_str().unwrap()
    );
    assert_eq!(snapshot.branch.as_deref(), Some("main"));
    assert!(snapshot.head.is_some());
    assert_eq!(snapshot.files.len(), 2);
    assert!(snapshot.files.values().all(|file| !file.dirty));
    assert_eq!(snapshot.files["tracked.txt"].kind, FileKind::Regular);
    assert_eq!(
        snapshot.files["tracked.txt"].digest.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert!(observer::diff(&None, &snapshot).is_empty());
    assert_eq!(snapshot.branches.len(), 1);
    assert_eq!(snapshot.branches[0].name, "main");
    assert_eq!(Some(snapshot.branches[0].commit.clone()), snapshot.head);
    assert!(!snapshot.branches[0].tree.is_empty());
    assert_eq!(Some(snapshot.branches[0].tree.clone()), snapshot.head_tree);
    let serialized = serde_json::to_string(&snapshot).unwrap();
    for private in [
        "Observer Test",
        "observer@example.invalid",
        "synthetic observer fixture",
        "\"abc\"",
    ] {
        assert!(
            !serialized.contains(private),
            "unexpected source or author text: {private}"
        );
    }
}

#[test]
fn dirty_untracked_and_missing_files_are_explicit_but_ignored_files_are_excluded() {
    let repo = Repo::new();
    repo.write(".gitignore", b"ignored/\n*.secret\n");
    repo.write("changed.txt", b"before");
    repo.write("deleted.txt", b"delete me");
    repo.commit();
    repo.write("changed.txt", b"after");
    fs::remove_file(repo.root.join("deleted.txt")).unwrap();
    repo.write("untracked.txt", b"new");
    repo.write("ignored/never-open", b"ignored fixture");
    repo.write("private.secret", b"ignored fixture");
    let snapshot = repo.scan();
    assert_eq!(
        changed_paths(&None, &snapshot),
        ["changed.txt", "deleted.txt", "untracked.txt"]
    );
    assert_eq!(snapshot.files["deleted.txt"].kind, FileKind::Missing);
    assert_eq!(snapshot.files["deleted.txt"].digest, None);
    assert!(
        !snapshot
            .files
            .keys()
            .any(|path| path.contains("ignored") || path.ends_with(".secret"))
    );
}

#[test]
fn staged_changes_and_staged_deletions_are_dirty_on_the_first_scan() {
    let repo = Repo::new();
    repo.write("staged.txt", b"old");
    repo.write("removed.txt", b"old");
    repo.commit();
    repo.write("staged.txt", b"new");
    git(&repo.root, &["add", "staged.txt"]);
    git(&repo.root, &["rm", "--quiet", "removed.txt"]);
    let snapshot = repo.scan();
    assert_eq!(
        changed_paths(&None, &snapshot),
        ["removed.txt", "staged.txt"]
    );
    assert_eq!(snapshot.files["removed.txt"].kind, FileKind::Missing);
}

#[test]
fn symlink_versions_hash_link_text_without_following_the_target() {
    let repo = Repo::new();
    let target = repo.base.join("outside-file");
    fs::write(&target, b"outside content one").unwrap();
    symlink(&target, repo.root.join("link")).unwrap();
    symlink("nonexistent-target", repo.root.join("dangling")).unwrap();
    repo.commit();
    let before = repo.scan();
    assert_eq!(before.files["link"].kind, FileKind::Symlink);
    assert!(!before.files["link"].dirty);
    assert!(!before.files["dangling"].dirty);
    fs::write(&target, b"different outside content").unwrap();
    let unchanged = repo.scan();
    assert!(observer::diff(&Some(before.clone()), &unchanged).is_empty());
    fs::remove_file(repo.root.join("link")).unwrap();
    symlink("a different link target", repo.root.join("link")).unwrap();
    let changed = repo.scan();
    assert_eq!(changed_paths(&Some(before), &changed), ["link"]);
    assert!(
        !serde_json::to_string(&changed)
            .unwrap()
            .contains("outside content")
    );
}

#[test]
fn a_file_type_change_is_visible_even_when_the_raw_content_digest_matches() {
    let repo = Repo::new();
    repo.write("file", b"target");
    repo.commit();
    let before = repo.scan();
    fs::remove_file(repo.root.join("file")).unwrap();
    symlink("target", repo.root.join("file")).unwrap();
    let after = repo.scan();
    assert_eq!(before.files["file"].digest, after.files["file"].digest);
    assert_eq!(before.files["file"].kind, FileKind::Regular);
    assert_eq!(after.files["file"].kind, FileKind::Symlink);
    assert_eq!(changed_paths(&Some(before), &after), ["file"]);
}

#[test]
fn digest_changes_are_detected_even_when_size_and_mtime_are_unchanged() {
    let repo = Repo::new();
    repo.write("same.txt", b"before");
    repo.commit();
    let before = repo.scan();
    let path = repo.root.join("same.txt");
    let metadata = fs::metadata(&path).unwrap();
    fs::write(&path, b"after!").unwrap();
    fs::File::open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_accessed(metadata.accessed().unwrap())
                .set_modified(metadata.modified().unwrap()),
        )
        .unwrap();
    let new_metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.len(), new_metadata.len());
    assert_eq!(
        metadata.modified().unwrap(),
        new_metadata.modified().unwrap()
    );
    let after = repo.scan();
    assert_ne!(
        before.files["same.txt"].digest,
        after.files["same.txt"].digest
    );
    assert_eq!(changed_paths(&Some(before), &after), ["same.txt"]);
}

#[test]
fn additions_deletions_and_renames_are_content_observations_without_authorship() {
    let repo = Repo::new();
    repo.write("old.txt", b"move me");
    repo.write("deleted.txt", b"remove me");
    repo.commit();
    let before = repo.scan();
    fs::rename(repo.root.join("old.txt"), repo.root.join("new.txt")).unwrap();
    fs::remove_file(repo.root.join("deleted.txt")).unwrap();
    repo.write("added.txt", b"new file");
    let after = repo.scan();
    assert_eq!(
        changed_paths(&Some(before), &after),
        ["added.txt", "deleted.txt", "new.txt", "old.txt"]
    );
    let repeated = repo.scan();
    assert!(observer::diff(&Some(after), &repeated).is_empty());
}

#[test]
fn unchanged_content_stays_silent_when_it_is_staged_or_committed() {
    let repo = Repo::new();
    repo.write("file.txt", b"old");
    repo.commit();
    repo.write("file.txt", b"new");
    let before = repo.scan();
    repo.commit();
    let after = repo.scan();
    assert!(!after.files["file.txt"].dirty);
    assert_ne!(before.head, after.head);
    assert!(observer::diff(&Some(before), &after).is_empty());
}

#[test]
fn worktrees_have_separate_roots_and_heads_but_share_local_branch_records() {
    let repo = Repo::new();
    repo.write("file.txt", b"main");
    repo.commit();
    let other = repo.base.join("linked-worktree");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature/one",
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join("file.txt"), b"feature").unwrap();
    git(&other, &["add", "file.txt"]);
    git(
        &other,
        &[
            "-c",
            "user.name=Observer Test",
            "-c",
            "user.email=observer@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "--message",
            "worktree fixture",
        ],
    );
    let main = repo.scan();
    let linked = observer::scan(&other, Instant::now() + Duration::from_secs(10)).unwrap();
    assert_ne!(main.root, linked.root);
    assert_eq!(main.common_dir, linked.common_dir);
    assert_ne!(main.head, linked.head);
    assert_eq!(linked.branch.as_deref(), Some("feature/one"));
    assert_eq!(main.branches, linked.branches);
    assert_eq!(
        main.branches
            .iter()
            .map(|branch| branch.name.as_str())
            .collect::<Vec<_>>(),
        ["feature/one", "main"]
    );
}

#[test]
fn unborn_and_detached_heads_are_explicit_states() {
    let repo = Repo::new();
    repo.write("untracked", b"not yet committed");
    let unborn = repo.scan();
    assert_eq!(unborn.head, None);
    assert_eq!(unborn.head_tree, None);
    assert_eq!(unborn.branch.as_deref(), Some("main"));
    assert!(unborn.branches.is_empty());
    assert_eq!(changed_paths(&None, &unborn), ["untracked"]);
    repo.commit();
    git(&repo.root, &["checkout", "--quiet", "--detach"]);
    let detached = repo.scan();
    assert!(detached.head.is_some());
    assert_eq!(
        detached.head_tree,
        Some(
            String::from_utf8(git(&repo.root, &["rev-parse", "HEAD^{tree}"]))
                .unwrap()
                .trim_end()
                .to_owned()
        )
    );
    assert_eq!(detached.branch, None);
}

fn protected_git_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(base: &Path, path: &Path, result: &mut BTreeMap<String, Vec<u8>>) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(base, &entry.unwrap().path(), result);
            }
        } else if path.exists() {
            result.insert(
                path.strip_prefix(base).unwrap().to_str().unwrap().into(),
                fs::read(path).unwrap(),
            );
        }
    }
    let base = root.join(".git");
    let mut result = BTreeMap::new();
    for entry in ["HEAD", "config", "index", "packed-refs", "refs"] {
        visit(&base, &base.join(entry), &mut result);
    }
    result
}

#[test]
fn scans_leave_git_config_index_and_refs_byte_identical() {
    let repo = Repo::new();
    repo.write("file.txt", b"baseline");
    repo.commit();
    repo.write("file.txt", b"dirty");
    repo.write("untracked.txt", b"untracked");
    let before = protected_git_bytes(&repo.root);
    repo.scan();
    repo.scan();
    assert_eq!(protected_git_bytes(&repo.root), before);
    assert!(!repo.root.join(".git/index.lock").exists());
}

#[test]
fn repository_filters_and_fsmonitor_are_not_executed() {
    let repo = Repo::new();
    repo.write(".gitattributes", b"*.txt filter=probe\n");
    repo.write("file.txt", b"before");
    repo.commit();
    let marker = repo.base.join("should-not-exist");
    let script = repo.base.join("repository-program");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf called > '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &repo.root,
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );
    git(
        &repo.root,
        &["config", "filter.probe.clean", script.to_str().unwrap()],
    );
    repo.write("file.txt", b"after");
    repo.scan();
    assert!(
        !marker.exists(),
        "observer executed repository-specified code"
    );
}

#[test]
fn an_ancestor_symlink_is_refused_instead_of_reading_outside_the_workspace() {
    let repo = Repo::new();
    repo.write("directory/file.txt", b"inside");
    repo.commit();
    let outside = repo.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file.txt"), b"outside").unwrap();
    fs::remove_dir_all(repo.root.join("directory")).unwrap();
    symlink(&outside, repo.root.join("directory")).unwrap();
    let error = observer::scan(&repo.root, Instant::now() + Duration::from_secs(10)).unwrap_err();
    assert!(error.contains("ancestor"), "{error}");
}

#[test]
fn metadata_bounds_reject_the_whole_scan() {
    let repo = Repo::new();
    repo.write("one", b"12345678");
    repo.write("two", b"12345678");
    repo.commit();
    let variants = [
        Limits {
            max_git_output_bytes: 8,
            ..Limits::default()
        },
        Limits {
            max_branches: 0,
            ..Limits::default()
        },
    ];
    for limits in variants {
        let error = observer::scan_with_limits(
            &repo.root,
            Instant::now() + Duration::from_secs(10),
            limits,
        )
        .unwrap_err();
        assert!(error.contains("bound"), "{limits:?}: {error}");
    }
}

fn scan_limited(repo: &Repo, limits: Limits) -> Snapshot {
    observer::scan_with_limits(&repo.root, Instant::now() + Duration::from_secs(10), limits)
        .unwrap()
}

/// A repository over the file-count bound,
/// with a file over the per-file bound, scans partially and says so.
#[test]
fn a_repository_over_the_file_bound_scans_changed_files_first_and_reports_coverage() {
    let repo = Repo::new();
    for index in 0..12 {
        repo.write(
            &format!("f{index:02}.txt"),
            format!("file {index}").as_bytes(),
        );
    }
    repo.write("big.bin", &[7; 64]);
    repo.commit();
    let limits = Limits {
        max_files: 5,
        max_file_bytes: 32,
        ..Limits::default()
    };
    // Changed files sort after the clean ones, so a scan in path order
    // would never reach them.
    repo.write("f09.txt", b"changed");
    repo.write("zz-new.txt", b"untracked");
    let first = scan_limited(&repo, limits);
    assert_eq!(first.files.len(), 5, "{:?}", first.files.keys());
    assert!(first.files["f09.txt"].dirty);
    assert!(first.files["zz-new.txt"].dirty);
    assert!(!first.files.contains_key("big.bin"));
    assert_eq!(first.coverage.too_large, 1);
    assert_eq!(first.coverage.too_large_paths, ["big.bin"]);
    // 14 listed: 5 versioned, 1 too large, 8 left out by the count bound.
    assert_eq!(first.coverage.omitted, 8);
    assert!(first.coverage.is_partial());
    assert!(first.unobserved.contains("big.bin"));
    assert_eq!(
        changed_paths(&None, &first),
        ["f09.txt", "zz-new.txt"],
        "the first scan reports only what is dirty"
    );

    // Another small file changes; one clean file falls out of the
    // selection. The change is seen and nothing is reported deleted.
    repo.write("f10.txt", b"changed too");
    let second = scan_limited(&repo, limits);
    assert!(second.files["f10.txt"].dirty);
    let changes = observer::diff(&Some(first.clone()), &second);
    assert_eq!(
        changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>(),
        ["f10.txt"],
        "{changes:?}"
    );
    assert!(
        changes[0].before_unknown,
        "the previous scan did not version it"
    );

    // The oversized file growing or shrinking past the bound is not a
    // deletion or a creation of it.
    repo.write("big.bin", &[8; 16]);
    let third = scan_limited(&repo, limits);
    assert!(third.files.contains_key("big.bin"));
    assert_eq!(third.coverage.too_large, 0);
    let changes = observer::diff(&Some(second.clone()), &third);
    assert!(
        changes
            .iter()
            .all(|change| change.path != "big.bin" || change.before_unknown),
        "{changes:?}"
    );
}

/// The default bounds on a repository over the 4,096-file bound: exactly
/// 4,096 files versioned, the file over 4 MiB listed as too large and the
/// rest left out. A two-minute deadline keeps the time bound, which leaves
/// clean files out once half of the deadline has passed, from stopping the
/// scan first on a slow or loaded machine. The daemon's own two-second
/// deadline is exercised in tests/repository_shapes.rs.
#[test]
fn the_default_file_bound_versions_exactly_4096_files() {
    let repo = Repo::new();
    for index in 0..4200 {
        repo.write(
            &format!("d{:02}/f{index:04}.txt", index / 100),
            format!("{index}\n").as_bytes(),
        );
    }
    repo.write("big.bin", &vec![b'x'; 5 * 1024 * 1024]);
    repo.commit();
    let started = Instant::now();
    let snapshot = observer::scan_with_limits(
        &repo.root,
        Instant::now() + Duration::from_secs(120),
        Limits::default(),
    )
    .unwrap();
    let seen = format!(
        "{} files versioned in {:?}, coverage {:?}",
        snapshot.files.len(),
        started.elapsed(),
        snapshot.coverage
    );
    assert_eq!(snapshot.files.len(), 4096, "{seen}");
    assert_eq!(snapshot.coverage.too_large_paths, ["big.bin"], "{seen}");
    // 4,201 listed: 4,096 versioned, 1 too large, the rest left out.
    assert_eq!(snapshot.coverage.omitted, 4201 - 4096 - 1, "{seen}");
    // Path order: the files left out are the last ones.
    assert!(snapshot.files.contains_key("d40/f4095.txt"), "{seen}");
    assert!(!snapshot.files.contains_key("d40/f4096.txt"), "{seen}");
}

#[test]
fn the_byte_budget_leaves_clean_files_out_before_changed_ones() {
    let repo = Repo::new();
    for index in 0..6 {
        repo.write(&format!("f{index}.txt"), b"0123456789");
    }
    repo.commit();
    repo.write("f5.txt", b"9876543210");
    let snapshot = scan_limited(
        &repo,
        Limits {
            max_total_bytes: 25,
            ..Limits::default()
        },
    );
    assert!(snapshot.files["f5.txt"].dirty, "{:?}", snapshot.files);
    assert!(snapshot.files.len() < 6);
    assert_eq!(snapshot.coverage.omitted as usize, 6 - snapshot.files.len());
}

#[test]
fn a_partial_scan_asks_git_for_changes_without_running_repository_filters() {
    let repo = Repo::new();
    repo.write(".gitattributes", b"*.txt filter=probe\n");
    for index in 0..4 {
        repo.write(&format!("f{index}.txt"), b"before");
    }
    repo.commit();
    let marker = repo.base.join("should-not-exist");
    let script = repo.base.join("repository-program");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf called > '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    for key in ["filter.probe.clean", "filter.probe.process"] {
        git(&repo.root, &["config", key, script.to_str().unwrap()]);
    }
    git(&repo.root, &["config", "filter.probe.required", "true"]);
    repo.write("f3.txt", b"after!");
    let snapshot = scan_limited(
        &repo,
        Limits {
            max_files: 2,
            ..Limits::default()
        },
    );
    assert!(snapshot.files["f3.txt"].dirty);
    assert!(
        !marker.exists(),
        "observer executed repository-specified code"
    );
}

/// A submodule is opaque.
#[test]
fn a_submodule_is_recorded_as_present_and_never_read_into() {
    let repo = Repo::new();
    let library = repo.base.join("library");
    fs::create_dir(&library).unwrap();
    git(&library, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(library.join("lib.txt"), b"library").unwrap();
    git(&library, &["add", "lib.txt"]);
    git(
        &library,
        &[
            "-c",
            "user.name=Observer Test",
            "-c",
            "user.email=observer@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "--message",
            "library",
        ],
    );
    repo.write("app.txt", b"app");
    git(
        &repo.root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "--quiet",
            "add",
            library.to_str().unwrap(),
            "vendor/library",
        ],
    );
    repo.commit();
    let before = repo.scan();
    assert_eq!(before.coverage.submodules, 1);
    assert_eq!(before.coverage.submodule_paths, ["vendor/library"]);
    assert!(!before.coverage.is_partial());
    assert!(before.files.contains_key(".gitmodules"));
    assert!(
        !before.files.keys().any(|path| path.starts_with("vendor/")),
        "{:?}",
        before.files.keys()
    );
    assert!(observer::diff(&None, &before).is_empty());
    // A change inside the submodule is its own repository's business.
    fs::write(repo.root.join("vendor/library/lib.txt"), b"changed").unwrap();
    let after = repo.scan();
    assert!(observer::diff(&Some(before), &after).is_empty());
}

/// Files outside a sparse checkout are
/// neither present nor deleted.
#[test]
fn a_sparse_checkout_scans_without_reporting_files_outside_it_as_deleted() {
    let repo = Repo::new();
    repo.write("top.txt", b"top");
    repo.write("inside/a.txt", b"inside");
    repo.write("outside/b.txt", b"outside");
    repo.write("outside/c.txt", b"outside too");
    repo.commit();
    let before = repo.scan();
    assert_eq!(before.files.len(), 4);
    git(&repo.root, &["sparse-checkout", "set", "inside"]);
    assert!(!repo.root.join("outside").exists());
    let after = repo.scan();
    assert_eq!(after.coverage.skip_worktree, 2);
    assert!(!after.coverage.is_partial());
    assert!(!after.files.contains_key("outside/b.txt"));
    assert_eq!(
        changed_paths(&Some(before), &after),
        Vec::<String>::new(),
        "no false deletions"
    );
    repo.write("inside/a.txt", b"changed");
    let changed = repo.scan();
    assert_eq!(changed_paths(&Some(after), &changed), ["inside/a.txt"]);
}

fn git_status(root: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Observer Test",
            "-c",
            "user.email=observer@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// A scan during a merge or a rebase that
/// stopped on a conflict.
#[test]
fn a_scan_during_a_conflicted_merge_or_rebase_completes_and_marks_the_conflicts() {
    for operation in ["merge", "rebase"] {
        let repo = Repo::new();
        repo.write("shared.txt", b"base\n");
        repo.write("other.txt", b"untouched\n");
        repo.commit();
        assert!(git_status(
            &repo.root,
            &["checkout", "--quiet", "-b", "theirs"]
        ));
        repo.write("shared.txt", b"theirs\n");
        repo.commit();
        assert!(git_status(&repo.root, &["checkout", "--quiet", "main"]));
        repo.write("shared.txt", b"ours\n");
        repo.commit();
        let before = repo.scan();
        let stopped = match operation {
            "merge" => git_status(&repo.root, &["merge", "--quiet", "theirs"]),
            _ => git_status(&repo.root, &["rebase", "--quiet", "theirs"]),
        };
        assert!(!stopped, "{operation} should stop on the conflict");
        let during = repo.scan();
        assert!(during.files["shared.txt"].conflicted, "{operation}");
        assert!(during.files["shared.txt"].dirty, "{operation}");
        assert!(!during.files["other.txt"].conflicted, "{operation}");
        assert_eq!(
            changed_paths(&Some(before), &during),
            ["shared.txt"],
            "{operation}"
        );
    }
}

#[test]
fn an_expired_deadline_is_an_explicit_failure() {
    let repo = Repo::new();
    let error = observer::scan(&repo.root, Instant::now()).unwrap_err();
    assert!(error.contains("deadline"), "{error}");
}

#[test]
fn a_stalled_git_process_uses_the_callers_deadline() {
    let repo = Repo::new();
    let bin = repo.base.join("bin");
    fs::create_dir(&bin).unwrap();
    let fake_git = bin.join("git");
    fs::write(&fake_git, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "stalled_git_child"])
        .env("LANGSPAN_OBSERVER_TEST_ROOT", &repo.root)
        .env("PATH", &bin)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("observer failed to bound a stalled Git child");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[ignore = "subprocess fixture for deadline isolation"]
fn stalled_git_child() {
    let Some(root) = std::env::var_os("LANGSPAN_OBSERVER_TEST_ROOT") else {
        return;
    };
    let error =
        observer::scan(Path::new(&root), Instant::now() + Duration::from_millis(40)).unwrap_err();
    assert!(error.contains("deadline"), "{error}");
}
