//! Desktop notices for the person (roadmap H3,
//! [ADR 0019](../docs/adr/0019-person-facing-surfaces-and-usage-accounting.md)).
//!
//! The daemon raises a notice from events it already records: a refusal, an
//! automatic check that failed, a handoff nobody has taken up. A notice is
//! shown by the platform's own notifier, `osascript` on macOS and
//! `notify-send` elsewhere, as a child process with a 2-second deadline, on
//! a thread of its own: no hook and no request ever waits for one. Nothing
//! reaches the model, nothing is held or refused, nothing is recorded as an
//! event, and no network call is made. Off unless the owner switches it on.

use langspan_core::{CheckResult, Event, Message, Recipient, RefusalKind, State};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// Every notice's title.
pub const TITLE: &str = "LangSpan";
/// The longest body, in bytes.
pub const MAX_BODY: usize = 200;
/// The most paths one body names; the rest are counted.
pub const MAX_PATHS: usize = 3;
/// How long a notifier may run before it is killed.
pub const DEADLINE: Duration = Duration::from_secs(2);
/// Notices waiting for the notifier thread; more are dropped.
pub const QUEUE: usize = 16;

/// What a notice is about. One notice of a kind per `kind_interval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A write or commit was refused (a `write_refused` event).
    Refusal,
    /// A handoff no session has taken up for `handoff_after`.
    HandoffWaiting,
    /// An automatic check failed (ADR 0030).
    CheckFailed,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Refusal => "refusal",
            Kind::HandoffWaiting => "handoff_waiting",
            Kind::CheckFailed => "check_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: Kind,
    /// At most `MAX_BODY` bytes: workspace, session and paths only, never a
    /// prompt, message text or file content.
    pub body: String,
}

/// The owner's choices, from `config.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub on: bool,
    pub sound: bool,
    /// At most one notice of a kind this often.
    pub kind_interval: Duration,
    /// At most this many notices in any hour, of all kinds.
    pub hourly_limit: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            on: false,
            sound: false,
            kind_interval: Duration::from_secs(300),
            hourly_limit: 10,
        }
    }
}

/// The rate limits. Pure: the caller supplies the time.
#[derive(Debug, Default)]
pub struct Limiter {
    last: BTreeMap<Kind, Instant>,
    /// When each notice of the last hour was let through.
    hour: VecDeque<Instant>,
}

const HOUR: Duration = Duration::from_secs(3600);

impl Limiter {
    /// Let `kind` through at `now`, or say which limit drops it.
    pub fn admit(
        &mut self,
        kind: Kind,
        now: Instant,
        settings: &Settings,
    ) -> Result<(), &'static str> {
        while self
            .hour
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= HOUR)
        {
            self.hour.pop_front();
        }
        if self
            .last
            .get(&kind)
            .is_some_and(|at| now.saturating_duration_since(*at) < settings.kind_interval)
        {
            return Err("kind_limit");
        }
        if self.hour.len() >= settings.hourly_limit as usize {
            return Err("hourly_limit");
        }
        self.last.insert(kind, now);
        self.hour.push_back(now);
        Ok(())
    }
}

/// The platform's notifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Osascript,
    NotifySend,
}

impl Program {
    /// The one this platform uses.
    pub fn here() -> Self {
        if cfg!(target_os = "macos") {
            Program::Osascript
        } else {
            Program::NotifySend
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Program::Osascript => "osascript",
            Program::NotifySend => "notify-send",
        }
    }
    /// Its arguments. The title and body are passed as arguments, never
    /// spliced into a script, so no path can change what runs. A sound is
    /// part of the same call: nothing else is started for it. With sound
    /// off, `notify-send` passes the freedesktop `suppress-sound` hint, so a
    /// desktop set to play a default sound stays quiet; `display
    /// notification` without a `sound name` plays no sound on macOS.
    pub fn args(self, body: &str, sound: bool) -> Vec<String> {
        match self {
            Program::Osascript => {
                let mut display =
                    "display notification (item 2 of argv) with title (item 1 of argv)".to_owned();
                if sound {
                    display.push_str(" sound name \"Glass\"");
                }
                // The title comes first, so a body that begins with `-` is
                // never read as an option.
                [
                    "-e",
                    "on run argv",
                    "-e",
                    &display,
                    "-e",
                    "end run",
                    TITLE,
                    body,
                ]
                .map(str::to_owned)
                .to_vec()
            }
            Program::NotifySend => {
                let mut args = vec![format!("--app-name={TITLE}")];
                args.push(if sound {
                    "--hint=string:sound-name:message-new-instant".into()
                } else {
                    "--hint=boolean:suppress-sound:true".into()
                });
                args.extend(["--".into(), TITLE.into(), body.into()]);
                args
            }
        }
    }
}

/// The notifier on `path` (a `PATH` value), if there is one: the first
/// regular, executable file of its name.
pub fn find(path: Option<&OsStr>) -> Option<(Program, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let program = Program::here();
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program.name()))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .map(|found| (program, found))
}

/// Why showing a notice failed. Each is logged once per daemon run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Failure {
    Missing,
    Failed,
    TimedOut,
}

impl Failure {
    fn name(self) -> &'static str {
        match self {
            Failure::Missing => "notifier_missing",
            Failure::Failed => "notifier_failed",
            Failure::TimedOut => "notifier_timed_out",
        }
    }
}

/// Run the notifier once, killing its process group at `DEADLINE`.
pub fn show(program: &Path, args: &[String]) -> Result<(), (Failure, String)> {
    use std::os::unix::process::CommandExt;
    let started = Instant::now();
    let mut child = loop {
        let spawned = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        match spawned {
            Ok(child) => break child,
            // A file just written can be busy for a moment (another
            // process's fork still holds it open).
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && started.elapsed() < DEADLINE =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err((Failure::Failed, format!("cannot start: {error}"))),
        }
    };
    let group = rustix::process::Pid::from_child(&child);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err((Failure::Failed, format!("exited with {status}"))),
            Ok(None) if started.elapsed() >= DEADLINE => {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                let _ = child.kill();
                let _ = child.wait();
                return Err((
                    Failure::TimedOut,
                    format!("killed after {} ms", DEADLINE.as_millis()),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                let _ = child.kill();
                let _ = child.wait();
                return Err((Failure::Failed, error.to_string()));
            }
        }
    }
}

fn log(value: serde_json::Value) {
    eprintln!("{value}");
}

/// The notifier thread. `settings` is read for each notice, so `langspan
/// notify on|off` takes effect without a restart; an unreadable
/// configuration counts as off.
pub fn worker(notices: mpsc::Receiver<Notice>, settings: impl Fn() -> Result<Settings, String>) {
    let mut limiter = Limiter::default();
    let mut logged = BTreeSet::<Failure>::new();
    let mut settings_error: Option<String> = None;
    while let Ok(notice) = notices.recv() {
        let settings = match settings() {
            Ok(settings) => {
                settings_error = None;
                settings
            }
            Err(detail) => {
                if settings_error.as_ref() != Some(&detail) {
                    log(
                        serde_json::json!({"level":"warn","event":"notify_config_unreadable","detail":detail}),
                    );
                    settings_error = Some(detail);
                }
                continue;
            }
        };
        if !settings.on {
            continue;
        }
        let kind = notice.kind.name();
        let dropped = |reason: &str| {
            log(
                serde_json::json!({"level":"info","event":"notice_dropped","kind":kind,"reason":reason}),
            )
        };
        if let Err(reason) = limiter.admit(notice.kind, Instant::now(), &settings) {
            dropped(reason);
            continue;
        }
        let result = match find(std::env::var_os("PATH").as_deref()) {
            Some((program, path)) => show(&path, &program.args(&notice.body, settings.sound)),
            None => Err((
                Failure::Missing,
                format!("no {} on PATH", Program::here().name()),
            )),
        };
        match result {
            Ok(()) => log(serde_json::json!({"level":"info","event":"notice_sent","kind":kind})),
            Err((failure, detail)) => {
                dropped(failure.name());
                if logged.insert(failure) {
                    log(serde_json::json!({"level":"warn","event":failure.name(),"detail":detail}));
                }
            }
        }
    }
}

/// `text` with control characters replaced, at most `limit` bytes. With
/// `tail`, a cut keeps the end (a path's file name), else the start.
fn clean(text: &str, limit: usize, tail: bool) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if text.len() <= limit {
        return text;
    }
    let room = limit.saturating_sub('…'.len_utf8());
    if tail {
        let mut start = text.len() - room;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        format!("…{}", &text[start..])
    } else {
        format!("{}…", cut(&text, room))
    }
}

fn cut(text: &str, limit: usize) -> &str {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A session as a notice names it: its host and the start of its ID.
pub fn session(session_id: &str) -> String {
    match session_id.split_once('.') {
        Some((host, id)) => format!("{}.{}", clean(host, 24, false), clean(id, 11, false)),
        None => clean(session_id, 16, false),
    }
}

/// The workspace as a notice names it: its directory's name.
fn workspace(state: &State, workspace_id: &str) -> String {
    let name = state
        .workspaces
        .get(workspace_id)
        .and_then(|workspace| Path::new(&workspace.root).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace_id.to_owned());
    clean(&name, 32, false)
}

fn paths(paths: &[String]) -> String {
    let mut named: Vec<String> = paths
        .iter()
        .take(MAX_PATHS)
        .map(|path| clean(path, 48, true))
        .collect();
    if paths.len() > MAX_PATHS {
        named.push(format!("{} more", paths.len() - MAX_PATHS));
    }
    named.join(", ")
}

/// The body, cut to `MAX_BODY` bytes.
fn body(text: String) -> String {
    if text.len() <= MAX_BODY {
        return text;
    }
    format!("{}…", cut(&text, MAX_BODY - '…'.len_utf8()))
}

/// The events a notice can come from, for the store's tap.
pub fn relevant(event: &Event) -> bool {
    matches!(
        event,
        Event::WriteRefused { .. }
            | Event::CheckReported {
                notice: langspan_core::CheckNotice {
                    result: CheckResult::Failed { .. },
                    ..
                },
                ..
            }
    )
}

/// The notices for events recorded together, in order. One check record's
/// result can be reported in several events, one per group of sessions told
/// the same (issue #69): it raises one notice.
pub fn notices_for(state: &State, events: &[Event]) -> Vec<Notice> {
    let mut reported = std::collections::BTreeSet::new();
    events
        .iter()
        .filter(|event| match event {
            Event::CheckReported { notice, .. } => reported.insert(notice.check_id.as_str()),
            _ => true,
        })
        .filter_map(|event| notice_for(state, event))
        .collect()
}

/// The notice for a recorded event, if it calls for one. `state` is the
/// state after it was applied.
pub fn notice_for(state: &State, event: &Event) -> Option<Notice> {
    match event {
        Event::WriteRefused {
            session_id,
            workspace_id,
            kind,
            paths: refused,
            peer_session_id,
            ..
        } => {
            let who = session(session_id);
            let on = paths(refused);
            let peer = peer_session_id.as_deref().map(session);
            let what = match (kind, peer) {
                (RefusalKind::Conflict, Some(peer)) => format!("refused on {on} (held by {peer})"),
                (RefusalKind::Conflict, None) => format!("refused on {on} (held)"),
                (RefusalKind::StaleBase, _) => {
                    format!("refused on {on} (changed since it read them)")
                }
                (RefusalKind::UnreadLines, Some(peer)) => {
                    format!("refused on {on} (lines {peer} wrote, unread)")
                }
                (RefusalKind::UnreadLines, None) => format!("refused on {on} (lines unread)"),
                (RefusalKind::CommitHeld, Some(peer)) => {
                    format!("commit held on {on} (changes of {peer})")
                }
                (RefusalKind::CommitHeld, None) => format!("commit held on {on}"),
            };
            Some(Notice {
                kind: Kind::Refusal,
                body: body(format!("{}: {who} {what}", workspace(state, workspace_id))),
            })
        }
        Event::CheckReported {
            session_ids,
            notice,
        } => {
            let CheckResult::Failed { exit_code } = notice.result else {
                return None;
            };
            let workspace_id = state
                .checks
                .iter()
                .find(|(_, records)| {
                    records
                        .iter()
                        .any(|record| record.check_id == notice.check_id)
                })
                .map(|(id, _)| id.clone())
                .or_else(|| {
                    session_ids
                        .first()
                        .and_then(|id| state.sessions.get(id))
                        .map(|session| session.workspace_id.clone())
                })?;
            let after = session_ids
                .first()
                .map(|id| format!(" after {}", session(id)))
                .unwrap_or_default();
            Some(Notice {
                kind: Kind::CheckFailed,
                body: body(format!(
                    "{}: check {} failed (exit {exit_code}){after}",
                    workspace(state, &workspace_id),
                    clean(&notice.name, 40, false),
                )),
            })
        }
        _ => None,
    }
}

/// The handoffs no session has taken up, by message ID.
pub fn waiting_handoffs(state: &State) -> impl Iterator<Item = &Message> {
    state.messages.values().flatten().filter(|message| {
        matches!(message.to, Recipient::NextSession {}) && message.acknowledged.is_empty()
    })
}

/// The notice for a handoff waiting `waited`.
pub fn handoff_notice(state: &State, message: &Message, waited: Duration) -> Notice {
    let from = message
        .from
        .as_deref()
        .map_or_else(|| "the owner".to_owned(), session);
    let about = if message.paths.is_empty() {
        String::new()
    } else {
        format!(" (about {})", paths(&message.paths))
    };
    Notice {
        kind: Kind::HandoffWaiting,
        body: body(format!(
            "{}: handoff from {from} not taken up for {}{about}",
            workspace(state, &message.workspace_id),
            if waited.as_secs() >= 60 {
                format!("{} min", waited.as_secs() / 60)
            } else {
                format!("{} s", waited.as_secs())
            },
        )),
    }
}

/// When each waiting handoff was first seen by this daemon run, and which
/// were told. A handoff waiting when the daemon starts is timed from then.
#[derive(Debug, Default)]
pub struct Handoffs {
    seen: BTreeMap<String, (Instant, bool)>,
}

impl Handoffs {
    /// The notices due at `now` for handoffs waiting at least `after`.
    pub fn due(&mut self, state: &State, now: Instant, after: Duration) -> Vec<Notice> {
        let mut current = BTreeSet::new();
        let mut due = Vec::new();
        for message in waiting_handoffs(state) {
            current.insert(message.message_id.clone());
            let (since, told) = self
                .seen
                .entry(message.message_id.clone())
                .or_insert((now, false));
            if !*told && now.saturating_duration_since(*since) >= after {
                *told = true;
                due.push(handoff_notice(state, message, after));
            }
        }
        self.seen.retain(|id, _| current.contains(id));
        due
    }
}

/// The largest `config.json` the daemon reads.
const CONFIG_LIMIT: u64 = 8192;

/// `config.json` in `state_dir` as a JSON object; empty when there is none.
fn read_config(
    state_dir: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>, crate::Error> {
    use std::io::Read;
    let path = state_dir.join("config.json");
    let file = match crate::local::private_file(&path, false) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(serde_json::Map::new());
        }
        Err(error) => return Err(format!("{}: {error}", path.display()).into()),
    };
    let mut bytes = Vec::new();
    file.take(CONFIG_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err("config.json exceeds 8 KiB".into());
    }
    match serde_json::from_slice(&bytes)? {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err("config.json must hold a JSON object".into()),
    }
}

/// Whether notices, and their sound, are on in `state_dir`'s configuration.
pub fn configured(state_dir: &Path) -> Result<(bool, bool), crate::Error> {
    let config = read_config(state_dir)?;
    let flag = |name: &str| match config.get(name) {
        None => Ok(false),
        Some(serde_json::Value::Bool(on)) => Ok(*on),
        Some(_) => Err(format!("config.json: {name} must be true or false")),
    };
    Ok((flag("notify")?, flag("notify_sound")?))
}

/// Set `notify` and, if given, `notify_sound` in `config.json`, keeping
/// every other setting. The file is replaced whole, owner-only, so the
/// daemon never reads half of it.
pub fn configure(
    state_dir: &Path,
    on: bool,
    sound: Option<bool>,
) -> Result<(bool, bool), crate::Error> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    crate::local::private_directory(state_dir)?;
    let mut config = read_config(state_dir)?;
    config.insert("notify".into(), on.into());
    if let Some(sound) = sound {
        config.insert("notify_sound".into(), sound.into());
    }
    let mut bytes = serde_json::to_vec_pretty(&config)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err("config.json would exceed 8 KiB".into());
    }
    let temporary = state_dir.join(format!(".config.json.{}.tmp", std::process::id()));
    let written = (|| -> Result<(), crate::Error> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, state_dir.join("config.json"))?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    configured(state_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            on: true,
            ..Settings::default()
        }
    }

    #[test]
    fn a_record_reported_to_two_groups_of_sessions_raises_one_notice() {
        let mut state = State::default();
        state.checks.insert(
            "work".into(),
            vec![langspan_core::CheckRecord {
                check_id: "c1".into(),
                workspace_id: "work".into(),
                name: "tests".into(),
                command: "cargo test".into(),
                tree_oid: "a".repeat(40),
                head_oid: None,
                inputs: Vec::new(),
                result: CheckResult::Failed { exit_code: 101 },
                duration_ms: 1,
                recorded_ms: 1,
                generation: 1,
                changed_since: false,
            }],
        );
        let report = |session: &str, check_id: &str| Event::CheckReported {
            session_ids: vec![session.into()],
            notice: langspan_core::CheckNotice {
                check_id: check_id.into(),
                name: "tests".into(),
                tree_oid: "a".repeat(40),
                result: CheckResult::Failed { exit_code: 101 },
                duration_ms: 1,
                timed_out_s: None,
                tail: String::new(),
                failures: Vec::new(),
                more_failures: false,
                baseline_check_id: None,
                fixed: Vec::new(),
                writers: Vec::new(),
            },
        };
        let notices = notices_for(
            &state,
            &[report("claude-code.a", "c1"), report("codex.b", "c1")],
        );
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].body.contains("claude-code"),
            "{}",
            notices[0].body
        );
    }

    #[test]
    fn one_notice_of_a_kind_per_interval_and_ten_an_hour() {
        let mut limiter = Limiter::default();
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let settings = settings();
        assert_eq!(limiter.admit(Kind::Refusal, at(0), &settings), Ok(()));
        assert_eq!(
            limiter.admit(Kind::Refusal, at(299), &settings),
            Err("kind_limit")
        );
        // Another kind is not held by the first.
        assert_eq!(limiter.admit(Kind::CheckFailed, at(1), &settings), Ok(()));
        assert_eq!(limiter.admit(Kind::Refusal, at(300), &settings), Ok(()));
        // A dropped notice does not restart the interval.
        assert_eq!(
            limiter.admit(Kind::Refusal, at(500), &settings),
            Err("kind_limit")
        );
        assert_eq!(limiter.admit(Kind::Refusal, at(600), &settings), Ok(()));

        let mut limiter = Limiter::default();
        let quick = Settings {
            kind_interval: Duration::ZERO,
            ..settings
        };
        for second in 0..10 {
            assert_eq!(limiter.admit(Kind::Refusal, at(second), &quick), Ok(()));
        }
        assert_eq!(
            limiter.admit(Kind::HandoffWaiting, at(100), &quick),
            Err("hourly_limit")
        );
        // The first falls out of the hour.
        assert_eq!(limiter.admit(Kind::Refusal, at(3600), &quick), Ok(()));
        assert_eq!(
            limiter.admit(Kind::Refusal, at(3600), &quick),
            Err("hourly_limit")
        );
    }

    #[test]
    fn a_body_is_cut_to_200_bytes_on_a_character_boundary() {
        let long = "é".repeat(300);
        let cut = body(long);
        assert!(cut.len() <= MAX_BODY, "{}", cut.len());
        assert!(cut.ends_with('…'));
        assert_eq!(body("short".into()), "short");
        assert_eq!(body("x".repeat(200)).len(), 200);
    }

    #[test]
    fn paths_are_at_most_three_with_the_rest_counted() {
        let five: Vec<String> = (0..5).map(|n| format!("src/{n}.rs")).collect();
        assert_eq!(paths(&five), "src/0.rs, src/1.rs, src/2.rs, 2 more");
        let deep = format!("{}/name.rs", "d".repeat(100));
        let shown = paths(&[deep]);
        assert!(
            shown.starts_with('…') && shown.ends_with("/name.rs"),
            "{shown}"
        );
        assert!(shown.len() <= 48);
        assert_eq!(clean("a\nb\u{1b}c", 10, false), "a?b?c");
    }

    #[test]
    fn sessions_are_named_by_host_and_the_start_of_their_id() {
        assert_eq!(
            session("codex.7c2e9a1b-0000-4000-8000-000000000000"),
            "codex.7c2e9a1b…"
        );
        assert_eq!(session("claude-code.me"), "claude-code.me");
    }

    #[test]
    fn the_title_comes_before_the_body_and_sound_is_only_an_argument() {
        let suppress = "--hint=boolean:suppress-sound:true";
        let quiet = Program::Osascript.args("-body", false);
        assert!(!quiet.iter().any(|arg| arg.contains("sound")), "{quiet:?}");
        let quiet = Program::NotifySend.args("-body", false);
        assert!(quiet.iter().any(|arg| arg == suppress), "{quiet:?}");
        assert!(
            !quiet.iter().any(|arg| arg.contains("sound-name")),
            "{quiet:?}"
        );
        let loud = Program::NotifySend.args("-body", true);
        assert!(!loud.iter().any(|arg| arg == suppress), "{loud:?}");
        for program in [Program::Osascript, Program::NotifySend] {
            let quiet = program.args("-body", false);
            let title = quiet.iter().position(|arg| arg == TITLE).unwrap();
            assert_eq!(quiet[title + 1], "-body");
            assert_eq!(quiet.len(), title + 2);
            let loud = program.args("-body", true);
            assert!(loud.iter().any(|arg| arg.contains("sound")), "{loud:?}");
        }
    }
}
