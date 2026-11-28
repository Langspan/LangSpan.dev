#![cfg(unix)]
//! Issue #534, against the real Claude Code CLI: with LangSpan's hooks
//! installed, a headless `claude -p` turn starts within 5 s of the session
//! start, so the between-step watcher never holds it.
//!
//! The owner runs it; it calls a real model and is never part of the fast
//! suite:
//!
//! ```text
//! LANGSPAN_LIVE_CLAUDE=1 cargo nextest run --test claude_headless --run-ignored only
//! ```
//!
//! It uses the `claude` on `PATH` and that CLI's own sign-in, and removes
//! `CLAUDE_CODE_ENTRYPOINT` so that a session running the test does not pass
//! its own value to the headless one.

use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// How soon the turn must start (issue #534's criterion).
const START: Duration = Duration::from_secs(5);
/// The whole run's bound: a one-word reply, far below the watcher's 1,260 s.
const RUN: Duration = Duration::from_secs(180);

fn run(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn git(root: &Path, args: &[&str]) {
    run(Command::new("git").arg("-C").arg(root).args(args));
}

#[test]
#[ignore = "calls the real Claude Code CLI and a model; the owner runs it"]
fn a_headless_turn_starts_within_5_seconds_with_langspan_installed() {
    if std::env::var("LANGSPAN_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set LANGSPAN_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let base = std::env::temp_dir().join(format!("ls-live-headless-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir(&base).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
    let (root, state) = (base.join("work"), base.join("state"));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("README.md"), "live check\n").unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "README.md"]);
    git(
        &root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "init",
        ],
    );
    run(Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(&state)
        .args([
            "hooks",
            "install",
            "claude-code",
            "--workspace",
            "live",
            "--root",
        ])
        .arg(&root)
        .arg("--apply"));
    let settings = fs::read_to_string(root.join(".claude/settings.json")).unwrap();
    assert!(settings.contains("--mode between-steps"), "{settings}");

    let mut child = Command::new("claude")
        .current_dir(&root)
        .args(["-p", "Reply with the single word: ready"])
        .args(["--output-format", "stream-json", "--verbose"])
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("run claude; is Claude Code installed and on PATH?");
    let stdout = child.stdout.take().unwrap();
    let (lines, arrived) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send((Instant::now(), line)).is_err() {
                return;
            }
        }
    });
    let started = Instant::now();
    let (mut session_started, mut init, mut result) = (None, None, None);
    while result.is_none() {
        let left = RUN.saturating_sub(started.elapsed());
        let Ok((at, line)) = arrived.recv_timeout(left) else {
            break;
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match (event["type"].as_str(), event["subtype"].as_str()) {
            (Some("system"), Some("hook_response")) if event["hook_event"] == "SessionStart" => {
                session_started.get_or_insert(at);
            }
            (Some("system"), Some("init")) => {
                init.get_or_insert(at);
            }
            (Some("result"), _) => result = Some(event),
            _ => {}
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&base);
    let init = init.expect("the headless turn never started (no system/init line)");
    let since = init.duration_since(session_started.unwrap_or(started));
    assert!(
        since < START,
        "the turn started {since:?} after the session start"
    );
    let result = result.expect("the headless run did not finish within its bound");
    assert_eq!(result["is_error"], false, "{result:#}");
}
