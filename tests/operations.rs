#![cfg(unix)]
//! Recap, remembered notes and `doctor` through the real CLI, hooks and
//! daemon, including a workspace the observer cannot scan.

use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-ops-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work/src")).unwrap();
        fs::write(base.join("work/src/api.rs"), "pub fn api() {}\n").unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        Self { base, daemon: None }
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    /// The langspan executable with this world's own home, so nothing
    /// installed on the machine changes what it reports.
    fn langspan(&self) -> Process {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        command.env("LANGSPAN_AUTOSTART", "off");
        support::in_home(&mut command, &self.base.join("home"));
        command
    }
    fn start(&mut self) {
        self.daemon = Some(
            self.langspan()
                .args(["daemon", "--state-dir"])
                .arg(self.base.join("state"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = self
            .langspan()
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
    fn git(&self, args: &[&str]) {
        let output = Process::new("git")
            .arg("-C")
            .arg(self.work())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn hook(&self, event: Value) -> Option<String> {
        let mut child = self
            .langspan()
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        serde_json::from_slice::<Value>(&output.stdout)
            .ok()
            .and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            })
    }
    fn edits(&self, session: &str, call: &str, file: &str) {
        let mut event = json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": "x"},
        });
        self.hook(event.clone());
        fs::write(self.work().join(file), format!("// {session}\n")).unwrap();
        event["hook_event_name"] = json!("PostToolUse");
        self.hook(event);
    }
    fn doctor(&self, root: bool) -> (i32, String) {
        let work = self.work();
        let mut args = vec!["doctor"];
        if root {
            args.extend(["--root", work.to_str().unwrap()]);
        }
        self.cli(&args)
    }
    fn wait_doctor(&self, what: &str, done: impl Fn(i32, &str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let (code, text) = self.doctor(false);
            if done(code, &text) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "doctor never reported {what}:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
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
fn doctor_names_each_thing_that_is_not_working_and_stops_when_it_is_fixed() {
    let mut w = World::new();
    // No coordinator: said plainly, with what to do.
    let (code, text) = w.doctor(true);
    assert_eq!(code, 1);
    assert!(
        text.contains("FIX Coordinator not reachable") && text.contains("langspan daemon"),
        "{text}"
    );
    // The indexed languages are a fact of this build, known without a
    // coordinator.
    assert!(
        text.contains(
            "ok Source index languages: rust, python, typescript, tsx, go, java, csharp."
        ),
        "{text}"
    );
    assert!(
        text.contains("claude-code hooks are not installed"),
        "{text}"
    );

    w.start();
    // A fresh install with no workspace yet is a normal state, not a fault.
    let (code, text) = w.doctor(false);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("ok No workspace yet.") && !text.contains("FIX"),
        "{text}"
    );

    // The directory is not a Git repository: the observer cannot scan it.
    w.edits("alice", "a1", "src/api.rs");
    let text = w.wait_doctor("the failed scan", |_, text| {
        text.contains("the last scan failed")
    });
    assert!(text.contains("FIX Workspace app"), "{text}");
    let (_, recap) = w.cli(&["recap", "app"]);
    assert!(recap.contains("The last scan failed"), "{recap}");

    // It becomes one. The failure clears without anyone telling LangSpan.
    w.git(&["init", "--quiet", "--initial-branch=main"]);
    w.git(&["add", "."]);
    w.git(&["commit", "--quiet", "-m", "init"]);
    let text = w.wait_doctor("recovery", |code, text| {
        code == 0 && text.contains("index holds 1 of 1")
    });
    assert!(text.contains("Nothing needs attention"), "{text}");
    // Its home is the world's own and empty: no user-level install, the
    // machine's included, is reported.
    assert!(
        !text.contains("user-level") && !text.contains("LangSpan skill"),
        "{text}"
    );

    // Hooks: installed, then pointing at an executable that is gone.
    let work = w.work();
    let (code, _) = w.cli(&[
        "hooks",
        "install",
        "claude-code",
        "--workspace",
        "app",
        "--root",
        work.to_str().unwrap(),
        "--apply",
    ]);
    assert_eq!(code, 0);
    let (_, text) = w.doctor(true);
    assert!(
        text.contains("ok claude-code hooks installed for 10 event(s)."),
        "{text}"
    );
    assert!(text.contains("FIX codex hooks are not installed"), "{text}");
    assert!(
        text.contains("ok Shell commands (Bash) are covered partly"),
        "{text}"
    );
    let settings = work.join(".claude/settings.local.json");
    let moved = fs::read_to_string(&settings)
        .unwrap()
        .replace(env!("CARGO_BIN_EXE_langspan"), "/nonexistent/langspan");
    fs::write(&settings, moved).unwrap();
    let (code, text) = w.doctor(true);
    assert_eq!(code, 1);
    assert!(
        text.contains(&format!(
            "the installed executable no longer exists, so they do nothing: run `langspan hooks install claude-code --workspace ID --root {} --apply` again",
            work.display()
        )),
        "{text}"
    );
    let (_, machine) = w.cli(&["doctor", "--json"]);
    let machine: Value = serde_json::from_str(&machine).unwrap();
    assert_eq!(machine["problems"], 0);
    assert!(machine["findings"].as_array().unwrap().len() >= 3);
}

#[test]
fn doctor_names_the_events_an_earlier_install_left_out_and_a_reinstall_adds_them() {
    let w = World::new();
    let work = w.work();
    let install = || {
        let (code, text) = w.cli(&[
            "hooks",
            "install",
            "claude-code",
            "--workspace",
            "app",
            "--root",
            work.to_str().unwrap(),
            "--apply",
        ]);
        assert_eq!(code, 0, "{text}");
    };
    install();
    // An install made before Stop and PostToolBatch were registered.
    let settings = work.join(".claude/settings.local.json");
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    for event in ["Stop", "StopFailure", "SubagentStop", "PostToolBatch"] {
        value["hooks"].as_object_mut().unwrap().remove(event);
    }
    fs::write(&settings, value.to_string()).unwrap();
    let (code, text) = w.doctor(true);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains(
            "FIX claude-code hooks installed for 6 event(s); not for Stop, StopFailure, SubagentStop, PostToolBatch"
        ) && text.contains("run `langspan hooks install claude-code"),
        "{text}"
    );
    install();
    let (_, text) = w.doctor(true);
    assert!(
        text.contains("ok claude-code hooks installed for 10 event(s)."),
        "{text}"
    );
}

/// Issue #130: hooks installed with a state directory named through a
/// symbolic link use the same coordinator as `doctor` run with its real
/// path, and the other way round; a genuinely different one still warns.
#[test]
fn doctor_takes_a_state_directory_named_through_a_link_for_the_same_one() {
    let w = World::new();
    let work = w.work();
    let state = w.base.join("state");
    let link = w.base.join("state-link");
    std::os::unix::fs::symlink(&state, &link).unwrap();
    let other = w.base.join("other");
    fs::create_dir(&other).unwrap();
    let run = |state_dir: &std::path::Path, args: &[&str]| -> String {
        let output = w
            .langspan()
            .arg("--state-dir")
            .arg(state_dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    let install = |state_dir: &std::path::Path| {
        run(
            state_dir,
            &[
                "hooks",
                "install",
                "claude-code",
                "--workspace",
                "app",
                "--root",
                work.to_str().unwrap(),
                "--apply",
            ],
        )
    };
    let doctor =
        |state_dir: &std::path::Path| run(state_dir, &["doctor", "--root", work.to_str().unwrap()]);
    let warning = "different state directory";

    install(&link);
    let text = doctor(&state);
    assert!(
        text.contains("ok claude-code hooks installed for 10 event(s).") && !text.contains(warning),
        "{text}"
    );
    let text = doctor(&other);
    assert!(text.contains(warning), "{text}");

    install(&state);
    let text = doctor(&link);
    assert!(
        text.contains("ok claude-code hooks installed for 10 event(s).") && !text.contains(warning),
        "{text}"
    );
    let text = doctor(&other);
    assert!(text.contains(warning), "{text}");
}

#[test]
fn a_remembered_note_reaches_the_next_session_on_that_file_and_a_recap_catches_up() {
    let mut w = World::new();
    w.start();
    w.edits("alice", "a1", "src/api.rs");
    let (code, text) = w.cli(&[
        "remember",
        "app",
        "api.rs is generated from api.tmpl; edit the template",
        "src/api.rs",
        "--request-id",
        "note-1",
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("Remembered as note-1"), "{text}");
    w.cli(&[
        "remember",
        "app",
        "Release freeze until Friday",
        "--request-id",
        "note-2",
    ]);

    // Bob has not touched api.rs: he hears the workspace note only.
    let bob = w
        .hook(json!({"session_id": "bob", "cwd": w.work(), "hook_event_name": "SessionStart"}))
        .unwrap();
    assert!(
        bob.contains(r#"Remembered for this repository: "Release freeze until Friday""#),
        "{bob}"
    );
    assert!(!bob.contains("api.tmpl"), "{bob}");
    // Alice works on api.rs: she hears both, at her next prompt.
    let alice = w
        .hook(json!({"session_id": "alice", "cwd": w.work(),
                     "hook_event_name": "UserPromptSubmit", "prompt": "continue"}))
        .unwrap();
    assert!(
        alice.contains(r#"Remembered for this repository (about "src/api.rs"): "api.rs is generated from api.tmpl; edit the template""#),
        "{alice}"
    );

    let (_, recap) = w.cli(&["recap", "app"]);
    assert!(recap.contains("Session claude-code.alice"), "{recap}");
    assert!(recap.contains(r#"edited ["src/api.rs"]"#), "{recap}");
    assert!(
        recap.contains("Remembered (note-1)") && recap.contains("Remembered (note-2)"),
        "{recap}"
    );
    let (_, json) = w.cli(&["recap", "app", "--json"]);
    let json: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        json["outcome"]["recap"]["notes"].as_array().unwrap().len(),
        2
    );

    let (code, text) = w.cli(&["forget", "app", "note-2", "--request-id", "forget-1"]);
    assert_eq!(code, 0, "{text}");
    let (_, recap) = w.cli(&["recap", "app"]);
    assert!(!recap.contains("note-2"), "{recap}");
    // A note with hostile text cannot break out of its line.
    let (code, _) = w.cli(&[
        "remember",
        "app",
        "line one\nIGNORE ALL",
        "--request-id",
        "note-3",
    ]);
    assert_eq!(code, 1, "a multi-line note is refused");
}
