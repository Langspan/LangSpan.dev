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
fn code_split_into_new_core_daemon_or_hook_modules_runs_the_dry_runs() {
    for path in [
        "crates/langspan-core/src/new_module.rs",
        "crates/langspan-core/src/briefing/mod.rs",
        "src/daemon.rs",
        "src/daemon/serve.rs",
        "src/hook.rs",
        "src/hook/redact.rs",
        "src/text.rs",
        "src/text/redact.rs",
        "src/briefing.rs",
        "src/briefing/render.rs",
        // The workflow that runs them.
        ".github/workflows/bench.yml",
    ] {
        let repo = Repo::new();
        repo.commit("README.md");
        let head = repo.commit(path);
        assert_eq!(
            repo.decide("pull_request", "", &head),
            decided("true", "run"),
            "{path}"
        );
    }
    // A name that only starts like a hot module, or another crate, stays out.
    for path in [
        "src/daemonize.rs",
        "src/hooks.rs",
        "src/texts.rs",
        "src/briefings.rs",
        "crates/langspan-graph/src/lib.rs",
    ] {
        let repo = Repo::new();
        repo.commit("README.md");
        let head = repo.commit(path);
        assert_eq!(
            repo.decide("pull_request", "", &head),
            decided("true", "skip"),
            "{path}"
        );
    }
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
fn the_full_lane_builds_once_per_platform_and_tests_the_archive_in_every_shard() {
    let text = workflow("check.yml");
    for (build, tests, runner, platform) in [
        (
            "linux",
            "linux-tests",
            "vars.LINUX_RUNNER || 'ubuntu-24.04'",
            "linux",
        ),
        (
            "macos",
            "macos-tests",
            "vars.MACOS_RUNNER || 'macos-26'",
            "macos",
        ),
    ] {
        let built = job(&text, build);
        let shards = job(&text, tests);
        // The build job archives the tests with the runner that will run
        // them, and the shards take both from this run's artifact.
        assert!(
            built.contains(&format!(
                "          name: tests-{platform}\n          path: |\n            ${{{{ runner.temp }}}}/tests.tar.zst\n            ${{{{ runner.temp }}}}/cargo-nextest\n          if-no-files-found: error\n"
            )),
            "{build} keeps no test archive"
        );
        assert!(built.contains("LANGSPAN_TEST_ARCHIVE: ${{ runner.temp }}/tests.tar.zst\n"));
        assert!(built.contains("      code: ${{ steps.changes.outputs.code }}\n"));
        for line in [
            format!("\n    needs: [{build}]\n"),
            format!(
                "\n    if: needs.{build}.outputs.code == 'true' && github.event_name != 'pull_request'\n"
            ),
            format!("\n    runs-on: ${{{{ {runner} }}}}\n"),
            "\n      fail-fast: false\n".into(),
            format!("          name: tests-{platform}\n"),
            "          LANGSPAN_CHECK_LANE: shard\n".into(),
            "          LANGSPAN_TEST_ARCHIVE: ${{ runner.temp }}/tests.tar.zst\n".into(),
            // The partition's total is the matrix's own size, so adding a
            // shard can't leave a test in none.
            "          LANGSPAN_TEST_PARTITION: time:${{ matrix.shard }}/${{ strategy.job-total }}\n"
                .into(),
            "          LANGSPAN_BENCH_DRY_RUNS: skip\n".into(),
            format!("          name: junit-{platform}-${{{{ matrix.shard }}}}\n"),
        ] {
            assert!(shards.contains(&line), "{tests} lacks {line}");
        }
        // Shards 1 to N, each once.
        let matrix = shards
            .split("\n        shard: [")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .unwrap_or_else(|| panic!("{tests} has no shard matrix"));
        let numbers: Vec<usize> = matrix.split(", ").map(|n| n.parse().unwrap()).collect();
        assert_eq!(numbers, (1..=numbers.len()).collect::<Vec<_>>(), "{matrix}");
        assert!(numbers.len() > 1, "{tests} runs one shard");
        // A shard neither restores nor saves the build cache.
        assert!(
            !shards.contains("actions/cache"),
            "{tests} uses the build cache"
        );
    }
}

/// The benchmark dry runs take 92 to 132 s each, so the required checks
/// never run them: every job of check.yml skips them, and bench.yml runs
/// them instead.
#[test]
fn check_yml_never_runs_the_benchmark_dry_runs() {
    let text = workflow("check.yml");
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("LANGSPAN_BENCH_DRY_RUNS:"))
        .collect();
    // Both build jobs and both shard jobs.
    assert_eq!(lines.len(), 4, "{lines:?}");
    for line in lines {
        assert_eq!(line.trim(), "LANGSPAN_BENCH_DRY_RUNS: skip");
    }
    assert!(
        !text.contains("outputs.dry_runs"),
        "check.yml reads dry_runs"
    );
}

/// bench.yml runs the dry runs for a pull request or push to main that
/// touches what they drive (scripts/ci-changes.sh decides), every night and
/// on demand, on Linux and macOS, and never in the merge queue.
#[test]
fn bench_yml_runs_the_dry_runs_off_the_merge_queue() {
    let text = workflow("bench.yml");
    let on = text
        .split("\non:\n")
        .nth(1)
        .and_then(|rest| rest.split("\npermissions:").next())
        .expect("bench.yml has a top-level `on:` block");
    for event in [
        "  pull_request:",
        "  push:",
        "  schedule:",
        "  workflow_dispatch:",
    ] {
        assert!(on.lines().any(|l| l == event), "bench.yml lacks {event}");
    }
    assert!(on.contains("    branches: [main]\n"), "{on}");
    assert!(!on.contains("merge_group"), "the merge queue waits on it");
    assert!(text.contains("\npermissions: {}\n"));
    let runs = job(&text, "dry-runs");
    for line in [
        "        run: bash scripts/ci-changes.sh\n",
        "      contents: read\n",
        "        platform: [linux, macos]\n",
        "github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' || steps.changes.outputs.dry_runs == 'run'",
        "cargo nextest run --locked --profile ci --test benchmark_harness -E 'test(/^dry_run_/)'",
    ] {
        assert!(runs.contains(line), "dry-runs lacks {line}");
    }
    assert!(!runs.contains("LANGSPAN_BENCH_DRY_RUNS"), "{runs}");
    assert!(
        !runs.contains("actions/cache/save"),
        "only main's check.yml saves"
    );
    let report = job(&text, "nightly-report");
    assert!(report.contains("\n    needs: [dry-runs]\n"), "{report}");
    assert!(report.contains("github.event_name == 'schedule' && failure()"));
    assert!(report.contains("      issues: write\n"));
    assert!(
        !report.contains("actions/checkout"),
        "the issue writer runs repository code"
    );
}

#[test]
fn the_required_checks_wait_for_the_build_and_every_shard_whatever_they_ended_with() {
    let text = workflow("check.yml");
    for (id, name, build, tests) in [
        ("linux-result", "Linux", "linux", "linux-tests"),
        ("macos-result", "macOS", "macos", "macos-tests"),
    ] {
        let result = job(&text, id);
        for line in [
            format!("\n    name: {name}\n"),
            format!("\n    needs: [{build}, {tests}]\n"),
            // Not `!cancelled()`: a skipped required check counts as passed.
            "\n    if: always()\n".into(),
            format!("          BUILD: ${{{{ needs.{build}.result }}}}\n"),
            format!("          TESTS: ${{{{ needs.{tests}.result }}}}\n"),
            format!("          CODE: ${{{{ needs.{build}.outputs.code }}}}\n"),
            "        run: bash scripts/ci-shard-results.sh \"$RUNNER_TEMP/junit\"".into(),
        ] {
            assert!(result.contains(&line), "{id} lacks {line}");
        }
        // Only the result jobs carry the required names.
        assert_eq!(
            text.matches(&format!("\n    name: {name}\n")).count(),
            1,
            "another job is named {name}"
        );
    }
    for id in ["flaky-report", "nightly-report"] {
        assert!(job(&text, id).contains("\n    needs: [linux-result, macos-result]\n"));
    }
}

/// Runs scripts/ci-shard-results.sh over `dir` with the given results;
/// returns (success, GITHUB_OUTPUT, stderr).
fn shard_results(dir: &Path, env: &[(&str, &str)]) -> (bool, String, String) {
    let output = dir.join("output");
    let _ = fs::remove_file(&output);
    let out = Command::new("bash")
        .arg(root().join("scripts/ci-shard-results.sh"))
        .arg(dir.join("junit"))
        .env("GITHUB_OUTPUT", &output)
        .env_remove("GITHUB_STEP_SUMMARY")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let written = fs::read_to_string(&output).unwrap_or_default();
    (
        out.status.success(),
        written,
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// The value of output `name` in a GITHUB_OUTPUT file.
fn output_value(written: &str, name: &str) -> String {
    let mut lines = written.lines();
    let delimiter = lines
        .find_map(|l| l.strip_prefix(&format!("{name}<<")))
        .unwrap_or_else(|| panic!("no {name} in {written:?}"));
    lines
        .take_while(|l| *l != delimiter)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_platform_check_passes_only_when_its_build_and_every_shard_passed() {
    // A shell script deciding from GitHub's job results, so it runs here as
    // check.yml runs it.
    let dir = temp_dir("shard-results");
    let ok = |event: &str, code: &str, build: &str, tests: &str| {
        let env = [
            ("EVENT", event),
            ("CODE", code),
            ("BUILD", build),
            ("TESTS", tests),
        ];
        shard_results(&dir, &env).0
    };
    assert!(ok("merge_group", "true", "success", "success"));
    // No shard runs on a pull request (the build job ran the fast lane) or
    // for a change that touches no code.
    assert!(ok("pull_request", "true", "success", "skipped"));
    assert!(ok("merge_group", "false", "success", "skipped"));
    for tests in ["failure", "cancelled"] {
        assert!(!ok("merge_group", "true", "success", tests), "{tests}");
        assert!(!ok("pull_request", "true", "success", tests), "{tests}");
    }
    // Shards skipped when they should have run: the tests didn't run.
    assert!(!ok("merge_group", "true", "success", "skipped"));
    assert!(!ok("push", "true", "success", "skipped"));
    for build in ["failure", "cancelled", "skipped", ""] {
        assert!(!ok("merge_group", "true", build, "skipped"), "{build}");
        assert!(!ok("pull_request", "true", build, "skipped"), "{build}");
    }

    // The names of flaky and failed tests come from every shard's report:
    // two shards, each with one of each.
    for shard in [1, 2] {
        let to = dir.join(format!("junit/junit-macos-{shard}"));
        fs::create_dir_all(&to).unwrap();
        fs::copy(
            root().join("tests/fixtures/nextest/junit-retries.xml"),
            to.join("junit.xml"),
        )
        .unwrap();
    }
    let env = [
        ("EVENT", "push"),
        ("CODE", "true"),
        ("BUILD", "success"),
        ("TESTS", "failure"),
    ];
    let (passed, written, stderr) = shard_results(&dir, &env);
    let _ = fs::remove_dir_all(&dir);
    assert!(!passed);
    assert!(
        stderr.contains("the test shards ended with 'failure'"),
        "{stderr}"
    );
    assert!(stderr.contains("flaky_sample fails_every_try"), "{stderr}");
    assert_eq!(
        output_value(&written, "failed"),
        "flaky_sample fails_every_try\nflaky_sample fails_every_try"
    );
    assert_eq!(
        output_value(&written, "flaky"),
        "flaky_sample passes_on_its_second_try\nflaky_sample passes_on_its_second_try"
    );
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
        "LANGSPAN_CHECK_LANE: ${{ github.event_name == 'pull_request' && 'fast' || 'build' }}",
        "NEXTEST_PROFILE: ${{ github.event_name == 'pull_request' && 'ci-fast' || 'ci' }}",
    ] {
        assert!(text.contains(line), "check.yml lacks {line}");
    }
    for id in ["linux", "macos"] {
        assert!(
            job(&text, id).contains(
                "\n    timeout-minutes: ${{ github.event_name == 'pull_request' && 6 || github.event_name == 'merge_group' && 15 || 30 }}\n"
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
        stderr.contains("LANGSPAN_CHECK_LANE is 'quick'; set it to fast, full, build or shard"),
        "{stderr}"
    );
}

/// Runs check.sh outside the repository with `env`; returns (exit code,
/// stderr). A refusal must come before any check, which would fail there.
fn check_sh_refusal(name: &str, env: &[(&str, &str)]) -> (Option<i32>, String) {
    let dir = temp_dir(name);
    let out = Command::new("sh")
        .arg(root().join("scripts/check.sh"))
        .current_dir(&dir)
        .env_remove("LANGSPAN_TEST_ARCHIVE")
        .env_remove("LANGSPAN_TEST_PARTITION")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn check_sh_refuses_a_build_or_shard_lane_without_its_archive_or_partition() {
    for lane in ["build", "shard"] {
        let (code, stderr) = check_sh_refusal("check-archive", &[("LANGSPAN_CHECK_LANE", lane)]);
        assert_eq!(code, Some(2), "{stderr}");
        assert!(
            stderr.contains(&format!(
                "the {lane} lane needs LANGSPAN_TEST_ARCHIVE, the nextest archive's path"
            )),
            "{stderr}"
        );
    }
    // A shard runs only the part its partition names; without one, or with
    // one nextest would read as a different scheme, it would run them all.
    for partition in ["", "1/4", "count:1/4", "hash:1/4", "time:0/4", "time:1"] {
        let (code, stderr) = check_sh_refusal(
            "check-partition",
            &[
                ("LANGSPAN_CHECK_LANE", "shard"),
                ("LANGSPAN_TEST_ARCHIVE", "/nonexistent/tests.tar.zst"),
                ("LANGSPAN_TEST_PARTITION", partition),
            ],
        );
        assert_eq!(code, Some(2), "{partition}: {stderr}");
        assert!(
            stderr.contains(&format!(
                "LANGSPAN_TEST_PARTITION is '{partition}'; the shard lane needs time:K/N"
            )),
            "{stderr}"
        );
    }
}

#[test]
fn the_shard_stage_refuses_to_run_without_its_shard_and_checks_what_nextest_selects() {
    // Outside the repository, so a stage that started would fail otherwise.
    let dir = temp_dir("foreign-home-shard");
    let out = Command::new("sh")
        .arg(root().join("scripts/test-foreign-home.sh"))
        .current_dir(&dir)
        .env("LANGSPAN_TEST_STAGE", "shard")
        .env("LANGSPAN_TEST_ARCHIVE", "/nonexistent/tests.tar.zst")
        .env_remove("LANGSPAN_TEST_SHARD")
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("the shard stage needs LANGSPAN_TEST_SHARD, the shard as K/N"),
        "{out:?}"
    );
    // The shard runs the filterset only once nextest has been seen to
    // select the count the assignment expects, and the dry runs weigh
    // nothing when they are skipped.
    let script = read("scripts/test-foreign-home.sh");
    for line in [
        "    if [ \"${LANGSPAN_BENCH_DRY_RUNS:-}\" = skip ]; then skip=--skip-dry-runs; fi\n",
        "    if [ \"$selected\" != \"$expected\" ]; then\n",
        "      exit 1\n    fi\n    set -- \"$@\" -E \"$filter\"\n",
    ] {
        assert!(script.contains(line), "test-foreign-home.sh lacks {line}");
    }
}

/// Runs scripts/ci-shards.py with `args`; returns (success, stdout, stderr).
/// It is a child process because CI runs it as one, from the shell.
fn ci_shards(args: &[&std::ffi::OsStr]) -> (bool, String, String) {
    let out = Command::new("python3")
        .args(["-I", "-B"])
        .arg(root().join("scripts/ci-shards.py"))
        .args(args)
        .output()
        .expect("python3 runs scripts/ci-shards.py");
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// `cargo nextest list --message-format json` output for `binaries` of
/// (binary id, [(test, matches)]): the fields ci-shards.py reads.
fn nextest_list(binaries: &[(&str, &[(&str, bool)])]) -> String {
    let mut suites = serde_json::Map::new();
    for (binary, tests) in binaries {
        let mut cases = serde_json::Map::new();
        for (test, matches) in *tests {
            let status = if *matches {
                serde_json::json!({"status": "matches"})
            } else {
                serde_json::json!({"status": "mismatch", "reason": "ignored"})
            };
            cases.insert(
                (*test).into(),
                serde_json::json!({"kind": "test", "ignored": !matches, "filter-match": status}),
            );
        }
        suites.insert(
            (*binary).into(),
            serde_json::json!({"binary-id": binary, "testcases": cases}),
        );
    }
    serde_json::json!({"test-count": 0, "rust-suites": suites}).to_string()
}

/// Whether one filterset `ci-shards.py assign` printed selects `test` of
/// `binary`. It reads only the three forms the script writes, joined by
/// ` | `, and `none()` for a shard given nothing, so a form it doesn't know
/// fails here.
fn selects(filter: &str, binary: &str, test: &str) -> bool {
    if filter == "none()" {
        return false;
    }
    let mut terms = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, c) in filter.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '|' if depth == 0 => {
                terms.push(filter[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    terms.push(filter[start..].trim());
    terms.iter().any(|term| {
        let is = |b: &str| b == binary;
        if let Some(rest) = term
            .strip_prefix("(binary_id(=")
            .and_then(|t| t.strip_suffix("))"))
        {
            let (b, inner) = rest.split_once(") & ").unwrap();
            if let Some(t) = inner.strip_prefix("test(=") {
                return is(b) && t == test;
            }
            let others = inner
                .strip_prefix("not (test(=")
                .and_then(|t| t.strip_suffix(')'))
                .unwrap();
            return is(b) && !others.split(") | test(=").any(|t| t == test);
        }
        let b = term
            .strip_prefix("binary_id(=")
            .and_then(|t| t.strip_suffix(')'))
            .unwrap_or_else(|| panic!("unknown filterset form {term}"));
        is(b)
    })
}

#[test]
fn measured_shards_give_every_test_to_exactly_one_shard() {
    let dir = temp_dir("ci-shards-cover");
    let times = dir.join("times.tsv");
    // langspan::b's `gone` was removed after recording, and langspan::c and
    // langspan::a's `new_test` were added: each still runs exactly once.
    fs::write(
        &times,
        "# fixture\n\
         linux\tlangspan::a\tslow_one\t60.0\t1\n\
         linux\tlangspan::a\tslow_two\t40.0\t1\n\
         linux\tlangspan::a\t*\t9.0\t3\n\
         linux\tlangspan::b\tgone\t30.0\t1\n\
         linux\tlangspan::b\t*\t20.0\t2\n\
         linux\tlangspan::benchmark_harness\tdry_run_x\t50.0\t1\n\
         linux\tlangspan::benchmark_harness\t*\t1.0\t1\n\
         linux\tlangspan\t*\t5.0\t10\n\
         macos\tlangspan::a\tslow_one\t90.0\t1\n",
    )
    .unwrap();
    let binaries: &[(&str, &[(&str, bool)])] = &[
        (
            "langspan",
            &[("m::tests::one", true), ("m::tests::two", true)],
        ),
        (
            "langspan::a",
            &[
                ("slow_one", true),
                ("slow_two", true),
                ("l1", true),
                ("l2", true),
                ("new_test", true),
                ("ignored_one", false),
            ],
        ),
        ("langspan::b", &[("b1", true), ("b2", true)]),
        (
            "langspan::benchmark_harness",
            &[("dry_run_x", true), ("plain", true)],
        ),
        ("langspan::c", &[("c1", true), ("c2", true)]),
        ("langspan::d", &[("only_ignored", false)]),
    ];
    let list = dir.join("list.json");
    fs::write(&list, nextest_list(binaries)).unwrap();
    let running: Vec<(&str, &str)> = binaries
        .iter()
        .flat_map(|(b, tests)| tests.iter().filter(|t| t.1).map(move |t| (*b, t.0)))
        .collect();
    for skip in [false, true] {
        let mut seen = vec![0; running.len()];
        let mut counted = 0;
        for k in 1..=4 {
            let shard = format!("{k}/4");
            let mut args: Vec<&std::ffi::OsStr> = vec![
                "--times".as_ref(),
                times.as_os_str(),
                "assign".as_ref(),
                "--platform".as_ref(),
                "linux".as_ref(),
                "--threads".as_ref(),
                "2".as_ref(),
                "--shard".as_ref(),
                shard.as_ref(),
            ];
            if skip {
                args.push("--skip-dry-runs".as_ref());
            }
            args.push(list.as_os_str());
            let (ok, stdout, stderr) = ci_shards(&args);
            assert!(ok, "{stderr}");
            let (count, filter) = stdout.trim_end().split_once('\n').unwrap();
            let count: usize = count.parse().unwrap();
            let mine = running
                .iter()
                .filter(|(b, t)| selects(filter, b, t))
                .count();
            // The count the shard checks nextest against is what its
            // filterset selects.
            assert_eq!(count, mine, "shard {k}: {filter}");
            counted += count;
            for (i, (b, t)) in running.iter().enumerate() {
                if selects(filter, b, t) {
                    seen[i] += 1;
                }
            }
        }
        assert_eq!(counted, running.len());
        for (i, n) in seen.iter().enumerate() {
            assert_eq!(*n, 1, "{:?} runs in {n} shards", running[i]);
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn measured_shards_are_the_same_on_every_shard_whatever_the_list_order() {
    let dir = temp_dir("ci-shards-same");
    let times = dir.join("times.tsv");
    fs::write(
        &times,
        "linux\tlangspan::a\tx\t10.0\t1\nlinux\tlangspan::a\ty\t10.0\t1\nlinux\tlangspan::b\t*\t10.0\t2\n",
    )
    .unwrap();
    let forward: &[(&str, &[(&str, bool)])] = &[
        ("langspan::a", &[("x", true), ("y", true)]),
        ("langspan::b", &[("p", true), ("q", true)]),
    ];
    let backward: &[(&str, &[(&str, bool)])] = &[
        ("langspan::b", &[("q", true), ("p", true)]),
        ("langspan::a", &[("y", true), ("x", true)]),
    ];
    let mut outputs = Vec::new();
    for (name, binaries) in [("f", forward), ("f", forward), ("b", backward)] {
        let list = dir.join(format!("{name}.json"));
        fs::write(&list, nextest_list(binaries)).unwrap();
        let shards: Vec<String> = (1..=2)
            .map(|k| {
                let shard = format!("{k}/2");
                let (ok, stdout, stderr) = ci_shards(&[
                    "--times".as_ref(),
                    times.as_os_str(),
                    "assign".as_ref(),
                    "--platform".as_ref(),
                    "linux".as_ref(),
                    "--threads".as_ref(),
                    "1".as_ref(),
                    "--shard".as_ref(),
                    shard.as_ref(),
                    list.as_os_str(),
                ]);
                assert!(ok, "{stderr}");
                stdout
            })
            .collect();
        outputs.push(shards);
    }
    // Without --threads the assignment is the platform's, not this
    // machine's: runners of one platform with different CPU counts would
    // otherwise split the tests differently, and a test would run twice or
    // not at all. A 40 s test and twenty 10 s binaries: at 4 threads the
    // other shard fills to 40 s after 16 binaries, at 3 after 12, and at 8
    // or more never, so each of those splits differently.
    let mut times_rows = String::from("linux\tlangspan::a\tslow\t40.0\t1\n");
    let mut binaries: Vec<(String, Vec<(&str, bool)>)> =
        vec![("langspan::a".into(), vec![("slow", true)])];
    for b in 0..20 {
        times_rows.push_str(&format!("linux\tlangspan::b{b}\t*\t10.0\t1\n"));
        binaries.push((format!("langspan::b{b}"), vec![("t", true)]));
    }
    fs::write(&times, times_rows).unwrap();
    let list = dir.join("threads.json");
    let borrowed: Vec<(&str, &[(&str, bool)])> = binaries
        .iter()
        .map(|(b, t)| (b.as_str(), t.as_slice()))
        .collect();
    fs::write(&list, nextest_list(&borrowed)).unwrap();
    let first = |threads: Option<&str>| {
        let mut args: Vec<&std::ffi::OsStr> = vec![
            "--times".as_ref(),
            times.as_os_str(),
            "assign".as_ref(),
            "--platform".as_ref(),
            "linux".as_ref(),
            "--shard".as_ref(),
            "1/2".as_ref(),
        ];
        if let Some(threads) = threads {
            args.push("--threads".as_ref());
            args.push(threads.as_ref());
        }
        args.push(list.as_os_str());
        let (ok, stdout, stderr) = ci_shards(&args);
        assert!(ok, "{stderr}");
        stdout
    };
    let standard = first(Some("4"));
    assert_ne!(standard, first(Some("3")));
    assert_ne!(standard, first(Some("8")));
    assert_eq!(first(None), standard);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0], outputs[2]);
    // Equal units, ties to the lower shard: the three ten-second units
    // split two and one.
    assert_eq!(
        outputs[0],
        [
            "3\n(binary_id(=langspan::a) & test(=x)) | binary_id(=langspan::b)\n",
            "1\n(binary_id(=langspan::a) & test(=y))\n",
        ]
    );
}

#[test]
fn measured_shards_refuse_a_bad_shard_or_a_name_a_filterset_cannot_hold() {
    let dir = temp_dir("ci-shards-refuse");
    let list = dir.join("list.json");
    fs::write(&list, nextest_list(&[("langspan::a", &[("x", true)])])).unwrap();
    let times = dir.join("none.tsv");
    for shard in ["0/4", "5/4", "1", "a/4"] {
        let (ok, _, stderr) = ci_shards(&[
            "--times".as_ref(),
            times.as_os_str(),
            "assign".as_ref(),
            "--platform".as_ref(),
            "linux".as_ref(),
            "--shard".as_ref(),
            shard.as_ref(),
            list.as_os_str(),
        ]);
        assert!(!ok, "{shard}");
        assert!(
            stderr.contains(&format!(
                "--shard is '{shard}'; expected K/N with 1 <= K <= N"
            )),
            "{stderr}"
        );
    }
    fs::write(&list, nextest_list(&[("langspan::a", &[("x(y)", true)])])).unwrap();
    let (ok, _, stderr) = ci_shards(&[
        "--times".as_ref(),
        times.as_os_str(),
        "assign".as_ref(),
        "--platform".as_ref(),
        "linux".as_ref(),
        "--shard".as_ref(),
        "1/4".as_ref(),
        list.as_os_str(),
    ]);
    let _ = fs::remove_dir_all(&dir);
    assert!(!ok);
    assert!(
        stderr.contains("the test name 'x(y)' has a character a nextest filterset can't hold"),
        "{stderr}"
    );
}

#[test]
fn measured_times_are_recorded_from_a_runs_junit_reports() {
    let dir = temp_dir("ci-shards-record");
    let times = dir.join("times.tsv");
    fs::write(
        &times,
        "macos\tlangspan::a\t*\t3.0\t2\nlinux\tlangspan::old\t*\t9.0\t9\n",
    )
    .unwrap();
    let junit = |name: &str, body: &str| {
        let path = dir.join(name);
        fs::write(
            &path,
            format!("<?xml version=\"1.0\"?><testsuites name=\"nextest-run\">{body}</testsuites>"),
        )
        .unwrap();
        path
    };
    let one = junit(
        "1.xml",
        "<testsuite name=\"langspan::a\"><testcase name=\"slow\" time=\"12.34\"/>\
         <testcase name=\"quick\" time=\"0.5\"/><testcase name=\"quicker\" time=\"0.3\"/></testsuite>",
    );
    let two = junit(
        "2.xml",
        "<testsuite name=\"langspan\"><testcase name=\"m::tests::t\" time=\"1.999\"/></testsuite>\
         <testsuite name=\"langspan::a\"><testcase name=\"edge\" time=\"2.0\"/></testsuite>",
    );
    // A second run's report: a test seen in both is weighed at its mean.
    let again = junit(
        "3.xml",
        "<testsuite name=\"langspan::a\"><testcase name=\"slow\" time=\"7.66\"/>\
         <testcase name=\"quick\" time=\"1.5\"/></testsuite>\
         <testsuite name=\"langspan\"><testcase name=\"m::tests::t\" time=\"2.001\"/></testsuite>",
    );
    let record = |reports: &[&std::path::Path]| {
        let mut args: Vec<&std::ffi::OsStr> = vec![
            "--times".as_ref(),
            times.as_os_str(),
            "record".as_ref(),
            "--platform".as_ref(),
            "linux".as_ref(),
        ];
        args.extend(reports.iter().map(|r| r.as_os_str()));
        let (ok, _, stderr) = ci_shards(&args);
        assert!(ok, "{stderr}");
        let text = fs::read_to_string(&times).unwrap();
        assert!(
            text.starts_with("# Measured test time per CI platform"),
            "{text}"
        );
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .map(String::from)
            .collect::<Vec<_>>()
    };
    // The platform's old rows go; the other platform's stay. A test of 2 s
    // or more has a row of its own; the rest of its binary are summed.
    assert_eq!(
        record(&[&one, &two]),
        [
            "linux\tlangspan\t*\t2.0\t1",
            "linux\tlangspan::a\t*\t0.8\t2",
            "linux\tlangspan::a\tedge\t2.0\t1",
            "linux\tlangspan::a\tslow\t12.3\t1",
            "macos\tlangspan::a\t*\t3.0\t2",
        ]
    );
    // Over two runs: slow (12.34 + 7.66) / 2, quick (0.5 + 1.5) / 2 plus
    // quicker 0.3, and m::tests::t's mean of 2.0 earns a row of its own.
    let rows = record(&[&one, &two, &again]);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        rows,
        [
            "linux\tlangspan\tm::tests::t\t2.0\t1",
            "linux\tlangspan::a\t*\t1.3\t2",
            "linux\tlangspan::a\tedge\t2.0\t1",
            "linux\tlangspan::a\tslow\t10.0\t1",
            "macos\tlangspan::a\t*\t3.0\t2",
        ]
    );
}

#[test]
fn measured_shards_balance_the_recorded_times_on_both_platforms() {
    // The threads of CI's standard runners: 4 on Linux, 3 on macOS.
    for (platform, threads) in [("linux", "4"), ("macos", "3")] {
        for skip in [false, true] {
            let mut args: Vec<&std::ffi::OsStr> = vec![
                "balance".as_ref(),
                "--platform".as_ref(),
                platform.as_ref(),
                "--threads".as_ref(),
                threads.as_ref(),
                "--shards".as_ref(),
                "4".as_ref(),
            ];
            if skip {
                args.push("--skip-dry-runs".as_ref());
            }
            let (ok, stdout, stderr) = ci_shards(&args);
            assert!(ok, "{stderr}");
            let seconds: Vec<f64> = stdout
                .lines()
                .map(|l| l.split('\t').nth(1).unwrap().parse().unwrap())
                .collect();
            assert_eq!(seconds.len(), 4, "{stdout}");
            let mean = seconds.iter().sum::<f64>() / 4.0;
            let slowest = seconds.iter().copied().fold(0.0, f64::max);
            assert!(
                slowest <= mean * 1.2,
                "{platform}, dry runs skipped {skip}: the slowest shard takes {slowest} s against a mean of {mean} s\n{stdout}"
            );
        }
    }
}

#[test]
fn pull_requests_read_the_build_cache_and_only_main_writes_it() {
    let text = workflow("check.yml");
    assert!(
        !text.contains("Swatinem/rust-cache"),
        "the build cache has one owner"
    );
    for id in ["linux", "macos"] {
        let job = job(&text, id);
        let restore = job
            .split("uses: actions/cache/restore@")
            .nth(1)
            .unwrap_or_else(|| panic!("{id} restores no build cache"));
        // Keyed on the platform, the job, the toolchain and the lockfile.
        // Every run, main's included, falls back to main's latest entry for
        // the toolchain, so main isn't left to build from nothing.
        for part in [
            "BUILD_PREFIX: rust-v1-${{ runner.os }}-${{ runner.arch }}-${{ github.job }}-${{ hashFiles('rust-toolchain.toml') }}-\n",
            "BUILD_HASH: ${{ hashFiles('Cargo.lock', 'scripts/install-ci-tools.sh') }}\n",
            "echo \"build-prefix=$BUILD_PREFIX\"\n",
            "echo \"build=$BUILD_PREFIX$BUILD_HASH\"\n",
        ] {
            assert!(job.contains(part), "{id}'s cache keys lack {part}");
        }
        for part in [
            "key: ${{ steps.keys.outputs.build }}\n",
            "restore-keys: ${{ steps.keys.outputs.build-prefix }}\n",
            "path: ${{ env.CACHE_PATHS }}",
        ] {
            assert!(restore.contains(part), "{id}'s restore lacks {part}");
        }
        assert!(
            job.contains("if: steps.changes.outputs.code == 'true' && !inputs.cold\n        uses: actions/cache/restore@"),
            "{id} restores the build cache on a cold run"
        );
        // Only main prunes, checks the size and saves, only when it found
        // nothing under the key, and whether the checks passed or failed,
        // but not after a cancelled or timed-out run.
        assert!(job.contains(
            "if: ${{ !cancelled() && github.ref == 'refs/heads/main' && steps.changes.outputs.code == 'true' && steps.cache.outputs.cache-hit != 'true' }}\n        env:\n          LIMIT_MB: ${{ env.CACHE_LIMIT_MB }}\n        run: bash scripts/ci-cache-prune.sh\n"
        ), "{id} could save a cache off main, or not after a failed test");
        let save = job
            .split("uses: actions/cache/save@")
            .nth(1)
            .unwrap_or_else(|| panic!("{id} saves no build cache"));
        assert!(
            job.contains(
                "if: ${{ !cancelled() && steps.size.outputs.save == 'true' }}\n        uses: actions/cache/save@"
            ),
            "{id} saves without the size check"
        );
        assert!(save.contains("key: ${{ steps.keys.outputs.build }}"));
    }
    // The release build uses no cache.
    let release = workflow("release.yml");
    for cache in ["actions/cache", "rust-cache", "sccache"] {
        assert!(!release.contains(cache), "release.yml uses {cache}");
    }
}

#[test]
fn cargo_audit_has_its_own_cache_and_the_build_cache_paths_stay_put() {
    let text = workflow("check.yml");
    // actions/cache versions an entry by its path list: a change here
    // matches no entry main saved, and every pull request builds cold.
    assert!(
        text.contains(
            "  CACHE_PATHS: |\n    ~/.cargo/registry/index\n    ~/.cargo/registry/cache\n    \
             ~/.cargo/git/db\n    target\n    ~/.cache/langspan-ci-tools/cargo-nextest\n  CACHE_LIMIT_MB:"
        ),
        "CACHE_PATHS changed; change it in a pull request of its own"
    );
    // Only Linux runs cargo-audit (the portable checks), so only Linux
    // builds and caches it.
    let job = job(&text, "linux");
    let restore = job
        .split("- name: Restore cargo-audit\n")
        .nth(1)
        .expect("linux doesn't restore cargo-audit");
    for part in [
        "AUDIT_PREFIX: cargo-audit-v1-${{ runner.os }}-${{ runner.arch }}-${{ github.job }}-\n",
        "AUDIT_HASH: ${{ hashFiles('scripts/install-ci-tools.sh') }}\n",
        "echo \"audit-prefix=$AUDIT_PREFIX\"\n",
        "echo \"audit=$AUDIT_PREFIX$AUDIT_HASH\"\n",
    ] {
        assert!(job.contains(part), "the cargo-audit cache keys lack {part}");
    }
    for part in [
        "path: ~/.cache/langspan-ci-tools/cargo-audit\n",
        "key: ${{ steps.keys.outputs.audit }}\n",
        "restore-keys: ${{ steps.keys.outputs.audit-prefix }}\n",
    ] {
        assert!(
            restore.contains(part),
            "the cargo-audit restore lacks {part}"
        );
    }
    assert!(
        restore.contains("- name: Install the dependency policy"),
        "linux restores cargo-audit after installing it"
    );
    let save = job
        .split("- name: Save cargo-audit\n")
        .nth(1)
        .expect("linux doesn't save cargo-audit");
    // After the install, whether the checks passed or failed.
    assert!(save.starts_with(
        "        if: ${{ !cancelled() && github.ref == 'refs/heads/main' && steps.tools.outcome == 'success' && steps.audit-cache.outputs.cache-hit != 'true' }}\n"
    ), "cargo-audit could be saved off main, or not after a failed test");
    assert!(save.contains("key: ${{ steps.keys.outputs.audit }}"));
}

#[test]
fn ci_downloads_cargo_nextest_checked_against_a_pinned_digest_for_each_platform() {
    let script = read("scripts/install-ci-tools.sh");
    assert!(
        script.contains(
            "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$nextest"
        ),
        "the installer doesn't download cargo-nextest's upstream release"
    );
    for platform in ["  Linux-x86_64)\n", "  Darwin-arm64)\n"] {
        let block = script
            .split(platform)
            .nth(1)
            .and_then(|rest| rest.split(";;").next())
            .unwrap_or_else(|| panic!("the installer has no {platform:?} case"));
        let sha = block
            .split("nextest_sha=")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .unwrap_or_else(|| panic!("{platform:?} pins no nextest_sha"));
        assert!(
            sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "{platform:?} nextest_sha {sha:?} isn't a SHA-256"
        );
    }
    // A digest check before anything is unpacked, as for cargo-deny.
    assert!(script.contains("\"$nextest_sha\""));
    assert!(
        !script.contains("build cargo-nextest"),
        "the installer still compiles cargo-nextest"
    );
}

/// The steps of a job, each from its `- ` line to the next.
fn steps(job: &str) -> Vec<String> {
    job.split("\n      - ")
        .skip(1)
        .map(|step| format!("- {step}"))
        .collect()
}

#[test]
fn a_cold_run_on_main_saves_under_keys_the_skipped_restores_did_not_name() {
    // A cold manual run skips both restores, and a skipped step has no
    // outputs, so a save keyed on a restore's `cache-primary-key` gets an
    // empty key and the cache action saves nothing. The keys come from a
    // step of their own that a cold run still runs.
    let text = workflow("check.yml");
    assert!(
        !text.contains("cache-primary-key"),
        "a save reads a key from a restore that a cold run skips"
    );
    for (id, kinds) in [
        ("linux", &["build", "audit"][..]),
        ("macos", &["build"][..]),
    ] {
        let job = job(&text, id);
        let steps = steps(&job);
        let keys = steps
            .iter()
            .position(|step| step.contains("\n        id: keys\n"))
            .unwrap_or_else(|| panic!("{id} names no cache keys"));
        assert!(
            steps[keys].contains("if: steps.changes.outputs.code == 'true'\n"),
            "{id}: {}",
            steps[keys]
        );
        assert!(
            !steps[keys].contains("inputs.cold"),
            "{id}: {}",
            steps[keys]
        );
        for kind in kinds {
            let key = format!("key: ${{{{ steps.keys.outputs.{kind} }}}}");
            let prefix = format!("restore-keys: ${{{{ steps.keys.outputs.{kind}-prefix }}}}");
            let restore = steps
                .iter()
                .position(|step| step.contains("actions/cache/restore@") && step.contains(&key))
                .unwrap_or_else(|| panic!("{id} restores no {kind} cache under the named key"));
            assert!(steps[restore].contains(&prefix), "{id}: {}", steps[restore]);
            assert!(keys < restore, "{id} names the keys after restoring");
            assert!(
                steps
                    .iter()
                    .any(|step| step.contains("actions/cache/save@") && step.contains(&key)),
                "{id} saves no {kind} cache under the named key"
            );
        }
    }
}

#[test]
fn macos_runs_lint_build_and_tests_and_leaves_the_portable_checks_to_linux() {
    let text = workflow("check.yml");
    let macos = job(&text, "macos");
    assert!(
        macos.contains(
            "bash scripts/install-ci-tools.sh \"$HOME/.cache/langspan-ci-tools\" cargo-nextest\n"
        ),
        "macOS installs more than the test runner"
    );
    assert!(!macos.contains("cargo-audit"), "{macos}");
    assert!(macos.contains(
        "\n          LANGSPAN_CHECK_PORTABLE: \"false\"\n        run: sh scripts/check.sh\n"
    ));
    assert!(!job(&text, "linux").contains("LANGSPAN_CHECK_PORTABLE"));
    // check.sh skips exactly the platform-independent checks: format,
    // dependency policy, advisories, unused dependencies and the docs.
    let check = read("scripts/check.sh");
    let portable: Vec<&str> = check
        .split("if [ \"$portable\" = true ]; then\n")
        .skip(1)
        .map(|block| &block[..block.find("\nfi\n").expect("the block ends")])
        .collect();
    let skipped = portable.join("\n");
    for command in [
        "cargo fmt --all -- --check",
        "cargo deny --locked check",
        "cargo audit --deny warnings",
        "cargo machete",
        "cargo doc --workspace",
    ] {
        assert!(skipped.contains(command), "macOS still runs {command}");
    }
    for command in [
        "cargo clippy",
        "test-foreign-home.sh",
        "demo-two-developers.sh",
    ] {
        assert!(!skipped.contains(command), "macOS skips {command}");
    }
}

#[test]
fn check_sh_refuses_an_unknown_portable_setting_before_it_runs_anything() {
    let dir = temp_dir("check-portable");
    let out = Command::new("sh")
        .arg(root().join("scripts/check.sh"))
        .current_dir(&dir)
        .env("LANGSPAN_CHECK_PORTABLE", "no")
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LANGSPAN_CHECK_PORTABLE is 'no'; set it to true or false"),
        "{stderr}"
    );
}

#[test]
fn runs_are_grouped_by_event_and_only_the_nightly_run_builds_the_release() {
    let text = workflow("check.yml");
    // A push to main and the merge-queue run of the same commit are in
    // different groups, so neither waits for or cancels the other.
    assert!(text.contains(
        "\nconcurrency:\n  group: check-${{ github.event_name }}-${{ github.event.pull_request.number || github.sha }}\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n"
    ));
    assert!(text.contains("\n  LANGSPAN_CHECK_RELEASE: ${{ github.event_name == 'schedule' }}\n"));
    // The release workflow builds its own binary, with no cache.
    assert!(workflow("release.yml").contains("cargo build"));
}

/// Runs scripts/ci-cache-prune.sh in `dir`; returns (GITHUB_OUTPUT, stdout).
fn cache_prune(dir: &Path) -> (String, String) {
    let output = dir.join("output");
    let out = Command::new("bash")
        .arg(root().join("scripts/ci-cache-prune.sh"))
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .env("GITHUB_OUTPUT", &output)
        .output()
        .unwrap();
    assert!(out.status.success(), "ci-cache-prune.sh failed: {out:?}");
    let written = fs::read_to_string(&output).unwrap_or_default();
    (written, String::from_utf8(out.stdout).unwrap())
}

#[test]
fn main_saves_no_build_cache_from_a_run_that_built_no_tests() {
    // A run whose format check or advisory check failed before the test
    // build: a restored or empty target directory and no langspan binary.
    let dir = temp_dir("cache-prune");
    fs::create_dir_all(dir.join("target/debug/deps")).unwrap();
    let (output, stdout) = cache_prune(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(output, "save=false\n", "{stdout}");
    assert!(stdout.contains("this run built no tests"), "{stdout}");
}

/// Runs scripts/ci-cache-size.sh over `paths` with `limit_mb`, and returns
/// (GITHUB_OUTPUT, stdout).
fn cache_size(paths: &str, limit_mb: &str) -> (String, String) {
    let dir = temp_dir("cache-size");
    let output = dir.join("output");
    let out = Command::new("bash")
        .arg(root().join("scripts/ci-cache-size.sh"))
        .env("CACHE_PATHS", paths)
        .env("LIMIT_MB", limit_mb)
        .env("GITHUB_OUTPUT", &output)
        .output()
        .unwrap();
    let written = fs::read_to_string(&output).unwrap_or_default();
    let _ = fs::remove_dir_all(&dir);
    assert!(out.status.success(), "ci-cache-size.sh failed: {out:?}");
    (written, String::from_utf8(out.stdout).unwrap())
}

#[test]
fn the_build_cache_is_saved_only_within_its_size_limit() {
    let dir = temp_dir("cache-tree");
    fs::create_dir_all(dir.join("target/debug")).unwrap();
    // 3 MiB, in a path the cache would hold, beside one that doesn't exist.
    fs::write(dir.join("target/debug/big"), vec![7u8; 3 * 1024 * 1024]).unwrap();
    let paths = format!(
        "{}\n{}\n",
        dir.join("target").display(),
        dir.join("missing").display()
    );
    let (output, stdout) = cache_size(&paths, "2");
    assert_eq!(output, "save=false\n", "{stdout}");
    assert!(stdout.contains("over the 2 MB limit"), "{stdout}");
    let (output, stdout) = cache_size(&paths, "64");
    assert_eq!(output, "save=true\n", "{stdout}");
    let _ = fs::remove_dir_all(&dir);
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

/// Runs scripts/ci-failed-tests.sh on `junit`; returns (stdout, GITHUB_OUTPUT).
fn failed_tests(junit: &Path) -> (String, String) {
    let dir = temp_dir("failed-tests");
    let output = dir.join("output");
    let out = Command::new("bash")
        .arg(root().join("scripts/ci-failed-tests.sh"))
        .arg(junit)
        .env("GITHUB_OUTPUT", &output)
        .output()
        .unwrap();
    let written = fs::read_to_string(&output).unwrap_or_default();
    let _ = fs::remove_dir_all(&dir);
    assert!(out.status.success(), "ci-failed-tests.sh failed: {out:?}");
    (String::from_utf8(out.stdout).unwrap(), written)
}

#[test]
fn the_macos_failure_report_names_the_tests_that_failed_every_try_and_no_other() {
    let (stdout, output) = failed_tests(&root().join("tests/fixtures/nextest/junit-retries.xml"));
    assert_eq!(stdout, "flaky_sample fails_every_try\n");
    let mut lines = output.lines();
    let delimiter = lines
        .next()
        .and_then(|l| l.strip_prefix("failed<<"))
        .unwrap_or_else(|| panic!("no failed output in {output:?}"));
    assert_eq!(lines.next(), Some("flaky_sample fails_every_try"));
    assert_eq!(lines.next(), Some(delimiter));

    // No report, as when the job stopped before the tests ended: no name.
    let (stdout, output) = failed_tests(Path::new("/nonexistent/junit.xml"));
    assert_eq!(stdout, "");
    assert_eq!(output.lines().count(), 2, "{output}");

    // Beyond 50 failures the rest are counted, not listed.
    let dir = temp_dir("failed-many");
    let cases: String = (0..53)
        .map(|i| format!(r#"<testcase name="t{i}" classname="b"><failure/></testcase>"#))
        .collect();
    let junit = dir.join("junit.xml");
    fs::write(
        &junit,
        format!("<testsuites><testsuite name=\"b\">{cases}</testsuite></testsuites>"),
    )
    .unwrap();
    let (stdout, _) = failed_tests(&junit);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(stdout.lines().count(), 51, "{stdout}");
    assert!(stdout.ends_with("and 3 more; the run's JUnit report lists them all\n"));
}

#[test]
fn a_red_or_green_main_runs_the_main_red_report_for_each_platform_with_issues_write_only() {
    let text = workflow("check.yml");
    let report = job(&text, "main-red");
    for line in [
        "\n    needs: [linux-result, macos-result]\n",
        // A push to main, or a manual run that asks for a dry run.
        "\n    if: always() && ((github.event_name == 'push' && github.ref == 'refs/heads/main') || (github.event_name == 'workflow_dispatch' && (inputs.main_red_dry_run == 'Linux' || inputs.main_red_dry_run == 'macOS')))\n",
        "\n    permissions:\n      issues: write\n    concurrency:\n",
        "\n      group: main-red-${{ github.event_name }}-${{ matrix.platform }}\n      cancel-in-progress: false\n",
        "\n        platform: [Linux, macOS]\n",
        "\n          persist-credentials: false\n",
        // Each platform reports its own result job; a dry run reports a
        // failure of the run it names and writes nothing.
        "\n          RESULT: ${{ github.event_name == 'workflow_dispatch' && 'failure' || (matrix.platform == 'Linux' && needs.linux-result.result || needs.macos-result.result) }}\n",
        "\n          RUN_ID: ${{ inputs.main_red_run || github.run_id }}\n",
        "\n          DRY_RUN: ${{ github.event_name == 'workflow_dispatch' }}\n",
        "\n        run: python3 -I -B scripts/ci-main-red.py",
    ] {
        assert!(report.contains(line), "main-red lacks {line}");
    }
    // Only the platform a dry run names runs the script.
    assert_eq!(
        report
            .matches(
                "if: github.event_name == 'push' || inputs.main_red_dry_run == matrix.platform"
            )
            .count(),
        2,
        "{report}"
    );
    // The token can write issues and nothing else.
    let permissions = report.split("    permissions:\n").nth(1).unwrap();
    let scopes: Vec<_> = permissions
        .lines()
        .take_while(|l| l.starts_with("      "))
        .collect();
    assert_eq!(scopes, ["      issues: write"], "{report}");
    // The dry-run inputs default to no dry run, so the release workflow's
    // `gh workflow run check.yml` starts none.
    let on = text.split("\npermissions:").next().unwrap();
    assert!(on.contains("      main_red_dry_run:\n        description: "));
    assert!(on.contains(
        "        type: choice\n        options: [\"off\", Linux, macOS]\n        default: \"off\"\n"
    ));
    assert!(on.contains("      main_red_run:\n        description: "));
    // The old macOS-only report is gone; no other job gained issues: write.
    assert!(!text.contains("\n  macos-report:\n"));
    let writers: Vec<_> = ["flaky-report", "nightly-report", "main-red"]
        .iter()
        .filter(|id| job(&text, id).contains("issues: write"))
        .collect();
    assert_eq!(writers.len(), 3);
    assert_eq!(text.matches("issues: write").count(), 3, "{text}");
}

/// What one run of scripts/ci-main-red.py did against the gh stub.
struct MainRed {
    ok: bool,
    stdout: String,
    stderr: String,
    /// Every gh call, one per line.
    calls: String,
    /// What the issue writes read on stdin.
    body: String,
    summary: String,
    /// The fixtures whose flood the stub wrote to the end.
    finished: String,
}

impl MainRed {
    fn writes(&self) -> Vec<&str> {
        self.calls
            .lines()
            .filter(|c| {
                [
                    "issue create",
                    "issue comment",
                    "issue close",
                    "label create",
                ]
                .iter()
                .any(|w| c.starts_with(w))
            })
            .collect()
    }
}

/// Runs scripts/ci-main-red.py with tests/fixtures/ci-main-red/gh first on
/// PATH, for run 200 of repository o/r. `files` go into the stub's override
/// directory (the open issues are `issue-list.json`). A process test: what
/// it verifies is the script driving a child gh and what it hands to it.
fn main_red(platform: &str, result: &str, dry_run: bool, files: &[(&str, &str)]) -> MainRed {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("main-red");
    let bin = dir.join("bin");
    let over = dir.join("override");
    fs::create_dir_all(&bin).unwrap();
    for (name, text) in files {
        let path = over.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let fixtures = root().join("tests/fixtures/ci-main-red");
    fs::copy(fixtures.join("gh"), bin.join("gh")).unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let log = dir.join("calls");
    let summary = dir.join("summary.md");
    let out = Command::new("python3")
        .args(["-I", "-B"])
        .arg(root().join("scripts/ci-main-red.py"))
        .env("PATH", path)
        .env("GH_STUB_FIXTURES", &fixtures)
        .env("GH_STUB_OVERRIDE", &over)
        .env("GH_STUB_LOG", &log)
        .env("GITHUB_STEP_SUMMARY", &summary)
        .env("PLATFORM", platform)
        .env("RESULT", result)
        .env("RUN_ID", "200")
        .env("REPO", "o/r")
        .env("SERVER_URL", "https://github.com")
        .env("DRY_RUN", if dry_run { "true" } else { "false" })
        .env_remove("GH_TOKEN")
        .output()
        .expect("python3 runs scripts/ci-main-red.py");
    let read_or_empty = |p: &Path| fs::read_to_string(p).unwrap_or_default();
    let run = MainRed {
        ok: out.status.success(),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
        calls: read_or_empty(&log),
        body: read_or_empty(&dir.join("calls.body")),
        summary: read_or_empty(&summary),
        finished: read_or_empty(&dir.join("calls.finished")),
    };
    let _ = fs::remove_dir_all(&dir);
    run
}

const MACOS_TITLE: &str = "main-red: macOS checks failing on main";
const LINUX_TITLE: &str = "main-red: Linux checks failing on main";

fn open_issues(issues: &[(u32, &str)]) -> String {
    let rows: Vec<_> = issues
        .iter()
        .map(|(n, t)| format!(r#"{{"number": {n}, "title": "{t}"}}"#))
        .collect();
    format!("[{}]", rows.join(", "))
}

#[test]
fn a_red_platform_with_no_open_issue_opens_one_labelled_main_red_with_the_whole_report() {
    let run = main_red("macOS", "failure", false, &[]);
    assert!(run.ok, "{}", run.stderr);
    let writes = run.writes();
    assert_eq!(writes.len(), 2, "{}", run.calls);
    assert!(
        writes[0].starts_with("label create main-red --repo o/r "),
        "{writes:?}"
    );
    assert_eq!(
        writes[1],
        format!("issue create --repo o/r --title {MACOS_TITLE} --label main-red --body-file -")
    );
    // The last green run is the newest earlier push run on main whose
    // `macOS` check passed: 198, since 199's macOS failed and 201 is later
    // (GitHub filters by `created`; the stub doesn't, so the script must too).
    assert!(run.calls.contains(
        "api repos/o/r/actions/workflows/check.yml/runs?branch=main&event=push&status=completed&per_page=20&created=%3C2026-11-27T10:00:00Z\n"
    ));
    assert!(
        run.calls.contains("--pattern junit-macos-*"),
        "{}",
        run.calls
    );
    let body = &run.body;
    for part in [
        // The commit and the run.
        "[`c0ffee0`](https://github.com/o/r/commit/c0ffee0000000000000000000000000000000003)",
        "https://github.com/o/r/actions/runs/200",
        // The failing jobs, the aggregate check last, not a lookalike.
        "- [macOS tests 2](https://github.com/o/r/actions/runs/200/job/2): failure\n- [macOS](https://github.com/o/r/actions/runs/200/job/3): failure\n",
        // The failed tests from the JUnit reports.
        "- `cli_golden every_option_against_every_command`\n",
        // The first error lines, without the log's timestamps.
        "\n~~~~text\n--- macOS tests 2\nFAIL [   2.001s] cli_golden every_option_against_every_command\n",
        "thread 'every_option_against_every_command' panicked at tests/cli_golden.rs:40:5:\n",
        // The merged pull requests since the last green run.
        "[run 198](https://github.com/o/r/actions/runs/198)",
        "- #501 `feat: a thing (#501)`\n",
        "- #502 `fix: ping @someone about x (#298) (#502)`\n",
    ] {
        assert!(body.contains(part), "the body lacks {part:?}:\n{body}");
    }
    for absent in [
        "macOSX lookalike",
        "Linux build",
        "help_is_pinned",
        "The body.",
    ] {
        assert!(!body.contains(absent), "the body has {absent:?}:\n{body}");
    }
    assert!(body.contains("optional in the merge queue"), "{body}");
}

#[test]
fn the_error_lines_are_capped_fenced_and_cleaned_as_log_data() {
    let mut log =
        String::from("2026-11-27T10:01:00.0000000Z \u{1b}[31merror\u{1b}[0m: in colour\n");
    log.push_str(
        &fs::read_to_string(
            root().join("tests/fixtures/ci-main-red/api/repos_o_r_actions_jobs_2_logs"),
        )
        .unwrap(),
    );
    let run = main_red(
        "macOS",
        "failure",
        false,
        &[("api/repos_o_r_actions_jobs_2_logs", &log)],
    );
    assert!(run.ok, "{}", run.stderr);
    let block = run
        .body
        .split("\n~~~~text\n")
        .nth(1)
        .and_then(|rest| rest.split("\n~~~~\n").next())
        .unwrap_or_else(|| panic!("no fenced log block:\n{}", run.body));
    let lines: Vec<_> = block.lines().filter(|l| !l.starts_with("--- ")).collect();
    assert_eq!(lines.len(), 15, "{block}");
    assert_eq!(lines[0], "error: in colour");
    assert!(lines.iter().all(|l| l.chars().count() <= 300), "{block}");
    assert!(lines.iter().any(|l| l.starts_with("error: xxx")), "{block}");
    // A fence in the log can't close the block.
    assert!(!block.contains("~~~"), "{block}");
    assert!(block.contains("repeated failure 9\n") || block.ends_with("repeated failure 9"));
    assert!(!block.contains("repeated failure 10"), "{block}");
    assert!(run.body.contains("More error lines are in the job logs."));
}

#[test]
fn a_red_platform_with_an_open_issue_comments_on_it() {
    let issues = open_issues(&[
        (8, "main-red: macOS checks failing on main, again"),
        (7, MACOS_TITLE),
    ]);
    let run = main_red("macOS", "cancelled", false, &[("issue-list.json", &issues)]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.writes(), ["issue comment 7 --repo o/r --body-file -"]);
    assert!(run.body.contains("ended with cancelled"), "{}", run.body);
}

#[test]
fn a_green_platform_closes_its_open_issue_with_the_green_runs_link_and_no_other() {
    let issues = open_issues(&[(7, MACOS_TITLE), (9, LINUX_TITLE)]);
    let run = main_red("Linux", "success", false, &[("issue-list.json", &issues)]);
    assert!(run.ok, "{}", run.stderr);
    let writes = run.writes();
    assert_eq!(writes.len(), 1, "{}", run.calls);
    assert!(
        writes[0].starts_with("issue close 9 --repo o/r --comment "),
        "{writes:?}"
    );
    assert!(writes[0].contains("https://github.com/o/r/actions/runs/200"));
    assert!(writes[0].contains("c0ffee0000000000000000000000000000000003"));

    // Green with no open issue for the platform: no write at all.
    let issues = open_issues(&[(7, MACOS_TITLE)]);
    let run = main_red("Linux", "success", false, &[("issue-list.json", &issues)]);
    assert!(run.ok, "{}", run.stderr);
    assert!(run.writes().is_empty(), "{}", run.calls);

    // A run whose checks were skipped decides nothing.
    let run = main_red("macOS", "skipped", false, &[("issue-list.json", &issues)]);
    assert!(run.ok, "{}", run.stderr);
    assert!(run.writes().is_empty(), "{}", run.calls);
}

#[test]
fn a_dry_run_prints_the_issue_and_writes_nothing() {
    let run = main_red("macOS", "failure", true, &[]);
    assert!(run.ok, "{}", run.stderr);
    assert!(run.writes().is_empty(), "{}", run.calls);
    assert!(
        run.stdout.starts_with(&format!(
            "Dry run: would open \"{MACOS_TITLE}\" labelled main-red with this body:\n"
        )),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("- `cli_golden every_option_against_every_command`")
    );
    assert!(run.summary.contains("would open"), "{}", run.summary);
    assert!(
        run.summary
            .contains("cli_golden every_option_against_every_command")
    );
}

#[test]
fn a_report_says_what_it_could_not_find_rather_than_failing() {
    // No green Linux run among those listed, no Linux JUnit report and no
    // readable Linux log: the issue still opens, saying so for each.
    let runs = r#"{"workflow_runs": [{"id": 200, "head_sha": "c0ffee0000000000000000000000000000000003", "html_url": "https://github.com/o/r/actions/runs/200", "created_at": "2026-11-27T10:00:00Z"}]}"#;
    let run = main_red(
        "Linux",
        "failure",
        false,
        &[("api/repos_o_r_actions_workflows_check_yml_runs", runs)],
    );
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.writes().len(), 2, "{}", run.calls);
    for part in [
        "- [Linux build](https://github.com/o/r/actions/runs/200/job/4): failure\n- [Linux](https://github.com/o/r/actions/runs/200/job/5): failure\n",
        "No failed test was recorded",
        "Could not read the log of Linux build",
        "No green Linux run on main among the last 20",
    ] {
        assert!(
            run.body.contains(part),
            "the body lacks {part:?}:\n{}",
            run.body
        );
    }
    assert!(
        !run.body.contains("optional in the merge queue"),
        "{}",
        run.body
    );

    // A download that fails for another reason says so, rather than that
    // no test failed.
    let run = main_red(
        "macOS",
        "failure",
        true,
        &[(
            "download-error",
            "HTTP 403: Resource not accessible by integration\n",
        )],
    );
    assert!(run.ok, "{}", run.stderr);
    assert!(
        run.stdout.contains(
            "Could not download the JUnit reports: gh run download exited 1: HTTP 403: Resource not accessible by integration"
        ),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("No failed test was recorded"));

    // A run id that isn't a number is refused before any call.
    let out = Command::new("python3")
        .args(["-I", "-B"])
        .arg(root().join("scripts/ci-main-red.py"))
        .env("PLATFORM", "macOS")
        .env("RESULT", "failure")
        .env("RUN_ID", "200; rm -rf /")
        .env("REPO", "o/r")
        .env("SERVER_URL", "https://github.com")
        .env("DRY_RUN", "true")
        .env("GH_STUB_LOG", "/nonexistent/calls")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("RUN_ID"), "{stderr}");
}

/// 64 MiB: twice the log cap and four times the cap on other output.
const FLOOD: &str = "67108864";

#[test]
fn a_log_over_its_cap_is_read_to_the_cap_and_its_download_stopped() {
    // The first error lines come from the start of the log; the 64 MiB
    // after them are never read, so the stub never writes them all.
    let run = main_red(
        "macOS",
        "failure",
        false,
        &[("api/repos_o_r_actions_jobs_2_logs.flood", FLOOD)],
    );
    assert!(run.ok, "{}", run.stderr);
    assert!(
        run.body
            .contains("\n~~~~text\n--- macOS tests 2\nFAIL [   2.001s] cli_golden"),
        "{}",
        run.body
    );
    assert!(!run.body.contains("Could not read the log"), "{}", run.body);
    assert_eq!(run.finished, "", "the log download ran to its end");
}

#[test]
fn other_gh_output_over_its_cap_is_refused_and_its_call_stopped() {
    // A jobs list too long to be the jobs list is not buffered whole: the
    // call stops at the cap and the report says so.
    let run = main_red(
        "macOS",
        "failure",
        false,
        &[("api/repos_o_r_actions_runs_200_jobs.flood", FLOOD)],
    );
    assert!(run.ok, "{}", run.stderr);
    assert!(
        run.body.contains(
            "Could not list the jobs: gh api printed more than 16777216 bytes; stopped it"
        ),
        "{}",
        run.body
    );
    assert_eq!(run.finished, "", "the jobs download ran to its end");
}

#[test]
fn a_flood_on_stderr_is_drained_and_its_first_line_reported() {
    // Reading stdout to its end while stderr fills its pipe would deadlock;
    // stderr is drained alongside, capped, and its first line reported.
    let run = main_red(
        "macOS",
        "failure",
        false,
        &[("api/repos_o_r_actions_jobs_2_logs.flood-stderr", FLOOD)],
    );
    assert!(run.ok, "{}", run.stderr);
    assert!(
        run.body
            .contains("Could not read the log of macOS tests 2: noise line"),
        "{}",
        run.body
    );
}

/// Runs scripts/advisory-ignores.py over `dir` with `args`; returns
/// (success, stdout, stderr).
fn advisory_ignores(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new("python3")
        .args(["-I", "-B"])
        .arg(root().join("scripts/advisory-ignores.py"))
        .args(args)
        .arg(dir)
        .output()
        .expect("python3 runs scripts/advisory-ignores.py");
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// A new directory holding a deny.toml whose advisory ignores are `entries`.
fn deny_toml(entries: &str) -> PathBuf {
    let dir = temp_dir("advisories");
    let text = format!(
        "[advisories]\nyanked = \"deny\"\nignore = [\n{entries}]\n\n[licenses]\nallow = []\n"
    );
    fs::write(dir.join("deny.toml"), text).unwrap();
    dir
}

#[test]
fn cargo_audit_takes_its_ignores_from_deny_toml() {
    let (ok, stdout, stderr) = advisory_ignores(root(), &["--audit-flags"]);
    assert!(ok, "{stderr}");
    let deny = read("deny.toml");
    let mut flags = 0;
    for line in stdout.lines() {
        let id = line
            .strip_prefix("--ignore ")
            .unwrap_or_else(|| panic!("not a cargo-audit flag: {line}"));
        assert!(
            deny.contains(&format!("id = \"{id}\"")),
            "{id} is not in deny.toml"
        );
        flags += 1;
    }
    assert_eq!(flags, deny.matches("{ id = \"RUSTSEC-").count(), "{stdout}");
    let check = read("scripts/check.sh");
    for line in [
        "cargo deny --locked check\n",
        "audit_flags=$(python3 -I -B scripts/advisory-ignores.py --audit-flags)\n",
        "cargo audit --deny warnings $audit_flags\n",
    ] {
        assert!(check.contains(line), "check.sh lacks {line}");
    }
    assert!(
        !root().join(".cargo/audit.toml").exists(),
        "cargo-audit's ignores come from deny.toml alone"
    );
}

/// The advisory dry run (issue #312): a manual run of check.yml with
/// `advisory_dry_run` audits a lockfile holding a known advisory after the
/// real one, so the `Linux` job fails on it and nightly-report files under
/// a title of its own. Read from the files: what it verifies is the
/// workflow's and check.sh's text; the dispatched run is the evidence
/// (docs/evidence/ci/advisories.md).
#[test]
fn an_advisory_dry_run_fails_linux_on_a_known_advisory_and_reports_apart() {
    let text = workflow("check.yml");
    assert!(
        text.contains(concat!(
            "      advisory_dry_run:\n",
            "        description: Audit a lockfile with a known advisory, so Linux fails and nightly-report runs\n",
            "        type: boolean\n",
            "        default: false\n",
        )),
        "check.yml lacks the advisory_dry_run input"
    );
    // Only the Linux build's check step sets it, and only from the input.
    let setting = "LANGSPAN_ADVISORY_DRY_RUN: ${{ inputs.advisory_dry_run && '1' || '' }}\n";
    assert_eq!(text.matches("LANGSPAN_ADVISORY_DRY_RUN").count(), 1);
    assert!(job(&text, "linux").contains(&format!("          {setting}")));

    let check = read("scripts/check.sh");
    let real = "  cargo audit --deny warnings $audit_flags\n";
    let dry = concat!(
        "  if [ \"${LANGSPAN_ADVISORY_DRY_RUN:-}\" = 1 ]; then\n",
        "    # shellcheck disable=SC2086 # each flag and id is one word\n",
        "    cargo audit --deny warnings $audit_flags --file tests/fixtures/advisory-dry-run/known-advisory.lock\n",
        "  fi\n",
    );
    let (at_real, at_dry) = (check.find(real), check.find(dry));
    assert!(
        matches!((at_real, at_dry), (Some(r), Some(d)) if r < d),
        "check.sh audits the fixture after the real lockfile"
    );

    // Not named Cargo.lock, so no tool reads it as this repository's.
    let fixture = read("tests/fixtures/advisory-dry-run/known-advisory.lock");
    assert!(
        fixture.contains("name = \"smallvec\"\nversion = \"1.6.0\"\n"),
        "{fixture}"
    );
    assert!(!read("deny.toml").contains("RUSTSEC-2021-0003"));

    let report = job(&text, "nightly-report");
    for line in [
        "    if: (github.event_name == 'schedule' || inputs.advisory_dry_run) && failure()\n",
        "          TITLE: Nightly Rust checks on main failed\n",
        "          DRY_RUN: ${{ inputs.advisory_dry_run == true }}\n",
        "          DRY_RUN_TITLE: Advisory dry run of the nightly report\n",
        "            TITLE=$DRY_RUN_TITLE\n",
        "export TITLE\n",
    ] {
        assert!(report.contains(line), "nightly-report lacks {line}");
    }
    assert!(!report.contains("actions/checkout"));
}

#[test]
fn an_advisory_ignore_needs_a_reason_and_a_review_date_still_ahead() {
    let good = concat!(
        "  { id = \"RUSTSEC-2026-0001\", reason = \"No fix upstream. Review by 2999-01-31.\" },\n",
        "  # A comment between entries.\n",
        "  { id = \"RUSTSEC-2026-0002\", reason = \"Only in a build tool. Review by 2999-06-30.\" },\n",
    );
    let dir = deny_toml(good);
    let (ok, stdout, stderr) = advisory_ignores(&dir, &["--audit-flags"]);
    assert!(ok, "{stderr}");
    assert_eq!(
        stdout,
        "--ignore RUSTSEC-2026-0001\n--ignore RUSTSEC-2026-0002\n"
    );
    let (ok, stdout, stderr) = advisory_ignores(&dir, &[]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("2 advisory ignores in deny.toml"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(&dir);

    for (entry, expected) in [
        (
            "  { id = \"RUSTSEC-2026-0003\", reason = \"No fix upstream.\" },",
            "RUSTSEC-2026-0003 has no review date",
        ),
        (
            "  { id = \"RUSTSEC-2026-0004\", reason = \"Review by 2001-02-03.\" },",
            "RUSTSEC-2026-0004 was due for review on 2001-02-03",
        ),
        (
            "  { id = \"RUSTSEC-2026-0005\", reason = \"\" },",
            "RUSTSEC-2026-0005 has no reason",
        ),
        (
            "  { id = \"RUSTSEC-2026-0006\" },",
            "can't read this ignore",
        ),
        ("  \"RUSTSEC-2026-0008\",", "can't read this ignore"),
        (
            concat!(
                "  { id = \"RUSTSEC-2026-0007\", reason = \"Review by 2999-01-01.\" },\n",
                "  { id = \"RUSTSEC-2026-0007\", reason = \"Review by 2999-01-01.\" },",
            ),
            "RUSTSEC-2026-0007 is listed twice",
        ),
    ] {
        let dir = deny_toml(&format!("{entry}\n"));
        for args in [&[][..], &["--audit-flags"][..]] {
            let (ok, stdout, stderr) = advisory_ignores(&dir, args);
            assert!(!ok, "{entry} passed: {stdout}");
            assert_eq!(stdout, "", "a failed check prints no flags");
            assert!(stderr.contains(expected), "{entry}: {stderr}");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn cargo_audit_may_not_keep_an_ignore_list_of_its_own() {
    let dir = deny_toml("  { id = \"RUSTSEC-2026-0001\", reason = \"Review by 2999-01-31.\" },\n");
    fs::create_dir_all(dir.join(".cargo")).unwrap();
    fs::write(
        dir.join(".cargo/audit.toml"),
        "[advisories]\nignore = [\"RUSTSEC-2026-0009\"]\n",
    )
    .unwrap();
    let (ok, _, stderr) = advisory_ignores(&dir, &[]);
    let _ = fs::remove_dir_all(&dir);
    assert!(!ok);
    assert!(
        stderr.contains(".cargo/audit.toml lists advisories to ignore"),
        "{stderr}"
    );
}

#[test]
fn check_sh_denies_doc_warnings_and_ends_each_lane_with_no_process_left() {
    let check = read("scripts/check.sh");
    assert!(
        check.contains("RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked\n"),
        "{check}"
    );
    // Every lane ends by looking for leftovers: the fast lane and a shard
    // before they exit, the full and build lanes on the last line.
    assert!(check.contains("  leftovers\n  echo \"check.sh: fast lane done"));
    assert!(check.contains(
        "  LANGSPAN_TEST_SHARD=${partition#time:} sh scripts/test-foreign-home.sh\n  leftovers\n  exit 0\n"
    ));
    assert!(check.trim_end().ends_with("\nleftovers"), "{check}");
}

/// Runs check.sh's `leftovers` function in `dir`, whose target directory
/// is `dir/target`; returns (success, stderr).
fn leftovers(dir: &Path) -> (bool, String) {
    let check = read("scripts/check.sh");
    let start = check
        .find("\nleftovers() {\n")
        .expect("check.sh defines leftovers");
    let body = &check[start..];
    let function = &body[..body.find("\n}\n").expect("leftovers ends") + 3];
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("{function}leftovers"))
        .current_dir(dir)
        // As a shell started there would have it, so `pwd` gives the path
        // as written (macOS's temporary directory is behind a symlink).
        .env("PWD", dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    (out.status.success(), String::from_utf8(out.stderr).unwrap())
}

#[test]
fn check_sh_fails_while_a_process_started_from_the_target_directory_runs() {
    let dir = temp_dir("leftovers");
    fs::create_dir_all(dir.join("target/debug")).unwrap();
    assert_eq!(leftovers(&dir), (true, String::new()));

    // A stand-in for a coordinator a test left running: its arguments name
    // the target directory, by the path the test used, not the resolved one.
    // It waits in the shell's own `read`, so killing it leaves no child.
    let fake = dir.join("target/debug/langspan");
    let mut child = Command::new("sh")
        .args(["-c", "read line"])
        .arg(&fake)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (ok, stderr) = leftovers(&dir);
    let _ = child.kill();
    let _ = child.wait();
    assert!(!ok, "a leftover process passed: {stderr}");
    assert!(stderr.contains(&child.id().to_string()), "{stderr}");
    assert!(stderr.contains("stop them with kill PID"), "{stderr}");
    assert_eq!(leftovers(&dir), (true, String::new()));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ci_installs_the_runners_the_real_tool_tests_need_and_requires_them() {
    let text = workflow("check.yml");
    assert!(
        text.contains("\n  LANGSPAN_REQUIRE_TOOLS: \"1\"\n"),
        "check.yml lets tests/auto_checks.rs skip a missing runner"
    );
    // The shards run the tests; the build jobs run none of these.
    for id in ["linux-tests", "macos-tests"] {
        let job = job(&text, id);
        for line in [
            "uses: actions/setup-go@",
            "cache: false",
            "python3 -m venv \"$RUNNER_TEMP/pytest\"",
            "-m pip install --disable-pip-version-check pytest==",
            "-m pytest --version",
        ] {
            assert!(job.contains(line), "the {id} job lacks {line}");
        }
    }
}

#[test]
fn coverage_runs_nightly_reports_a_drop_and_is_not_a_required_check() {
    let text = workflow("coverage.yml");
    assert!(text.contains("\n  schedule:\n"), "{text}");
    // Every push to main is measured too, so the gate has current main.
    assert!(text.contains("\n  push:\n    branches: [main]\n"), "{text}");
    assert!(
        text.contains("cancel-in-progress: ${{ github.event_name == 'pull_request' }}"),
        "a run on main is never cancelled"
    );
    assert!(
        !text.contains("merge_group"),
        "coverage is no merge-queue gate"
    );
    let coverage = job(&text, "coverage");
    for line in [
        "install-ci-tools.sh \"$HOME/.cache/langspan-ci-tools\" cargo-nextest cargo-llvm-cov",
        "rustup component add llvm-tools",
        "run: bash scripts/coverage.sh target/coverage",
        "cat target/coverage/report.md >> \"$GITHUB_STEP_SUMMARY\"",
    ] {
        assert!(coverage.contains(line), "the coverage job lacks {line}");
    }
    // A drop opens or comments on the issue a failed nightly check opens.
    let report = job(&text, "nightly-report");
    assert!(report.contains("if: github.event_name == 'schedule' && failure()"));
    assert!(report.contains("TITLE: Nightly Rust checks on main failed"));
    assert!(
        job(&workflow("check.yml"), "nightly-report")
            .contains("TITLE: Nightly Rust checks on main failed")
    );
    let script = read("scripts/coverage.sh");
    for line in [
        "export LLVM_PROFILE_FILE_NAME='langspan-%c%16m.profraw'",
        "eval \"$(cargo llvm-cov show-env --sh)\"",
        "cargo nextest run --workspace --locked || status=$?",
        "export_lcov production.lcov test\n",
        "export_lcov all.lcov test lib bin proc-macro\n",
        "python3 -I -B scripts/coverage-report.py --prepare . \"$out\"\n",
    ] {
        assert!(script.contains(line), "coverage.sh lacks {line}");
    }
}

#[test]
fn fuzzing_runs_nightly_only_and_files_one_bug_issue_with_the_crash_artifact() {
    let text = workflow("fuzz.yml");
    assert!(
        text.contains("\non:\n  schedule:\n    - cron: \"17 5 * * *\"\n  workflow_dispatch:\n"),
        "{text}"
    );
    for trigger in ["pull_request", "merge_group", "push:"] {
        assert!(!text.contains(trigger), "fuzz.yml still names {trigger}");
    }
    assert!(
        job(&text, "fuzz").contains(
            "    outputs:\n      artifact-url: ${{ steps.crashes.outputs.artifact-url }}\n"
        )
    );
    let report = job(&text, "nightly-report");
    for line in [
        "if: github.event_name == 'schedule' && failure()",
        "TITLE: Nightly fuzz run on main failed",
        "ARTIFACT_URL: ${{ needs.fuzz.outputs.artifact-url }}",
        // An open issue with the same title gets a comment, never a twin.
        "gh issue list --state open --label bug --search \"\\\"$TITLE\\\" in:title\"",
        "gh issue comment \"$number\" --body \"$body\"",
        "gh issue create --title \"$TITLE\" --label bug --body \"$body\"",
    ] {
        assert!(report.contains(line), "nightly-report lacks {line}");
    }
}

/// One measured file: its production lines, how many of them integration
/// tests cover, and how many more only in-file unit tests cover.
struct Measured(&'static str, usize, usize, usize);

/// A checkout under `dir` with a coverage baseline of `baseline` rows, and
/// the two exports scripts/coverage.sh writes, for `files`.
fn coverage_fixture(dir: &Path, files: &[Measured], baseline: &str) -> PathBuf {
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(
        dir.join("scripts/coverage-baseline.tsv"),
        format!("# A comment.\n{baseline}"),
    )
    .unwrap();
    let out = dir.join("target/coverage");
    fs::create_dir_all(&out).unwrap();
    let (mut production, mut all) = (String::new(), String::new());
    for Measured(path, lines, covered, unit_only) in files {
        let source = dir.join(path);
        production += &format!("SF:{}\n", source.display());
        all += &format!("SF:{}\n", source.display());
        for line in 1..=*lines {
            let integration = usize::from(line <= *covered);
            let unit = usize::from(line <= covered + unit_only);
            production += &format!("DA:{line},{integration}\n");
            all += &format!("DA:{line},{}\n", integration.max(unit));
        }
        // A line of an in-file test module: measured only with the unit
        // tests, so it is not production code.
        all += &format!("DA:{},1\n", lines + 1);
        production += &format!("LF:{lines}\nLH:{covered}\nend_of_record\n");
        all += "end_of_record\n";
    }
    // Neither the tests nor the toolchain's sources count.
    let extra = format!(
        "SF:{}\nDA:1,0\nend_of_record\nSF:/rustc/abc/library/core/src/lib.rs\nDA:1,0\nend_of_record\n",
        dir.join("tests/support/mod.rs").display()
    );
    fs::write(out.join("production.lcov"), format!("{production}{extra}")).unwrap();
    fs::write(out.join("all.lcov"), format!("{all}{extra}")).unwrap();
    // As scripts/coverage.sh does in the checkout it built.
    let (ok, stdout, stderr) = coverage_report(dir, &out, &["--prepare"]);
    assert!(ok, "{stdout}{stderr}");
    out
}

/// Runs scripts/coverage-report.py; returns (success, stdout, stderr).
fn coverage_report(dir: &Path, out: &Path, args: &[&str]) -> (bool, String, String) {
    let run = Command::new("python3")
        .args(["-I", "-B"])
        .arg(root().join("scripts/coverage-report.py"))
        .args(args)
        .arg(dir)
        .arg(out)
        .output()
        .expect("python3 runs scripts/coverage-report.py");
    (
        run.status.success(),
        String::from_utf8(run.stdout).unwrap(),
        String::from_utf8(run.stderr).unwrap(),
    )
}

/// The files `eleven` measures between the first and the eleventh.
const MIDDLE: [&str; 9] = [
    "src/f01.rs",
    "src/f02.rs",
    "src/f03.rs",
    "src/f04.rs",
    "crates/langspan-core/src/f05.rs",
    "src/f06.rs",
    "src/f07.rs",
    "src/f08.rs",
    "src/daemon/f09.rs",
];

/// Eleven files, src/f00.rs the largest; each 95% covered.
fn eleven(first: Measured, eleventh: Measured) -> Vec<Measured> {
    let mut files = vec![first];
    files.extend(MIDDLE.iter().map(|name| Measured(name, 200, 190, 0)));
    files.push(eleventh);
    files
}

/// A 95.0 row for each of `eleven`'s files, src/f00.rs at `first`.
fn rows(first: &str) -> String {
    let mut rows = format!("src/f00.rs\t{first}\n");
    for name in MIDDLE.iter().chain(&["src/f10.rs"]) {
        rows += &format!("{name}\t95.0\n");
    }
    rows
}

#[test]
fn coverage_holds_when_the_total_and_every_row_hold() {
    let dir = temp_dir("coverage-holds");
    // Integration tests cover 372 of 400 lines, 2 points under the 95.0
    // baseline; in-file unit tests cover 8 more, so 95.0 again. The
    // eleventh file, the smallest, is held to its row too: 93.0 is 2 under.
    let mut files = eleven(
        Measured("src/f00.rs", 400, 372, 8),
        Measured("src/f10.rs", 100, 93, 0),
    );
    // Under 100 production lines, a file needs no row.
    files.push(Measured("src/small.rs", 99, 50, 0));
    let out = coverage_fixture(&dir, &files, &rows("95.0"));
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(ok, "{stdout}{stderr}");
    // 2,233 of 2,399 production lines; the test-module lines aren't counted.
    assert!(
        stdout.contains("Production lines covered: 93.1% (2233 of 2399); the floor is 90%."),
        "{stdout}"
    );
    assert!(
        stdout.contains("| File | Lines | Covered | % | Baseline |"),
        "{stdout}"
    );
    assert!(
        stdout.contains("| `src/f00.rs` | 400 | 380 | 95.0 | 95.0 |"),
        "{stdout}"
    );
    assert!(
        stdout.contains("| `src/f10.rs` | 100 | 93 | 93.0 | 95.0 |"),
        "{stdout}"
    );
    assert!(
        stdout.contains("| `src/small.rs` | 99 | 50 | 50.5 | none |"),
        "{stdout}"
    );
    assert!(!stdout.contains("tests/support"), "{stdout}");
    assert!(!stdout.contains("/rustc/"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn coverage_fails_under_the_total_floor_or_two_points_under_any_row() {
    let dir = temp_dir("coverage-drops");
    // The largest file is 2.1 points under its row; the eleventh, outside
    // the ten largest, 3 points.
    let files = eleven(
        Measured("src/f00.rs", 1000, 929, 0),
        Measured("src/f10.rs", 100, 92, 0),
    );
    let out = coverage_fixture(&dir, &files, &rows("95.0"));
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(!ok, "{stdout}");
    for (file, now) in [("src/f00.rs", "92.9"), ("src/f10.rs", "92.0")] {
        let message = format!(
            "`{file}` covers {now}% of its production lines, more than 2 points under its baseline of 95.0%"
        );
        assert!(stdout.contains(&message), "{stdout}");
        assert!(stderr.contains(&message), "{stderr}");
    }
    let _ = fs::remove_dir_all(&dir);

    // Exactly 2 points under holds.
    let dir = temp_dir("coverage-two-points");
    let files = eleven(
        Measured("src/f00.rs", 1000, 930, 0),
        Measured("src/f10.rs", 100, 93, 0),
    );
    let out = coverage_fixture(&dir, &files, &rows("95.0"));
    let (ok, stdout, _) = coverage_report(&dir, &out, &[]);
    assert!(ok, "{stdout}");
    let _ = fs::remove_dir_all(&dir);

    let dir = temp_dir("coverage-total");
    let files = eleven(
        Measured("src/f00.rs", 4000, 3200, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    let out = coverage_fixture(&dir, &files, &rows("80.0"));
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(!ok, "{stdout}");
    let message = "the total, 84.8% of production lines, is under the 90% floor";
    assert!(stderr.contains(message), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn coverage_fails_for_a_row_without_its_file_or_a_large_file_without_a_row() {
    // src/f05.rs was split: its row stays behind and the new file has none.
    let dir = temp_dir("coverage-moved");
    let mut files = eleven(
        Measured("src/f00.rs", 400, 380, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    files.push(Measured("src/f05/part.rs", 100, 95, 0));
    // A row whose file exists but measures no production lines.
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/tests_only.rs"), "#[cfg(test)]\nmod t {}\n").unwrap();
    let baseline = rows("95.0") + "src/f05.rs\t95.0\nsrc/tests_only.rs\t90.0\n";
    let out = coverage_fixture(&dir, &files, &baseline);
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(!ok, "{stdout}");
    for message in [
        "`src/f05.rs` has a baseline row but no longer exists; move its row to the files its lines went to",
        "`src/tests_only.rs` has a baseline row but no production lines measured; remove the row",
        "`src/f05/part.rs` has 100 production lines and no baseline row; add one",
    ] {
        assert!(stdout.contains(message), "{stdout}");
        assert!(stderr.contains(message), "{stderr}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn coverage_fails_for_a_row_whose_record_keeps_no_production_lines() {
    // The export has a record for src/tests_only.rs, but --prepare drops
    // every line of it as test-only: the row fails as if the record were
    // absent, and the file isn't counted as 100% covered.
    let dir = temp_dir("coverage-emptied");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/tests_only.rs"), "#[cfg(test)]\nmod t {}\n").unwrap();
    let mut files = eleven(
        Measured("src/f00.rs", 400, 380, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    files.push(Measured("src/tests_only.rs", 2, 2, 0));
    let baseline = rows("95.0") + "src/tests_only.rs\t90.0\n";
    let out = coverage_fixture(&dir, &files, &baseline);
    let prepared = fs::read_to_string(out.join("production.lcov")).unwrap();
    assert!(
        kept_lines(&prepared, "src/tests_only.rs").is_empty(),
        "{prepared}"
    );
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(!ok, "{stdout}");
    let message =
        "`src/tests_only.rs` has a baseline row but no production lines measured; remove the row";
    assert!(stdout.contains(message), "{stdout}");
    assert!(stderr.contains(message), "{stderr}");
    assert!(!stdout.contains("| `src/tests_only.rs` |"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);

    // Every record emptied: no production lines at all, which fails like an
    // export with no records instead of passing at 100%.
    let dir = temp_dir("coverage-all-emptied");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/tests_only.rs"), "#[cfg(test)]\nmod t {}\n").unwrap();
    let files = [Measured("src/tests_only.rs", 2, 2, 0)];
    let out = coverage_fixture(&dir, &files, "");
    for args in [&[][..], &["--write-baseline"]] {
        let (ok, stdout, stderr) = coverage_report(&dir, &out, args);
        assert!(!ok, "{args:?}: {stdout}");
        assert!(stderr.contains("no production lines"), "{args:?}: {stderr}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn writing_the_coverage_baseline_records_one_row_per_file() {
    let dir = temp_dir("coverage-write");
    let mut files = eleven(
        Measured("src/f00.rs", 400, 390, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    // Under 100 production lines: no row.
    files.push(Measured("src/small.rs", 99, 50, 0));
    let out = coverage_fixture(&dir, &files, "src/gone.rs\t99.0\n");
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &["--write-baseline"]);
    assert!(ok, "{stdout}{stderr}");
    let written = fs::read_to_string(dir.join("scripts/coverage-baseline.tsv")).unwrap();
    let rows: Vec<&str> = written.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(rows.len(), 11, "{written}");
    assert_eq!(rows[0], "crates/langspan-core/src/f05.rs\t95.0");
    assert!(rows.contains(&"src/f00.rs\t97.5"), "{written}");
    assert!(!written.contains("gone.rs"), "{written}");
    assert!(!written.contains("small.rs"), "{written}");
    // The written baseline holds at once.
    let (ok, stdout, _) = coverage_report(&dir, &out, &[]);
    assert!(ok, "{stdout}");
    assert!(!stdout.contains("no baseline row"), "{stdout}");

    // A nightly artifact measured in another checkout updates this one's
    // baseline: the prepared exports name repository-relative sources.
    let elsewhere = temp_dir("coverage-elsewhere");
    let artifact = elsewhere.join("target/coverage");
    fs::create_dir_all(&artifact).unwrap();
    fs::create_dir_all(elsewhere.join("scripts")).unwrap();
    fs::write(
        elsewhere.join("scripts/coverage-baseline.tsv"),
        "# A comment.\n",
    )
    .unwrap();
    for name in ["production.lcov", "all.lcov"] {
        let text = fs::read_to_string(out.join(name)).unwrap();
        assert!(!text.contains(&*dir.to_string_lossy()), "{text}");
        fs::write(artifact.join(name), text).unwrap();
    }
    let (ok, stdout, stderr) = coverage_report(&elsewhere, &artifact, &["--write-baseline"]);
    assert!(ok, "{stdout}{stderr}");
    let moved = fs::read_to_string(elsewhere.join("scripts/coverage-baseline.tsv")).unwrap();
    assert_eq!(moved, written, "the same rows in either checkout");
    let _ = fs::remove_dir_all(&dir);

    // Exports never prepared, from another checkout, fail and say why.
    let dir = temp_dir("coverage-unprepared");
    let raw = format!(
        "SF:{}\nDA:1,1\nend_of_record\n",
        dir.join("src/f00.rs").display()
    );
    fs::write(artifact.join("production.lcov"), &raw).unwrap();
    fs::write(artifact.join("all.lcov"), &raw).unwrap();
    let (ok, _, stderr) = coverage_report(&elsewhere, &artifact, &[]);
    assert!(!ok);
    assert!(
        stderr.contains("no production lines")
            && stderr.contains("1 source outside the checkout")
            && stderr.contains("coverage-report.py --prepare"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&elsewhere);

    // The repository's own baseline is one sorted row per file.
    let baseline = read("scripts/coverage-baseline.tsv");
    let rows: Vec<&str> = baseline.lines().filter(|l| !l.starts_with('#')).collect();
    let mut sorted = rows.clone();
    sorted.sort_unstable();
    sorted.dedup_by(|a, b| a.split('\t').next() == b.split('\t').next());
    assert_eq!(rows, sorted, "rows are sorted, one per file");
}

#[test]
fn coverage_drops_test_only_items_that_integration_tests_compile() {
    // A source an integration test includes with #[path], so its #[cfg(test)]
    // items are measured with the production lines. Each line is production
    // code (P) or test-only (T).
    let source = [
        "pub fn a() {}",                           // P
        "#[cfg(test)]",                            // T: a module
        "mod tests {",                             // T
        "    fn t() { let _ = \"}\"; }",           // T: a brace in a string
        "}",                                       // T
        "#[cfg(test)]",                            // T: stacked attributes
        "#[path = \"../tests/support/shape.rs\"]", // T
        "mod shape;",                              // T
        "pub fn b() {",                            // P
        "    #[cfg(test)]",                        // T: a statement
        "    COUNT.with(|c| c.set(1));",           // T
        "    let x = 1;",                          // P
        "    #[cfg(all(test, unix))]",             // T: an indented module
        "    mod inner {",                         // T
        "        fn u() {}",                       // T
        "    }",                                   // T
        "    #[cfg(test)]",                        // T: a multi-line call
        "    record(",                             // T
        "        x,",                              // T
        "    );",                                  // T
        "}",                                       // P
        "#[cfg(test)]",                            // T: a multi-line signature
        "pub(crate) fn helper(",                   // T
        "    a: u8,",                              // T
        ") -> u8 {",                               // T
        "    a",                                   // T
        "}",                                       // T
        "#[cfg(any(test, feature = \"x\"))]",      // P: also built without tests
        "pub fn c() {}",                           // P
        "#[cfg(not(test))]",                       // P
        "pub fn d() {}",                           // P
        "#[cfg(test)]",                            // T: literals at column 0
        "mod literals {",                          // T
        "    const RAW: &str = r#\"",              // T
        "}",                                       // T: in a raw string
        "#[cfg(test)]",                            // T: not an attribute
        "\"#;",                                    // T
        "    const TEXT: &str = \"",               // T
        "} \\\" {",                                // T: in a string
        "\";",                                     // T
        "    const OPEN: char = '{';",             // T: a char
        "    fn f<'a>(x: &'a str) -> &'a str {",   // T: lifetimes
        "        x // }",                          // T: in a comment
        "    }",                                   // T
        "    /* } */",                             // T
        "}",                                       // T
        "pub const S: &str = \"#[cfg(test)]\";",   // P: in a string
        "pub fn e() -> u8 {",                      // P
        "    #[cfg(test)]",                        // T: a final expression
        "    { 1 }",                               // T
        "}",                                       // P
    ];
    let test_only = |n| matches!(n, 2..=8 | 10..=11 | 13..=20 | 22..=27 | 32..=46 | 49..=50);
    let production: Vec<usize> = (1..=source.len()).filter(|&n| !test_only(n)).collect();
    let dir = temp_dir("coverage-test-only");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/m.rs"), source.join("\n") + "\n").unwrap();
    let measured = [Measured("src/m.rs", source.len(), source.len(), 0)];
    let out = coverage_fixture(&dir, &measured, "");
    let prepared = fs::read_to_string(out.join("production.lcov")).unwrap();
    assert_eq!(kept_lines(&prepared, "src/m.rs"), production, "{prepared}");
    // The record's totals count what it keeps.
    let totals = format!("LF:{0}\nLH:{0}\nend_of_record\n", production.len());
    assert!(prepared.contains(&totals), "{prepared}");
    let (_, stdout, _) = coverage_report(&dir, &out, &[]);
    let row = format!(
        "| `src/m.rs` | {0} | {0} | 100.0 | none |",
        production.len()
    );
    assert!(stdout.contains(&row), "{stdout}");
    // A second preparation changes nothing.
    let (ok, _, stderr) = coverage_report(&dir, &out, &["--prepare"]);
    assert!(ok, "{stderr}");
    let again = fs::read_to_string(out.join("production.lcov")).unwrap();
    assert_eq!(again, prepared);
    let _ = fs::remove_dir_all(&dir);

    // tests/parser_properties.rs includes src/text/redact.rs: from its first
    // #[cfg(test)] to the end, only the blank lines between test items stay.
    let dir = temp_dir("coverage-redact");
    let text = read("src/text/redact.rs");
    let source: Vec<&str> = text.lines().collect();
    let first = 1 + source.iter().position(|l| *l == "#[cfg(test)]").unwrap();
    fs::create_dir_all(dir.join("src/text")).unwrap();
    fs::write(dir.join("src/text/redact.rs"), &text).unwrap();
    let measured = [Measured("src/text/redact.rs", source.len(), 0, 0)];
    let out = coverage_fixture(&dir, &measured, "");
    let prepared = fs::read_to_string(out.join("production.lcov")).unwrap();
    let kept = kept_lines(&prepared, "src/text/redact.rs");
    let (before, after): (Vec<usize>, Vec<usize>) = kept.iter().partition(|&&n| n < first);
    assert_eq!(before, (1..first).collect::<Vec<_>>());
    assert!(
        after.len() == 1 && source[after[0] - 1].is_empty(),
        "{after:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The line numbers the record of `file` in a prepared export keeps.
fn kept_lines(export: &str, file: &str) -> Vec<usize> {
    export
        .split("end_of_record\n")
        .find(|record| record.starts_with(&format!("SF:{file}\n")))
        .unwrap_or_else(|| panic!("no record of {file} in {export}"))
        .lines()
        .filter_map(|line| line.strip_prefix("DA:"))
        .map(|da| da.split(',').next().unwrap().parse().unwrap())
        .collect()
}

/// Runs scripts/github-drift.py against a saved observed state; returns
/// (exit code, stdout).
fn github_drift(observed: &Path) -> (i32, String) {
    let out = Command::new("python3")
        .args(["-I", "-B"])
        .arg(root().join("scripts/github-drift.py"))
        .arg("--settings")
        .arg(root().join(".github/desired-settings.json"))
        .arg("--observed")
        .arg(observed)
        .output()
        .expect("python3 runs scripts/github-drift.py");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
    )
}

#[test]
fn the_settings_drift_audit_names_each_changed_setting() {
    let desired = read(".github/desired-settings.json");
    for word in ["ghp_", "github_pat_", "ghs_", "\"token\"", "\"key\""] {
        assert!(
            !desired.contains(word),
            "desired-settings.json holds {word}"
        );
    }
    let dir = temp_dir("github-drift");
    let same = dir.join("same.json");
    fs::write(&same, &desired).unwrap();
    let (code, stdout) = github_drift(&same);
    assert_eq!(code, 0, "{stdout}");

    let drifted = desired
        .replacen(
            "\"delete_branch_on_merge\": true",
            "\"delete_branch_on_merge\": false",
            1,
        )
        .replacen(
            "\"required_approving_review_count\": 0",
            "\"required_approving_review_count\": 1",
            1,
        );
    assert_ne!(drifted, desired);
    let changed = dir.join("changed.json");
    fs::write(&changed, drifted).unwrap();
    let (code, stdout) = github_drift(&changed);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains("repo.delete_branch_on_merge: want true, GitHub has false"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "rulesets.main.rules.pull_request.required_approving_review_count: want 0, GitHub has 1"
        ),
        "{stdout}"
    );
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
}

const WORKSPACE_PACKAGES: [&str; 3] = ["langspan", "langspan-core", "langspan-graph"];

/// The version of each workspace package that `lock` pins, from its
/// `[[package]]` tables, sorted by name.
fn locked_workspace_versions(lock: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for table in lock.split("[[package]]").skip(1) {
        let field = |key: &str| {
            table.lines().find_map(|line| {
                line.strip_prefix(&format!("{key} = \""))
                    .and_then(|rest| rest.strip_suffix('"'))
                    .map(str::to_owned)
            })
        };
        if let (Some(name), Some(version)) = (field("name"), field("version"))
            && WORKSPACE_PACKAGES.contains(&name.as_str())
        {
            found.push((name, version));
        }
    }
    found.sort();
    found
}

/// The fuzz targets build with `--locked` against their own lock file
/// (fuzz.yml), which release-please's rust updater doesn't touch, so a
/// release PR that bumps the workspace version without it fails Fuzz (#571).
/// release-please-config.json bumps it through `extra-files`, one entry per
/// workspace package, and the lock holds the current version. release-please's
/// toml updater parses the file into tagged values (`{ value, start, end }`),
/// so a filter compares `@.name.value`; `@.name` is an object and matches
/// nothing.
#[test]
fn fuzz_lock_pins_the_current_workspace_version() {
    let version = env!("CARGO_PKG_VERSION");
    let expected: Vec<(String, String)> = WORKSPACE_PACKAGES
        .iter()
        .map(|name| ((*name).to_owned(), version.to_owned()))
        .collect();
    assert_eq!(
        locked_workspace_versions(&read("fuzz/Cargo.lock")),
        expected,
        "fuzz/Cargo.lock pins another version of the workspace than Cargo.toml's {version}: run \
         `cargo update -p langspan --manifest-path fuzz/Cargo.toml`"
    );
    let config: serde_json::Value =
        serde_json::from_str(&read("release-please-config.json")).expect("the config parses");
    let extra = config["packages"]["."]["extra-files"]
        .as_array()
        .expect("release-please-config.json lists extra-files");
    for name in WORKSPACE_PACKAGES {
        let entry = serde_json::json!({
            "type": "toml",
            "path": "fuzz/Cargo.lock",
            "jsonpath": format!("$.package[?(@.name.value=='{name}')].version"),
        });
        assert!(
            extra.contains(&entry),
            "release-please-config.json doesn't bump {name} in fuzz/Cargo.lock: add {entry} to extra-files"
        );
    }
}
