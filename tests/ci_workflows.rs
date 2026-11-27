#![cfg(unix)]
//! The workflows that provide required checks also run on merge-queue
//! commits, and scripts/ci-changes.sh, which each check.yml job runs to
//! decide what the change touches, decides from the merge group's base
//! there. It runs here with bash in temporary git repositories. The tests
//! that run under nextest and the report of those that passed only on retry
//! are checked here too.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn workflow(name: &str) -> String {
    read(&format!(".github/workflows/{name}"))
}

/// The text of one job in a workflow: from `  id:` to the next job.
fn job(text: &str, id: &str) -> String {
    let header = format!("  {id}:");
    let mut lines = text.lines().skip_while(|line| *line != header);
    let first = lines.next().unwrap_or_else(|| panic!("no job {id}"));
    let body = lines.take_while(|line| line.is_empty() || line.starts_with("   "));
    std::iter::once(first)
        .chain(body)
        .collect::<Vec<_>>()
        .join("\n")
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

    /// Runs scripts/ci-changes.sh for `event` and returns (code, dry_runs).
    fn decide(&self, event: &str, base: &str, head: &str) -> (String, String) {
        // Outside the work tree, so a later commit never adds it.
        let output = self.0.with_extension("github-output");
        let _ = fs::remove_file(&output);
        let out = Command::new("bash")
            .arg(root().join("scripts/ci-changes.sh"))
            .current_dir(&self.0)
            .env("EVENT", event)
            .env("BEFORE", "")
            .env("BASE_SHA", base)
            .env("HEAD_SHA", head)
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

#[test]
fn linux_and_macos_start_together() {
    let text = workflow("check.yml");
    for id in ["linux", "macos"] {
        assert!(
            !job(&text, id).contains("\n    needs:"),
            "the {id} job waits for another job, so Linux and macOS run in series"
        );
    }
}

#[test]
fn each_job_takes_its_runner_from_a_repository_variable() {
    let text = workflow("check.yml");
    assert!(
        job(&text, "linux").contains("\n    runs-on: ${{ vars.LINUX_RUNNER || 'ubuntu-24.04' }}\n")
    );
    // On a pull request the macOS check only reports, on a standard runner.
    assert!(job(&text, "macos").contains(
        "\n    runs-on: ${{ github.event_name == 'pull_request' && 'ubuntu-24.04' || vars.MACOS_RUNNER || 'macos-26' }}\n"
    ));
}

/// The lines of one `[profile.NAME]` section of .config/nextest.toml.
fn profile(config: &str, name: &str) -> String {
    let header = format!("[profile.{name}]");
    config
        .lines()
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_pull_request_runs_the_fast_lane_and_a_queued_commit_every_test() {
    let text = workflow("check.yml");
    for line in [
        "LANGSPAN_CHECK_LANE: ${{ github.event_name == 'pull_request' && 'fast' || 'full' }}",
        "NEXTEST_PROFILE: ${{ github.event_name == 'pull_request' && 'ci-fast' || 'ci' }}",
    ] {
        assert!(text.contains(line), "check.yml lacks {line}");
    }
    for id in ["linux", "macos"] {
        assert!(
            job(&text, id).contains(
                "\n    timeout-minutes: ${{ github.event_name == 'pull_request' && 10 || github.event_name == 'merge_group' && 15 || 30 }}\n"
            ),
            "the {id} job has no time limit by lane"
        );
    }
    // The macOS check passes at once on a pull request and says why; it
    // checks out and decides nothing there, so every later step skips.
    let macos = job(&text, "macos");
    assert!(macos.contains("the full macOS run happens in the merge queue"));
    for step in [
        "uses: actions/checkout@",
        "name: Decide what the change touches",
    ] {
        assert!(
            macos.contains(&format!(
                "- if: github.event_name != 'pull_request'\n        {step}"
            )),
            "macOS runs `{step}` on a pull request"
        );
    }
    let config = read(".config/nextest.toml");
    let fast = profile(&config, "ci-fast");
    assert!(fast.contains("inherits = \"ci\""), "{fast}");
    assert!(fast.contains("default-filter = "), "{fast}");
    for name in ["default", "ci"] {
        assert!(
            !profile(&config, name).contains("default-filter"),
            "the {name} profile leaves tests out, so the full lane would too"
        );
    }
}

#[test]
fn check_sh_refuses_an_unknown_lane_before_it_runs_anything() {
    // Outside the repository, so a check that started would fail otherwise.
    let dir = temp_dir("check-lane");
    let out = Command::new("sh")
        .arg(root().join("scripts/check.sh"))
        .current_dir(&dir)
        .env("LANGSPAN_CHECK_LANE", "quick")
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LANGSPAN_CHECK_LANE is 'quick'; set it to fast or full"),
        "{stderr}"
    );
}

#[test]
fn the_suite_runs_under_nextest_with_retries_only_off_pull_requests() {
    let script = read("scripts/test-foreign-home.sh");
    assert!(script.contains("cargo nextest run --workspace --locked"));
    assert!(script.contains("cargo test --doc --workspace --locked"));
    // A flaky test must show on a pull request; in the merge queue and on
    // main it may pass on a later try, and is then reported.
    assert!(
        workflow("check.yml")
            .contains("NEXTEST_RETRIES: ${{ github.event_name == 'pull_request' && '0' || '2' }}")
    );
    let config = read(".config/nextest.toml");
    for line in [
        "slow-timeout = { period = \"60s\", terminate-after = 3 }",
        "leak-timeout = { period = \"1s\", result = \"fail\" }",
        "test-threads = \"num-cpus\"",
        "junit.path = \"junit.xml\"",
    ] {
        assert!(
            config.lines().any(|l| l == line),
            "nextest.toml lacks {line}"
        );
    }
    assert!(
        !config.lines().any(|l| l.starts_with("retries")),
        "retries come from NEXTEST_RETRIES by event, not from the profile"
    );
}

/// A new empty directory under the system temporary directory.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ls-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs scripts/flaky-tests.py on `junit` and returns (stdout, summary,
/// GITHUB_OUTPUT).
fn flaky_report(junit: &Path) -> (String, String, String) {
    let dir = temp_dir("flaky-report");
    let summary = dir.join("summary.md");
    let output = dir.join("output");
    let out = Command::new("python3")
        .args(["-I", "-B"])
        .arg(root().join("scripts/flaky-tests.py"))
        .arg(junit)
        .env("GITHUB_STEP_SUMMARY", &summary)
        .env("GITHUB_OUTPUT", &output)
        .output()
        .expect("python3 runs scripts/flaky-tests.py");
    let read_or_empty = |p: &Path| fs::read_to_string(p).unwrap_or_default();
    let result = (read_or_empty(&summary), read_or_empty(&output));
    let _ = fs::remove_dir_all(&dir);
    assert!(out.status.success(), "flaky-tests.py failed: {out:?}");
    (String::from_utf8(out.stdout).unwrap(), result.0, result.1)
}

#[test]
fn the_flaky_report_names_a_test_that_passed_only_on_retry_and_no_other() {
    let (stdout, summary, output) =
        flaky_report(&root().join("tests/fixtures/nextest/junit-retries.xml"));
    assert_eq!(stdout, "flaky_sample passes_on_its_second_try\n");
    assert!(summary.contains("passed only on retry"), "{summary}");
    assert!(summary.contains("`flaky_sample passes_on_its_second_try`"));
    // A test that failed every try fails the run; it is not flaky.
    assert!(!summary.contains("fails_every_try"), "{summary}");
    assert!(!summary.contains("always_passes"), "{summary}");
    let mut lines = output.lines();
    let delimiter = lines
        .next()
        .and_then(|l| l.strip_prefix("flaky<<"))
        .unwrap_or_else(|| panic!("no flaky output in {output:?}"));
    assert_eq!(lines.next(), Some("flaky_sample passes_on_its_second_try"));
    assert_eq!(lines.next(), Some(delimiter));
}

#[test]
fn the_flaky_report_says_so_when_no_test_or_no_report_is_there() {
    let dir = temp_dir("flaky-none");
    let junit = dir.join("junit.xml");
    fs::write(
        &junit,
        r#"<testsuites><testsuite name="a"><testcase name="t" classname="a" time="0.1"/></testsuite></testsuites>"#,
    )
    .unwrap();
    let (stdout, summary, output) = flaky_report(&junit);
    assert_eq!(stdout, "");
    assert!(
        summary.contains("No test passed only on retry."),
        "{summary}"
    );
    assert!(output.starts_with("flaky<<"), "{output}");
    assert_eq!(output.lines().count(), 2, "{output}");

    let (stdout, summary, _) = flaky_report(&dir.join("missing.xml"));
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(stdout, "");
    assert!(summary.contains("No JUnit report"), "{summary}");
}
