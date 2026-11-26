//! `langspan guidance`: the LangSpan block for AGENTS.md and similar files
//! (roadmap I7, ADR 0018). Real Git repositories with the owner's own
//! AGENTS.md, and the real executable.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BEGIN: &str = "<!-- langspan:begin -->";
const END: &str = "<!-- langspan:end -->";

/// A temporary Git repository, removed when dropped.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ls-guidance-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        Self(dir)
    }

    fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }

    fn write(&self, file: &str, bytes: &str) {
        std::fs::write(self.path(file), bytes).unwrap();
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.path(file)).unwrap()
    }

    fn guidance(&self, args: &[&str]) -> Output {
        langspan_in(&self.0, args)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn langspan_in(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("guidance")
        .args(args)
        .args(["--root", root.to_str().unwrap()])
        .env_remove("LANGSPAN_STATE_DIR")
        .output()
        .unwrap()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn failed(output: &Output) -> String {
    assert!(!output.status.success(), "{output:?}");
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// The owner's AGENTS.md, without a final newline to make byte-for-byte
/// restoration harder, and with a CRLF line.
const OWNER: &str = "# Project notes\r\n\nUse tabs.\n\n## Tests\nRun make test.";

fn printed_block() -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("guidance")
        .output()
        .unwrap();
    ok(&output)
}

#[test]
fn guidance_prints_the_block_for_each_format() {
    let agents = printed_block();
    assert!(agents.starts_with(&format!("{BEGIN}\n")), "{agents}");
    assert!(agents.ends_with(&format!("{END}\n")), "{agents}");
    assert!(agents.len() <= 1500, "{} bytes", agents.len());
    for tool in ["langspan_brief", "langspan_admit", "langspan_complete"] {
        assert!(agents.contains(tool), "{tool}: {agents}");
    }
    assert!(agents.contains("advice"), "{agents}");
    let run = |format: &str| {
        ok(&Command::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["guidance", "--format", format])
            .output()
            .unwrap())
    };
    assert_eq!(run("agents"), agents);
    assert_eq!(run("gemini"), agents);
    let rules = run("cursor-rules");
    assert!(rules.starts_with("---\n"), "{rules}");
    assert!(rules.contains("alwaysApply: true"), "{rules}");
    assert!(rules.ends_with(&agents), "{rules}");
    let wrong = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["guidance", "--format", "windsurf"])
        .output()
        .unwrap();
    assert!(failed(&wrong).contains("agents|gemini|cursor-rules"));
}

#[test]
fn the_dry_run_prints_a_diff_and_changes_no_file() {
    let repo = Repo::new("dry");
    repo.write("AGENTS.md", OWNER);
    let before = std::fs::metadata(repo.path("AGENTS.md")).unwrap();
    let shown = ok(&repo.guidance(&["install", "--file", "AGENTS.md"]));
    assert!(shown.contains(&format!("+ {BEGIN}\n")), "{shown}");
    assert!(shown.contains(&format!("+ {END}\n")), "{shown}");
    assert!(
        shown.contains("dry run: add --apply to write AGENTS.md"),
        "{shown}"
    );
    // No line of the owner's is shown as removed.
    assert!(!shown.lines().any(|line| line.starts_with("- ")), "{shown}");
    assert_eq!(repo.read("AGENTS.md"), OWNER);
    let after = std::fs::metadata(repo.path("AGENTS.md")).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    // Nothing else appeared in the repository either.
    let entries: Vec<_> = std::fs::read_dir(&repo.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries.len(), 2, "{entries:?}"); // .git and AGENTS.md
}

#[test]
fn apply_adds_one_block_keeps_the_rest_and_remove_restores_it() {
    let repo = Repo::new("apply");
    repo.write("AGENTS.md", OWNER);
    let block = printed_block();
    let shown = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(shown.contains("wrote"), "{shown}");
    let installed = repo.read("AGENTS.md");
    assert_eq!(installed.matches(BEGIN).count(), 1, "{installed}");
    assert_eq!(installed.matches(END).count(), 1, "{installed}");
    // Exactly the block was added; every other byte is the owner's.
    assert_eq!(installed.replacen(&block, "", 1), format!("\n{OWNER}"));

    // Twice leaves one block, and says nothing changed.
    let again = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(again.contains("already"), "{again}");
    assert_eq!(repo.read("AGENTS.md"), installed);

    // The removal's dry run shows only the block going, and changes nothing.
    let dry = ok(&repo.guidance(&["remove", "--file", "AGENTS.md"]));
    assert!(dry.contains(&format!("- {BEGIN}\n")), "{dry}");
    assert!(!dry.lines().any(|line| line.starts_with("+ ")), "{dry}");
    assert_eq!(repo.read("AGENTS.md"), installed);

    ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert_eq!(repo.read("AGENTS.md"), OWNER);
    let nothing = ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert!(nothing.contains("no LangSpan block"), "{nothing}");
    assert_eq!(repo.read("AGENTS.md"), OWNER);
}

#[test]
fn a_file_with_a_leading_blank_line_or_nothing_in_it_round_trips() {
    for original in ["", "\n", "\n\nText\n", "x"] {
        let repo = Repo::new("edges");
        repo.write("GEMINI.md", original);
        ok(&repo.guidance(&["install", "--file", "GEMINI.md", "--apply"]));
        assert_eq!(repo.read("GEMINI.md").matches(BEGIN).count(), 1);
        ok(&repo.guidance(&["remove", "--file", "GEMINI.md", "--apply"]));
        // An empty file is removed with the block: nothing of it is lost.
        if original.is_empty() {
            assert!(!repo.path("GEMINI.md").exists());
        } else {
            assert_eq!(repo.read("GEMINI.md"), original, "{original:?}");
        }
    }
}

#[test]
fn a_moved_block_is_left_where_the_owner_put_it() {
    let repo = Repo::new("moved");
    let block = printed_block();
    let moved = format!("# Notes\n\n{block}\nMore.\n");
    repo.write("AGENTS.md", &moved);
    let again = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(again.contains("already"), "{again}");
    assert_eq!(repo.read("AGENTS.md"), moved);
    ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert_eq!(repo.read("AGENTS.md"), "# Notes\n\n\nMore.\n");
}

#[test]
fn a_hand_edited_block_or_broken_markers_stop_with_the_line_numbers() {
    let block = printed_block();
    let edited = block.replacen("Other agent", "Some agent", 1);
    assert_ne!(edited, block);
    let block_lines = block.lines().count();
    for (text, lines) in [
        (
            format!("# Notes\n\n{edited}"),
            format!("lines 3-{}", 2 + block_lines),
        ),
        (format!("# Notes\n{BEGIN}\nhalf\n"), "line 2".to_owned()),
        (format!("{END}\n# Notes\n"), "line 1".to_owned()),
        (
            format!("{block}\n{block}"),
            format!("line {}", block_lines + 2),
        ),
        (
            "# Notes\n<!-- langspan:begin v2 -->\n".to_owned(),
            "line 2".to_owned(),
        ),
    ] {
        let repo = Repo::new("edited");
        repo.write("AGENTS.md", &text);
        for action in ["install", "remove"] {
            let error = failed(&repo.guidance(&[action, "--file", "AGENTS.md", "--apply"]));
            assert!(error.contains(&lines), "{action} {text:?}: {error}");
            assert!(error.contains("nothing was changed"), "{error}");
            assert_eq!(repo.read("AGENTS.md"), text);
        }
    }
}

#[test]
fn a_missing_file_is_created_only_with_create() {
    let repo = Repo::new("create");
    let error = failed(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(error.contains("--create"), "{error}");
    assert!(!repo.path("AGENTS.md").exists());
    let shown = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--create"]));
    assert!(shown.contains("dry run"), "{shown}");
    assert!(!repo.path("AGENTS.md").exists());
    ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--create", "--apply"]));
    assert_eq!(repo.read("AGENTS.md"), printed_block());
    // Removing the block removes the file it created.
    ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert!(!repo.path("AGENTS.md").exists());
}

#[test]
fn the_file_must_stay_inside_the_root_and_not_be_a_symlink() {
    let repo = Repo::new("bounds");
    repo.write("real.md", OWNER);
    std::os::unix::fs::symlink(repo.path("real.md"), repo.path("AGENTS.md")).unwrap();
    for file in ["../AGENTS.md", "/tmp/AGENTS.md", "", "AGENTS.md"] {
        let error = failed(&repo.guidance(&["install", "--file", file, "--apply"]));
        assert!(!error.is_empty(), "{file}");
    }
    assert_eq!(repo.read("real.md"), OWNER);
    let error = failed(&repo.guidance(&["install", "--apply"]));
    assert!(error.contains("--file"), "{error}");
}

#[test]
fn a_file_in_a_subdirectory_of_the_repository_is_supported() {
    let repo = Repo::new("nested");
    std::fs::create_dir_all(repo.path("pkg")).unwrap();
    repo.write("pkg/AGENTS.md", OWNER);
    ok(&repo.guidance(&["install", "--file", "pkg/AGENTS.md", "--apply"]));
    assert_eq!(repo.read("pkg/AGENTS.md").matches(BEGIN).count(), 1);
    ok(&repo.guidance(&["remove", "--file", "pkg/AGENTS.md", "--apply"]));
    assert_eq!(repo.read("pkg/AGENTS.md"), OWNER);
}

#[test]
fn a_rule_files_front_matter_stays_first_when_the_block_is_installed() {
    let repo = Repo::new("front-matter");
    std::fs::create_dir_all(repo.path(".cursor/rules")).unwrap();
    let file = ".cursor/rules/style.mdc";
    let front = "---\ndescription: \"House style\"\nglobs: src/**/*.rs\nalwaysApply: false\n---\n";
    let body = "Use tabs.\r\n\nRun make test.";
    let owner = format!("{front}{body}");
    repo.write(file, &owner);
    let block = printed_block();
    ok(&repo.guidance(&["install", "--file", file, "--apply"]));
    let installed = repo.read(file);
    // The rule's metadata is still the file's first lines, so the host still
    // reads `globs` and `alwaysApply`; the block follows it.
    assert_eq!(installed, format!("{front}{block}\n{body}"), "{installed}");
    let again = ok(&repo.guidance(&["install", "--file", file, "--apply"]));
    assert!(again.contains("already"), "{again}");
    ok(&repo.guidance(&["remove", "--file", file, "--apply"]));
    assert_eq!(repo.read(file), owner);

    // CRLF front matter, and front matter with nothing after it.
    for (front, body) in [
        ("---\r\nalwaysApply: true\r\n---\r\n", "Text\r\n"),
        ("---\nalwaysApply: true\n---\n", ""),
    ] {
        let owner = format!("{front}{body}");
        repo.write(file, &owner);
        ok(&repo.guidance(&["install", "--file", file, "--apply"]));
        let installed = repo.read(file);
        assert_eq!(installed, format!("{front}{block}\n{body}"), "{installed}");
        ok(&repo.guidance(&["remove", "--file", file, "--apply"]));
        assert_eq!(repo.read(file), owner);
    }

    // Front matter whose closing line has no newline cannot take the block
    // after it without changing the owner's bytes: refused, file unchanged.
    let unterminated = "---\nalwaysApply: true\n---";
    repo.write(file, unterminated);
    let error = failed(&repo.guidance(&["install", "--file", file, "--apply"]));
    assert!(error.contains("front matter"), "{error}");
    assert_eq!(repo.read(file), unterminated);
}

/// The block's bullet that starts with `lead`, up to the next bullet.
fn bullet(block: &str, lead: &str) -> String {
    let start = block
        .find(&format!("\n- {lead}"))
        .unwrap_or_else(|| panic!("no bullet starts with {lead:?}:\n{block}"));
    let rest = &block[start + 3..];
    rest[..rest.find("\n- ").unwrap_or(rest.len())].to_owned()
}

fn git_in(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// A daemon child, stopped when dropped.
#[cfg(unix)]
struct Daemon(std::process::Child);

#[cfg(unix)]
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
async fn call_tool(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tool: &'static str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let request = rmcp::model::CallToolRequestParams::new(tool)
        .with_arguments(arguments.as_object().unwrap().clone());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        client.call_tool(request),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result.structured_content.unwrap()
}

/// A session without hooks that follows the block exactly (register, admit,
/// edit, complete) leaves nothing in the write journal, which only hooks
/// fill: `langspan_commit_mine` then answers `nothing_to_commit`. So the
/// block must not tell such a session to commit with it, and must give it a
/// commit that does take its work.
#[cfg(unix)]
#[tokio::test]
async fn the_blocks_commit_advice_for_a_session_without_hooks_is_what_works_for_it() {
    use rmcp::{ServiceExt, transport::TokioChildProcess};
    use serde_json::json;
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };

    use std::os::unix::fs::PermissionsExt;
    // A short directory, so the daemon's socket path fits the platform's
    // limit (104 bytes on macOS).
    let repo = Repo(std::env::temp_dir().join(format!("ls-g-{}", std::process::id())));
    std::fs::create_dir(&repo.0).unwrap();
    let base = std::fs::canonicalize(&repo.0).unwrap();
    // The daemon keeps its state only under an owner-only directory.
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let work = base.join("work");
    let state = base.join("state");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("a.txt"), "alpha\n").unwrap();
    git_in(&work, &["init", "-q"]);
    git_in(&work, &["config", "user.name", "Owner"]);
    git_in(&work, &["config", "user.email", "owner@example.invalid"]);
    git_in(&work, &["add", "."]);
    git_in(&work, &["commit", "-q", "-m", "start"]);

    let _daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["daemon", "--state-dir"])
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(state.join("daemon.sock")).is_err() {
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(&state)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    call_tool(
        &client,
        "langspan_workspace_register",
        json!({"request_id": "ws", "workspace_id": "work", "root": work}),
    )
    .await;
    call_tool(
        &client,
        "langspan_session_register",
        json!({"request_id": "session", "session_id": "agent-a", "workspace_id": "work"}),
    )
    .await;
    let admitted = call_tool(
        &client,
        "langspan_admit",
        json!({"request_id": "admit-a", "session_id": "agent-a", "paths": ["a.txt"], "ttl_ms": 60000}),
    )
    .await;
    assert!(admitted.to_string().contains("admit-a"), "{admitted}");
    std::fs::write(work.join("a.txt"), "alpha, by a session without hooks\n").unwrap();
    call_tool(
        &client,
        "langspan_complete",
        json!({"request_id": "complete-a", "session_id": "agent-a", "lease_id": "admit-a"}),
    )
    .await;
    let answer = call_tool(
        &client,
        "langspan_commit_mine",
        json!({"request_id": "commit-a", "workspace_id": "work",
               "session_id": "agent-a", "message": "Mine"}),
    )
    .await;
    client.cancel().await.unwrap();
    assert_eq!(answer["result"], "nothing_to_commit", "{answer}");
    assert_eq!(git_in(&work, &["log", "--format=%s"]), "start\n");

    let block = printed_block();
    let hooked = bullet(&block, "With LangSpan's hooks");
    assert!(hooked.contains("`langspan_commit_mine`"), "{hooked}");
    let hookless = bullet(&block, "Without hooks");
    assert!(!hookless.contains("langspan_commit_mine"), "{hookless}");
    assert!(hookless.contains("journal"), "the reason: {hookless}");
    let explicit = "`git commit -m \"...\" -- PATH...`";
    assert!(hookless.contains(explicit), "{hookless}");

    // That commit takes the session's file, and only it.
    git_in(&work, &["commit", "-q", "-m", "Mine", "--", "a.txt"]);
    assert_eq!(git_in(&work, &["log", "--format=%s"]), "Mine\nstart\n");
    assert_eq!(git_in(&work, &["status", "--porcelain=v1"]), "");
}

/// One Claude Code hook event, through the real hook executable; its
/// permission decision, if it gave one.
#[cfg(unix)]
fn claude_hook(state: &Path, work: &Path, event: &serde_json::Value) -> Option<String> {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["hook", "claude-code", "--state-dir"])
        .arg(state)
        .args(["--workspace", "work", "--root"])
        .arg(work)
        .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let reply: Option<serde_json::Value> = serde_json::from_slice(&output.stdout).ok();
    reply.and_then(|reply| {
        let output = &reply["hookSpecificOutput"];
        Some(format!(
            "{}: {}",
            output["permissionDecision"].as_str()?,
            output["permissionDecisionReason"].as_str().unwrap_or("")
        ))
    })
}

/// A session whose hooks are active and that also calls `langspan_admit`,
/// as the block's admission bullet once told every session to, holds a
/// manual lease that its own hooked edit then conflicts with: the pre-tool
/// hook refuses the edit. So the block tells only sessions without hooks to
/// admit and complete by hand.
#[cfg(unix)]
#[tokio::test]
async fn the_blocks_admission_advice_is_only_for_sessions_without_hooks() {
    use rmcp::{ServiceExt, transport::TokioChildProcess};
    use serde_json::json;
    use std::{
        os::unix::fs::PermissionsExt,
        process::Stdio,
        time::{Duration, Instant},
    };

    let repo = Repo(std::env::temp_dir().join(format!("ls-gh-{}", std::process::id())));
    std::fs::create_dir(&repo.0).unwrap();
    let base = std::fs::canonicalize(&repo.0).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let work = base.join("work");
    let state = base.join("state");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("a.txt"), "alpha\n").unwrap();
    git_in(&work, &["init", "-q"]);

    let _daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["daemon", "--state-dir"])
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(state.join("daemon.sock")).is_err() {
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }

    // The hooks register the session when it first reads the file.
    let host = "00000000-0000-4000-8000-0000000000a7";
    let file = work.join("a.txt");
    let read = json!({
        "session_id": host, "cwd": work, "hook_event_name": "PostToolUse",
        "tool_name": "Read", "tool_use_id": "read-1", "tool_input": {"file_path": file},
    });
    claude_hook(&state, &work, &read);

    // The session then admits the file by hand.
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.arg("mcp").arg("--state-dir").arg(&state);
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    call_tool(
        &client,
        "langspan_admit",
        json!({"request_id": "manual", "session_id": format!("claude-code.{host}"),
               "paths": ["a.txt"], "ttl_ms": 60000}),
    )
    .await;
    client.cancel().await.unwrap();

    // Its own hooked edit is refused on that manual lease.
    let edit = json!({
        "session_id": host, "cwd": work, "hook_event_name": "PreToolUse",
        "tool_name": "Edit", "tool_use_id": "edit-1",
        "tool_input": {"file_path": file, "old_string": "alpha", "new_string": "beta"},
    });
    let refused = claude_hook(&state, &work, &edit).unwrap_or_default();
    assert!(refused.starts_with("deny: "), "{refused}");
    let own = format!("held by session claude-code.{host} (admission manual)");
    assert!(refused.contains(&own), "{refused}");

    // So only the bullet for sessions without hooks names the manual
    // admission, and a hooked session is told to leave it to the hooks.
    let block = printed_block();
    let admit = bullet(&block, "Without LangSpan's hooks");
    assert!(admit.contains("`langspan_admit`"), "{admit}");
    assert!(admit.contains("`langspan_complete`"), "{admit}");
    assert!(admit.contains("With hooks, edit as usual"), "{admit}");
    let rest = block.replacen(&admit, "", 1);
    assert!(!rest.contains("langspan_admit"), "{block}");
    assert!(!rest.contains("langspan_complete"), "{block}");
}
