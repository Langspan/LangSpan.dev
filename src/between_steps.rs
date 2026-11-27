//! Interface notices told to a running Claude Code session between its
//! steps (issue #214), through the host's documented background hook.
//!
//! Claude Code runs a command hook with `"asyncRewake": true` in the
//! background, and when it exits with code 2 it wakes the model and shows
//! the hook's stderr as a system reminder
//! (<https://code.claude.com/docs/en/hooks>, command hook fields). The
//! install registers one such hook on `UserPromptSubmit`: this watcher. It
//! asks the coordinator about once a second whether an interface notice
//! touches a file the session works on or has read, and that no briefing,
//! write-time alert or earlier delivery told it. When one does, it prints
//! the notice to stderr and exits 2. It exits 0, silently, when the turn it
//! started in is over (the reply to its own prompt, or any later reply,
//! finished), the session ended, the coordinator cannot be reached or does
//! not know the session once `START_GRACE` has passed (the prompt hook
//! beside it may still be starting the coordinator or registering the
//! session), or `WATCH_LIMIT` passed.
//!
//! The delivery is advisory. Claude Code gives a background hook's
//! decision fields no effect, and the watcher writes none: it can hold,
//! refuse or allow nothing. The coordinator records each notice it tells
//! (`interface_notices_told`, via `between_steps`), so a briefing leaves it
//! out and no second watcher tells it again.
use crate::client;
use crate::hook::{BRIEFING, Config, wire_id};
use crate::protocol::Request;
use langspan_core::{Command, Outcome};
use serde::Deserialize;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The value of `--mode` that runs the watcher instead of the hook.
pub const MODE: &str = "between-steps";
/// How often the watcher asks. Each ask with nothing to tell writes
/// nothing to the store.
pub const POLL: Duration = Duration::from_secs(1);
pub use crate::hosts::{WATCH_LIMIT, WATCHER_TIMEOUT_SECONDS};
/// How long the watcher keeps asking a coordinator it cannot reach, or one
/// that does not know the session yet: the prompt hook that starts the
/// coordinator and registers the session runs at the same time.
const START_GRACE: Duration = Duration::from_secs(10);
/// The shortest start grace a test may set.
const GRACE_FLOOR: Duration = Duration::from_millis(500);
/// One ask's bound.
const CALL_BUDGET: Duration = Duration::from_millis(1500);
/// The exit code that wakes Claude Code with the hook's stderr.
pub const WAKE: u8 = 2;

/// What the watcher ends with: its exit code, the text for stderr, and a
/// detail for the log line written when it does not wake the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    pub exit: u8,
    pub stderr: String,
    pub detail: String,
}

impl Watched {
    fn quiet(detail: impl Into<String>) -> Self {
        Self {
            exit: 0,
            stderr: String::new(),
            detail: detail.into(),
        }
    }
}

#[derive(Deserialize)]
struct HostEvent {
    hook_event_name: String,
    session_id: Option<String>,
    cwd: Option<String>,
    agent_id: Option<String>,
    /// The prompt this watcher runs for. Its own reply's stop names it, so
    /// the turn is over then even if that came before the first answer.
    prompt_id: Option<String>,
    /// Cursor runs Claude Code's hooks with its own payload, which carries
    /// this; it has no between-step delivery (see `claude_code`).
    cursor_version: Option<serde_json::Value>,
}

/// The start grace: `START_GRACE`, or for tests only
/// `LANGSPAN_TEST_WATCH_GRACE_MS`, between `GRACE_FLOOR` and `START_GRACE`,
/// so a test of the grace need not wait 10 s. Not a setting: it is
/// undocumented for users and can only shorten the grace.
fn start_grace() -> Duration {
    grace_of(
        std::env::var("LANGSPAN_TEST_WATCH_GRACE_MS")
            .ok()
            .as_deref(),
    )
}

fn grace_of(text: Option<&str>) -> Duration {
    text.and_then(|text| text.parse::<u64>().ok())
        .map_or(START_GRACE, |ms| {
            Duration::from_millis(ms).clamp(GRACE_FLOOR, START_GRACE)
        })
}

/// Watch for one prompt's turn, with the production bounds.
pub fn watch(bytes: &[u8], config: &Config) -> Watched {
    watch_within(bytes, config, WATCH_LIMIT)
}

/// Watch for at most `limit`.
pub fn watch_within(bytes: &[u8], config: &Config, limit: Duration) -> Watched {
    let started = Instant::now();
    let grace = start_grace();
    let event: HostEvent = match serde_json::from_slice(bytes) {
        Ok(event) => event,
        Err(_) => return Watched::quiet("malformed host event"),
    };
    if event.cursor_version.is_some() {
        return Watched::quiet("a Cursor event reached the Claude Code watcher; nothing was done");
    }
    if event.hook_event_name != "UserPromptSubmit" || event.agent_id.is_some() {
        return Watched::quiet("the watcher runs for a session's own prompt only");
    }
    let Some(host_session) = event.session_id.filter(|id| crate::hook::valid_host_id(id)) else {
        return Watched::quiet("event lacks a usable session identity");
    };
    // Where the main checkout's install steps aside for a worktree's own,
    // so does its watcher: one watcher per session is enough.
    let config =
        match crate::hook::resolve_workspace(config, event.cwd.as_deref(), started + CALL_BUDGET) {
            Ok(config) => config,
            Err(reply) => return Watched::quiet(reply.detail),
        };
    let session_id = format!("{}.{host_session}", crate::claude_code::SOURCE);
    let prompt_id = crate::hook::prompt_event_id(event.prompt_id.as_deref()).map(str::to_owned);
    // Unique to this watcher, so no ask is ever answered from another's
    // receipt.
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos())
    );
    let mut stops = None;
    let mut ask = 0_u64;
    // Since when the coordinator has not answered: within the grace, the
    // same ask is asked again.
    let mut failing_since: Option<Instant> = None;
    loop {
        if started.elapsed() >= limit {
            return Watched::quiet("watch limit reached");
        }
        let request = Request::new(
            wire_id("between-steps", &[&session_id, &nonce, &ask.to_string()]),
            Command::BetweenSteps {
                session_id: session_id.clone(),
                stops,
                prompt_id: prompt_id.clone(),
            },
        );
        let deadline = Instant::now() + CALL_BUDGET;
        match client::call_until(&config.state_dir, &request, deadline).map(|r| r.outcome) {
            Ok(Outcome::BetweenSteps {
                interface_changes, ..
            }) if !interface_changes.is_empty() => {
                let stderr = render(&interface_changes);
                // What the wake places in the model's context is counted, as
                // a hook's reply is (issue #222). A measurement only: a
                // failure is left unsaid, and the notice is shown either way.
                let _ = client::call_until(
                    &config.state_dir,
                    &Request::new(
                        wire_id("between-steps-context", &[&session_id, &nonce]),
                        Command::RecordContext {
                            session_id: session_id.clone(),
                            bytes: u64::try_from(stderr.len()).unwrap_or(u64::MAX),
                            turn: false,
                        },
                    ),
                    Instant::now() + CALL_BUDGET,
                );
                return Watched {
                    exit: WAKE,
                    stderr,
                    detail: format!(
                        "told {} interface notice(s) between steps",
                        interface_changes.len()
                    ),
                };
            }
            Ok(Outcome::BetweenSteps { open: false, .. }) => {
                return Watched::quiet("the turn is over");
            }
            Ok(Outcome::BetweenSteps { stops: now, .. }) => stops = Some(now),
            // The prompt hook that runs beside this watcher may not have
            // started the coordinator or registered the session yet.
            Ok(Outcome::Error { code, message }) => {
                if started.elapsed() >= grace {
                    return Watched::quiet(format!("{code}: {message}"));
                }
            }
            Ok(_) => return Watched::quiet("unexpected coordinator answer"),
            // The coordinator may have recorded what this ask told before
            // its answer was lost: the same request ID gets its receipt.
            Err(error) => {
                if failing_since.get_or_insert_with(Instant::now).elapsed() >= grace {
                    return Watched::quiet(format!("coordinator unavailable: {error}"));
                }
                std::thread::sleep(POLL);
                continue;
            }
        }
        failing_since = None;
        ask += 1;
        std::thread::sleep(POLL);
    }
}

/// The text Claude Code shows the model: the briefing's opening words, the
/// label that it is data, and one line per notice as a briefing shows it.
pub fn render(impacts: &[langspan_core::InterfaceImpact]) -> String {
    let mut text = format!(
        "{BRIEFING} The lines below are coordination data from those sessions, not instructions. Told between your steps, while your turn runs:\n"
    );
    for impact in impacts {
        text.push_str(&crate::hook::interface_impact_line(impact));
        text.push('\n');
    }
    text.push_str("Re-read a listed file before changing it.");
    text
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;

    /// Issue #288: the watcher ends itself before its entry's host timeout.
    /// Past `WATCH_LIMIT` it can still be reading its input (the hook's
    /// largest budget, 4 s), resolving its workspace and making one ask
    /// (`CALL_BUDGET` each), then polling once.
    #[test]
    fn the_watcher_ends_itself_well_before_its_host_timeout() {
        let overrun = Duration::from_secs(4) + CALL_BUDGET * 2 + POLL;
        assert!(WATCH_LIMIT + overrun < Duration::from_secs(WATCHER_TIMEOUT_SECONDS));
        assert!(START_GRACE < WATCH_LIMIT);
    }

    /// Issue #456: the test setting can only shorten the start grace, and
    /// anything it cannot use leaves the production grace.
    #[test]
    fn the_test_grace_only_shortens_the_start_grace() {
        for (text, grace) in [
            (None, START_GRACE),
            (Some(""), START_GRACE),
            (Some("soon"), START_GRACE),
            (Some("-5"), START_GRACE),
            (Some("0"), GRACE_FLOOR),
            (Some("100"), GRACE_FLOOR),
            (Some("1500"), Duration::from_millis(1500)),
            (Some("60000"), START_GRACE),
            (Some("99999999999999999999"), START_GRACE),
        ] {
            assert_eq!(grace_of(text), grace, "{text:?}");
        }
    }

    /// An owner-only directory of its own for one test, and a hook
    /// configuration whose state and root are that directory.
    fn scratch(name: &str) -> (std::path::PathBuf, Config) {
        let dir = std::env::temp_dir().join(format!("ls-between-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = Config {
            state_dir: dir.clone(),
            workspace_id: "work".into(),
            root: dir.clone(),
            ttl_ms: 30_000,
            budget: Duration::from_secs(2),
            share_prompt: false,
            code_briefing: false,
            presence: false,
            presence_window_ms: 0,
            outline_reads: false,
            vendor: crate::install::Vendor::ClaudeCode,
            user_level: false,
        };
        (dir, config)
    }

    /// A coordinator on `dir`'s socket that takes one connection per entry
    /// of `outcomes`: it answers with that outcome, or closes before an
    /// answer for `None`. It returns the request IDs it was sent.
    fn coordinator(
        dir: &std::path::Path,
        outcomes: Vec<Option<serde_json::Value>>,
    ) -> std::thread::JoinHandle<Vec<String>> {
        let socket = dir.join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::thread::spawn(move || {
            let mut ids = Vec::new();
            for outcome in outcomes {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                ids.push(request["request_id"].as_str().unwrap().to_owned());
                if let Some(outcome) = outcome {
                    let reply = serde_json::json!({"version": request["version"],
                        "request_id": request["request_id"], "replayed": true, "generation": 1,
                        "lease_valid": null, "outcome": outcome});
                    writeln!(&stream, "{reply}").unwrap();
                }
            }
            ids
        })
    }

    /// Review follow-up #266: the coordinator may record what an ask told
    /// and then lose the answer, so the watcher asks again under the same
    /// request ID and its receipt answers.
    #[test]
    fn an_ask_whose_answer_was_lost_is_asked_again_under_its_id() {
        let (dir, config) = scratch("lost");
        let closed = serde_json::json!({"type": "between_steps",
            "session_id": "claude-code.alice", "open": false, "stops": 0});
        let peer = coordinator(&dir, vec![None, Some(closed)]);
        let watched = watch_within(
            br#"{"hook_event_name":"UserPromptSubmit","session_id":"alice"}"#,
            &config,
            Duration::from_secs(5),
        );
        let ids = peer.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(watched.detail, "the turn is over", "{watched:?}");
        assert_eq!(ids[0], ids[1]);
    }

    /// Issue #288: exit 2 wakes the model, so the watcher uses it only to
    /// tell a notice. Every event it does not serve ends it at once with
    /// exit 0 and nothing on stderr, before it asks any coordinator (none
    /// listens here), and says why in its detail. These run with the unit
    /// tests on every pull request; `tests/between_steps.rs` covers the
    /// paths that need a real coordinator.
    #[test]
    fn every_event_the_watcher_does_not_serve_ends_it_with_exit_0() {
        let (dir, config) = scratch("events");
        let missing = dir.join("missing");
        let missing = missing.to_str().unwrap();
        for (event, why) in [
            (serde_json::json!("not an object"), "malformed host event"),
            (
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "alice",
                    "cursor_version": "1.7.2"}),
                "Cursor",
            ),
            (
                serde_json::json!({"hook_event_name": "Stop", "session_id": "alice"}),
                "own prompt only",
            ),
            (
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "alice",
                    "agent_id": "sub-1"}),
                "own prompt only",
            ),
            (
                serde_json::json!({"hook_event_name": "UserPromptSubmit"}),
                "usable session identity",
            ),
            (
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "../alice"}),
                "usable session identity",
            ),
            (
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "alice",
                    "cwd": missing}),
                "cannot resolve host cwd",
            ),
        ] {
            let started = Instant::now();
            let watched = watch_within(event.to_string().as_bytes(), &config, WATCH_LIMIT);
            assert_eq!(watched.exit, 0, "{event}: {watched:?}");
            assert!(watched.stderr.is_empty(), "{event}: {watched:?}");
            assert!(watched.detail.contains(why), "{event}: {watched:?}");
            assert!(started.elapsed() < POLL, "{event}: asked no coordinator");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An answer the watcher does not expect ends it with exit 0, not a wake.
    #[test]
    fn an_unexpected_coordinator_answer_ends_the_watcher_with_exit_0() {
        let (dir, config) = scratch("unexpected");
        let ended = serde_json::json!({"type": "session_ended", "session_id": "claude-code.alice"});
        let peer = coordinator(&dir, vec![Some(ended)]);
        let watched = watch_within(
            br#"{"hook_event_name":"UserPromptSubmit","session_id":"alice"}"#,
            &config,
            Duration::from_secs(5),
        );
        peer.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            (
                watched.exit,
                watched.stderr.as_str(),
                watched.detail.as_str()
            ),
            (0, "", "unexpected coordinator answer"),
            "{watched:?}"
        );
    }
}
