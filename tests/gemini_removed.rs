#![cfg(unix)]
//! Gemini CLI support is removed (issue #246, ADR 0033). For one release an
//! earlier install keeps working without coordinating: `langspan hook
//! gemini-cli` allows every action and records nothing, `uninstall` and
//! `hooks remove` take LangSpan's entries out of Gemini CLI's settings, and
//! `doctor` names what is left. These tests run the real executable against
//! a real daemon, real Git repositories and settings files written the way
//! the last release with the adapter wrote them.

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

mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);
const SESSION: &str = "00000000-0000-4000-8000-0000000000e1";
/// The line the retired hook prints, from its start.
const NOTICE: &str = "LangSpan: Gemini CLI support was removed";
/// The markers the last release with the adapter put on its commands.
const USER_MARK: &str = " # langspan-managed:gemini-cli:user:v1";
const PROJECT_MARK: &str = " # langspan-managed:gemini-cli:v1";

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-gemini-removed-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("notes.txt"), "alpha\nbeta\n").unwrap();
        let status = Process::new("git")
            .args(["init", "-q"])
            .current_dir(&work)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
        Self { base, daemon: None }
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn home(&self) -> PathBuf {
        self.base.join("home")
    }

    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
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
        let socket = self.state_dir().join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Runs `langspan` with this fixture's home and state directory.
    fn langspan(&self, args: &[&str]) -> Output {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut command, &self.home())
            .env("LANGSPAN_AUTOSTART", "off")
            .args(args)
            .arg("--state-dir")
            .arg(self.state_dir())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.langspan(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// Runs an installed hook's words as Gemini CLI would, with `event` on
    /// stdin, and returns its exit code, stdout and stderr. Autostart is
    /// left as it is by default, so a hook that started a coordinator would
    /// show it.
    fn hook(&self, words: &[String], event: &[u8]) -> (Option<i32>, String, String) {
        let mut child = Process::new(&words[0])
            .args(&words[1..])
            .current_dir(self.work())
            .env_remove("LANGSPAN_AUTOSTART")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(event).unwrap();
        let output = child.wait_with_output().unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }

    /// A Gemini CLI event as its hooks reference (0.70.0) describes it,
    /// with this fixture's paths.
    fn event(&self, name: &str) -> Vec<u8> {
        let mut event = json!({
            "session_id": SESSION,
            "transcript_path": "",
            "cwd": self.work(),
            "hook_event_name": name,
            "timestamp": "2026-11-24T23:15:03.000Z",
        });
        match name {
            "SessionStart" => event["source"] = json!("startup"),
            "BeforeAgent" => event["prompt"] = json!("Add a line to notes.txt"),
            "SessionEnd" => event["reason"] = json!("exit"),
            _ => {
                event["tool_name"] = json!("write_file");
                event["tool_input"] = json!({
                    "file_path": self.work().join("notes.txt"),
                    "content": "alpha\nbeta\ngamma\n",
                });
            }
        }
        serde_json::to_vec(&event).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The command the last release wrote for a Gemini CLI hook: user level
/// without a workspace, project level with one.
fn old_command(state_dir: &Path, root: Option<&Path>) -> String {
    let quote = |path: &Path| format!("'{}'", path.display());
    let body = format!(
        "{} hook gemini-cli --state-dir {}",
        quote(Path::new(env!("CARGO_BIN_EXE_langspan"))),
        quote(state_dir)
    );
    match root {
        None => format!("{body}{USER_MARK}"),
        Some(root) => format!(
            "{body} --workspace 'demo' --root {}{PROJECT_MARK}",
            quote(root)
        ),
    }
}

/// `settings` with LangSpan's hooks added as the last release added them,
/// and, at user level, its MCP server entry.
fn with_old_install(settings: &str, state_dir: &Path, root: Option<&Path>) -> String {
    let mut value: Value = serde_json::from_str(settings).unwrap();
    let command = old_command(state_dir, root);
    for event in [
        "BeforeTool",
        "AfterTool",
        "SessionStart",
        "BeforeAgent",
        "SessionEnd",
    ] {
        let mut group = json!({"hooks": [{
            "name": "langspan", "type": "command", "command": command, "timeout": 5000,
        }]});
        match event {
            "BeforeTool" => group["matcher"] = json!("^(write_file|replace|run_shell_command)$"),
            "AfterTool" => {
                group["matcher"] = json!("^(write_file|replace|run_shell_command|read_file)$")
            }
            _ => {}
        }
        let groups = value["hooks"][event]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut groups = groups;
        groups.push(group);
        value["hooks"][event] = Value::Array(groups);
    }
    if root.is_none() {
        value["mcpServers"]["langspan"] = json!({
            "command": env!("CARGO_BIN_EXE_langspan"),
            "args": ["mcp", "--state-dir", state_dir],
        });
    }
    serde_json::to_string_pretty(&value).unwrap() + "\n"
}

/// An owner's Gemini CLI settings, in the two-space layout Gemini CLI and
/// LangSpan write, with a hook and an MCP server of the owner's own and a
/// credential a dry run must not print.
const OWNER_SETTINGS: &str = r#"{
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

const OWNER_NOTES: &str = "# My notes\n\nAnswer briefly.\n";

fn words(command: &str) -> Vec<String> {
    command
        .split(" # ")
        .next()
        .unwrap()
        .split(' ')
        .map(|word| word.trim_matches('\'').to_owned())
        .collect()
}

#[test]
fn the_gemini_hook_allows_records_nothing_and_says_once_per_session_that_support_was_removed() {
    let mut f = Fixture::new();
    f.start();
    let events = [
        "SessionStart",
        "BeforeAgent",
        "BeforeTool",
        "AfterTool",
        "SessionEnd",
    ];
    for root in [None, Some(f.work())] {
        for host in ["gemini-cli", "gemini"] {
            let mut command = words(&old_command(&f.state_dir(), root.as_deref()));
            command[2] = host.into();
            let mut notices = 0;
            for event in events {
                let (code, stdout, stderr) = f.hook(&command, &f.event(event));
                assert_eq!(code, Some(0), "{event}: {stderr}");
                // No decision: Gemini CLI's own approval decides, as with
                // any hook that does not deny.
                assert_eq!(stdout, "{}", "{event}");
                notices += stderr.matches(NOTICE).count();
                if event == "SessionStart" {
                    assert_eq!(stderr.lines().count(), 1, "one line: {stderr}");
                    assert!(
                        stderr.contains("langspan uninstall --hosts gemini-cli --apply"),
                        "{stderr}"
                    );
                    assert!(
                        stderr.contains("langspan hooks remove gemini-cli --root"),
                        "{stderr}"
                    );
                } else {
                    assert_eq!(stderr, "", "{event}");
                }
            }
            assert_eq!(notices, 1, "once per session ({host}, {root:?})");
        }
    }
    // Malformed input is allowed too, without a notice.
    let command = words(&old_command(&f.state_dir(), None));
    let (code, stdout, stderr) = f.hook(&command, b"{broken");
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (Some(0), "{}", "")
    );
    // Nothing reached the coordinator: no event, no session.
    let events = f.ok(&["events", "--json"]);
    assert!(!events.contains("gemini"), "{events}");
    let status = f.ok(&["status", "--json"]);
    assert!(!status.contains("gemini"), "{status}");
    assert!(!f.state_dir().join("gemini-calls").exists());
}

#[test]
fn the_gemini_hook_starts_no_coordinator() {
    let f = Fixture::new();
    let command = words(&old_command(&f.state_dir(), None));
    let (code, stdout, _) = f.hook(&command, &f.event("BeforeTool"));
    assert_eq!((code, stdout.as_str()), (Some(0), "{}"));
    assert!(
        !f.state_dir().exists(),
        "the state directory was not even created"
    );
}

#[test]
fn uninstall_removes_an_earlier_gemini_install_and_keeps_every_other_byte() {
    let f = Fixture::new();
    let settings = f.home().join(".gemini/settings.json");
    let notes = f.home().join(".gemini/GEMINI.md");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let installed = with_old_install(OWNER_SETTINGS, &f.state_dir(), None);
    fs::write(&settings, &installed).unwrap();
    // The skill section as `install --skill` wrote it, after the owner's text.
    let section = langspan::skill::render(langspan::install::Vendor::Codex);
    fs::write(&notes, format!("{OWNER_NOTES}\n{section}")).unwrap();
    // Antigravity's files under `~/.gemini/config` are not Gemini CLI's.
    let antigravity = f.home().join(".gemini/config/hooks.json");
    fs::create_dir_all(antigravity.parent().unwrap()).unwrap();
    fs::write(&antigravity, "{\n  \"mine\": {}\n}\n").unwrap();

    let dry = f.ok(&["uninstall", "--hosts", "gemini-cli"]);
    assert!(!dry.contains("not-a-real-token"), "{dry}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), installed);

    f.ok(&["uninstall", "--hosts", "gemini-cli", "--apply"]);
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        OWNER_SETTINGS,
        "only LangSpan's hooks and MCP entry go"
    );
    assert_eq!(fs::read_to_string(&notes).unwrap(), OWNER_NOTES);
    assert_eq!(
        fs::read_to_string(&antigravity).unwrap(),
        "{\n  \"mine\": {}\n}\n"
    );
    let again = f.ok(&["uninstall", "--hosts", "gemini-cli", "--apply"]);
    assert!(again.contains("already matches"), "{again}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), OWNER_SETTINGS);
}

#[test]
fn hooks_remove_takes_an_earlier_gemini_project_install_out() {
    let f = Fixture::new();
    let settings = f.work().join(".gemini/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let owner = "{\n  \"general\": {\n    \"vimMode\": true\n  }\n}\n";
    fs::write(
        &settings,
        with_old_install(owner, &f.state_dir(), Some(&f.work())),
    )
    .unwrap();
    let root = f.work();
    let root = root.to_str().unwrap();
    f.ok(&["hooks", "remove", "gemini-cli", "--root", root, "--apply"]);
    assert_eq!(fs::read_to_string(&settings).unwrap(), owner);
}

#[test]
fn installing_for_gemini_cli_is_refused_and_says_why() {
    let f = Fixture::new();
    fs::create_dir_all(f.home().join(".gemini")).unwrap();
    fs::write(f.home().join(".gemini/settings.json"), "{}\n").unwrap();
    let root = f.work();
    let root = root.to_str().unwrap();
    for args in [
        &["install", "--hosts", "gemini-cli", "--apply"][..],
        &["install", "--hosts", "claude-code,gemini-cli", "--apply"][..],
        &[
            "hooks",
            "install",
            "gemini-cli",
            "--workspace",
            "demo",
            "--root",
            root,
            "--apply",
        ][..],
    ] {
        let output = f.langspan(args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Gemini CLI support was removed"),
            "{args:?}: {stderr}"
        );
    }
    assert_eq!(
        fs::read_to_string(f.home().join(".gemini/settings.json")).unwrap(),
        "{}\n"
    );
    assert!(!f.work().join(".gemini").exists());
    assert!(!f.home().join(".claude").exists());
}

#[test]
fn doctor_names_leftover_gemini_hooks_and_the_commands_that_remove_them() {
    let f = Fixture::new();
    let user = f.home().join(".gemini/settings.json");
    let project = f.work().join(".gemini/settings.json");
    for (path, root) in [(&user, None), (&project, Some(f.work()))] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            with_old_install("{}", &f.state_dir(), root.as_deref()),
        )
        .unwrap();
    }
    let root = f.work();
    let root = root.to_str().unwrap();
    let doctor =
        |f: &Fixture| String::from_utf8(f.langspan(&["doctor", "--root", root]).stdout).unwrap();
    let report = doctor(&f);
    let lines: Vec<&str> = report
        .lines()
        .filter(|line| line.contains("gemini-cli"))
        .collect();
    assert_eq!(lines.len(), 2, "{report}");
    assert!(
        lines.iter().all(|line| line.starts_with("FIX ")),
        "{report}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line.contains("support was removed")),
        "{report}"
    );
    assert!(
        report.contains(&format!(
            "`langspan hooks remove gemini-cli --root {root} --apply`"
        )),
        "{report}"
    );
    assert!(
        report.contains("`langspan uninstall --hosts gemini-cli --apply`"),
        "{report}"
    );
    f.ok(&["uninstall", "--hosts", "gemini-cli", "--apply"]);
    f.ok(&["hooks", "remove", "gemini-cli", "--root", root, "--apply"]);
    let report = doctor(&f);
    assert!(!report.contains("gemini-cli"), "{report}");
}
