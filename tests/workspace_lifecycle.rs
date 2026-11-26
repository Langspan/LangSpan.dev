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
    fn restart(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        self.start();
    }
    /// Wait until a completed scan of `workspace` has seen HEAD at `head`.
    fn wait_for_scan(&self, workspace: &str, head: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let changes = self.json(&["changes", workspace, "--after", "0", "--limit", "1"]);
            if changes["outcome"]["scan"]["head_oid"] == head {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no scan of {head}: {changes} {}",
                self.status()["scan_failures"]
            );
            std::thread::sleep(Duration::from_millis(50));
        }
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

/// More workspaces than one frame holds are listed page by page, each page
/// within a frame, every workspace once, in ID order.
#[test]
fn workspace_list_pages_with_a_cursor_however_many_workspaces_there_are() {
    let mut home = Home::new();
    home.start();
    // Long roots, so that all of them in one answer would exceed a frame.
    let long = "d".repeat(200);
    let parent = home.base.join([long.as_str(); 4].join("/"));
    let ids: Vec<String> = (0..80).map(|n| format!("w{n:03}")).collect();
    for id in &ids {
        let root = parent.join(id);
        fs::create_dir_all(&root).unwrap();
        home.ok(&[
            "workspace",
            "add",
            id,
            text(&root),
            "--request-id",
            &format!("add-{id}"),
        ]);
    }
    let (mut seen, mut after, mut pages) = (Vec::new(), None::<String>, 0);
    loop {
        let mut args = vec!["workspace", "list", "--json"];
        if let Some(cursor) = &after {
            args.extend(["--after", cursor]);
        }
        let output = home.run(&args);
        assert!(
            output.stdout.len() < langspan::protocol::MAX_FRAME_BYTES,
            "a page of {} bytes",
            output.stdout.len()
        );
        let page: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stdout)));
        assert_eq!(page["outcome"]["type"], "workspaces", "{page}");
        let listed = page["outcome"]["workspaces"].as_array().unwrap();
        assert!(!listed.is_empty(), "{page}");
        seen.extend(
            listed
                .iter()
                .map(|w| w["workspace_id"].as_str().unwrap().to_owned()),
        );
        pages += 1;
        assert!(pages < 40, "paging does not end");
        match page["outcome"]["next"].as_str() {
            Some(next) => after = Some(next.to_owned()),
            None => break,
        }
    }
    assert!(pages > 1, "all {} in one page", seen.len());
    assert_eq!(seen, ids);
    // The text form names the command for the next page.
    let first = home.ok(&["workspace", "list"]);
    assert!(
        first.contains("More: langspan workspace list --after "),
        "{first}"
    );
    // A cursor is a workspace ID; anything else is refused, not guessed at.
    let (code, refused) = home.cli(&["workspace", "list", "--after", "not an id"]);
    assert_eq!(code, 1, "{refused}");
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

/// The checkout's directory moves to another disk and a symbolic link takes
/// its place (issue #170). Every path LangSpan holds still names the same
/// directory, so it keeps observing it under the registered path.
#[test]
fn a_checkout_replaced_by_a_link_to_its_new_place_is_still_the_same_checkout() {
    let mut home = Home::new();
    home.install();
    home.start();
    let old = home.repo("disk1/app");
    home.ok(&["workspace", "add", "app", text(&old), "--request-id", "add"]);
    let first = head(&old);
    home.wait_for_scan("app", &first);

    fs::create_dir(home.base.join("disk2")).unwrap();
    let new = home.base.join("disk2/app");
    fs::rename(&old, &new).unwrap();
    std::os::unix::fs::symlink(&new, &old).unwrap();
    // The next coordinator start, as after the move that found this.
    home.restart();
    commit(&new, "src/b.rs");
    let second = head(&new);
    assert_ne!(first, second);
    home.wait_for_scan("app", &second);
    let state = home.status();
    assert!(
        state["scan_failures"].get("app").is_none(),
        "{}",
        state["scan_failures"]
    );
    assert_eq!(home.workspaces()[0]["root"], text(&old), "kept as given");

    // A session reached through the link joins the registered workspace,
    // and registering the link path again is the same registration.
    let (_, coverage) = home.session_start(".claude/settings.json", ALICE, &old);
    assert_ne!(coverage, "unsupported");
    assert_eq!(
        home.sessions()
            .get(&format!("claude-code.{ALICE}"))
            .map(|s| &s["workspace_id"]),
        Some(&json!("app"))
    );
    home.ok(&[
        "workspace",
        "add",
        "app",
        text(&old),
        "--request-id",
        "add-again",
    ]);
    assert_eq!(home.workspaces().len(), 1, "no second workspace");
    home.ok(&["remember", "app", NOTE, "--request-id", "note"]);
    // A session there is admitted through the link.
    home.ok(&["session", "start", "s1", "app", "--request-id", "s1"]);
    let admitted = home.json(&[
        "admit",
        "s1",
        "src/a.rs",
        "--ttl-ms",
        "60000",
        "--request-id",
        "admit",
    ]);
    assert_eq!(admitted["lease_valid"], true, "{admitted}");
    let lease = admitted["outcome"]["lease"]["lease_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{admitted}"))
        .to_owned();
    // Finish it before the move: a move waits for held admissions. A
    // completion, not a lapse, so that no step depends on how fast the
    // machine is.
    home.ok(&["complete", "s1", &lease, "--request-id", "complete"]);

    // Moving to the link's target needs no --switch and records it.
    home.ok(&[
        "workspace",
        "move",
        "app",
        text(&new),
        "--request-id",
        "move",
    ]);
    let listed = home.workspaces();
    assert_eq!(listed[0]["root"], text(&new));
    assert_eq!(listed[0]["notes"], 1);
    commit(&new, "src/c.rs");
    home.wait_for_scan("app", &head(&new));
    assert!(home.status()["scan_failures"].get("app").is_none());
}

/// A checkout registered through a link from the start, as a home directory
/// reached through one is, scans normally and its sessions join it.
#[test]
fn a_checkout_registered_through_a_link_scans_normally() {
    let mut home = Home::new();
    home.install();
    home.start();
    let real = home.repo("real/app");
    let link = home.base.join("link");
    std::os::unix::fs::symlink(home.base.join("real"), &link).unwrap();
    let through = link.join("app");
    home.ok(&[
        "workspace",
        "add",
        "app",
        text(&through),
        "--request-id",
        "add",
    ]);
    home.wait_for_scan("app", &head(&real));
    commit(&real, "src/b.rs");
    home.wait_for_scan("app", &head(&real));
    assert!(home.status()["scan_failures"].get("app").is_none());
    let (_, coverage) = home.session_start(".claude/settings.json", ALICE, &through);
    assert_ne!(coverage, "unsupported");
    assert_eq!(
        home.sessions()
            .get(&format!("claude-code.{ALICE}"))
            .map(|s| &s["workspace_id"]),
        Some(&json!("app"))
    );
    assert_eq!(home.workspaces().len(), 1, "no second workspace");
}

/// An old root that is now a link to another checkout is still there: the
/// refusal says it is a link and names where it points, and `--switch`
/// still moves the workspace to a different checkout.
#[test]
fn a_move_refused_because_the_old_root_is_a_link_says_where_it_points() {
    let mut home = Home::new();
    home.start();
    let old = home.repo("app");
    home.ok(&["workspace", "add", "app", text(&old), "--request-id", "add"]);
    let elsewhere = home.base.join("elsewhere");
    fs::rename(&old, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &old).unwrap();
    let clone = home.base.join("clone");
    git(
        &home.base,
        &["clone", "--quiet", text(&elsewhere), text(&clone)],
    );
    let refused = home.json(&[
        "workspace",
        "move",
        "app",
        text(&clone),
        "--request-id",
        "m1",
    ]);
    assert_eq!(refused["outcome"]["code"], "old_root_exists", "{refused}");
    let message = refused["outcome"]["message"].as_str().unwrap();
    assert!(message.contains("symbolic link"), "{message}");
    assert!(message.contains(text(&elsewhere)), "{message}");
    assert_eq!(home.workspaces()[0]["root"], text(&old), "nothing changed");
    home.ok(&[
        "workspace",
        "move",
        "app",
        text(&clone),
        "--switch",
        "--request-id",
        "m2",
    ]);
    assert_eq!(
        home.workspaces()[0]["root"],
        text(&fs::canonicalize(&clone).unwrap())
    );
}

/// A link at the registered root to a checkout of another repository is not
/// the same checkout: the scan stops with the identity change, as a
/// replacement directory there would.
#[test]
fn a_link_to_another_repository_at_the_registered_root_is_an_identity_change() {
    let mut home = Home::new();
    home.start();
    let old = home.repo("app");
    home.ok(&["workspace", "add", "app", text(&old), "--request-id", "add"]);
    home.wait_for_scan("app", &head(&old));
    let stranger = home.repo("stranger");
    fs::rename(&old, home.base.join("aside")).unwrap();
    std::os::unix::fs::symlink(&stranger, &old).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let failure = home.status()["scan_failures"]["app"]["detail"].clone();
        if failure
            .as_str()
            .is_some_and(|detail| detail.contains("Git identity changed"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "no identity change: {failure}");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Nor is a session there admitted through the link.
    home.ok(&["session", "start", "s1", "app", "--request-id", "s1"]);
    let refused = home.json(&["admit", "s1", "src/a.rs", "--request-id", "admit"]);
    assert_eq!(refused["outcome"]["type"], "error", "{refused}");
    assert!(
        refused["outcome"]["message"]
            .as_str()
            .unwrap()
            .contains("identity changed"),
        "{refused}"
    );
    let changes = home.json(&["changes", "app", "--after", "0", "--limit", "1"]);
    assert_ne!(
        changes["outcome"]["scan"]["head_oid"],
        head(&stranger).as_str(),
        "{changes}"
    );
}

/// A registered root that is now a link to the checkout's new place still
/// holds that checkout: a second ID for the link's target, by registration
/// or by moving another workspace there, is refused, so two workspaces never
/// admit the same physical file.
#[test]
fn a_second_id_for_the_checkout_a_registered_link_leads_to_is_refused() {
    let mut home = Home::new();
    home.start();
    let old = home.repo("disk1/app");
    home.ok(&["workspace", "add", "a", text(&old), "--request-id", "add-a"]);
    fs::create_dir(home.base.join("disk2")).unwrap();
    let new = home.base.join("disk2/app");
    fs::rename(&old, &new).unwrap();
    std::os::unix::fs::symlink(&new, &old).unwrap();

    let refused = home.json(&["workspace", "add", "b", text(&new), "--request-id", "add-b"]);
    assert_eq!(refused["outcome"]["type"], "error", "{refused}");
    assert!(
        refused["outcome"]["message"]
            .as_str()
            .unwrap()
            .contains("overlaps an existing physical workspace"),
        "{refused}"
    );
    // Nor inside it.
    let refused = home.json(&[
        "workspace",
        "add",
        "b",
        text(&new.join("src")),
        "--request-id",
        "add-b-src",
    ]);
    assert_eq!(refused["outcome"]["type"], "error", "{refused}");

    // Nor by moving another workspace onto the link's target.
    let other = home.repo("other");
    home.ok(&[
        "workspace",
        "add",
        "c",
        text(&other),
        "--request-id",
        "add-c",
    ]);
    fs::remove_dir_all(&other).unwrap();
    let refused = home.json(&[
        "workspace",
        "move",
        "c",
        text(&new),
        "--request-id",
        "move-c",
    ]);
    assert_eq!(
        refused["outcome"]["code"], "workspace_conflict",
        "{refused}"
    );

    let ids: Vec<_> = home
        .workspaces()
        .iter()
        .map(|w| w["workspace_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["a", "c"]);
    // Through the link, the registration of `a` is unchanged.
    home.ok(&[
        "workspace",
        "add",
        "a",
        text(&old),
        "--request-id",
        "add-a-again",
    ]);
}

/// After a checkout moves and a link takes its place, its linked worktrees
/// stay registrable: a new one under the new place, and one registered
/// under the old root that moves to its resolved path. The registered
/// parent's root is still the link, so the two roots do not overlap as text.
/// While the parent holds an admission, the new registration waits, since
/// the core cannot tell which of its paths the worktree covers.
#[test]
fn a_linked_worktree_of_a_checkout_moved_behind_a_link_registers() {
    let mut home = Home::new();
    home.start();
    let old = home.repo("disk1/app");
    git(&old, &["worktree", "add", "--quiet", ".worktrees/third"]);
    home.ok(&["workspace", "add", "a", text(&old), "--request-id", "add-a"]);
    home.ok(&[
        "workspace",
        "add",
        "third",
        text(&old.join(".worktrees/third")),
        "--request-id",
        "add-third",
    ]);
    fs::create_dir(home.base.join("disk2")).unwrap();
    let new = home.base.join("disk2/app");
    fs::rename(&old, &new).unwrap();
    std::os::unix::fs::symlink(&new, &old).unwrap();
    git(&new, &["worktree", "add", "--quiet", ".worktrees/second"]);
    let second = new.join(".worktrees/second");

    // A held parent admission makes the registration wait.
    home.ok(&["session", "start", "s1", "a", "--request-id", "s1"]);
    let admitted = home.json(&[
        "admit",
        "s1",
        "src/a.rs",
        "--ttl-ms",
        "60000",
        "--request-id",
        "admit",
    ]);
    let lease = admitted["outcome"]["lease"]["lease_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{admitted}"))
        .to_owned();
    let waiting = home.json(&[
        "workspace",
        "add",
        "second",
        text(&second),
        "--request-id",
        "add-second-held",
    ]);
    assert_eq!(waiting["outcome"]["code"], "workspace_in_use", "{waiting}");
    home.ok(&["complete", "s1", &lease, "--request-id", "complete"]);

    home.ok(&[
        "workspace",
        "add",
        "second",
        text(&second),
        "--request-id",
        "add-second",
    ]);
    home.ok(&[
        "workspace",
        "move",
        "third",
        text(&new.join(".worktrees/third")),
        "--request-id",
        "move-third",
    ]);
    let roots: Vec<_> = home
        .workspaces()
        .iter()
        .map(|w| w["root"].as_str().unwrap().to_owned())
        .collect();
    assert!(roots.contains(&text(&second).to_owned()), "{roots:?}");
    assert!(
        roots.contains(&text(&new.join(".worktrees/third")).to_owned()),
        "{roots:?}"
    );
    // The recorded links replay: a restart reads the same registrations.
    home.restart();
    assert_eq!(home.workspaces().len(), 3);
}

/// Review finding: after a checkout moves behind a link, its registered
/// root stays the link while a linked worktree registers at its resolved
/// place inside. The parent must still refuse that worktree's files, so the
/// two workspaces never admit one physical file under different IDs.
#[test]
fn a_parent_behind_a_link_refuses_its_registered_worktree_files() {
    let mut home = Home::new();
    home.start();
    let old = home.repo("disk1/app");
    home.ok(&["workspace", "add", "a", text(&old), "--request-id", "add-a"]);
    fs::create_dir(home.base.join("disk2")).unwrap();
    let new = home.base.join("disk2/app");
    fs::rename(&old, &new).unwrap();
    std::os::unix::fs::symlink(&new, &old).unwrap();
    git(&new, &["worktree", "add", "--quiet", ".worktrees/child"]);
    let child = new.join(".worktrees/child");
    home.ok(&[
        "workspace",
        "add",
        "child",
        text(&child),
        "--request-id",
        "add-child",
    ]);
    fs::write(child.join("file"), "x\n").unwrap();
    // Without its `.git` file, only the registration marks the worktree,
    // so the nested-checkout check cannot refuse its files instead.
    fs::remove_file(child.join(".git")).unwrap();
    home.ok(&["session", "start", "s1", "a", "--request-id", "s1"]);
    let refused = home.json(&[
        "admit",
        "s1",
        ".worktrees/child/file",
        "--ttl-ms",
        "60000",
        "--request-id",
        "admit-child-file",
    ]);
    assert!(
        refused["outcome"]["lease"]["lease_id"].is_null(),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("registered descendant workspace"),
        "{refused}"
    );
}

fn head(root: &Path) -> String {
    let output = Process::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Commit a new file at `relative` in the checkout at `root`.
fn commit(root: &Path, relative: &str) {
    fs::write(root.join(relative), "fn added() {}\n").unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--quiet",
            "-m",
            relative,
        ],
    );
}
