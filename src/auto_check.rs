//! The automatic check after a session's writes
//! ([ADR 0030](../docs/adr/0030-automatic-checks-after-write-batches.md)).
//!
//! The daemon's state owner sends a [`Message::Wrote`] when a session that
//! wrote files reaches a tool batch or a stop in a workspace whose owner
//! turned the check on. This worker waits until that workspace has been
//! quiet for the debounce interval, takes the exact working tree as
//! `check run` does, and runs the command detached from every hook, in its
//! own process group and within its time limit. If a session writes again
//! while it runs and the tree is then different, the run is stopped and the
//! new tree is checked instead. A finished run is recorded as an ordinary
//! check record bound to its tree, with a notice for the sessions it ran
//! after, delivered at their next briefing.
//!
//! A failing run that named its failing tests, and took at most the rerun
//! bound, is run once more on the same tree before it is recorded: the tests
//! both runs named go with the record, so the state owner learns which tests
//! flip on unchanged code (issue #70). The rerun is no record of its own:
//! the first run's failure is the result, and a pass of the rerun never
//! replaces it.
//!
//! Nothing here touches coordination state directly: the record goes to the
//! state owner as a request, like any client's.

use crate::{integrate, test_output};
use langspan_core::{AutoCheck, CheckReport, CheckResult, Command, Outcome, TestResults};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// A burst of batches is waited out for at most this long: a session that
/// never pauses still gets a check.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// How long a snapshot of the working tree may take.
const SNAPSHOT: Duration = Duration::from_secs(120);
/// How much of the command's combined output is kept for its tail once the
/// whole of it no longer fits `test_output::MAX_OUTPUT_BYTES`.
const KEPT_BYTES: usize = 8 * 1024;
/// What of it is reported: the last lines, at most this many bytes.
const TAIL_BYTES: usize = 1500;
const TAIL_LINES: usize = 20;

/// The daemon's timing for automatic checks (`config.json`).
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Quiet time after a session's last batch before a check starts.
    pub debounce: Duration,
    /// A failing run that took at most this long is run once more on the
    /// same tree; zero runs nothing again.
    pub rerun_max: Duration,
}

pub enum Message {
    /// `session_id` wrote files and reached a batch or stop.
    Wrote {
        check: AutoCheck,
        root: PathBuf,
        session_id: String,
    },
    /// The owner turned the check off or changed it: stop what runs, forget
    /// what waits.
    Off { workspace_id: String },
    Finished {
        workspace_id: String,
        run: u64,
        ended: Ended,
    },
}

/// A check's command may not carry what looks like a secret: it is stored
/// in the event log as it is, to be run again.
pub fn refuse_secrets(argv: &[String]) -> Result<(), String> {
    if crate::hook::contains_secret(&argv.join(" ")) {
        return Err(
            "this command looks like it holds a secret (a token, password or key); LangSpan stores the command to run it again, so pass the secret through your environment or a script instead. Nothing was changed".into(),
        );
    }
    Ok(())
}

/// How a run ended.
pub enum Ended {
    /// The command finished or was stopped at its limit on a tree that held
    /// still: a result about that tree.
    Done(Box<Finished>),
    /// The tree changed while it ran: stopped for it (`None`), or found
    /// changed when the command finished, whose result is then inconclusive.
    Changed(Option<Box<Finished>>),
    /// Stopped for a newer tree or because the owner turned it off.
    Cancelled,
    /// The tree could not be taken or the command could not start.
    Failed(String),
}

pub struct Finished {
    tree: String,
    head: Option<String>,
    result: CheckResult,
    timed_out_s: Option<u32>,
    duration_ms: u64,
    tail: String,
    /// The tests its output named, and those of its rerun.
    tests: Option<TestResults>,
    rerun: Option<TestResults>,
}

struct Running {
    run: u64,
    cancel: Arc<AtomicBool>,
    recheck: Arc<Recheck>,
    /// Each session with the recheck request its writes came with: 0 for
    /// the sessions the run started for.
    sessions: BTreeMap<String, u64>,
    check: AutoCheck,
}

/// Requests to compare the tree again, one per burst of writes that
/// arrived while a run was going, and the newest one the run answered by
/// finding the tree unchanged.
#[derive(Default)]
struct Recheck {
    requested: AtomicU64,
    confirmed: AtomicU64,
}

impl Running {
    /// The sessions this run's result is about, and the ones whose writes
    /// it never compared.
    fn split(self) -> (Self, BTreeSet<String>) {
        let confirmed = self.recheck.confirmed.load(Ordering::SeqCst);
        let (covered, late): (BTreeMap<_, _>, BTreeMap<_, _>) = self
            .sessions
            .clone()
            .into_iter()
            .partition(|(_, request)| *request <= confirmed);
        (
            Self {
                sessions: covered,
                ..self
            },
            late.into_keys().collect(),
        )
    }
    /// A session wrote while the run was going: its result covers this
    /// session only if the run compares the tree after `request`, even when
    /// the session was already one the run was for.
    fn wrote_again(&mut self, session: String, request: u64) {
        self.sessions.insert(session, request);
    }
    /// A session the run is for wrote again: until the run compares the
    /// tree after that write, its result does not cover the session. No
    /// comparison confirms `u64::MAX`; the write's debounce replaces it with
    /// the request the run then answers.
    fn not_covered_yet(&mut self, session: &str) {
        if let Some(request) = self.sessions.get_mut(session) {
            *request = u64::MAX;
        }
    }
    fn wrote_meanwhile(&self) -> bool {
        self.recheck.requested.load(Ordering::SeqCst) > 0
    }
}

#[derive(Default)]
struct Workspace {
    check: Option<(AutoCheck, PathBuf)>,
    /// Sessions whose writes are not yet covered by a run.
    waiting: BTreeSet<String>,
    first: Option<Instant>,
    due: Option<Instant>,
    running: Option<Running>,
}

/// The worker loop. `record` sends a command to the state owner and returns
/// its outcome, or `None` when the owner could not be reached.
#[allow(clippy::too_many_lines)]
pub fn worker(
    sender: mpsc::SyncSender<Message>,
    messages: mpsc::Receiver<Message>,
    timing: Timing,
    record: impl Fn(String, Command) -> Option<Outcome>,
) {
    let debounce = timing.debounce;
    let mut workspaces = BTreeMap::<String, Workspace>::new();
    let mut next_run = 0_u64;
    loop {
        let wait = workspaces
            .values()
            .filter_map(|workspace| workspace.due)
            .min()
            .map_or(Duration::from_secs(1), |due| {
                due.saturating_duration_since(Instant::now())
            });
        match messages.recv_timeout(wait) {
            Ok(Message::Wrote {
                check,
                root,
                session_id,
            }) => {
                let workspace = workspaces.entry(check.workspace_id.clone()).or_default();
                if workspace
                    .check
                    .as_ref()
                    .is_some_and(|(current, _)| *current != check)
                {
                    workspace.cancel();
                }
                workspace.check = Some((check, root));
                if let Some(running) = &mut workspace.running {
                    running.not_covered_yet(&session_id);
                }
                workspace.waiting.insert(session_id);
                let now = Instant::now();
                let first = *workspace.first.get_or_insert(now);
                workspace.due = Some((now + debounce).min(first + MAX_WAIT));
            }
            Ok(Message::Off { workspace_id }) => {
                if let Some(workspace) = workspaces.get_mut(&workspace_id) {
                    workspace.cancel();
                    workspace.check = None;
                }
            }
            Ok(Message::Finished {
                workspace_id,
                run,
                ended,
            }) => {
                let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                    continue;
                };
                let Some(running) = workspace.running.take_if(|running| running.run == run) else {
                    continue;
                };
                // Turned off or changed after the command ended: whatever
                // it found is not reported.
                let ended = if running.cancel.load(Ordering::SeqCst) {
                    Ended::Cancelled
                } else {
                    ended
                };
                match ended {
                    Ended::Done(finished) => {
                        // Writes that came after the run last compared the
                        // tree are not in its result: they get a run of
                        // their own.
                        let (running, late) = running.split();
                        if !late.is_empty() {
                            workspace.waiting.extend(late);
                            workspace.due.get_or_insert_with(Instant::now);
                        }
                        if !running.sessions.is_empty() {
                            report(&record, &workspace_id, running, *finished);
                        }
                    }
                    // A session's new writes are checked on the new tree;
                    // the run's sessions are told that result instead.
                    Ended::Changed(_)
                        if running.wrote_meanwhile() || !workspace.waiting.is_empty() =>
                    {
                        workspace.waiting.extend(running.sessions.into_keys());
                        workspace.due.get_or_insert_with(Instant::now);
                    }
                    // No session wrote: the command itself, or something
                    // outside any session, changed the tree. Its result is
                    // about no tree, and the sessions are told so.
                    Ended::Changed(Some(finished)) => {
                        report(&record, &workspace_id, running, *finished)
                    }
                    Ended::Changed(None) | Ended::Cancelled => {}
                    Ended::Failed(detail) => log("auto_check_failed", &workspace_id, &detail),
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        for (workspace_id, workspace) in &mut workspaces {
            if workspace.due.is_none_or(|due| due > now) {
                continue;
            }
            let Some((check, root)) = workspace.check.clone() else {
                workspace.waiting.clear();
                workspace.due = None;
                workspace.first = None;
                continue;
            };
            if workspace
                .running
                .as_ref()
                .is_some_and(|running| running.cancel.load(Ordering::SeqCst))
            {
                // A stopped run has yet to report; start after it has.
                workspace.due = Some(now + Duration::from_millis(100));
                continue;
            }
            if let Some(running) = &mut workspace.running {
                // The run decides on its own thread whether the tree moved.
                // These sessions share its result only if it compares the
                // tree after this request and finds it unchanged.
                let request = running.recheck.requested.fetch_add(1, Ordering::SeqCst) + 1;
                for session in std::mem::take(&mut workspace.waiting) {
                    running.wrote_again(session, request);
                }
            } else if !still_enabled(&record, &check) {
                // A turn-off that did not reach this worker: nothing runs.
                workspace.check = None;
                workspace.waiting.clear();
            } else {
                next_run += 1;
                let running = Running {
                    run: next_run,
                    cancel: Arc::default(),
                    recheck: Arc::default(),
                    sessions: std::mem::take(&mut workspace.waiting)
                        .into_iter()
                        .map(|session| (session, 0))
                        .collect(),
                    check: check.clone(),
                };
                let (cancel, recheck, sender) = (
                    running.cancel.clone(),
                    running.recheck.clone(),
                    sender.clone(),
                );
                let (workspace_id, run) = (workspace_id.clone(), next_run);
                let started = std::thread::Builder::new()
                    .name("langspan-auto-check".into())
                    .spawn(move || {
                        let ended = run_once(&root, &check, &cancel, &recheck, timing.rerun_max);
                        let _ = sender.send(Message::Finished {
                            workspace_id,
                            run,
                            ended,
                        });
                    });
                match started {
                    Ok(_) => workspace.running = Some(running),
                    Err(error) => log(
                        "auto_check_failed",
                        &running.check.workspace_id,
                        &error.to_string(),
                    ),
                }
            }
            workspace.due = None;
            workspace.first = None;
        }
    }
}

/// Whether the owner's current setting is still `check`, asked of the state
/// owner just before a run starts. Unreachable counts as not enabled. A
/// check at its size limits can be left out of the bounded view, so it is
/// read complete then.
fn still_enabled(record: &impl Fn(String, Command) -> Option<Outcome>, check: &AutoCheck) -> bool {
    let ask = |command| {
        record("read".into(), command).ok_or_else(|| crate::Error::from("the state owner is gone"))
    };
    crate::client::status_with(ask, &["auto_checks"])
        .is_ok_and(|(state, _)| state.auto_checks.get(&check.workspace_id) == Some(check))
}

impl Workspace {
    fn cancel(&mut self) {
        if let Some(running) = &self.running {
            running.cancel.store(true, Ordering::SeqCst);
        }
        self.waiting.clear();
        self.due = None;
        self.first = None;
    }
}

fn log(event: &str, workspace_id: &str, detail: &str) {
    eprintln!(
        "{}",
        serde_json::json!({"level":"warn","event":event,"workspace_id":workspace_id,"detail":detail})
    );
}

/// Record the result as a check of its tree, for the sessions it ran after.
fn report(
    record: &impl Fn(String, Command) -> Option<Outcome>,
    workspace_id: &str,
    running: Running,
    finished: Finished,
) {
    let check = running.check;
    let request_id = crate::hook::wire_id(
        "auto-check",
        &[
            workspace_id,
            &check.name,
            &finished.tree,
            &running.run.to_string(),
            &crate::clock::now_ms().to_string(),
        ],
    );
    let mut sessions: Vec<String> = running.sessions.into_keys().collect();
    sessions.truncate(langspan_core::MAX_CHECK_REPORT_SESSIONS);
    let command = Command::RecordCheck {
        workspace_id: workspace_id.into(),
        name: check.name.clone(),
        command: crate::landing::command_line(&check.argv),
        tree_oid: finished.tree,
        head_oid: finished.head,
        inputs: Vec::new(),
        result: finished.result,
        duration_ms: finished.duration_ms,
        report: Some(CheckReport {
            session_ids: sessions,
            tail: finished.tail,
            timed_out_s: finished.timed_out_s,
        }),
        tests: finished.tests,
        rerun: finished.rerun,
    };
    // The state owner's queue can be briefly full; the same request is
    // retried for up to five seconds.
    for _ in 0..50 {
        match record(request_id.clone(), command.clone()) {
            Some(Outcome::CheckRecorded { .. }) => return,
            Some(Outcome::Error { code, message }) => {
                return log(
                    "auto_check_not_recorded",
                    workspace_id,
                    &format!("{code}: {message}"),
                );
            }
            Some(_) => {
                return log(
                    "auto_check_not_recorded",
                    workspace_id,
                    "unexpected outcome",
                );
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    log(
        "auto_check_not_recorded",
        workspace_id,
        "the coordinator could not be reached",
    );
}

/// One run: the tree before, the command, the tree after; then, for a quick
/// failure that named its failing tests, the command again on that tree.
fn run_once(
    root: &Path,
    check: &AutoCheck,
    cancel: &AtomicBool,
    recheck: &Recheck,
    rerun_max: Duration,
) -> Ended {
    let snapshot = || integrate::working_tree(root, Instant::now() + SNAPSHOT);
    let (head, before) = match snapshot() {
        Ok(tree) => tree,
        Err(error) => return Ended::Failed(format!("could not take the working tree: {error}")),
    };
    if cancel.load(Ordering::SeqCst) {
        return Ended::Cancelled;
    }
    let limit = Duration::from_secs(u64::from(check.timeout_s));
    let tree_changed = |deadline: Instant| {
        // Asked only when a session wrote again, and never past the
        // command's own time limit. A tree that cannot be taken now is
        // treated as changed: the next run says why.
        let request = recheck.requested.load(Ordering::SeqCst);
        if request <= recheck.confirmed.load(Ordering::SeqCst) {
            return false;
        }
        let changed = integrate::working_tree(root, deadline.min(Instant::now() + SNAPSHOT))
            .map_or(true, |(_, tree)| tree != before);
        if !changed {
            recheck.confirmed.fetch_max(request, Ordering::SeqCst);
        }
        changed
    };
    let stop = |deadline| cancel.load(Ordering::SeqCst) || tree_changed(deadline);
    let (ran, duration_ms, output) = match run_command(root, &check.argv, limit, stop) {
        Ok(ran) => ran,
        Err(error) => return Ended::Failed(error),
    };
    let ran = match ran {
        Some(ran) => ran,
        None if cancel.load(Ordering::SeqCst) => return Ended::Cancelled,
        // Stopped because a session's writes changed the tree.
        None => return Ended::Changed(None),
    };
    // Every request made before this snapshot is answered by it, and the
    // answer is whether the tree still is the one the command ran on.
    let held_still = || {
        let request = recheck.requested.load(Ordering::SeqCst);
        let held = snapshot().map(|(_, after)| after == before)?;
        if held {
            recheck.confirmed.fetch_max(request, Ordering::SeqCst);
        }
        Ok::<_, crate::Error>(held)
    };
    let held = match held_still() {
        Ok(held) => held,
        Err(error) => return Ended::Failed(format!("could not take the working tree: {error}")),
    };
    if cancel.load(Ordering::SeqCst) {
        return Ended::Cancelled;
    }
    // As `check run` decides: a result is about a tree only if the tree held
    // still while it ran.
    let (result, timed_out_s) = match ran {
        _ if !held => (CheckResult::Inconclusive {}, None),
        Exit::Code(0) => (CheckResult::Passed {}, None),
        Exit::Code(exit_code) => (CheckResult::Failed { exit_code }, None),
        Exit::Signal => (CheckResult::Inconclusive {}, None),
        Exit::TimedOut => (CheckResult::Inconclusive {}, Some(check.timeout_s)),
    };
    let tests = match result {
        CheckResult::Passed {} => output.tests(true, &[]),
        CheckResult::Failed { .. } => output.tests(false, &[]),
        CheckResult::Inconclusive {} => None,
    };
    // A quick failure that named a failing test runs once more on the same
    // tree, within the same limit and stopped by a tree change like any
    // run. Only the tests it names are kept: its result is no record.
    let mut rerun = None;
    if let Some(first) = &tests
        && !first.failed.is_empty()
        && !rerun_max.is_zero()
        && u128::from(duration_ms) <= rerun_max.as_millis()
    {
        match run_command(root, &check.argv, limit, stop) {
            Ok((Some(Exit::Code(code)), _, again)) => match held_still() {
                Ok(true) => rerun = again.tests(code == 0, &first.failed),
                // Changed while it ran again: the first result stands, and
                // the writes get a run of their own.
                Ok(false) => {}
                Err(error) => log(
                    "auto_check_rerun_failed",
                    &check.workspace_id,
                    &error.to_string(),
                ),
            },
            Ok((Some(_), _, _)) => {}
            Ok((None, _, _)) if cancel.load(Ordering::SeqCst) => return Ended::Cancelled,
            Ok((None, _, _)) => {}
            Err(error) => log("auto_check_rerun_failed", &check.workspace_id, &error),
        }
        if cancel.load(Ordering::SeqCst) {
            return Ended::Cancelled;
        }
    }
    let finished = Box::new(Finished {
        tree: before,
        head,
        result,
        timed_out_s,
        duration_ms,
        tail: output.tail(),
        tests,
        rerun,
    });
    if held {
        Ended::Done(finished)
    } else {
        Ended::Changed(Some(finished))
    }
}

enum Exit {
    Code(i32),
    Signal,
    TimedOut,
}

/// What was kept of a run's combined output: all of it while it fits
/// `test_output::MAX_OUTPUT_BYTES`, then only its end, with `cut` set.
#[derive(Default, Clone)]
struct Output {
    bytes: Vec<u8>,
    cut: bool,
}

impl Output {
    fn push(&mut self, read: &[u8]) {
        self.bytes.extend_from_slice(read);
        let bound = if self.cut {
            KEPT_BYTES
        } else {
            test_output::MAX_OUTPUT_BYTES
        };
        if self.bytes.len() > bound {
            // Past the whole-output bound only the end is kept, so a long
            // output costs one move of the bound and then small ones.
            let excess = self.bytes.len() - KEPT_BYTES;
            self.bytes.drain(..excess);
            self.cut = true;
        }
    }
    fn tail(&self) -> String {
        let start = self.bytes.len().saturating_sub(KEPT_BYTES);
        tail(&self.bytes[start..], self.cut || start > 0)
    }
    fn tests(&self, all_passed: bool, prefer: &[String]) -> Option<TestResults> {
        test_output::read(&self.bytes, self.cut, all_passed, prefer)
    }
}

/// Run `argv` in `dir` without a shell, in its own process group, with its
/// standard output and error joined and kept as [`Output`]. `None` when
/// `stop` asked for it to be stopped. Its whole process group is killed
/// when it ends, whatever the reason, as `check run` does.
#[allow(clippy::type_complexity)]
fn run_command(
    dir: &Path,
    argv: &[String],
    limit: Duration,
    mut stop: impl FnMut(Instant) -> bool,
) -> Result<(Option<Exit>, u64, Output), String> {
    use std::os::unix::process::CommandExt;
    let (program, args) = argv.split_first().ok_or("the check has no command")?;
    let (mut reader, writer) =
        std::io::pipe().map_err(|e| format!("cannot create a pipe for the check's output: {e}"))?;
    let error_writer = writer
        .try_clone()
        .map_err(|e| format!("cannot create a pipe for the check's output: {e}"))?;
    let started = Instant::now();
    let deadline = started + limit;
    let mut child = Process::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(error_writer)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot start {program:?}: {e}"))?;
    let group = rustix::process::Pid::from_child(&child);
    // The pipe's writing ends now belong to the command alone, so the
    // reader sees its end when the command's process group is gone.
    let stop_all = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    };
    let kept = Arc::new(Mutex::new(Output::default()));
    let (finished_reading, read_done) = mpsc::channel::<()>();
    let reading = {
        let kept = kept.clone();
        std::thread::Builder::new()
            .name("langspan-auto-check-output".into())
            .spawn(move || {
                let mut buffer = [0_u8; 4096];
                while let Ok(read) = reader.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    kept.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(&buffer[..read]);
                }
                let _ = finished_reading.send(());
            })
    };
    if let Err(error) = reading {
        stop_all(&mut child);
        return Err(format!("cannot read the check's output: {error}"));
    }
    let mut last_poll = Instant::now();
    let exit = loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                stop_all(&mut child);
                return Err(format!("cannot wait for the check: {error}"));
            }
        };
        match status {
            Some(status) => break Some(status.code().map_or(Exit::Signal, Exit::Code)),
            None if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break Some(Exit::TimedOut);
            }
            None => {
                // `stop` may take a snapshot: asked at most ten times a
                // second, and never past the time limit.
                if last_poll.elapsed() >= Duration::from_millis(100) {
                    last_poll = Instant::now();
                    if stop(deadline) {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    // A process that left the group can hold the pipe open: its output is
    // not waited for beyond a second.
    let _ = read_done.recv_timeout(Duration::from_secs(1));
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let output = std::mem::take(&mut *kept.lock().unwrap_or_else(|e| e.into_inner()));
    Ok((exit, duration_ms, output))
}

/// The last lines of the output, redacted, without control characters other
/// than line breaks and tabs, and within the reported bound. When the start
/// of the kept bytes was cut, the partial first line is dropped before
/// redaction: it could hold the end of a secret whose recognisable start
/// was cut off.
fn tail(bytes: &[u8], cut: bool) -> String {
    let bytes = match bytes.iter().position(|byte| *byte == b'\n') {
        Some(newline) if cut => &bytes[newline + 1..],
        None if cut => &[],
        _ => bytes,
    };
    let text = crate::hook::redact(&String::from_utf8_lossy(bytes));
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            line.chars()
                .map(|c| if c.is_control() && c != '\t' { ' ' } else { c })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .filter(|line| !line.is_empty())
        .collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in lines.iter().rev().take(TAIL_LINES) {
        if size + line.len() + 1 > TAIL_BYTES {
            if kept.is_empty() {
                let mut start = line.len() - (TAIL_BYTES - 1);
                while !line.is_char_boundary(start) {
                    start += 1;
                }
                kept.push(&line[start..]);
            }
            break;
        }
        size += line.len() + 1;
        kept.push(line);
    }
    kept.reverse();
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_keeps_the_last_lines_within_its_bound_and_redacts() {
        let mut text = String::new();
        for n in 0..100 {
            text.push_str(&format!("line {n}\n"));
        }
        text.push_str("\u{1b}[31mFAILED\u{1b}[0m password=hunter2hunter2\n");
        let tail = tail(text.as_bytes(), false);
        assert!(tail.len() <= TAIL_BYTES);
        assert_eq!(tail.lines().count(), TAIL_LINES);
        assert!(tail.starts_with("line 81"), "{tail}");
        assert!(
            !tail.contains('\u{1b}') && !tail.contains("hunter2"),
            "{tail}"
        );
        let long = "é".repeat(2000);
        let tail_of_long = super::tail(long.as_bytes(), false);
        assert!(tail_of_long.len() < TAIL_BYTES, "{}", tail_of_long.len());
        // Cut in the middle of a secret: its end is not kept.
        let cut = b"9WgXq2LmZ8vR4tYp\nFAILED: 1 test\n";
        assert_eq!(super::tail(cut, true), "FAILED: 1 test");
        assert_eq!(super::tail(b"no newline at all", true), "");
    }

    #[test]
    fn writes_the_run_never_compared_are_not_given_its_result() {
        let running = Running {
            run: 1,
            cancel: Arc::default(),
            recheck: Arc::default(),
            sessions: [
                ("first".into(), 0),
                ("second".into(), 1),
                ("late".into(), 2),
            ]
            .into_iter()
            .collect(),
            check: AutoCheck {
                workspace_id: "work".into(),
                name: "tests".into(),
                argv: vec!["true".into()],
                timeout_s: 1,
            },
        };
        running.recheck.requested.store(2, Ordering::SeqCst);
        // The run found the tree unchanged after the first request only.
        running.recheck.confirmed.store(1, Ordering::SeqCst);
        let (covered, late) = running.split();
        assert_eq!(
            covered.sessions.into_keys().collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(late.into_iter().collect::<Vec<_>>(), ["late"]);
    }

    #[test]
    fn a_session_that_writes_again_waits_for_its_newest_request() {
        let mut running = Running {
            run: 1,
            cancel: Arc::default(),
            recheck: Arc::default(),
            sessions: [("first".into(), 0)].into_iter().collect(),
            check: AutoCheck {
                workspace_id: "work".into(),
                name: "tests".into(),
                argv: vec!["true".into()],
                timeout_s: 1,
            },
        };
        running.wrote_again("first".into(), 1);
        running.wrote_again("first".into(), 2);
        running.recheck.requested.store(2, Ordering::SeqCst);
        // The run compared the tree after the first request, not the second.
        running.recheck.confirmed.store(1, Ordering::SeqCst);
        let (covered, late) = running.split();
        assert!(covered.sessions.is_empty());
        assert_eq!(late.into_iter().collect::<Vec<_>>(), ["first"]);
    }

    #[test]
    fn a_write_after_the_last_comparison_is_not_covered_before_its_debounce() {
        let mut running = Running {
            run: 1,
            cancel: Arc::default(),
            recheck: Arc::default(),
            sessions: [("first".into(), 0), ("second".into(), 0)]
                .into_iter()
                .collect(),
            check: AutoCheck {
                workspace_id: "work".into(),
                name: "tests".into(),
                argv: vec!["true".into()],
                timeout_s: 1,
            },
        };
        // The run's final snapshot found the tree unchanged, then "first"
        // wrote again and the run finished before that write's debounce.
        running.recheck.confirmed.store(1, Ordering::SeqCst);
        running.not_covered_yet("first");
        let (covered, late) = running.split();
        assert_eq!(covered.sessions.into_keys().collect::<Vec<_>>(), ["second"]);
        assert_eq!(late.into_iter().collect::<Vec<_>>(), ["first"]);
    }

    #[test]
    fn a_command_with_a_secret_shape_is_refused_and_a_plain_one_is_not() {
        let argv = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert!(refuse_secrets(&argv("cargo test --workspace")).is_ok());
        assert!(refuse_secrets(&argv("deploy --token abc123")).is_err());
        assert!(refuse_secrets(&argv("env API_TOKEN=abc123 make test")).is_err());
    }
}
