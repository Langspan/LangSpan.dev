#![cfg(unix)]
//! `scripts/capture-gemini.sh` refuses to run, and writes nothing, unless
//! `GEMINI_CLI_HOME` names an existing directory and Gemini CLI answers a
//! one-word prompt there (issue A6). Gemini CLI has no sign-in status
//! command. The `gemini` on `PATH` here is a stand-in for that one prompt
//! only: the real one needs an account, and the script stops before any
//! other command.

use serde_json::Value;
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/capture-gemini.sh");

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A new directory holding a `gemini` that, asked a `-p` prompt in the
/// default approval mode, prints `output` and exits with `code`, and whose
/// every other command fails.
fn scratch(output: &str, code: i32) -> Scratch {
    let base = std::env::temp_dir().join(format!(
        "ls-capture-gemini-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(base.join("bin")).unwrap();
    let gemini = base.join("bin/gemini");
    fs::write(
        &gemini,
        format!(
            "#!/bin/sh\n[ \"$1 $2 $3\" = '--approval-mode default -p' ] || exit 99\necho '{output}'\nexit {code}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&gemini, fs::Permissions::from_mode(0o700)).unwrap();
    Scratch(base)
}

/// Run the script with `gemini_home` as `GEMINI_CLI_HOME`, or with it unset.
fn capture(base: &Path, gemini_home: Option<&Path>) -> Output {
    let path = format!(
        "{}:{}",
        base.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new("bash");
    command
        .arg(SCRIPT)
        .arg(base.join("out"))
        .env("PATH", path)
        .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_STATE_DIR", base.join("state"))
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(base);
    match gemini_home {
        Some(home) => command.env("GEMINI_CLI_HOME", home),
        None => command.env_remove("GEMINI_CLI_HOME"),
    };
    command.output().unwrap()
}

#[test]
fn the_gemini_capture_stops_and_writes_nothing_when_gemini_is_not_signed_in() {
    for (output, code) in [
        ("Please set an Auth method", 41),
        ("", 1),
        ("an answer without the word", 0),
    ] {
        let base = scratch(output, code);
        let home = base.0.join("gemini-home");
        fs::create_dir(&home).unwrap();
        let run = capture(&base.0, Some(&home));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert_ne!(run.status.code(), Some(0), "{output:?} {code}: {stderr}");
        assert!(
            stderr.contains("gemini is not signed in"),
            "{output:?} {code}: {stderr}"
        );
        // Gemini's own answer is not echoed.
        if !output.is_empty() {
            assert!(!stderr.contains(output), "{output:?} {code}: {stderr}");
        }
        for written in ["out", "state"] {
            assert!(
                !base.0.join(written).exists(),
                "{output:?} {code}: wrote {written}"
            );
        }
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "{output:?} {code}");
    }
}

#[test]
fn the_gemini_capture_refuses_to_run_without_a_separate_gemini_home() {
    let base = scratch("ready", 0);
    let missing = base.0.join("no-such-home");
    for home in [None, Some(missing.as_path())] {
        let run = capture(&base.0, home);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert_ne!(run.status.code(), Some(0), "{home:?}: {stderr}");
        assert!(stderr.contains("set GEMINI_CLI_HOME"), "{home:?}: {stderr}");
        assert!(!base.0.join("out").exists(), "{home:?}");
        assert!(!base.0.join("state").exists(), "{home:?}");
        assert!(!missing.exists(), "{home:?}");
    }
}

/// Every Gemini CLI command the script runs is in the default approval mode,
/// with no option that approves a tool, allows one by policy or skips trust:
/// the person approves each edit by hand.
#[test]
fn the_gemini_capture_never_approves_automatically() {
    let script = fs::read_to_string(SCRIPT).unwrap();
    let code: Vec<&str> = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    for banned in [
        "auto_edit",
        "yolo",
        "--policy",
        "--allowed-tools",
        "--skip-trust",
        " -y ",
    ] {
        for line in &code {
            assert!(!line.contains(banned), "{banned:?} in: {line}");
        }
    }
    let runs: Vec<&&str> = code
        .iter()
        .filter(|line| line.contains("gemini --") || line.contains("gemini -p"))
        .collect();
    assert!(runs.len() >= 3, "{runs:?}");
    for line in runs {
        assert!(line.contains("gemini --approval-mode default"), "{line}");
    }
}

/// A relative `GEMINI_CLI_HOME`, `LANGSPAN_STATE_DIR` or `XDG_STATE_HOME`
/// names a directory under the caller's working directory, yet Gemini CLI,
/// its hooks and the steps run from other directories. The script makes each
/// absolute before the first `gemini` runs, so the sign-in probe reads the
/// signed-in home, and the hooks in the throwaway repository and the steps
/// reach the coordinator that `langspan status` checked.
#[test]
fn the_gemini_capture_resolves_relative_homes_before_changing_directory() {
    let base = scratch("ready", 0);
    fs::create_dir(base.0.join("gemini-home")).unwrap();
    // The stand-in records what Gemini CLI would see, outside the home.
    let seen = base.0.join("seen");
    let stub = [
        "#!/bin/sh",
        "[ \"$1 $2 $3\" = '--approval-mode default -p' ] || exit 99",
        &format!(
            "printf '%s\\n' \"$(pwd -P)\" \"$GEMINI_CLI_HOME\" \"$LANGSPAN_STATE_DIR\" \"$XDG_STATE_HOME\" > '{}'",
            seen.display()
        ),
        "echo ready",
        "",
    ]
    .join("\n");
    fs::write(base.0.join("bin/gemini"), stub).unwrap();
    let path = format!(
        "{}:{}",
        base.0.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = Command::new("bash")
        .arg(SCRIPT)
        .arg("out")
        .env("PATH", path)
        .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
        .env("GEMINI_CLI_HOME", "gemini-home")
        .env("LANGSPAN_STATE_DIR", "state")
        .env("XDG_STATE_HOME", "./xdg")
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(&base.0)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    // Past the sign-in probe: it stops at the coordinator check instead.
    assert!(!stderr.contains("not signed in"), "{stderr}");
    assert!(stderr.contains("start the coordinator first"), "{stderr}");
    let base_path = fs::canonicalize(&base.0).unwrap();
    let seen = fs::read_to_string(&seen).unwrap();
    let lines: Vec<&str> = seen.lines().collect();
    assert_eq!(lines.len(), 4, "{seen}");
    let home = Path::new(lines[1]);
    assert!(home.is_absolute(), "{seen}");
    for dir in [lines[0], lines[1]] {
        assert_eq!(
            fs::canonicalize(dir).unwrap(),
            base_path.join("gemini-home"),
            "{seen}"
        );
    }
    for (line, name) in [(lines[2], "state"), (lines[3], "xdg")] {
        let resolved = Path::new(line);
        assert!(resolved.is_absolute(), "{name}: {seen}");
        assert_eq!(
            fs::canonicalize(resolved.parent().unwrap()).unwrap(),
            base_path,
            "{name}: {seen}"
        );
        assert_eq!(resolved.file_name().unwrap(), name, "{name}: {seen}");
    }
    assert!(!base.0.join("out").exists());
    assert!(!base.0.join("state").exists());
}

/// A coordinator the test starts, and stops when dropped.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon(state: &Path) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["daemon", "--state-dir"])
        .arg(state)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let daemon = Daemon(child);
    let socket = state.join("daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    daemon
}

/// The steps run in a second terminal, from any directory and without the
/// script's environment. Given a relative `LANGSPAN_STATE_DIR`, the script
/// writes its absolute form into `peer.sh` and `step.sh`, so they reach the
/// coordinator `langspan status` checked, and not one under the directory
/// the person runs them from.
#[test]
fn the_gemini_capture_steps_reach_the_checked_coordinator_from_anywhere() {
    let base = scratch("ready", 0);
    let root = fs::canonicalize(&base.0).unwrap();
    fs::create_dir(root.join("gemini-home")).unwrap();
    let _daemon = daemon(&root.join("state"));
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = Command::new("bash")
        .arg(SCRIPT)
        .arg("out")
        .env("PATH", &path)
        .env("HOME", &root)
        .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
        .env("GEMINI_CLI_HOME", "gemini-home")
        .env("LANGSPAN_STATE_DIR", "state")
        .env_remove("XDG_STATE_HOME")
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(&root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    // Everything is set up; it stops where a person would press Enter.
    assert!(stderr.contains("run this at a terminal"), "{stderr}");
    let out = root.join("out");
    let state = root.join("state");
    for script in ["peer.sh", "step.sh"] {
        let text = fs::read_to_string(out.join(script)).unwrap();
        assert!(
            text.contains(&format!("export LANGSPAN_STATE_DIR='{}'", state.display())),
            "{script}: {text}"
        );
    }
    // From another directory, with no state directory in the environment.
    let elsewhere = root.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let snap = Command::new("bash")
        .arg(out.join("step.sh"))
        .args(["snap", "probe"])
        .env("PATH", &path)
        .env("HOME", &root)
        .env_remove("LANGSPAN_STATE_DIR")
        .env_remove("XDG_STATE_HOME")
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(&elsewhere)
        .output()
        .unwrap();
    assert!(snap.status.success(), "{snap:?}");
    let status = fs::read_to_string(out.join("status-probe.json")).unwrap();
    let status: Value =
        serde_json::from_str(&status).unwrap_or_else(|error| panic!("{error}: {status}"));
    // The script's workspace, and the session `peer.sh` started, are both
    // on that coordinator.
    for name in ["capture-repo-", "capture-peer-repo-"] {
        assert!(status.to_string().contains(name), "{name}: {status}");
    }
    assert!(!elsewhere.join("state").exists());
}

/// `langspan` against the coordinator in `state`.
fn cli(state: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(args)
        .env("LANGSPAN_STATE_DIR", state)
        .env("LANGSPAN_AUTOSTART", "off")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}{}",
        &args[..2],
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// More than one `langspan events` page of events, by sessions named
/// `{prefix}-N` in workspace `{prefix}`, rooted at `base/{prefix}`: each
/// admission names 40 long paths.
fn fill_a_page(state: &Path, base: &Path, prefix: &str) {
    let root = base.join(prefix);
    fs::create_dir(&root).unwrap();
    cli(
        state,
        &[
            "workspace",
            "add",
            prefix,
            &root.display().to_string(),
            "--request-id",
            &format!("{prefix}-workspace"),
        ],
    );
    for n in 0..12 {
        let session = format!("{prefix}.{prefix}-{n}");
        cli(
            state,
            &[
                "session",
                "start",
                &session,
                prefix,
                "--request-id",
                &format!("{prefix}-start-{n}"),
            ],
        );
        let paths: Vec<String> = (0..40)
            .map(|p| format!("{prefix}/{n}/{p}/{}.txt", "x".repeat(160)))
            .collect();
        let mut args = vec!["admit", session.as_str()];
        args.extend(paths.iter().map(String::as_str));
        let request = format!("{prefix}-admit-{n}");
        args.extend(["--ttl-ms", "60000", "--request-id", &request]);
        cli(state, &args);
    }
}

/// Run the script in `root` against the coordinator in `state`, up to where
/// a person would press Enter, writing `root/out`.
fn set_up(root: &Path, state: &Path) {
    let run = Command::new("bash")
        .arg(SCRIPT)
        .arg("out")
        .env("PATH", test_path(root))
        .env("HOME", root)
        .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
        .env("GEMINI_CLI_HOME", "gemini-home")
        .env("LANGSPAN_STATE_DIR", state)
        .env_remove("XDG_STATE_HOME")
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("run this at a terminal"),
        "{:?} {stderr}",
        run.status
    );
}

fn test_path(root: &Path) -> String {
    format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// `step.sh` with `args`, from `root`, as a second terminal runs it: without
/// the script's state directory in the environment.
fn step(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("bash");
    command
        .arg(root.join("out/step.sh"))
        .args(args)
        .env("PATH", test_path(root))
        .env("HOME", root)
        .env_remove("LANGSPAN_STATE_DIR")
        .env_remove("XDG_STATE_HOME")
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(root);
    command
}

/// `step.sh hold` and `step.sh peer` each record what they set up: the
/// hold's admission takes a request id, as every coordinator mutation
/// does, and a refusal is reported rather than dropped.
#[test]
fn the_gemini_capture_hold_and_peer_steps_reach_the_coordinator() {
    let base = scratch("ready", 0);
    let root = fs::canonicalize(&base.0).unwrap();
    fs::create_dir(root.join("gemini-home")).unwrap();
    let state = root.join("state");
    let _daemon = daemon(&state);
    set_up(&root, &state);
    let out = root.join("out");

    // `hold` sleeps past the lease once it has taken it: stop it there.
    let mut hold = step(&root, &["hold"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let held = out.join("status-after-hold.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !held.exists() {
        if let Some(status) = hold.try_wait().unwrap() {
            panic!(
                "hold ended first, {status}: {}",
                fs::read_to_string(out.join("hold.txt")).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "hold took no lease");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = hold.kill();
    let _ = hold.wait();
    // The snapshot is written once `admit` has returned successfully.
    let status = cli(&state, &["status", "--json"]);
    assert!(status.contains(".holder"), "{status}");
    assert!(status.contains("notes.txt"), "{status}");

    let peer = step(&root, &["peer"]).output().unwrap();
    assert!(peer.status.success(), "{peer:?}");
    for written in ["peer-PreToolUse.json", "peer-PostToolUse.json"] {
        assert!(out.join(written).exists(), "{written}");
    }
    let saved = step(&root, &["events"]).output().unwrap();
    assert!(saved.status.success(), "{saved:?}");
    let events = fs::read_to_string(out.join("events.txt")).unwrap();
    // The hold's lease, under its own request id, and the peer's write,
    // refused while that lease lasts (the person waits it out) and then seen.
    assert!(
        events.contains(r#""type":"lease_granted""#) && events.contains("-hold-"),
        "{events}"
    );
    assert!(events.contains(r#""type":"write_refused""#), "{events}");
    assert!(events.contains(r#""type":"source_observed""#), "{events}");
}

/// Every sequence recorded so far, read a page at a time.
fn sequences(state: &Path) -> Vec<u64> {
    let mut all = Vec::new();
    loop {
        let after = all.last().copied().unwrap_or(0).to_string();
        let page = cli(state, &["events", "--after", &after, "--limit", "1000"]);
        let before = all.len();
        all.extend(
            page.lines()
                .map(|line| line.split(' ').next().unwrap().parse::<u64>().unwrap()),
        );
        if all.len() == before {
            return all;
        }
    }
}

/// The coordinator already holds more than a page of other history when the
/// capture starts, and the capture's own events fill more than a page too.
/// `step.sh events`, which the script runs after `/quit`, saves exactly the
/// events recorded since the script started, every page of them: not the
/// coordinator's oldest page.
#[test]
fn the_gemini_capture_saves_its_own_events_past_older_history_and_one_page() {
    let base = scratch("ready", 0);
    let root = fs::canonicalize(&base.0).unwrap();
    fs::create_dir(root.join("gemini-home")).unwrap();
    let state = root.join("state");
    let _daemon = daemon(&state);
    fill_a_page(&state, &root, "older");
    let before = sequences(&state);
    set_up(&root, &state);
    // What the person's Gemini CLI session would add, more than a page.
    fill_a_page(&state, &root, "during");
    let all = sequences(&state);

    let out = root.join("out");
    let saved = step(&root, &["events"]).output().unwrap();
    assert!(saved.status.success(), "{saved:?}");
    let events = fs::read_to_string(out.join("events.txt")).unwrap();
    // One page holds at most 48 KiB of events.
    assert!(events.len() > 48 * 1024, "{} bytes", events.len());
    let seqs: Vec<u64> = events
        .lines()
        .map(|line| line.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(seqs, all[before.len()..], "{events}");
    for line in events.lines() {
        let (_, event) = line.split_once(' ').unwrap();
        serde_json::from_str::<Value>(event).unwrap_or_else(|error| panic!("{error}: {line}"));
    }
    // The capture's setup, and the last event, are there; older history is not.
    assert!(events.contains("capture-peer-repo-"));
    assert!(events.contains("during-11"));
    assert!(!events.contains("older-"));
    assert_eq!(
        fs::metadata(out.join("events.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
