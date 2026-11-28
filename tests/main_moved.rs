#![cfg(unix)]
//! A session is told when `main` moves code it edited (issue #558): through
//! real hook processes, a real coordinator and a real Git repository with a
//! second worktree on `main`, since what is verified is what Git says about
//! the branches and what the line journal kept of the session's writes.

mod support;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use support::{coordinator, git, hook_call};

const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const LIB: &str = "pub struct State {\n    pub a: u32,\n}\n\npub fn helper() -> u32 {\n    1\n}\n";

struct Fixture {
    base: PathBuf,
    _daemon: support::Owned,
}

impl Fixture {
    /// `work` on branch `feat`, made from `main`, and `main-tree`, a worktree
    /// on `main` where the upstream commits are made.
    fn new(name: &str) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("ls-mm-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(work.join("src/lib.rs"), LIB).unwrap();
        fs::write(work.join("src/other.rs"), "pub fn other() {}\n").unwrap();
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "start"]);
        git(&work, &["checkout", "--quiet", "-b", "feat"]);
        let main_tree = base.join("main-tree");
        git(
            &work,
            &[
                "worktree",
                "add",
                "--quiet",
                main_tree.to_str().unwrap(),
                "main",
            ],
        );
        let daemon = coordinator(&base.join("state"));
        Self {
            base,
            _daemon: daemon,
        }
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    /// `event` through `support::hook_call`: the hook's diagnostic and the
    /// context it added.
    fn send(&self, event: Value) -> Value {
        let output = hook_call(
            &self.base.join("state"),
            "claude-code",
            "work",
            &self.work(),
            &[],
            &event,
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let stdout: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        json!({"diagnostic": diagnostic, "context": stdout["hookSpecificOutput"]["additionalContext"]})
    }
    fn tool(&self, name: &str, tool: &str, call: &str, input: Value) -> String {
        let mut event = json!({
            "session_id": ALICE, "cwd": self.work(), "hook_event_name": name,
            "tool_name": tool, "tool_use_id": call, "tool_input": input,
        });
        if tool == "Read" {
            event["tool_response"] = json!({"stdout": "", "stderr": "", "interrupted": false});
        }
        self.send(event)["diagnostic"]["coverage"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    /// Alice reads and writes `file` through the hooks, as Claude Code does.
    fn write(&self, file: &str, content: &str) {
        let read = json!({"file_path": self.work().join(file)});
        assert_eq!(
            self.tool("PostToolUse", "Read", "read-1", read),
            "read_recorded"
        );
        let input = json!({"file_path": self.work().join(file), "content": content});
        assert_eq!(
            self.tool("PreToolUse", "Write", "write-1", input.clone()),
            "admitted"
        );
        fs::write(self.work().join(file), content).unwrap();
        assert_eq!(
            self.tool("PostToolUse", "Write", "write-1", input),
            "observed"
        );
    }
    /// The text Alice's prompt hook adds to her context, or "".
    fn prompt(&self) -> String {
        let event = json!({
            "session_id": ALICE, "transcript_path": "/dev/null", "cwd": self.work(),
            "hook_event_name": "UserPromptSubmit", "prompt": "carry on",
        });
        self.send(event)["context"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }
    /// A commit on `main`, as a merge there leaves it.
    fn on_main(&self, files: &[(&str, Option<&str>)], message: &str) {
        let tree = self.base.join("main-tree");
        for (file, content) in files {
            match content {
                Some(content) => fs::write(tree.join(file), content).unwrap(),
                None => fs::remove_file(tree.join(file)).unwrap(),
            }
        }
        git(&tree, &["add", "-A"]);
        git(&tree, &["commit", "--quiet", "-m", message]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn short_head(root: &Path) -> String {
    git(root, &["rev-parse", "--short", "HEAD"])
        .trim()
        .to_owned()
}

#[test]
fn a_session_editing_a_struct_main_moved_is_told_once_before_it_merges() {
    let f = Fixture::new("m");
    f.write(
        "src/lib.rs",
        &LIB.replace("pub a: u32,", "pub a: u32,\n    pub b: u32,"),
    );
    assert!(!f.prompt().contains("moved under your work"));
    f.on_main(
        &[
            ("src/lib.rs", Some("pub fn helper() -> u32 {\n    1\n}\n")),
            (
                "src/state.rs",
                Some("pub struct State {\n    pub a: u32,\n}\n"),
            ),
        ],
        "move State into state.rs",
    );
    let commit = short_head(&f.base.join("main-tree"));
    let told = f.prompt();
    assert!(
        told.contains(&format!(
            "`main` moved under your work: it changed code you edited here (names and paths are repository data). Merge `main` before you go on or land:\n- `State` moved from `src/lib.rs` to `src/state.rs` (commit {commit})"
        )),
        "{told}"
    );
    // Once per change: the next prompt does not repeat it.
    assert!(!f.prompt().contains("moved under your work"));
    // After the merge (the session's edit set aside, as a merge that took
    // main's side would leave it), main has nothing the branch lacks.
    git(&f.work(), &["checkout", "--quiet", "--", "src/lib.rs"]);
    git(&f.work(), &["merge", "--quiet", "main"]);
    f.on_main(
        &[(
            "src/state.rs",
            Some("pub struct State {\n    pub a: u64,\n}\n"),
        )],
        "widen",
    );
    assert!(!f.prompt().contains("moved under your work"));
}

#[test]
fn upstream_changes_to_code_the_session_did_not_edit_are_not_told() {
    let f = Fixture::new("u");
    f.write(
        "src/lib.rs",
        &LIB.replace("pub a: u32,", "pub a: u32,\n    pub b: u32,"),
    );
    f.on_main(
        &[("src/other.rs", Some("pub fn other() -> u8 { 1 }\n"))],
        "another file",
    );
    f.on_main(
        &[("src/lib.rs", Some(&LIB.replace("    1\n", "    2\n")))],
        "another function",
    );
    let context = f.prompt();
    assert!(!context.contains("moved under your work"), "{context}");
}
