#![cfg(unix)]
//! CLI paths no other test reaches (issue #325), through the real binary:
//! what each verifies is the argv, the exit status and what the process
//! prints, so each one runs `langspan` as a child process.

mod support;

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command as Process, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// An owner-only temporary directory, removed when dropped.
struct Base(PathBuf);

impl Base {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-cli-coverage-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        Self(base)
    }
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
    fn home(&self) -> PathBuf {
        self.0.join("home")
    }
    /// `langspan --state-dir STATE ARGS`, with no coordinator started for
    /// it and a home of its own.
    fn langspan(&self, args: &[&str]) -> Output {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut command, &self.home())
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .output()
            .unwrap()
    }
    fn stdout(&self, args: &[&str]) -> String {
        let output = self.langspan(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Base {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{text}"))
}

#[test]
fn doctor_names_a_project_mcp_file_langspan_did_not_write() {
    let base = Base::new();
    let root = base.0.join("repo");
    fs::create_dir(&root).unwrap();
    let root_text = root.to_str().unwrap();
    base.stdout(&[
        "hooks",
        "install",
        "cursor",
        "--workspace",
        "work",
        "--root",
        root_text,
        "--apply",
    ]);
    let mcp = root.join(".cursor/mcp.json");
    let unreadable = format!("{}: ", mcp.display());
    for (contents, expected) in [
        ("{}", "the MCP server is not registered in"),
        (
            r#"{"mcpServers":{"langspan":{"command":"someone-else"}}}"#,
            "but LangSpan did not write it",
        ),
        ("not json", unreadable.as_str()),
    ] {
        fs::write(&mcp, contents).unwrap();
        let output = base.langspan(&["doctor", "--root", root_text]);
        let text = String::from_utf8(output.stdout).unwrap();
        let line = line_with(&text, expected);
        assert!(!line.starts_with("ok "), "{contents}: {line}");
        assert!(line.contains(&mcp.display().to_string()), "{line}");
        assert!(
            !output.status.success(),
            "{contents}: a finding fails doctor"
        );
    }
}

#[test]
fn doctor_names_a_user_level_mcp_server_without_its_hooks() {
    let base = Base::new();
    let home = base.home();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(home.join(".claude/settings.json"), "{}").unwrap();
    fs::write(home.join(".claude.json"), "{}").unwrap();
    let home_text = home.to_str().unwrap();
    base.stdout(&["install", "--apply", "--home", home_text]);
    // The hooks are gone; the MCP server entry is still there.
    fs::write(home.join(".claude/settings.json"), "{}").unwrap();
    let output = base.langspan(&["doctor", "--home", home_text]);
    let text = String::from_utf8(output.stdout).unwrap();
    let line = line_with(
        &text,
        "the MCP server is registered but the user-level hooks are not installed",
    );
    assert!(line.contains("langspan install --hosts"), "{line}");
    assert!(!output.status.success(), "{text}");
}

/// Needs a real coordinator: `land-queue` and `doctor` read its state.
/// The coordinator gets the test's home too, so its host worker never
/// reads the machine's own host files.
#[test]
fn queue_cancel_and_doctor_against_a_coordinator() {
    let base = Base::new();
    let mut daemon = Process::new(env!("CARGO_BIN_EXE_langspan"));
    support::in_home(&mut daemon, &base.home())
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("daemon")
        .arg("--state-dir")
        .arg(base.state())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _daemon = support::Owned::spawn(&mut daemon);
    support::daemon_ready(&base.state());
    let gone = base.0.join("gone");
    fs::create_dir(&gone).unwrap();
    base.stdout(&[
        "workspace",
        "add",
        "app",
        gone.to_str().unwrap(),
        "--request-id",
        "r-app",
    ]);

    let queue = base.stdout(&["land-queue", "app", "--onto", "main"]);
    assert_eq!(
        queue,
        "The push turn of \"main\" is free.\nNobody is waiting for it.\n"
    );
    let cancel = base.stdout(&[
        "land-cancel",
        "app",
        "--onto",
        "main",
        "--request-id",
        "t-1",
        "--json",
    ]);
    let cancel: serde_json::Value = serde_json::from_str(&cancel).unwrap();
    assert_eq!(cancel, serde_json::json!({"ticket": "t-1", "left": false}));

    fs::remove_dir(&gone).unwrap();
    let output = base.langspan(&["doctor"]);
    let text = String::from_utf8(output.stdout).unwrap();
    let line = line_with(&text, "Workspace app: its root");
    assert!(line.contains("no longer exists"), "{line}");
    assert!(!line.starts_with("ok "), "{line}");
}

#[test]
fn a_hook_copies_its_input_into_the_capture_directory() {
    let base = Base::new();
    let capture = base.0.join("capture");
    fs::create_dir(&capture).unwrap();
    let root = base.0.join("work");
    fs::create_dir(&root).unwrap();
    let input = br#"{"hook_event_name":"Stop","session_id":"s"}"#;
    let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
    support::in_home(&mut command, &base.home())
        .env("LANGSPAN_AUTOSTART", "off")
        .env("LANGSPAN_CAPTURE_DIR", &capture)
        .args(["hook", "claude-code", "--state-dir"])
        .arg(base.state())
        .args(["--workspace", "work", "--root"])
        .arg(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = support::Owned::spawn(&mut command);
    child
        .child()
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .unwrap();
    let output = child.wait_with_output();
    assert!(output.status.success(), "{output:?}");
    let captured: Vec<_> = fs::read_dir(&capture)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(captured.len(), 1, "{captured:?}");
    let file = &captured[0];
    let name = file.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("claude-code-") && name.ends_with(".json"),
        "{name}"
    );
    assert_eq!(fs::read(file).unwrap(), input);
    assert_eq!(
        fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_capture_skipped_for_a_file(&base, &root, input);
}

/// A capture variable naming something other than a directory captures
/// nothing and leaves the hook's answer as it was.
fn assert_capture_skipped_for_a_file(base: &Base, root: &Path, input: &[u8]) {
    let not_dir = base.0.join("not-a-directory");
    fs::write(&not_dir, "").unwrap();
    let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
    support::in_home(&mut command, &base.home())
        .env("LANGSPAN_AUTOSTART", "off")
        .env("LANGSPAN_CAPTURE_DIR", &not_dir)
        .args(["hook", "claude-code", "--state-dir"])
        .arg(base.state())
        .args(["--workspace", "work", "--root"])
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = support::Owned::spawn(&mut command);
    child
        .child()
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .unwrap();
    let output = child.wait_with_output();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(&not_dir).unwrap(), b"");
}
