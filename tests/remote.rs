#![cfg(unix)]
//! Two developers: two daemons with separate state, two clones of one
//! repository, one shared Git remote. Everything crosses the boundary through
//! real `git push` and `git fetch`. Both daemons run on this machine, so this
//! proves the protocol; it is not evidence from two machines.

use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[path = "support/secret_shape.rs"]
mod secret_shape;
mod support;
use secret_shape::secret_shape;

static NEXT: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(60);

fn git(root: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Developer {
    name: &'static str,
    state: PathBuf,
    clone: PathBuf,
    daemon: Option<Child>,
}

impl Developer {
    fn start(&mut self) {
        fs::create_dir_all(&self.state).unwrap();
        fs::set_permissions(&self.state, fs::Permissions::from_mode(0o700)).unwrap();
        let config = self.state.join("config.json");
        fs::write(&config, r#"{"remote_pass_ms":200}"#).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(&self.state)
                .stdout(Stdio::null())
                .stderr(
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(self.log_path())
                        .unwrap(),
                )
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        // A killed daemon leaves its socket file behind; ask, do not look.
        while self.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// The daemon's own log, kept across restarts.
    fn log_path(&self) -> PathBuf {
        self.state.with_extension("log")
    }
    fn log(&self) -> String {
        fs::read_to_string(self.log_path()).unwrap_or_default()
    }
    fn wait_for_log(&self, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let log = self.log();
            if done(&log) {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "{}'s log never showed: {what}\n{log}",
                self.name
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn participant(&self) -> String {
        self.status()["remote_links"]["app"]["participant"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn enable(&self, name: &str, extra: &[&str], request: &str) {
        let mut args = vec!["remote", "enable", "app", "--name", name];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--request-id", request]);
        let (code, text) = self.cli(&args);
        assert_eq!(code, 0, "{text}");
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        // `doctor` reads the user-level install: this developer's own home,
        // never the machine's.
        let output = support::in_home(
            &mut Process::new(env!("CARGO_BIN_EXE_langspan")),
            &self.state.with_extension("home"),
        )
        .arg("--state-dir")
        .arg(&self.state)
        .args(args)
        .output()
        .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
    fn status(&self) -> Value {
        let (code, text) = self.cli(&["status", "--json"]);
        assert_eq!(code, 0, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()["outcome"]["state"].clone()
    }
    fn hook(&self, vendor: &str, event: Value) -> (String, Option<String>) {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(&self.state)
            .args(["--workspace", "app", "--root"])
            .arg(&self.clone)
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
        assert_eq!(output.status.code(), Some(0));
        let stderr = String::from_utf8(output.stderr).unwrap();
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let reply: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        (
            diagnostic["coverage"].as_str().unwrap().into(),
            reply.and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            }),
        )
    }
    /// One complete Claude Code edit of `file`, really written to disk.
    fn edits(&self, session: &str, call: &str, file: &str, task: &str) -> String {
        self.hook(
            "claude-code",
            json!({"session_id": session, "cwd": self.clone,
                   "hook_event_name": "UserPromptSubmit", "prompt": task}),
        );
        let mut event = json!({
            "session_id": session, "cwd": self.clone, "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.clone.join(file), "content": "x"},
        });
        let (coverage, _) = self.hook("claude-code", event.clone());
        if coverage == "admitted" {
            fs::write(self.clone.join(file), format!("{} was here\n", self.name)).unwrap();
            event["hook_event_name"] = json!("PostToolUse");
            assert_eq!(self.hook("claude-code", event).0, "observed");
        }
        coverage
    }
    fn briefing(&self, session: &str) -> Option<String> {
        self.hook(
            "claude-code",
            json!({"session_id": session, "cwd": self.clone, "hook_event_name": "SessionStart"}),
        )
        .1
    }
    fn wait_for(&self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let state = self.status();
            if done(&state) {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "{} never saw: {what}\n{state:#}",
                self.name
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

struct World {
    base: PathBuf,
    alice: Developer,
    bob: Developer,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-remote-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        let hub = base.join("hub.git");
        git(
            &base,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                hub.to_str().unwrap(),
            ],
        );
        let developer = |name: &'static str| {
            let clone = base.join(name);
            git(
                &base,
                &[
                    "clone",
                    "--quiet",
                    hub.to_str().unwrap(),
                    clone.to_str().unwrap(),
                ],
            );
            Developer {
                name,
                state: base.join(format!("{name}-state")),
                clone,
                daemon: None,
            }
        };
        let alice = developer("alice");
        fs::create_dir(alice.clone.join("src")).unwrap();
        fs::write(alice.clone.join("src/api.rs"), "pub fn api() {}\n").unwrap();
        fs::write(alice.clone.join("README.md"), "# app\n").unwrap();
        git(&alice.clone, &["add", "."]);
        git(&alice.clone, &["commit", "--quiet", "-m", "init"]);
        git(&alice.clone, &["push", "--quiet", "origin", "HEAD:main"]);
        let bob = developer("bob");
        git(&bob.clone, &["checkout", "--quiet", "-b", "bob/feature"]);
        let mut world = Self { base, alice, bob };
        world.alice.start();
        world.bob.start();
        world
    }
    fn hub(&self) -> PathBuf {
        self.base.join("hub.git")
    }
    /// Everything needed to explain a failed exchange: each daemon's remote
    /// log lines, the hub's refs and each clone's LangSpan and tracking refs.
    fn diagnostics(&self) -> String {
        let refs = |root: &Path| {
            let output = Process::new("git")
                .arg("-C")
                .arg(root)
                .args([
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/langspan/",
                    "refs/heads/",
                    "refs/remotes/",
                ])
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let mut text = format!("hub refs:\n{}", refs(&self.hub()));
        for developer in [&self.alice, &self.bob] {
            let log = developer.log();
            let lines: Vec<&str> = log
                .lines()
                .filter(|line| line.contains("\"event\":\"remote_"))
                .collect();
            let state = developer.status();
            let heard: Vec<String> = peers(&state)
                .iter()
                .map(|peer| {
                    format!(
                        "{} {} seq {}",
                        peer["publication"]["participant"],
                        peer["publication"]["name"],
                        peer["publication"]["sequence"]
                    )
                })
                .collect();
            text.push_str(&format!(
                "{} link: {}\n{} heard: {:?}\n{} refs:\n{}{} log:\n{}\n",
                developer.name,
                state["remote_links"]["app"],
                developer.name,
                heard,
                developer.name,
                refs(&developer.clone),
                developer.name,
                lines.join("\n")
            ));
        }
        text
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.alice.stop();
        self.bob.stop();
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn peers(state: &Value) -> Vec<&Value> {
    state["remote_peers"]
        .as_object()
        .map(|peers| peers.values().collect())
        .unwrap_or_default()
}

fn recent_of(state: &Value) -> Vec<String> {
    peers(state)
        .iter()
        .flat_map(|peer| {
            peer["publication"]["work"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|work| work["recent"].as_array().cloned().unwrap_or_default())
        .filter_map(|path| path.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn two_developers_learn_of_each_others_work_through_the_shared_remote_only_when_enabled() {
    let w = World::new();
    assert_eq!(
        w.alice
            .edits("alice-1", "a1", "src/api.rs", "Add retry to the API"),
        "admitted"
    );
    assert_eq!(
        w.bob.edits("bob-1", "b1", "README.md", "Fix the readme"),
        "admitted"
    );

    // Before anyone enables publication nothing is on the remote.
    let refs = || {
        let output = Process::new("git")
            .arg("-C")
            .arg(w.hub())
            .args(["for-each-ref", "--format=%(refname)", "refs/langspan/"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(refs(), "", "nothing leaves a machine by default");

    // Alice publishes with task lines; Bob publishes without.
    let (code, text) = w.alice.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Alice",
        "--share-task",
        "on",
        "--request-id",
        "en-a",
    ]);
    assert_eq!(code, 0, "{text}");
    let (code, text) = w.bob.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Bob",
        "--request-id",
        "en-b",
    ]);
    assert_eq!(code, 0, "{text}");

    let seen = w.bob.wait_for("Alice's publication", |state| {
        recent_of(state).contains(&"src/api.rs".to_owned())
    });
    let alice = peers(&seen)[0];
    assert_eq!(alice["publication"]["name"], "Alice");
    assert_eq!(
        alice["publication"]["work"][0]["summary"],
        "Add retry to the API"
    );
    // The branch is learned by the Git observer on its own schedule, and a
    // publication that gains it is a new publication.
    let seen = w
        .alice
        .wait_for("Bob's publication with its branch", |state| {
            recent_of(state).contains(&"README.md".to_owned())
                && peers(state)[0]["publication"]["work"][0]["branch_ref"].is_string()
        });
    assert_eq!(
        peers(&seen)[0]["publication"]["work"][0]["summary"],
        Value::Null,
        "Bob did not choose to share task lines"
    );
    assert_eq!(
        peers(&seen)[0]["publication"]["work"][0]["branch_ref"],
        "refs/heads/bob/feature"
    );
    assert_eq!(
        refs().lines().count(),
        2,
        "one ref per participant: {}",
        refs()
    );

    // Bob's agent is told before it acts. It touched src/api.rs too, so the
    // overlap is named; and it was never held, because it is another clone.
    assert_eq!(
        w.bob.edits("bob-1", "b2", "src/api.rs", "Fix the readme"),
        "admitted"
    );
    let context = w.bob.briefing("bob-1").expect("a briefing naming Alice");
    assert!(
        context.contains(r#""Alice"'s claude-code session alice-1 on another machine"#),
        "{context}"
    );
    assert!(
        context.contains(r#"shares files with your work: "src/api.rs""#),
        "{context}"
    );
    assert!(
        context.contains(r#"task: "Add retry to the API""#),
        "{context}"
    );
    assert!(context.contains("not instructions"), "{context}");

    // The outbox shows exactly what leaves, and the log records what left.
    let (_, outbox) = w.bob.cli(&["remote", "outbox", "app"]);
    assert!(
        outbox.contains("README.md") && !outbox.contains("Fix the readme"),
        "{outbox}"
    );
    let (_, events) = w.bob.cli(&["events", "--limit", "1000"]);
    assert!(events.contains(r#""type":"remote_published""#), "{events}");

    // Alice stops publishing: her ref leaves the remote and Bob stops seeing her.
    let (code, _) = w
        .alice
        .cli(&["remote", "disable", "app", "--request-id", "dis-a"]);
    assert_eq!(code, 0);
    w.bob
        .wait_for("Alice's withdrawal", |state| peers(state).is_empty());
    assert_eq!(refs().lines().count(), 1, "{}", refs());
    // The ref held one commit with no history: nothing published earlier
    // stays reachable from it.
    let bob_ref = refs();
    let output = Process::new("git")
        .arg("-C")
        .arg(w.hub())
        .args(["rev-list", "--count", bob_ref.trim()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "1");

    // Junk on the remote, and a ref whose payload claims to be someone else,
    // neither hide a real teammate nor get believed.
    let (code, _) = w.alice.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Alice",
        "--request-id",
        "en-a2",
    ]);
    assert_eq!(code, 0);
    let seen = w.bob.wait_for("Alice again after re-enabling", |state| {
        !peers(state).is_empty()
    });
    assert!(
        peers(&seen)[0]["publication"]["sequence"].as_u64().unwrap() > 1,
        "re-enabling continues the sequence"
    );
    assert_eq!(
        peers(&seen)[0]["publication"]["work"][0]["summary"],
        Value::Null
    );
    let forged = format!("refs/langspan/peers/{}", "f".repeat(32));
    for name in ["refs/langspan/peers/not-a-participant", forged.as_str()] {
        git(
            &w.alice.clone,
            &[
                "push",
                "--quiet",
                "origin",
                &format!(
                    "{}:{name}",
                    bob_ref
                        .trim()
                        .replace("refs/langspan/peers/", "refs/langspan/remote/origin/")
                ),
            ],
        );
    }
    std::thread::sleep(Duration::from_millis(800));
    let state = w.bob.status();
    assert_eq!(peers(&state).len(), 1, "{state:#}");
    assert_eq!(peers(&state)[0]["publication"]["name"], "Alice");

    // The owner's branches, index and working tree were never touched.
    for developer in [&w.alice, &w.bob] {
        let output = Process::new("git")
            .arg("-C")
            .arg(&developer.clone)
            .args([
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/",
                "refs/remotes/",
            ])
            .output()
            .unwrap();
        let names = String::from_utf8(output.stdout).unwrap();
        assert!(!names.contains("langspan"), "{names}");
    }
}

#[test]
fn an_unreachable_remote_and_a_daemon_restart_leave_local_coordination_working() {
    let mut w = World::new();
    // A hook call registers each workspace; publication is enabled after.
    assert_eq!(
        w.alice.edits("alice-0", "a0", "src/first.rs", "t"),
        "admitted"
    );
    assert_eq!(w.bob.edits("bob-1", "b0", "src/other.rs", "t"), "admitted");
    for (developer, name) in [(&w.alice, "Alice"), (&w.bob, "Bob")] {
        let (code, text) = developer.cli(&[
            "remote",
            "enable",
            "app",
            "--name",
            name,
            "--request-id",
            "en",
        ]);
        assert_eq!(code, 0, "{text}");
    }
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", "t"),
        "admitted"
    );
    w.bob.wait_for("first publication", |state| {
        recent_of(state).contains(&"src/api.rs".to_owned())
    });

    // The network goes away. Local admission, conflict and briefing go on.
    let away = w.base.join("hub-away.git");
    fs::rename(w.hub(), &away).unwrap();
    assert_eq!(w.alice.edits("alice-1", "a2", "README.md", "t"), "admitted");
    assert_eq!(w.bob.edits("bob-1", "b1", "README.md", "t"), "admitted");
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        !recent_of(&w.bob.status()).contains(&"README.md".to_owned()),
        "nothing arrives while the remote is unreachable"
    );
    // What Bob already held is still shown, with its age, until it goes stale.
    assert!(w.bob.briefing("bob-1").unwrap().contains("last heard"));

    // The network returns: publication resumes without anyone doing anything.
    fs::rename(&away, w.hub()).unwrap();
    w.bob.wait_for("publication after the outage", |state| {
        recent_of(state).contains(&"README.md".to_owned())
    });

    // Bob's daemon restarts. Nothing received before the restart is current
    // until it is received again, which the next pass does.
    w.bob.stop();
    w.bob.start();
    let state = w.bob.wait_for("publication after restart", |state| {
        recent_of(state).contains(&"README.md".to_owned())
    });
    assert_eq!(peers(&state)[0]["generation"], state["generation"]);
    assert_eq!(
        state["remote_links"]["app"]["name"], "Bob",
        "the choice to publish persists"
    );
}

/// Every file under `dir`, read whole, so a test can say a value is nowhere.
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_containing(&path, needle));
        } else if fs::read(&path)
            .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
        {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_secret_on_the_first_line_of_a_shared_prompt_never_reaches_the_log_a_briefing_or_the_remote() {
    let w = World::new();
    let key = &secret_shape("sk_", "live_51HqLZ2EYpK8dQ8X5vT3nR9bC7mWfJ0aG");
    let task = format!("STRIPE_KEY={key} deploy the payment fix\nsecond line");
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", &task),
        "admitted"
    );
    assert_eq!(
        w.bob.edits("bob-1", "b1", "README.md", "readme"),
        "admitted"
    );
    for (developer, name, share) in [(&w.alice, "Alice", "on"), (&w.bob, "Bob", "off")] {
        let (code, text) = developer.cli(&[
            "remote",
            "enable",
            "app",
            "--name",
            name,
            "--share-task",
            share,
            "--request-id",
            "en",
        ]);
        assert_eq!(code, 0, "{text}");
    }
    let seen = w.bob.wait_for("Alice's task line", |state| {
        peers(state)
            .first()
            .is_some_and(|peer| peer["publication"]["work"][0]["summary"].is_string())
    });
    assert_eq!(
        peers(&seen)[0]["publication"]["work"][0]["summary"],
        "STRIPE_KEY=[redacted] deploy the payment fix"
    );
    let context = w.bob.briefing("bob-1").expect("a briefing naming Alice");
    assert!(context.contains("STRIPE_KEY=[redacted]"), "{context}");
    assert!(!context.contains(key), "{context}");

    let (_, events) = w.alice.cli(&["events", "--limit", "1000"]);
    assert!(
        events.contains("[redacted]") && !events.contains(key),
        "{events}"
    );
    // Each coordinator publishes on its own pass, so Bob's ref can arrive
    // after Alice's line has already reached him.
    let deadline = Instant::now() + Duration::from_secs(30);
    let names = loop {
        let names = hub_refs(&w.hub(), "refs/langspan/peers/");
        if names.lines().count() >= 2 || Instant::now() >= deadline {
            break names;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(names.lines().count(), 2, "{names}");
    for name in names.lines() {
        let output = Process::new("git")
            .arg("-C")
            .arg(w.hub())
            .args(["cat-file", "blob", &format!("{name}:state.json")])
            .output()
            .unwrap();
        assert!(output.status.success());
        let published = String::from_utf8(output.stdout).unwrap();
        assert!(!published.contains(key), "{published}");
    }
    // Not in any database page, write-ahead log, receipt or scratch file of
    // either daemon, nor anywhere in the shared remote.
    for dir in [&w.alice.state, &w.bob.state, &w.hub()] {
        assert_eq!(files_containing(dir, key.as_bytes()), Vec::<PathBuf>::new());
    }
}

#[test]
fn local_coordination_keeps_working_while_a_credential_helper_hangs_the_remote() {
    let w = World::new();
    assert_eq!(
        w.alice.edits("alice-0", "a0", "src/first.rs", "t"),
        "admitted"
    );
    // A second remote that asks for credentials, answered by a helper that
    // never returns. Configured only in Alice's clone.
    let (url, _) = support::asks_for_credentials();
    let pid_file = w.base.join("helper.pid");
    let helper = w.base.join("helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 60\n",
            pid_file.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    git(&w.alice.clone, &["config", "credential.helper", ""]);
    git(
        &w.alice.clone,
        &[
            "config",
            "--add",
            "credential.helper",
            &format!("!{}", helper.display()),
        ],
    );
    git(&w.alice.clone, &["remote", "add", "slow", &url]);
    let (code, text) = w.alice.cli(&[
        "remote",
        "enable",
        "app",
        "--remote",
        "slow",
        "--name",
        "Alice",
        "--request-id",
        "en",
    ]);
    assert_eq!(code, 0, "{text}");
    let hung_by = Instant::now() + WAIT;
    while !pid_file.exists() {
        assert!(
            Instant::now() < hung_by,
            "the remote pass never reached the helper"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // While the remote pass is stuck, admissions, conflicts and briefings
    // answer as they always do.
    for (index, file) in ["src/a.rs", "src/b.rs", "README.md"].iter().enumerate() {
        let started = Instant::now();
        assert_eq!(
            w.alice.edits("alice-1", &format!("a{index}"), file, "t"),
            "admitted"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }
    assert!(
        w.alice.briefing("alice-2").is_some(),
        "alice-1's work is briefed"
    );
    let pid: i32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        support::alive(pid),
        "the helper is still hanging the remote pass"
    );
    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid).unwrap(),
        rustix::process::Signal::KILL,
    );
}

#[test]
fn remote_sent_lists_exactly_the_bytes_that_the_remote_holds() {
    let w = World::new();
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", "Add retry"),
        "admitted"
    );
    let (code, _) = w.alice.cli(&["remote", "sent", "app"]);
    assert_eq!(code, 1, "nothing was ever enabled, so nothing has left");
    let (code, text) = w.alice.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Alice",
        "--share-task",
        "on",
        "--request-id",
        "en",
    ]);
    assert_eq!(code, 0, "{text}");
    let participant = w.alice.status()["remote_links"]["app"]["participant"]
        .as_str()
        .unwrap()
        .to_owned();
    let on_remote = || {
        let output = Process::new("git")
            .arg("-C")
            .arg(w.hub())
            .args([
                "cat-file",
                "blob",
                &format!("refs/langspan/peers/{participant}:state.json"),
            ])
            .output()
            .unwrap();
        output.status.success().then_some(output.stdout)
    };
    let sent = || {
        let (code, text) = w.alice.cli(&["remote", "sent", "app", "--json"]);
        assert_eq!(code, 0, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone()
    };
    // The push comes before its record, so wait until both have settled on
    // the same publication.
    let deadline = Instant::now() + WAIT;
    let (listed, remote) = loop {
        let listed = sent();
        let newest = listed["sent"][0]["payload"].as_str().map(str::to_owned);
        if let (Some(newest), Some(remote)) = (newest, on_remote())
            && newest.as_bytes() == remote.as_slice()
        {
            break (listed, remote);
        }
        assert!(Instant::now() < deadline, "never settled: {listed:#}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let payload: Value = serde_json::from_slice(&remote).unwrap();
    assert_eq!(
        payload["publication"]["sequence"], listed["sent"][0]["sequence"],
        "{listed:#}"
    );
    assert!(listed["sent"][0]["published_at_ms"].as_u64().unwrap() > 0);
    let sequences: Vec<u64> = listed["sent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["sequence"].as_u64().unwrap())
        .collect();
    assert!(
        sequences.windows(2).all(|pair| pair[0] > pair[1]),
        "{sequences:?}"
    );
    let (code, text) = w.alice.cli(&["remote", "sent", "app", "--limit", "1"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(&String::from_utf8(remote).unwrap()),
        "the exact payload is printed: {text}"
    );
}

#[test]
fn a_remote_that_refuses_forced_updates_still_receives_every_publication() {
    // Found on a real second machine: a hosting proxy refused the forced
    // replacement of the publication ref. A bare repository with
    // receive.denyNonFastForwards refuses it the same way.
    let w = World::new();
    // `receive.denyNonFastForwards` only covers branches, so refuse every
    // non-fast-forward update with a server-side hook, as a proxy would.
    let hook = w.hub().join("hooks/update");
    fs::write(
        &hook,
        "#!/bin/sh\nzero=0000000000000000000000000000000000000000\n[ \"$2\" = \"$zero\" ] && exit 0\n[ \"$3\" = \"$zero\" ] && exit 0\ngit merge-base --is-ancestor \"$2\" \"$3\" || { echo 'forced update refused' >&2; exit 1; }\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", "first"),
        "admitted"
    );
    let (code, text) = w.alice.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Alice",
        "--request-id",
        "en-a",
    ]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(w.bob.edits("bob-1", "b0", "README.md", "t"), "admitted");
    let (code, text) = w.bob.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Bob",
        "--request-id",
        "en-b",
    ]);
    assert_eq!(code, 0, "{text}");
    w.bob.wait_for("Alice's first publication", |state| {
        recent_of(state).contains(&"src/api.rs".to_owned())
    });
    // A second, different publication can only arrive as a fast-forward.
    assert_eq!(
        w.alice.edits("alice-1", "a2", "README.md", "second"),
        "admitted"
    );
    let state = w.bob.wait_for("Alice's second publication", |state| {
        recent_of(state).contains(&"README.md".to_owned())
            && peers(state)
                .iter()
                .any(|peer| peer["publication"]["name"] == "Alice")
    });
    let alice = peers(&state)
        .into_iter()
        .find(|peer| peer["publication"]["name"] == "Alice")
        .unwrap();
    assert!(alice["publication"]["sequence"].as_u64().unwrap() >= 2);
}

#[test]
fn a_machine_where_no_hook_has_run_can_join_in_one_step() {
    let w = World::new();
    let (code, text) = w.bob.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Bob",
        "--request-id",
        "en-b",
    ]);
    assert_eq!(code, 1, "without --root there is no workspace yet: {text}");
    let root = w.bob.clone.to_str().unwrap().to_owned();
    let (code, text) = w.bob.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Bob",
        "--root",
        &root,
        "--request-id",
        "en-b2",
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("publishes to Git remote"), "{text}");
}

/// Every ref under `prefix` on the hub.
fn hub_refs(hub: &Path, prefix: &str) -> String {
    let output = Process::new("git")
        .arg("-C")
        .arg(hub)
        .args(["for-each-ref", "--format=%(refname)", prefix])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

/// A hub that behaves like the Git proxy measured on a cloud machine: it
/// accepts pushes only to branches, refuses every deletion and refuses
/// forced updates. Each attempt is written to `attempts` in the hub.
fn accept_only_branches(hub: &Path) {
    let hook = hub.join("hooks/update");
    fs::write(
        &hook,
        "#!/bin/sh\n\
         echo \"$1\" >> \"$GIT_DIR/attempts\"\n\
         zero=0000000000000000000000000000000000000000\n\
         case \"$1\" in refs/heads/*) ;; *) echo \"error: refusing $1: only branches may be pushed here\" >&2; exit 1;; esac\n\
         [ \"$3\" = \"$zero\" ] && { echo \"error: refusing to delete $1\" >&2; exit 1; }\n\
         [ \"$2\" = \"$zero\" ] && exit 0\n\
         git merge-base --is-ancestor \"$2\" \"$3\" || { echo 'error: refusing a forced update' >&2; exit 1; }\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_host_that_accepts_only_branches_takes_a_branch_publication_and_says_why_it_refuses_the_rest() {
    let w = World::new();
    accept_only_branches(&w.hub());
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", "first"),
        "admitted"
    );
    assert_eq!(w.bob.edits("bob-1", "b0", "README.md", "t"), "admitted");
    w.alice.enable("Alice", &["--publish-as-branch"], "en-a");
    let (_, doctor) = w.alice.cli(&["doctor"]);
    let alice = w.alice.participant();
    let alice_ref = format!("refs/heads/langspan/peers/{alice}");
    assert!(doctor.contains(&alice_ref), "{doctor}");
    w.bob.enable("Bob", &[], "en-b");

    // (a) A reader in the default namespace receives a branch publication.
    w.bob.wait_for("Alice's branch publication", |state| {
        recent_of(state).contains(&"src/api.rs".to_owned())
    });
    assert_eq!(hub_refs(&w.hub(), "refs/heads/langspan/").trim(), alice_ref);
    // A second, different publication arrives as a fast-forward of the branch.
    assert_eq!(
        w.alice.edits("alice-1", "a2", "README.md", "second"),
        "admitted"
    );
    w.bob.wait_for("Alice's second publication", |state| {
        recent_of(state).contains(&"README.md".to_owned())
    });

    // (c) Without the flag the host refuses Bob, and his log carries Git's
    // own refusal and the way out.
    let bob = w.bob.participant();
    let log = w
        .bob
        .wait_for_log("the refusal", |log| log.contains("refusing"));
    assert!(
        log.contains(&format!(
            "refusing refs/langspan/peers/{bob}: only branches may be pushed here"
        )),
        "{log}"
    );
    assert!(log.contains("Git said"), "{log}");
    assert!(log.contains("--publish-as-branch"), "{log}");
    assert_eq!(hub_refs(&w.hub(), "refs/langspan/"), "");
    // A refusal for good is not pushed again every pass.
    let attempts = || {
        fs::read_to_string(w.hub().join("attempts"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.ends_with(&bob))
            .count()
    };
    // Bob's publication changes once more when the Git observer learns his
    // branch; a changed publication is tried again, once.
    let deadline = Instant::now() + WAIT;
    while !w
        .bob
        .cli(&["remote", "outbox", "app"])
        .1
        .contains("refs/heads/bob/feature")
    {
        assert!(Instant::now() < deadline, "Bob's branch was never observed");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(800));
    let before = attempts();
    std::thread::sleep(Duration::from_millis(1500));
    // The host answered, so the pass is not backed off: Bob keeps receiving
    // while only his own push waits.
    for line in w.bob.log().lines().filter(|line| line.contains("refusing")) {
        let entry: Value = serde_json::from_str(line).unwrap();
        assert_eq!(entry["retry_in_ms"], 0, "{line}");
    }
    assert_eq!(
        attempts(),
        before,
        "seven passes, no new attempt: {}\n{}",
        fs::read_to_string(w.hub().join("attempts")).unwrap_or_default(),
        w.bob.log()
    );

    // (d) Alice stops. The host refuses the deletion: she is told so, with
    // the exact ref, and Bob stops showing her work anyway.
    let (code, _) = w
        .alice
        .cli(&["remote", "disable", "app", "--request-id", "dis-a"]);
    assert_eq!(code, 0);
    let log = w.alice.wait_for_log("the refused deletion", |log| {
        log.contains("refused to delete")
    });
    assert!(
        log.contains(&format!("refused to delete {alice_ref}")),
        "{log}"
    );
    assert!(log.contains("still on the remote"), "{log}");
    assert!(
        log.contains(&format!("git push origin --delete {alice_ref}")),
        "{log}"
    );
    w.bob
        .wait_for("Alice shown as gone", |state| peers(state).is_empty());
    assert!(
        !w.bob
            .briefing("bob-1")
            .unwrap_or_default()
            .contains("Alice"),
        "no briefing from a teammate who left"
    );
    assert_eq!(hub_refs(&w.hub(), "refs/heads/langspan/").trim(), alice_ref);
    let output = Process::new("git")
        .arg("-C")
        .arg(w.hub())
        .args(["cat-file", "blob", &format!("{alice_ref}:state.json")])
        .output()
        .unwrap();
    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["publication"]["withdrawn"], true);
    assert_eq!(payload["publication"]["work"], json!([]));
    // It is recorded like any publication: `remote sent` shows those bytes.
    let (code, sent) = w.alice.cli(&["remote", "sent", "app", "--json"]);
    assert_eq!(code, 0, "{sent}");
    assert!(
        sent.contains(
            &String::from_utf8(output.stdout.clone())
                .unwrap()
                .replace('"', "\\\"")
        ),
        "{sent}"
    );
    // Never claimed as removed.
    let state = w.alice.status();
    assert_eq!(state["remote_links"]["app"]["withdrawn"], false);
    let (_, doctor) = w.alice.cli(&["doctor"]);
    assert!(
        doctor.contains(&format!("its ref {alice_ref} has not yet been removed")),
        "{doctor}"
    );
}

#[test]
fn branch_and_default_publishers_see_each_other_and_a_move_leaves_one_ref() {
    let w = World::new();
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", "t"),
        "admitted"
    );
    assert_eq!(w.bob.edits("bob-1", "b0", "README.md", "t"), "admitted");
    w.alice.enable("Alice", &[], "en-a");
    w.bob.enable("Bob", &["--publish-as-branch"], "en-b");
    let alice = w.alice.participant();
    let bob = w.bob.participant();

    // (b) A participant publishing as a branch receives a default one, and
    // the other way round.
    w.bob.wait_for("Alice from the default namespace", |state| {
        recent_of(state).contains(&"src/api.rs".to_owned())
    });
    w.alice.wait_for("Bob from the branch namespace", |state| {
        recent_of(state).contains(&"README.md".to_owned())
    });
    assert_eq!(
        hub_refs(&w.hub(), "refs/heads/langspan/").trim(),
        format!("refs/heads/langspan/peers/{bob}")
    );
    let sequence = |state: &Value| {
        peers(state)
            .iter()
            .find(|peer| peer["publication"]["name"] == "Alice")
            .and_then(|peer| peer["publication"]["sequence"].as_u64())
    };
    let before = sequence(&w.bob.status()).unwrap();

    // Alice moves to the branch namespace: Bob keeps hearing her, and the
    // ref she no longer writes is removed.
    w.alice.enable("Alice", &["--publish-as-branch"], "en-a2");
    let state = w.bob.wait_for("Alice from her new ref", |state| {
        sequence(state).is_some_and(|now| now > before)
    });
    assert_eq!(peers(&state).len(), 1);
    let deadline = Instant::now() + WAIT;
    while !hub_refs(&w.hub(), "refs/langspan/").is_empty() {
        assert!(
            Instant::now() < deadline,
            "the old ref was never removed\n{}",
            w.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        hub_refs(&w.hub(), "refs/heads/langspan/")
            .contains(&format!("refs/heads/langspan/peers/{alice}"))
    );

    // Withdrawal deletes the ref in the namespace it is in.
    let (code, _) = w
        .bob
        .cli(&["remote", "disable", "app", "--request-id", "dis-b"]);
    assert_eq!(code, 0);
    w.bob.wait_for("Bob's ref removed", |state| {
        state["remote_links"]["app"]["withdrawn"] == true
    });
    assert!(!hub_refs(&w.hub(), "refs/heads/langspan/").contains(&bob));
    w.alice
        .wait_for("Bob gone", |state| peers(state).is_empty());

    // The owner's own branches and remote-tracking branches stay as they were.
    for developer in [&w.alice, &w.bob] {
        let output = Process::new("git")
            .arg("-C")
            .arg(&developer.clone)
            .args([
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/",
                "refs/remotes/",
            ])
            .output()
            .unwrap();
        let names = String::from_utf8(output.stdout).unwrap();
        assert!(!names.contains("langspan"), "{names}");
    }
}

/// Place `state_json` on the shared remote as participant `participant`'s
/// publication, as a teammate's coordinator would push it.
fn place_publication(hub: &Path, participant: &str, state_json: &[u8]) {
    let run = |args: &[&str], input: Option<&[u8]>| {
        let mut child = Process::new("git")
            .arg("-C")
            .arg(hub)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.unwrap_or_default())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let blob = run(&["hash-object", "-w", "--stdin"], Some(state_json));
    let tree = run(
        &["mktree"],
        Some(format!("100644 blob {blob}\tstate.json\n").as_bytes()),
    );
    let commit = run(&["commit-tree", &tree, "-m", "langspan"], None);
    run(
        &[
            "update-ref",
            &format!("refs/langspan/peers/{participant}"),
            &commit,
        ],
        None,
    );
}

/// A newer teammate's build may add fields to what it publishes. An older
/// receiver ignores them and shows the rest; the size and value bounds still
/// hold for everything it does read.
#[test]
fn a_teammates_publication_with_fields_this_build_does_not_know_is_still_shown() {
    let w = World::new();
    let newer = "c".repeat(32);
    let oversized = "d".repeat(32);
    let out_of_bounds = "e".repeat(32);
    let publication = |participant: &str, name: &str, host: &str, extra: &str| {
        format!(
            r#"{{"v":1,"from_a_newer_build":{{"nested":[1,2]}},"publication":{{"participant":"{participant}","name":"{name}","sequence":1,"added_later":true,"work":[{{"host":"{host}","session":"c0ffee00","branch_ref":"refs/heads/next","summary":null,"editing":["src/api.rs"],"recent":[],"region_hints":["Store::open"]{extra}}}]}}}}"#
        )
    };
    place_publication(
        &w.hub(),
        &newer,
        publication(&newer, "Carol", "codex", "").as_bytes(),
    );
    // Over the 16 KiB bound: refused, however its fields are named.
    let padding = format!(r#","padding":"{}""#, "x".repeat(17 * 1024));
    place_publication(
        &w.hub(),
        &oversized,
        publication(&oversized, "Dave", "codex", &padding).as_bytes(),
    );
    // Within the size bound but over a value bound (a 33-byte host name).
    place_publication(
        &w.hub(),
        &out_of_bounds,
        publication(&out_of_bounds, "Erin", &"h".repeat(33), "").as_bytes(),
    );
    let root = w.bob.clone.to_str().unwrap().to_owned();
    let (code, text) = w.bob.cli(&[
        "remote",
        "enable",
        "app",
        "--name",
        "Bob",
        "--root",
        &root,
        "--request-id",
        "en-b",
    ]);
    assert_eq!(code, 0, "{text}");
    let seen = w.bob.wait_for("Carol's publication", |state| {
        peers(state)
            .iter()
            .any(|peer| peer["publication"]["name"] == "Carol")
    });
    let carol = peers(&seen)
        .into_iter()
        .find(|peer| peer["publication"]["name"] == "Carol")
        .unwrap();
    assert_eq!(
        carol["publication"]["work"][0]["editing"],
        json!(["src/api.rs"])
    );
    assert!(
        carol["publication"].get("added_later").is_none(),
        "unknown fields are ignored, not kept"
    );
    // One more pass after Carol's: the other two are still not taken.
    std::thread::sleep(Duration::from_millis(600));
    let names: Vec<_> = peers(&w.bob.status())
        .iter()
        .map(|peer| peer["publication"]["name"].clone())
        .collect();
    assert_eq!(names, [json!("Carol")], "{names:?}");
    let context = w.bob.briefing("bob-1").unwrap_or_default();
    assert!(context.contains("Carol"), "{context}");
}

/// A hub that accepts both namespaces but refuses every deletion.
fn refuse_deletions(hub: &Path) {
    let hook = hub.join("hooks/update");
    fs::write(
        &hook,
        "#!/bin/sh\n\
         zero=0000000000000000000000000000000000000000\n\
         [ \"$3\" = \"$zero\" ] && { echo \"error: refusing to delete $1\" >&2; exit 1; }\n\
         exit 0\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_withdrawal_reaches_the_active_ref_when_an_older_one_already_says_withdrawn() {
    // Review of #29: a refused deletion of an old ref that already holds a
    // withdrawal must not end the withdrawal before the active ref in the
    // other namespace is reached.
    let w = World::new();
    refuse_deletions(&w.hub());
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", "t"),
        "admitted"
    );
    assert_eq!(w.bob.edits("bob-1", "b0", "README.md", "t"), "admitted");
    w.alice.enable("Alice", &[], "en-a");
    w.bob.enable("Bob", &[], "en-b");
    w.bob.wait_for("Alice", |state| !peers(state).is_empty());

    // Alice stops: the deletion is refused, so the default ref says withdrawn.
    let (code, _) = w
        .alice
        .cli(&["remote", "disable", "app", "--request-id", "dis-a1"]);
    assert_eq!(code, 0);
    w.bob
        .wait_for("Alice shown as gone", |state| peers(state).is_empty());

    // She comes back publishing as a branch, with a higher sequence.
    w.alice.enable("Alice", &["--publish-as-branch"], "en-a2");
    w.bob.wait_for("Alice back from the branch", |state| {
        !peers(state).is_empty()
    });

    // She stops again: Bob must stop showing her, though the first ref he
    // can't delete already says withdrawn.
    let (code, _) = w
        .alice
        .cli(&["remote", "disable", "app", "--request-id", "dis-a2"]);
    assert_eq!(code, 0);
    w.bob
        .wait_for("Alice gone again", |state| peers(state).is_empty());
}

#[test]
fn following_the_suggested_branch_command_publishes_at_once_after_a_refusal() {
    // Review of #29: a refusal held for one destination must not hold the
    // same work for another. Alice's default ref is refused for good; she
    // follows the suggestion, and her unchanged work goes out at once.
    let w = World::new();
    accept_only_branches(&w.hub());
    assert_eq!(
        w.alice.edits("alice-1", "a1", "src/api.rs", "t"),
        "admitted"
    );
    w.alice.enable("Alice", &[], "en-a");
    w.alice
        .wait_for_log("the refusal", |log| log.contains("--publish-as-branch"));
    let moved = Instant::now();
    w.alice.enable("Alice", &["--publish-as-branch"], "en-a2");
    let alice = w.alice.participant();
    while hub_refs(&w.hub(), "refs/heads/langspan/").trim()
        != format!("refs/heads/langspan/peers/{alice}")
    {
        assert!(
            moved.elapsed() < Duration::from_secs(30),
            "the branch publication waited on the old refusal\n{}",
            w.alice.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
