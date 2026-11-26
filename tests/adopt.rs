#![cfg(unix)]
//! A session that continues an ended session's task takes over its
//! uncommitted work (issue #161), through the real hook executable, a real
//! daemon, a real Git repository and a real process standing in for the
//! ended session's host.

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
const BOB: &str = "00000000-0000-4000-8000-0000000000b2";
const CAROL: &str = "00000000-0000-4000-8000-0000000000c3";
const TASK: &str = "Fix the upload handler";

fn id(session: &str) -> String {
    format!("claude-code.{session}")
}

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    context: Option<String>,
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Process::new("git")
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

fn numbered() -> String {
    (1..=20).map(|n| format!("line {n}\n")).collect()
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-adopt-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("a.txt"), "alpha\n").unwrap();
        fs::write(work.join("b.txt"), "beta\n").unwrap();
        fs::write(work.join("shared.txt"), numbered()).unwrap();
        git(&work, &["init", "-q"]);
        git(&work, &["config", "user.name", "Owner"]);
        git(&work, &["config", "user.email", "owner@example.invalid"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "start"]);
        let mut fixture = Self { base, child: None };
        fixture.start_daemon();
        fixture
    }

    fn start_daemon(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(self.state())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = self.state().join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn restart_daemon(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let _ = fs::remove_file(self.state().join("daemon.sock"));
        self.start_daemon();
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn hook(&self, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
            .args(["--budget-ms", "4000"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let reply: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            context: reply.as_ref().and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            }),
        }
    }

    fn lifecycle(&self, session: &str, name: &str, prompt: Option<&str>) -> Run {
        let mut event = json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.work(),
            "hook_event_name": name,
        });
        match name {
            "SessionStart" => event["source"] = json!("startup"),
            "SessionEnd" => event["reason"] = json!("other"),
            _ => event["prompt"] = json!(prompt.unwrap_or("continue")),
        }
        self.hook(event)
    }

    fn edit(&self, session: &str, call: &str, file: &str, old: &str, new: &str) {
        self.edit_as(session, None, call, file, old, new);
    }

    /// An edit by `session`, or by its subagent `agent` when given.
    fn edit_as(
        &self,
        session: &str,
        agent: Option<&str>,
        call: &str,
        file: &str,
        old: &str,
        new: &str,
    ) {
        let path = self.work().join(file);
        let input = json!({"file_path": path, "old_string": old, "new_string": new});
        let event = |phase: &str| {
            let mut event = json!({
                "session_id": session, "cwd": self.work(), "hook_event_name": phase,
                "tool_name": "Edit", "tool_use_id": call, "tool_input": input,
            });
            if phase == "PostToolUse" {
                event["tool_response"] = json!({"filePath": path});
            }
            if let Some(agent) = agent {
                event["agent_id"] = json!(agent);
                event["agent_type"] = json!("general-purpose");
            }
            event
        };
        let pre = self.hook(event("PreToolUse"));
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(old), "{file} lacks {old:?}");
        fs::write(&path, content.replacen(old, new, 1)).unwrap();
        let post = self.hook(event("PostToolUse"));
        assert_eq!(post.coverage, "observed", "{post:?}");
    }

    fn bash(&self, session: &str, phase: &str, call: &str, command: &str) -> Run {
        self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": phase,
            "tool_name": "Bash", "tool_use_id": call,
            "tool_input": {"command": command, "description": "test command"},
            "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
        }))
    }

    /// A whole Bash call that runs `langspan ARGS` in the checkout: the
    /// pre hook, the command, and the post hook.
    fn langspan(&self, session: &str, call: &str, args: &str) -> Output {
        let command = format!(
            "'{}' --state-dir '{}' {args}",
            env!("CARGO_BIN_EXE_langspan"),
            self.state().display()
        );
        let pre = self.bash(session, "PreToolUse", call, &command);
        assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
        let output = Process::new("/bin/sh")
            .args(["-c", &command])
            .current_dir(self.work())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        self.bash(session, "PostToolUse", call, &command);
        output
    }

    fn status_mine(&self, session: &str, call: &str) -> Value {
        let output = self.langspan(session, call, "status --mine --json");
        assert!(output.status.success(), "{}", said(&output));
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .output()
            .unwrap()
    }

    fn recap(&self) -> String {
        let output = self.cli(&["recap", "work"]);
        assert!(output.status.success(), "{}", said(&output));
        String::from_utf8(output.stdout).unwrap()
    }

    fn adoptions(&self) -> usize {
        let output = self.cli(&["events", "--json"]);
        assert!(output.status.success(), "{}", said(&output));
        String::from_utf8_lossy(&output.stdout)
            .matches("\"session_adopted\"")
            .count()
    }

    /// A process standing in for `session`'s host, attached as its own.
    fn host(&self, session: &str) -> Child {
        let host = Process::new("sleep")
            .arg("120")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let attached = self.cli(&[
            "session",
            "attach-pid",
            &id(session),
            &host.id().to_string(),
            "--request-id",
            &format!("attach-{session}"),
        ]);
        assert!(attached.status.success(), "{}", said(&attached));
        host
    }

    /// Kill `host` and wait until the daemon has seen its process exit.
    fn kill(&self, mut host: Child, session: &str) {
        host.kill().unwrap();
        host.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let request = langspan::protocol::Request {
                version: langspan::protocol::PROTOCOL_VERSION,
                request_id: "read".into(),
                command: langspan_core::Command::Status,
            };
            let state = match langspan::client::call(&self.state(), &request)
                .unwrap()
                .outcome
            {
                langspan_core::Outcome::Status { state, .. } => state,
                other => panic!("{other:?}"),
            };
            if state
                .session_liveness
                .get(&id(session))
                .is_some_and(|liveness| liveness.status == langspan_core::LivenessStatus::Exited)
            {
                return;
            }
            assert!(Instant::now() < deadline, "the exit was not observed");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn status(&self) -> String {
        git(&self.work(), &["status", "--porcelain=v1"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn owners(status: &Value) -> Vec<(String, String)> {
    status["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().to_owned(),
                file["owner"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pair(path: &str, owner: &str) -> (String, String) {
    (path.to_owned(), owner.to_owned())
}

#[test]
fn a_session_continuing_a_killed_sessions_task_is_told_once_adopts_its_work_and_commits_exactly_it()
{
    let mut f = Fixture::new();
    // Alice works on her task, then her host is killed mid-edit.
    f.lifecycle(ALICE, "UserPromptSubmit", Some(TASK));
    let alices_host = f.host(ALICE);
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(ALICE, "alice-2", "b.txt", "beta", "beta, by Alice");
    f.edit(
        ALICE,
        "alice-3",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    // Carol, live, works on lines of the same file.
    f.edit(
        CAROL,
        "carol-1",
        "shared.txt",
        "line 18\n",
        "line 18, by Carol\n",
    );
    f.kill(alices_host, ALICE);

    // Bob starts in the same checkout and is told once.
    let start = f.lifecycle(BOB, "SessionStart", None);
    let context = start.context.unwrap_or_default();
    let told = format!(
        "Session {} ended and left uncommitted work in this checkout",
        id(ALICE)
    );
    assert!(context.contains(&told), "{context}");
    assert!(context.contains(TASK), "{context}");
    assert!(
        context.contains(&format!("langspan adopt -- {}", id(ALICE))),
        "{context}"
    );
    assert!(
        !context.contains(&format!("Session {} ended", id(CAROL))),
        "{context}"
    );
    let again = f.lifecycle(BOB, "UserPromptSubmit", Some("carry on"));
    assert!(
        !again.context.unwrap_or_default().contains(&told),
        "told only once"
    );
    // Before adopting, none of it is Bob's.
    assert_eq!(owners(&f.status_mine(BOB, "bob-1")), Vec::new());

    let adopted = f.langspan(BOB, "bob-2", &format!("adopt {}", id(ALICE)));
    let text = said(&adopted);
    assert!(adopted.status.success(), "{text}");
    assert!(text.contains("adopted the uncommitted work"), "{text}");
    assert!(text.contains("a.txt, b.txt, shared.txt"), "{text}");
    assert_eq!(f.adoptions(), 1);

    let expected = vec![
        pair("a.txt", "mine"),
        pair("b.txt", "mine"),
        pair("shared.txt", "mixed"),
    ];
    let mine = f.status_mine(BOB, "bob-3");
    assert_eq!(owners(&mine), expected, "{mine}");
    let recap = f.recap();
    assert!(
        recap.contains(&format!("adopted by session {}", id(BOB))),
        "{recap}"
    );
    let status = String::from_utf8(f.cli(&["status"]).stdout).unwrap();
    assert!(
        status.contains(&format!(
            "session {} took over the uncommitted work of ended session {}",
            id(BOB),
            id(ALICE)
        )),
        "{status}"
    );

    // A restart replays the event; ownership is the same.
    f.restart_daemon();
    let recap = f.recap();
    assert!(
        recap.contains(&format!("adopted by session {}", id(BOB))),
        "{recap}"
    );
    assert_eq!(owners(&f.status_mine(BOB, "bob-4")), expected);
    // Adopting again records nothing more.
    let again = f.langspan(BOB, "bob-5", &format!("adopt -- {}", id(ALICE)));
    assert!(again.status.success(), "{}", said(&again));
    assert!(
        said(&again).contains("had already adopted"),
        "{}",
        said(&again)
    );
    assert_eq!(f.adoptions(), 1);

    // commit --mine takes exactly Alice's hunks, and leaves Carol's.
    let committed = f.langspan(BOB, "bob-6", "commit --mine -m 'Alice and Bob'");
    assert!(committed.status.success(), "{}", said(&committed));
    let head = git(&f.work(), &["show", "--name-only", "--format=%s", "HEAD"]);
    assert_eq!(
        head.trim(),
        "Alice and Bob\n\na.txt\nb.txt\nshared.txt",
        "{head}"
    );
    let shared = git(&f.work(), &["show", "HEAD:shared.txt"]);
    assert!(shared.contains("line 2, by Alice\n"), "{shared}");
    assert!(!shared.contains("by Carol"), "{shared}");
    assert_eq!(f.status(), " M shared.txt\n");
    let carols = f.status_mine(CAROL, "carol-2");
    assert_eq!(
        owners(&carols),
        vec![pair("shared.txt", "mine")],
        "{carols}"
    );
}

#[test]
fn adopting_a_live_session_is_refused_and_changes_nothing() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    let before = owners(&f.status_mine(BOB, "bob-2"));
    let refused = f.langspan(BOB, "bob-3", &format!("adopt {}", id(ALICE)));
    let text = said(&refused);
    assert!(!refused.status.success(), "{text}");
    assert!(text.contains("session_live"), "{text}");
    assert_eq!(f.adoptions(), 0);
    assert_eq!(owners(&f.status_mine(BOB, "bob-4")), before);
    assert_eq!(before, vec![pair("b.txt", "mine")]);
    let alices = f.status_mine(ALICE, "alice-2");
    assert_eq!(owners(&alices), vec![pair("a.txt", "mine")]);
}

#[tokio::test]
async fn the_mcp_tool_adopts_an_ended_sessions_work_and_a_retry_is_safe() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.lifecycle(ALICE, "SessionEnd", None);
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(f.state())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    for (attempt, result, paths) in [
        ("adopt-1", "adopted", json!(["a.txt"])),
        ("adopt-2", "already_adopted", json!([])),
    ] {
        let request = CallToolRequestParams::new("langspan_adopt").with_arguments(
            json!({"request_id": attempt, "session_id": id(BOB), "from": id(ALICE)})
                .as_object()
                .unwrap()
                .clone(),
        );
        let reply = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply.is_error, Some(false), "{reply:?}");
        let value = reply.structured_content.unwrap();
        assert_eq!(value["result"], result, "{value}");
        assert_eq!(value["paths"], paths, "{value}");
    }
    client.cancel().await.unwrap();
    let mine = f.status_mine(BOB, "bob-2");
    assert_eq!(
        owners(&mine),
        vec![pair("a.txt", "mine"), pair("b.txt", "mine")],
        "{mine}"
    );
}

/// `adopt` with `--json`, as `session`, with `request_id`.
fn adopt_json(f: &Fixture, session: &str, from: &str, request_id: &str) -> Output {
    let work = f.work();
    f.cli(&[
        "adopt",
        &id(from),
        "--session",
        &id(session),
        "--request-id",
        request_id,
        "--root",
        work.to_str().unwrap(),
        "--json",
    ])
}

/// A retry of an adoption's request ID is decided again: once the ended
/// session has resumed and written more, the retry is refused and credits
/// none of its new lines (review of #176).
#[test]
fn a_retried_adoption_after_the_ended_session_resumes_is_refused_and_moves_nothing() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.lifecycle(ALICE, "SessionEnd", None);
    f.lifecycle(BOB, "SessionStart", None);
    let first = adopt_json(&f, BOB, ALICE, "adopt-once");
    assert!(first.status.success(), "{}", said(&first));
    let report: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report["paths"], json!(["a.txt"]), "{report}");

    // Alice resumes and writes another file.
    f.lifecycle(ALICE, "UserPromptSubmit", Some("carry on"));
    f.edit(ALICE, "alice-2", "b.txt", "beta", "beta, by Alice");

    let retry = adopt_json(&f, BOB, ALICE, "adopt-once");
    let text = said(&retry);
    assert!(!retry.status.success(), "{text}");
    assert!(text.contains("session_live"), "{text}");
    assert_eq!(
        owners(&f.status_mine(BOB, "bob-1")),
        vec![pair("a.txt", "mine")]
    );
    assert_eq!(
        owners(&f.status_mine(ALICE, "alice-3")),
        vec![pair("b.txt", "mine")]
    );
}

/// Ending a session does not end its subagents: adopting it credits its
/// own lines and leaves a still-running subagent's lines with that
/// subagent (review of #176).
#[test]
fn adopting_an_ended_session_leaves_its_live_subagents_lines_with_the_subagent() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit_as(
        ALICE,
        Some("agent-one"),
        "alice-sub-1",
        "b.txt",
        "beta",
        "beta, by the subagent",
    );
    f.lifecycle(ALICE, "SessionEnd", None);
    f.lifecycle(BOB, "SessionStart", None);
    let adopted = adopt_json(&f, BOB, ALICE, "adopt-parent");
    assert!(adopted.status.success(), "{}", said(&adopted));
    let report: Value = serde_json::from_slice(&adopted.stdout).unwrap();
    assert_eq!(report["paths"], json!(["a.txt"]), "{report}");
    assert_eq!(
        owners(&f.status_mine(BOB, "bob-1")),
        vec![pair("a.txt", "mine")]
    );
}

/// An adopter that ends after the coordinator records its adoption but
/// before the line journal is rewritten leaves the original session's lines
/// under that session. Adopting the adopter credits them too, so the
/// successor's `commit --mine` takes all of the work (review of #176).
#[test]
fn adopting_an_adopter_that_ended_mid_transfer_credits_the_original_sessions_lines() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.lifecycle(ALICE, "SessionEnd", None);
    f.lifecycle(BOB, "SessionStart", None);
    // Bob's adoption is recorded, then Bob ends before crediting the lines.
    let request = langspan::protocol::Request {
        version: langspan::protocol::PROTOCOL_VERSION,
        request_id: "bob-adopts".into(),
        command: langspan_core::Command::Adopt {
            session_id: id(BOB),
            from: id(ALICE),
        },
    };
    let recorded = langspan::client::call(&f.state(), &request)
        .unwrap()
        .outcome;
    assert!(
        matches!(
            recorded,
            langspan_core::Outcome::Adopted { recorded: true, .. }
        ),
        "{recorded:?}"
    );
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    f.lifecycle(BOB, "SessionEnd", None);
    assert_eq!(f.adoptions(), 1);

    f.lifecycle(CAROL, "SessionStart", None);
    let adopted = adopt_json(&f, CAROL, BOB, "carol-adopts");
    assert!(adopted.status.success(), "{}", said(&adopted));
    let report: Value = serde_json::from_slice(&adopted.stdout).unwrap();
    assert_eq!(report["paths"], json!(["a.txt", "b.txt"]), "{report}");
    assert_eq!(
        owners(&f.status_mine(CAROL, "carol-1")),
        vec![pair("a.txt", "mine"), pair("b.txt", "mine")]
    );
    let committed = f.langspan(CAROL, "carol-2", "commit --mine -m 'all of it'");
    assert!(committed.status.success(), "{}", said(&committed));
    assert_eq!(f.status(), "");
}

/// Bob adopts Alice; Alice resumes and writes more; Bob ends. Carol's
/// adoption of Bob credits Bob's lines, and Alice's new lines stay hers:
/// a session that has resumed is not an earlier source (review of #176).
#[test]
fn adopting_an_adopter_leaves_a_resumed_earlier_sessions_new_lines_with_it() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.lifecycle(ALICE, "SessionEnd", None);
    f.lifecycle(BOB, "SessionStart", None);
    let first = adopt_json(&f, BOB, ALICE, "bob-adopts");
    assert!(first.status.success(), "{}", said(&first));

    // Alice resumes and writes another file; then Bob ends.
    f.lifecycle(ALICE, "UserPromptSubmit", Some("carry on"));
    f.edit(ALICE, "alice-2", "b.txt", "beta", "beta, by Alice");
    f.lifecycle(BOB, "SessionEnd", None);

    f.lifecycle(CAROL, "SessionStart", None);
    let adopted = adopt_json(&f, CAROL, BOB, "carol-adopts");
    assert!(adopted.status.success(), "{}", said(&adopted));
    let report: Value = serde_json::from_slice(&adopted.stdout).unwrap();
    assert_eq!(report["paths"], json!(["a.txt"]), "{report}");
    assert_eq!(
        owners(&f.status_mine(CAROL, "carol-1")),
        vec![pair("a.txt", "mine")]
    );
    assert_eq!(
        owners(&f.status_mine(ALICE, "alice-3")),
        vec![pair("b.txt", "mine")]
    );
}
