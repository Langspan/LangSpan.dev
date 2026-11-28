#![cfg(unix)]
//! `langspan tokens` (#493) through the real CLI: a real Claude Code folder
//! on disk and a real Git repository, because what is verified here is the
//! walk of the transcript tree (links not followed, copies counted once),
//! the first-parent history Git reports and the exit status. The counting
//! itself is tested in-process in `src/tokens.rs`.

mod support;

use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SLICE: &str = include_str!("fixtures/tokens/claude-transcript-slice.jsonl");

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn base() -> Dir {
    let dir = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!(
            "ls-tokens-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(dir.join("work")).unwrap();
    Dir(dir)
}

/// Today in UTC, `YYYY-MM-DD`.
fn today() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (year, month, day) = langspan::today::civil_from_days((seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn commit(work: &Path, file: &str, date: &str) {
    fs::create_dir_all(work.join(file).parent().unwrap()).unwrap();
    fs::write(work.join(file), date).unwrap();
    support::git(work, &["add", "."]);
    let status = Command::new("git")
        .arg("-C")
        .arg(work)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["commit", "-q", "-m", file])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_COMMITTER_DATE", format!("{date}T00:00:01Z"))
        .env("GIT_AUTHOR_DATE", format!("{date}T00:00:01Z"))
        .status()
        .unwrap();
    assert!(status.success());
}

fn tokens(claude: &Path, work: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("tokens")
        .args(args)
        .arg("--root")
        .arg(work)
        .env("CLAUDE_CONFIG_DIR", claude)
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

#[test]
fn tokens_per_merged_change_from_a_claude_folder_and_a_branch() {
    let base = base();
    let work = base.0.join("work");
    let today = today();
    support::git(&work, &["init", "-q", "-b", "main"]);
    commit(&work, "src/a.rs", &today);
    commit(&work, "AGENTS.md", &today);
    let session = base.0.join("claude/projects/-work");
    fs::create_dir_all(session.join("s1/subagents")).unwrap();
    let slice = SLICE.replace("2026-11-25", &today);
    fs::write(session.join("s1.jsonl"), &slice).unwrap();
    // A resumed session's copy of the same messages counts once.
    fs::write(session.join("s1/subagents/agent-1.jsonl"), &slice).unwrap();
    // A link is not followed: its target would add a fifth message.
    let outside = base.0.join("outside.jsonl");
    fs::write(
        &outside,
        slice.replace("msg_011ChQ2khYGmsb3gBTYeMaLa", "msg_outside"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, session.join("link.jsonl")).unwrap();

    let output = tokens(
        &base.0.join("claude"),
        &work,
        &["--onto", "main", "--days", "2", "--json"],
    );
    assert!(output.status.success(), "{output:?}");
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["files"], 2);
    assert_eq!(answer["messages"], 4);
    let days = answer["days"].as_array().unwrap();
    assert_eq!(days.len(), 2);
    assert_eq!(days[0]["merged"], 0);
    assert_eq!(days[0]["per_change"], Value::Null);
    assert_eq!(days[1]["day"], today);
    assert_eq!(days[1]["merged"], 2);
    assert_eq!(days[1]["tokens"]["total"], 187_597);
    assert_eq!(days[1]["per_change"]["total"], 93_798);
    assert_eq!(days[1]["per_change"]["uncached_input"], 30_998);
    assert_eq!(days[1]["per_change"]["output"], 309);
    assert_eq!(days[1]["process_changes"], serde_json::json!(["AGENTS.md"]));
    assert_eq!(answer["confounders"].as_array().unwrap().len(), 4);

    let text = tokens(
        &base.0.join("claude"),
        &work,
        &["--onto", "main", "--days", "1"],
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains(&format!(
            "{today}: 2 merged, 0.1M tokens per change (0.0M uncached input, 0.0M output)  [process change: AGENTS.md]"
        )),
        "{text}"
    );
    assert!(
        text.contains("Confounders:\n- Tokens are every Claude Code session"),
        "{text}"
    );
}

/// Git itself is what's verified here: how `--since` walks a history whose
/// commit dates go backwards, how a rename is reported, and how a non-ASCII
/// path is written.
#[test]
fn history_with_older_dates_renames_and_quoted_paths_is_read_whole() {
    let base = base();
    let work = base.0.join("work");
    let today = today();
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (year, month, day) = langspan::today::civil_from_days((seconds / 86_400) as i64 - 40);
    let older = format!("{year:04}-{month:02}-{day:02}");
    support::git(&work, &["init", "-q", "-b", "main"]);
    commit(&work, "é/CLAUDE.md", &today);
    // Older than the window, between two commits inside it.
    commit(&work, "AGENTS.md", &older);
    support::git(&work, &["mv", "AGENTS.md", "old-instructions.md"]);
    commit(&work, "src/a.rs", &today);
    fs::create_dir_all(base.0.join("claude/projects")).unwrap();

    let output = tokens(
        &base.0.join("claude"),
        &work,
        &["--onto", "main", "--days", "1", "--json"],
    );
    assert!(output.status.success(), "{output:?}");
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    let days = answer["days"].as_array().unwrap();
    assert_eq!(days[0]["merged"], 2, "{answer}");
    assert_eq!(
        days[0]["process_changes"],
        serde_json::json!(["AGENTS.md", "é/CLAUDE.md"]),
        "{answer}"
    );
}

#[test]
fn tokens_says_what_is_missing() {
    let base = base();
    let work = base.0.join("work");
    support::git(&work, &["init", "-q", "-b", "main"]);
    commit(&work, "src/a.rs", &today());
    let missing = tokens(&base.0.join("none"), &work, &["--onto", "main"]);
    assert_eq!(missing.status.code(), Some(1));
    let said = String::from_utf8(missing.stderr).unwrap();
    assert!(said.contains("no Claude Code transcripts at"), "{said}");
    assert!(said.contains("set CLAUDE_CONFIG_DIR"), "{said}");

    fs::create_dir_all(base.0.join("claude/projects")).unwrap();
    let branch = tokens(&base.0.join("claude"), &work, &["--onto", "nowhere"]);
    assert_eq!(branch.status.code(), Some(1));
    let said = String::from_utf8(branch.stderr).unwrap();
    assert!(
        said.contains("cannot read the history of nowhere"),
        "{said}"
    );
    assert!(said.contains("check that --onto names a branch"), "{said}");

    let unnamed = tokens(&base.0.join("claude"), &work, &[]);
    assert!(
        String::from_utf8(unnamed.stderr)
            .unwrap()
            .contains("needs --onto BRANCH")
    );
    let days = tokens(
        &base.0.join("claude"),
        &work,
        &["--onto", "main", "--days", "91"],
    );
    assert!(
        String::from_utf8(days.stderr)
            .unwrap()
            .contains("--days must be 1 to 90")
    );
}
