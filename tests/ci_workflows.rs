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
                "\n    timeout-minutes: ${{ github.event_name == 'pull_request' && 10 || github.event_name == 'merge_group' && 35 || 30 }}\n"
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
fn a_macos_failure_on_main_opens_or_updates_one_issue() {
    let text = workflow("check.yml");
    assert!(job(&text, "macos").contains("\n      failed: ${{ steps.failed.outputs.failed }}\n"));
    assert!(job(&text, "macos").contains(
        "if: always() && steps.changes.outputs.code == 'true'\n        run: bash scripts/ci-failed-tests.sh \"target/nextest/$NEXTEST_PROFILE/junit.xml\"\n"
    ));
    let report = job(&text, "macos-report");
    for line in [
        "\n    needs: [macos]\n",
        "\n    if: always() && github.event_name == 'push' && github.ref == 'refs/heads/main' && (needs.macos.result == 'failure' || needs.macos.result == 'cancelled')\n",
        "\n      issues: write\n",
        "TITLE: macOS Rust checks on main failed",
        "SHA: ${{ github.sha }}",
        "FAILED: ${{ needs.macos.outputs.failed }}",
        "gh issue comment \"$number\"",
        "gh issue create --title \"$TITLE\"",
    ] {
        assert!(report.contains(line), "macos-report lacks {line}");
    }
    // It runs no repository code: nothing is checked out.
    assert!(!report.contains("actions/checkout"), "{report}");
    assert!(!report.contains("contents:"), "{report}");
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
    // Both lanes end by looking for leftovers: the fast lane before it
    // exits, the full lane on its last line.
    assert!(check.contains("  leftovers\n  echo \"check.sh: fast lane done"));
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
    for id in ["linux", "macos"] {
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

/// Eleven files, src/f00.rs the largest; each 95% covered.
fn eleven(first: Measured, eleventh: Measured) -> Vec<Measured> {
    const NAMES: [&str; 9] = [
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
    let mut files = vec![first];
    files.extend(NAMES.iter().map(|name| Measured(name, 200, 190, 0)));
    files.push(eleventh);
    files
}

const BASELINE: &str = "src/f00.rs\t95.0\nsrc/f10.rs\t95.0\n";

#[test]
fn coverage_holds_when_the_total_and_the_largest_files_hold() {
    let dir = temp_dir("coverage-holds");
    // Integration tests cover 372 of 400 lines, 2 points under the 95.0
    // baseline; in-file unit tests cover 8 more, so 95.0 again.
    let files = eleven(
        Measured("src/f00.rs", 400, 372, 8),
        Measured("src/f10.rs", 100, 80, 0),
    );
    let out = coverage_fixture(&dir, &files, BASELINE);
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(ok, "{stdout}{stderr}");
    // 2,170 of 2,300 production lines; the test-module lines aren't counted.
    assert!(
        stdout.contains("Production lines covered: 94.3% (2170 of 2300); the floor is 90%."),
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
    // The eleventh largest may fall further: only the ten largest are held.
    assert!(
        stdout.contains("| `src/f10.rs` | 100 | 80 | 80.0 | 95.0 |"),
        "{stdout}"
    );
    assert!(!stdout.contains("tests/support"), "{stdout}");
    assert!(!stdout.contains("/rustc/"), "{stdout}");
    // A file among the largest with no row is named, not failed.
    assert!(
        stdout.contains("`src/f01.rs` has no baseline row"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn coverage_fails_under_the_total_floor_or_two_points_under_a_largest_file() {
    let dir = temp_dir("coverage-drops");
    // 2.1 points under its baseline.
    let files = eleven(
        Measured("src/f00.rs", 1000, 929, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    let out = coverage_fixture(&dir, &files, BASELINE);
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(!ok, "{stdout}");
    let message = "`src/f00.rs` covers 92.9% of its production lines, more than 2 points under its baseline of 95.0%";
    assert!(stdout.contains(message), "{stdout}");
    assert!(stderr.contains(message), "{stderr}");
    let _ = fs::remove_dir_all(&dir);

    // Exactly 2 points under holds.
    let dir = temp_dir("coverage-two-points");
    let files = eleven(
        Measured("src/f00.rs", 1000, 930, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    let out = coverage_fixture(&dir, &files, BASELINE);
    let (ok, stdout, _) = coverage_report(&dir, &out, &[]);
    assert!(ok, "{stdout}");
    let _ = fs::remove_dir_all(&dir);

    let dir = temp_dir("coverage-total");
    let files = eleven(
        Measured("src/f00.rs", 4000, 3200, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    let out = coverage_fixture(&dir, &files, "src/f00.rs\t80.0\n");
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &[]);
    assert!(!ok, "{stdout}");
    let message = "the total, 84.8% of production lines, is under the 90% floor";
    assert!(stderr.contains(message), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn writing_the_coverage_baseline_records_one_row_per_file() {
    let dir = temp_dir("coverage-write");
    let files = eleven(
        Measured("src/f00.rs", 400, 390, 0),
        Measured("src/f10.rs", 100, 95, 0),
    );
    let out = coverage_fixture(&dir, &files, "src/gone.rs\t99.0\n");
    let (ok, stdout, stderr) = coverage_report(&dir, &out, &["--write-baseline"]);
    assert!(ok, "{stdout}{stderr}");
    let written = fs::read_to_string(dir.join("scripts/coverage-baseline.tsv")).unwrap();
    let rows: Vec<&str> = written.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(rows.len(), 11, "{written}");
    assert_eq!(rows[0], "crates/langspan-core/src/f05.rs\t95.0");
    assert!(rows.contains(&"src/f00.rs\t97.5"), "{written}");
    assert!(!written.contains("gone.rs"), "{written}");
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
