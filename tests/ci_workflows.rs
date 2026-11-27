#![cfg(unix)]
//! The workflows that provide required checks also run on merge-queue
//! commits, and check.yml's "Decide what the change touches" step decides
//! from the merge group's base there. The step's own text, taken from
//! check.yml, runs here with bash in temporary git repositories.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn workflow(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/workflows")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn required_check_workflows_run_on_merge_queue_commits() {
    for name in ["check.yml", "content.yml"] {
        let text = workflow(name);
        let on = text
            .split("\non:\n")
            .nth(1)
            .and_then(|rest| rest.split("\npermissions:").next())
            .unwrap_or_else(|| panic!("{name} has no top-level `on:` block"));
        assert!(
            on.lines().any(|line| line.trim_end() == "  merge_group:"),
            "{name} does not run on merge_group, so a merge queue would wait for its checks"
        );
    }
}

/// The step's environment names and script, as check.yml has them.
struct Step {
    bench_paths: String,
    script: String,
}

fn decide_step() -> Step {
    let text = workflow("check.yml");
    let step = text
        .split("- name: Decide what the change touches\n")
        .nth(1)
        .expect("check.yml has the step `Decide what the change touches`");
    let bench_paths = step
        .split("BENCH_PATHS: >-\n")
        .nth(1)
        .expect("the step sets BENCH_PATHS")
        .lines()
        .take_while(|line| !line.trim_start().starts_with("run:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    let script = step
        .split("        run: |\n")
        .nth(1)
        .expect("the step has a run block")
        .lines()
        .take_while(|line| line.is_empty() || line.starts_with("          "))
        .map(|line| line.strip_prefix("          ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    Step {
        bench_paths,
        script,
    }
}

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_file(self.0.with_extension("github-output"));
    }
}

impl Repo {
    fn new() -> Repo {
        let dir = std::env::temp_dir().join(format!(
            "ls-ci-workflows-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let repo = Repo(dir);
        repo.git(&["init", "-q"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Commits `path` and returns the new commit.
    fn commit(&self, path: &str) -> String {
        let file = self.0.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, path).unwrap();
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", path]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Runs the step for `event` and returns (code, dry_runs).
    fn decide(&self, event: &str, base: &str, head: &str) -> (String, String) {
        let step = decide_step();
        // Outside the work tree, so a later commit never adds it.
        let output = self.0.with_extension("github-output");
        let _ = fs::remove_file(&output);
        let out = Command::new("bash")
            .args(["-e", "-c", &step.script])
            .current_dir(&self.0)
            .env("EVENT", event)
            .env("BEFORE", "")
            .env("BASE_SHA", base)
            .env("HEAD_SHA", head)
            .env("BENCH_PATHS", &step.bench_paths)
            .env("GITHUB_OUTPUT", &output)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "the step failed: {out:?}");
        let written = fs::read_to_string(&output).unwrap();
        let value = |key: &str| {
            written
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .unwrap_or_else(|| panic!("no {key} in {written:?}"))
                .to_owned()
        };
        (value("code"), value("dry_runs"))
    }
}

fn decided(code: &str, dry_runs: &str) -> (String, String) {
    (code.to_owned(), dry_runs.to_owned())
}

#[test]
fn a_merge_group_decides_from_its_base_not_only_head_parent() {
    let repo = Repo::new();
    let base = repo.commit("src/lib.rs");
    // A pull request queued ahead touched the benchmark harness; this one
    // changes only Markdown. HEAD^1..HEAD alone would skip the dry runs.
    repo.commit("benchmarks/coordination/driver.py");
    let head = repo.commit("docs/notes.md");
    assert_eq!(
        repo.decide("merge_group", &base, &head),
        decided("true", "run")
    );
}

#[test]
fn a_merge_group_of_markdown_or_unrelated_code_skips_what_a_pull_request_skips() {
    let repo = Repo::new();
    let base = repo.commit("src/lib.rs");
    repo.commit("docs/a.md");
    let head = repo.commit("README.md");
    assert_eq!(
        repo.decide("merge_group", &base, &head),
        decided("false", "skip")
    );
    let head = repo.commit("src/cli.rs");
    assert_eq!(
        repo.decide("merge_group", &base, &head),
        decided("true", "skip")
    );
}

#[test]
fn a_merge_group_without_a_base_runs_everything() {
    let repo = Repo::new();
    repo.commit("src/lib.rs");
    let head = repo.commit("docs/a.md");
    assert_eq!(
        repo.decide("merge_group", "", &head),
        decided("true", "run")
    );
}

#[test]
fn a_pull_request_still_decides_from_head_parent() {
    let repo = Repo::new();
    repo.commit("benchmarks/coordination/driver.py");
    let head = repo.commit("src/cli.rs");
    assert_eq!(
        repo.decide("pull_request", "", &head),
        decided("true", "skip")
    );
    let head = repo.commit("docs/a.md");
    assert_eq!(
        repo.decide("pull_request", "", &head),
        decided("false", "skip")
    );
}
