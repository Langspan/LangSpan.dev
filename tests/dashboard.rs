#![cfg(unix)]
//! `langspan dashboard` as a real child process against a real daemon, with
//! sessions driven through the real hook executable, and requests made over
//! real TCP: who may read it, what it shows, and what it refuses.

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "11111111-0000-4000-8000-00000000000b";
const CAROL: &str = "22222222-0000-4000-8000-00000000000c";
const HOSTILE: &str = r#"<script>alert("x")</script> it's <b>bold</b> & </title>"#;

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
    dashboard: Option<Child>,
}

fn git(root: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=LangSpan test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Http {
    status: u16,
    headers: String,
    body: String,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-dash-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("work");
        fs::create_dir_all(&root).unwrap();
        for file in ["a.txt", "b.txt"] {
            fs::write(root.join(file), "hello\n").unwrap();
        }
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "--quiet", "-m", "fixture"]);
        Self {
            base,
            daemon: None,
            dashboard: None,
        }
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
    fn start_daemon(&mut self) {
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
        while !Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["status", "--state-dir"])
            .arg(self.state_dir())
            .output()
            .unwrap()
            .status
            .success()
        {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    /// Start the dashboard on an ephemeral loopback port and return the URL
    /// it printed.
    fn start_dashboard(&mut self) -> (u16, String) {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["dashboard", "--listen", "127.0.0.1:0", "--state-dir"])
            .arg(self.state_dir())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        self.dashboard = Some(child);
        let url = line.trim();
        let rest = url.strip_prefix("http://127.0.0.1:").expect(url);
        let (port, query) = rest.split_once('/').unwrap();
        let token = query.strip_prefix("?t=").unwrap();
        assert_eq!(token.len(), 64, "a 256-bit token in hex: {url}");
        (port.parse().unwrap(), token.into())
    }
    fn hook(&self, vendor: &str, event: &Value) -> String {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
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
        diagnostic["coverage"].as_str().unwrap().into()
    }
    fn prompt(&self, session: &str, text: &str) {
        let event = json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.root(),
            "hook_event_name": "UserPromptSubmit", "prompt": text,
        });
        self.hook("claude-code", &event);
    }
    fn codex_admit(&self, session: &str, call: &str, file: &str) -> String {
        let event = json!({
            "session_id": session, "turn_id": "turn", "transcript_path": null, "cwd": self.root(),
            "hook_event_name": "PreToolUse", "model": "example", "permission_mode": "default",
            "tool_name": "apply_patch", "tool_use_id": call,
            "tool_input": {"command": format!(
                "*** Begin Patch\n*** Update File: {file}\n@@\n-hello\n+hi\n*** End Patch"
            )},
        });
        self.hook("codex", &event)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for child in [self.dashboard.take(), self.daemon.take()]
            .into_iter()
            .flatten()
        {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Send raw bytes and read the whole reply.
fn send(port: u16, request: &[u8]) -> Http {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (headers, body) = text.split_once("\r\n\r\n").expect(&text);
    Http {
        status: headers[9..12].parse().unwrap(),
        headers: headers.to_ascii_lowercase(),
        body: body.into(),
    }
}

fn get(port: u16, target: &str, extra: &str) -> Http {
    send(
        port,
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}\r\n").as_bytes(),
    )
}

#[test]
fn the_dashboard_shows_every_session_only_to_its_token_holder_and_refuses_the_rest() {
    let mut f = Fixture::new();
    f.start_daemon();
    f.prompt(ALICE, HOSTILE);
    assert_eq!(f.codex_admit(BOB, "call-1", "a.txt"), "admitted");
    assert_eq!(f.codex_admit(CAROL, "call-2", "a.txt"), "denied");
    let (port, token) = f.start_dashboard();

    // No token, a wrong one, the right one.
    let anonymous = get(port, "/api/state", "");
    assert_eq!(anonymous.status, 401, "{}", anonymous.body);
    assert!(!anonymous.body.contains("agents"));
    let wrong = get(
        port,
        "/api/state",
        &format!("X-LangSpan-Token: {}\r\n", "0".repeat(64)),
    );
    assert_eq!(wrong.status, 403);
    assert_eq!(get(port, "/?t=abc", "").status, 403);
    assert_eq!(get(port, "/", "").status, 401);

    let state = get(
        port,
        "/api/state",
        &format!("X-LangSpan-Token: {token}\r\n"),
    );
    assert_eq!(state.status, 200, "{}", state.body);
    assert!(state.headers.contains("content-type: application/json"));
    assert!(state.headers.contains("x-content-type-options: nosniff"));
    assert!(state.headers.contains("cache-control: no-store"));
    assert!(!state.headers.contains("access-control-"), "no CORS");
    assert!(
        !state.body.contains("<script>") && !state.body.contains('<'),
        "markup characters are escaped even inside JSON: {}",
        state.body
    );
    let body: Value = serde_json::from_str(&state.body).unwrap();
    assert_eq!(body["available"], true);
    let view = &body["dashboard"];
    let agents = view["agents"].as_array().unwrap();
    let alice = agents
        .iter()
        .find(|row| row["host"] == "claude-code")
        .expect("Alice's session");
    assert_eq!(alice["session"], "00000000");
    assert_eq!(alice["task"], HOSTILE, "the task line is returned as data");
    let bob = agents
        .iter()
        .find(|row| row["session"] == "11111111")
        .expect("Bob's session");
    assert_eq!(bob["state"], "editing");
    assert_eq!(bob["editing"], json!(["a.txt"]));
    assert_eq!(view["editing"][0]["path"], "a.txt");
    // Carol was refused and has recorded no work, but her hook ran: she is
    // active (roadmap J12), editing nothing.
    let carol = agents
        .iter()
        .find(|row| row["session"] == "22222222")
        .expect("Carol's session");
    assert_eq!(carol["state"], "active", "{carol}");
    assert_eq!(carol["editing"], json!([]), "{carol}");
    assert_eq!(view["active_agents"], 3);
    assert!(view["uptime_ms"].as_u64().is_some());
    let conflict = &view["denials"]["conflicts"][0];
    assert_eq!(conflict["session_id"], format!("codex.{CAROL}"));
    assert_eq!(conflict["holders"], json!([format!("codex.{BOB}")]));
    assert_eq!(conflict["age_ms"], Value::Null, "conflicts carry no time");
    assert_eq!(view["workspaces"][0]["workspace_id"], "work");

    // The token may also be a query parameter, as the page's URL has it.
    assert_eq!(get(port, &format!("/api/state?t={token}"), "").status, 200);

    // The page: self-contained, strict policy, data placed as text only.
    let page = get(port, &format!("/?t={token}"), "");
    assert_eq!(page.status, 200);
    assert!(page.headers.contains("content-type: text/html"));
    let policy = page
        .headers
        .lines()
        .find_map(|line| line.strip_prefix("content-security-policy: "))
        .expect("a content security policy");
    assert!(policy.contains("default-src 'none'"), "{policy}");
    assert!(!policy.contains("unsafe"), "{policy}");
    assert!(page.headers.contains("x-frame-options: deny"));
    assert!(page.headers.contains("referrer-policy: no-referrer"));
    let html = &page.body;
    assert!(
        !html.contains("langspan-demo-scenes-v1"),
        "the live page never loads the demo scenes"
    );
    for external in [
        "http:", "https:", "//cdn", "src=", "href=", "url(", "@import",
    ] {
        assert!(!html.contains(external), "the page loads {external}");
    }
    assert!(html.contains("textContent"));
    for sink in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
    ] {
        assert!(!html.contains(sink), "the page uses {sink}");
    }
    assert!(!html.contains(HOSTILE) && !html.contains("alert("));
    assert!(html.contains("fetch('/api/state'") && html.contains("2000"));

    // A page served under another name: DNS rebinding.
    let rebound = send(
        port,
        format!("GET /api/state?t={token} HTTP/1.1\r\nHost: attacker.example:{port}\r\n\r\n")
            .as_bytes(),
    );
    assert_eq!(rebound.status, 421);
    let hostless = send(
        port,
        format!("GET /api/state?t={token} HTTP/1.1\r\n\r\n").as_bytes(),
    );
    assert_eq!(hostless.status, 421);

    // Only GET.
    let post = send(
        port,
        format!(
            "POST /api/state?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 2\r\n\r\n{{}}"
        )
        .as_bytes(),
    );
    assert_eq!(post.status, 405);
    assert!(post.headers.contains("allow: get"));
    let body = get(
        port,
        &format!("/api/state?t={token}"),
        "Content-Length: 5\r\n",
    );
    assert_eq!(body.status, 413);

    // Too large a request.
    let oversized = get(
        port,
        &format!("/api/state?t={token}"),
        &format!("X-Padding: {}\r\n", "a".repeat(16 * 1024)),
    );
    assert_eq!(oversized.status, 431);

    // Unknown paths are not found, token or not.
    assert_eq!(
        get(port, &format!("/state.sqlite3?t={token}"), "").status,
        404
    );

    // The coordinator stops: the view says so instead of showing nothing.
    f.stop_daemon();
    let down = get(
        port,
        "/api/state",
        &format!("X-LangSpan-Token: {token}\r\n"),
    );
    assert_eq!(down.status, 503);
    let body: Value = serde_json::from_str(&down.body).unwrap();
    assert_eq!(body["available"], false);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("coordination unavailable"),
        "{body}"
    );
}

/// A file whose name is markup, held by one session and refused to another:
/// the JSON carries the name as escaped data, every row has an identity,
/// and the page never holds the name.
#[test]
fn a_hostile_file_name_is_escaped_data_and_every_row_has_an_identity() {
    let mut f = Fixture::new();
    let hostile = r#"<img onerror=alert(1)>&"q".txt"#;
    fs::write(f.root().join(hostile), "hello\n").unwrap();
    f.start_daemon();
    assert_eq!(f.codex_admit(BOB, "call-1", hostile), "admitted");
    assert_eq!(f.codex_admit(CAROL, "call-2", hostile), "denied");
    let (port, token) = f.start_dashboard();
    let state = get(port, &format!("/api/state?t={token}"), "");
    assert_eq!(state.status, 200, "{}", state.body);
    assert!(
        !state.body.contains('<') && !state.body.contains('&'),
        "{}",
        state.body
    );
    let body: Value = serde_json::from_str(&state.body).unwrap();
    let view = &body["dashboard"];
    let ids: Vec<&str> = view["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().expect("every row has an id"))
        .collect();
    assert!(
        ids.contains(&format!("local:codex.{BOB}").as_str()),
        "{ids:?}"
    );
    assert_eq!(view["editing"][0]["path"], hostile);
    assert_eq!(view["denials"]["conflicts"][0]["paths"][0], hostile);
    let page = get(port, &format!("/?t={token}"), "").body;
    assert!(!page.contains(hostile) && !page.contains("onerror"));
}

/// `--demo` serves the same renderer with fictional scenes, labeled as
/// simulated, behind the same token and policy, and serves no coordinator
/// state, even with a coordinator running.
#[test]
fn the_demo_is_labeled_simulated_and_serves_no_coordinator_state() {
    let mut f = Fixture::new();
    f.start_daemon();
    f.prompt(ALICE, "real task line");
    let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args([
            "dashboard",
            "--demo",
            "--listen",
            "127.0.0.1:0",
            "--state-dir",
        ])
        .arg(f.state_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    f.dashboard = Some(child);
    let rest = line.trim().strip_prefix("http://127.0.0.1:").expect(&line);
    let (port, query) = rest.split_once('/').unwrap();
    let port: u16 = port.parse().unwrap();
    let token = query.strip_prefix("?t=").unwrap();
    assert_eq!(
        get(port, "/", "").status,
        401,
        "the demo needs its token too"
    );
    let page = get(port, &format!("/?t={token}"), "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("langspan-demo-scenes-v1"));
    assert!(page.body.contains("Simulated data"));
    assert!(!page.body.contains("real task line"));
    for external in ["http:", "https:", "src=", "href=", "url(", "innerHTML"] {
        assert!(
            !page.body.contains(external),
            "the demo page holds {external}"
        );
    }
    assert!(
        page.headers
            .contains("content-security-policy: default-src 'none'")
    );
    let state = get(port, &format!("/api/state?t={token}"), "");
    assert_eq!(state.status, 404, "{}", state.body);
    assert!(!state.body.contains("real task line"));
    let events = get(port, &format!("/api/events?t={token}"), "");
    assert_eq!(events.status, 404, "{}", events.body);
}

#[test]
fn a_network_address_is_refused_without_allow_network() {
    let f = Fixture::new();
    for listen in ["0.0.0.0:0", "[::]:0", "192.0.2.1:7311"] {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["dashboard", "--listen", listen, "--state-dir"])
            .arg(f.state_dir())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{listen}");
        assert!(output.stdout.is_empty(), "no URL was printed for {listen}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("not a loopback address"), "{stderr}");
        assert!(stderr.contains("--allow-network"), "{stderr}");
        assert!(stderr.contains("SSH"), "{stderr}");
    }
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["dashboard", "--listen", "localhost:7311", "--state-dir"])
        .arg(f.state_dir())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("IP address and port"));
}

#[test]
fn the_dashboard_install_advice_names_this_coordinator_state_directory() {
    let mut f = Fixture::new();
    // A home with Cursor and no LangSpan install: the daemon's host check
    // holds it as not installed.
    let home = f.base.join("home");
    fs::create_dir_all(home.join(".cursor")).unwrap();
    fs::create_dir(f.state_dir()).unwrap();
    fs::set_permissions(f.state_dir(), fs::Permissions::from_mode(0o700)).unwrap();
    let config = f.state_dir().join("config.json");
    fs::write(&config, r#"{"host_check_ms": 200}"#).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    f.daemon = Some(
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(f.state_dir())
            .env("HOME", &home)
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("CODEX_HOME", home.join(".codex"))
            .env("GEMINI_CLI_HOME", &home)
            .env_remove("LANGSPAN_STATE_DIR")
            .env_remove("XDG_STATE_HOME")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (port, token) = f.start_dashboard();
    let deadline = Instant::now() + Duration::from_secs(10);
    let body = loop {
        let reply = get(port, &format!("/api/state?t={token}"), "");
        if reply.status == 200 {
            let body: Value = serde_json::from_str(&reply.body).unwrap();
            if body["dashboard"]["uninstalled_hosts"] == json!(["cursor"]) {
                break body;
            }
        }
        assert!(Instant::now() < deadline, "cursor never shown");
        std::thread::sleep(Duration::from_millis(20));
    };
    let advice = body["install_advice"]["cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("{body}"));
    assert_eq!(
        advice,
        format!(
            "langspan install --hosts cursor --state-dir '{}'",
            f.state_dir().display()
        )
    );
    // The page shows the coordinator's command rather than building one.
    let page = get(port, &format!("/?t={token}"), "").body;
    assert!(page.contains("install_advice"), "the page uses the advice");
}

const FEED_PROMPT: &str = "FEED-PROMPT-TEXT rework the parser";

/// Poll `/api/events` two at a time from `after` until a page is empty,
/// adding each row's seq and type to `seen`.
fn drain_feed(port: u16, header: &str, after: &mut u64, seen: &mut Vec<(u64, String)>) {
    loop {
        let reply = get(port, &format!("/api/events?after={after}&limit=2"), header);
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert!(reply.headers.contains("content-type: application/json"));
        assert!(!reply.body.contains("FEED-PROMPT-TEXT"), "{}", reply.body);
        let page: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(page["available"], true, "{page}");
        assert_eq!(page["version"], 1, "{page}");
        let events = page["events"].as_array().unwrap();
        assert!(events.len() <= 2, "{page}");
        let next = page["next"].as_u64().unwrap();
        if events.is_empty() {
            assert_eq!(next, *after, "an empty page resumes where it was asked");
            return;
        }
        for event in events {
            let kind = event["type"].as_str().unwrap().to_owned();
            seen.push((event["seq"].as_u64().unwrap(), kind));
        }
        *after = next;
    }
}

/// Real daemon, hooks and TCP, because what is verified is the order of
/// the events the coordinator really appended, read across separate polls
/// while more arrive (issue #500).
#[test]
fn the_event_feed_pages_every_event_in_seq_order_without_gaps_or_prompt_text() {
    let mut f = Fixture::new();
    f.start_daemon();
    f.prompt(ALICE, FEED_PROMPT);
    assert_eq!(f.codex_admit(BOB, "call-1", "a.txt"), "admitted");
    let (port, token) = f.start_dashboard();
    let header = format!("X-LangSpan-Token: {token}\r\n");

    let (mut after, mut seen) = (0, Vec::new());
    drain_feed(port, &header, &mut after, &mut seen);
    let first = seen.len();
    assert!(first > 2, "several pages were read: {seen:?}");
    // More happens; the next polls carry on from `next`.
    assert_eq!(f.codex_admit(CAROL, "call-2", "a.txt"), "denied");
    f.prompt(CAROL, FEED_PROMPT);
    drain_feed(port, &header, &mut after, &mut seen);
    assert!(seen.len() > first, "the later events arrived: {seen:?}");
    assert_eq!(seen[0].0, 1, "{seen:?}");
    for pair in seen.windows(2) {
        assert_eq!(pair[1].0, pair[0].0 + 1, "seq order, no gaps: {seen:?}");
    }
    for kind in ["session_prompted", "lease_granted", "write_refused"] {
        assert!(seen.iter().any(|(_, t)| t == kind), "{kind} in {seen:?}");
    }

    // The same refusals as the rest of the dashboard.
    assert_eq!(get(port, "/api/events", "").status, 401);
    let wrong = format!("X-LangSpan-Token: {}\r\n", "0".repeat(64));
    assert_eq!(get(port, "/api/events", &wrong).status, 403);
    let target = format!("/api/events?t={token}");
    let foreign = format!("GET {target} HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n");
    assert_eq!(send(port, foreign.as_bytes()).status, 421);
    let post = format!("POST {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    assert_eq!(send(port, post.as_bytes()).status, 405);
    let bad = get(port, "/api/events?limit=5000", &header);
    assert_eq!(bad.status, 400, "{}", bad.body);
    assert!(bad.body.contains("between 1 and 1000"), "{}", bad.body);

    // A stopped coordinator is said, not hidden.
    f.stop_daemon();
    let down = get(port, "/api/events", &header);
    assert_eq!(down.status, 503, "{}", down.body);
    let down: Value = serde_json::from_str(&down.body).unwrap();
    assert_eq!(down["available"], false, "{down}");
}
