#![cfg(unix)]
//! Checks bound to exact trees, combining branches and landing, through the
//! real CLI, a real daemon and real Git repositories with a shared remote.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn git(root: &Path, args: &[&str]) -> String {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Dev",
            "-c",
            "user.email=dev@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-land-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        let hub = base.join("hub.git");
        git(
            &base,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                hub.to_str().unwrap(),
            ],
        );
        let work = base.join("work");
        git(
            &base,
            &[
                "clone",
                "--quiet",
                hub.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        git(&work, &["config", "user.name", "Dev"]);
        git(&work, &["config", "user.email", "dev@example.invalid"]);
        git(&work, &["checkout", "--quiet", "-b", "main"]);
        for file in ["a.txt", "b.txt"] {
            fs::write(work.join(file), "base\n").unwrap();
        }
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "base"]);
        git(&work, &["push", "--quiet", "origin", "main"]);
        // Three branches: A and B touch different files, C conflicts with A.
        for (branch, file, text) in [
            ("feature-a", "a.txt", "A\n"),
            ("feature-b", "b.txt", "B\n"),
            ("feature-c", "a.txt", "C\n"),
        ] {
            git(&work, &["checkout", "--quiet", "-b", branch, "main"]);
            fs::write(work.join(file), text).unwrap();
            git(&work, &["commit", "--quiet", "-am", branch]);
        }
        git(&work, &["checkout", "--quiet", "main"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self { base, daemon: None };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(&state)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while world.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let root = world.work();
        let (code, text) = world.cli(&[
            "workspace",
            "add",
            "app",
            root.to_str().unwrap(),
            "--request-id",
            "ws",
        ]);
        assert_eq!(code, 0, "{text}");
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn hub(&self) -> PathBuf {
        self.base.join("hub.git")
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }
    /// Everything about the owner's checkout that these commands must not move.
    fn checkout(&self) -> String {
        let work = self.work();
        format!(
            "{}\n{}\n{}\n{}",
            git(&work, &["symbolic-ref", "HEAD"]),
            git(
                &work,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads/"
                ]
            ),
            git(&work, &["status", "--porcelain"]),
            git(&work, &["worktree", "list", "--porcelain"]),
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_check_result_stops_applying_the_moment_the_working_tree_differs() {
    let w = World::new();
    let before = w.checkout();
    let (code, text) = w.cli(&[
        "check", "run", "app", "--name", "unit", "--", "test", "-f", "a.txt",
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("unit passed on tree"), "{text}");
    let (_, status) = w.cli(&["check", "status", "app"]);
    assert!(
        status.contains("Applies to the working tree as it is now"),
        "{status}"
    );
    assert_eq!(w.checkout(), before, "snapshotting moved nothing");

    // One byte changes, in a file Git does not even track yet.
    fs::write(w.work().join("new-untracked.txt"), "x").unwrap();
    let (_, status) = w.cli(&["check", "status", "app"]);
    assert!(status.contains("Does NOT apply"), "{status}");
    assert!(
        status.contains("passed"),
        "the old result is kept as history: {status}"
    );
    fs::remove_file(w.work().join("new-untracked.txt")).unwrap();
    let (_, status) = w.cli(&["check", "status", "app"]);
    assert!(
        status.contains("Applies to the working tree"),
        "the same tree again: {status}"
    );

    // A failing command is recorded as a failure on that tree.
    let (code, text) = w.cli(&["check", "run", "app", "--name", "lint", "--", "false"]);
    assert_eq!(code, 1, "{text}");
    // A command that changes its own inputs proves nothing about any tree.
    let (code, text) = w.cli(&[
        "check",
        "run",
        "app",
        "--name",
        "gen",
        "--",
        "sh",
        "-c",
        "echo generated >> b.txt",
    ]);
    assert_eq!(code, 5, "{text}");
    assert!(text.contains("inconclusive"), "{text}");
    // So does one that does not finish.
    let (code, text) = w.cli(&[
        "check",
        "run",
        "app",
        "--name",
        "slow",
        "--timeout-s",
        "1",
        "--",
        "sleep",
        "30",
    ]);
    assert_eq!(code, 5, "{text}");
}

#[test]
fn independent_branches_are_checked_together_and_landed_only_on_current_evidence() {
    let w = World::new();
    let before = w.checkout();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    let both = ["feature-a", "feature-b"];
    let verify = ["--", "sh", "-c", "grep -q A a.txt && grep -q B b.txt"];

    // A conflicting pair is reported before anything runs.
    let (code, text) = w.cli(
        &[
            &[
                "integrate",
                "check",
                "app",
                "--onto",
                "main",
                "--name",
                "unit",
                "feature-a",
                "feature-c",
            ][..],
            &verify[..],
        ]
        .concat(),
    );
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("Not combinable") && text.contains("a.txt"),
        "{text}"
    );

    // Landing without evidence is refused, and says how to get it.
    let land = |extra: &[&str]| {
        w.cli(
            &[
                &["land", "app", "--onto", "main", "--name", "unit"][..],
                extra,
            ]
            .concat(),
        )
    };
    let (code, text) = land(&both);
    assert_eq!(code, 6, "{text}");
    assert!(text.contains("no check of that name"), "{text}");

    // The two independent branches are combined and checked as one tree,
    // which exists in no checkout.
    let (code, text) = w.cli(
        &[
            &[
                "integrate",
                "check",
                "app",
                "--onto",
                "main",
                "--name",
                "unit",
            ][..],
            &both[..],
            &verify[..],
        ]
        .concat(),
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("unit passed on the combined tree"), "{text}");

    // Evidence for A and B together says nothing about A alone.
    let (code, text) = land(&["feature-a"]);
    assert_eq!(code, 6, "{text}");
    assert!(text.contains("other trees"), "{text}");

    // A dry run pushes nothing.
    let (code, text) = land(&both);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("Ready") && text.contains("Dry run"), "{text}");
    assert_eq!(hub_main(), original);

    // Someone else lands first. The evidence was for a different result.
    let other = w.base.join("other");
    git(
        &w.base,
        &[
            "clone",
            "--quiet",
            w.hub().to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join("c.txt"), "theirs\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "--quiet", "-m", "concurrent"]);
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let moved = hub_main();
    let (code, text) = land(&[&both[..], &["--apply"]].concat());
    assert_eq!(code, 6, "{text}");
    assert_eq!(hub_main(), moved, "nothing was pushed on stale evidence");

    // Checked again against the new target, it lands.
    let (code, text) = w.cli(
        &[
            &[
                "integrate",
                "check",
                "app",
                "--onto",
                "main",
                "--name",
                "unit",
            ][..],
            &both[..],
            &verify[..],
        ]
        .concat(),
    );
    assert_eq!(code, 0, "{text}");
    let (code, text) = land(&[&both[..], &["--apply"]].concat());
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("Landed"), "{text}");
    let landed = hub_main();
    assert_ne!(landed, moved);
    for (file, content) in [("a.txt", "A"), ("b.txt", "B"), ("c.txt", "theirs")] {
        assert_eq!(
            git(&w.hub(), &["show", &format!("{landed}:{file}")]),
            content
        );
    }
    // History is preserved: both branches and the concurrent commit are ancestors.
    for commit in ["feature-a", "feature-b"] {
        let tip = git(&w.work(), &["rev-parse", commit]);
        git(&w.hub(), &["merge-base", "--is-ancestor", &tip, &landed]);
    }
    git(&w.hub(), &["merge-base", "--is-ancestor", &moved, &landed]);

    // The owner's checkout, HEAD and branches are where they were. Git itself
    // moved the remote-tracking ref to what the push put on the remote.
    assert_eq!(w.checkout(), before);
    assert_eq!(
        git(&w.work(), &["rev-parse", "refs/remotes/origin/main"]),
        landed
    );
    // Landing the same thing again finds nothing to do.
    let (code, text) = land(&[&both[..], &["--apply"]].concat());
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("Nothing to land"), "{text}");
    let (_, events) = w.cli(&["events", "--limit", "1000"]);
    assert!(events.contains(r#""type":"landed""#), "{events}");
}

#[test]
fn a_failed_combined_check_blocks_landing_and_bad_names_are_refused() {
    let w = World::new();
    let (code, text) = w.cli(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature-a",
        "--",
        "false",
    ]);
    assert_eq!(code, 1, "{text}");
    let (code, text) = w.cli(&[
        "land",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature-a",
        "--apply",
    ]);
    assert_eq!(code, 6, "{text}");
    assert!(text.contains("failed on exactly this tree"), "{text}");
    for bad in [
        "--upload-pack=x",
        "main..feature-a",
        "feature-a^{tree}",
        "no-such-branch",
    ] {
        let (code, text) = w.cli(&[
            "integrate",
            "check",
            "app",
            "--onto",
            "main",
            "--name",
            "unit",
            "--",
            bad,
        ]);
        assert_ne!(code, 0, "{bad}: {text}");
    }
    let (code, text) = w.cli(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "no-such-branch",
        "--",
        "true",
    ]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("does not name a commit"), "{text}");
}

#[test]
fn a_combined_check_that_changes_its_own_tree_is_not_evidence_for_it() {
    // Review finding: a command that fixes a file and then passes must not
    // authorize landing the original, unfixed tree.
    let w = World::new();
    let (code, text) = w.cli(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature-a",
        "--",
        "sh",
        "-c",
        "echo fixed > a.txt",
    ]);
    assert_eq!(code, 5, "{text}");
    assert!(
        text.contains("did not hold exactly the combined tree"),
        "{text}"
    );
    // It names the file that changed, and what to do about it.
    assert!(text.contains("a.txt (modified)"), "{text}");
    assert!(text.contains("run the check again"), "{text}");
    let (code, text) = w.cli(&[
        "land",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature-a",
        "--apply",
    ]);
    assert_eq!(code, 6, "{text}");
    assert!(text.contains("inconclusive"), "{text}");
}

#[test]
fn a_timed_out_check_leaves_nothing_running() {
    // Review finding: a descendant of the check outlived its time limit.
    let w = World::new();
    let marker = w.base.join("late-write");
    let script = format!("(sleep 2; touch {}) & sleep 30", marker.display());
    let (code, text) = w.cli(&[
        "check",
        "run",
        "app",
        "--name",
        "slow",
        "--timeout-s",
        "1",
        "--",
        "sh",
        "-c",
        &script,
    ]);
    assert_eq!(code, 5, "{text}");
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !marker.exists(),
        "a descendant of the check ran after its time limit"
    );
}

/// The tree `check run` printed.
fn recorded_tree(text: &str) -> String {
    text.split("passed on tree ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_hexdigit()).next())
        .unwrap_or_else(|| panic!("no tree in: {text}"))
        .to_owned()
}

/// In a sparse checkout, the files outside
/// it are part of the tree, not deletions from it.
#[test]
fn check_run_in_a_sparse_checkout_records_heads_tree_when_nothing_changed() {
    let w = World::new();
    let work = w.work();
    for file in ["docs/guide.txt", "src/main.txt"] {
        fs::create_dir_all(work.join(file).parent().unwrap()).unwrap();
        fs::write(work.join(file), "content\n").unwrap();
    }
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "dirs"]);
    git(&work, &["sparse-checkout", "set", "src"]);
    assert!(!work.join("docs").exists());
    let head_tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
    let (code, text) = w.cli(&["check", "run", "app", "--name", "unit", "--", "true"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(recorded_tree(&text), head_tree, "{text}");
    let (_, status) = w.cli(&["check", "status", "app"]);
    assert!(status.contains("Applies to the working tree"), "{status}");
    assert!(
        git(&work, &["status", "--porcelain"]).is_empty(),
        "the owner's index is untouched"
    );
}

/// Git LFS is configured, required and not
/// installed. Its filters are not run; the tree is HEAD plus the raw local
/// change.
#[test]
fn check_run_with_a_required_filter_that_is_not_installed_records_the_raw_tree() {
    let w = World::new();
    let work = w.work();
    // Not named `lfs`: a machine with Git LFS installed (GitHub's macOS
    // image) defines that filter globally, and it would run on the commit.
    fs::write(work.join(".gitattributes"), "*.bin filter=missing-tool\n").unwrap();
    fs::write(work.join("asset.bin"), "pointer-like text\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "asset"]);
    let missing = w.base.join("no-such-git-lfs");
    for (key, value) in [
        (
            "filter.missing-tool.clean",
            format!("{} clean -- %f", missing.display()),
        ),
        (
            "filter.missing-tool.smudge",
            format!("{} smudge -- %f", missing.display()),
        ),
        (
            "filter.missing-tool.process",
            format!("{} filter-process", missing.display()),
        ),
        ("filter.missing-tool.required", "true".into()),
    ] {
        git(&work, &["config", key, &value]);
    }
    let (code, text) = w.cli(&["check", "run", "app", "--name", "unit", "--", "true"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        recorded_tree(&text),
        git(&work, &["rev-parse", "HEAD^{tree}"])
    );

    // A local change: the expected tree, built independently from HEAD and
    // the file's raw bytes.
    fs::write(work.join("asset.bin"), "local change\n").unwrap();
    let blob = git(&work, &["hash-object", "-w", "--no-filters", "asset.bin"]);
    let index = w.base.join("expected-index");
    let with_index = |args: &[&str]| {
        let output = Process::new("git")
            .arg("-C")
            .arg(&work)
            .args(args)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    with_index(&["read-tree", "HEAD"]);
    with_index(&[
        "update-index",
        "--cacheinfo",
        &format!("100644,{blob},asset.bin"),
    ]);
    let expected = with_index(&["write-tree"]);
    let (code, text) = w.cli(&["check", "run", "app", "--name", "unit", "--", "true"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(recorded_tree(&text), expected, "{text}");
}

/// A shallow clone cannot combine
/// branches correctly, so it is refused before anything is fetched.
#[test]
fn integrate_and_land_in_a_shallow_clone_are_refused_with_what_to_do() {
    let w = World::new();
    let work = w.work();
    fs::write(work.join("a.txt"), "second\n").unwrap();
    git(&work, &["commit", "--quiet", "-am", "second"]);
    git(&work, &["push", "--quiet", "origin", "main", "feature-a"]);
    let shallow = w.base.join("shallow");
    let url = format!("file://{}", w.hub().display());
    git(
        &w.base,
        &[
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--no-single-branch",
            &url,
            shallow.to_str().unwrap(),
        ],
    );
    assert_eq!(
        git(&shallow, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    git(
        &shallow,
        &["branch", "--quiet", "feature-a", "origin/feature-a"],
    );
    let (code, text) = w.cli(&[
        "workspace",
        "add",
        "thin",
        shallow.to_str().unwrap(),
        "--request-id",
        "ws-thin",
    ]);
    assert_eq!(code, 0, "{text}");
    let before = git(&shallow, &["for-each-ref"]);
    for args in [
        &[
            "integrate",
            "check",
            "thin",
            "--onto",
            "main",
            "--name",
            "unit",
            "feature-a",
            "--",
            "true",
        ][..],
        &[
            "land",
            "thin",
            "--onto",
            "main",
            "--name",
            "unit",
            "feature-a",
            "--apply",
        ][..],
    ] {
        let (code, text) = w.cli(args);
        assert_ne!(code, 0, "{text}");
        assert!(
            text.contains("this clone is shallow") && text.contains("git fetch --unshallow"),
            "{args:?}: {text}"
        );
    }
    assert_eq!(
        git(&shallow, &["for-each-ref"]),
        before,
        "nothing was fetched"
    );
}

impl World {
    /// Standard output alone, for `--json`.
    fn json(&self, args: &[&str]) -> (i32, serde_json::Value) {
        let output = self.spawn(args).wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        (
            output.status.code().unwrap(),
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}")),
        )
    }
    fn spawn(&self, args: &[&str]) -> Child {
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
    /// The owner's own pre-push hook, which every landing must run.
    fn pre_push(&self, script: &str) {
        let hook = self.work().join(".git/hooks/pre-push");
        fs::write(&hook, format!("#!/bin/sh\n{script}\nexit 0\n")).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    /// A second clone, as a teammate would have, with one commit ready to push.
    fn teammate(&self, file: &str) -> PathBuf {
        let other = self.base.join(format!("other-{file}"));
        git(
            &self.base,
            &[
                "clone",
                "--quiet",
                self.hub().to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        fs::write(other.join(file), "theirs\n").unwrap();
        git(&other, &["add", "."]);
        git(&other, &["commit", "--quiet", "-m", file]);
        other
    }
    fn events(&self) -> Vec<serde_json::Value> {
        let (code, value) = self.json(&["events", "--limit", "1000", "--json"]);
        assert_eq!(code, 0);
        value["outcome"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["event"].clone())
            .collect()
    }
}

const VERIFY: &str = "test -f a.txt && test -f b.txt";

fn land_args<'a>(branch: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    [
        &[
            "land", "app", "--onto", "main", "--name", "unit", branch, "--apply",
        ][..],
        extra,
    ]
    .concat()
}

impl World {
    /// `land-queue` for the main branch, as JSON.
    fn queue(&self) -> serde_json::Value {
        let (code, value) = self.json(&["land-queue", "app", "--onto", "main", "--json"]);
        assert_eq!(code, 0, "{value}");
        value
    }
    /// Wait, at most 20 s, until the queue view satisfies `ready`.
    fn queue_until(&self, ready: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let queue = self.queue();
            if ready(&queue) {
                return queue;
            }
            assert!(
                Instant::now() < deadline,
                "the queue never got there: {queue}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A finished child's exit code, standard output and both streams together.
fn finish(child: Child) -> (i32, String, String) {
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let both = format!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap(), stdout, both)
}

fn holds(queue: &serde_json::Value, ticket: &str) -> bool {
    queue["holder"]["turn_id"] == ticket
}

#[test]
fn a_second_landing_waits_its_turn_and_lands_on_the_new_target_with_its_check_run_again() {
    let w = World::new();
    let before = w.checkout();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    // A slow pre-push hook keeps the first landing inside its turn.
    w.pre_push("sleep 3");
    let again = ["--json", "--check-again", "--", "sh", "-c", VERIFY];
    let first = w.spawn(
        &[
            &land_args("feature-a", &["--request-id", "land-1"])[..],
            &again,
        ]
        .concat(),
    );
    w.queue_until(|queue| holds(queue, "land-1"));
    let second = w.spawn(
        &[
            &land_args(
                "feature-b",
                &["--request-id", "land-2", "--session", "agent-2"],
            )[..],
            &again,
        ]
        .concat(),
    );
    // While it waits, the line is visible with who holds the turn.
    let queue = w.queue_until(|queue| queue["waiting"][0]["ticket"] == "land-2");
    assert!(holds(&queue, "land-1"), "{queue}");
    assert_eq!(queue["waiting"][0]["position"], 1, "{queue}");
    assert_eq!(queue["waiting"][0]["session_id"], "agent-2", "{queue}");
    assert_eq!(queue["waiting"].as_array().unwrap().len(), 1, "{queue}");

    let (code, out, text) = finish(first);
    assert_eq!(code, 0, "the first landing lands: {text}");
    let first: serde_json::Value = serde_json::from_str(&out).unwrap();
    let (code, out, text) = finish(second);
    assert_eq!(code, 0, "the second waited instead of failing: {text}");
    let second: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(second["result"], "landed", "{second}");
    assert!(second["waited_ms"].as_u64().unwrap() > 0, "{second}");

    // It combined onto what the first landed, not onto the target it saw
    // when it started, and checked that new tree before pushing it.
    let after_first = first["landing"]["commit_oid"].as_str().unwrap().to_owned();
    assert_ne!(after_first, original);
    assert_eq!(
        second["landing"]["inputs"][0],
        after_first.as_str(),
        "{second}"
    );
    let last = hub_main();
    assert_eq!(second["landing"]["commit_oid"], last.as_str());
    git(
        &w.hub(),
        &["merge-base", "--is-ancestor", &after_first, &last],
    );
    for (file, content) in [("a.txt", "A"), ("b.txt", "B")] {
        assert_eq!(git(&w.hub(), &["show", &format!("{last}:{file}")]), content);
    }
    let events = w.events();
    let check = events
        .iter()
        .find(|event| {
            event["type"] == "check_recorded"
                && event["check"]["check_id"] == second["landing"]["check_id"]
        })
        .unwrap_or_else(|| panic!("the second landing's check: {second}"));
    assert_eq!(check["check"]["tree_oid"], second["landing"]["tree_oid"]);
    assert_eq!(check["check"]["inputs"][0], after_first.as_str());
    assert_eq!(check["check"]["result"]["type"], "passed");
    assert_eq!(w.checkout(), before, "the owner's checkout never moved");

    // The log: the second joined the line while the first held the turn,
    // took it only after the first released it, and one held it at a time.
    let mut holder: Option<String> = None;
    let mut order = Vec::new();
    for event in &events {
        match event["type"].as_str().unwrap() {
            "turn_taken" => {
                assert!(holder.is_none(), "two holders at once: {event}");
                let id = event["turn"]["turn_id"].as_str().unwrap().to_owned();
                order.push(format!("taken {id}"));
                holder = Some(id);
            }
            "turn_released" | "turn_expired" if holder.as_deref() == event["turn_id"].as_str() => {
                order.push(format!("released {}", holder.take().unwrap()));
            }
            "turn_queue_joined" => order.push(format!(
                "joined {}",
                event["waiter"]["ticket"].as_str().unwrap()
            )),
            "turn_queue_left" => panic!("nobody left the line: {event}"),
            _ => {}
        }
    }
    assert_eq!(
        order,
        [
            "taken land-1",
            "joined land-2",
            "released land-1",
            "taken land-2",
            "released land-2"
        ]
    );
    assert_eq!(w.queue()["waiting"], serde_json::json!([]));
}

#[test]
fn a_waiting_landing_stops_at_its_wait_limit_or_when_cancelled_and_pushes_nothing() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    w.pre_push("sleep 5");
    let again = ["--check-again", "--", "sh", "-c", VERIFY];
    let first = w.spawn(
        &[
            &land_args("feature-a", &["--request-id", "land-1"])[..],
            &again,
        ]
        .concat(),
    );
    w.queue_until(|queue| holds(queue, "land-1"));

    // Asked not to wait, it is told at once, as before the queue.
    let (code, text) = w.cli(&[&land_args("feature-b", &["--wait-s", "0"])[..], &again].concat());
    assert_eq!(code, 7, "{text}");
    assert!(
        text.contains("another landing is in progress") && text.contains("a terminal"),
        "{text}"
    );

    // A short wait runs out: exit 7, nothing fetched or pushed, out of line.
    let started = Instant::now();
    let (code, text) = w.cli(
        &[
            &land_args("feature-b", &["--request-id", "land-2", "--wait-s", "1"])[..],
            &again,
        ]
        .concat(),
    );
    assert!(started.elapsed() >= Duration::from_secs(1), "it waited");
    assert_eq!(code, 7, "{text}");
    assert!(
        text.contains("waited 1 s for the push turn")
            && text.contains("Nothing was fetched or pushed"),
        "{text}"
    );
    assert_eq!(w.queue()["waiting"], serde_json::json!([]));

    // A waiting landing is cancelled from elsewhere, by its request ID.
    let third = w.spawn(
        &[
            &land_args("feature-b", &["--request-id", "land-3", "--wait-s", "60"])[..],
            &again,
        ]
        .concat(),
    );
    w.queue_until(|queue| queue["waiting"][0]["ticket"] == "land-3");
    let (code, text) = w.cli(&[
        "land-cancel",
        "app",
        "--onto",
        "main",
        "--request-id",
        "land-3",
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("land-3 stopped waiting"), "{text}");
    let (code, _, text) = finish(third);
    assert_eq!(code, 7, "{text}");
    assert!(text.contains("cancelled"), "{text}");
    // Cancelling again changes nothing and says so.
    let (code, text) = w.cli(&[
        "land-cancel",
        "app",
        "--onto",
        "main",
        "--request-id",
        "land-3",
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("was not waiting"), "{text}");

    let (code, _, text) = finish(first);
    assert_eq!(code, 0, "{text}");
    let landed = hub_main();
    assert_eq!(git(&w.hub(), &["show", &format!("{landed}:b.txt")]), "base");
    let left: Vec<_> = w
        .events()
        .into_iter()
        .filter(|event| event["type"] == "turn_queue_left")
        .map(|event| (event["ticket"].clone(), event["reason"].clone()))
        .collect();
    assert_eq!(
        left,
        [
            ("land-2".into(), "timed_out".into()),
            ("land-3".into(), "cancelled".into())
        ]
    );
}

impl World {
    /// Commit `files` on `branch`, starting it from `from`, in the owner's clone.
    fn commit_on(&self, branch: &str, from: &str, files: &[(&str, String)]) {
        let work = self.work();
        git(&work, &["checkout", "--quiet", "-B", branch, from]);
        for (file, text) in files {
            fs::write(work.join(file), text).unwrap();
        }
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", branch]);
        git(&work, &["checkout", "--quiet", "main"]);
    }
}

fn numbered(count: usize, line: impl Fn(usize) -> String) -> String {
    (1..=count).map(|n| line(n) + "\n").collect()
}

#[test]
fn a_conflict_reports_its_hunks_per_file_bounded_and_as_data() {
    let w = World::new();
    let base_many = numbered(60, |n| format!("line {n}"));
    let base_long = numbered(20, |n| format!("long {n}"));
    w.commit_on(
        "main",
        "main",
        &[("many.txt", base_many.clone()), ("long.txt", base_long)],
    );
    git(&w.work(), &["push", "--quiet", "origin", "main"]);
    // Ten separate one-line conflicts in one file, one twenty-line conflict
    // in another, and a branch line carrying a terminal escape.
    let changed = |side: &str| {
        numbered(60, |n| {
            if n % 6 == 1 {
                format!("{side} {n}")
            } else {
                format!("line {n}")
            }
        })
    };
    w.commit_on(
        "target-side",
        "main",
        &[
            ("many.txt", changed("target")),
            ("long.txt", numbered(20, |n| format!("target long {n}"))),
        ],
    );
    git(
        &w.work(),
        &["push", "--quiet", "origin", "target-side:main"],
    );
    w.commit_on(
        "clash",
        "main",
        &[
            ("many.txt", changed("branch")),
            (
                "long.txt",
                numbered(20, |n| {
                    if n == 1 {
                        "branch \u{1b}[2J long 1".into()
                    } else {
                        format!("branch long {n}")
                    }
                }),
            ),
        ],
    );
    let (code, report) = w.json(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "clash",
        "--json",
        "--",
        "true",
    ]);
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["result"], "not_combinable", "{report}");
    assert_eq!(report["paths"], serde_json::json!(["long.txt", "many.txt"]));
    let conflicts = report["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 2, "{report}");

    let long = &conflicts[0];
    assert_eq!(long["path"], "long.txt");
    assert_eq!(long["kinds"], serde_json::json!(["contents"]));
    let hunk = &long["hunks"][0];
    assert_eq!(
        (&hunk["target"]["start"], &hunk["target"]["lines"]),
        (&1.into(), &20.into())
    );
    assert_eq!(
        (&hunk["branch"]["start"], &hunk["branch"]["lines"]),
        (&1.into(), &20.into())
    );
    let shown = hunk["target"]["text"].as_array().unwrap();
    assert_eq!(shown.len(), 6, "a few lines of each side: {hunk}");
    assert_eq!(shown[0], "target long 1");
    let first_branch = hunk["branch"]["text"][0].as_str().unwrap();
    assert!(
        first_branch.starts_with("branch ") && !first_branch.contains('\u{1b}'),
        "control characters are not relayed: {first_branch:?}"
    );

    let many = &conflicts[1];
    assert_eq!(many["path"], "many.txt");
    assert_eq!(many["hunks"].as_array().unwrap().len(), 4, "{many}");
    assert_eq!(many["more_hunks"], 6, "{many}");
    for (index, hunk) in many["hunks"].as_array().unwrap().iter().enumerate() {
        let line = 6 * index + 1;
        assert_eq!(hunk["target"]["start"], line, "{hunk}");
        assert_eq!(hunk["branch"]["start"], line, "{hunk}");
        assert_eq!(
            hunk["target"]["text"],
            serde_json::json!([format!("target {line}")])
        );
        assert_eq!(
            hunk["branch"]["text"],
            serde_json::json!([format!("branch {line}")])
        );
    }

    // The same detail, for a person, labelled as the repository's text.
    let (code, text) = w.cli(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "clash",
        "--",
        "true",
    ]);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("repository text, shown as data"), "{text}");
    assert!(text.contains("many.txt line 7"), "{text}");
    assert!(text.contains("| branch 7"), "{text}");
    assert!(text.contains("and 6 more conflicts in this file"), "{text}");
    // A dry-run landing reports the same.
    let (code, report) = w.json(&[
        "land", "app", "--onto", "main", "--name", "unit", "clash", "--json",
    ]);
    assert_eq!(code, 2, "{report}");
    assert_eq!(
        report["conflicts"],
        serde_json::Value::Array(conflicts.clone())
    );
}

#[test]
fn conflict_line_numbers_are_each_sides_own_after_clean_changes_elsewhere() {
    let w = World::new();
    w.commit_on(
        "main",
        "main",
        &[("f.txt", numbered(30, |n| format!("line {n}")))],
    );
    git(&w.work(), &["push", "--quiet", "origin", "main"]);
    // The target adds two lines at the top and conflicts at line 20; the
    // branch deletes line 5 and conflicts at the same line.
    let edit = |side: &str, keep: &dyn Fn(usize) -> bool, top: &str| {
        top.to_owned()
            + &numbered(30, |n| {
                if n == 20 {
                    format!("{side} 20")
                } else {
                    format!("line {n}")
                }
            })
            .lines()
            .enumerate()
            .filter(|(i, _)| keep(i + 1))
            .map(|(_, l)| format!("{l}\n"))
            .collect::<String>()
    };
    w.commit_on(
        "target-side",
        "main",
        &[("f.txt", edit("target", &|_| true, "new 1\nnew 2\n"))],
    );
    git(
        &w.work(),
        &["push", "--quiet", "origin", "target-side:main"],
    );
    w.commit_on(
        "clash",
        "main",
        &[("f.txt", edit("branch", &|n| n != 5, ""))],
    );
    let (code, report) = w.json(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "clash",
        "--json",
        "--",
        "true",
    ]);
    assert_eq!(code, 2, "{report}");
    let hunk = &report["conflicts"][0]["hunks"][0];
    assert_eq!(
        hunk["target"]["text"],
        serde_json::json!(["target 20"]),
        "{report}"
    );
    assert_eq!(
        hunk["target"]["start"], 22,
        "after two lines added on top: {report}"
    );
    assert_eq!(
        hunk["branch"]["start"], 19,
        "after one line deleted above: {report}"
    );
}

#[test]
fn a_rename_that_already_landed_is_followed_and_said() {
    let w = World::new();
    let work = w.work();
    // The target renames a.txt; feature-a, made before that, edits a.txt.
    git(&work, &["mv", "a.txt", "renamed.txt"]);
    git(&work, &["commit", "--quiet", "-m", "rename a"]);
    git(&work, &["push", "--quiet", "origin", "main"]);
    // The owner turned rename detection off for their own merges; a
    // landing's combination does not depend on that setting.
    git(&work, &["config", "merge.renames", "false"]);
    let rename = serde_json::json!([{"branch": "feature-a", "from": "a.txt", "to": "renamed.txt"}]);
    let check = ["sh", "-c", "test ! -e a.txt && grep -q A renamed.txt"];
    let (code, report) = w.json(
        &[
            &[
                "integrate",
                "check",
                "app",
                "--onto",
                "main",
                "--name",
                "unit",
                "feature-a",
            ][..],
            &["--json", "--"],
            &check,
        ]
        .concat(),
    );
    assert_eq!(
        code, 0,
        "the branch's edit is combined onto the new path: {report}"
    );
    assert_eq!(report["combined"]["renames"], rename, "{report}");
    let (code, text) = w.cli(
        &land_args("feature-a", &[])
            .iter()
            .filter(|a| **a != "--apply")
            .copied()
            .collect::<Vec<_>>(),
    );
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("Followed a rename that already landed: feature-a edits \"a.txt\", which is now \"renamed.txt\"."),
        "{text}"
    );
    let (code, report) = w.json(&land_args("feature-a", &["--json"]));
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    assert_eq!(report["renames"], rename, "{report}");
    let landed = git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    assert_eq!(
        git(&w.hub(), &["show", &format!("{landed}:renamed.txt")]),
        "A"
    );
    assert_eq!(
        git(&w.hub(), &["ls-tree", "--name-only", &landed]),
        "b.txt\nrenamed.txt"
    );
    // A branch that touches no renamed file reports none.
    let (code, report) = w.json(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature-b",
        "--json",
        "--",
        "true",
    ]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        report["combined"]["renames"],
        serde_json::json!([]),
        "{report}"
    );
}

#[test]
fn a_target_that_moves_during_the_push_is_landed_only_on_a_new_passing_check() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    // A teammate pushes while the owner's push is under way, once per marker.
    let moves = |file: &str| {
        let other = w.teammate(file);
        let marker = w.base.join(format!("moved-{file}"));
        w.pre_push(&format!(
            "if [ ! -e {m} ]; then touch {m}; env -i PATH=\"$PATH\" HOME=\"$HOME\" git -C {o} push --quiet origin HEAD:main; fi",
            m = marker.display(),
            o = other.display()
        ));
        other
    };
    let (code, text) = w.cli(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature-a",
        "--",
        "test",
        "-f",
        "a.txt",
    ]);
    assert_eq!(code, 0, "{text}");

    // Without --check-again the new combination has no evidence: refused.
    let other = moves("c.txt");
    let (code, text) = w.cli(&land_args("feature-a", &[]));
    assert_eq!(code, 6, "{text}");
    assert!(
        text.contains("moved during the landing") && text.contains("2 attempts"),
        "{text}"
    );
    assert_eq!(
        hub_main(),
        git(&other, &["rev-parse", "HEAD"]),
        "only the teammate's commit is there; nothing unchecked was pushed"
    );

    // With it, each new combined tree is checked before it is pushed.
    moves("d.txt");
    let (code, report) = w.json(
        &[
            &land_args("feature-a", &["--check-again", "--json", "--"])[..],
            &["test", "-f", "a.txt"],
        ]
        .concat(),
    );
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let attempts = report["attempts"].as_array().unwrap();
    let kinds: Vec<_> = attempts
        .iter()
        .map(|a| a["result"]["type"].clone())
        .collect();
    assert_eq!(kinds, ["moved", "landed"], "{report}");
    assert_ne!(attempts[0]["target_oid"], attempts[1]["target_oid"]);
    let landed = hub_main();
    assert_eq!(report["landing"]["commit_oid"], landed.as_str());
    for (file, content) in [("a.txt", "A"), ("c.txt", "theirs"), ("d.txt", "theirs")] {
        assert_eq!(
            git(&w.hub(), &["show", &format!("{landed}:{file}")]),
            content
        );
    }
    // The landing relied on a passing check of exactly the tree it pushed.
    let tree = git(&w.hub(), &["rev-parse", &format!("{landed}^{{tree}}")]);
    assert_eq!(report["landing"]["tree_oid"], tree.as_str());
    let events = w.events();
    assert!(events.iter().any(|event| event["type"] == "check_recorded"
        && event["check"]["check_id"] == report["landing"]["check_id"]
        && event["check"]["tree_oid"] == tree.as_str()
        && event["check"]["result"]["type"] == "passed"));
    // Each attempt is recorded with the release of its turn.
    let released: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "turn_released")
        .map(|event| event["attempts"].as_array().unwrap().len())
        .collect();
    assert_eq!(released, [2, 2]);
}

#[test]
fn a_push_refused_by_a_hook_is_not_retried() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    let count = w.base.join("hook-runs");
    w.pre_push(&format!("echo run >> {}; exit 1", count.display()));
    let (code, text) = w.cli(&land_args(
        "feature-a",
        &["--check-again", "--", "test", "-f", "a.txt"],
    ));
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("is not retried"), "{text}");
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);
    assert_eq!(hub_main(), original);
}

impl World {
    /// The owner's pre-push hook stops the daemon, so the push goes ahead
    /// and recording the landing after it cannot.
    fn stop_daemon_in_pre_push(&self) {
        let pid = self.daemon.as_ref().unwrap().id();
        self.pre_push(&format!("kill -9 {pid}; sleep 0.5"));
    }
}

#[test]
fn a_push_whose_landing_cannot_be_recorded_is_reported_as_pushed() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    w.stop_daemon_in_pre_push();
    let (code, text) = w.cli(&land_args(
        "feature-a",
        &["--check-again", "--", "test", "-f", "a.txt"],
    ));
    let landed = hub_main();
    assert_ne!(landed, original, "the push happened: {text}");
    assert_eq!(code, 8, "{text}");
    assert!(
        text.contains(&format!("Pushed {landed} to origin/main")),
        "{text}"
    );
    assert!(text.contains("could not record the landing"), "{text}");
    assert!(text.contains("git -C"), "{text}");
    assert!(text.contains("ls-remote origin refs/heads/main"), "{text}");
    assert!(!text.contains("othing was pushed"), "{text}");
}

#[tokio::test]
async fn an_mcp_landing_whose_record_fails_reports_the_push_and_commit() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(w.base.join("state"));
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let call = |name: &'static str, arguments: serde_json::Value| {
        let request =
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone());
        let client = &client;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_error, Some(false), "{result:?}");
            result.structured_content.unwrap()
        }
    };
    let checked = call(
        "langspan_integrate_check",
        serde_json::json!({
            "request_id": "agent-check-1", "workspace_id": "app", "onto": "main",
            "name": "unit", "branches": ["feature-a"],
            "command": ["test", "-f", "a.txt"], "timeout_s": 60,
        }),
    )
    .await;
    assert_eq!(checked["check"]["result"]["type"], "passed", "{checked}");
    w.stop_daemon_in_pre_push();
    let landed = call(
        "langspan_land",
        serde_json::json!({
            "request_id": "agent-land-1", "workspace_id": "app", "onto": "main",
            "name": "unit", "branches": ["feature-a"], "apply": true,
        }),
    )
    .await;
    let tip = hub_main();
    assert_ne!(tip, original, "the push happened: {landed}");
    assert_eq!(
        (&landed["result"], &landed["exit_code"]),
        (&"pushed_not_recorded".into(), &8.into()),
        "{landed}"
    );
    assert_eq!(landed["combined"]["commit"], tip.as_str(), "{landed}");
    assert_eq!(landed["target_oid"], original.as_str(), "{landed}");
    assert!(
        landed["verify"]
            .as_str()
            .unwrap()
            .ends_with("ls-remote origin refs/heads/main"),
        "{landed}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_mcp_check_whose_command_creates_an_untracked_file_says_so_and_what_to_do() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let w = World::new();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(w.base.join("state"));
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let call = |arguments: serde_json::Value| {
        let request = CallToolRequestParams::new("langspan_integrate_check")
            .with_arguments(arguments.as_object().unwrap().clone());
        let client = &client;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_error, Some(false), "{result:?}");
            result.structured_content.unwrap()
        }
    };
    // As a build tool that writes a lock file the branch does not track.
    let check = serde_json::json!({
        "request_id": "agent-check-gen", "workspace_id": "app", "onto": "main",
        "name": "unit", "branches": ["feature-a"],
        "command": ["sh", "-c", "echo generated > Generated.lock"], "timeout_s": 60,
    });
    let first = call(check.clone()).await;
    assert_eq!(
        (
            &first["result"],
            &first["exit_code"],
            &first["check"]["result"]["type"]
        ),
        (&"checked".into(), &5.into(), &"inconclusive".into()),
        "{first}"
    );
    let reason = &first["inconclusive"];
    assert_eq!(reason["cause"], "changed_during_run", "{first}");
    assert_eq!(
        reason["changed"],
        serde_json::json!([{"path": "Generated.lock", "change": "added", "untracked": true}]),
        "{first}"
    );
    assert_eq!(reason["changed_total"], 1, "{first}");
    let message = reason["message"].as_str().unwrap();
    assert!(
        message.contains("did not hold exactly the combined tree"),
        "the same reason the CLI prints: {message}"
    );
    let next = reason["next"].as_str().unwrap();
    assert!(next.contains("Generated.lock"), "{next}");
    assert!(next.contains(".gitignore"), "{next}");
    assert!(next.contains("new request_id"), "{next}");
    assert!(
        reason["data"].as_str().unwrap().contains("data"),
        "the paths are labelled as data: {first}"
    );
    // A retry replays the record, and says why there is no fresh reason.
    let retry = call(check).await;
    assert_eq!(retry["replayed"], true, "{retry}");
    assert_eq!(retry["inconclusive"]["cause"], "replayed", "{retry}");
    assert!(
        retry["inconclusive"]["next"]
            .as_str()
            .unwrap()
            .contains("new request_id"),
        "{retry}"
    );
    // A path is repository text: redacted like other check output.
    let token = format!("ghp_{}", "aB3".repeat(12));
    let secret = call(serde_json::json!({
        "request_id": "agent-check-secret", "workspace_id": "app", "onto": "main",
        "name": "unit", "branches": ["feature-a"],
        "command": ["sh", "-c", format!("echo x > {token}")], "timeout_s": 60,
    }))
    .await;
    let reason = &secret["inconclusive"];
    assert_eq!(reason["changed"][0]["path"], "[redacted]", "{secret}");
    assert_eq!(reason["changed"][0]["untracked"], true, "{secret}");
    assert!(!reason.to_string().contains(&token), "{secret}");
    client.cancel().await.unwrap();
}

/// Every byte of the daemon's SQLite store, its write-ahead log included.
fn store_bytes(state: &Path) -> String {
    let mut bytes = Vec::new();
    for entry in fs::read_dir(state).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().contains(".sqlite") {
            bytes.extend(fs::read(&path).unwrap());
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn a_secret_on_a_check_command_line_never_reaches_the_store_cli_or_mcp() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let w = World::new();
    let token = format!("ghp_{}", "aB3".repeat(12));
    let password = "hunter2-plain";

    // The CLI: a token after `--token` and a key-shaped argument.
    let (code, text) = w.cli(&[
        "check", "run", "app", "--name", "deploy", "--json", "--", "true", "--token", password,
        &token,
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains(&token) && !text.contains(password), "{text}");
    let (code, status) = w.cli(&["check", "status", "app", "--json"]);
    assert_eq!(code, 0, "{status}");
    assert!(
        !status.contains(&token) && !status.contains(password),
        "{status}"
    );
    let status: serde_json::Value = serde_json::from_str(status.trim()).unwrap();
    let record = &status["checks"][0]["record"];
    // Still identifiable: its name and the program it ran.
    assert_eq!(record["name"], "deploy", "{status}");
    assert_eq!(
        record["command"], "true --token [redacted] [redacted]",
        "{status}"
    );

    // MCP: a combined check's record and the event history.
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(w.base.join("state"));
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let call = |name: &'static str, arguments: serde_json::Value| {
        let request =
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone());
        let client = &client;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_error, Some(false), "{result:?}");
            result.structured_content.unwrap()
        }
    };
    let checked = call(
        "langspan_integrate_check",
        serde_json::json!({
            "request_id": "agent-check-secret-argv", "workspace_id": "app", "onto": "main",
            "name": "unit", "branches": ["feature-a"],
            "command": ["true", "--token", password, &token], "timeout_s": 60,
        }),
    )
    .await;
    assert_eq!(checked["check"]["result"]["type"], "passed", "{checked}");
    assert_eq!(
        checked["check"]["command"], "true --token [redacted] [redacted]",
        "{checked}"
    );
    let events = call(
        "langspan_events",
        serde_json::json!({"after": 0, "limit": 1000}),
    )
    .await;
    assert!(
        events.to_string().contains("true --token [redacted]"),
        "{events}"
    );
    for shown in [checked.to_string(), events.to_string()] {
        assert!(
            !shown.contains(&token) && !shown.contains(password),
            "{shown}"
        );
    }
    client.cancel().await.unwrap();

    let stored = store_bytes(&w.base.join("state"));
    assert!(
        stored.contains("true --token [redacted]"),
        "the store was read"
    );
    assert!(
        !stored.contains(&token) && !stored.contains(password),
        "the secret reached the store"
    );
}

#[tokio::test]
async fn agents_check_and_land_through_the_mcp_adapter() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let w = World::new();
    let before = w.checkout();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(w.base.join("state"));
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let call = |name: &'static str, arguments: serde_json::Value| {
        let request =
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone());
        let client = &client;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_error, Some(false), "{result:?}");
            result.structured_content.unwrap()
        }
    };
    let check = serde_json::json!({
        "request_id": "agent-check-1", "workspace_id": "app", "onto": "main",
        "name": "unit", "branches": ["feature-a", "feature-b"],
        "command": ["sh", "-c", "grep -q A a.txt && grep -q B b.txt"], "timeout_s": 60,
    });
    let first = call("langspan_integrate_check", check.clone()).await;
    assert_eq!(
        (
            &first["result"],
            &first["exit_code"],
            &first["check"]["result"]["type"]
        ),
        (&"checked".into(), &0.into(), &"passed".into()),
        "{first}"
    );
    assert_eq!(first["check"]["check_id"], "agent-check-1");
    // A retry is the same record, not a second run.
    let retry = call("langspan_integrate_check", check).await;
    assert_eq!(retry["replayed"], true, "{retry}");
    assert_eq!(retry["check"], first["check"]);

    let land = |apply: bool, id: &'static str| {
        serde_json::json!({
            "request_id": id, "workspace_id": "app", "onto": "main", "name": "unit",
            "branches": ["feature-a", "feature-b"], "apply": apply, "session_id": "agent-1",
        })
    };
    let dry = call("langspan_land", land(false, "agent-land-dry")).await;
    assert_eq!(
        (&dry["result"], &dry["exit_code"]),
        (&"ready".into(), &0.into()),
        "{dry}"
    );
    assert_eq!(dry["combined"]["tree"], first["combined"]["tree"]);
    assert_eq!(hub_main(), original, "a dry run pushes nothing");

    let landed = call("langspan_land", land(true, "agent-land-1")).await;
    assert_eq!(landed["result"], "landed", "{landed}");
    assert_eq!(landed["landing"]["commit_oid"], hub_main().as_str());
    let again = call("langspan_land", land(true, "agent-land-1")).await;
    assert_eq!(
        (&again["result"], &again["replayed"]),
        (&"landed".into(), &true.into()),
        "a retry reports the landing: {again}"
    );
    assert_eq!(w.checkout(), before);
    client.cancel().await.unwrap();
}

impl World {
    /// `cli` with the push budget cut to `budget_ms`, a test-only setting.
    fn cli_with_push_budget(&self, budget_ms: u64, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .env("LANGSPAN_TEST_PUSH_BUDGET_MS", budget_ms.to_string())
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }
    /// A hook in the shared remote that runs after it has updated its refs,
    /// as a slow hosted remote answers late.
    fn post_receive(&self, script: &str) {
        let hook = self.hub().join("hooks/post-receive");
        fs::write(&hook, format!("#!/bin/sh\n{script}\nexit 0\n")).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The remote took the push but answered after the push's budget: the
/// remote's tip is the pushed commit, so it landed and is recorded.
#[test]
fn a_push_that_timed_out_after_the_remote_took_it_is_settled_as_landed() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    w.post_receive("sleep 4");
    let started = Instant::now();
    let (code, text) = w.cli_with_push_budget(
        1_000,
        &land_args("feature-a", &["--check-again", "--", "test", "-f", "a.txt"]),
    );
    let landed = hub_main();
    assert_ne!(landed, original, "the remote took the push: {text}");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("did not finish"), "{text}");
    assert!(text.contains(&format!("Landed {landed}")), "{text}");
    assert!(started.elapsed() < Duration::from_secs(30), "bounded");
    assert!(
        w.events()
            .iter()
            .any(|event| event["type"] == "landed"
                && event["landing"]["commit_oid"] == landed.as_str()),
        "the landing is recorded"
    );
}

/// The push was stopped before it reached the remote: the remote's tip is
/// the one before it, so nothing was pushed, and the report says so.
#[test]
fn a_push_that_timed_out_before_reaching_the_remote_is_settled_as_not_pushed() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let original = hub_main();
    w.pre_push("sleep 5");
    let (code, text) = w.cli_with_push_budget(
        1_000,
        &land_args("feature-a", &["--check-again", "--", "test", "-f", "a.txt"]),
    );
    assert_eq!(hub_main(), original);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("did not finish"), "{text}");
    assert!(text.contains(&format!("still at {original}")), "{text}");
    assert!(text.contains("othing was pushed"), "{text}");
    assert!(!w.events().iter().any(|event| event["type"] == "landed"));
}

/// While the push hung, someone else moved the target: the remote's tip is
/// neither commit, so whether this push happened is still not known.
#[test]
fn a_push_that_timed_out_while_the_target_moved_stays_unknown_with_how_to_check() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    let teammate = w.teammate("t.txt");
    let teammate = teammate.display().to_string();
    w.pre_push(&format!(
        "git -C {teammate} push --quiet origin HEAD:main; sleep 5"
    ));
    let (code, text) = w.cli_with_push_budget(
        2_000,
        &land_args("feature-a", &["--check-again", "--", "test", "-f", "a.txt"]),
    );
    let now = hub_main();
    assert_eq!(code, 9, "{text}");
    assert!(
        text.contains("Whether the remote branch changed is not known"),
        "{text}"
    );
    assert!(text.contains(&format!("is at {now}")), "{text}");
    assert!(text.contains("ls-remote origin refs/heads/main"), "{text}");
}

/// A landing combines named, committed
/// branches in a private index and pushes that commit, so uncommitted work
/// in the checkout, whoever made it, is never pushed and never blocks it;
/// there is no "this session's own work" case to refuse. The work stays
/// exactly where it was.
#[test]
fn uncommitted_work_in_the_checkout_never_blocks_a_landing_and_is_never_pushed() {
    let w = World::new();
    let hub_main = || git(&w.hub(), &["rev-parse", "refs/heads/main"]);
    // Half-done work: a tracked file changed and a new file, neither committed.
    fs::write(w.work().join("b.txt"), "half done\n").unwrap();
    fs::write(w.work().join("notes.txt"), "draft\n").unwrap();
    let before = w.checkout();
    let (code, text) = w.cli(&land_args(
        "feature-a",
        &["--check-again", "--", "test", "-f", "a.txt"],
    ));
    assert_eq!(code, 0, "{text}");
    let landed = hub_main();
    assert_eq!(git(&w.hub(), &["show", &format!("{landed}:a.txt")]), "A");
    assert_eq!(
        git(&w.hub(), &["show", &format!("{landed}:b.txt")]),
        "base",
        "the uncommitted change was not pushed"
    );
    let (listed, _) = (git(&w.hub(), &["ls-tree", "--name-only", &landed]), ());
    assert!(!listed.contains("notes.txt"), "{listed}");
    assert_eq!(w.checkout(), before, "the checkout is as it was");
    assert_eq!(
        fs::read_to_string(w.work().join("b.txt")).unwrap(),
        "half done\n"
    );
}

#[tokio::test]
async fn agents_see_the_landing_queue_and_conflict_hunks_and_cancel_a_wait_through_mcp() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let w = World::new();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(w.base.join("state"));
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let call = |name: &'static str, arguments: serde_json::Value| {
        let request =
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone());
        let client = &client;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_error, Some(false), "{result:?}");
            result.structured_content.unwrap()
        }
    };

    // Conflicting branches: the hunks, not only the paths.
    let conflict = call(
        "langspan_integrate_check",
        serde_json::json!({
            "request_id": "agent-check-c", "workspace_id": "app", "onto": "main",
            "name": "unit", "branches": ["feature-a", "feature-c"],
            "command": ["true"], "timeout_s": 60,
        }),
    )
    .await;
    assert_eq!(
        (&conflict["result"], &conflict["exit_code"]),
        (&"not_combinable".into(), &2.into()),
        "{conflict}"
    );
    let hunk = &conflict["conflicts"][0]["hunks"][0];
    assert_eq!(conflict["conflicts"][0]["path"], "a.txt", "{conflict}");
    assert_eq!(
        hunk["target"]["text"],
        serde_json::json!(["A"]),
        "{conflict}"
    );
    assert_eq!(
        hunk["branch"]["text"],
        serde_json::json!(["C"]),
        "{conflict}"
    );

    // A landing holds the turn and another waits: an agent sees both.
    w.pre_push("sleep 5");
    let again = ["--check-again", "--", "sh", "-c", VERIFY];
    let first = w.spawn(
        &[
            &land_args("feature-a", &["--request-id", "land-1"])[..],
            &again,
        ]
        .concat(),
    );
    w.queue_until(|queue| holds(queue, "land-1"));
    let waiter = w.spawn(
        &[
            &land_args("feature-b", &["--request-id", "land-2", "--wait-s", "60"])[..],
            &again,
        ]
        .concat(),
    );
    w.queue_until(|queue| queue["waiting"][0]["ticket"] == "land-2");
    let queue = call(
        "langspan_land_queue",
        serde_json::json!({"workspace_id": "app", "onto": "main"}),
    )
    .await;
    assert_eq!(queue["holder"]["turn_id"], "land-1", "{queue}");
    assert_eq!(queue["waiting"][0]["ticket"], "land-2", "{queue}");
    assert_eq!(queue["waiting"][0]["position"], 1, "{queue}");

    // An agent that does not wait is told who holds the turn and who waits.
    let busy = call(
        "langspan_land",
        serde_json::json!({
            "request_id": "agent-land-busy", "workspace_id": "app", "onto": "main",
            "name": "unit", "branches": ["feature-b"], "apply": true, "wait_s": 0,
        }),
    )
    .await;
    assert_eq!(
        (&busy["result"], &busy["exit_code"], &busy["reason"]),
        (&"busy".into(), &7.into(), &"turn_held".into()),
        "{busy}"
    );
    assert_eq!(busy["holder"]["turn_id"], "land-1", "{busy}");
    assert_eq!(busy["waiting"][0]["ticket"], "land-2", "{busy}");

    let cancelled = call(
        "langspan_land_cancel",
        serde_json::json!({"workspace_id": "app", "onto": "main", "request_id": "land-2"}),
    )
    .await;
    assert_eq!(cancelled["left"], true, "{cancelled}");
    let (code, _, text) = finish(waiter);
    assert_eq!(code, 7, "{text}");
    assert!(text.contains("cancelled"), "{text}");
    let (code, _, text) = finish(first);
    assert_eq!(code, 0, "{text}");
    client.cancel().await.unwrap();
}

// Roadmap F5: identifier renames on the target, adapting a branch to them,
// and rebase landings. Each runs on real Git with a local file remote and a
// real daemon.

const USERS: &str = "pub fn fetch_user(id: u32) -> u32 {\n    id + 1\n}\n";
const RENAMED: &str = "pub fn load_user(id: u32) -> u32 {\n    id + 1\n}\n";
const API: &str = "pub fn api() {}\n";
/// The branch's version: new calls to `fetch_user` at lines 3 and 6.
const API_CALLS: &str = "pub fn api() {}\n\npub fn one() -> u32 { fetch_user(1) }\n\n// between\npub fn two() -> u32 { fetch_user(2) + fetch_user(3) }\n";

impl World {
    /// `main` holds `src/users.rs` with `fetch_user`; branch `calls` adds
    /// calls to it; then the target renames it to `load_user`, same body.
    fn renamed_target(&self) {
        let work = self.work();
        fs::create_dir_all(work.join("src")).unwrap();
        self.commit_on(
            "main",
            "main",
            &[("src/users.rs", USERS.into()), ("src/api.rs", API.into())],
        );
        git(&work, &["push", "--quiet", "origin", "main"]);
        self.commit_on("calls", "main", &[("src/api.rs", API_CALLS.into())]);
        self.commit_on("main", "main", &[("src/users.rs", RENAMED.into())]);
        git(&work, &["push", "--quiet", "origin", "main"]);
    }
    fn hub_main(&self) -> String {
        git(&self.hub(), &["rev-parse", "refs/heads/main"])
    }
}

fn uses(lines: &[u64]) -> serde_json::Value {
    lines
        .iter()
        .map(|line| serde_json::json!({"path": "src/api.rs", "line": line}))
        .collect()
}

#[test]
fn a_target_rename_is_reported_with_each_use_the_branch_adds() {
    let w = World::new();
    w.renamed_target();
    let expected = serde_json::json!([{
        "branch": "calls", "from": "fetch_user", "to": "load_user",
        "path": "src/users.rs", "uses": uses(&[3, 6]),
    }]);
    let (code, report) = w.json(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "calls",
        "--json",
        "--",
        "true",
    ]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["identifier_renames"], expected, "{report}");
    // The dry run says what to do about it.
    let (code, report) = w.json(&[
        "land", "app", "--onto", "main", "--name", "other", "calls", "--json",
    ]);
    assert_eq!(code, 6, "{report}");
    assert_eq!(report["result"], "not_ready", "{report}");
    assert_eq!(report["identifier_renames"], expected, "{report}");
    let reason = report["reason"].as_str().unwrap();
    assert!(
        reason.contains("the target renamed fetch_user, which this branch still calls")
            && reason.contains("--adapt-renames"),
        "{report}"
    );
    // A branch that does not mention the old name reports none.
    let (code, report) = w.json(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature-b",
        "--json",
        "--",
        "true",
    ]);
    assert_eq!(code, 0, "{report}");
    assert!(report.get("identifier_renames").is_none(), "{report}");
}

#[test]
fn without_adapt_renames_the_branch_is_pushed_unchanged_or_not_at_all() {
    let w = World::new();
    w.renamed_target();
    let original = w.hub_main();
    let branch = git(&w.work(), &["rev-parse", "calls"]);
    // A check that needs the new name fails, and nothing is pushed.
    let (code, report) = w.json(&land_args(
        "calls",
        &[
            "--json",
            "--check-again",
            "--",
            "sh",
            "-c",
            "! grep -qw fetch_user src/api.rs",
        ],
    ));
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["result"], "check_failed", "{report}");
    assert_eq!(
        report["identifier_renames"][0]["from"], "fetch_user",
        "{report}"
    );
    assert_eq!(w.hub_main(), original, "nothing was pushed");
    // A check that passes lands the branch's own commit, unchanged, by merge.
    let (code, report) = w.json(&land_args(
        "calls",
        &["--json", "--check-again", "--", "true"],
    ));
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    assert!(report.get("adapted").is_none(), "{report}");
    let landed = w.hub_main();
    git(&w.hub(), &["merge-base", "--is-ancestor", &branch, &landed]);
    assert_eq!(
        git(&w.hub(), &["show", &format!("{landed}:src/api.rs")]),
        API_CALLS.trim_end()
    );
    assert_eq!(report["identifier_renames"][0]["uses"], uses(&[3, 6]));
}

#[test]
fn adapt_renames_pushes_the_new_name_at_each_use_only_on_its_own_passing_check() {
    let w = World::new();
    w.renamed_target();
    let before = w.checkout();
    let original = w.hub_main();
    let branch = git(&w.work(), &["rev-parse", "calls"]);
    let verify =
        "test $(grep -o load_user src/api.rs | wc -l) -eq 3 && ! grep -qw fetch_user src/api.rs";
    // Without a check of the adapted tree it is not landed.
    let (code, report) = w.json(
        &land_args("calls", &["--json", "--adapt-renames"])
            .into_iter()
            .filter(|arg| *arg != "--apply")
            .collect::<Vec<_>>(),
    );
    assert_eq!(code, 6, "{report}");
    assert_eq!(report["result"], "not_ready", "{report}");
    assert_eq!(w.hub_main(), original, "nothing was pushed");
    // A failing check of the adapted tree lands nothing either.
    let (code, report) = w.json(&land_args(
        "calls",
        &["--json", "--adapt-renames", "--check-again", "--", "false"],
    ));
    assert_eq!(code, 1, "{report}");
    assert_eq!(w.hub_main(), original, "nothing was pushed");

    let (code, report) = w.json(&land_args(
        "calls",
        &[
            "--json",
            "--adapt-renames",
            "--check-again",
            "--",
            "sh",
            "-c",
            verify,
        ],
    ));
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let landed = w.hub_main();
    let api = git(&w.hub(), &["show", &format!("{landed}:src/api.rs")]);
    assert_eq!(api, API_CALLS.replace("fetch_user", "load_user").trim_end());
    // The adapted commit sits on top of the branch, and is named.
    let adapted = report["adapted"][0]["commit"].as_str().unwrap().to_owned();
    assert_eq!(report["adapted"][0]["branch"], "calls", "{report}");
    assert_eq!(
        git(&w.hub(), &["rev-parse", &format!("{adapted}^")]),
        branch
    );
    git(
        &w.hub(),
        &["merge-base", "--is-ancestor", &adapted, &landed],
    );
    // The pushed tree is the checked tree.
    let tree = git(&w.hub(), &["rev-parse", &format!("{landed}^{{tree}}")]);
    assert_eq!(report["landing"]["tree_oid"], tree.as_str(), "{report}");
    let events = w.events();
    let check = events
        .iter()
        .find(|event| {
            event["type"] == "check_recorded"
                && event["check"]["check_id"] == report["landing"]["check_id"]
        })
        .unwrap();
    assert_eq!(check["check"]["tree_oid"], tree.as_str());
    assert_eq!(check["check"]["result"]["type"], "passed");
    let recorded = events
        .iter()
        .find(|event| event["type"] == "landed")
        .unwrap();
    assert_eq!(
        recorded["landing"]["adapted"][0],
        adapted.as_str(),
        "{recorded}"
    );
    // The owner's branch and checkout did not move.
    assert_eq!(w.checkout(), before);
    assert_eq!(git(&w.work(), &["rev-parse", "calls"]), branch);
}

#[test]
fn a_rename_that_cannot_be_applied_with_certainty_stops_the_landing() {
    let w = World::new();
    w.renamed_target();
    // The branch already mentions the new name as well: which `load_user`
    // a rewritten call would reach is not certain.
    git(&w.work(), &["checkout", "--quiet", "calls"]);
    fs::write(
        w.work().join("src/api.rs"),
        format!("{API_CALLS}pub fn load_user() {{}}\n"),
    )
    .unwrap();
    git(&w.work(), &["commit", "--quiet", "-am", "own load_user"]);
    git(&w.work(), &["checkout", "--quiet", "main"]);
    let original = w.hub_main();
    let (code, report) = w.json(&land_args(
        "calls",
        &["--json", "--adapt-renames", "--check-again", "--", "true"],
    ));
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["result"], "not_adapted", "{report}");
    let reason = report["reason"].as_str().unwrap();
    assert!(
        reason.contains("load_user") && reason.contains("by hand"),
        "{report}"
    );
    assert_eq!(w.hub_main(), original, "nothing was pushed");
}

#[test]
fn renames_past_the_bound_across_branches_stop_an_adapting_landing() {
    let w = World::new();
    let work = w.work();
    fs::create_dir_all(work.join("src")).unwrap();
    let count = 17;
    let defs = |prefix: &str| {
        numbered(count, |n| {
            format!("pub fn {prefix}{n}(id: u32) -> u32 {{\n    id + {n}\n}}\n")
        })
    };
    w.commit_on("main", "main", &[("src/many.rs", defs("old_"))]);
    git(&work, &["push", "--quiet", "origin", "main"]);
    // `first` uses the first 16 old names, `second` the seventeenth.
    let calls = |range: std::ops::RangeInclusive<usize>| {
        range
            .map(|n| format!("pub fn use{n}() -> u32 {{ old_{n}(0) }}\n"))
            .collect::<String>()
    };
    w.commit_on("first", "main", &[("src/first.rs", calls(1..=16))]);
    w.commit_on("second", "main", &[("src/second.rs", calls(17..=17))]);
    w.commit_on("main", "main", &[("src/many.rs", defs("new_"))]);
    git(&work, &["push", "--quiet", "origin", "main"]);
    let original = w.hub_main();
    let land = |branches: &[&str]| {
        w.json(
            &[
                &["land", "app", "--onto", "main", "--name", "unit"][..],
                branches,
                &[
                    "--apply",
                    "--json",
                    "--adapt-renames",
                    "--check-again",
                    "--",
                    "true",
                ],
            ]
            .concat(),
        )
    };
    // The bound is on the whole landing: the second branch's rename does not
    // fit, so nothing is adapted for it, and nothing is pushed.
    let (code, report) = land(&["first", "second"]);
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["result"], "not_adapted", "{report}");
    assert_eq!(report["branch"], "second", "{report}");
    assert!(
        report["reason"].as_str().unwrap().contains("by hand"),
        "{report}"
    );
    assert_eq!(w.hub_main(), original, "nothing was pushed");
    // Each branch alone is within the bound and is adapted.
    let (code, report) = land(&["first"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let (code, report) = land(&["second"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let landed = w.hub_main();
    let second = git(&w.hub(), &["show", &format!("{landed}:src/second.rs")]);
    assert_eq!(second, "pub fn use17() -> u32 { new_17(0) }");
}

#[test]
fn adapt_renames_rewrites_uses_in_files_whose_names_git_quotes() {
    let w = World::new();
    w.renamed_target();
    // A space (Git ends the header with a tab), a tab and a quote (Git
    // quotes the name, with escapes), and a non-ASCII name.
    let names = [
        "src/my file.rs",
        "src/tab\tname.rs",
        "src/q\"uote.rs",
        "src/ünï.rs",
    ];
    let call = "pub fn call() -> u32 { fetch_user(1) }\n";
    let files: Vec<(&str, String)> = names.iter().map(|name| (*name, call.into())).collect();
    w.commit_on("odd", "main~1", &files);
    let (code, report) = w.json(&land_args(
        "odd",
        &["--json", "--adapt-renames", "--check-again", "--", "true"],
    ));
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let mut paths: Vec<&str> = report["identifier_renames"][0]["uses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["path"].as_str().unwrap())
        .collect();
    paths.sort_unstable();
    let mut expected = names.to_vec();
    expected.sort_unstable();
    assert_eq!(paths, expected, "{report}");
    let landed = w.hub_main();
    for name in names {
        assert_eq!(
            git(&w.hub(), &["show", &format!("{landed}:{name}")]),
            call.replace("fetch_user", "load_user").trim_end(),
            "{name}"
        );
    }
}

impl World {
    /// Branch `linear`: two commits, adding c.txt and then d.txt; then a
    /// teammate lands a commit on b.txt.
    fn diverged(&self) -> Vec<String> {
        let work = self.work();
        git(&work, &["checkout", "--quiet", "-b", "linear", "main"]);
        let mut commits = Vec::new();
        for file in ["c.txt", "d.txt"] {
            fs::write(work.join(file), format!("{file}\n")).unwrap();
            git(&work, &["add", file]);
            git(&work, &["commit", "--quiet", "-m", &format!("add {file}")]);
            commits.push(git(&work, &["rev-parse", "HEAD"]));
        }
        git(&work, &["checkout", "--quiet", "main"]);
        let other = self.teammate("b.txt");
        git(&other, &["push", "--quiet", "origin", "main"]);
        commits
    }
}

#[test]
fn a_rebase_landing_pushes_linear_history_whose_tree_is_the_checked_tree() {
    let w = World::new();
    let commits = w.diverged();
    let before = w.checkout();
    let target = w.hub_main();
    let (code, report) = w.json(&land_args(
        "linear",
        &[
            "--json",
            "--rebase",
            "--check-again",
            "--",
            "sh",
            "-c",
            "test -f c.txt && test -f d.txt && grep -q theirs b.txt",
        ],
    ));
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let landed = w.hub_main();
    let range = format!("{target}..{landed}");
    assert_eq!(
        git(&w.hub(), &["rev-list", "--merges", &range]),
        "",
        "no merge commit"
    );
    assert_eq!(git(&w.hub(), &["rev-list", &range]).lines().count(), 2);
    assert_eq!(
        git(&w.hub(), &["rev-parse", &format!("{landed}~2")]),
        target
    );
    let rebased = report["rebased"].as_array().unwrap();
    assert_eq!(rebased.len(), 2, "{report}");
    assert_eq!(rebased[0]["from"], commits[0].as_str());
    assert_eq!(rebased[1]["from"], commits[1].as_str());
    assert_eq!(rebased[1]["to"], landed.as_str());
    assert_eq!(
        git(&w.hub(), &["rev-parse", &format!("{landed}~1")]),
        rebased[0]["to"].as_str().unwrap()
    );
    // The author and message are the branch's own.
    assert_eq!(
        git(&w.hub(), &["log", "-1", "--format=%an %s", &landed]),
        "Dev add d.txt"
    );
    let tree = git(&w.hub(), &["rev-parse", &format!("{landed}^{{tree}}")]);
    assert_eq!(report["landing"]["tree_oid"], tree.as_str());
    let recorded = w
        .events()
        .into_iter()
        .find(|event| event["type"] == "landed")
        .unwrap();
    assert_eq!(
        recorded["landing"]["rebased"], report["rebased"],
        "{recorded}"
    );
    assert_eq!(w.checkout(), before, "the owner's branch was not rewritten");

    // Landing the same branch again finds its patches on the target.
    for extra in [&["--rebase"][..], &[][..]] {
        let (code, report) = w.json(&land_args(
            "linear",
            &[&["--json"][..], extra, &["--check-again", "--", "true"]].concat(),
        ));
        assert_eq!(code, 0, "{extra:?}: {report}");
        assert_eq!(report["result"], "nothing_to_land", "{extra:?}: {report}");
        assert_eq!(w.hub_main(), landed);
    }
}

#[test]
fn a_rebase_that_conflicts_in_an_intermediate_commit_names_it_and_pushes_nothing() {
    let w = World::new();
    let work = w.work();
    // The branch changes a.txt and then changes it back: as a merge it is
    // clean, but its first commit conflicts with the target's a.txt.
    git(&work, &["checkout", "--quiet", "-b", "wobble", "main"]);
    fs::write(work.join("a.txt"), "X\n").unwrap();
    git(&work, &["commit", "--quiet", "-am", "x"]);
    let first = git(&work, &["rev-parse", "HEAD"]);
    fs::write(work.join("a.txt"), "base\n").unwrap();
    fs::write(work.join("e.txt"), "e\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "back"]);
    git(&work, &["checkout", "--quiet", "main"]);
    let other = w.teammate("a.txt");
    git(&other, &["push", "--quiet", "origin", "main"]);
    let original = w.hub_main();
    // As a merge it combines cleanly.
    let (code, report) = w.json(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "wobble",
        "--json",
        "--",
        "true",
    ]);
    assert_eq!(code, 0, "{report}");
    let (code, report) = w.json(&land_args(
        "wobble",
        &["--json", "--rebase", "--check-again", "--", "true"],
    ));
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["result"], "not_combinable", "{report}");
    assert_eq!(report["commit"], first.as_str(), "{report}");
    assert_eq!(report["paths"], serde_json::json!(["a.txt"]), "{report}");
    assert_eq!(
        report["conflicts"][0]["hunks"][0]["branch"]["text"],
        serde_json::json!(["X"]),
        "{report}"
    );
    assert_eq!(w.hub_main(), original, "nothing was pushed");
}

#[test]
fn a_branch_with_a_merge_commit_is_not_rebased_and_nothing_is_pushed() {
    let w = World::new();
    let work = w.work();
    git(&work, &["checkout", "--quiet", "-b", "merged", "feature-a"]);
    git(
        &work,
        &["merge", "--quiet", "--no-ff", "-m", "merge b", "feature-b"],
    );
    git(&work, &["checkout", "--quiet", "main"]);
    let other = w.teammate("c.txt");
    git(&other, &["push", "--quiet", "origin", "main"]);
    let original = w.hub_main();
    let (code, report) = w.json(&land_args(
        "merged",
        &["--json", "--rebase", "--check-again", "--", "true"],
    ));
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["result"], "not_rebased", "{report}");
    assert!(
        report["reason"]
            .as_str()
            .unwrap()
            .contains("without --rebase"),
        "{report}"
    );
    assert_eq!(w.hub_main(), original, "nothing was pushed");
}

#[test]
fn a_rebase_landing_refuses_merge_commits_even_when_it_would_fast_forward() {
    let w = World::new();
    let work = w.work();
    git(&work, &["checkout", "--quiet", "-b", "merged", "feature-a"]);
    git(
        &work,
        &["merge", "--quiet", "--no-ff", "-m", "merge b", "feature-b"],
    );
    git(&work, &["checkout", "--quiet", "main"]);
    // The target has not moved: `merged` contains it, so it would
    // fast-forward.
    let original = w.hub_main();
    for branches in [&["merged"][..], &["feature-a", "merged"][..]] {
        let (code, report) = w.json(
            &[
                &["land", "app", "--onto", "main", "--name", "unit"][..],
                branches,
                &[
                    "--apply",
                    "--json",
                    "--rebase",
                    "--check-again",
                    "--",
                    "true",
                ],
            ]
            .concat(),
        );
        assert_eq!(code, 2, "{branches:?}: {report}");
        assert_eq!(report["result"], "not_rebased", "{branches:?}: {report}");
        assert_eq!(report["branch"], "merged", "{branches:?}: {report}");
        assert!(
            report["reason"].as_str().unwrap().contains("merge commits"),
            "{report}"
        );
        assert_eq!(w.hub_main(), original, "nothing was pushed");
    }
    // Without --rebase the same branch lands as a fast-forward.
    let (code, report) = w.json(&land_args(
        "merged",
        &["--json", "--check-again", "--", "true"],
    ));
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
}

impl World {
    /// Branch `name` from main with `count` commits, each adding one file.
    fn many_commits(&self, name: &str, count: usize) {
        let work = self.work();
        git(&work, &["checkout", "--quiet", "-b", name, "main"]);
        for n in 0..count {
            let file = format!("{name}-{n}.txt");
            fs::write(work.join(&file), "x\n").unwrap();
            git(&work, &["add", &file]);
            git(&work, &["commit", "--quiet", "-m", &file]);
        }
        git(&work, &["checkout", "--quiet", "main"]);
    }
}

#[test]
fn a_rebase_landing_bounds_the_rewritten_commits_of_all_its_branches_together() {
    use langspan_core::MAX_REWRITTEN;
    let w = World::new();
    let half = MAX_REWRITTEN / 2;
    // Each branch is within the bound; together they are one over it.
    w.many_commits("first", half);
    w.many_commits("second", half + 1);
    let other = w.teammate("b.txt");
    git(&other, &["push", "--quiet", "origin", "main"]);
    let original = w.hub_main();
    let land = |branches: &[&str]| {
        w.json(
            &[
                &["land", "app", "--onto", "main", "--name", "unit"][..],
                branches,
                &[
                    "--apply",
                    "--json",
                    "--rebase",
                    "--check-again",
                    "--",
                    "true",
                ],
            ]
            .concat(),
        )
    };
    let (code, report) = land(&["first", "second"]);
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["result"], "not_rebased", "{report}");
    assert_eq!(report["branch"], "second", "{report}");
    assert!(
        report["reason"]
            .as_str()
            .unwrap()
            .contains(&MAX_REWRITTEN.to_string()),
        "{report}"
    );
    assert_eq!(w.hub_main(), original, "nothing was pushed");
    assert!(
        !w.events().iter().any(|event| event["type"] == "landed"),
        "nothing was recorded"
    );

    // Exactly the bound lands, and the landing is recorded.
    w.many_commits("third", half - 1);
    let (code, report) = land(&["first", "third", "second"]);
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["branch"], "second", "{report}");
    let (code, report) = land(&["first", "third"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    assert_eq!(
        report["rebased"].as_array().unwrap().len(),
        MAX_REWRITTEN - 1,
        "{report}"
    );
    let (code, report) = land(&["second"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
}

impl World {
    /// A state folder whose socket answers as a daemon of protocol version
    /// 5 would, the build before rebase landings: it refuses requests of
    /// later versions in version 5, refuses a request to record a landing with
    /// `rebased` or `adapted` as unreadable, and passes every other request
    /// to the real daemon. The flag says whether such a request arrived.
    fn version_5_daemon(&self) -> (PathBuf, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        let older = self.base.join("older");
        fs::create_dir(&older).unwrap();
        fs::set_permissions(&older, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = older.join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let real = self.base.join("state/daemon.sock");
        let unreadable = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = unreadable.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (real, seen) = (real.clone(), seen.clone());
                std::thread::spawn(move || {
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let refuse = |code: &str| {
                        let reply = serde_json::json!({
                            "version": 5, "request_id": request["request_id"],
                            "replayed": false, "generation": 0, "lease_valid": null,
                            "outcome": {"type": "error", "code": code, "message": "older"},
                        });
                        format!("{reply}\n")
                    };
                    let command = &request["command"];
                    let reply = if request["version"].as_u64() != Some(5) {
                        refuse("unsupported_version")
                    } else if command["op"] == "record_landing"
                        && (command.get("rebased").is_some() || command.get("adapted").is_some())
                    {
                        seen.store(true, Ordering::SeqCst);
                        refuse("invalid_request")
                    } else {
                        let mut daemon = UnixStream::connect(&real).unwrap();
                        daemon.write_all(line.as_bytes()).unwrap();
                        let mut reply = String::new();
                        BufReader::new(&daemon).read_line(&mut reply).unwrap();
                        reply
                    };
                    stream.write_all(reply.as_bytes()).unwrap();
                });
            }
        });
        (older, unreadable)
    }
    fn cli_in(&self, state: &Path, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(state)
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }
}

/// Review comment 4712442596: a CLI of this build used with a daemon of
/// the build before must not push a rebased or adapted landing that daemon
/// cannot record. It finds the daemon's version while holding the turn,
/// before the push, and stops with nothing pushed and nothing recorded.
#[test]
fn a_rebased_or_adapted_landing_is_not_pushed_through_a_daemon_that_cannot_record_it() {
    for adapt in [false, true] {
        let w = World::new();
        let (older, unreadable) = w.version_5_daemon();
        let (branch, flag) = if adapt {
            w.renamed_target();
            ("calls", "--adapt-renames")
        } else {
            w.diverged();
            ("linear", "--rebase")
        };
        let original = w.hub_main();
        let (code, text) = w.cli_in(
            &older,
            &land_args(branch, &[flag, "--check-again", "--", "true"]),
        );
        assert_ne!(code, 0, "{flag}: {text}");
        assert!(
            text.contains("protocol version 5")
                && text.contains("restart the daemon")
                && text.contains("nothing was pushed"),
            "{flag}: {text}"
        );
        assert_eq!(w.hub_main(), original, "{flag}: nothing was pushed");
        assert!(
            !unreadable.load(Ordering::SeqCst),
            "{flag}: no unreadable request was sent"
        );
        assert!(
            !w.events().iter().any(|event| event["type"] == "landed"),
            "{flag}: nothing was recorded"
        );
        // The turn was released: the same landing through the current
        // daemon lands at once.
        let (code, text) = w.cli(&land_args(
            branch,
            &[flag, "--wait-s", "0", "--check-again", "--", "true"],
        ));
        assert_eq!(code, 0, "{flag}: {text}");
        assert_ne!(w.hub_main(), original, "{flag}");

        // A version 5 client is told about that landing without the fields
        // version 6 added, in events and in status alike.
        for view in [
            &["events", "--limit", "1000", "--json"][..],
            &["status", "--json"],
        ] {
            let (code, text) = w.cli_in(&older, view);
            assert_eq!(code, 0, "{view:?}: {text}");
            assert!(text.contains("landing_id"), "{view:?}: {text}");
            assert!(
                !text.contains("\"rebased\"") && !text.contains("\"adapted\""),
                "{view:?}: {text}"
            );
        }
        let (_, current) = w.cli(&["events", "--limit", "1000", "--json"]);
        assert!(
            current.contains(if adapt { "\"adapted\"" } else { "\"rebased\"" }),
            "{current}"
        );
    }
}

impl World {
    /// Review comment 4712442606: `stacked-a` adds `feature.txt`,
    /// `stacked-b` descends from it and deletes it, and the target adds the
    /// identical file on its own, so merging `stacked-a` changes nothing.
    /// `unrelated` adds another file. The target is pushed.
    fn stacked_no_op(&self) {
        let work = self.work();
        self.commit_on("stacked-a", "main", &[("feature.txt", "feature\n".into())]);
        git(
            &work,
            &["checkout", "--quiet", "-B", "stacked-b", "stacked-a"],
        );
        git(&work, &["rm", "--quiet", "feature.txt"]);
        git(&work, &["commit", "--quiet", "-m", "remove feature"]);
        git(&work, &["checkout", "--quiet", "main"]);
        self.commit_on("unrelated", "main", &[("other.txt", "other\n".into())]);
        self.commit_on(
            "main",
            "main",
            &[
                ("feature.txt", "feature\n".into()),
                ("main.txt", "main\n".into()),
            ],
        );
        git(&work, &["push", "--quiet", "origin", "main"]);
    }
    fn land_branches(&self, branches: &[&str], verify: &str) -> (i32, serde_json::Value) {
        self.json(
            &[
                &["land", "app", "--onto", "main", "--name", "unit"][..],
                branches,
                &[
                    "--apply",
                    "--json",
                    "--check-again",
                    "--",
                    "sh",
                    "-c",
                    verify,
                ],
            ]
            .concat(),
        )
    }
    fn is_ancestor(&self, ancestor: &str, of: &str) -> bool {
        Process::new("git")
            .arg("-C")
            .arg(self.hub())
            .args(["merge-base", "--is-ancestor", ancestor, of])
            .status()
            .unwrap()
            .success()
    }
}

/// A merge that changes nothing still carries its branch's history to the
/// branches after it: `stacked-b`'s deletion is applied, not lost.
#[test]
fn a_branch_stacked_on_a_no_op_merge_still_applies_its_own_change() {
    let w = World::new();
    w.stacked_no_op();
    let original = w.hub_main();
    let (code, report) = w.land_branches(&["stacked-a", "stacked-b"], "test ! -e feature.txt");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let landed = w.hub_main();
    assert_ne!(landed, original);
    assert_eq!(
        git(&w.hub(), &["ls-tree", "--name-only", &landed]),
        "a.txt\nb.txt\nmain.txt",
        "the deletion landed"
    );
    let b = git(&w.work(), &["rev-parse", "stacked-b"]);
    assert!(
        w.is_ancestor(&b, &landed),
        "stacked-b is part of the target"
    );
}

/// A no-op merge stays only where a later branch needs its history: as the
/// last merge, or the only one, it is left out.
#[test]
fn a_no_op_merge_is_left_out_when_nothing_after_it_changes_the_result() {
    let w = World::new();
    w.stacked_no_op();
    let original = w.hub_main();
    let (code, report) = w.land_branches(&["stacked-a"], "true");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "nothing_to_land", "{report}");
    assert_eq!(w.hub_main(), original);

    // Before an unrelated branch, its history is kept, under that merge.
    let a = git(&w.work(), &["rev-parse", "stacked-a"]);
    let (code, report) = w.land_branches(&["stacked-a", "unrelated"], "test -f other.txt");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["result"], "landed", "{report}");
    let landed = w.hub_main();
    assert!(w.is_ancestor(&a, &landed), "{report}");

    // After one, it is left out: the landing is that branch's merge alone.
    let w = World::new();
    w.stacked_no_op();
    let original = w.hub_main();
    let (code, report) = w.land_branches(&["unrelated", "stacked-a"], "test -f other.txt");
    assert_eq!(code, 0, "{report}");
    let landed = w.hub_main();
    assert_eq!(
        git(&w.hub(), &["rev-parse", &format!("{landed}^1")]),
        original,
        "one merge on top of the target: {report}"
    );
    assert!(!w.is_ancestor(&a, &landed), "{report}");
}

/// A landing that rewrote nothing records the same through either daemon.
#[test]
fn a_merge_landing_still_lands_through_a_daemon_of_the_build_before() {
    let w = World::new();
    let (older, unreadable) = w.version_5_daemon();
    w.diverged();
    let original = w.hub_main();
    let (code, text) = w.cli_in(
        &older,
        &land_args("linear", &["--json", "--check-again", "--", "true"]),
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"result\":\"landed\""), "{text}");
    assert_ne!(w.hub_main(), original);
    assert!(!unreadable.load(Ordering::SeqCst));
    assert!(w.events().iter().any(|event| event["type"] == "landed"));
}

#[test]
fn a_changed_file_that_does_not_parse_makes_rename_detection_say_so() {
    let w = World::new();
    w.renamed_target();
    // The target also changes a source file into one that does not parse.
    w.commit_on("main", "main", &[("src/broken.rs", "pub fn a( {\n".into())]);
    git(&w.work(), &["push", "--quiet", "origin", "main"]);
    let (code, report) = w.json(&[
        "integrate",
        "check",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "calls",
        "--json",
        "--",
        "true",
    ]);
    assert_eq!(code, 0, "{report}");
    assert!(report.get("identifier_renames").is_none(), "{report}");
    let why = report["identifier_renames_incomplete"].as_str().unwrap();
    assert!(why.contains("src/broken.rs"), "{report}");
    // Adapting stops only when there is something it would rewrite; here
    // there is nothing it found, so the branch lands as it is.
    let (code, report) = w.json(&land_args(
        "calls",
        &["--json", "--adapt-renames", "--check-again", "--", "true"],
    ));
    assert_eq!(code, 0, "{report}");
    assert!(report.get("adapted").is_none(), "{report}");
}

#[tokio::test]
async fn two_agent_sessions_landing_through_mcp_land_in_arrival_order() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;
    async fn call(
        client: &Client,
        name: &'static str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let request =
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone());
        let result = tokio::time::timeout(Duration::from_secs(90), client.call_tool(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.is_error, Some(false), "{result:?}");
        result.structured_content.unwrap()
    }
    let w = World::new();
    let original = w.hub_main();
    let connect = || async {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
        command.env("LANGSPAN_AUTOSTART", "off");
        command
            .arg("mcp")
            .arg("--state-dir")
            .arg(w.base.join("state"));
        ().serve(TokioChildProcess::new(command).unwrap())
            .await
            .unwrap()
    };
    let (one, two) = (connect().await, connect().await);
    // Evidence for each tree either landing can push: A alone, then A and B.
    for (id, branches) in [
        ("c-a", vec!["feature-a"]),
        ("c-ab", vec!["feature-a", "feature-b"]),
    ] {
        let checked = call(
            &one,
            "langspan_integrate_check",
            serde_json::json!({
                "request_id": id, "workspace_id": "app", "onto": "main", "name": "unit",
                "branches": branches, "command": ["true"], "timeout_s": 60,
            }),
        )
        .await;
        assert_eq!(checked["exit_code"], 0, "{checked}");
    }
    w.pre_push("sleep 3");
    let land = |id: &str, branch: &str, session: &str| {
        serde_json::json!({
            "request_id": id, "workspace_id": "app", "onto": "main", "name": "unit",
            "branches": [branch], "apply": true, "session_id": session, "wait_s": 60,
        })
    };
    let first = call(
        &one,
        "langspan_land",
        land("agent-1", "feature-a", "session-1"),
    );
    let second = async {
        // Polled on a blocking thread so the first call keeps running.
        let state = w.base.join("state");
        tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
                    .env("LANGSPAN_AUTOSTART", "off")
                    .arg("--state-dir")
                    .arg(&state)
                    .args(["land-queue", "app", "--onto", "main", "--json"])
                    .output()
                    .unwrap();
                let queue: serde_json::Value =
                    serde_json::from_slice(&output.stdout).unwrap_or_default();
                if holds(&queue, "agent-1") {
                    return;
                }
                assert!(Instant::now() < deadline, "the first never took the turn");
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .await
        .unwrap();
        call(
            &two,
            "langspan_land",
            land("agent-2", "feature-b", "session-2"),
        )
        .await
    };
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first["result"], "landed", "{first}");
    assert_eq!(second["result"], "landed", "{second}");
    assert!(second["waited_ms"].as_u64().unwrap() > 0, "{second}");
    let (a, b) = (
        first["landing"]["commit_oid"].as_str().unwrap(),
        second["landing"]["commit_oid"].as_str().unwrap(),
    );
    assert_eq!(second["landing"]["inputs"][0], a, "{second}");
    assert_eq!(w.hub_main(), b);
    git(&w.hub(), &["merge-base", "--is-ancestor", &original, a]);
    git(&w.hub(), &["merge-base", "--is-ancestor", a, b]);
    let taken: Vec<String> = w
        .events()
        .into_iter()
        .filter(|event| event["type"] == "turn_taken")
        .map(|event| {
            event["turn"]["session_id"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    assert_eq!(taken, ["session-1", "session-2"]);
    one.cancel().await.unwrap();
    two.cancel().await.unwrap();
}

/// `save` takes three arguments on the target; `load` is exported.
const STORE: &str = "pub fn save(\n    x: u32,\n    y: u32,\n    z: u32,\n) {\n    let _ = (x, y, z);\n}\n\n\n\npub fn load() -> u32 {\n    0\n}\n";
const IMPORT: &str = "pub fn run() {\n}\n";

impl World {
    /// Rust sources on the target, and branches that each change them so
    /// that Git combines every pair cleanly: `feature-sync` adds a fourth
    /// parameter to `save`, `feature-retry` changes its first one,
    /// `feature-import` adds a call of `save` with three arguments,
    /// `feature-drop` removes `load` and `feature-use` adds a file that
    /// calls it.
    ///
    /// The branches are made in another clone and fetched, so the
    /// workspace's checkout goes from no sources straight to the target's
    /// two. It returns once the daemon's own index holds both at their
    /// current content: that index follows the checkout in the background,
    /// and a test that compares its bytes must start after it settles.
    fn with_definitions(&self) {
        let work = self.work();
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(work.join("src/store.rs"), STORE).unwrap();
        fs::write(work.join("src/import.rs"), IMPORT).unwrap();
        git(&work, &["add", "src"]);
        git(&work, &["commit", "--quiet", "-m", "sources"]);
        git(&work, &["push", "--quiet", "origin", "main"]);
        let side = self.base.join("side");
        git(
            &self.base,
            &[
                "clone",
                "--quiet",
                self.hub().to_str().unwrap(),
                side.to_str().unwrap(),
            ],
        );
        git(&side, &["config", "user.name", "Dev"]);
        git(&side, &["config", "user.email", "dev@example.invalid"]);
        let edits: [(&str, &str, String); 5] = [
            (
                "feature-sync",
                "src/store.rs",
                STORE.replace("    z: u32,\n", "    z: u32,\n    sync: bool,\n"),
            ),
            (
                "feature-retry",
                "src/store.rs",
                STORE.replace("    x: u32,\n", "    x: u64,\n"),
            ),
            (
                "feature-import",
                "src/import.rs",
                "pub fn run() {\n    crate::store::save(1, 2, 3);\n}\n".into(),
            ),
            (
                "feature-drop",
                "src/store.rs",
                STORE.replace("pub fn load() -> u32 {\n    0\n}\n", ""),
            ),
            (
                "feature-use",
                "src/report.rs",
                "pub fn report() -> u32 {\n    crate::store::load()\n}\n".into(),
            ),
        ];
        let mut fetch = vec!["fetch".to_string(), "--quiet".into(), "origin".into()];
        for (branch, path, text) in edits {
            git(&side, &["checkout", "--quiet", "-b", branch, "main"]);
            fs::write(side.join(path), text).unwrap();
            git(&side, &["add", path]);
            git(&side, &["commit", "--quiet", "-m", branch]);
            git(&side, &["push", "--quiet", "origin", branch]);
            fetch.push(format!("{branch}:{branch}"));
        }
        git(&work, &fetch.iter().map(String::as_str).collect::<Vec<_>>());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (_, text) = self.cli(&["graph", "symbol", "app", "save", "--json"]);
            let outcome = serde_json::from_str::<serde_json::Value>(&text)
                .map(|value| value["outcome"].clone())
                .unwrap_or_default();
            let coverage = &outcome["coverage"];
            if coverage["indexable_files"] == 2
                && coverage["current_files"] == 2
                && coverage["partial_files"] == 0
                && coverage.get("blind_in_scope").is_none()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the target's sources were never indexed: {text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn check_json(&self, branches: &[&str], extra: &[&str], command: &[&str]) -> serde_json::Value {
        let (code, text) = self.integrate(branches, &[&["--json"][..], extra].concat(), command);
        assert!(code == 0 || code == 1, "{text}");
        serde_json::from_str(text.lines().last().unwrap()).unwrap()
    }
    fn integrate(&self, branches: &[&str], extra: &[&str], command: &[&str]) -> (i32, String) {
        self.cli(
            &[
                &[
                    "integrate",
                    "check",
                    "app",
                    "--onto",
                    "main",
                    "--name",
                    "unit",
                ][..],
                extra,
                branches,
                &["--"],
                command,
            ]
            .concat(),
        )
    }
    /// Every row of the source index, by table and sorted, to show a run
    /// put nothing there. The daemon rescans the workspace after Git
    /// activity, and each rescan commits a pass, rewriting the same rows
    /// and counting the pass in `passes`: neither the file bytes nor that
    /// counter can show whether a check wrote anything.
    fn graph_rows(&self) -> Vec<(String, Vec<String>)> {
        let connection = rusqlite::Connection::open_with_flags(
            self.base.join("state").join("graph.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        // One read snapshot for every table.
        connection.execute_batch("BEGIN").unwrap();
        let tables: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let rows = tables
            .into_iter()
            .filter(|table| table != "passes")
            .map(|table| {
                let mut statement = connection
                    .prepare(&format!("SELECT * FROM \"{table}\""))
                    .unwrap();
                let width = statement.column_count();
                let mut rows: Vec<String> = statement
                    .query_map([], |row| {
                        (0..width)
                            .map(|i| row.get::<_, rusqlite::types::Value>(i))
                            .collect::<Result<Vec<_>, _>>()
                            .map(|values| format!("{values:?}"))
                    })
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                rows.sort();
                (table, rows)
            })
            .collect();
        connection.execute_batch("COMMIT").unwrap();
        rows
    }
}

#[test]
fn symbols_name_a_call_that_a_clean_combination_breaks_and_never_change_the_result() {
    let w = World::new();
    w.with_definitions();
    let graph = w.graph_rows();
    assert!(
        graph
            .iter()
            .any(|(table, rows)| table == "symbols" && !rows.is_empty()),
        "the target is indexed before the checks: {graph:?}"
    );
    let pair = ["feature-sync", "feature-import"];
    let report = w.check_json(&pair, &["--symbols", "--request-id", "s1"], &["true"]);
    assert_eq!(report["check"]["result"]["type"], "passed", "{report}");
    let found = &report["check"]["symbol_findings"];
    assert_eq!(found["findings"].as_array().unwrap().len(), 1, "{report}");
    let finding = &found["findings"][0];
    for (field, value) in [
        ("finding", "changed_and_called"),
        ("name", "save"),
        ("branch", "feature-sync"),
        ("path", "src/store.rs"),
        ("other_branch", "feature-import"),
        ("other_path", "src/import.rs"),
    ] {
        assert_eq!(finding[field], value, "{field}: {report}");
    }
    assert_eq!(finding["line"], 1, "{report}");
    assert_eq!(finding["other_line"], 2, "{report}");
    assert_eq!(finding["args"], 3, "{report}");
    assert_eq!(w.graph_rows(), graph, "the source index is not written");

    // A finding never fails a passing check, and is told to a person.
    let (code, text) = w.integrate(&pair, &["--symbols", "--request-id", "s2"], &["true"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("unit passed on the combined tree"), "{text}");
    assert!(
        text.contains("Definition findings (found by name; advisory):")
            && text.contains(
                "changed_and_called: \"save\" now takes 4 arguments (feature-sync, \"src/store.rs\":1); feature-import calls it with 3 at \"src/import.rs\":2"
            ),
        "{text}"
    );
    // Nor passes a failing one.
    let (code, text) = w.integrate(&pair, &["--symbols", "--request-id", "s3"], &["false"]);
    assert_eq!(code, 1, "{text}");

    // Without --symbols the output is as before.
    let plain = w.check_json(&pair, &["--request-id", "p1"], &["true"]);
    assert!(!plain.to_string().contains("symbol_findings"), "{plain}");
    let (code, text) = w.integrate(&pair, &["--request-id", "p2"], &["true"]);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("Definition findings"), "{text}");
    assert_eq!(w.graph_rows(), graph, "the source index is not written");

    // A landing compares nothing.
    let (code, text) = w.cli(&[
        "land",
        "app",
        "--onto",
        "main",
        "--name",
        "unit",
        "--symbols",
        "feature-sync",
    ]);
    assert_ne!(code, 0, "{text}");
    assert!(
        text.contains("--symbols is valid only for `integrate check`"),
        "{text}"
    );
}

#[test]
fn symbols_name_two_signature_changes_and_a_removal_that_git_combines_cleanly() {
    let w = World::new();
    w.with_definitions();
    let both = w.check_json(
        &["feature-sync", "feature-retry"],
        &["--symbols"],
        &["true"],
    );
    assert_eq!(both["result"], "checked", "Git combined them: {both}");
    let findings = both["check"]["symbol_findings"]["findings"]
        .as_array()
        .unwrap();
    assert_eq!(findings.len(), 1, "{both}");
    assert_eq!(findings[0]["finding"], "both_changed", "{both}");
    assert_eq!(findings[0]["name"], "save", "{both}");
    assert_eq!(findings[0]["other_branch"], "feature-retry", "{both}");
    assert!(
        findings[0]["after"]
            .as_str()
            .unwrap()
            .contains("sync: bool")
            && findings[0]["other_after"]
                .as_str()
                .unwrap()
                .contains("x: u64"),
        "{both}"
    );

    let removed = w.check_json(&["feature-drop", "feature-use"], &["--symbols"], &["true"]);
    let findings = removed["check"]["symbol_findings"]["findings"]
        .as_array()
        .unwrap();
    assert_eq!(findings.len(), 1, "{removed}");
    assert_eq!(findings[0]["finding"], "removed_and_used", "{removed}");
    assert_eq!(findings[0]["name"], "load", "{removed}");
    assert_eq!(findings[0]["other_path"], "src/report.rs", "{removed}");
    assert_eq!(findings[0]["other_line"], 2, "{removed}");

    // Unrelated branches give no finding, and say what was indexed.
    let none = w.check_json(&["feature-retry", "feature-use"], &["--symbols"], &["true"]);
    let found = &none["check"]["symbol_findings"];
    assert_eq!(found["findings"], serde_json::json!([]), "{none}");
    assert_eq!(found["files_indexed"], 2, "{none}");
}

#[test]
fn symbols_past_their_time_limit_are_skipped_with_a_note_and_the_check_runs() {
    let w = World::new();
    w.with_definitions();
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .env("LANGSPAN_TEST_SYMBOL_BUDGET_MS", "0")
        .arg("--state-dir")
        .arg(w.base.join("state"))
        .args([
            "integrate",
            "check",
            "app",
            "--onto",
            "main",
            "--name",
            "unit",
            "--symbols",
            "--json",
            "feature-sync",
            "feature-import",
            "--",
            "true",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{text}");
    let report: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(report["check"]["result"]["type"], "passed", "{report}");
    let found = &report["check"]["symbol_findings"];
    assert_eq!(found["findings"], serde_json::json!([]), "{report}");
    assert!(
        found["note"].as_str().unwrap().contains("skipped"),
        "{report}"
    );
}

#[tokio::test]
async fn agents_ask_for_definition_findings_through_mcp() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let w = World::new();
    w.with_definitions();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(w.base.join("state"));
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let call = |arguments: serde_json::Value| {
        let request = CallToolRequestParams::new("langspan_integrate_check")
            .with_arguments(arguments.as_object().unwrap().clone());
        let client = &client;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_error, Some(false), "{result:?}");
            result.structured_content.unwrap()
        }
    };
    let arguments = serde_json::json!({
        "request_id": "agent-symbols", "workspace_id": "app", "onto": "main",
        "name": "unit", "branches": ["feature-sync", "feature-import"],
        "command": ["true"], "timeout_s": 60, "symbols": true,
    });
    let first = call(arguments.clone()).await;
    assert_eq!(first["exit_code"], 0, "{first}");
    let findings = &first["check"]["symbol_findings"]["findings"];
    assert_eq!(findings[0]["finding"], "changed_and_called", "{first}");
    assert_eq!(findings[0]["other_path"], "src/import.rs", "{first}");
    // A retry answers from the record, findings included.
    let retry = call(arguments).await;
    assert_eq!(retry["replayed"], true, "{retry}");
    assert_eq!(retry["check"], first["check"]);
    // Without symbols, nothing about definitions.
    let plain = call(serde_json::json!({
        "request_id": "agent-plain", "workspace_id": "app", "onto": "main",
        "name": "unit", "branches": ["feature-sync", "feature-import"],
        "command": ["true"], "timeout_s": 60,
    }))
    .await;
    assert!(!plain.to_string().contains("symbol_findings"), "{plain}");
    client.cancel().await.unwrap();
}
