#![cfg(unix)]
//! Roadmap G6 against a real daemon: a checkout moved on disk keeps what
//! LangSpan knew about it, the installed user-level Claude Code and Codex
//! hooks find it again from the new directory, a move to the wrong place
//! changes nothing, and a removed workspace leaves every view but the log.

use langspan::hook::user_workspace_id;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-0000000000a1";
const ALICE_LATER: &str = "00000000-0000-4000-8000-0000000000a2";
const BOB: &str = "00000000-0000-4000-8000-0000000000b1";
const BOB_LATER: &str = "00000000-0000-4000-8000-0000000000b2";
const NOTE: &str = "upload keeps its retry budget";

struct Home {
    base: PathBuf,
    daemon: Option<Child>,
}

impl Home {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "ls-g6-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let base = fs::canonicalize(base).unwrap();
        fs::create_dir(base.join("home")).unwrap();
        Self { base, daemon: None }
    }
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn run(&self, args: &[&str]) -> Output {
        // Before the arguments: a check's command follows a literal `--`.
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.state_dir())
            .args(args)
            .env_remove("CLAUDE_PROJECT_DIR")
            .output()
            .unwrap()
    }
    /// Exit code and stdout.
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = self.run(args);
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {text}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        text
    }
    fn json(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.run(&args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: {error}: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }
    /// Every registered session, from the complete state records.
    fn sessions(&self) -> serde_json::Map<String, Value> {
        let records = self.json(&["status", "--all"]);
        records["outcome"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|record| record["kind"] == "session")
            .map(|record| {
                (
                    record["key"].as_str().unwrap().to_owned(),
                    record["session"].clone(),
                )
            })
            .collect()
    }
    fn install(&self) {
        for (relative, text) in [
            (".claude/settings.json", "{}\n"),
            (".claude.json", "{}\n"),
            (".codex/hooks.json", "{}\n"),
            (".codex/config.toml", ""),
        ] {
            let path = self.home().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["install", "--apply", "--home"])
            .arg(self.home())
            .arg("--state-dir")
            .arg(self.state_dir())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    fn start(&mut self) {
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.run(&["status"]).status.code() != Some(0) {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// The installed user-level command for one host event.
    fn installed(&self, relative: &str, event: &str) -> String {
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(self.home().join(relative)).unwrap()).unwrap();
        settings["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["hooks"].as_array().unwrap())
            .map(|hook| hook["command"].as_str().unwrap().to_owned())
            .find(|command| command.contains(":user:v1"))
            .unwrap()
    }
    /// Run a hook command as the host does, in `cwd`. Stdout and coverage.
    fn hook(&self, command: &str, cwd: &Path, event: &Value) -> (String, String) {
        let mut child = Process::new("/bin/sh")
            .args(["-c", command])
            .current_dir(cwd)
            .env_remove("CLAUDE_PROJECT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap())
            .unwrap_or_else(|_| panic!("structured diagnostic expected: {stderr}"));
        (
            String::from_utf8(output.stdout).unwrap(),
            diagnostic["coverage"].as_str().unwrap().to_owned(),
        )
    }
    fn session_start(&self, relative: &str, session: &str, cwd: &Path) -> (String, String) {
        let event = json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": cwd,
            "hook_event_name": "SessionStart", "source": "startup",
        });
        self.hook(&self.installed(relative, "SessionStart"), cwd, &event)
    }
    /// A Git checkout with one commit at `relative` under the test base.
    fn repo(&self, relative: &str) -> PathBuf {
        let root = self.base.join(relative);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/a.rs"),
            format!("fn a() {{}} // {relative}\n"),
        )
        .unwrap();
        for args in [
            &["init", "--quiet"][..],
            &["add", "."],
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "start",
            ],
        ] {
            git(&root, args);
        }
        fs::canonicalize(root).unwrap()
    }
    fn workspaces(&self) -> Vec<Value> {
        self.json(&["workspace", "list"])["outcome"]["workspaces"]
            .as_array()
            .unwrap()
            .clone()
    }
    fn status(&self) -> Value {
        self.json(&["status"])["outcome"]["state"].clone()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Process::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn a_moved_checkout_keeps_its_notes_and_checks_and_its_hooks_find_it_again() {
    let mut home = Home::new();
    home.install();
    home.start();
    let old = home.repo("src/app");
    let id = user_workspace_id(&old);

    // Alice (Claude Code) and Bob (Codex) start in the checkout.
    let (_, coverage) = home.session_start(".claude/settings.json", ALICE, &old);
    assert_ne!(coverage, "unsupported");
    let (_, coverage) = home.session_start(".codex/hooks.json", BOB, &old);
    assert_ne!(coverage, "unsupported");
    home.ok(&["remember", &id, NOTE, "--request-id", "note-1"]);
    home.ok(&["check", "run", &id, "--name", "unit", "--", "true"]);
    let listed = home.workspaces();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["workspace_id"], id.as_str());
    assert_eq!(listed[0]["root_exists"], true);
    assert_eq!(listed[0]["sessions_current"], 2);
    assert_eq!(listed[0]["notes"], 1);
    assert_eq!(listed[0]["checks"], 1);

    // The owner moves the checkout on disk.
    fs::create_dir_all(home.base.join("work")).unwrap();
    let new = home.base.join("work/app");
    fs::rename(&old, &new).unwrap();
    assert_eq!(home.workspaces()[0]["root_exists"], false);

    let moved = home.ok(&[
        "workspace",
        "move",
        &id,
        text(&new),
        "--request-id",
        "ws-move-1",
    ]);
    assert!(
        moved.contains(&format!("Workspace {id} now at {}", new.display())),
        "{moved}"
    );
    assert!(moved.contains("Kept: 1 notes"), "{moved}");
    assert!(moved.contains("1 check records"), "{moved}");
    // User-level hooks need nothing rerun.
    assert!(!moved.contains("hooks install"), "{moved}");
    // The identical retry is answered from its receipt.
    let (code, again) = home.cli(&[
        "workspace",
        "move",
        &id,
        text(&new),
        "--request-id",
        "ws-move-1",
    ]);
    assert_eq!(code, 0, "{again}");
    assert!(again.contains("Original result replayed"), "{again}");

    let recap = &home.json(&["recap", &id])["outcome"]["recap"];
    assert_eq!(recap["root"], text(&new));
    assert_eq!(recap["notes"][0]["text"], NOTE);
    assert_eq!(recap["checks"].as_array().unwrap().len(), 1);
    let listed = home.workspaces();
    assert_eq!(listed[0]["root"], text(&new));
    assert_eq!(listed[0]["root_exists"], true);

    // Alice's next session from the new directory joins the same ID and is
    // told the same note; Bob's Codex session does too.
    let (briefing, _) = home.session_start(".claude/settings.json", ALICE_LATER, &new);
    assert!(briefing.contains(NOTE), "{briefing}");
    let (_, coverage) = home.session_start(".codex/hooks.json", BOB_LATER, &new);
    assert_ne!(coverage, "unsupported");
    let state = home.status();
    let workspaces = state["workspaces"].as_object().unwrap();
    assert_eq!(workspaces.len(), 1, "no second ID: {workspaces:?}");
    assert!(!workspaces.contains_key(&user_workspace_id(&new)));
    let sessions = home.sessions();
    for session in [
        format!("claude-code.{ALICE_LATER}"),
        format!("codex.{BOB_LATER}"),
    ] {
        assert_eq!(
            sessions.get(&session).map(|s| &s["workspace_id"]),
            Some(&json!(id)),
            "{session}: {sessions:?}"
        );
    }
}

#[test]
fn a_move_names_the_project_hook_installs_to_rerun_and_edits_none() {
    let mut home = Home::new();
    home.start();
    let old = home.repo("proj");
    for vendor in ["claude-code", "codex"] {
        home.ok(&[
            "hooks",
            "install",
            vendor,
            "--workspace",
            "app",
            "--root",
            text(&old),
            "--apply",
        ]);
    }
    home.ok(&["workspace", "add", "app", text(&old), "--request-id", "add"]);
    let settings = fs::read_to_string(old.join(".claude/settings.local.json"))
        .or_else(|_| fs::read_to_string(old.join(".claude/settings.json")))
        .unwrap();
    let new = home.base.join("moved");
    fs::rename(&old, &new).unwrap();
    let moved = home.ok(&["workspace", "move", "app", text(&new), "--request-id", "m"]);
    assert!(
        moved.contains("Project hooks name the old root; rerun:"),
        "{moved}"
    );
    for vendor in ["claude-code", "codex"] {
        assert!(
            moved.contains(&format!(
                "langspan hooks install {vendor} --workspace 'app' --root '{}'",
                new.display()
            )),
            "{vendor}: {moved}"
        );
    }
    let after = fs::read_to_string(new.join(".claude/settings.local.json"))
        .or_else(|_| fs::read_to_string(new.join(".claude/settings.json")))
        .unwrap();
    assert_eq!(after, settings, "host configuration is not edited");
}

#[test]
fn a_move_that_fails_a_check_says_which_and_changes_nothing() {
    let mut home = Home::new();
    home.start();
    let old = home.repo("app");
    home.ok(&["workspace", "add", "app", text(&old), "--request-id", "add"]);
    let before = home.workspaces();
    let refused = |args: &[&str], code: &str| {
        let output = home.json(args);
        assert_eq!(output["outcome"]["code"], code, "{args:?}: {output}");
        let (exit, _) = home.cli(args);
        assert_eq!(exit, 1);
    };

    // The old root still exists: a move is for a checkout that was moved.
    let clone = home.base.join("clone");
    git(&home.base, &["clone", "--quiet", text(&old), text(&clone)]);
    refused(
        &[
            "workspace",
            "move",
            "app",
            text(&clone),
            "--request-id",
            "m1",
        ],
        "old_root_exists",
    );
    // Not a checkout, and not the top of one.
    let plain = home.base.join("plain");
    fs::create_dir(&plain).unwrap();
    refused(
        &[
            "workspace",
            "move",
            "app",
            text(&plain),
            "--switch",
            "--request-id",
            "m2",
        ],
        "not_checkout_top",
    );
    refused(
        &[
            "workspace",
            "move",
            "app",
            text(&clone.join("src")),
            "--switch",
            "--request-id",
            "m3",
        ],
        "not_checkout_top",
    );
    // --switch to a clone of another repository.
    let stranger = home.repo("stranger");
    refused(
        &[
            "workspace",
            "move",
            "app",
            text(&stranger),
            "--switch",
            "--request-id",
            "m4",
        ],
        "different_repository",
    );
    // Onto another registered checkout of the same repository.
    let registered = home.base.join("registered");
    git(
        &home.base,
        &["clone", "--quiet", text(&old), text(&registered)],
    );
    home.ok(&[
        "workspace",
        "add",
        "other",
        text(&registered),
        "--request-id",
        "add-2",
    ]);
    refused(
        &[
            "workspace",
            "move",
            "app",
            text(&registered),
            "--switch",
            "--request-id",
            "m5",
        ],
        "workspace_conflict",
    );
    let after: Vec<Value> = home
        .workspaces()
        .into_iter()
        .filter(|w| w["workspace_id"] == "app")
        .collect();
    assert_eq!(after, before, "nothing changed");

    // A real clone of the same repository switches.
    let switched = home.ok(&[
        "workspace",
        "move",
        "app",
        text(&clone),
        "--switch",
        "--request-id",
        "m6",
    ]);
    assert!(switched.contains("now at"), "{switched}");
    let app = home
        .workspaces()
        .into_iter()
        .find(|w| w["workspace_id"] == "app")
        .unwrap();
    assert_eq!(app["root"], text(&fs::canonicalize(&clone).unwrap()));
    // The old checkout is no longer this ID's: it registers as a stranger.
    let (code, _) = home.cli(&[
        "workspace",
        "add",
        "app",
        text(&old),
        "--request-id",
        "add-3",
    ]);
    assert_eq!(code, 1);
}

#[test]
fn a_switch_away_from_the_checkout_its_id_was_derived_from_is_refused() {
    let mut home = Home::new();
    home.install();
    home.start();
    let old = home.repo("app");
    let id = user_workspace_id(&old);
    let (_, coverage) = home.session_start(".claude/settings.json", ALICE, &old);
    assert_ne!(coverage, "unsupported");
    let clone = home.base.join("clone");
    git(&home.base, &["clone", "--quiet", text(&old), text(&clone)]);
    let args = [
        "workspace",
        "move",
        &id,
        text(&clone),
        "--switch",
        "--request-id",
        "switch-1",
    ];
    let refused = home.json(&args);
    assert_eq!(refused["outcome"]["code"], "root_derived_id", "{refused}");
    let message = refused["outcome"]["message"].as_str().unwrap();
    assert!(message.contains(text(&old)), "{message}");
    assert!(message.contains("without --switch"), "{message}");
    assert_eq!(home.workspaces()[0]["root"], text(&old), "nothing changed");
    // A fresh session in the old checkout still joins it.
    let (_, coverage) = home.session_start(".claude/settings.json", ALICE_LATER, &old);
    assert_ne!(coverage, "unsupported");
    assert_eq!(
        home.sessions()
            .get(&format!("claude-code.{ALICE_LATER}"))
            .map(|s| &s["workspace_id"]),
        Some(&json!(id))
    );

    // Once the old checkout is gone, the clone takes the ID over by a move.
    fs::remove_dir_all(&old).unwrap();
    home.ok(&[
        "workspace",
        "move",
        &id,
        text(&clone),
        "--request-id",
        "move-1",
    ]);
    assert_eq!(
        home.workspaces()[0]["root"],
        text(&fs::canonicalize(&clone).unwrap())
    );
}

#[test]
fn removal_is_a_dry_run_until_applied_and_waits_for_admissions_and_sharing() {
    let mut home = Home::new();
    home.start();
    let root = home.repo("gone");
    let shared = home.repo("shared");
    home.ok(&[
        "workspace",
        "add",
        "gone",
        text(&root),
        "--request-id",
        "a1",
    ]);
    home.ok(&[
        "workspace",
        "add",
        "shared",
        text(&shared),
        "--request-id",
        "a2",
    ]);
    home.ok(&["session", "start", "s1", "gone", "--request-id", "s1"]);
    home.ok(&["session", "start", "s2", "shared", "--request-id", "s2"]);
    home.ok(&[
        "intent",
        "set",
        "s1",
        "src/a.rs",
        "--summary",
        "retry",
        "--request-id",
        "i1",
    ]);
    home.ok(&["remember", "gone", NOTE, "--request-id", "n1"]);
    let admitted = home.json(&[
        "admit",
        "s1",
        "src/a.rs",
        "--ttl-ms",
        "60000",
        "--request-id",
        "ad1",
    ]);
    let lease = admitted["outcome"]["lease"]["lease_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{admitted}"))
        .to_owned();

    // Held: refused, with or without --apply.
    for args in [
        &[
            "workspace",
            "remove",
            "gone",
            "--apply",
            "--request-id",
            "r1",
        ][..],
        &["workspace", "remove", "gone"],
    ] {
        let refused = home.json(args);
        assert_eq!(refused["outcome"]["code"], "workspace_busy", "{refused}");
        assert!(
            refused["outcome"]["message"]
                .as_str()
                .unwrap()
                .contains("s1")
        );
    }
    home.ok(&["complete", "s1", &lease, "--request-id", "c1"]);

    // A dry run lists what would end and changes nothing.
    let status_before = home.status();
    let dry = home.ok(&["workspace", "remove", "gone"]);
    assert!(dry.contains("Dry run, nothing changed"), "{dry}");
    assert!(dry.contains("1 sessions (s1), 1 intents, 1 notes"), "{dry}");
    assert_eq!(home.status()["workspaces"], status_before["workspaces"]);
    assert_eq!(home.status()["sessions"], status_before["sessions"]);

    // Remote sharing on: refused, naming the command to run first.
    home.ok(&[
        "remote",
        "enable",
        "shared",
        "--name",
        "Alice",
        "--request-id",
        "re",
    ]);
    let refused = home.json(&[
        "workspace",
        "remove",
        "shared",
        "--apply",
        "--request-id",
        "r2",
    ]);
    assert_eq!(refused["outcome"]["code"], "remote_sharing_on");
    assert!(
        refused["outcome"]["message"]
            .as_str()
            .unwrap()
            .contains("langspan remote disable shared"),
        "{refused}"
    );

    let removed = home.ok(&[
        "workspace",
        "remove",
        "gone",
        "--apply",
        "--request-id",
        "r3",
    ]);
    assert!(removed.contains("Workspace gone removed"), "{removed}");
    let state = home.status();
    assert!(state["workspaces"].get("gone").is_none(), "{state}");
    assert!(state["sessions"].get("s1").is_none(), "{state}");
    assert!(
        home.workspaces()
            .iter()
            .all(|w| w["workspace_id"] != "gone")
    );
    let briefed = home.json(&["brief", "s1", "--request-id", "b1"]);
    assert_eq!(briefed["outcome"]["code"], "session_not_found");
    let recap = home.json(&["recap", "gone"]);
    assert_eq!(recap["outcome"]["type"], "error", "{recap}");
    // The history stays.
    let events = home.json(&["events", "--limit", "1000"]);
    let types: Vec<&str> = events["outcome"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["event"]["workspace"]["workspace_id"] == "gone"
                || e["event"]["workspace_id"] == "gone"
                || e["event"]["note"]["workspace_id"] == "gone"
        })
        .map(|e| e["event"]["type"].as_str().unwrap())
        .collect();
    for kind in ["workspace_registered", "remembered", "workspace_removed"] {
        assert!(types.contains(&kind), "{kind} in {types:?}");
    }
    // The day's account reads that history without the removed records.
    let today = home.json(&["today"]);
    assert_eq!(today["outcome"]["type"], "today", "{today}");
    // The daemon recovers the removal from its log.
    home.daemon.take().map(|mut child| {
        child.kill().unwrap();
        child.wait().unwrap()
    });
    home.start();
    assert!(home.status()["workspaces"].get("gone").is_none());
}
