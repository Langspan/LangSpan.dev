#![cfg(unix)]
//! Break alerts at write time (ADR 0029): a write whose call does not fit a
//! signature another live session just changed is told so, before the
//! write's result, through the host's context. Real daemon, real observer
//! and indexer, a real Git repository and the real hook executable.

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

static NEXT: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(60);
const HEADER: &str = "LangSpan: a call in this write";

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

struct Run {
    coverage: String,
    detail: String,
    context: Option<String>,
    elapsed: Duration,
}

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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const FILES: &[(&str, &str)] = &[
    (
        "src/api.rs",
        "pub fn resize(width: u32) -> u32 {\n    width\n}\n",
    ),
    (
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(1);\n}\n",
    ),
    ("app/api.py", "def scale(factor):\n    return factor\n"),
    (
        "app/main.py",
        "from api import scale\n\n\ndef main():\n    return scale(2)\n",
    ),
    (
        "web/api.ts",
        "export function render(view: string): string {\n  return view;\n}\n",
    ),
    (
        "web/main.ts",
        "import { render } from \"./api\";\n\nexport function main() {\n  return render(\"home\");\n}\n",
    ),
    (
        "cmd/api.go",
        "package main\n\nfunc Encode(value int) int {\n\treturn value\n}\n",
    ),
    (
        "cmd/main.go",
        "package main\n\nfunc main() {\n\tEncode(1)\n}\n",
    ),
    (
        "shop/Orders.java",
        "package shop;\n\npublic class Orders {\n    public Orders() {}\n\n    public Order place(Cart cart) {\n        return null;\n    }\n}\n",
    ),
    (
        "shop/Checkout.java",
        "package shop;\n\nclass Checkout {\n    Order submit(Orders orders, Cart cart) {\n        return orders.place(cart);\n    }\n}\n",
    ),
    (
        "Billing/Invoices.cs",
        "namespace Billing;\n\npublic class Invoices\n{\n    public Invoices() { }\n\n    public Invoice Issue(Order order) => null;\n}\n",
    ),
    (
        "Billing/Desk.cs",
        "namespace Billing;\n\nclass Desk\n{\n    Invoice Close(Invoices invoices, Order order)\n    {\n        return invoices.Issue(order);\n    }\n}\n",
    ),
];

impl World {
    /// A repository with each indexed language's files, or with `git: false`
    /// a plain directory, which is never scanned or indexed.
    fn new(git_repo: bool) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-breaks-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let work = base.join("work");
        for (path, text) in FILES {
            let full = work.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        if git_repo {
            git(&work, &["init", "--quiet", "--initial-branch=main"]);
            git(&work, &["add", "."]);
            git(&work, &["commit", "--quiet", "-m", "init"]);
        }
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self { base, daemon: None };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(&state)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while world.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let root = world.work();
        let added = world.cli(&[
            "workspace",
            "add",
            "app",
            root.to_str().unwrap(),
            "--request-id",
            "ws",
        ]);
        assert_eq!(added.0, 0, "{}", added.1);
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
    fn coverage(&self) -> Value {
        let (_, text) = self.cli(&["graph", "symbol", "app", "resize", "--json"]);
        serde_json::from_str::<Value>(&text).unwrap()["outcome"]["coverage"].clone()
    }
    fn wait_for_index(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let coverage = self.coverage();
            if coverage["current_files"] == FILES.len()
                && coverage["indexable_files"] == FILES.len()
            {
                return;
            }
            assert!(Instant::now() < deadline, "never indexed: {coverage:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// Wait until the coordinator holds an interface notice for `name`.
    fn wait_for_notice(&self, name: &str, after: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let (_, events) = self.cli(&["events", "--limit", "1000", "--json"]);
            if events.contains("interface_changed")
                && events.contains(&format!("\"name\":\"{name}\""))
                && events.contains(after)
            {
                return;
            }
            assert!(Instant::now() < deadline, "no notice for {name}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn hook(&self, vendor: &str, event: &Value) -> Run {
        let started = Instant::now();
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
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
        let elapsed = started.elapsed();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let reply: Option<Value> = serde_json::from_str(&stdout).ok();
        if let Some(reply) = reply
            .as_ref()
            .filter(|_| event["hook_event_name"] == "PreToolUse")
        {
            // Never a decision: the host's own permission flow is untouched.
            let specific = &reply["hookSpecificOutput"];
            assert_eq!(specific["hookEventName"], "PreToolUse", "{stdout}");
            assert!(specific.get("permissionDecision").is_none(), "{stdout}");
        }
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or("").into(),
            context: reply.and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            }),
            elapsed,
        }
    }
    fn claude(&self, session: &str, call: &str, tool: &str, input: Value, phase: &str) -> Run {
        self.hook(
            "claude-code",
            &json!({
                "session_id": session, "cwd": self.work(), "hook_event_name": phase,
                "tool_name": tool, "tool_use_id": call, "tool_input": input,
            }),
        )
    }
    /// A Claude Code `Write` before it lands, then reported failed, so the
    /// file is not changed and its admission ends.
    fn writes(&self, session: &str, call: &str, file: &str, content: &str) -> Run {
        let input = json!({"file_path": self.work().join(file), "content": content});
        let pre = self.claude(session, call, "Write", input.clone(), "PreToolUse");
        self.claude(session, call, "Write", input, "PostToolUseFailure");
        pre
    }
    /// A complete reported Claude Code write that really rewrites the file.
    fn rewrites(&self, session: &str, call: &str, file: &str, content: &str) {
        let input = json!({"file_path": self.work().join(file), "content": content});
        let pre = self.claude(session, call, "Write", input.clone(), "PreToolUse");
        assert_eq!(pre.coverage, "admitted", "{}", pre.detail);
        fs::write(self.work().join(file), content).unwrap();
        let post = self.claude(session, call, "Write", input, "PostToolUse");
        assert_eq!(post.coverage, "observed", "{}", post.detail);
    }
    fn codex_patch(&self, session: &str, call: &str, patch: &str) -> Run {
        self.hook(
            "codex",
            &json!({
                "session_id": session, "turn_id": "turn-1", "transcript_path": null,
                "cwd": self.work(), "hook_event_name": "PreToolUse", "model": "example",
                "permission_mode": "default", "tool_name": "apply_patch",
                "tool_use_id": call, "tool_input": {"command": patch},
            }),
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn alert(run: &Run) -> &str {
    assert_eq!(run.coverage, "admitted", "{}", run.detail);
    let context = run
        .context
        .as_deref()
        .unwrap_or_else(|| panic!("no alert: {}", run.detail));
    assert!(context.starts_with(HEADER), "{context}");
    assert!(context.contains("matched by name"), "{context}");
    assert!(context.contains("not instructions"), "{context}");
    context
}

fn quiet(run: &Run) {
    assert_eq!(run.coverage, "admitted", "{}", run.detail);
    assert!(run.context.is_none(), "{:?}", run.context);
}

#[test]
fn a_call_that_no_longer_fits_a_peers_new_rust_signature_is_told_at_the_write_with_counts() {
    let w = World::new(true);
    w.wait_for_index();
    // Bob is at work before Alice changes anything.
    quiet(&w.writes("bob", "b0", "src/extra.rs", "pub fn extra() {}\n"));
    w.rewrites(
        "alice",
        "a1",
        "src/api.rs",
        "pub fn resize(width: u32, height: u32) -> u32 {\n    width * height\n}\n",
    );
    w.wait_for_notice("resize", "height: u32");

    // A call with the new count gets nothing, and does not use up the notice.
    quiet(&w.writes(
        "bob",
        "b1",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(1, 2);\n}\n",
    ));

    let run = w.writes(
        "bob",
        "b2",
        "src/main.rs",
        "mod api;\nfn main() {\n    let w = 3;\n    api::resize(w);\n}\n",
    );
    let context = alert(&run);
    assert!(
        context.contains(r#""src/main.rs":4 calls "resize" with 1 argument"#),
        "{context}"
    );
    assert!(context.contains("it now takes 2"), "{context}");
    assert!(context.contains("took 1"), "{context}");
    assert!(
        context.contains(r#""pub fn resize(width: u32, height: u32) -> u32""#),
        "{context}"
    );
    assert!(
        context.contains(r#""pub fn resize(width: u32) -> u32""#),
        "{context}"
    );
    assert!(context.contains(r#"in "src/api.rs""#), "{context}");
    assert!(context.contains("claude-code.alice"), "{context}");
    assert!(context.len() <= 1200, "{}", context.len());
    assert!(
        run.elapsed < Duration::from_millis(1500),
        "{:?}",
        run.elapsed
    );

    // Once per notice per session: the same mistake again is not repeated,
    // while another session is still told.
    quiet(&w.writes(
        "bob",
        "b3",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(9);\n}\n",
    ));
    alert(&w.writes(
        "carol",
        "c1",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(9);\n}\n",
    ));
    // The session that made the change is not told about its own change.
    quiet(&w.writes(
        "alice",
        "a2",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(9);\n}\n",
    ));
    // Only the event that the alert was given is recorded, never the code.
    let (_, events) = w.cli(&["events", "--limit", "1000", "--json"]);
    assert!(events.contains("interface_alerted"), "{events}");
    assert!(!events.contains("let w = 3"), "{events}");
}

#[test]
fn python_typescript_and_go_calls_are_checked_through_both_hosts() {
    let w = World::new(true);
    w.wait_for_index();
    w.rewrites(
        "alice",
        "a1",
        "app/api.py",
        "def scale(factor, offset):\n    return factor + offset\n",
    );
    w.rewrites(
        "alice",
        "a2",
        "web/api.ts",
        "export function render(view: string, theme: string): string {\n  return view + theme;\n}\n",
    );
    w.rewrites(
        "alice",
        "a3",
        "cmd/api.go",
        "package main\n\nfunc Encode(value, base int) int {\n\treturn value * base\n}\n",
    );
    w.wait_for_notice("scale", "offset");
    w.wait_for_notice("render", "theme");
    w.wait_for_notice("Encode", "base int");

    // Gemini CLI reads no context on BeforeTool, so its write is not
    // checked: an alert there would use up the notice unread.
    let replace = json!({"file_path": w.work().join("app/main.py"),
                         "old_string": "    return scale(2)\n",
                         "new_string": "    total = scale(2)\n    return total\n"});
    let gemini = |phase: &str, response: Value| {
        w.hook(
            "gemini-cli",
            &json!({"session_id": "gina", "cwd": w.work(), "hook_event_name": phase,
                    "tool_name": "replace", "tool_input": replace, "tool_response": response}),
        )
    };
    let before = gemini("BeforeTool", Value::Null);
    quiet(&before);
    assert!(!before.detail.contains("break"), "{}", before.detail);
    gemini(
        "AfterTool",
        json!({"error": {"message": "not run", "type": "edit_no_occurrence_found"}}),
    );

    // Python, through a Claude Code Edit: only the edited lines are checked.
    let edit = w.claude(
        "bob",
        "b1",
        "Edit",
        json!({"file_path": w.work().join("app/main.py"),
               "old_string": "    return scale(2)\n",
               "new_string": "    total = scale(2)\n    return total\n"}),
        "PreToolUse",
    );
    let context = alert(&edit);
    assert!(
        context.contains(r#""app/main.py":5 calls "scale" with 1 argument"#),
        "{context}"
    );
    assert!(
        context.contains(r#""def scale(factor, offset)""#),
        "{context}"
    );

    // TypeScript, through a Codex patch: a right count first, then a wrong one.
    quiet(&w.codex_patch(
        "dave",
        "d1",
        "*** Begin Patch\n*** Update File: web/main.ts\n@@\n export function main() {\n-  return render(\"home\");\n+  return render(\"home\", \"dark\");\n }\n*** End Patch",
    ));
    let patch = w.codex_patch(
        "dave",
        "d2",
        "*** Begin Patch\n*** Update File: web/main.ts\n@@\n export function main() {\n-  return render(\"home\");\n+  const page = render(\"home\");\n+  return page;\n }\n*** End Patch",
    );
    let context = alert(&patch);
    assert!(
        context.contains(r#""web/main.ts":4 calls "render" with 1 argument"#),
        "{context}"
    );
    assert!(context.contains("it now takes 2"), "{context}");

    // Go, through a Codex patch that adds a file.
    let added = w.codex_patch(
        "dave",
        "d3",
        "*** Begin Patch\n*** Add File: cmd/extra.go\n+package main\n+\n+func extra() int {\n+\treturn Encode(5)\n+}\n*** End Patch",
    );
    let context = alert(&added);
    assert!(
        context.contains(r#""cmd/extra.go":4 calls "Encode" with 1 argument"#),
        "{context}"
    );
    assert!(
        context.contains(r#""func Encode(value, base int) int""#),
        "{context}"
    );
}

/// Roadmap D6, Java: the issue's example. A peer adds a parameter to a
/// public method; a write that keeps the one-argument call is told.
#[test]
fn a_java_method_call_that_no_longer_fits_is_told_at_the_write() {
    let w = World::new(true);
    w.wait_for_index();
    w.rewrites(
        "alice",
        "a1",
        "shop/Orders.java",
        "package shop;\n\npublic class Orders {\n    public Orders(Store store) {}\n\n    public Order place(Cart cart, Coupon coupon) {\n        return null;\n    }\n}\n",
    );
    w.wait_for_notice("place", "Coupon coupon");
    w.wait_for_notice("Orders", "Store store");
    let edit = w.claude(
        "bob",
        "b1",
        "Edit",
        json!({"file_path": w.work().join("shop/Checkout.java"),
               "old_string": "        return orders.place(cart);\n",
               "new_string": "        Order order = orders.place(cart);\n        return order;\n"}),
        "PreToolUse",
    );
    let context = alert(&edit);
    assert!(
        context.contains(r#""shop/Checkout.java":5 calls "place" with 1 argument"#),
        "{context}"
    );
    assert!(
        context.contains(r#""public Order place(Cart cart, Coupon coupon)""#),
        "{context}"
    );
    // A constructor call is a call of the type's name. Carol, since one
    // notice is told to a session once.
    let created = w.writes(
        "carol",
        "c1",
        "shop/Shop.java",
        "package shop;\n\nclass Shop {\n    Orders open() {\n        return new Orders();\n    }\n}\n",
    );
    let context = alert(&created);
    assert!(
        context.contains(r#""shop/Shop.java":5 calls "Orders" with 0 arguments"#),
        "{context}"
    );
    assert!(
        context.contains(r#""public Orders(Store store)""#),
        "{context}"
    );
    // A call with the new count is not told.
    quiet(&w.claude(
        "bob",
        "b3",
        "Edit",
        json!({"file_path": w.work().join("shop/Checkout.java"),
               "old_string": "        return orders.place(cart);\n",
               "new_string": "        return orders.place(cart, null);\n"}),
        "PreToolUse",
    ));
}

/// Roadmap D6, C#: a peer adds a parameter to a public method and to a
/// constructor; writes that keep the old counts are told.
#[test]
fn a_csharp_method_call_that_no_longer_fits_is_told_at_the_write() {
    let w = World::new(true);
    w.wait_for_index();
    w.rewrites(
        "alice",
        "a1",
        "Billing/Invoices.cs",
        "namespace Billing;\n\npublic class Invoices\n{\n    public Invoices(Ledger ledger) { }\n\n    public Invoice Issue(Order order, Tax tax) => null;\n}\n",
    );
    w.wait_for_notice("Issue", "Tax tax");
    w.wait_for_notice("Invoices", "Ledger ledger");
    let edit = w.claude(
        "bob",
        "b1",
        "Edit",
        json!({"file_path": w.work().join("Billing/Desk.cs"),
               "old_string": "        return invoices.Issue(order);\n",
               "new_string": "        var invoice = invoices.Issue(order);\n        return invoice;\n"}),
        "PreToolUse",
    );
    let context = alert(&edit);
    assert!(
        context.contains(r#""Billing/Desk.cs":7 calls "Issue" with 1 argument"#),
        "{context}"
    );
    assert!(
        context.contains(r#""public Invoice Issue(Order order, Tax tax)""#),
        "{context}"
    );
    // A constructor call is a call of the type's name. Carol, since one
    // notice is told to a session once.
    let created = w.writes(
        "carol",
        "c1",
        "Billing/Front.cs",
        "namespace Billing;\n\nclass Front\n{\n    Invoices Open()\n    {\n        return new Invoices();\n    }\n}\n",
    );
    let context = alert(&created);
    assert!(
        context.contains(r#""Billing/Front.cs":7 calls "Invoices" with 0 arguments"#),
        "{context}"
    );
    assert!(
        context.contains(r#""public Invoices(Ledger ledger)""#),
        "{context}"
    );
    // A call with the new count is not told.
    quiet(&w.claude(
        "bob",
        "b3",
        "Edit",
        json!({"file_path": w.work().join("Billing/Desk.cs"),
               "old_string": "        return invoices.Issue(order);\n",
               "new_string": "        return invoices.Issue(order, null);\n"}),
        "PreToolUse",
    ));
}

#[test]
fn without_a_ready_index_a_write_is_not_checked_and_not_delayed() {
    // A plain directory is never scanned, so the index holds nothing for it.
    let w = World::new(false);
    w.rewrites(
        "alice",
        "a1",
        "src/api.rs",
        "pub fn resize(width: u32, height: u32) -> u32 {\n    width * height\n}\n",
    );
    // The notice the indexer would have sent, sent by hand.
    let request = json!({"version": 3, "request_id": "notice-1", "command": {
        "op": "observe_interface", "workspace_id": "app", "path": "src/api.rs",
        "changes": [{"name": "resize", "kind": "function", "change": "signature_changed",
                     "before": "pub fn resize(width: u32) -> u32",
                     "after": "pub fn resize(width: u32, height: u32) -> u32",
                     "before_arity": {"min": 1, "max": 1, "receiver": false},
                     "after_arity": {"min": 2, "max": 2, "receiver": false}}],
        "dependents": ["src/main.rs"]}});
    let (code, text) = w.cli(&["request", &request.to_string(), "--json"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("interface_observed"), "{text}");

    let run = w.writes(
        "bob",
        "b1",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(1);\n}\n",
    );
    quiet(&run);
    assert!(
        run.detail
            .contains("break check skipped: the source index is not ready"),
        "{}",
        run.detail
    );
    assert!(
        run.elapsed < Duration::from_millis(1500),
        "{:?}",
        run.elapsed
    );
}

#[test]
fn a_second_same_named_definition_coming_or_going_is_not_a_changed_signature_at_the_write() {
    let w = World::new(true);
    w.wait_for_index();
    quiet(&w.writes("bob", "b0", "src/extra.rs", "pub fn extra() {}\n"));
    // Alice adds a second `resize`, in another module of the same file,
    // with the same signature as the first.
    let one = "pub fn resize(width: u32) -> u32 {\n    width\n}\n";
    let two = format!(
        "{one}\npub mod wide {{\n    pub fn resize(width: u32) -> u32 {{\n        width * 2\n    }}\n}}\n"
    );
    w.rewrites("alice", "a1", "src/api.rs", &two);
    w.wait_for_notice("resize", "\"definition_added\"");
    let (_, events) = w.cli(&["events", "--limit", "1000", "--json"]);
    assert!(
        events.contains(r#""before_count":1"#) && events.contains(r#""after_count":2"#),
        "{events}"
    );
    assert!(!events.contains("signature_changed"), "{events}");
    // Nothing a call was written against changed: no alert, whatever the
    // call's argument count.
    quiet(&w.writes(
        "bob",
        "b1",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(1, 2);\n}\n",
    ));

    // Alice takes it away again: a removal, still no alert.
    w.rewrites("alice", "a2", "src/api.rs", one);
    w.wait_for_notice("resize", "\"definition_removed\"");
    quiet(&w.writes(
        "bob",
        "b2",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(3, 4);\n}\n",
    ));

    // A real change to the one left is alerted to, as before.
    w.rewrites(
        "alice",
        "a3",
        "src/api.rs",
        "pub fn resize(width: u32, height: u32) -> u32 {\n    width * height\n}\n",
    );
    w.wait_for_notice("resize", "height: u32");
    let run = w.writes(
        "bob",
        "b3",
        "src/main.rs",
        "mod api;\nfn main() {\n    api::resize(5);\n}\n",
    );
    let context = alert(&run);
    assert!(context.contains("it now takes 2"), "{context}");
}

#[test]
fn a_notice_left_out_of_a_full_alert_is_told_at_a_later_write() {
    let w = World::new(true);
    w.wait_for_index();
    // Three changed signatures, each long enough that the alert's 1,200
    // bytes hold only some of them.
    let long = |c: &str| c.repeat(150);
    w.rewrites(
        "alice",
        "a1",
        "app/api.py",
        &format!(
            "def scale(factor, offset_{}):\n    return factor\n",
            long("o")
        ),
    );
    w.rewrites(
        "alice",
        "a2",
        "web/api.ts",
        &format!(
            "export function render(view: string, theme_{}: string): string {{\n  return view;\n}}\n",
            long("t")
        ),
    );
    w.rewrites(
        "alice",
        "a3",
        "cmd/api.go",
        &format!(
            "package main\n\nfunc Encode(value, base_{} int) int {{\n\treturn value\n}}\n",
            long("b")
        ),
    );
    w.wait_for_notice("scale", "offset_o");
    w.wait_for_notice("render", "theme_t");
    w.wait_for_notice("Encode", "base_b");

    // Calls are matched by name, so one Rust file can break all three.
    let calls = "fn extra() {\n    scale(1);\n    render(1);\n    Encode(1);\n}\n";
    let first = w.writes("bob", "b1", "src/extra.rs", calls);
    let first = alert(&first).to_owned();
    assert!(first.contains("more not shown"), "{first}");
    let told = |context: &str| {
        ["\"scale\"", "\"render\"", "\"Encode\""]
            .into_iter()
            .filter(|name| context.contains(&format!("calls {name}")))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let shown = told(&first);
    assert!(!shown.is_empty() && shown.len() < 3, "{first}");

    // The notices that did not fit were not used up: the next write with
    // the same calls names the rest, and not the ones already told.
    let second = w.writes("bob", "b2", "src/extra.rs", calls);
    let second = alert(&second).to_owned();
    let rest = told(&second);
    assert!(!rest.is_empty(), "{second}");
    assert!(
        rest.iter().all(|name| !shown.contains(name)),
        "{first}\n{second}"
    );
}
