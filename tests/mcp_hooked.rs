#![cfg(unix)]
//! `langspan mcp --tools hooked` (roadmap I11, ADR 0018) against a real
//! daemon, with hook events recorded from Claude Code 2.1.331
//! (`fixtures/claude-code-2.1.331`) and Codex 0.171.0
//! (`fixtures/codex-0.171.0`) run through the real hook executable.
//!
//! The quiet period is shortened with `LANGSPAN_TEST_HOOK_QUIET_MS`, which
//! also shortens how often the server asks the daemon.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use langspan::mcp::{HIDDEN_WHEN_HOOKED, TOOL_NAMES};
use rmcp::model::CallToolRequestParams;
use rmcp::service::{NotificationContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::{Instant, sleep, timeout};

const DEADLINE: Duration = Duration::from_secs(30);
static NEXT: AtomicU64 = AtomicU64::new(0);
const CLAUDE_SESSION: &str = "claude-code.00000000-0000-4000-8000-000000000001";

/// Counts `notifications/tools/list_changed`.
#[derive(Clone)]
struct Watcher(Arc<watch::Sender<usize>>);

impl ClientHandler for Watcher {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.0.send_modify(|count| *count += 1);
    }
}

struct Client {
    service: RunningService<RoleClient, Watcher>,
    changes: watch::Receiver<usize>,
}

impl Client {
    async fn names(&self) -> Vec<String> {
        let tools = timeout(DEADLINE, self.service.list_all_tools())
            .await
            .expect("tools/list timed out")
            .expect("tools/list failed");
        let mut names: Vec<String> = tools.into_iter().map(|tool| tool.name.into()).collect();
        names.sort();
        names
    }

    /// Wait until `count` list-changed notifications have arrived.
    async fn changed(&mut self, count: usize) {
        timeout(DEADLINE, self.changes.wait_for(|seen| *seen >= count))
            .await
            .unwrap_or_else(|_| panic!("no list-changed notification number {count}"))
            .expect("client handler alive");
    }

    fn seen(&self) -> usize {
        *self.changes.borrow()
    }

    fn instructions(&self) -> String {
        self.service
            .peer_info()
            .and_then(|info| info.instructions.clone())
            .unwrap_or_default()
    }

    async fn call(&self, name: &'static str, arguments: Value) -> rmcp::model::CallToolResult {
        let request = CallToolRequestParams::new(name)
            .with_arguments(arguments.as_object().expect("object arguments").clone());
        timeout(DEADLINE, self.service.call_tool(request))
            .await
            .expect("tool call timed out")
            .expect("tool call failed")
    }
}

/// As `tests/support`'s `holds_for`, but yielding to the runtime between
/// looks, so the client keeps receiving notifications while it waits.
async fn holds_for(what: &str, period: Duration, holds: impl Fn() -> bool) {
    let started = Instant::now();
    loop {
        assert!(
            holds(),
            "{what}: no longer true {:?} into the {period:?} it must hold for",
            started.elapsed()
        );
        if started.elapsed() >= period {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
}

fn every_tool() -> Vec<String> {
    TOOL_NAMES.iter().map(|name| (*name).to_owned()).collect()
}

fn shorter_list() -> Vec<String> {
    TOOL_NAMES
        .iter()
        .filter(|name| !HIDDEN_WHEN_HOOKED.contains(name))
        .map(|name| (*name).to_owned())
        .collect()
}

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-mcp-hooked-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(base.join("work")).unwrap();
        fs::write(base.join("work/a.txt"), "hello\n").unwrap();
        fs::write(base.join("work/README.md"), "# demo\n").unwrap();
        Self { base, daemon: None }
    }

    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }

    fn root(&self) -> PathBuf {
        self.base.join("work")
    }

    async fn start_daemon(&mut self) {
        let child = Command::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(self.state_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start real daemon");
        self.daemon = Some(child);
        let deadline = Instant::now() + DEADLINE;
        while !self.state_dir().join("daemon.sock").exists() {
            assert!(Instant::now() < deadline, "daemon startup timed out");
            sleep(Duration::from_millis(10)).await;
        }
    }

    fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// A recorded hook event, moved into this fixture's checkout, run
    /// through the real hook executable as the host runs it.
    fn hook(&self, vendor: &str, fixture: &str) {
        self.run_hook(vendor, &self.fixture_text(fixture));
    }

    fn fixture_text(&self, fixture: &str) -> String {
        let path = format!("{}/fixtures/{fixture}.json", env!("CARGO_MANIFEST_DIR"));
        fs::read_to_string(path)
            .unwrap()
            .replace("/work/demo", self.root().to_str().unwrap())
    }

    fn run_hook(&self, vendor: &str, event: &str) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
            .current_dir(self.root())
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .env_remove("LANGSPAN_CAPTURE_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn claude_edit(&self) {
        self.hook("claude-code", "claude-code-2.1.331/pre-edit");
        self.hook("claude-code", "claude-code-2.1.331/post-edit");
    }

    fn codex_patch(&self) {
        self.hook("codex", "codex-0.171.0/pre-apply-patch");
        self.hook("codex", "codex-0.171.0/post-apply-patch");
    }

    /// `langspan mcp` started by a host in this checkout, with `args` after
    /// `--state-dir`, and the quiet period shortened to `quiet_ms`.
    async fn connect(&self, args: &[&str], quiet_ms: Option<u64>) -> Client {
        self.connect_in(&self.root(), args, quiet_ms).await
    }

    async fn connect_in(&self, cwd: &Path, args: &[&str], quiet_ms: Option<u64>) -> Client {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
        command
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("mcp")
            .arg("--state-dir")
            .arg(self.state_dir())
            .args(args)
            .current_dir(cwd)
            .stderr(Stdio::null());
        match quiet_ms {
            Some(ms) => command.env("LANGSPAN_TEST_HOOK_QUIET_MS", ms.to_string()),
            None => command.env_remove("LANGSPAN_TEST_HOOK_QUIET_MS"),
        };
        let (sender, changes) = watch::channel(0);
        let transport = TokioChildProcess::new(command).expect("spawn real MCP adapter");
        let service = timeout(DEADLINE, Watcher(Arc::new(sender)).serve(transport))
            .await
            .expect("MCP initialization timed out")
            .expect("MCP initialization failed");
        Client { service, changes }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_daemon();
        let _ = fs::remove_dir_all(&self.base);
    }
}

const HOOKED_CLAUDE: &[&str] = &["--tools", "hooked", "--host", "claude-code"];
const HOOKED_CODEX: &[&str] = &["--tools", "hooked", "--host", "codex"];

#[tokio::test]
async fn every_tool_is_listed_with_tools_all_in_every_state() {
    let mut fixture = Fixture::new();
    // Daemon down.
    let default = fixture.connect(&[], Some(1_000)).await;
    assert_eq!(default.names().await, every_tool());
    fixture.start_daemon().await;
    fixture.claude_edit();
    let all = fixture.connect(&["--tools", "all"], Some(1_000)).await;
    // Long enough for a hooked server to have asked many times.
    holds_for(
        "no list-changed notification with --tools all",
        Duration::from_millis(500),
        || all.seen() == 0,
    )
    .await;
    assert_eq!(all.names().await, every_tool());
    assert_eq!(default.names().await, every_tool());
    assert_eq!(all.seen(), 0);
    assert!(!all.instructions().contains("langspan_session_register"));
}

/// The measurement recorded in `benchmarks/mcp-tools/README.md`: bytes of
/// the tool list as served (each tool as compact JSON, the form a host
/// receives) and of the server instructions, for every tool and for the
/// list `--tools hooked` serves while hooks report. Print it with
/// `cargo test --test mcp_hooked measure -- --nocapture`.
#[tokio::test]
async fn measure_the_served_tool_list() {
    let fixture = Fixture::new();
    let all = fixture.connect(&[], None).await;
    let hooked = fixture.connect(HOOKED_CLAUDE, None).await;
    let tools = timeout(DEADLINE, all.service.list_all_tools())
        .await
        .unwrap()
        .unwrap();
    let bytes = |tool: &rmcp::model::Tool| serde_json::to_vec(tool).unwrap().len();
    let kept: Vec<_> = tools
        .iter()
        .filter(|tool| !HIDDEN_WHEN_HOOKED.contains(&tool.name.as_ref()))
        .collect();
    let total: usize = tools.iter().map(bytes).sum();
    let kept_total: usize = kept.iter().map(|tool| bytes(tool)).sum();
    let hidden: serde_json::Map<String, Value> = tools
        .iter()
        .filter(|tool| HIDDEN_WHEN_HOOKED.contains(&tool.name.as_ref()))
        .map(|tool| (tool.name.to_string(), json!(bytes(tool))))
        .collect();
    let per_tool: serde_json::Map<String, Value> = tools
        .iter()
        .map(|tool| (tool.name.to_string(), json!(bytes(tool))))
        .collect();
    let report = json!({
        "langspan": env!("CARGO_PKG_VERSION"),
        "tools": tools.len(),
        "tools_bytes": total,
        "hooked_tools": kept.len(),
        "hooked_tools_bytes": kept_total,
        "hidden_bytes": hidden,
        "instructions_bytes": all.instructions().len(),
        "hooked_instructions_bytes": hooked.instructions().len(),
        "per_tool_bytes": per_tool,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(kept.len() + HIDDEN_WHEN_HOOKED.len(), tools.len());
    assert!(kept_total < total);
}

#[tokio::test]
async fn with_the_daemon_down_a_hooked_server_lists_every_tool() {
    let fixture = Fixture::new();
    let client = fixture.connect(HOOKED_CLAUDE, Some(30_000)).await;
    holds_for(
        "no list-changed notification with the daemon down",
        Duration::from_millis(500),
        || client.seen() == 0,
    )
    .await;
    assert_eq!(client.names().await, every_tool());
    assert_eq!(client.seen(), 0);
    // A call is answered, with the daemon's unavailability.
    let status = client.call("langspan_status", json!({})).await;
    assert_eq!(status.is_error, Some(true));
}

/// No hook event: every tool. After a recorded Claude Code edit through the
/// hooks: the shorter list, a list-changed notification, instructions naming
/// the hidden tools, and `langspan_admit` still answering. With the daemon
/// gone: every tool again.
#[tokio::test]
async fn a_claude_code_hook_event_shortens_the_list_and_hidden_tools_still_answer() {
    let mut fixture = Fixture::new();
    fixture.start_daemon().await;
    let mut client = fixture.connect(HOOKED_CLAUDE, Some(30_000)).await;
    let instructions = client.instructions();
    for hidden in HIDDEN_WHEN_HOOKED {
        assert!(instructions.contains(hidden), "{instructions}");
    }
    holds_for(
        "no list-changed notification before a hook event",
        Duration::from_millis(500),
        || client.seen() == 0,
    )
    .await;
    assert_eq!(client.names().await, every_tool(), "no hook event yet");
    assert_eq!(client.seen(), 0);

    fixture.claude_edit();
    client.changed(1).await;
    assert_eq!(client.names().await, shorter_list());

    let admitted = client
        .call(
            "langspan_admit",
            json!({"request_id": "mcp-admit-1", "session_id": CLAUDE_SESSION,
                   "paths": ["a.txt"], "ttl_ms": 5_000}),
        )
        .await;
    assert_eq!(admitted.is_error, Some(false), "{admitted:?}");
    let outcome = &admitted.structured_content.unwrap()["outcome"];
    assert_eq!(outcome["type"], "admitted", "{outcome}");

    fixture.stop_daemon();
    client.changed(2).await;
    assert_eq!(client.names().await, every_tool(), "daemon unavailable");
}

/// A Codex hook event shortens a Codex server's list and not a Claude Code
/// server's in the same checkout; a server started outside the checkout is
/// not affected either.
#[tokio::test]
async fn a_codex_hook_event_shortens_only_the_codex_list_for_its_workspace() {
    let mut fixture = Fixture::new();
    fixture.start_daemon().await;
    let mut codex = fixture.connect(HOOKED_CODEX, Some(30_000)).await;
    let claude = fixture.connect(HOOKED_CLAUDE, Some(30_000)).await;
    let elsewhere_dir = fixture.base.join("elsewhere");
    fs::create_dir(&elsewhere_dir).unwrap();
    let elsewhere = fixture
        .connect_in(&elsewhere_dir, HOOKED_CODEX, Some(30_000))
        .await;

    fixture.codex_patch();
    codex.changed(1).await;
    assert_eq!(codex.names().await, shorter_list());
    // Several of their polls later, the others still list every tool.
    holds_for(
        "no list-changed notification to the other servers",
        Duration::from_millis(1_500),
        || claude.seen() == 0 && elsewhere.seen() == 0,
    )
    .await;
    assert_eq!(claude.seen(), 0);
    assert_eq!(claude.names().await, every_tool());
    assert_eq!(elsewhere.seen(), 0);
    assert_eq!(elsewhere.names().await, every_tool());
}

/// After the quiet period with no further hook event, the list returns to
/// every tool and a second list-changed notification is sent.
#[tokio::test]
async fn the_list_returns_to_every_tool_when_hooks_go_quiet() {
    let mut fixture = Fixture::new();
    fixture.start_daemon().await;
    let mut client = fixture.connect(HOOKED_CLAUDE, Some(4_000)).await;
    fixture.claude_edit();
    client.changed(1).await;
    client.changed(2).await;
    assert_eq!(client.names().await, every_tool());
    // A new hook event shortens it again.
    fixture.claude_edit_again();
    client.changed(3).await;
}

impl Fixture {
    /// The same recorded edit by the same session again, as a new tool call.
    fn claude_edit_again(&self) {
        for name in ["pre-edit", "post-edit"] {
            let text = self
                .fixture_text(&format!("claude-code-2.1.331/{name}"))
                .replace("toolu_018A5huJynCN32GAjA9oC2mA", "toolu_again");
            self.run_hook("claude-code", &text);
        }
    }
}
