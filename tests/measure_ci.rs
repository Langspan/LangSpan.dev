//! Runs `scripts/measure-ci.sh` against a stand-in `gh` that serves one
//! pull-request run, its Linux job and that job's log. The stand-in behaves
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
*/actions/runs/1/attempts/1/jobs*) jq -c "$filter" "$FIXTURE/jobs.json" ;;
*/actions/jobs/11/logs)
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
 "run_started_at": "2026-11-27T04:00:00Z", "updated_at": "2026-11-27T04:10:00Z", "head_sha": "abc"}]}"#;

const JOBS: &str = r#"{"jobs": [{"run_id": 1, "id": 11, "name": "Linux", "conclusion": "success",
 "started_at": "2026-11-27T04:00:30Z", "completed_at": "2026-11-27T04:09:30Z",
 "steps": [{"name": "Check formatting, lint and tests", "conclusion": "success",
            "started_at": "2026-11-27T04:01:00Z", "completed_at": "2026-11-27T04:09:00Z"}]}]}"#;

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
fn report(name: &str, has_flag: bool) -> Option<String> {
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
    fs::write(base.join("jobs.json"), JOBS).unwrap();
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
    if let Some(report) = report("old", false) {
        assert!(report.contains(READ_ROW), "{report}");
    }
}

#[test]
fn keeps_escape_sequences_with_a_gh_that_has_the_flag() {
    if let Some(report) = report("new", true) {
        assert!(report.contains(READ_ROW), "{report}");
    }
}
