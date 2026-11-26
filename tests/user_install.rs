#![cfg(unix)]
//! `langspan install` / `uninstall` against a temporary home, and the hook
//! command they write, run as the host would run it, against a real daemon.

#[allow(dead_code)] // Only `output_once_not_busy` and `in_home` are used here.
mod support;

use langspan::{
    client,
    hook::user_workspace_id,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{Command, Outcome};
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
const SESSION: &str = "00000000-0000-4000-8000-0000000000c1";

const CLAUDE_SETTINGS: &str = r#"{
  "theme": "dark",
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/x/guard.sh"
          }
        ]
      }
    ]
  },
  "model": "m"
}
"#;
const CLAUDE_JSON: &str = r#"{
  "numStartups": 4,
  "mcpServers": {
    "docs": {
      "type": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "docs"
      ]
    }
  },
  "projects": {}
}
"#;
const CODEX_HOOKS: &str = r#"{
  "description": "mine",
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "say done"
          }
        ]
      }
    ]
  }
}
"#;
const CODEX_CONFIG: &str =
    "model = \"o3\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs\"]\n";

struct Home {
    base: PathBuf,
    daemon: Option<Child>,
}

impl Home {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "ls-user-{}-{}",
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
    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.home().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
    fn claude_and_codex(&self) {
        self.write(".claude/settings.json", CLAUDE_SETTINGS);
        self.write(".claude.json", CLAUDE_JSON);
        self.write(".codex/hooks.json", CODEX_HOOKS);
        self.write(".codex/config.toml", CODEX_CONFIG);
    }
    fn files(&self) -> Vec<(PathBuf, Option<String>)> {
        [
            ".claude/settings.json",
            ".claude.json",
            ".codex/hooks.json",
            ".codex/config.toml",
        ]
        .iter()
        .map(|relative| {
            let path = self.home().join(relative);
            let text = fs::read_to_string(&path).ok();
            (path, text)
        })
        .collect()
    }
    fn langspan(&self, args: &[&str]) -> Output {
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(args)
            .arg("--home")
            .arg(self.home())
            .arg("--state-dir")
            .arg(self.state_dir())
            .env_remove("CLAUDE_PROJECT_DIR")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.langspan(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
    fn start(&mut self) {
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&self.state_dir(), &status()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// The installed command for one event, exactly as written to the file.
    fn installed_command(&self, relative: &str, event: &str) -> String {
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
    /// Run a hook command through the shell, as the host does, with `cwd`
    /// as its working directory. Returns stdout and the logged coverage.
    fn run_hook(&self, command: &str, cwd: &Path, event: &Value) -> (String, String) {
        let mut child = Process::new("/bin/sh")
            .env("LANGSPAN_AUTOSTART", "off")
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
    fn repo(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        let status = Process::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        fs::canonicalize(root).unwrap()
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

fn status() -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Status,
    }
}

fn edit(cwd: &Path, file: &Path, tool_use_id: &str) -> Value {
    json!({
        "session_id": SESSION,
        "transcript_path": "/dev/null",
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {
            "file_path": file,
            "old_string": "a",
            "new_string": "b",
            "replace_all": false
        },
        "tool_use_id": tool_use_id,
    })
}

#[test]
fn dry_run_writes_nothing_and_apply_then_uninstall_round_trips_every_file() {
    let home = Home::new();
    home.claude_and_codex();
    fs::set_permissions(
        home.home().join(".claude.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let before = home.files();

    let dry = home.ok(&["install"]);
    assert!(dry.contains("Dry run; nothing was written"), "{dry}");
    assert!(dry.contains("[mcp_servers.langspan]"), "{dry}");
    // The host's own state in ~/.claude.json is not echoed back.
    assert!(!dry.contains("numStartups"), "{dry}");
    assert!(dry.contains("only this entry changes"), "{dry}");
    assert_eq!(home.files(), before);
    assert!(!home.state_dir().exists());

    home.ok(&["install", "--apply"]);
    let installed = home.files();
    for ((path, old), (_, new)) in before.iter().zip(&installed) {
        assert_ne!(old, new, "{} was not changed", path.display());
    }
    let claude: Value = serde_json::from_str(installed[1].1.as_ref().unwrap()).unwrap();
    let keys: Vec<_> = claude.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["numStartups", "mcpServers", "projects"]);
    assert_eq!(claude["mcpServers"]["docs"]["command"], "npx");
    assert_eq!(
        claude["mcpServers"]["langspan"]["args"],
        json!(["mcp", "--state-dir", home.state_dir()])
    );
    let executable = claude["mcpServers"]["langspan"]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(Path::new(&executable).is_file());
    assert_eq!(
        fs::metadata(home.home().join(".claude.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let toml = installed[3].1.as_ref().unwrap();
    assert!(toml.starts_with(CODEX_CONFIG), "{toml}");
    assert!(
        toml.contains(&format!("command = \"{executable}\"")),
        "{toml}"
    );
    let pre = home.installed_command(".claude/settings.json", "PreToolUse");
    assert!(
        !pre.contains("--workspace") && !pre.contains("--root"),
        "{pre}"
    );

    let again = home.ok(&["install", "--apply"]);
    assert_eq!(again.matches("already matches").count(), 4, "{again}");
    assert_eq!(home.files(), installed);

    // The user's state is never part of what uninstall removes.
    fs::create_dir_all(home.state_dir()).unwrap();
    let dry = home.ok(&["uninstall"]);
    assert!(dry.contains("Dry run"), "{dry}");
    assert_eq!(home.files(), installed);
    home.ok(&["uninstall", "--apply"]);
    assert_eq!(
        home.files(),
        before,
        "uninstall restores every file exactly"
    );
    assert!(home.state_dir().is_dir());
    let nothing = home.ok(&["uninstall", "--apply"]);
    assert_eq!(nothing.matches("already matches").count(), 4, "{nothing}");
}

#[test]
fn default_hosts_are_those_present_and_refusals_write_nothing() {
    let home = Home::new();
    home.write(".codex/config.toml", CODEX_CONFIG);
    home.ok(&["install", "--apply"]);
    assert!(!home.home().join(".claude").exists());
    assert!(home.home().join(".codex/hooks.json").is_file());
    home.ok(&["uninstall", "--apply"]);

    let empty = Home::new();
    let output = empty.langspan(&["install"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--hosts"));

    // A foreign `langspan` server stops the whole install, hooks included.
    let foreign = Home::new();
    foreign.claude_and_codex();
    foreign.write(
        ".claude.json",
        r#"{"mcpServers":{"langspan":{"command":"/somewhere/else","args":[]}}}"#,
    );
    let before = foreign.files();
    let output = foreign.langspan(&["install", "--apply"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not written by LangSpan"),
        "{output:?}"
    );
    assert_eq!(foreign.files(), before);

    let table = Home::new();
    table.claude_and_codex();
    table.write(
        ".codex/config.toml",
        "[mcp_servers.langspan]\ncommand = \"/mine\"\n",
    );
    let before = table.files();
    let output = table.langspan(&["install", "--apply"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("did not write"));
    assert_eq!(table.files(), before);

    let linked = Home::new();
    linked.claude_and_codex();
    let target = linked.base.join("real-settings.json");
    fs::write(&target, CLAUDE_SETTINGS).unwrap();
    fs::remove_file(linked.home().join(".claude/settings.json")).unwrap();
    std::os::unix::fs::symlink(&target, linked.home().join(".claude/settings.json")).unwrap();
    let output = linked.langspan(&["install", "--apply"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("symlink"));
    assert_eq!(fs::read_to_string(&target).unwrap(), CLAUDE_SETTINGS);
    assert_eq!(
        fs::read_to_string(linked.home().join(".claude.json")).unwrap(),
        CLAUDE_JSON
    );
}

#[test]
fn the_installed_user_level_hook_coordinates_any_repository_by_its_cwd() {
    let mut home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--apply"]);
    home.start();
    let pre = home.installed_command(".claude/settings.json", "PreToolUse");

    let repo = home.repo("project");
    let (stdout, coverage) = home.run_hook(
        &pre,
        &repo,
        &edit(&repo.join("src"), &repo.join("src/a.rs"), "toolu_u1"),
    );
    assert_eq!((coverage.as_str(), stdout.as_str()), ("admitted", ""));
    let Outcome::Status { state, .. } = client::call(&home.state_dir(), &status()).unwrap().outcome
    else {
        panic!("status expected");
    };
    let id = user_workspace_id(&repo);
    langspan_core::validate_request_id(&id).unwrap();
    assert_eq!(id, user_workspace_id(&repo), "derived deterministically");
    assert_eq!(state.workspaces[&id].root, repo.to_string_lossy());

    // A working directory outside any repository: no decision, no output.
    let outside = home.base.join("plain");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("x.txt"), "x\n").unwrap();
    let (stdout, coverage) = home.run_hook(
        &pre,
        &outside,
        &edit(&outside, &outside.join("x.txt"), "toolu_u2"),
    );
    assert_eq!((coverage.as_str(), stdout.as_str()), ("unsupported", ""));

    // The doctor reports the user-level install for both hosts.
    let report = home.langspan(&["doctor"]);
    let text = String::from_utf8(report.stdout).unwrap();
    assert!(
        text.contains("ok claude-code: user-level hooks installed"),
        "{text}"
    );
    assert!(
        text.contains("ok claude-code: MCP server `langspan` registered"),
        "{text}"
    );
    assert!(
        text.contains("ok codex: MCP server `langspan` registered"),
        "{text}"
    );
}

#[test]
fn a_repository_with_a_project_install_is_left_to_that_install() {
    let mut home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--apply"]);
    home.start();
    let repo = home.repo("covered");
    for vendor in ["claude-code", "codex"] {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args([
                "hooks",
                "install",
                vendor,
                "--workspace",
                "covered",
                "--apply",
            ])
            .arg("--root")
            .arg(&repo)
            .arg("--state-dir")
            .arg(home.state_dir())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let claude = home.installed_command(".claude/settings.json", "PreToolUse");
    let (stdout, coverage) = home.run_hook(
        &claude,
        &repo,
        &edit(&repo, &repo.join("src/a.rs"), "toolu_p1"),
    );
    assert_eq!(
        (coverage.as_str(), stdout.as_str()),
        ("deferred_to_project", "")
    );
    let codex = home.installed_command(".codex/hooks.json", "SessionStart");
    let start = json!({
        "session_id": SESSION,
        "hook_event_name": "SessionStart",
        "cwd": repo.join("src"),
        "source": "startup",
    });
    let (stdout, coverage) = home.run_hook(&codex, &repo, &start);
    assert_eq!(
        (coverage.as_str(), stdout.as_str()),
        ("deferred_to_project", "")
    );
    let Outcome::Status { state, .. } = client::call(&home.state_dir(), &status()).unwrap().outcome
    else {
        panic!("status expected");
    };
    assert!(
        !state.workspaces.contains_key(&user_workspace_id(&repo)),
        "the user-level hook registered nothing"
    );
}

#[test]
fn a_user_level_hook_adopts_the_id_a_project_install_already_registered() {
    // Review finding: Claude Code keeps its project install while Codex uses
    // the user-level hooks. The repository is registered as "covered"; the
    // user-level hook's derived ID must not make every Codex event fail.
    let mut home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--apply"]);
    home.start();
    let repo = home.repo("mixed");
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args([
            "hooks",
            "install",
            "claude-code",
            "--workspace",
            "covered",
            "--apply",
        ])
        .arg("--root")
        .arg(&repo)
        .arg("--state-dir")
        .arg(home.state_dir())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The project-level Claude Code hook registers the repository first.
    let project = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["workspace", "add", "covered"])
        .arg(&repo)
        .args(["--request-id", "ws-covered", "--state-dir"])
        .arg(home.state_dir())
        .output()
        .unwrap();
    assert!(project.status.success(), "{project:?}");
    let codex = home.installed_command(".codex/hooks.json", "SessionStart");
    let start = json!({
        "session_id": SESSION,
        "hook_event_name": "SessionStart",
        "cwd": repo,
        "source": "startup",
    });
    let (stdout, coverage) = home.run_hook(&codex, &repo, &start);
    assert_ne!(coverage, "unknown", "{stdout}");
    assert_ne!(coverage, "unsupported", "{stdout}");
    let Outcome::Status { state, .. } = client::call(&home.state_dir(), &status()).unwrap().outcome
    else {
        panic!("status expected");
    };
    assert!(
        !state.workspaces.contains_key(&user_workspace_id(&repo)),
        "no second ID"
    );
    let session = state
        .sessions
        .values()
        .find(|session| session.session_id.starts_with("codex."))
        .expect("the Codex session is registered");
    assert_eq!(
        session.workspace_id, "covered",
        "it joined the existing workspace"
    );
}

#[test]
fn a_user_install_registers_the_end_of_reply_events_and_doctor_names_any_left_out() {
    let home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--apply"]);
    for (file, events) in [
        (
            ".claude/settings.json",
            &["Stop", "StopFailure", "SubagentStop", "PostToolBatch"][..],
        ),
        (".codex/hooks.json", &["Stop", "SessionEnd"][..]),
    ] {
        for event in events {
            assert!(
                home.installed_command(file, event).contains("hook"),
                "{file} {event}"
            );
        }
    }
    // The owner's own Codex Stop hook is kept, ahead of LangSpan's.
    let codex: Value =
        serde_json::from_str(&fs::read_to_string(home.home().join(".codex/hooks.json")).unwrap())
            .unwrap();
    assert_eq!(codex["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");

    // A user-level install from before these events existed.
    let path = home.home().join(".codex/hooks.json");
    let mut older = codex.clone();
    older["hooks"]["Stop"].as_array_mut().unwrap().truncate(1);
    older["hooks"].as_object_mut().unwrap().remove("SessionEnd");
    fs::write(&path, older.to_string()).unwrap();
    let text = String::from_utf8(home.langspan(&["doctor"]).stdout).unwrap();
    assert!(
        text.contains("FIX codex: user-level hooks installed")
            && text.contains("not for Stop, SessionEnd")
            && text.contains("run `langspan install --hosts codex --apply` again"),
        "{text}"
    );
    home.ok(&["install", "--apply"]);
    let text = String::from_utf8(home.langspan(&["doctor"]).stdout).unwrap();
    assert!(
        text.contains("ok codex: user-level hooks installed"),
        "{text}"
    );
    // Uninstall takes every entry it wrote, the new ones included.
    home.ok(&["uninstall", "--apply"]);
    let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after, serde_json::from_str::<Value>(CODEX_HOOKS).unwrap());
    let claude: Value = serde_json::from_str(
        &fs::read_to_string(home.home().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        claude,
        serde_json::from_str::<Value>(CLAUDE_SETTINGS).unwrap()
    );
}

/// Gemini CLI's user settings, with a credential in an MCP server's `env`
/// that a dry run must not print.
const GEMINI_SETTINGS: &str = r#"{
  "general": {
    "vimMode": true
  },
  "hooks": {
    "BeforeTool": [
      {
        "matcher": "write_file",
        "hooks": [
          {
            "name": "guard",
            "type": "command",
            "command": "/x/guard.sh",
            "timeout": 2000
          }
        ]
      }
    ]
  },
  "mcpServers": {
    "docs": {
      "command": "npx",
      "args": [
        "docs"
      ],
      "env": {
        "DOCS_TOKEN": "not-a-real-token-but-shaped-like-one"
      }
    }
  }
}
"#;

#[test]
fn gemini_settings_get_hooks_and_the_mcp_server_in_one_guarded_write_and_lose_them_exactly() {
    let mut home = Home::new();
    let settings = home.write(".gemini/settings.json", GEMINI_SETTINGS);
    // Without `--hosts`, Gemini CLI is not chosen: its adapter is not yet
    // verified on a live session, and other tools also make `~/.gemini`.
    assert!(!home.langspan(&["install"]).status.success());
    let dry = home.ok(&["install", "--hosts", "gemini-cli"]);
    assert!(dry.contains("Dry run; nothing was written"), "{dry}");
    assert!(dry.contains("LangSpan's hook on 5 event(s)"), "{dry}");
    assert!(dry.contains(r#""mcpServers": { "langspan""#), "{dry}");
    assert!(
        !dry.contains("not-a-real-token"),
        "the owner's settings are not shown: {dry}"
    );
    assert!(!home.home().join(".claude").exists() && !home.home().join(".codex").exists());
    assert_eq!(fs::read_to_string(&settings).unwrap(), GEMINI_SETTINGS);

    let applied = home.ok(&["install", "--hosts", "gemini-cli", "--apply"]);
    assert_eq!(
        applied.matches("Wrote").count(),
        1,
        "one file, one write: {applied}"
    );
    let installed = fs::read_to_string(&settings).unwrap();
    let value: Value = serde_json::from_str(&installed).unwrap();
    assert_eq!(value["general"]["vimMode"], true);
    assert_eq!(
        value["hooks"]["BeforeTool"][0]["hooks"][0]["command"],
        "/x/guard.sh"
    );
    assert_eq!(
        value["mcpServers"]["docs"]["env"]["DOCS_TOKEN"],
        "not-a-real-token-but-shaped-like-one"
    );
    assert_eq!(
        value["mcpServers"]["langspan"]["args"],
        json!(["mcp", "--state-dir", home.state_dir()])
    );
    assert!(value["mcpServers"]["langspan"].get("trust").is_none());
    let again = home.ok(&["install", "--hosts", "gemini-cli", "--apply"]);
    assert_eq!(again.matches("already matches").count(), 1, "{again}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), installed);

    // The installed command, run as Gemini CLI runs it, coordinates the
    // repository it is started in and always answers with a JSON object.
    home.start();
    let repo = home.repo("project");
    let command = home.installed_command(".gemini/settings.json", "BeforeTool");
    let event = json!({
        "session_id": "00000000-0000-4000-8000-0000000000e2",
        "transcript_path": "", "cwd": repo, "hook_event_name": "BeforeTool",
        "timestamp": "2026-11-24T23:15:02.000Z", "tool_name": "write_file",
        "tool_input": {"file_path": repo.join("src/a.rs"), "content": "fn b() {}\n"},
    });
    let (stdout, coverage) = home.run_hook(&command, &repo, &event);
    assert_eq!((coverage.as_str(), stdout.as_str()), ("admitted", "{}"));
    let report = String::from_utf8(home.langspan(&["doctor"]).stdout).unwrap();
    assert!(
        report.contains("ok gemini-cli: user-level hooks installed"),
        "{report}"
    );
    assert!(
        report.contains("ok gemini-cli: MCP server `langspan` registered"),
        "{report}"
    );

    home.ok(&["uninstall", "--hosts", "gemini-cli", "--apply"]);
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        GEMINI_SETTINGS,
        "uninstall restores the file exactly"
    );
    let nothing = home.ok(&["uninstall", "--hosts", "gemini-cli", "--apply"]);
    assert_eq!(nothing.matches("already matches").count(), 1, "{nothing}");

    // An entry the owner changed after install is theirs: an uninstall dry
    // run keeps it and does not print it.
    home.ok(&["install", "--hosts", "gemini-cli", "--apply"]);
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    value["mcpServers"]["langspan"]["env"] = json!({"SECRET": "owner-added-secret"});
    fs::write(
        &settings,
        serde_json::to_string_pretty(&value).unwrap() + "\n",
    )
    .unwrap();
    let dry = home.ok(&["uninstall", "--hosts", "gemini-cli"]);
    assert!(
        dry.contains("kept as it is: LangSpan did not write it"),
        "{dry}"
    );
    assert!(!dry.contains("owner-added-secret"), "{dry}");

    // A `langspan` server LangSpan did not write stops the install.
    let foreign = Home::new();
    let path = foreign.write(
        ".gemini/settings.json",
        r#"{"mcpServers":{"langspan":{"httpUrl":"https://example.invalid/mcp"}}}"#,
    );
    let before = fs::read_to_string(&path).unwrap();
    let output = foreign.langspan(&["install", "--hosts", "gemini-cli", "--apply"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not written by LangSpan"),
        "{output:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn a_gemini_user_hook_defers_only_to_project_settings_gemini_loads() {
    let mut home = Home::new();
    home.write(".gemini/settings.json", "{}\n");
    home.ok(&["install", "--hosts", "gemini-cli", "--apply"]);
    home.start();
    let command = home.installed_command(".gemini/settings.json", "BeforeTool");
    let repo = home.repo("project");
    let project = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args([
            "hooks",
            "install",
            "gemini-cli",
            "--workspace",
            "covered",
            "--root",
        ])
        .arg(&repo)
        .arg("--state-dir")
        .arg(home.state_dir())
        .arg("--apply")
        .output()
        .unwrap();
    assert!(project.status.success(), "{project:?}");
    let event = |cwd: &Path, n: u32| {
        json!({
            "session_id": format!("00000000-0000-4000-8000-0000000000f{n}"),
            "transcript_path": "", "cwd": cwd, "hook_event_name": "BeforeTool",
            "timestamp": "2026-11-24T23:15:02.000Z", "tool_name": "write_file",
            "tool_input": {"file_path": repo.join("src/a.rs"), "content": "x\n"},
        })
    };
    // Started at the root, Gemini loads the project hook: the user hook steps aside.
    let (stdout, coverage) = home.run_hook(&command, &repo, &event(&repo, 1));
    assert_eq!(
        (coverage.as_str(), stdout.as_str()),
        ("deferred_to_project", "{}")
    );
    // Started in a subdirectory, Gemini reads only that directory's
    // settings, so the project hook does not run and the user hook does.
    let sub = repo.join("src");
    let (stdout, coverage) = home.run_hook(&command, &sub, &event(&sub, 2));
    assert_eq!((coverage.as_str(), stdout.as_str()), ("admitted", "{}"));
}

/// A Homebrew-shaped prefix under the test's base: the binary in a versioned
/// keg, with the `opt` and `bin` links Homebrew makes pointing at it.
fn brew_keg(base: &Path, version: &str) -> PathBuf {
    let keg = base.join("brew/Cellar/langspan").join(version);
    fs::create_dir_all(keg.join("bin")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_langspan"), keg.join("bin/langspan")).unwrap();
    for (link, target) in [
        ("brew/opt/langspan", format!("../Cellar/langspan/{version}")),
        (
            "brew/bin/langspan",
            format!("../Cellar/langspan/{version}/bin/langspan"),
        ),
    ] {
        let link = base.join(link);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(target, link).unwrap();
    }
    keg
}

#[test]
fn a_homebrew_install_names_the_stable_opt_path_and_survives_an_upgrade() {
    let home = Home::new();
    home.write(".claude/settings.json", CLAUDE_SETTINGS);
    home.write(".claude.json", CLAUDE_JSON);
    let repo = home.repo("project");
    let old = brew_keg(&home.base, "0.1.0");
    let brew = |args: &[&str]| -> String {
        let output = support::output_once_not_busy(|| {
            let mut command = Process::new(home.base.join("brew/bin/langspan"));
            command
                .args(args)
                .arg("--state-dir")
                .arg(home.state_dir())
                .env_remove("CLAUDE_PROJECT_DIR");
            command
        });
        String::from_utf8(output.stdout).unwrap()
    };
    let home_dir = home.home();
    let home_arg = home_dir.to_str().unwrap();
    let root = repo.to_str().unwrap();
    brew(&[
        "install",
        "--hosts",
        "claude-code",
        "--home",
        home_arg,
        "--apply",
    ]);
    brew(&[
        "hooks",
        "install",
        "claude-code",
        "--workspace",
        "app",
        "--root",
        root,
        "--apply",
    ]);

    let stable = home.base.join("brew/opt/langspan/bin/langspan");
    let quoted = format!("'{}'", stable.display());
    let claude: Value =
        serde_json::from_str(&fs::read_to_string(home_dir.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        claude["mcpServers"]["langspan"]["command"],
        json!(stable),
        "the MCP entry names the opt link, not the keg"
    );
    let user = home.installed_command(".claude/settings.json", "PreToolUse");
    assert!(user.starts_with(&quoted), "{user}");
    let project = fs::read_to_string(repo.join(".claude/settings.local.json")).unwrap();
    assert!(project.contains(&quoted), "{project}");
    assert!(!project.contains("Cellar"), "{project}");

    // `brew upgrade`: a new keg, the links moved to it, the old keg removed.
    brew_keg(&home.base, "0.2.0");
    fs::remove_dir_all(old).unwrap();
    let text = brew(&["doctor", "--home", home_arg, "--root", root]);
    assert!(!text.contains("no longer exists"), "{text}");
    assert!(
        text.contains("ok claude-code: user-level hooks installed"),
        "{text}"
    );
}

const OWNER_AGENTS: &str = "# My Codex notes\n\nPrefer small commits.\n";
const OWNER_GEMINI: &str = "# My Gemini notes\n\nAnswer briefly.\n";
const SKILL_HOSTS: &str = "claude-code,codex,gemini-cli";

impl Home {
    fn skill_files(&self) -> Vec<(PathBuf, Option<String>)> {
        [
            ".claude/skills/langspan/SKILL.md",
            ".codex/AGENTS.md",
            ".gemini/GEMINI.md",
            ".claude/settings.json",
            ".claude.json",
            ".codex/hooks.json",
            ".codex/config.toml",
            ".gemini/settings.json",
        ]
        .iter()
        .map(|relative| {
            let path = self.home().join(relative);
            (path.clone(), fs::read_to_string(&path).ok())
        })
        .collect()
    }
    fn doctor_lines(&self) -> String {
        String::from_utf8(self.langspan(&["doctor"]).stdout).unwrap()
    }
}

#[test]
fn the_skill_is_written_only_when_asked_reported_by_doctor_and_removed_exactly() {
    use langspan::{install::Vendor, skill};
    let home = Home::new();
    home.claude_and_codex();
    home.write(".codex/AGENTS.md", OWNER_AGENTS);
    home.write(".gemini/GEMINI.md", OWNER_GEMINI);
    let skill_md = home.home().join(".claude/skills/langspan/SKILL.md");

    // Off by default: an install without --skill neither shows nor writes it.
    home.ok(&["install", "--hosts", SKILL_HOSTS, "--apply"]);
    assert!(!skill_md.exists());
    assert_eq!(
        fs::read_to_string(home.home().join(".codex/AGENTS.md")).unwrap(),
        OWNER_AGENTS
    );
    let text = home.doctor_lines();
    for host in ["claude-code", "codex", "gemini-cli"] {
        assert!(
            text.contains(&format!("ok {host}: the LangSpan skill is not installed")),
            "{text}"
        );
    }
    let before = home.skill_files();

    // A dry run shows each host's form and its size, and writes nothing.
    let dry = home.ok(&["install", "--skill", "--hosts", SKILL_HOSTS]);
    for vendor in [Vendor::ClaudeCode, Vendor::Codex, Vendor::Gemini] {
        let path = skill::path(
            vendor,
            &home.home().join(match vendor {
                Vendor::ClaudeCode => ".claude",
                Vendor::Codex => ".codex",
                _ => ".gemini",
            }),
        );
        assert!(
            dry.contains(&format!("{} would become (", path.display())),
            "{dry}"
        );
    }
    assert!(dry.contains(skill::SOURCE.lines().next().unwrap()), "{dry}");
    assert!(dry.contains("Dry run; nothing was written"), "{dry}");
    assert_eq!(home.skill_files(), before);

    home.ok(&["install", "--skill", "--hosts", SKILL_HOSTS, "--apply"]);
    assert_eq!(
        fs::read_to_string(&skill_md).unwrap(),
        skill::render(Vendor::ClaudeCode)
    );
    assert_eq!(
        fs::metadata(&skill_md).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for (relative, owner, vendor) in [
        (".codex/AGENTS.md", OWNER_AGENTS, Vendor::Codex),
        (".gemini/GEMINI.md", OWNER_GEMINI, Vendor::Gemini),
    ] {
        let text = fs::read_to_string(home.home().join(relative)).unwrap();
        assert!(text.starts_with(owner), "{text}");
        assert!(text.ends_with(&skill::render(vendor)), "{text}");
    }
    let text = home.doctor_lines();
    for host in ["claude-code", "codex", "gemini-cli"] {
        assert!(
            text.contains(&format!(
                "ok {host}: the LangSpan skill is installed and up to date"
            )),
            "{text}"
        );
    }
    let again = home.ok(&["install", "--skill", "--hosts", SKILL_HOSTS, "--apply"]);
    assert_eq!(again.matches("already matches").count(), 8, "{again}");

    // A copy that differs from this version's is named, and replaced.
    let installed = fs::read_to_string(&skill_md).unwrap();
    fs::write(&skill_md, installed.replace("Never edit", "Do not edit")).unwrap();
    let text = home.doctor_lines();
    assert!(
        text.contains("FIX claude-code: the LangSpan skill in")
            && text.contains("differs from this version's"),
        "{text}"
    );
    home.ok(&["install", "--skill", "--hosts", "claude-code", "--apply"]);
    assert_eq!(fs::read_to_string(&skill_md).unwrap(), installed);

    // Uninstall shows, then removes, exactly what was written: the owner's
    // own text in AGENTS.md and GEMINI.md stays byte for byte.
    let dry = home.ok(&["uninstall", "--hosts", SKILL_HOSTS]);
    assert!(
        dry.contains(&format!("{} would be removed.", skill_md.display())),
        "{dry}"
    );
    home.ok(&["uninstall", "--hosts", SKILL_HOSTS, "--apply"]);
    assert!(!skill_md.exists());
    assert!(
        !skill_md.parent().unwrap().exists(),
        "its own folder goes too"
    );
    assert_eq!(
        fs::read_to_string(home.home().join(".codex/AGENTS.md")).unwrap(),
        OWNER_AGENTS
    );
    assert_eq!(
        fs::read_to_string(home.home().join(".gemini/GEMINI.md")).unwrap(),
        OWNER_GEMINI
    );
    // Removing what install wrote leaves nothing to remove.
    let nothing = home.ok(&["uninstall", "--hosts", SKILL_HOSTS, "--apply"]);
    assert!(!nothing.contains("SKILL.md"), "{nothing}");

    // `--skill` is for install only.
    assert!(!home.langspan(&["uninstall", "--skill"]).status.success());
}

#[test]
fn a_skill_file_langspan_did_not_write_stops_the_install_and_is_kept() {
    let home = Home::new();
    home.claude_and_codex();
    let mine = home.write(
        ".claude/skills/langspan/SKILL.md",
        "---\nname: langspan\n---\nMine.\n",
    );
    let before = home.skill_files();
    let output = home.langspan(&["install", "--skill", "--apply"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not written by LangSpan"),
        "{output:?}"
    );
    // Nothing at all was written, hooks included.
    assert_eq!(home.skill_files(), before);
    let text = home.doctor_lines();
    assert!(
        text.contains("FIX") && text.contains("not written by LangSpan"),
        "{text}"
    );
    home.ok(&["install", "--apply"]);
    home.ok(&["uninstall", "--apply"]);
    assert_eq!(
        fs::read_to_string(&mine).unwrap(),
        "---\nname: langspan\n---\nMine.\n"
    );
    // Without a skill to write, the install of every other host goes ahead.
    assert!(!home.home().join(".codex/AGENTS.md").exists());
}

#[test]
fn guidance_files_that_are_symlinks_do_not_stop_uninstall_and_are_left_alone() {
    let home = Home::new();
    home.claude_and_codex();
    // The owner keeps both guidance files in their dotfiles, linked in.
    let dotfiles = home.base.join("dotfiles");
    fs::create_dir_all(&dotfiles).unwrap();
    for (relative, owner, name) in [
        (".codex/AGENTS.md", OWNER_AGENTS, "AGENTS.md"),
        (".gemini/GEMINI.md", OWNER_GEMINI, "GEMINI.md"),
    ] {
        let target = dotfiles.join(name);
        fs::write(&target, owner).unwrap();
        let link = home.home().join(relative);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
    }
    home.ok(&["install", "--hosts", SKILL_HOSTS, "--apply"]);
    let text = home.doctor_lines();
    for host in ["codex", "gemini-cli"] {
        assert!(
            text.contains(&format!("ok {host}: ")) && text.contains("is a symlink"),
            "{text}"
        );
        assert!(!text.contains(&format!("FIX {host}: ")), "{text}");
    }

    let removed = home.ok(&["uninstall", "--hosts", SKILL_HOSTS, "--apply"]);
    assert!(!removed.contains("AGENTS.md"), "{removed}");
    assert!(
        !home.home().join(".codex/hooks.json").exists()
            || !fs::read_to_string(home.home().join(".codex/hooks.json"))
                .unwrap()
                .contains("langspan"),
        "the hooks were removed"
    );
    for (relative, owner, name) in [
        (".codex/AGENTS.md", OWNER_AGENTS, "AGENTS.md"),
        (".gemini/GEMINI.md", OWNER_GEMINI, "GEMINI.md"),
    ] {
        let link = home.home().join(relative);
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(dotfiles.join(name)).unwrap(), owner);
    }

    // Writing the skill through a symlink is still refused.
    let output = home.langspan(&["install", "--skill", "--hosts", "codex", "--apply"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("symlink"),
        "{output:?}"
    );
    assert_eq!(
        fs::read_to_string(dotfiles.join("AGENTS.md")).unwrap(),
        OWNER_AGENTS
    );
}

#[test]
fn doctor_warns_when_codex_would_not_read_the_whole_section() {
    use langspan::{install::Vendor, skill};
    let home = Home::new();
    home.claude_and_codex();
    // The owner's own text before the section nearly fills Codex's budget.
    let long = format!("# Notes\n\n{}\n", "x".repeat(30 * 1024));
    home.write(".codex/AGENTS.md", &long);
    home.ok(&["install", "--skill", "--hosts", "codex", "--apply"]);
    let text = home.doctor_lines();
    assert!(
        text.contains("FIX codex: ") && text.contains("32 KiB"),
        "{text}"
    );

    // Within the budget, nothing is said about it.
    home.write(
        ".codex/AGENTS.md",
        &format!("{OWNER_AGENTS}\n{}", skill::render(Vendor::Codex)),
    );
    let text = home.doctor_lines();
    assert!(!text.contains("32 KiB"), "{text}");
    assert!(!text.contains("FIX codex"), "{text}");

    // AGENTS.override.md is read instead of AGENTS.md.
    home.write(".codex/AGENTS.override.md", "# Override\n");
    let text = home.doctor_lines();
    assert!(
        text.contains("FIX codex: ") && text.contains("AGENTS.override.md"),
        "{text}"
    );
}

#[test]
fn cursor_gets_the_skill_as_a_project_rule_that_hooks_remove_takes_away() {
    use langspan::{install::Vendor, skill};
    let home = Home::new();
    let root = home.repo("project");
    let hooks = |args: &[&str]| {
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(args)
            .arg("--root")
            .arg(&root)
            .arg("--state-dir")
            .arg(home.state_dir())
            .output()
            .unwrap()
    };
    let rule = root.join(".cursor/rules/langspan.mdc");
    let dry = hooks(&[
        "hooks",
        "install",
        "cursor",
        "--workspace",
        "demo",
        "--skill",
    ]);
    assert!(dry.status.success(), "{dry:?}");
    let dry = String::from_utf8(dry.stdout).unwrap();
    assert!(
        dry.contains(&format!("{} would become (", rule.display())),
        "{dry}"
    );
    assert!(!rule.exists());

    let applied = hooks(&[
        "hooks",
        "install",
        "cursor",
        "--workspace",
        "demo",
        "--skill",
        "--apply",
    ]);
    assert!(applied.status.success(), "{applied:?}");
    assert_eq!(
        fs::read_to_string(&rule).unwrap(),
        skill::render(Vendor::Cursor)
    );
    let doctor = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["doctor", "--root"])
        .arg(&root)
        .arg("--home")
        .arg(home.home())
        .arg("--state-dir")
        .arg(home.state_dir())
        .output()
        .unwrap();
    let text = String::from_utf8(doctor.stdout).unwrap();
    assert!(
        text.contains("ok cursor: the LangSpan skill is installed and up to date"),
        "{text}"
    );

    // Only Cursor takes the rule from `hooks install`.
    let refused = hooks(&[
        "hooks",
        "install",
        "claude-code",
        "--workspace",
        "demo",
        "--skill",
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("langspan install --skill"));

    let removed = hooks(&["hooks", "remove", "cursor", "--apply"]);
    assert!(removed.status.success(), "{removed:?}");
    assert!(!rule.exists());
    assert!(
        !root.join(".cursor/hooks.json").exists() || {
            let settings: Value =
                serde_json::from_str(&fs::read_to_string(root.join(".cursor/hooks.json")).unwrap())
                    .unwrap();
            settings.get("hooks").is_none()
        }
    );
}

const HOSTS: [&str; 4] = ["claude-code", "codex", "cursor", "gemini-cli"];

#[test]
fn uninstall_removes_the_files_install_created_in_an_empty_home_for_every_host() {
    for host in HOSTS {
        let home = Home::new();
        home.ok(&["install", "--hosts", host, "--apply"]);
        let installed = contents(&home.home());
        assert!(!installed.is_empty(), "{host}: install wrote nothing");

        let dry = home.ok(&["uninstall", "--hosts", host]);
        for path in installed.keys() {
            assert!(
                dry.contains(&format!("{} would be removed.", path.display())),
                "{host}: {dry}"
            );
        }
        assert_eq!(contents(&home.home()), installed, "{host}: dry run");

        let removed = home.ok(&["uninstall", "--hosts", host, "--apply"]);
        for path in installed.keys() {
            assert!(
                removed.contains(&format!("Removed {}.", path.display())),
                "{host}: {removed}"
            );
        }
        assert!(
            contents(&home.home()).is_empty(),
            "{host}: uninstall leaves no file that install created"
        );
        let again = home.ok(&["uninstall", "--hosts", host, "--apply"]);
        assert!(!again.contains("Removed"), "{host}: {again}");
        assert!(contents(&home.home()).is_empty(), "{host}");
    }
}

const CURSOR_HOOKS: &str = r#"{
  "version": 1,
  "hooks": {
    "stop": [
      {
        "command": "say done"
      }
    ]
  }
}
"#;
const CURSOR_MCP: &str = r#"{
  "mcpServers": {
    "docs": {
      "command": "npx",
      "args": [
        "docs"
      ]
    }
  }
}
"#;

#[test]
fn uninstall_keeps_files_that_held_the_owners_entries_byte_for_byte_for_every_host() {
    for host in HOSTS {
        let home = Home::new();
        match host {
            "claude-code" => {
                home.write(".claude/settings.json", CLAUDE_SETTINGS);
                home.write(".claude.json", CLAUDE_JSON);
            }
            "codex" => {
                home.write(".codex/hooks.json", CODEX_HOOKS);
                home.write(".codex/config.toml", CODEX_CONFIG);
            }
            "cursor" => {
                home.write(".cursor/hooks.json", CURSOR_HOOKS);
                home.write(".cursor/mcp.json", CURSOR_MCP);
            }
            _ => {
                home.write(".gemini/settings.json", GEMINI_SETTINGS);
            }
        }
        let before = contents(&home.home());
        home.ok(&["install", "--hosts", host, "--apply"]);
        let installed = contents(&home.home());
        assert_eq!(installed.len(), before.len(), "{host}: no file added");
        assert_ne!(installed, before, "{host}: install changed nothing");
        let removed = home.ok(&["uninstall", "--hosts", host, "--apply"]);
        assert!(!removed.contains("Removed"), "{host}: {removed}");
        assert_eq!(
            contents(&home.home()),
            before,
            "{host}: only LangSpan's entries are removed"
        );
    }
}

#[test]
fn uninstall_keeps_a_file_the_owner_added_to_after_install() {
    // The file LangSpan created, which the owner (or the host) then wrote
    // a setting into, is theirs too: only LangSpan's entries go.
    let home = Home::new();
    home.ok(&["install", "--hosts", "codex", "--apply"]);
    let path = home.home().join(".codex/hooks.json");
    let mut settings: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    settings["description"] = json!("mine");
    fs::write(&path, serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
    home.ok(&["uninstall", "--hosts", "codex", "--apply"]);
    let left: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(left, json!({"description": "mine"}));
    assert!(!home.home().join(".codex/config.toml").exists());
}

#[test]
fn uninstall_removes_files_an_earlier_uninstall_left_empty_and_skips_absent_ones() {
    // An earlier LangSpan's uninstall left these holding nothing; running
    // a newer uninstall again removes them.
    let cases: [(&str, &[(&str, &str)]); 2] = [
        (
            "codex",
            &[(".codex/hooks.json", "{}\n"), (".codex/config.toml", "")],
        ),
        (
            "cursor",
            &[
                (".cursor/hooks.json", "{\n  \"version\": 1\n}\n"),
                (".cursor/mcp.json", "{}\n"),
            ],
        ),
    ];
    for (host, files) in cases {
        let home = Home::new();
        for (path, body) in files {
            home.write(path, body);
        }
        let before = contents(&home.home());
        let dry = home.ok(&["uninstall", "--hosts", host]);
        for (path, _) in files {
            let path = home.home().join(path);
            assert!(
                dry.contains(&format!("{} would be removed.", path.display())),
                "{host}: {dry}"
            );
        }
        // The files LangSpan would use but that are absent are not listed.
        assert_eq!(
            dry.matches("would be removed.").count(),
            files.len(),
            "{host}: {dry}"
        );
        assert!(!dry.contains("would become"), "{host}: {dry}");
        assert_eq!(contents(&home.home()), before, "{host}: dry run");

        let removed = home.ok(&["uninstall", "--hosts", host, "--apply"]);
        for (path, _) in files {
            let path = home.home().join(path);
            assert!(
                removed.contains(&format!("Removed {}.", path.display())),
                "{host}: {removed}"
            );
        }
        assert_eq!(
            removed.matches("Removed").count(),
            files.len(),
            "{host}: {removed}"
        );
        assert!(contents(&home.home()).is_empty(), "{host}");
    }
}

/// Every file under `dir` with its content, so a check can show that
/// nothing in a host's configuration changed.
fn contents(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut found = std::collections::BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    found
}

fn host<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|host| host["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {report}"))
}

#[test]
fn hosts_reports_a_host_added_after_install_and_changes_nothing() {
    let home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--hosts", "claude-code", "--apply"]);
    // Installed afterwards: Cursor, and a Gemini CLI directory.
    fs::create_dir(home.home().join(".cursor")).unwrap();
    home.write(".gemini/settings.json", "{}\n");
    let before = contents(&home.home());

    let report: Value = serde_json::from_str(&home.ok(&["hosts", "--json"])).unwrap();
    let cursor = host(&report, "cursor");
    assert_eq!(cursor["present"], true, "{report}");
    assert_eq!(cursor["user_install"], "none", "{report}");
    assert_eq!(
        cursor["config_dir"],
        home.home().join(".cursor").to_string_lossy().as_ref()
    );
    let next = cursor["next_step"].as_str().unwrap();
    assert!(
        next.starts_with("langspan install --hosts cursor --apply"),
        "{next}"
    );
    assert_eq!(host(&report, "claude-code")["user_install"], "current");
    assert_eq!(host(&report, "claude-code")["next_step"], Value::Null);
    assert_eq!(host(&report, "codex")["present"], true);
    assert_eq!(host(&report, "codex")["user_install"], "none");
    // Gemini CLI is reported, but proposed only when the owner names it.
    let gemini = host(&report, "gemini-cli");
    assert_eq!(gemini["present"], true, "{report}");
    assert_eq!(gemini["user_install"], "none", "{report}");
    assert_eq!(gemini["next_step"], Value::Null, "{report}");

    // An install for another state directory talks to another coordinator.
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["hosts", "--json", "--home"])
        .arg(home.home())
        .arg("--state-dir")
        .arg(home.base.join("elsewhere"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let other: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        host(&other, "claude-code")["user_install"],
        "other_state_dir"
    );

    let text = home.ok(&["hosts"]);
    assert!(text.contains("cursor"), "{text}");
    assert!(text.contains("langspan install --hosts cursor"), "{text}");
    assert_eq!(contents(&home.home()), before, "detection writes nothing");
}

#[test]
fn an_unreadable_host_directory_is_unknown_not_absent() {
    let home = Home::new();
    let locked = home.home().join("locked");
    fs::create_dir_all(locked.join(".codex")).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads it anyway: nothing to show then.
    let unreadable = fs::metadata(locked.join(".codex")).is_err();
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["hosts", "--json", "--home"])
        .arg(&locked)
        .arg("--state-dir")
        .arg(home.state_dir())
        .output()
        .unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(output.status.success(), "{output:?}");
    if !unreadable {
        return;
    }
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let codex = host(&report, "codex");
    assert_eq!(codex["present"], Value::Null, "{report}");
    assert_eq!(codex["user_install"], "unknown", "{report}");
    assert!(
        codex["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{report}"
    );
    assert_eq!(codex["next_step"], Value::Null, "{report}");
}

#[test]
fn the_daemon_notes_an_uninstalled_host_once_until_it_is_installed() {
    let mut home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--hosts", "claude-code,codex", "--apply"]);
    fs::create_dir(home.home().join(".cursor")).unwrap();
    home.write(".gemini/settings.json", "{}\n");
    fs::create_dir(home.state_dir()).unwrap();
    fs::set_permissions(home.state_dir(), fs::Permissions::from_mode(0o700)).unwrap();
    let config = home.state_dir().join("config.json");
    fs::write(&config, r#"{"host_check_ms": 200}"#).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let before = contents(&home.home());

    let mut daemon = Process::new(env!("CARGO_BIN_EXE_langspan"));

    daemon.env("LANGSPAN_AUTOSTART", "off");
    support::in_home(&mut daemon, &home.home())
        .args(["daemon", "--state-dir"])
        .arg(home.state_dir())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    home.daemon = Some(daemon.spawn().unwrap());
    let state_dir = home.state_dir();
    let held = |want: bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(response) = client::call(&state_dir, &status())
                && let Outcome::Status { state, .. } = response.outcome
                && state.uninstalled_hosts.contains_key("cursor") == want
            {
                return state;
            }
            assert!(Instant::now() < deadline, "cursor never held as {want}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let state = held(true);
    // Gemini CLI is never proposed unless named; installed hosts are covered.
    assert_eq!(
        state.uninstalled_hosts.keys().collect::<Vec<_>>(),
        ["cursor"]
    );
    let events = |kind: &str| {
        let Outcome::Events { events } = client::call(
            &state_dir,
            &Request {
                version: PROTOCOL_VERSION,
                request_id: "read".into(),
                command: Command::Events {
                    after: 0,
                    limit: 1000,
                },
            },
        )
        .unwrap()
        .outcome
        else {
            panic!("events expected");
        };
        events
            .iter()
            .filter(|event| serde_json::to_value(&event.event).unwrap()["type"] == kind)
            .count()
    };
    // Several checks later, still one record.
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(events("host_not_installed"), 1);

    let status_text = || {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["status", "--state-dir"])
            .arg(&state_dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let text = status_text();
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("hosts:")).collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].contains("cursor"), "{text}");
    assert!(
        lines[0].contains("langspan install --hosts cursor"),
        "{text}"
    );

    // Doctor marks it once a session of another host works in a workspace.
    let doctor = || String::from_utf8(home.langspan(&["doctor"]).stdout).unwrap();
    let report = doctor();
    assert!(
        report.contains("ok cursor: no user-level install"),
        "{report}"
    );
    let repo = home.repo("project");
    for (id, command) in [
        (
            "w1",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: repo.to_string_lossy().into(),
            },
        ),
        (
            "s1",
            Command::SessionRegister {
                session_id: "claude-code.s1".into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
        ),
    ] {
        client::call(
            &state_dir,
            &Request {
                version: PROTOCOL_VERSION,
                request_id: id.into(),
                command,
            },
        )
        .unwrap();
    }
    let report = doctor();
    assert!(
        report.contains("FIX cursor: no user-level install"),
        "{report}"
    );
    assert!(
        report.contains("langspan install --hosts cursor"),
        "{report}"
    );
    assert_eq!(contents(&home.home()), before, "detection writes nothing");

    home.ok(&["install", "--hosts", "cursor", "--apply"]);
    held(false);
    assert!(
        !status_text().lines().any(|l| l.starts_with("hosts:")),
        "the line is gone"
    );
    assert_eq!(events("host_cleared"), 1);
    assert_eq!(events("host_not_installed"), 1);
}

impl Home {
    /// A daemon in this home checking its hosts every 200 ms, logging to
    /// `daemon.log` in the base directory.
    fn start_host_check(&mut self) {
        fs::create_dir(self.state_dir()).unwrap();
        fs::set_permissions(self.state_dir(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = self.state_dir().join("config.json");
        fs::write(&config, r#"{"host_check_ms": 200}"#).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let log = fs::File::create(self.base.join("daemon.log")).unwrap();
        let mut daemon = Process::new(env!("CARGO_BIN_EXE_langspan"));
        daemon.env("LANGSPAN_AUTOSTART", "off");
        support::in_home(&mut daemon, &self.home())
            .args(["daemon", "--state-dir"])
            .arg(self.state_dir())
            .stdout(Stdio::null())
            .stderr(log);
        self.daemon = Some(daemon.spawn().unwrap());
    }
    /// Wait until `host` is held as not installed (or not); the state then.
    fn held(&self, host: &str, want: bool) -> langspan_core::State {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(response) = client::call(&self.state_dir(), &status())
                && let Outcome::Status { state, .. } = response.outcome
                && state.uninstalled_hosts.contains_key(host) == want
            {
                return state;
            }
            assert!(Instant::now() < deadline, "{host} never held as {want}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    /// How many events of `kind` the coordinator has recorded.
    fn count(&self, kind: &str) -> usize {
        let Outcome::Events { events } = client::call(
            &self.state_dir(),
            &Request {
                version: PROTOCOL_VERSION,
                request_id: "read".into(),
                command: Command::Events {
                    after: 0,
                    limit: 1000,
                },
            },
        )
        .unwrap()
        .outcome
        else {
            panic!("events expected");
        };
        events
            .iter()
            .filter(|event| serde_json::to_value(&event.event).unwrap()["type"] == kind)
            .count()
    }
}

#[test]
fn a_host_notice_is_kept_while_the_host_cannot_be_inspected() {
    let mut home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--hosts", "claude-code,codex", "--apply"]);
    fs::create_dir(home.home().join(".cursor")).unwrap();
    home.start_host_check();
    home.held("cursor", true);

    // Cursor's hook file cannot be parsed for a moment: neither an install
    // nor a removal is established, so the notice stays, nothing new is
    // recorded, and the inspection error is logged once.
    let hooks = home.write(".cursor/hooks.json", "{ not json");
    std::thread::sleep(Duration::from_millis(1200));
    let state = home.held("cursor", true);
    assert!(state.uninstalled_hosts.contains_key("cursor"));
    assert_eq!(home.count("host_cleared"), 0);
    let log = fs::read_to_string(home.base.join("daemon.log")).unwrap();
    let failed: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("host_check_failed"))
        .collect();
    assert_eq!(failed.len(), 1, "logged once: {log}");
    assert!(
        failed[0].contains("cursor") && failed[0].contains("hooks.json"),
        "{log}"
    );

    // Readable again and still not installed: the same notice, not a new one.
    fs::remove_file(hooks).unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    home.held("cursor", true);
    assert_eq!(home.count("host_cleared"), 0);
    assert_eq!(home.count("host_not_installed"), 1);
}

#[test]
fn following_the_status_advice_verbatim_installs_for_this_coordinator() {
    let mut home = Home::new();
    home.claude_and_codex();
    home.ok(&["install", "--hosts", "claude-code,codex", "--apply"]);
    fs::create_dir(home.home().join(".cursor")).unwrap();
    // The coordinator's state directory is not this home's default one.
    home.start_host_check();
    home.held("cursor", true);

    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["status", "--state-dir"])
        .arg(home.state_dir())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.starts_with("hosts: cursor"))
        .unwrap_or_else(|| panic!("{text}"));
    let advice = line.split('`').nth(1).unwrap_or_else(|| panic!("{line}"));

    // Run as the owner would, in a shell in that home with `langspan` on
    // the path: the preview, then the same command with --apply.
    let bin = Path::new(env!("CARGO_BIN_EXE_langspan")).parent().unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    for command in [advice.to_owned(), format!("{advice} --apply")] {
        let mut shell = Process::new("/bin/sh");
        let output = support::in_home(&mut shell, &home.home())
            .args(["-c", &command])
            .env("PATH", &path)
            .current_dir(&home.base)
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}: {output:?}");
    }

    let report: Value = serde_json::from_str(&home.ok(&["hosts", "--json"])).unwrap();
    assert_eq!(
        host(&report, "cursor")["user_install"],
        "current",
        "{advice}: {report}"
    );
    home.held("cursor", false);
}

/// Issue #130: a user-level install whose state directory was named through
/// a symbolic link is the same coordinator as `doctor` run with its real
/// path; a genuinely different state directory still warns.
#[test]
fn doctor_takes_a_user_level_state_directory_named_through_a_link_for_the_same_one() {
    let home = Home::new();
    home.claude_and_codex();
    fs::create_dir(home.state_dir()).unwrap();
    let link = home.base.join("state-link");
    std::os::unix::fs::symlink(home.state_dir(), &link).unwrap();
    let other = home.base.join("other");
    fs::create_dir(&other).unwrap();
    let run = |state_dir: &Path, args: &[&str]| -> String {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(args)
            .arg("--home")
            .arg(home.home())
            .arg("--state-dir")
            .arg(state_dir)
            .env_remove("CLAUDE_PROJECT_DIR")
            .output()
            .unwrap();
        assert!(output.status.success() || args[0] == "doctor", "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let warning = "different state directory";
    run(
        &link,
        &["install", "--hosts", "claude-code,codex", "--apply"],
    );
    let text = run(&home.state_dir(), &["doctor"]);
    assert!(
        text.contains("ok claude-code: user-level hooks installed")
            && text.contains("ok codex: MCP server `langspan` registered")
            && !text.contains(warning),
        "{text}"
    );
    let text = run(&other, &["doctor"]);
    assert!(
        text.contains("FIX claude-code: user-level hooks installed") && text.contains(warning),
        "{text}"
    );
}
