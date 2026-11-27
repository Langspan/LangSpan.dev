//! `langspan demo`: a real refusal with no agent signed in (issue #290).
//!
//! The demo makes a temporary directory with a home, a state directory and a
//! workspace of its own, starts a real coordinator there, and sends it two
//! example Claude Code sessions' `PreToolUse` events through the same hook
//! code an installed hook runs. It prints the hook's refusal text as the
//! agent would receive it, then stops the coordinator and removes the
//! directory. It never reads or writes the user's state directory, host
//! settings or workspaces: the coordinator's `HOME` is the temporary one.

use crate::{autostart, claude_code, client, hook};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// How long the demo's coordinator has to accept connections.
const READY_WITHIN: Duration = Duration::from_secs(10);
/// The budget for each example hook call: the most an installed hook allows.
const HOOK_BUDGET: Duration = Duration::from_millis(4000);
/// The file both example sessions edit, relative to the demo's workspace.
const HELD: &str = "src/session.rs";

/// The demo's temporary directory and coordinator. Dropping it stops the
/// coordinator and removes the directory, also after an error or a panic.
struct Scratch {
    dir: PathBuf,
    coordinator: Option<Child>,
}

impl Scratch {
    fn create() -> Result<Self, crate::Error> {
        let name = format!(
            "langspan-demo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.subsec_nanos())
        );
        // The system's temporary directory, unless the coordinator's socket
        // would not fit under it (macOS's is long); then `/tmp`.
        let fits = |parent: &PathBuf| {
            crate::local::check_socket_fits(&parent.join(&name).join("state")).is_ok()
        };
        let parent = [std::env::temp_dir(), PathBuf::from("/tmp")]
            .into_iter()
            .find(fits)
            .ok_or(
                "no temporary directory is short enough for the coordinator's socket; \
                 set TMPDIR to a shorter directory",
            )?;
        let dir = parent.join(name);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let scratch = Self {
            dir: fs::canonicalize(&dir)?,
            coordinator: None,
        };
        fs::DirBuilder::new().mode(0o700).create(scratch.home())?;
        fs::create_dir_all(scratch.root().join("src"))?;
        fs::write(
            scratch.root().join(HELD),
            "pub fn refresh(token: &str) -> String {\n    token.to_owned()\n}\n",
        )?;
        Ok(scratch)
    }
    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }
    fn state_dir(&self) -> PathBuf {
        self.dir.join("state")
    }
    fn root(&self) -> PathBuf {
        self.dir.join("work")
    }
    fn log(&self) -> PathBuf {
        self.dir.join("coordinator.log")
    }

    /// Starts `executable daemon` for the demo's state directory, with the
    /// demo's home as `HOME`, and waits until it accepts connections.
    fn start(&mut self, executable: &Path) -> Result<(), crate::Error> {
        let log = fs::File::create(self.log())?;
        let child = Command::new(executable)
            .arg("daemon")
            .arg("--state-dir")
            .arg(self.state_dir())
            .env_clear()
            .envs(autostart::child_environment(
                std::env::vars_os(),
                Some(&self.home()),
            ))
            .env(autostart::ENV, "off")
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", executable.display()))?;
        self.coordinator = Some(child);
        let deadline = Instant::now() + READY_WITHIN;
        while !client::accepts(&self.state_dir(), deadline) {
            let exited = self
                .coordinator
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten());
            if exited.is_some() || Instant::now() >= deadline {
                let mut error = if exited.is_some() {
                    "the demo's coordinator exited before it accepted connections".to_owned()
                } else {
                    format!(
                        "the demo's coordinator did not accept connections within {} seconds",
                        READY_WITHIN.as_secs()
                    )
                };
                // The log is removed with the directory, so its last line
                // is the only trace of why.
                if let Some(last) = fs::read_to_string(self.log())
                    .ok()
                    .and_then(|log| log.lines().last().map(str::to_owned))
                {
                    error.push_str(&format!("; its last log line: {last}"));
                }
                return Err(error.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    /// Stops the coordinator and removes the directory, saying what failed.
    fn remove(mut self) -> Result<(), crate::Error> {
        self.stop();
        let dir = std::mem::take(&mut self.dir);
        fs::remove_dir_all(&dir).map_err(|error| {
            format!(
                "cannot remove {}: {error}; remove it yourself",
                dir.display()
            )
            .into()
        })
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.coordinator.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.stop();
        if !self.dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// One example session's hook calls, as Claude Code sends them.
struct Session<'a> {
    name: &'static str,
    config: &'a hook::Config,
}

/// One edit of `HELD`: the text it replaces and the text that replaces it.
type Change = (&'static str, &'static str);
const ALICE_CHANGE: Change = ("token.to_owned()", "token.trim().to_owned()");
const BOB_CHANGE: Change = ("token.trim().to_owned()", "token.trim().to_lowercase()");

impl Session<'_> {
    fn call(&self, phase: &str, tool: &str, tool_use_id: &str, input: Value) -> hook::Reply {
        let event = json!({
            "session_id": self.name,
            "transcript_path": "/dev/null",
            "cwd": self.config.root,
            "permission_mode": "default",
            "hook_event_name": phase,
            "tool_name": tool,
            "tool_input": input,
            "tool_use_id": tool_use_id,
        });
        claude_code::run(event.to_string().as_bytes(), self.config)
    }
    fn edit(&self, phase: &str, tool_use_id: &str, (old, new): Change) -> hook::Reply {
        let input = json!({
            "file_path": self.config.root.join(HELD),
            "old_string": old,
            "new_string": new,
            "replace_all": false
        });
        self.call(phase, "Edit", tool_use_id, input)
    }
    /// A `PreToolUse` that must be admitted: no decision, so the host's
    /// own permission flow goes on.
    fn admitted(&self, tool_use_id: &str, change: Change) -> Result<(), crate::Error> {
        let reply = self.edit("PreToolUse", tool_use_id, change);
        if reply.coverage != "admitted" || !reply.stdout.is_empty() {
            return Err(unexpected(self.name, "admitted", &reply));
        }
        Ok(())
    }
    /// What the host does after an admitted edit: write it, then report it
    /// done, which ends the hold.
    fn finish(&self, tool_use_id: &str, change @ (old, new): Change) -> Result<(), crate::Error> {
        let path = self.config.root.join(HELD);
        let text = fs::read_to_string(&path)?;
        fs::write(&path, text.replacen(old, new, 1))?;
        let reply = self.edit("PostToolUse", tool_use_id, change);
        if matches!(reply.coverage, "timeout" | "unknown") {
            return Err(unexpected(self.name, "recorded as done", &reply));
        }
        Ok(())
    }
    /// A completed `Read` of the file, as the host reports it.
    fn read(&self, tool_use_id: &str) -> Result<(), crate::Error> {
        let input = json!({ "file_path": self.config.root.join(HELD) });
        let reply = self.call("PostToolUse", "Read", tool_use_id, input);
        if reply.coverage != "read_recorded" {
            return Err(unexpected(self.name, "read", &reply));
        }
        Ok(())
    }
}

/// The demo stopped because the hook answered otherwise than expected.
fn unexpected(name: &str, expected: &str, reply: &hook::Reply) -> crate::Error {
    format!(
        "the demo expected {name}'s call on {HELD} to be {expected}, but the hook answered {} ({}). Report this as a LangSpan bug.",
        reply.coverage, reply.detail
    )
    .into()
}

/// Runs the demo, writing what happens to `out`. `executable` is the
/// `langspan` binary whose coordinator the demo starts.
pub fn run(executable: &Path, out: &mut dyn Write) -> Result<(), crate::Error> {
    let mut scratch = Scratch::create()?;
    scratch.start(executable)?;
    let config = hook::Config {
        state_dir: scratch.state_dir(),
        workspace_id: "demo".into(),
        root: scratch.root(),
        ttl_ms: 30_000,
        budget: HOOK_BUDGET,
        share_prompt: false,
        code_briefing: false,
        presence: false,
        presence_window_ms: langspan_core::PRESENCE_WINDOW_MS,
        vendor: crate::install::Vendor::ClaudeCode,
        user_level: false,
    };
    let alice = Session {
        name: "alice",
        config: &config,
    };
    let bob = Session {
        name: "bob",
        config: &config,
    };
    writeln!(
        out,
        "LangSpan demo: two example sessions, alice and bob, edit one workspace. No agent runs."
    )?;
    writeln!(
        out,
        "A coordinator for this demo only runs in {}.",
        scratch.dir.display()
    )?;
    writeln!(out)?;

    alice.admitted("toolu_alice_1", ALICE_CHANGE)?;
    writeln!(
        out,
        "1. alice's edit of {HELD}: admitted. alice holds the file until the edit ends."
    )?;

    let refused = bob.edit("PreToolUse", "toolu_bob_1", ALICE_CHANGE);
    let reason = denial_reason(&refused).ok_or_else(|| unexpected("bob", "refused", &refused))?;
    writeln!(
        out,
        "2. bob's edit of {HELD}: refused. The hook tells bob's agent:"
    )?;
    writeln!(out, "  {reason}")?;

    alice.finish("toolu_alice_1", ALICE_CHANGE)?;
    writeln!(out, "3. alice's edit is written. alice's hold ends.")?;

    bob.read("toolu_bob_2")?;
    bob.admitted("toolu_bob_3", BOB_CHANGE)?;
    writeln!(
        out,
        "4. bob reads {HELD} with alice's change, then edits it again: admitted."
    )?;
    writeln!(out)?;

    scratch.remove()?;
    writeln!(
        out,
        "Removed the demo's coordinator and files. Nothing else was changed."
    )?;
    writeln!(
        out,
        "Next: open two agent sessions in one Git repository and ask both to change the same file."
    )?;
    Ok(())
}

/// The reason in a Claude Code `deny` answer, or `None` for any other.
fn denial_reason(reply: &hook::Reply) -> Option<String> {
    let answer: Value = serde_json::from_str(&reply.stdout).ok()?;
    let output = answer.get("hookSpecificOutput")?;
    (output.get("permissionDecision")? == "deny")
        .then(|| {
            output
                .get("permissionDecisionReason")?
                .as_str()
                .map(str::to_owned)
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The demo directories this process left in either temporary parent.
    fn left_behind() -> Vec<PathBuf> {
        let prefix = format!("langspan-demo-{}-", std::process::id());
        [std::env::temp_dir(), PathBuf::from("/tmp")]
            .iter()
            .filter_map(|parent| fs::read_dir(parent).ok())
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .collect()
    }

    #[test]
    fn a_coordinator_that_cannot_start_or_exits_leaves_nothing_behind() {
        let mut out = Vec::new();
        let error = run(Path::new("/nonexistent/langspan"), &mut out).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot start /nonexistent/langspan"),
            "{error}"
        );
        // `false` exits at once and logs nothing, as a coordinator that
        // fails early would.
        let error = run(Path::new("false"), &mut out).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the demo's coordinator exited before it accepted connections"
        );
        // A coordinator that logs why it stopped: the reason is kept.
        let script =
            std::env::temp_dir().join(format!("langspan-demo-test-{}", std::process::id()));
        fs::write(
            &script,
            "#!/bin/sh\necho 'cannot open the store' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let error = run(&script, &mut out).unwrap_err();
        let _ = fs::remove_file(&script);
        assert_eq!(
            error.to_string(),
            "the demo's coordinator exited before it accepted connections; its last log line: cannot open the store"
        );
        assert!(out.is_empty());
        assert_eq!(left_behind(), Vec::<PathBuf>::new());
    }

    #[test]
    fn only_a_deny_answer_has_a_reason() {
        let reply = |decision: &str| hook::Reply {
            stdout: json!({"hookSpecificOutput": {
                "permissionDecision": decision,
                "permissionDecisionReason": "held",
            }})
            .to_string(),
            coverage: "denied",
            detail: String::new(),
        };
        assert_eq!(denial_reason(&reply("deny")).as_deref(), Some("held"));
        assert_eq!(denial_reason(&reply("ask")), None);
        for stdout in ["", "not json"] {
            let reply = hook::Reply {
                stdout: stdout.into(),
                ..reply("deny")
            };
            assert_eq!(denial_reason(&reply), None);
        }
    }
}
