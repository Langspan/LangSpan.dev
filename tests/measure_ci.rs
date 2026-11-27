//! Runs `scripts/measure-ci.sh` against a stand-in `gh` that serves one
//! pull-request run and one merge-queue run, their jobs, a log for each job
//! and the Content workflow's runs. The stand-in behaves
//! as one of two GitHub CLI generations: one that has no
//! `--allow-escape-sequences` flag and passes a log's escape sequences
//! through, and one that has the flag and, without it, rewrites each escape
//! character as `^[`. Real `bash`, `jq`, `awk` and `cargo metadata`: what is
//! verified is the script's own handling of the CLI, so it runs as a process.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The stand-in `gh`. `GH_HAS_FLAG=yes` is the newer CLI, anything else the
/// older one. It applies `--jq` with the real `jq`, as `gh` does.
const GH: &str = r#"#!/bin/sh
[ "$1" = api ] || { echo "stub gh: only api" >&2; exit 2; }
shift
flag=no filter= url=
while [ "$#" -gt 0 ]; do
	case "$1" in
	--help)
		echo "Usage: gh api <endpoint> [flags]"
		echo "  -q, --jq expression   Query to select values from the response"
		[ "$GH_HAS_FLAG" = yes ] && echo "      --allow-escape-sequences   Keep terminal escape sequences in the output"
		exit 0 ;;
	--allow-escape-sequences)
		[ "$GH_HAS_FLAG" = yes ] || { echo "unknown flag: --allow-escape-sequences" >&2; exit 1; }
		flag=yes ;;
	--jq) filter=$2; shift ;;
	--paginate) ;;
	-*) echo "stub gh: unexpected flag $1" >&2; exit 2 ;;
	*) url=$1 ;;
	esac
	shift
done
case "$url" in
*/actions/workflows/check.yml/runs*) jq -c "$filter" "$FIXTURE/runs.json" ;;
*/actions/workflows/content.yml/runs*) jq -c "$filter" "$FIXTURE/content.json" ;;
*/actions/runs/1/attempts/1/jobs*) jq -c "$filter" "$FIXTURE/jobs.json" ;;
*/actions/runs/2/attempts/1/jobs*) jq -c "$filter" "$FIXTURE/queue-jobs.json" ;;
*/actions/jobs/*/logs)
	if [ "$GH_HAS_FLAG" = yes ] && [ "$flag" = no ]; then
		sed "s/$(printf '\033')/^[/g" "$FIXTURE/log"
	else
		cat "$FIXTURE/log"
	fi ;;
*) echo "stub gh: unexpected endpoint $url" >&2; exit 2 ;;
esac
"#;

const RUNS: &str = r#"{"workflow_runs": [{"id": 1, "status": "completed", "conclusion": "success",
 "run_attempt": 1, "event": "pull_request", "created_at": "2026-11-27T04:00:00Z",
 "run_started_at": "2026-11-27T04:00:00Z", "updated_at": "2026-11-27T04:10:00Z", "head_sha": "abc"},
 {"id": 2, "status": "completed", "conclusion": "success",
 "run_attempt": 1, "event": "merge_group", "created_at": "2026-11-27T05:00:00Z",
 "run_started_at": "2026-11-27T05:00:00Z", "updated_at": "2026-11-27T05:20:30Z", "head_sha": "queued"}]}"#;

const JOBS: &str = r#"{"jobs": [{"run_id": 1, "id": 11, "name": "Linux", "conclusion": "success",
 "started_at": "2026-11-27T04:00:30Z", "completed_at": "2026-11-27T04:09:30Z",
 "steps": [{"name": "Check formatting, lint and tests", "conclusion": "success",
            "started_at": "2026-11-27T04:01:00Z", "completed_at": "2026-11-27T04:09:00Z"}]}]}"#;

/// The merge-queue run's jobs: the required Linux and Workflows checks end
/// at 05:09 and 05:01, and macOS, which the queue doesn't wait for, at 05:20.
const QUEUE_JOBS: &str = r#"{"jobs": [
 {"run_id": 2, "id": 21, "name": "Linux", "conclusion": "success",
  "started_at": "2026-11-27T05:00:30Z", "completed_at": "2026-11-27T05:09:00Z",
  "steps": [{"name": "Check formatting, lint and tests", "conclusion": "success",
             "started_at": "2026-11-27T05:01:00Z", "completed_at": "2026-11-27T05:08:30Z"}]},
 {"run_id": 2, "id": 22, "name": "macOS", "conclusion": "success",
  "started_at": "2026-11-27T05:00:30Z", "completed_at": "2026-11-27T05:20:00Z",
  "steps": [{"name": "Check formatting, lint and tests", "conclusion": "success",
             "started_at": "2026-11-27T05:01:00Z", "completed_at": "2026-11-27T05:19:30Z"}]},
 {"run_id": 2, "id": 23, "name": "Workflows", "conclusion": "success",
  "started_at": "2026-11-27T05:00:20Z", "completed_at": "2026-11-27T05:01:00Z", "steps": []}]}"#;

/// The Content workflow's run on the merge-queue commit, ending at `updated`.
fn content(updated: &str) -> String {
    format!(
        r#"{{"workflow_runs": [{{"id": 3, "status": "completed", "conclusion": "success",
 "run_attempt": 1, "event": "merge_group", "created_at": "2026-11-27T05:00:00Z",
 "updated_at": "{updated}", "head_sha": "queued"}}]}}"#
    )
}

/// The Linux check is the last required one to end.
fn content_first() -> String {
    content("2026-11-27T05:02:00Z")
}

/// No Content run in the window.
const NO_CONTENT: &str = r#"{"workflow_runs": []}"#;

/// A job log as GitHub stores it: timestamped lines, with cargo's colored
/// `Compiling` lines, one for a dependency and one for a workspace crate.
const LOG: &str = "2026-11-27T04:00:31.0000000Z ##[group]Run actions/cache/restore@v4\n\
2026-11-27T04:00:31.0000000Z   key: rust-v1-Linux-X64-Linux-abc\n\
2026-11-27T04:00:40.0000000Z Cache restored from key: rust-v1-Linux-X64-Linux-abc\n\
2026-11-27T04:01:00.0000000Z ##[group]Run sh scripts/check.sh\n\
2026-11-27T04:01:05.0000000Z \x1b[1m\x1b[32m   Compiling\x1b[0m serde v1.0.0\n\
2026-11-27T04:01:09.0000000Z \x1b[1m\x1b[32m   Compiling\x1b[0m langspan-core v0.7.0\n";

/// Runs the script with the stand-in `gh` first on `PATH` and returns its
/// report, or `None` when `jq` is missing and tools aren't required.
fn report(name: &str, has_flag: bool, content: &str) -> Option<String> {
    report_with(name, has_flag, content, QUEUE_JOBS)
}

/// [`report`] with the merge-queue run's jobs given.
fn report_with(name: &str, has_flag: bool, content: &str, queue_jobs: &str) -> Option<String> {
    report_jobs(name, has_flag, content, JOBS, queue_jobs)
}

/// [`report`] with both runs' jobs given.
fn report_jobs(
    name: &str,
    has_flag: bool,
    content: &str,
    jobs: &str,
    queue_jobs: &str,
) -> Option<String> {
    if Command::new("jq").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("LANGSPAN_REQUIRE_TOOLS").is_none(),
            "jq is required"
        );
        println!("skipped: jq is not installed");
        return None;
    }
    let base = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("langspan-measure-ci-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("bin")).unwrap();
    let gh = base.join("bin/gh");
    fs::write(&gh, GH).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(base.join("runs.json"), RUNS).unwrap();
    fs::write(base.join("jobs.json"), jobs).unwrap();
    fs::write(base.join("queue-jobs.json"), queue_jobs).unwrap();
    fs::write(base.join("content.json"), content).unwrap();
    fs::write(base.join("log"), LOG).unwrap();
    let path = format!(
        "{}:{}",
        base.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let output = Command::new("bash")
        .arg(repo("scripts/measure-ci.sh"))
        .args(["--logs", "1"])
        .current_dir(repo(""))
        .env("PATH", path)
        .env("FIXTURE", &base)
        .env("GH_HAS_FLAG", if has_flag { "yes" } else { "no" })
        .output()
        .unwrap();
    fs::remove_dir_all(&base).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "measure-ci failed: {stderr}");
    assert!(stderr.is_empty(), "measure-ci warned: {stderr}");
    Some(String::from_utf8(output.stdout).unwrap())
}

/// The row the report gives the one job whose log was read: exact cache
/// hit, one dependency and one workspace crate compiled, a 9-minute job.
const READ_ROW: &str =
    "| 1 | pull_request | Linux | exact | 1 | 1 | 9 | `   Compiling serde v1.0.0` |";

#[test]
fn reads_logs_with_a_gh_that_has_no_escape_sequence_flag() {
    if let Some(report) = report("old", false, &content_first()) {
        assert!(report.contains(READ_ROW), "{report}");
    }
}

#[test]
fn keeps_escape_sequences_with_a_gh_that_has_the_flag() {
    if let Some(report) = report("new", true, &content_first()) {
        assert!(report.contains(READ_ROW), "{report}");
    }
}

/// Workflow completion waits for macOS: 20.5 minutes from run creation.
const WORKFLOW_ROW: &str = "| merge_group | 1 | 20.5 | 20.5 | 20.5 | 0 |";

#[test]
fn the_queue_gate_ends_with_the_last_required_check_not_macos() {
    if let Some(report) = report("linux-last", false, &content_first()) {
        assert!(report.contains(WORKFLOW_ROW), "{report}");
        // Linux, the last required check, ends 9 minutes after creation.
        assert!(
            report.contains("| merge_group | 1 | 9 | 9 | 9 | 0 |"),
            "{report}"
        );
    }
}

#[test]
fn the_queue_gate_waits_for_a_content_check_that_ends_last() {
    let late = content("2026-11-27T05:12:00Z");
    if let Some(report) = report("content-last", false, &late) {
        assert!(
            report.contains("| merge_group | 1 | 12 | 12 | 12 | 0 |"),
            "{report}"
        );
    }
}

#[test]
fn a_queue_run_without_a_workflows_result_is_left_out_of_the_gate_and_counted() {
    let jobs = QUEUE_JOBS.replace(r#""name": "Workflows""#, r#""name": "Other""#);
    if let Some(report) = report_with("no-workflows", false, &content_first(), &jobs) {
        assert!(report.contains(WORKFLOW_ROW), "{report}");
        assert!(
            report.contains("| merge_group | 0 | – | – | – | 1 |"),
            "{report}"
        );
    }
}

/// The sharded layout's pull-request run: the fast lane ran in `Linux build`,
/// the shard was skipped, `Linux` only reported, and `macOS build` passed at
/// once.
const SHARDED_JOBS: &str = r#"{"jobs": [
 {"run_id": 1, "id": 11, "name": "Linux build", "conclusion": "success",
  "started_at": "2026-11-27T04:00:30Z", "completed_at": "2026-11-27T04:09:30Z",
  "steps": [{"name": "Check formatting, dependencies, lint, real integration tests and the demonstration",
             "conclusion": "success", "started_at": "2026-11-27T04:01:00Z", "completed_at": "2026-11-27T04:09:00Z"}]},
 {"run_id": 1, "id": 12, "name": "Linux tests 1", "conclusion": "skipped",
  "started_at": "2026-11-27T04:09:31Z", "completed_at": "2026-11-27T04:09:31Z", "steps": []},
 {"run_id": 1, "id": 13, "name": "Linux", "conclusion": "success",
  "started_at": "2026-11-27T04:09:40Z", "completed_at": "2026-11-27T04:09:50Z",
  "steps": [{"name": "Pass only when the build and every shard passed", "conclusion": "success",
             "started_at": "2026-11-27T04:09:42Z", "completed_at": "2026-11-27T04:09:45Z"}]},
 {"run_id": 1, "id": 14, "name": "macOS build", "conclusion": "success",
  "started_at": "2026-11-27T04:00:30Z", "completed_at": "2026-11-27T04:00:40Z",
  "steps": [{"name": "Pass a pull request at once", "conclusion": "success",
             "started_at": "2026-11-27T04:00:31Z", "completed_at": "2026-11-27T04:00:32Z"}]}]}"#;

/// The sharded layout's merge-queue run. `Linux build` takes 5.5 minutes,
/// two Linux shards take 2 and 1 minutes (their step 1 minute each), the
/// required `Linux` result ends at 05:09, and `macOS` at 05:20.
const SHARDED_QUEUE_JOBS: &str = r#"{"jobs": [
 {"run_id": 2, "id": 21, "name": "Linux build", "conclusion": "success",
  "started_at": "2026-11-27T05:00:30Z", "completed_at": "2026-11-27T05:06:00Z",
  "steps": [{"name": "Check formatting, dependencies, lint, real integration tests and the demonstration",
             "conclusion": "success", "started_at": "2026-11-27T05:01:00Z", "completed_at": "2026-11-27T05:05:30Z"}]},
 {"run_id": 2, "id": 24, "name": "Linux tests 1", "conclusion": "success",
  "started_at": "2026-11-27T05:06:10Z", "completed_at": "2026-11-27T05:08:10Z",
  "steps": [{"name": "Run this shard of the tests", "conclusion": "success",
             "started_at": "2026-11-27T05:07:00Z", "completed_at": "2026-11-27T05:08:00Z"}]},
 {"run_id": 2, "id": 25, "name": "Linux tests 2", "conclusion": "success",
  "started_at": "2026-11-27T05:06:10Z", "completed_at": "2026-11-27T05:07:10Z",
  "steps": [{"name": "Run this shard of the tests", "conclusion": "success",
             "started_at": "2026-11-27T05:06:20Z", "completed_at": "2026-11-27T05:07:20Z"}]},
 {"run_id": 2, "id": 26, "name": "Linux", "conclusion": "success",
  "started_at": "2026-11-27T05:08:30Z", "completed_at": "2026-11-27T05:09:00Z",
  "steps": [{"name": "Pass only when the build and every shard passed", "conclusion": "success",
             "started_at": "2026-11-27T05:08:40Z", "completed_at": "2026-11-27T05:08:50Z"}]},
 {"run_id": 2, "id": 22, "name": "macOS build", "conclusion": "success",
  "started_at": "2026-11-27T05:00:30Z", "completed_at": "2026-11-27T05:10:00Z",
  "steps": [{"name": "Check formatting, dependencies, lint, real integration tests and the demonstration",
             "conclusion": "success", "started_at": "2026-11-27T05:01:00Z", "completed_at": "2026-11-27T05:09:30Z"}]},
 {"run_id": 2, "id": 27, "name": "macOS tests 1", "conclusion": "success",
  "started_at": "2026-11-27T05:10:10Z", "completed_at": "2026-11-27T05:19:10Z",
  "steps": [{"name": "Run this shard of the tests", "conclusion": "success",
             "started_at": "2026-11-27T05:11:00Z", "completed_at": "2026-11-27T05:19:00Z"}]},
 {"run_id": 2, "id": 28, "name": "macOS", "conclusion": "success",
  "started_at": "2026-11-27T05:19:30Z", "completed_at": "2026-11-27T05:20:00Z", "steps": []},
 {"run_id": 2, "id": 23, "name": "Workflows", "conclusion": "success",
  "started_at": "2026-11-27T05:00:20Z", "completed_at": "2026-11-27T05:01:00Z", "steps": []}]}"#;

/// The fixtures above copy check.yml's job and step names, which the script
/// selects by; a rename there must come here and to the script too.
#[test]
fn the_fixtures_name_the_jobs_and_steps_check_yml_has() {
    let check = fs::read_to_string(repo(".github/workflows/check.yml")).unwrap();
    for name in [
        "name: Linux build\n",
        "name: macOS build\n",
        "name: Linux tests ${{ matrix.shard }}\n",
        "name: macOS tests ${{ matrix.shard }}\n",
        "name: Linux\n",
        "name: macOS\n",
        "name: Workflows\n",
    ] {
        assert!(check.contains(&format!("    {name}")), "no job {name}");
    }
    let step = |prefix: &str| check.matches(&format!("- name: {prefix}")).count();
    // One build step in each build job, one shard step in each shard job.
    assert_eq!(step("Check formatting, "), 2);
    assert_eq!(step("Run this shard of the tests\n"), 2);
}

fn sharded(name: &str, queue_jobs: &str) -> Option<String> {
    report_jobs(name, false, &content_first(), SHARDED_JOBS, queue_jobs)
}

#[test]
fn a_sharded_run_counts_as_built_and_its_gate_ends_with_the_linux_result() {
    if let Some(report) = sharded("sharded", SHARDED_QUEUE_JOBS) {
        assert!(
            report.contains("| pull_request | 1 | 10 | 10 | 10 | 0 |"),
            "{report}"
        );
        assert!(report.contains(WORKFLOW_ROW), "{report}");
        assert!(
            report.contains("| merge_group | 1 | 9 | 9 | 9 | 0 |"),
            "{report}"
        );
    }
}

#[test]
fn a_sharded_run_reports_build_jobs_with_cache_facts_and_shards_together() {
    if let Some(report) = sharded("sharded-jobs", SHARDED_QUEUE_JOBS) {
        for row in [
            "| pull_request | Linux build | exact | 1 | 9 | 8 | 1 |",
            "| merge_group | Linux build | exact | 1 | 5.5 | 4.5 | 1 |",
            "| merge_group | macOS build | exact | 1 | 9.5 | 8.5 | 1 |",
            "| merge_group | Linux tests | no log read | 2 | 1.5 | 1 | – |",
            "| merge_group | macOS tests | no log read | 1 | 9 | 8 | – |",
            "| 2 | merge_group | Linux build | exact | 1 | 1 | 5.5 |",
        ] {
            assert!(report.contains(row), "no row {row}: {report}");
        }
        // The result jobs only report, and a skipped shard or a build job
        // that passed at once ran nothing.
        for absent in [
            "| Linux | ",
            "| macOS | ",
            "| pull_request | Linux tests",
            "| pull_request | macOS build",
        ] {
            assert!(!report.contains(absent), "row {absent}: {report}");
        }
    }
}

#[test]
fn a_sharded_run_whose_build_jobs_skipped_their_check_is_markdown_only() {
    let jobs = SHARDED_QUEUE_JOBS.replace(
        r#""conclusion": "success", "started_at": "2026-11-27T05:01:00Z""#,
        r#""conclusion": "skipped", "started_at": "2026-11-27T05:01:00Z""#,
    );
    if let Some(report) = sharded("sharded-markdown", &jobs) {
        assert!(
            report.contains("| merge_group | 0 | – | – | – | 1 |"),
            "{report}"
        );
    }
}

#[test]
fn a_queue_run_without_a_content_result_is_left_out_of_the_gate_and_counted() {
    if let Some(report) = report("no-content", false, NO_CONTENT) {
        assert!(report.contains(WORKFLOW_ROW), "{report}");
        assert!(
            report.contains("| merge_group | 0 | – | – | – | 1 |"),
            "{report}"
        );
    }
}
