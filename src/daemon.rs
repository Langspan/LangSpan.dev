//! Bounded local transport; one dedicated owner applies and persists all state.

#[cfg(not(unix))]
use std::path::PathBuf;

pub use crate::registration::Registration;

#[cfg(unix)]
mod index_queries;

#[cfg(not(unix))]
pub async fn run(_state_dir: PathBuf) -> Result<(), crate::Error> {
    Err("this initial daemon requires Unix sockets; Windows transport is not implemented".into())
}

#[cfg(unix)]
mod unix {
    use super::Registration;
    use super::index_queries::{
        check_calls, check_write, graph_blind_spots, graph_context, graph_inflight, graph_map,
        graph_path, graph_query, observe_elsewhere, outline_read, prepare_anchors, prepare_brief,
    };
    use crate::{
        auto_check, local, notify,
        protocol::{self, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response},
        store::{Execution, Store},
    };
    use langspan_core::{Command, Outcome, State, validate_request_id};
    use serde::Deserialize;
    use std::{
        fs,
        io::{self, Read},
        os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::{UnixListener, UnixStream},
        sync::{Semaphore, oneshot},
        time::timeout,
    };

    #[derive(Debug, Clone, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Config {
        io_timeout_ms: u64,
        request_timeout_ms: u64,
        max_clients: usize,
        /// How often each publishing workspace is fetched and, if due,
        /// published.
        remote_pass_ms: u64,
        /// How long a workspace's sessions must stop writing before its
        /// automatic check starts, so one burst of batches runs one check.
        auto_check_debounce_ms: u64,
        /// When set, the daemon compacts the event log once a day, keeping
        /// this many days (ADR 0025, J7). Unset: nothing is ever deleted.
        retention_days: Option<u32>,
        /// A failing automatic check that took at most this long is run once
        /// more on the same tree, to learn which tests flip; 0 turns it off.
        flaky_rerun_max_s: u64,
        /// Desktop notices (roadmap H3): off unless switched on.
        notify: bool,
        /// A sound with each notice: off unless switched on.
        notify_sound: bool,
        /// At most one notice of a kind this often.
        notify_kind_interval_ms: u64,
        /// At most this many notices in any hour.
        notify_hourly_limit: u32,
        /// A handoff not taken up this long gets a notice.
        notify_handoff_after_ms: u64,
        /// How often the agent hosts on this machine are checked for a
        /// LangSpan install (roadmap A7). The first check is at start.
        host_check_ms: u64,
        /// How far back a starting session's "recently in this repository"
        /// section looks (roadmap C3), within the current daemon run.
        recent_window_ms: u64,
        /// A whole read is outlined first (roadmap A11) only of a file of at
        /// least this many lines, or of at least `outline_min_bytes`.
        outline_min_lines: u64,
        outline_min_bytes: u64,
    }
    impl Default for Config {
        fn default() -> Self {
            Self {
                io_timeout_ms: 2000,
                request_timeout_ms: 5000,
                max_clients: 32,
                remote_pass_ms: 5000,
                auto_check_debounce_ms: 2000,
                retention_days: None,
                flaky_rerun_max_s: 30,
                notify: false,
                notify_sound: false,
                notify_kind_interval_ms: 300_000,
                notify_hourly_limit: 10,
                notify_handoff_after_ms: 600_000,
                host_check_ms: 3_600_000,
                recent_window_ms: langspan_core::RECENT_WINDOW_MS,
                outline_min_lines: OUTLINE_MIN_LINES,
                outline_min_bytes: OUTLINE_MIN_BYTES,
            }
        }
    }
    impl Config {
        fn read(directory: &Path) -> Result<Self, crate::Error> {
            let path = directory.join("config.json");
            let result = match local::private_file(&path, false) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(8193).read_to_end(&mut bytes)?;
                    if bytes.len() > 8192 {
                        return Err("config.json exceeds 8 KiB".into());
                    }
                    serde_json::from_slice(&bytes)?
                }
                Err(error)
                    if error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
                {
                    Self::default()
                }
                Err(error) => return Err(error),
            };
            if !(100..=5000).contains(&result.io_timeout_ms)
                || !(100..=5000).contains(&result.request_timeout_ms)
                || !(1..=128).contains(&result.max_clients)
                || !(200..=600_000).contains(&result.remote_pass_ms)
                || !(50..=60_000).contains(&result.auto_check_debounce_ms)
                || result.flaky_rerun_max_s > 3600
                || !(1000..=86_400_000).contains(&result.notify_kind_interval_ms)
                || !(1..=60).contains(&result.notify_hourly_limit)
                || !(1000..=86_400_000).contains(&result.notify_handoff_after_ms)
                || !(100..=86_400_000).contains(&result.host_check_ms)
                || !(langspan_core::MIN_RECENT_WINDOW_MS..=langspan_core::MAX_RECENT_WINDOW_MS)
                    .contains(&result.recent_window_ms)
                || !(1..=1_000_000).contains(&result.outline_min_lines)
                || !(1..=langspan_graph::MAX_SOURCE_BYTES as u64)
                    .contains(&result.outline_min_bytes)
                || result.retention_days.is_some_and(|days| {
                    !(langspan_core::MIN_RETENTION_DAYS..=langspan_core::MAX_RETENTION_DAYS)
                        .contains(&days)
                })
            {
                return Err(
                    "config limits: io_timeout_ms/request_timeout_ms 100..5000, max_clients 1..128, remote_pass_ms 200..600000, auto_check_debounce_ms 50..60000, flaky_rerun_max_s 0..3600, notify_kind_interval_ms 1000..86400000, notify_hourly_limit 1..60, notify_handoff_after_ms 1000..86400000, host_check_ms 100..86400000, recent_window_ms 60000..86400000, outline_min_lines 1..1000000, outline_min_bytes 1..1048576, retention_days 7..3650"
                        .into(),
                );
            }
            Ok(result)
        }
        fn notice_settings(&self) -> notify::Settings {
            notify::Settings {
                on: self.notify,
                sound: self.notify_sound,
                kind_interval: Duration::from_millis(self.notify_kind_interval_ms),
                hourly_limit: self.notify_hourly_limit,
            }
        }
    }
    struct Job {
        request: Request,
        reply: oneshot::Sender<Response>,
        /// A worker's report about one registration of a workspace: refused,
        /// unhandled, once the workspace is no longer in it.
        of: Option<Registration>,
    }

    struct IndexJob {
        /// The registration the scan was of. A root or registration revision
        /// other than the one last indexed starts the index again.
        of: Registration,
        /// Its unobserved paths are kept in the index as they were, never
        /// treated as removed.
        scan: crate::graph::ScanView,
    }

    /// An export or import of the source index (roadmap D5), handed by
    /// the state owner to the indexer, which writes the index alone and
    /// replies itself.
    struct Transfer {
        of: Registration,
        request_id: String,
        generation: u64,
        command: Command,
        reply: oneshot::Sender<Response>,
    }

    /// How long a connection waits for an export or import, and how long
    /// the indexer may spend on one. An import of the largest file reads up
    /// to 256 MiB and every listed source file.
    const TRANSFER_WAIT: Duration = Duration::from_secs(120);
    const TRANSFER_BUDGET: Duration = Duration::from_secs(100);

    fn is_transfer(command: &Command) -> bool {
        matches!(
            command,
            Command::GraphExport { .. } | Command::GraphImport { .. }
        )
    }

    /// For tests only: `LANGSPAN_TEST_INDEX_FILES_PER_PASS` sets how many
    /// files the indexer parses per pass (default `FILES_PER_PASS`), so a
    /// test can show what an import loads without a parse. Not a setting:
    /// it is undocumented for users.
    fn files_per_pass() -> usize {
        std::env::var("LANGSPAN_TEST_INDEX_FILES_PER_PASS")
            .ok()
            .and_then(|text| text.parse::<usize>().ok())
            .map_or(crate::graph::FILES_PER_PASS, |files| {
                files.min(crate::graph::FILES_PER_PASS)
            })
    }

    /// Check a transfer request and hand it to the indexer; anything that
    /// fails is answered here.
    fn forward_transfer(state: &State, transfers: &mpsc::SyncSender<Transfer>, job: Job) {
        let version = job.request.version;
        let request_id = job.request.request_id;
        let command = job.request.command;
        let refuse = |code: &str, message: String| {
            error(request_id.clone(), state.generation, code, message)
        };
        let (Command::GraphExport { workspace_id, path }
        | Command::GraphImport {
            workspace_id, path, ..
        }) = &command
        else {
            return;
        };
        let refused = if !protocol::answers(version) {
            Some(refuse("unsupported_version", unsupported_version()))
        } else if let Err(e) = validate_request_id(&request_id) {
            Some(refuse(&e.code, e.message))
        } else if !Path::new(path).is_absolute() || path.len() > 4096 || path.contains('\0') {
            Some(refuse(
                "invalid_path",
                "the file must be named by an absolute path of at most 4096 bytes".into(),
            ))
        } else {
            None
        };
        if let Some(refused) = refused {
            let _ = job.reply.send(refused);
            return;
        }
        let Some(of) = Registration::of(state, workspace_id) else {
            let _ = job.reply.send(refuse(
                "unknown_workspace",
                format!(
                    "workspace {workspace_id} is not registered; run `langspan workspace list`"
                ),
            ));
            return;
        };
        let transfer = Transfer {
            of,
            request_id: request_id.clone(),
            generation: state.generation,
            command,
            reply: job.reply,
        };
        if let Err(error) = transfers.try_send(transfer) {
            let (message, transfer) = match error {
                mpsc::TrySendError::Full(transfer) => (
                    "the indexer is handling another export or import; try again when it finishes",
                    transfer,
                ),
                mpsc::TrySendError::Disconnected(transfer) => (
                    "the index is not available; see the daemon's index_unavailable log line",
                    transfer,
                ),
            };
            let _ = transfer
                .reply
                .send(refuse("index_unavailable", message.into()));
        }
    }

    /// Run one transfer on the indexer thread and build its reply.
    fn transfer(
        index: &mut crate::graph::Index,
        scans: &std::collections::HashMap<String, (Registration, crate::graph::ScanView)>,
        transfer: &Transfer,
    ) -> Result<Outcome, (&'static str, String)> {
        let deadline = Instant::now() + TRANSFER_BUDGET;
        let of = &transfer.of;
        match &transfer.command {
            Command::GraphExport { path, .. } => {
                let path = Path::new(path);
                let file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                    .open(path)
                    .map_err(|error| {
                        let hint = if error.kind() == io::ErrorKind::AlreadyExists {
                            "; an export never replaces a file: remove it or choose another path"
                        } else {
                            ""
                        };
                        (
                            "index_export_failed",
                            format!("cannot create {}: {error}{hint}", path.display()),
                        )
                    })?;
                let mut out = io::BufWriter::new(file);
                let written = index
                    .export(of, &mut out, now_ms(), deadline)
                    .and_then(|exported| {
                        use std::io::Write;
                        out.flush()?;
                        out.get_ref().sync_all()?;
                        Ok(exported)
                    });
                match written {
                    Ok(exported) => Ok(Outcome::GraphExported {
                        workspace_id: of.workspace_id.clone(),
                        files: exported.files,
                        partial_files: exported.partial_files,
                        bytes: exported.bytes,
                    }),
                    Err(error) => {
                        drop(out);
                        let _ = fs::remove_file(path);
                        Err(("index_export_failed", error.to_string()))
                    }
                }
            }
            Command::GraphImport { path, apply, .. } => {
                let refused = |message: String| ("index_import_refused", message);
                let Some((_, scan)) = scans
                    .get(&of.workspace_id)
                    .filter(|(scanned, _)| scanned == of)
                else {
                    return Err(refused(format!(
                        "workspace {} has no complete scan yet; try again in a few seconds",
                        of.workspace_id
                    )));
                };
                // Opened nonblocking, so a FIFO with no writer (or a symlink
                // to one) cannot hold the indexer before the type check;
                // a regular file ignores the flag.
                let file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
                    .open(path)
                    .map_err(|error| refused(format!("cannot open {path}: {error}")))?;
                let metadata = file
                    .metadata()
                    .map_err(|error| refused(format!("cannot read {path}: {error}")))?;
                if !metadata.is_file() {
                    return Err(refused(format!("{path} is not a regular file")));
                }
                let max = crate::index_file::MAX_FILE_BYTES;
                if metadata.len() > max {
                    return Err(refused(format!(
                        "{path} is {} bytes; an import reads at most {} MiB",
                        metadata.len(),
                        max / (1024 * 1024)
                    )));
                }
                let mut input = io::BufReader::new(file.take(max));
                let imported = index
                    .import(of, scan, &mut input, *apply, now_ms(), deadline)
                    .map_err(|error| refused(error.to_string()))?;
                Ok(Outcome::GraphImported {
                    workspace_id: of.workspace_id.clone(),
                    applied: *apply,
                    loaded: imported.loaded,
                    different: imported.different,
                    kept: imported.kept,
                    over_limits: imported.over_limits,
                    malformed: imported.malformed,
                })
            }
            _ => Err(("internal", "not an index transfer".into())),
        }
    }

    /// For tests only: `LANGSPAN_TEST_INDEX_REPORT_DELAY_MS`, at most 5 s,
    /// holds an index pass's notices that long before they are reported, so
    /// a test can move or remove the workspace in between. Not a setting: it
    /// is undocumented for users and only delays what the indexer reports.
    fn report_delay() -> Duration {
        std::env::var("LANGSPAN_TEST_INDEX_REPORT_DELAY_MS")
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
            .map_or(Duration::ZERO, |ms| Duration::from_millis(ms.min(5000)))
    }

    /// The index's single writer. It only ever reads working trees and its
    /// own file; what it learns reaches coordination state as a request to
    /// the state owner, like any other client's.
    fn index_worker(
        jobs: mpsc::Receiver<IndexJob>,
        transfers: mpsc::Receiver<Transfer>,
        sender: mpsc::SyncSender<Job>,
        path: PathBuf,
    ) {
        let mut index = match crate::graph::Index::open(&path) {
            Ok(index) => index,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"error","event":"index_unavailable","detail":error.to_string()})
                );
                return;
            }
        };
        index.set_files_per_pass(files_per_pass());
        let mut last_failure: Option<String> = None;
        let mut anchor_checks: u64 = 0;
        // Each workspace's last complete scan: an import loads only files
        // it lists, so the next pass cannot report a loaded file removed.
        let mut scans =
            std::collections::HashMap::<String, (Registration, crate::graph::ScanView)>::new();
        loop {
            while let Ok(request) = transfers.try_recv() {
                let outcome =
                    transfer(&mut index, &scans, &request).unwrap_or_else(|(code, message)| {
                        Outcome::Error {
                            code: code.into(),
                            message,
                        }
                    });
                let _ = request.reply.send(Response {
                    version: PROTOCOL_VERSION,
                    request_id: request.request_id,
                    replayed: false,
                    generation: request.generation,
                    lease_valid: None,
                    outcome,
                });
            }
            let job = match jobs.recv_timeout(Duration::from_millis(50)) {
                Ok(job) => job,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            let of = &job.of;
            match index.update(
                &of.workspace_id,
                &of.root,
                of.revision,
                &job.scan,
                now_ms(),
                deadline,
            ) {
                Ok(notices) => {
                    last_failure = None;
                    if !notices.is_empty() {
                        std::thread::sleep(report_delay());
                    }
                    for notice in notices {
                        // One request per transition of the file in one index
                        // pass of one registration: redelivery is a replay,
                        // and the same transition in a later pass (a file
                        // moved back and forth) or a later registration is a
                        // new request. A
                        // pass of a checkout the workspace has since left, by
                        // a move or a removal, is refused.
                        ask_as(
                            &sender,
                            Some(of.clone()),
                            crate::hook::wire_id(
                                "interface",
                                &[
                                    &of.workspace_id,
                                    &of.root.to_string_lossy(),
                                    &of.revision.to_string(),
                                    &notice.path,
                                    &notice.before_digest,
                                    &notice.digest,
                                    &notice.pass.to_string(),
                                ],
                            ),
                            Command::ObserveInterface {
                                workspace_id: of.workspace_id.clone(),
                                path: notice.path,
                                changes: notice.changes,
                                dependents: notice.dependents,
                            },
                        );
                    }
                    // Roadmap E1: anchored notes are compared with the index
                    // after every pass. A new ID each time, since a recheck
                    // that changed something keeps a receipt; one that
                    // changed nothing writes nothing.
                    anchor_checks += 1;
                    ask_as(
                        &sender,
                        Some(of.clone()),
                        crate::hook::wire_id(
                            "anchors",
                            &[
                                &of.workspace_id,
                                &of.revision.to_string(),
                                &std::process::id().to_string(),
                                &now_ms().to_string(),
                                &anchor_checks.to_string(),
                            ],
                        ),
                        Command::ObserveNoteAnchors {
                            workspace_id: of.workspace_id.clone(),
                        },
                    );
                }
                Err(error) => {
                    let detail = error.to_string();
                    if last_failure.as_ref() != Some(&detail) {
                        eprintln!(
                            "{}",
                            serde_json::json!({"level":"warn","event":"index_update_failed","workspace_id":of.workspace_id,"detail":detail})
                        );
                        last_failure = Some(detail);
                    }
                }
            }
            scans.insert(job.of.workspace_id.clone(), (job.of, job.scan));
        }
    }

    /// How often the daemon compacts when `retention_days` is set.
    const COMPACTION_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

    /// How often the state owner checks that its state directory is still
    /// the one it started with, and whether it has been idle too long.
    const WATCH_EVERY: Duration = Duration::from_secs(1);

    /// The state directory the daemon started with, by identity, so that a
    /// folder made later at the same path is not taken for it (issue #239).
    struct StateAnchor {
        path: PathBuf,
        device: u64,
        inode: u64,
    }
    impl StateAnchor {
        fn of(path: &Path) -> io::Result<Self> {
            let metadata = fs::symlink_metadata(path)?;
            Ok(Self {
                path: path.to_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }

        /// Why the daemon must stop serving this directory, if it must: it
        /// was removed, replaced by another folder, or is no longer
        /// owner-only. The reason code and a detail for the log.
        fn lost(&self) -> Option<(&'static str, String)> {
            match fs::symlink_metadata(&self.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Some((
                    "state_directory_removed",
                    format!("{} no longer exists", self.path.display()),
                )),
                // Unreadable is not proof that it is gone: the next check
                // looks again.
                Err(_) => None,
                Ok(metadata) if metadata.dev() != self.device || metadata.ino() != self.inode => {
                    Some((
                        "state_directory_replaced",
                        format!("{} is now another folder", self.path.display()),
                    ))
                }
                Ok(metadata) => local::check_private_directory(&metadata)
                    .err()
                    .map(|error| ("state_directory_not_private", error.to_string())),
            }
        }
    }

    /// How long a debug-build daemon may go without a client before it
    /// exits, so a coordinator left by a test that was killed ends by itself
    /// (issue #239). Five minutes, or `LANGSPAN_TEST_IDLE_EXIT_MS` (at least
    /// 100 ms; `0` turns it off). Not a setting: a release build has no idle
    /// exit and never reads the variable.
    #[cfg(debug_assertions)]
    fn idle_limit() -> Option<Duration> {
        idle_limit_of(std::env::var("LANGSPAN_TEST_IDLE_EXIT_MS").ok().as_deref())
    }
    #[cfg(not(debug_assertions))]
    fn idle_limit() -> Option<Duration> {
        None
    }
    #[cfg(debug_assertions)]
    const DEBUG_IDLE_LIMIT: Duration = Duration::from_secs(300);
    /// A value that is not a number keeps the default.
    #[cfg(debug_assertions)]
    fn idle_limit_of(value: Option<&str>) -> Option<Duration> {
        match value.and_then(|text| text.trim().parse::<u64>().ok()) {
            None => Some(DEBUG_IDLE_LIMIT),
            Some(0) => None,
            Some(ms) => Some(Duration::from_millis(ms.max(100))),
        }
    }

    /// When a client last connected or finished, for the idle limit.
    #[derive(Clone)]
    struct Activity(Arc<std::sync::Mutex<Instant>>);
    impl Activity {
        fn new() -> Self {
            Self(Arc::new(std::sync::Mutex::new(Instant::now())))
        }
        fn touch(&self) {
            *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        }
        fn idle_for(&self) -> Duration {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).elapsed()
        }
    }

    /// How long a stopping daemon waits for its running automatic checks to
    /// stop their commands.
    const CHECKS_STOP_WITHIN: Duration = Duration::from_secs(2);

    /// Logs why the daemon is stopping, stops any automatic check still
    /// running (its command's process group would otherwise outlive the
    /// daemon that enforces its time limit) and ends the process. Called by
    /// the state owner between requests, so no transaction of its own is
    /// open.
    fn stop(checks: &auto_check::Runs, level: &str, reason: &str, detail: &str, code: i32) -> ! {
        eprintln!(
            "{}",
            serde_json::json!({"level":level,"event":"daemon_stopping","reason":reason,"detail":detail})
        );
        end(checks, code)
    }

    /// Stops any automatic check still running, then ends the process.
    fn end(checks: &auto_check::Runs, code: i32) -> ! {
        if !checks.cancel_all(CHECKS_STOP_WITHIN) {
            eprintln!(
                "{}",
                serde_json::json!({"level":"warn","event":"auto_check_not_stopped","detail":format!("an automatic check was still running {} ms after it was cancelled", CHECKS_STOP_WITHIN.as_millis())})
            );
        }
        std::process::exit(code)
    }

    /// The signal that asked the coordinator to stop, once one has.
    type AskedToStop = Arc<std::sync::OnceLock<&'static str>>;

    /// Catches SIGTERM (`uninstall`, a service manager), SIGINT and SIGHUP
    /// (a terminal) so the state owner stops through [`stop`], which ends
    /// any running automatic check: its command runs in its own process
    /// group, so the default action would leave it running (issue #292).
    /// Replaces an inherited ignored disposition too. Called before the
    /// socket exists, so no client learns this process's id while a signal
    /// would still end it at once.
    fn stop_on_signal() -> Result<AskedToStop, crate::Error> {
        use tokio::signal::unix::{SignalKind, signal};
        let asked = AskedToStop::default();
        for (kind, name) in [
            (SignalKind::terminate(), "SIGTERM"),
            (SignalKind::interrupt(), "SIGINT"),
            (SignalKind::hangup(), "SIGHUP"),
        ] {
            let mut stream =
                signal(kind).map_err(|e| format!("listen for {name} to stop cleanly: {e}"))?;
            let asked = asked.clone();
            tokio::spawn(async move {
                if stream.recv().await.is_some() {
                    let _ = asked.set(name);
                }
            });
        }
        Ok(asked)
    }

    /// Makes a panic on any of the coordinator's threads end the process
    /// with status 1 (issue #329). Unwinding would end only that thread and
    /// leave the socket accepting requests nobody answers; a failed exit
    /// lets a login service start the coordinator again, and a removed
    /// socket lets a hook start one. Writes one structured line in place of
    /// the default message. Only `langspan daemon` installs it: the hooks
    /// keep their own panic fallback (`main.rs`).
    fn stop_on_panic(socket: PathBuf, checks: auto_check::Runs) {
        std::panic::set_hook(Box::new(move |info| {
            let _ = remove_own_socket(&socket);
            let thread = std::thread::current();
            let detail = format!(
                "{} at {}",
                info.payload_as_str().unwrap_or("a panic with no message"),
                info.location()
                    .map_or_else(|| "an unknown location".to_owned(), ToString::to_string)
            );
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","event":"daemon_stopping","reason":"panic","thread":thread.name().unwrap_or("unnamed"),"detail":detail})
            );
            end(&checks, 1)
        }));
    }

    /// `LANGSPAN_TEST_COMPACT_PAUSE_MS`, at most 10 s: how long a compaction
    /// holds its transaction open before committing, so that a test can
    /// kill the daemon mid-compaction. Unset in normal use.
    fn compaction_pause() -> Option<Duration> {
        std::env::var("LANGSPAN_TEST_COMPACT_PAUSE_MS")
            .ok()?
            .parse::<u64>()
            .ok()
            .map(|ms| Duration::from_millis(ms.min(10_000)))
    }

    /// For tests only, in a debug build: the state owner panics on the
    /// request whose ID `LANGSPAN_TEST_PANIC_ON_REQUEST` names, so a test can
    /// see what a panic in a coordinator thread does (issue #329). A release
    /// build never reads the variable.
    #[cfg(debug_assertions)]
    #[allow(clippy::panic, reason = "the test-only switch exists to panic")]
    fn panic_for_test(request_id: &str) {
        if std::env::var("LANGSPAN_TEST_PANIC_ON_REQUEST").as_deref() == Ok(request_id) {
            panic!("LANGSPAN_TEST_PANIC_ON_REQUEST named request {request_id}");
        }
    }
    #[cfg(not(debug_assertions))]
    fn panic_for_test(_: &str) {}

    /// The daily compaction. Its outcome is a log line; a failure leaves the
    /// log as it was and is tried again the next day.
    fn compact_daily(store: &mut Store, days: u32) {
        let line = match store.compact(days, true) {
            Ok(Outcome::Compaction { report }) => {
                if report.events == 0 && report.receipts == 0 {
                    return;
                }
                serde_json::json!({"level":"info","event":"compacted","report":report})
            }
            Ok(Outcome::Error { code, message }) => {
                serde_json::json!({"level":"error","event":"compaction_failed","code":code,"detail":message})
            }
            Ok(_) => return,
            Err(error) => {
                serde_json::json!({"level":"error","event":"compaction_failed","detail":error.to_string()})
            }
        };
        eprintln!("{line}");
    }

    /// A scan of the registration the workspace had when it was asked for.
    struct ScanResult {
        of: Registration,
        snapshot: Result<crate::observer::Snapshot, String>,
    }

    /// Remove a socket this user left at `path` from an earlier run. Anything
    /// else there (a file, a link, another user's socket) is refused.
    fn remove_own_socket(path: &Path) -> Result<(), crate::Error> {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == rustix::process::geteuid().as_raw() =>
            {
                Ok(fs::remove_file(path)?)
            }
            Ok(_) => Err(format!(
                "refusing to replace a non-socket or foreign {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )
            .into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn run(state_dir: PathBuf) -> Result<(), crate::Error> {
        let _ = STARTED_MS.set(now_ms());
        let directory = local::prepare_directory(&state_dir)
            .map_err(|e| format!("prepare state directory: {e}"))?;
        let anchor =
            StateAnchor::of(&directory).map_err(|e| format!("read the state directory: {e}"))?;
        let idle_limit = idle_limit();
        let activity = Activity::new();
        let lock = local::private_file(&directory.join("daemon.lock"), true)?;
        lock.try_lock()
            .map_err(|error| format!("another daemon may own this state directory: {error}"))?;
        let config =
            Config::read(&directory).map_err(|e| format!("read daemon configuration: {e}"))?;
        let _ = RECENT_WINDOW_MS.set(config.recent_window_ms);
        let _ = OUTLINE_MIN.set((config.outline_min_lines, config.outline_min_bytes));
        let asked_to_stop = stop_on_signal()?;
        local::private_file(&directory.join("state.sqlite3"), true)?;
        for name in ["state.sqlite3-wal", "state.sqlite3-shm"] {
            local::check_existing_file(&directory.join(name))?;
        }
        let mut store = Store::open(&directory.join("state.sqlite3"))
            .map_err(|e| format!("open coordination store: {e}"))?;
        store.set_compaction_pause(compaction_pause());
        let retention_days = config.retention_days;
        let generation = store.state().generation;
        let socket = directory.join("daemon.sock");
        // Clients look only at `daemon.sock` and refuse one that is not
        // owner-only. Binding creates the socket with the process umask, so
        // bind it under another name, make it owner-only, then rename it into
        // place: `daemon.sock` never exists with wider permissions, and once
        // it exists it is listening (issue #205). The name is no longer than
        // `daemon.sock`, so the state directory's length check covers it.
        let binding = directory.join("daemon.bind");
        for path in [&socket, &binding] {
            remove_own_socket(path)?;
        }
        let listener =
            UnixListener::bind(&binding).map_err(|e| format!("bind local Unix socket: {e}"))?;
        fs::set_permissions(&binding, fs::Permissions::from_mode(0o600))?;
        fs::rename(&binding, &socket)
            .map_err(|e| format!("move the local Unix socket into place: {e}"))?;
        let check_runs = auto_check::Runs::default();
        stop_on_panic(socket.clone(), check_runs.clone());
        let (sender, receiver) = mpsc::sync_channel::<Job>(config.max_clients);
        let (scan_sender, scan_receiver) = mpsc::sync_channel::<Registration>(1);
        let (scan_result_sender, scan_results) = mpsc::sync_channel::<ScanResult>(1);
        std::thread::Builder::new()
            .name("langspan-git-reader".into())
            .spawn(move || {
                while let Ok(of) = scan_receiver.recv() {
                    // Hook reports keep arriving while this thread reads the
                    // checkout: the core compares them with this time.
                    let from = now_ms();
                    let snapshot =
                        crate::observer::scan(&of.root, Instant::now() + Duration::from_secs(2))
                            .map(|mut snapshot| {
                                snapshot.scanned_from_ms = Some(from);
                                snapshot
                            });
                    if scan_result_sender
                        .send(ScanResult { of, snapshot })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        let graph_path = directory.join("graph.sqlite3");
        local::private_file(&graph_path, true)?;
        let (index_sender, index_receiver) = mpsc::sync_channel::<IndexJob>(1);
        let (transfer_sender, transfer_receiver) = mpsc::sync_channel::<Transfer>(1);
        {
            let sender = sender.clone();
            let graph_path = graph_path.clone();
            std::thread::Builder::new()
                .name("langspan-indexer".into())
                .spawn(move || {
                    index_worker(index_receiver, transfer_receiver, sender, graph_path)
                })?;
        }
        let (check_sender, check_receiver) = mpsc::sync_channel::<auto_check::Message>(256);
        let write_counts = auto_check::WriteCounts::default();
        {
            let sender = sender.clone();
            let check_sender = check_sender.clone();
            let write_counts = write_counts.clone();
            let check_runs = check_runs.clone();
            let timing = auto_check::Timing {
                debounce: Duration::from_millis(config.auto_check_debounce_ms),
                rerun_max: Duration::from_secs(config.flaky_rerun_max_s),
            };
            std::thread::Builder::new()
                .name("langspan-auto-checks".into())
                .spawn(move || {
                    auto_check::worker(
                        check_sender,
                        check_receiver,
                        timing,
                        write_counts,
                        check_runs,
                        |of: Option<&Registration>, id, command| {
                            ask_as(&sender, of.cloned(), id, command)
                        },
                    )
                })?;
        }
        let (notice_sender, notice_receiver) = mpsc::sync_channel::<notify::Notice>(notify::QUEUE);
        {
            // Read again for each notice, so `langspan notify on|off` needs
            // no restart.
            let directory = directory.clone();
            std::thread::Builder::new()
                .name("langspan-notices".into())
                .spawn(move || {
                    notify::worker(notice_receiver, || {
                        Config::read(&directory)
                            .map(|config| config.notice_settings())
                            .map_err(|error| error.to_string())
                    })
                })?;
        }
        let handoff_after = Duration::from_millis(config.notify_handoff_after_ms);
        store.tap(notify::relevant);
        let slots = Arc::new(Semaphore::new(config.max_clients));
        // A write check reads the line journal under the state directory.
        let owner_directory = directory.clone();
        let owner_slots = slots.clone();
        let owner_activity = activity.clone();
        let max_clients = config.max_clients;
        std::thread::Builder::new()
            .name("langspan-owner".into())
            .spawn(move || {
                // Holding the file handle keeps ownership for exactly the writer's lifetime.
                let _lock = lock;
                let mut handoffs = notify::Handoffs::default();
                // Never waits: a notice that finds the queue full is dropped.
                let raise = |notice: notify::Notice| {
                    let kind = notice.kind.name();
                    if notice_sender.try_send(notice).is_err() {
                        eprintln!("{}", serde_json::json!({"level":"info","event":"notice_dropped","kind":kind,"reason":"queue_full"}));
                    }
                };
                let mut scan_pending = false;
                let mut graph_reader: Option<crate::graph::Index> = None;
                let mut next_scan = Instant::now();
                let mut next_liveness = Instant::now();
                let mut previous_workspace: Option<String> = None;
                let mut previous_session: Option<String> = None;
                let mut scan_errors = std::collections::BTreeMap::<String, String>::new();
                let mut last_maintenance_error: Option<String> = None;
                let mut urgent = std::collections::VecDeque::<String>::new();
                let mut last_scanned = std::collections::HashMap::<String, Instant>::new();
                // The first compaction runs as the writer starts, then one a
                // day.
                let mut next_compaction = Instant::now();
                let mut next_watch = Instant::now() + WATCH_EVERY;
                loop {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(job) => {
                            panic_for_test(&job.request.request_id);
                            if graph_reader.is_none() {
                                graph_reader = crate::graph::Index::open_reader(&graph_path).ok();
                            }
                            if let Some(refused) = stale_report(store.state(), &job) {
                                let _ = job.reply.send(refused);
                            } else if is_transfer(&job.request.command) {
                                forward_transfer(store.state(), &transfer_sender, job);
                            } else {
                            let touched = touched_workspace(store.state(), &job.request.command);
                            let check_before = auto_check_of(store.state(), &job.request.command);
                            let boundary = boundary_session(&job.request.command);
                            let response = handle(&mut store, graph_reader.as_ref(), &owner_directory, job.request);
                            let messages = [
                                auto_check_message(store.state(), graph_reader.as_ref(), check_before, boundary, &response),
                                interface_message(store.state(), graph_reader.as_ref(), &response),
                            ];
                            for message in messages.into_iter().flatten() {
                                // Never waits: a full queue loses this one
                                // trigger, and the session's next one counts.
                                if let Err(error) = check_sender.try_send(message) {
                                    eprintln!("{}", serde_json::json!({"level":"warn","event":"auto_check_unavailable","detail":error.to_string()}));
                                }
                            }
                            // A workspace where a session just acted is scanned
                            // next, not after every other workspace in turn.
                            if let Some(id) = touched
                                && last_scanned.get(&id).is_none_or(|at| at.elapsed() >= URGENT_AFTER)
                                && !urgent.contains(&id)
                                && urgent.len() < 64
                            {
                                urgent.push_back(id);
                            }
                            let _ = job.reply.send(response); // Retry recovers a committed but dropped reply.
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let now = now_ms();
                    match store.refresh(now) {
                        Ok(()) => { last_maintenance_error = None; }
                        Err(error) => {
                            let message = error.to_string();
                            if last_maintenance_error.as_ref() != Some(&message) {
                                eprintln!("{}", serde_json::json!({"level":"error","event":"maintenance_failed","detail":message}));
                                last_maintenance_error = Some(message);
                            }
                        }
                    }
                    if let Ok(ScanResult {of, snapshot}) = scan_results.try_recv() {
                        scan_pending = false;
                        let workspace_id = of.workspace_id.clone();
                        // The same registration: a scan of another root, or
                        // of the workspace's earlier lifetime, is not
                        // recorded for this one.
                        let recorded = if of.is_current(store.state()) {
                            snapshot.and_then(|snapshot| {
                                store.record_scan(&workspace_id, &snapshot, now).map_err(|e| e.to_string())?;
                                // Hand the scan to the indexer. If it is busy the
                                // next scan carries the same information.
                                let _ = index_sender.try_send(IndexJob {
                                    of,
                                    scan: crate::graph::ScanView::of(&snapshot),
                                });
                                Ok(snapshot.coverage)
                            })
                        } else { Err("workspace changed before the observation could be recorded".into()) };
                        // The outcome of every attempt is state, not only a log
                        // line: a failed scan means what LangSpan shows for this
                        // workspace is as of the last one that completed.
                        if store.state().workspaces.contains_key(&workspace_id) {
                            let command = Command::ObserveScan {
                                workspace_id: workspace_id.clone(),
                                failure: recorded.as_ref().err().cloned(),
                                coverage: recorded.as_ref().ok().cloned(),
                            };
                            let id = format!("scan-outcome-{}-{now}", store.state().generation);
                            let _ = store.execute(&id, &command, &command, now);
                            observe_elsewhere(&mut store, graph_reader.as_ref(), &workspace_id, now);
                        }
                        match recorded {
                            Ok(_) => {
                                if scan_errors.remove(&workspace_id).is_some() {
                                    eprintln!("{}", serde_json::json!({"level":"info","event":"git_observation_recovered","workspace_id":workspace_id}));
                                }
                            }
                            Err(detail) => {
                                if scan_errors.get(&workspace_id) != Some(&detail) {
                                    eprintln!("{}", serde_json::json!({"level":"warn","event":"git_observation_incomplete","workspace_id":workspace_id,"detail":detail}));
                                    scan_errors.insert(workspace_id, detail);
                                }
                            }
                        }
                    }
                    if let Some(days) = retention_days
                        && Instant::now() >= next_compaction
                    {
                        next_compaction = Instant::now() + COMPACTION_EVERY;
                        compact_daily(&mut store, days);
                    }
                    if Instant::now() >= next_liveness {
                        observe_processes(&mut store, &mut previous_session, now);
                        for notice in handoffs.due(store.state(), Instant::now(), handoff_after) {
                            raise(notice);
                        }
                        next_liveness = Instant::now() + Duration::from_secs(1);
                    }
                    // Between requests, so nothing this thread writes is cut
                    // short (issue #239).
                    if let Some(signal) = asked_to_stop.get() {
                        stop(&check_runs, "info", "signal", &format!("received {signal}"), 0);
                    }
                    if Instant::now() >= next_watch {
                        next_watch = Instant::now() + WATCH_EVERY;
                        if let Some((reason, detail)) = anchor.lost() {
                            stop(&check_runs, "warn", reason, &detail, 1);
                        }
                        // A running automatic check is activity too: its
                        // result is still to be recorded.
                        if let Some(limit) = idle_limit
                            && owner_slots.available_permits() == max_clients
                            && !check_runs.any()
                            && owner_activity.idle_for() >= limit
                        {
                            stop(&check_runs, "info", "idle", &format!("no client for {} ms", limit.as_millis()), 0);
                        }
                    }
                    // Facts already committed, whoever asked for them.
                    let tapped = store.take_tapped();
                    for notice in notify::notices_for(store.state(), &tapped) {
                        raise(notice);
                    }
                    if !scan_pending && (!urgent.is_empty() || Instant::now() >= next_scan) {
                        let workspace = std::iter::from_fn(|| urgent.pop_front())
                            .find_map(|id| Registration::of(store.state(), &id))
                            .or_else(|| {
                                let next = next_workspace(store.state(), previous_workspace.as_deref())
                                    .and_then(|(id, _)| Registration::of(store.state(), &id));
                                if let Some(of) = &next {
                                    previous_workspace = Some(of.workspace_id.clone());
                                }
                                next
                            });
                        if let Some(of) = workspace {
                            last_scanned.insert(of.workspace_id.clone(), Instant::now());
                            match scan_sender.try_send(of) {
                                Ok(()) => { scan_pending = true; }
                                Err(error) => eprintln!("{}", serde_json::json!({"level":"error","event":"git_worker_unavailable","detail":error.to_string()})),
                            }
                        }
                        next_scan = Instant::now() + Duration::from_secs(2);
                    }
                    // What the automatic-check worker marks a run with. Only
                    // ever behind the state, so a stale mark refuses a
                    // comparison and never allows one.
                    let counts = &store.state().writes;
                    let mut published = write_counts.lock().unwrap_or_else(|e| e.into_inner());
                    if *published != *counts {
                        published.clone_from(counts);
                    }
                }
            })?;
        {
            let sender = sender.clone();
            let directory = directory.clone();
            let pass = config.remote_pass_ms;
            std::thread::Builder::new()
                .name("langspan-remote".into())
                .spawn(move || {
                    remote_worker(sender, directory, generation, Duration::from_millis(pass))
                })?;
        }
        {
            let sender = sender.clone();
            let directory = directory.clone();
            let every = Duration::from_millis(config.host_check_ms);
            std::thread::Builder::new()
                .name("langspan-hosts".into())
                .spawn(move || host_worker(sender, directory, every))?;
        }
        eprintln!(
            "{}",
            serde_json::json!({"level":"info","event":"daemon_ready","generation":generation})
        );
        loop {
            let (stream, _) = listener.accept().await?;
            activity.touch();
            if !local::peer_is_owner(&stream) {
                continue;
            }
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let sender = sender.clone();
            let config = config.clone();
            let activity = activity.clone();
            tokio::spawn(async move {
                let _permit = permit;
                serve_connection(stream, sender, config).await;
                activity.touch();
            });
        }
    }

    const REMOTE_DEADLINE: Duration = Duration::from_secs(20);
    /// Unchanged work is republished this often, so teammates can tell a
    /// quiet participant from a disconnected one.
    const REMOTE_HEARTBEAT: Duration = Duration::from_secs(120);

    /// Check the agent hosts in the daemon's home at start and every
    /// `every` after (roadmap A7). Reads directory entries and LangSpan's
    /// own entries in host files, never writes them; what it finds reaches
    /// the state owner as one request naming the uncovered hosts.
    fn host_worker(sender: mpsc::SyncSender<Job>, directory: PathBuf, every: Duration) {
        let home = match crate::hosts::UserHome::new(None) {
            Ok(home) => home,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"warn","event":"host_check_unavailable","detail":error.to_string()})
                );
                return;
            }
        };
        let mut checks: u64 = 0;
        let mut last_failure: Option<String> = None;
        loop {
            let known = match ask(&sender, "read".into(), Command::Status) {
                Some(Outcome::Status { state, .. }) => Some((
                    state
                        .workspaces
                        .values()
                        .map(|workspace| PathBuf::from(&workspace.root))
                        .collect::<Vec<_>>(),
                    state.uninstalled_hosts.keys().cloned().collect::<Vec<_>>(),
                )),
                _ => None,
            };
            let failure = match known {
                // The owner is busy or stopping: the next check counts.
                None => Some("the state owner did not answer".to_owned()),
                Some((roots, held)) => {
                    let reports = crate::hosts::report(&home, &directory);
                    // A host that could not be inspected keeps the notice
                    // it has, and the reason is logged.
                    let observation = crate::hosts::observe(&reports, &roots, &directory, &held);
                    checks += 1;
                    let answer = match ask(
                        &sender,
                        format!("hosts-{}-{checks}", now_ms()),
                        Command::ObserveHosts {
                            not_installed: observation.not_installed,
                        },
                    ) {
                        Some(Outcome::HostsObserved { .. }) => None,
                        Some(Outcome::Error { code, message }) => {
                            Some(format!("{code}: {message}"))
                        }
                        _ => Some("the state owner did not answer".to_owned()),
                    };
                    let mut problems = observation.errors;
                    problems.extend(answer);
                    (!problems.is_empty()).then(|| problems.join("; "))
                }
            };
            if failure != last_failure {
                if let Some(detail) = &failure {
                    eprintln!(
                        "{}",
                        serde_json::json!({"level":"warn","event":"host_check_failed","detail":detail})
                    );
                }
                last_failure = failure;
            }
            std::thread::sleep(every);
        }
    }

    /// Ask the single state owner, exactly as a socket client would.
    fn ask(
        sender: &mpsc::SyncSender<Job>,
        request_id: String,
        command: Command,
    ) -> Option<Outcome> {
        ask_as(sender, None, request_id, command)
    }

    /// Ask the state owner to record what a worker learned during the
    /// registration `of`: refused if the workspace is no longer in it.
    fn ask_as(
        sender: &mpsc::SyncSender<Job>,
        of: Option<Registration>,
        request_id: String,
        command: Command,
    ) -> Option<Outcome> {
        let (reply, receive) = oneshot::channel();
        sender
            .try_send(Job {
                request: Request::new(request_id, command),
                reply,
                of,
            })
            .ok()?;
        receive
            .blocking_recv()
            .ok()
            .map(|response| response.outcome)
    }

    /// Why a remote pass failed. `class` names the network operation for a
    /// failure that waiting can help (an unreachable remote, refused
    /// authentication, a deadline); it is `None` for one that the remote
    /// answered, which never delays the next pass.
    struct RemoteFailure {
        class: Option<&'static str>,
        detail: String,
    }

    /// What the remote worker remembers about one publishing workspace.
    #[derive(Default)]
    struct Linked {
        seen: std::collections::BTreeSet<String>,
        last_publish: Option<Instant>,
        /// The failure last logged and the wait it caused.
        failure: Option<(String, Duration)>,
        backoff: crate::remote::Backoff,
        refused: Refused,
        retire_refused: Refused,
    }

    /// A push the host refused for good (HTTP 401/403, refusing, denied,
    /// protected): `what` is not pushed again until its own backoff, set
    /// straight to the longest wait, is due. A different publication is
    /// tried at once. Kept apart from the pass's backoff so that receiving
    /// teammates' publications goes on meanwhile.
    #[derive(Default)]
    struct Refused {
        what: String,
        detail: String,
        backoff: crate::remote::Backoff,
    }

    impl Refused {
        fn held(&self, what: &str) -> Option<String> {
            (self.what == what && !self.backoff.due(Instant::now())).then(|| self.detail.clone())
        }
        fn hold(what: &str, class: &'static str, detail: &str) -> Self {
            let mut backoff = crate::remote::Backoff::default();
            backoff.refused(class, Instant::now());
            Self {
                what: what.to_owned(),
                detail: detail.to_owned(),
                backoff,
            }
        }
    }

    fn network(class: &'static str) -> impl FnOnce(String) -> RemoteFailure {
        move |detail| RemoteFailure {
            class: Some(class),
            detail,
        }
    }

    impl From<String> for RemoteFailure {
        fn from(detail: String) -> Self {
            Self {
                class: None,
                detail,
            }
        }
    }

    /// One pass for one publishing workspace: receive, then publish if due.
    fn remote_pass(
        sender: &mpsc::SyncSender<Job>,
        directory: &Path,
        generation: u64,
        root: &Path,
        link: &langspan_core::RemoteLink,
        linked: &mut Linked,
    ) -> Result<(), RemoteFailure> {
        let Linked {
            seen,
            last_publish,
            refused,
            retire_refused,
            ..
        } = linked;
        // Ordering and replay are decided by the core from each sender's
        // sequence, so every attempt is its own request.
        static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let generation = generation.to_string();
        let id = |kind: &str, subject: &str| {
            let attempt = ATTEMPT
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .to_string();
            crate::hook::wire_id(kind, &[&link.workspace_id, subject, &generation, &attempt])
        };
        let deadline = Instant::now() + REMOTE_DEADLINE;
        let received = crate::remote::fetch(root, link, deadline).map_err(network("fetch"))?;
        let mut rejected = Vec::new();
        for publication in received.publications {
            let request_id = id("remote-observe", &publication.participant);
            if let Some(Outcome::Error { code, message }) = ask(
                sender,
                request_id,
                Command::ObserveRemote {
                    workspace_id: link.workspace_id.clone(),
                    publication,
                },
            ) {
                rejected.push(format!("{code}: {message}"));
            }
        }
        // A ref that was there and is gone is a withdrawal, but only when
        // every participant on the remote was listed.
        if received.complete {
            for participant in seen
                .iter()
                .filter(|participant| !received.present.contains(participant))
            {
                ask(
                    sender,
                    id("remote-forget", participant),
                    Command::ForgetRemote {
                        workspace_id: link.workspace_id.clone(),
                        participant: participant.clone(),
                    },
                );
            }
            *seen = received
                .present
                .into_iter()
                .filter(|participant| participant != &link.participant)
                .collect();
        }
        // A move to the other namespace leaves the old ref behind; it is
        // removed once the new one is on the remote, checked every pass.
        // A refusal here never stops publication: it is reported, and a
        // refusal for good is asked again only after the longest wait.
        let retire = match retire_refused.held("retire") {
            Some(detail) => Some(detail),
            None => match crate::remote::retire_other(root, link, deadline) {
                Ok(()) => {
                    *retire_refused = Refused::default();
                    None
                }
                Err(refusal) => {
                    if refusal.permanent {
                        *retire_refused = Refused::hold("retire", "retire", &refusal.detail);
                    }
                    Some(refusal.detail)
                }
            },
        };
        // Reported through the same once-per-change path as any failure.
        let rejections = |rejected: &[String]| match rejected.first() {
            _ if retire.is_some() => Err(retire.clone().unwrap_or_default()),
            None => Ok(()),
            Some(first) => Err(format!(
                "{} publication(s) from teammates were rejected: {first}",
                rejected.len()
            )),
        };
        let Some(Outcome::RemoteOutbox {
            publication,
            signature,
            changed,
        }) = ask(
            sender,
            "read".into(),
            Command::RemoteOutbox {
                workspace_id: link.workspace_id.clone(),
            },
        )
        else {
            return Err(String::from("the daemon did not return this workspace's outbox").into());
        };
        let heartbeat = !publication.work.is_empty()
            && last_publish.is_none_or(|at| at.elapsed() >= REMOTE_HEARTBEAT);
        if !changed && !heartbeat {
            return rejections(&rejected).map_err(RemoteFailure::from);
        }
        // A refusal is the destination's answer about this content: a move
        // to another remote or namespace is asked afresh.
        let refusal_key = format!(
            "{}\n{}\n{signature}",
            link.remote,
            crate::remote::peer_ref(link)
        );
        if let Some(detail) = refused.held(&refusal_key) {
            return Err(detail.into());
        }
        let payload = match crate::remote::publish(root, directory, link, &publication, deadline) {
            Ok(payload) => payload,
            // The host answered: the pass's own backoff is not touched, so
            // receipt goes on; only this push waits.
            Err(refusal) if refusal.permanent => {
                *refused = Refused::hold(&refusal_key, "publish", &refusal.detail);
                return Err(refusal.detail.into());
            }
            Err(refusal) => return Err(network("publish")(refusal.detail)),
        };
        *refused = Refused::default();
        let published_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        *last_publish = Some(Instant::now());
        // Recorded after the push: the log holds what left, not what was
        // tried. If recording fails, the same sequence is pushed again next
        // pass, which replaces the ref with the same content.
        match ask(
            sender,
            id("remote-published", &publication.sequence.to_string()),
            Command::RemotePublished {
                workspace_id: link.workspace_id.clone(),
                sequence: publication.sequence,
                signature,
                payload,
                published_at_ms,
                // Where this push went: the owner may have moved meanwhile.
                as_branch: Some(link.publish_as_branch),
            },
        ) {
            Some(Outcome::Error { code, message }) => Err(format!("{code}: {message}").into()),
            Some(_) => rejections(&rejected).map_err(RemoteFailure::from),
            None => Err(String::from("publication was pushed but could not be recorded").into()),
        }
    }

    /// Remove a disabled workspace's ref. Repeated every pass, across
    /// restarts, until the remote confirms it is gone. Where the host refuses
    /// the deletion, the ref is replaced once with a final "withdrawn"
    /// publication, so teammates stop showing this machine's work, and the
    /// owner is told the ref is still there.
    fn remote_withdraw(
        sender: &mpsc::SyncSender<Job>,
        directory: &Path,
        generation: u64,
        root: &Path,
        link: &langspan_core::RemoteLink,
        refused: &mut Refused,
    ) -> Result<(), RemoteFailure> {
        const WHAT: &str = "withdraw";
        if let Some(detail) = refused.held(WHAT) {
            return Err(detail.into());
        }
        let deadline = Instant::now() + REMOTE_DEADLINE;
        crate::remote::fetch(root, link, deadline).map_err(network("fetch"))?;
        let crate::remote::Withdrawal::Refused {
            remote_ref,
            said,
            mut told,
        } = crate::remote::withdraw(root, link, deadline).map_err(network("withdraw"))?
        else {
            ask(
                sender,
                crate::hook::wire_id(
                    "remote-withdrawn",
                    &[
                        &link.workspace_id,
                        &link.sequence.to_string(),
                        &generation.to_string(),
                    ],
                ),
                Command::RemoteWithdrawn {
                    workspace_id: link.workspace_id.clone(),
                },
            );
            *refused = Refused::default();
            return Ok(());
        };
        let mut detail = None;
        if !told {
            let (publication, target) = crate::remote::withdrawal(link, &remote_ref);
            match crate::remote::publish(root, directory, &target, &publication, deadline) {
                Ok(payload) => {
                    told = true;
                    let published_at_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
                    // Recorded like any publication, so `remote sent` shows
                    // it and enabling again never reuses the number.
                    if let Some(Outcome::Error { code, message }) = ask(
                        sender,
                        crate::hook::wire_id(
                            "remote-published",
                            &[
                                &link.workspace_id,
                                &publication.sequence.to_string(),
                                &generation.to_string(),
                            ],
                        ),
                        Command::RemotePublished {
                            workspace_id: link.workspace_id.clone(),
                            sequence: publication.sequence,
                            signature: "withdrawn".into(),
                            payload,
                            published_at_ms,
                            as_branch: None,
                        },
                    ) {
                        detail = Some(format!(
                            "the final withdrawn publication was pushed but could not be recorded: {code}: {message}"
                        ));
                    }
                }
                Err(refusal) => {
                    detail = Some(format!(
                        "the final withdrawn publication was refused too: {}",
                        refusal.detail
                    ))
                }
            }
        }
        let mut message = crate::remote::refused_withdrawal(link, &remote_ref, &said, told);
        if let Some(detail) = detail {
            message.push_str(&format!(" ({detail})"));
        }
        if !crate::remote::permanent(&said) {
            return Err(network("withdraw")(message));
        }
        // The host will refuse again: ask it again only after the longest
        // wait, in case the owner deleted the ref by hand meanwhile.
        if told {
            *refused = Refused::hold(WHAT, "withdraw", &message);
        }
        Err(message.into())
    }

    /// Network work never runs on the state owner's thread. A failure here
    /// leaves local coordination exactly as it was.
    fn remote_worker(
        sender: mpsc::SyncSender<Job>,
        directory: PathBuf,
        generation: u64,
        pass: Duration,
    ) {
        let mut linked = std::collections::BTreeMap::<String, Linked>::new();
        loop {
            std::thread::sleep(pass);
            let Some(Outcome::Status { state, .. }) = ask(&sender, "read".into(), Command::Status)
            else {
                continue;
            };
            linked.retain(|workspace_id, _| state.remote_links.contains_key(workspace_id));
            for (workspace_id, link) in &state.remote_links {
                let Some(workspace) = state.workspaces.get(workspace_id) else {
                    continue;
                };
                let root = Path::new(&workspace.root);
                let entry = linked.entry(workspace_id.clone()).or_default();
                if !entry.backoff.due(Instant::now()) {
                    continue;
                }
                let result = if link.enabled {
                    remote_pass(&sender, &directory, generation, root, link, entry)
                } else if link.withdrawn {
                    continue;
                } else {
                    entry.seen.clear();
                    entry.last_publish = None;
                    remote_withdraw(
                        &sender,
                        &directory,
                        generation,
                        root,
                        link,
                        &mut entry.refused,
                    )
                };
                // Local coordination never waits for this: only this
                // workspace's next remote pass is delayed.
                let failure = match result {
                    Ok(()) => {
                        entry.backoff.succeeded();
                        None
                    }
                    Err(RemoteFailure { class, detail }) => {
                        let wait = match class {
                            Some(class) => entry.backoff.failed(class, Instant::now()),
                            None => {
                                entry.backoff.succeeded();
                                Duration::ZERO
                            }
                        };
                        Some((detail, wait))
                    }
                };
                // Logged once per change of failure or of the wait it causes.
                match (&failure, &entry.failure) {
                    (None, Some(_)) => eprintln!(
                        "{}",
                        serde_json::json!({"level":"info","event":"remote_recovered","workspace_id":workspace_id})
                    ),
                    (Some((detail, wait)), previous) if previous.as_ref() != failure.as_ref() => {
                        eprintln!(
                            "{}",
                            serde_json::json!({"level":"warn","event":"remote_unavailable","workspace_id":workspace_id,"detail":detail,"retry_in_ms":wait.as_millis() as u64})
                        )
                    }
                    _ => {}
                }
                entry.failure = failure;
            }
        }
    }

    /// The refusal of a worker's report about a registration the workspace
    /// has left: moved or switched to another root, or removed (and perhaps
    /// registered again). Nothing is handled or recorded; the worker's next
    /// pass, of the current registration, says what is true now.
    fn stale_report(state: &State, job: &Job) -> Option<Response> {
        let of = job.of.as_ref().filter(|of| !of.is_current(state))?;
        Some(error(
            job.request.request_id.clone(),
            state.generation,
            "workspace_changed",
            format!(
                "workspace {} was moved, switched or removed after this was observed in {}; nothing was recorded",
                of.workspace_id,
                of.root.display()
            ),
        ))
    }

    /// Why a request of another protocol version is refused.
    fn unsupported_version() -> String {
        format!(
            "expected protocol version {} to {PROTOCOL_VERSION}; restart the daemon after upgrading LangSpan",
            protocol::OLDEST_PROTOCOL_VERSION
        )
    }

    pub(super) fn error(
        id: String,
        generation: u64,
        code: &str,
        message: impl Into<String>,
    ) -> Response {
        Response {
            version: PROTOCOL_VERSION,
            request_id: id,
            replayed: false,
            generation,
            lease_valid: None,
            outcome: Outcome::Error {
                code: code.into(),
                message: message.into(),
            },
        }
    }
    fn response(id: String, execution: Execution) -> Response {
        Response {
            version: PROTOCOL_VERSION,
            request_id: id,
            replayed: execution.replayed,
            generation: execution.generation,
            lease_valid: execution.lease_valid,
            outcome: execution.outcome,
        }
    }
    /// The time the core is told: see `crate::clock` for why it counts sleep.
    pub(super) fn now_ms() -> u64 {
        crate::clock::now_ms()
    }
    /// When this daemon started, on the same clock, for the uptime shown.
    static STARTED_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    /// The configured `recent_window_ms`, set once at start.
    pub(super) static RECENT_WINDOW_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    /// `outline_min_lines` and `outline_min_bytes`, set once at start.
    pub(super) static OUTLINE_MIN: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    /// The defaults of `outline_min_lines` and `outline_min_bytes`.
    pub(super) const OUTLINE_MIN_LINES: u64 = 1500;
    pub(super) const OUTLINE_MIN_BYTES: u64 = 64 * 1024;
    /// A workspace scanned less than this long ago is not moved up again.
    const URGENT_AFTER: Duration = Duration::from_secs(5);

    /// The workspace a request is about, when it is one where a session is
    /// acting: a registration, an edit, a prompt.
    fn touched_workspace(state: &State, command: &Command) -> Option<String> {
        match command {
            Command::WorkspaceRegister { workspace_id, .. }
            | Command::WorkspaceMove { workspace_id, .. } => Some(workspace_id.clone()),
            Command::AdmitTool { session_id, .. }
            | Command::ObserveTool { session_id, .. }
            | Command::SessionPrompt { session_id, .. } => state
                .sessions
                .get(session_id)
                .map(|session| session.workspace_id.clone()),
            _ => None,
        }
    }

    /// A workspace's automatic check and root before a request that can
    /// change either.
    type CheckBefore = (String, Option<langspan_core::AutoCheck>, Option<String>);

    /// For a request that turns a workspace's automatic check on or off, or
    /// moves, switches or removes the workspace: that workspace with its
    /// check and root before the request.
    fn auto_check_of(state: &State, command: &Command) -> Option<CheckBefore> {
        match command {
            Command::AutoCheckEnable { workspace_id, .. }
            | Command::AutoCheckDisable { workspace_id }
            | Command::WorkspaceMove { workspace_id, .. }
            | Command::WorkspaceRemove { workspace_id, .. } => Some((
                workspace_id.clone(),
                state.auto_checks.get(workspace_id).cloned(),
                state
                    .workspaces
                    .get(workspace_id)
                    .map(|workspace| workspace.root.clone()),
            )),
            _ => None,
        }
    }

    /// The session of a tool batch or a stop: where a check may start.
    fn boundary_session(command: &Command) -> Option<String> {
        match command {
            Command::ObserveToolBatch { session_id, .. }
            | Command::SessionStop { session_id, .. } => Some(session_id.clone()),
            _ => None,
        }
    }

    /// What the automatic-check worker is told after a request: a session
    /// that wrote reached a batch or stop in a workspace with a check on, or
    /// the owner changed the check or the workspace's root. What waits or
    /// runs for the old check or in the old root is stopped; the setting
    /// itself applies to the next writes. A replayed request tells it
    /// nothing.
    fn auto_check_message(
        state: &State,
        graph: Option<&crate::graph::Index>,
        check_before: Option<CheckBefore>,
        boundary: Option<String>,
        response: &Response,
    ) -> Option<auto_check::Message> {
        if response.replayed {
            return None;
        }
        if let Some((workspace_id, before, root_before)) = check_before {
            let root = state
                .workspaces
                .get(&workspace_id)
                .map(|workspace| &workspace.root);
            return (state.auto_checks.get(&workspace_id) != before.as_ref()
                || root != root_before.as_ref())
            .then_some(auto_check::Message::Off { workspace_id });
        }
        let session_id = boundary?;
        let wrote = matches!(
            response.outcome,
            Outcome::ToolBatchObserved { wrote: true, .. }
                | Outcome::SessionStopped { wrote: true, .. }
        );
        let workspace_id = &state.sessions.get(&session_id)?.workspace_id;
        let check = state.auto_checks.get(workspace_id).filter(|_| wrote)?;
        let of = Registration::of(state, workspace_id)?;
        // Read only for a check with a wider command: one bounded query of
        // the index's own file.
        let index_scanned = check.on_interface_change.is_some()
            && index_coverage(graph, &of).is_some_and(|coverage| coverage.scanned);
        Some(auto_check::Message::Wrote {
            check: check.clone(),
            of,
            session_id,
            index_scanned,
        })
    }

    fn index_coverage(
        graph: Option<&crate::graph::Index>,
        of: &Registration,
    ) -> Option<langspan_core::GraphCoverage> {
        graph?
            .with_coverage(of, |_| Ok(()))
            .ok()
            .map(|((), coverage)| coverage)
    }

    /// An interface notice the index just reported, for the automatic-check
    /// worker when the workspace's check has a wider command and a session's
    /// writes changed an export that other files mention. A replayed report
    /// tells it nothing.
    fn interface_message(
        state: &State,
        graph: Option<&crate::graph::Index>,
        response: &Response,
    ) -> Option<auto_check::Message> {
        let Outcome::InterfaceObserved { notice } = &response.outcome else {
            return None;
        };
        if response.replayed || notice.dependents.is_empty() {
            return None;
        }
        state
            .auto_checks
            .get(&notice.workspace_id)?
            .on_interface_change
            .as_ref()?;
        let by_session = notice.by_session.clone()?;
        if state.sessions.get(&by_session)?.workspace_id != notice.workspace_id {
            return None;
        }
        let of = Registration::of(state, &notice.workspace_id)?;
        Some(auto_check::Message::Interface {
            workspace_id: notice.workspace_id.clone(),
            change: auto_check::Pending {
                notice_id: notice.notice_id.clone(),
                path: notice.path.clone(),
                changes: notice.changes.clone(),
                dependents: notice.dependents.clone(),
                by_session,
                blind_files: index_coverage(graph, &of)
                    .map_or(0, |coverage| coverage.blind_in_scope),
            },
        })
    }

    fn next_workspace(state: &State, after: Option<&str>) -> Option<(String, PathBuf)> {
        use std::ops::Bound::{Excluded, Unbounded};
        let next = after
            .and_then(|cursor| {
                state
                    .workspaces
                    .range((Excluded(cursor.to_owned()), Unbounded))
                    .next()
            })
            .or_else(|| state.workspaces.iter().next());
        next.map(|(id, workspace)| (id.clone(), PathBuf::from(&workspace.root)))
    }
    /// An unchanged process observation younger than this is not recorded
    /// again, nor re-probed when a hook reports it.
    const LIVENESS_COALESCE_MS: u64 = 15_000;
    fn observe_processes(store: &mut Store, after: &mut Option<String>, now: u64) {
        use std::ops::Bound::{Excluded, Unbounded};
        let mut observations: Vec<_> = if let Some(cursor) = after.as_ref() {
            store
                .state()
                .session_liveness
                .range((Excluded(cursor.clone()), Unbounded))
                .take(64)
                .map(|(_, value)| value.clone())
                .collect()
        } else {
            Vec::new()
        };
        if observations.is_empty() {
            observations = store
                .state()
                .session_liveness
                .values()
                .take(64)
                .cloned()
                .collect();
        }
        // One probe for the whole page: on macOS a single process listing.
        let processes: Vec<_> = observations
            .iter()
            .map(|prior| prior.process.clone())
            .collect();
        let statuses = crate::process::probe_all(&processes);
        for (index, (prior, status)) in observations.into_iter().zip(statuses).enumerate() {
            *after = Some(prior.session_id.clone());
            let same = prior.generation == store.state().generation && prior.status == status;
            // Dead incarnations remain dead. Other unchanged samples are
            // coalesced, while every transition is recorded at the next poll.
            if same
                && (status == langspan_core::LivenessStatus::Exited
                    || now.saturating_sub(prior.observed_at_ms) < LIVENESS_COALESCE_MS)
            {
                continue;
            }
            let command = Command::ObserveSessionLiveness {
                session_id: prior.session_id,
                process: prior.process,
                status,
            };
            let id = format!("liveness-{}-{now}-{index}", store.state().generation);
            let failure = match store.execute(&id, &command, &command, now) {
                Ok(Execution {
                    outcome: Outcome::Error { code, message },
                    ..
                }) => Some(format!("{code}: {message}")),
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            if let Some(detail) = failure {
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"warn","event":"process_observation_failed","detail":detail})
                );
            }
        }
    }
    fn current_response(store: &Store, id: String, mut execution: Execution) -> Response {
        if let Outcome::Admitted { lease } = &execution.outcome {
            let now = now_ms();
            execution.lease_valid = Some(store.state().leases.get(&lease.lease_id).is_some_and(
                |current| {
                    current.status == langspan_core::LeaseStatus::Active
                        && current.generation == store.state().generation
                        && current.expires_at_ms > now
                },
            ));
        }
        response(id, execution)
    }
    fn handle(
        store: &mut Store,
        graph: Option<&crate::graph::Index>,
        state_dir: &Path,
        request: Request,
    ) -> Response {
        let reads_blind = reads_blind_in_scope(&request.command);
        let acting = langspan_core::acting_session(&request.command).map(str::to_owned);
        let mut response = answer(store, graph, state_dir, request);
        // Refused or retried, the request still shows the session's hook ran.
        if let Some(session_id) = acting {
            record_seen(store, session_id);
        }
        if !reads_blind {
            for coverage in coverages(&mut response.outcome) {
                coverage.blind_in_scope = 0;
            }
        }
        response
    }

    /// After a request a session made at work, note that its hook ran: at
    /// most once per `SESSION_SEEN_INTERVAL_MS`, so a burst writes one event.
    /// A failure is logged; the request's own answer stands (roadmap J12).
    fn record_seen(store: &mut Store, session_id: String) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = now_ms();
        if !langspan_core::session_seen_due(store.state(), &session_id, now) {
            return;
        }
        let id = format!(
            "seen-{}-{now}-{}",
            store.state().generation,
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let command = Command::SessionSeen { session_id };
        let failure = match store.execute(&id, &command, &command, now) {
            Ok(Execution {
                outcome: Outcome::Error { code, message },
                ..
            }) => Some(format!("{code}: {message}")),
            Ok(_) => None,
            Err(error) => Some(error.to_string()),
        };
        if let Some(detail) = failure {
            eprintln!(
                "{}",
                serde_json::json!({"level":"warn","event":"session_seen_failed","detail":detail})
            );
        }
    }

    /// Whether the client asked for `GraphCoverage::blind_in_scope`. Earlier
    /// builds read coverage with `deny_unknown_fields`; a request that does
    /// not ask is answered in the shape they know, the field absent.
    fn reads_blind_in_scope(command: &Command) -> bool {
        matches!(
            command,
            Command::GraphBlindSpots { .. }
                | Command::GraphPath { .. }
                | Command::GraphMap { .. }
                | Command::GraphSymbol {
                    blind_in_scope: true,
                    ..
                }
                | Command::GraphReferences {
                    blind_in_scope: true,
                    ..
                }
                | Command::GraphImpact {
                    blind_in_scope: true,
                    ..
                }
        )
    }

    /// Every index coverage an outcome carries.
    fn coverages(outcome: &mut Outcome) -> Vec<&mut langspan_core::GraphCoverage> {
        match outcome {
            Outcome::Graph { coverage, .. }
            | Outcome::GraphContext { coverage, .. }
            | Outcome::CallBreaks { coverage, .. }
            | Outcome::GraphPath { coverage, .. }
            | Outcome::GraphMap { coverage, .. }
            | Outcome::GraphBlindSpots { coverage, .. } => vec![coverage],
            Outcome::Dashboard { dashboard } => dashboard
                .workspaces
                .iter_mut()
                .filter_map(|workspace| workspace.coverage.as_mut())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn answer(
        store: &mut Store,
        graph: Option<&crate::graph::Index>,
        state_dir: &Path,
        request: Request,
    ) -> Response {
        let version = request.version;
        let granted = protocol::Capabilities::negotiate(version, request.capabilities.as_deref());
        let request_id = request.request_id;
        let command = request.command;
        if !protocol::answers(version) {
            return error(
                request_id,
                store.state().generation,
                "unsupported_version",
                unsupported_version(),
            );
        }
        if let Err(e) = validate_request_id(&request_id) {
            return error(request_id, store.state().generation, &e.code, e.message);
        }
        // Every client, not only the CLI: the command is stored as it is.
        if let Command::AutoCheckEnable {
            argv,
            on_interface_change,
            ..
        } = &command
            && let Err(message) = auto_check::refuse_secrets(argv).and_then(|()| {
                on_interface_change
                    .as_ref()
                    .map_or(Ok(()), |wider| auto_check::refuse_secrets(&wider.argv))
            })
        {
            return error(
                request_id,
                store.state().generation,
                "secret_in_command",
                message,
            );
        }
        if command == Command::Version {
            return Response {
                version: PROTOCOL_VERSION,
                request_id,
                replayed: false,
                generation: store.state().generation,
                lease_valid: None,
                outcome: Outcome::Version {
                    version: crate::binary_version::VERSION.into(),
                    pid: std::process::id(),
                },
            };
        }
        if let Command::CheckCalls { session_id, calls } = &command {
            return check_calls(store, graph, request_id, session_id, calls);
        }
        if let Command::OutlineRead { .. } = &command {
            return outline_read(store, graph, request_id, command);
        }
        if let Command::CheckWrite {
            session_id,
            paths,
            symbols,
        } = &command
        {
            return check_write(
                store,
                graph,
                state_dir,
                request_id,
                (session_id, paths, symbols),
            );
        }
        if matches!(
            command,
            Command::GraphSymbol { .. }
                | Command::GraphReferences { .. }
                | Command::GraphImpact { .. }
                | Command::GraphBlindSpots { .. }
                | Command::GraphContext { .. }
                | Command::GraphInflight { .. }
                | Command::GraphPath { .. }
                | Command::GraphMap { .. }
        ) {
            let answer = match &command {
                Command::GraphPath {
                    workspace_id,
                    from,
                    to,
                    max_hops,
                } => graph_path(graph, store.state(), (workspace_id, from, to, *max_hops))
                    .map_err(|message| ("index_unavailable", message)),
                Command::GraphMap {
                    workspace_id,
                    depth,
                } => graph_map(graph, store.state(), workspace_id, *depth)
                    .map_err(|message| ("index_unavailable", message)),
                Command::GraphInflight { workspace_id } => {
                    graph_inflight(graph, store.state(), workspace_id)
                        .map_err(|message| ("index_unavailable", message))
                }
                Command::GraphBlindSpots {
                    workspace_id,
                    limit,
                } => graph_blind_spots(graph, store.state(), workspace_id, *limit)
                    .map_err(|message| ("index_unavailable", message)),
                Command::GraphContext { .. } => graph_context(graph, store.state(), &command),
                _ => graph_query(graph, store.state(), &command)
                    .map_err(|message| ("index_unavailable", message)),
            };
            return match answer {
                Ok(outcome) => Response {
                    version: PROTOCOL_VERSION,
                    request_id,
                    replayed: false,
                    generation: store.state().generation,
                    lease_valid: None,
                    outcome,
                },
                Err((code, message)) => error(request_id, store.state().generation, code, message),
            };
        }
        // A user-level hook names a repository by an ID derived from its
        // root. If that root is already registered under another ID (a
        // project install, or a hook from before), answer with the existing
        // workspace so the hook adopts it. Explicit IDs still conflict.
        if let Command::WorkspaceRegister { workspace_id, root } = &command
            && workspace_id.starts_with("lsh1-root-")
            && let Ok(canonical) = fs::canonicalize(root)
            && let Some(existing) = store.state().workspaces.values().find(|existing| {
                existing.workspace_id != *workspace_id
                    && (Path::new(&existing.root) == canonical
                        || crate::observer::same_root(Path::new(&existing.root), &canonical))
            })
        {
            return Response {
                version: PROTOCOL_VERSION,
                request_id,
                replayed: false,
                generation: store.state().generation,
                lease_valid: None,
                outcome: Outcome::Workspace {
                    workspace: existing.clone(),
                },
            };
        }
        match store.replay(&request_id, &command, now_ms()) {
            Ok(Some(execution)) => return current_response(store, request_id, execution),
            Ok(None) => {}
            Err(e) => {
                return error(
                    request_id,
                    store.state().generation,
                    "storage_error",
                    e.to_string(),
                );
            }
        }
        let prepared = if let Command::WorkspaceMove {
            workspace_id,
            root,
            switch,
        } = &command
        {
            match prepare_move(store.state(), workspace_id, root, *switch) {
                Ok(command) => command,
                Err((code, message)) => {
                    return error(request_id, store.state().generation, code, message);
                }
            }
        } else if let Command::Brief { .. } = &command {
            prepare_brief(store.state(), graph, &command)
        } else if matches!(
            command,
            Command::Remember { .. } | Command::ObserveNoteAnchors { .. }
        ) {
            match prepare_anchors(store.state(), graph, &command) {
                Ok(command) => command,
                Err((code, message)) => {
                    return error(request_id, store.state().generation, code, message);
                }
            }
        } else {
            match prepare(store.state(), &command) {
                Ok(command) => command,
                Err(e) => {
                    return error(
                        request_id,
                        store.state().generation,
                        "invalid_path",
                        e.to_string(),
                    );
                }
            }
        };
        match store.execute(&request_id, &command, &prepared, now_ms()) {
            Ok(mut execution) => {
                // A record given in parts is cut from its JSON in the
                // client's shape, which the reply's own reshaping cannot
                // reach (review comment 4712860167).
                if granted != protocol::Capabilities::ALL
                    && let Command::StatusRecords { after, page_bytes } = &command
                    && matches!(execution.outcome, Outcome::StatusRecords { .. })
                {
                    execution.outcome =
                        store.status_page(after.as_deref(), *page_bytes, |record| {
                            protocol::encode_record_with(granted, record)
                                .map_err(|error| error.to_string())
                        });
                }
                if let Outcome::Dashboard { dashboard } = &mut execution.outcome {
                    complete_dashboard(dashboard, store.state(), graph);
                }
                // Only the daemon looks at the filesystem.
                if let Outcome::Workspaces { workspaces, .. } = &mut execution.outcome {
                    for workspace in workspaces {
                        workspace.root_exists = Path::new(&workspace.root).is_dir();
                    }
                }
                current_response(store, request_id, execution)
            }
            Err(e) => error(
                request_id,
                store.state().generation,
                "storage_error",
                e.to_string(),
            ),
        }
    }

    /// What only the daemon knows: how long it has run and how much of each
    /// workspace the source index covers. A workspace whose coverage cannot
    /// be read is left without it, and the page says so.
    fn complete_dashboard(
        dashboard: &mut langspan_core::Dashboard,
        state: &State,
        graph: Option<&crate::graph::Index>,
    ) {
        dashboard.uptime_ms = STARTED_MS
            .get()
            .map(|started| now_ms().saturating_sub(*started));
        if let Some(graph) = graph {
            for workspace in &mut dashboard.workspaces {
                workspace.coverage = Registration::of(state, &workspace.workspace_id)
                    .and_then(|of| graph.with_coverage(&of, |_| Ok(())).ok())
                    .map(|(_, coverage)| coverage);
            }
        }
    }

    async fn serve_connection(stream: UnixStream, sender: mpsc::SyncSender<Job>, config: Config) {
        let (reader, mut writer) = stream.into_split();
        let read = async {
            let mut bytes = Vec::new();
            BufReader::new(reader.take((MAX_FRAME_BYTES + 1) as u64))
                .read_until(b'\n', &mut bytes)
                .await?;
            if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected newline-terminated JSON <=64 KiB",
                ));
            }
            Ok(bytes)
        };
        // Answered in the version the client asked in, and in that
        // version's shape; a request that does not parse, in the version it
        // names when the daemon answers that one.
        // So are the capabilities it named, of a request that does not
        // parse too, so that its client can ask again with those granted
        // (ADR 0037).
        let mut asked = PROTOCOL_VERSION;
        let mut named: Option<Vec<String>> = None;
        let mut reply = match timeout(Duration::from_millis(config.io_timeout_ms), read).await {
            Ok(Ok(bytes)) => match serde_json::from_slice::<Request>(&bytes) {
                Ok(mut request) => {
                    asked = request.version;
                    named.clone_from(&request.capabilities);
                    // Clients redact before sending; a client that did not
                    // still cannot have a secret recorded.
                    crate::hook::redact_command(&mut request.command);
                    let id = request.request_id.clone();
                    let transfer = is_transfer(&request.command);
                    let (tx, rx) = oneshot::channel();
                    match sender.try_send(Job {
                        request,
                        reply: tx,
                        of: None,
                    }) {
                        Ok(()) => {
                            let wait = if transfer {
                                TRANSFER_WAIT
                            } else {
                                Duration::from_millis(config.request_timeout_ms)
                            };
                            match timeout(wait, rx).await {
                                Ok(Ok(reply)) => reply,
                                _ => error(
                                    id,
                                    0,
                                    "coordination_unavailable",
                                    "request outcome is unknown; retry the identical request ID",
                                ),
                            }
                        }
                        Err(_) => error(
                            id,
                            0,
                            "coordination_unavailable",
                            "coordinator queue unavailable; retry the identical request ID",
                        ),
                    }
                }
                Err(_) => {
                    let request = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
                    let field = |key| request.as_ref().and_then(|request| request.get(key));
                    if let Some(version) = field("version")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|version| u16::try_from(version).ok())
                    {
                        asked = version;
                    }
                    named = field("capabilities")
                        .and_then(|names| serde_json::from_value(names.clone()).ok());
                    error(
                        String::new(),
                        0,
                        "invalid_request",
                        "expected a typed version 1 request; unknown fields are rejected",
                    )
                }
            },
            Ok(Err(e)) => error(String::new(), 0, "invalid_frame", e.to_string()),
            Err(_) => error(
                String::new(),
                0,
                "read_timeout",
                "JSON request was not received before the I/O deadline",
            ),
        };
        if !protocol::answers(asked) {
            asked = PROTOCOL_VERSION;
        }
        reply.version = asked;
        let granted = protocol::Capabilities::negotiate(asked, named.as_deref());
        let bytes = protocol::encode_with(granted, named.as_deref(), &reply).or_else(|_| {
            let mut too_large = error(
                reply.request_id,
                reply.generation,
                "response_too_large",
                "response exceeds 64 KiB; reduce the event page size, or page the state with `status --all`",
            );
            too_large.version = asked;
            protocol::encode(&too_large)
        });
        if let Ok(bytes) = bytes {
            let _ = timeout(
                Duration::from_millis(config.io_timeout_ms),
                writer.write_all(&bytes),
            )
            .await;
        }
    }

    /// The other workspaces whose roots overlap `canonical` as distinct
    /// linked worktrees of one repository. Any other overlap is refused.
    /// A registered root is kept as given, so it is compared both as given
    /// and resolved: a root that is now a link to `canonical` overlaps it.
    fn verified_overlaps(
        state: &State,
        workspace_id: &str,
        canonical: &Path,
        deadline: Instant,
    ) -> Result<Vec<String>, String> {
        let mut verified = Vec::new();
        for old in state.workspaces.values() {
            let given = Path::new(&old.root);
            let resolved = fs::canonicalize(given).ok();
            let overlaps =
                |other: &Path| canonical.starts_with(other) || other.starts_with(canonical);
            if old.workspace_id != workspace_id
                && (overlaps(given) || resolved.as_deref().is_some_and(overlaps))
            {
                let other = resolved.as_deref().unwrap_or(given);
                let linked = crate::git::identity(canonical, deadline)
                    .and_then(|a| crate::git::identity(other, deadline).map(|b| (a, b)))
                    .is_ok_and(|(a, b)| {
                        a.root == canonical
                            && b.root == other
                            && a.root != b.root
                            && a.common_dir == b.common_dir
                    });
                if !linked {
                    return Err(
                        "workspace root overlaps an existing physical workspace; reuse its ID"
                            .into(),
                    );
                }
                verified.push(old.workspace_id.clone());
            }
        }
        Ok(verified)
    }

    /// The checks `workspace move` makes before the core decides (roadmap
    /// G6), each refusing with what failed and changing nothing: NEW_ROOT is
    /// the top of a Git checkout; without `switch` the old root is gone;
    /// with it the old root is still there and its `HEAD` commit is in
    /// NEW_ROOT's object database, so both are checkouts of one repository.
    /// Read-only Git, bounded by one deadline.
    fn prepare_move(
        state: &State,
        workspace_id: &str,
        root: &str,
        switch: bool,
    ) -> Result<Command, (&'static str, String)> {
        let Some(registered) = state.workspaces.get(workspace_id) else {
            // The core refuses an unknown ID in its own words.
            return Ok(Command::WorkspaceMoveVerified {
                workspace_id: workspace_id.into(),
                root: root.into(),
                verified_overlaps: Vec::new(),
            });
        };
        let invalid = |message: String| ("invalid_root", message);
        if root.len() > 4096 {
            return Err(invalid("NEW_ROOT exceeds 4096 bytes".into()));
        }
        let canonical = fs::canonicalize(root)
            .map_err(|e| invalid(format!("cannot resolve NEW_ROOT {root}: {e}")))?;
        if !canonical.is_dir() {
            return Err(invalid(format!(
                "NEW_ROOT {} is not a directory",
                canonical.display()
            )));
        }
        let text = canonical
            .to_str()
            .ok_or_else(|| invalid("NEW_ROOT must be UTF-8".into()))?
            .to_owned();
        let deadline = Instant::now() + Duration::from_secs(1);
        let not_top = |detail: String| {
            (
                "not_checkout_top",
                format!("{detail}; give the top directory of the checkout. Nothing changed"),
            )
        };
        let identity = crate::git::identity(&canonical, deadline).map_err(|_| {
            not_top(format!(
                "NEW_ROOT {} is not inside a Git checkout",
                canonical.display()
            ))
        })?;
        if identity.root != canonical {
            return Err(not_top(format!(
                "NEW_ROOT {} is inside the Git checkout at {}, not its top",
                canonical.display(),
                identity.root.display()
            )));
        }
        let old = Path::new(&registered.root);
        let old_exists = match fs::symlink_metadata(old) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(invalid(format!(
                    "cannot tell whether the old root {} still exists: {error}",
                    old.display()
                )));
            }
        };
        // The old root still resolves to NEW_ROOT, as when a link was left
        // where the checkout used to be: the same checkout, moved.
        // With or without --switch, there is nothing else to tell apart.
        let same = old_exists && crate::observer::same_root(old, &canonical);
        match (switch, old_exists) {
            _ if same => {}
            (false, true) => {
                // A link at the old root, or on the way to it, names the
                // directory it now leads to.
                let link = match fs::canonicalize(old) {
                    Ok(target) if target != old => format!(
                        ": it is reached through a symbolic link and resolves to {}, not to NEW_ROOT",
                        target.display()
                    ),
                    Err(_) if old.is_symlink() => {
                        ": it is a symbolic link whose target cannot be resolved".into()
                    }
                    _ => String::new(),
                };
                return Err((
                    "old_root_exists",
                    format!(
                        "The old root {} still exists{link}. To coordinate the checkout at NEW_ROOT under {workspace_id} instead, add --switch; to coordinate both, register NEW_ROOT with `langspan workspace add`. Nothing changed",
                        old.display()
                    ),
                ));
            }
            (true, false) => {
                return Err((
                    "old_root_missing",
                    format!(
                        "The old root {} no longer exists; for a checkout that was moved, run without --switch. Nothing changed",
                        old.display()
                    ),
                ));
            }
            // User-level hooks derive this ID from the old root, so every
            // new session there would claim it again and be refused.
            (true, true) if workspace_id == crate::hook::user_workspace_id(old) => {
                return Err((
                    "root_derived_id",
                    format!(
                        "Workspace {workspace_id} has the ID user-level hooks derive from {}, so new sessions there would still claim it. To coordinate NEW_ROOT under this ID, move or delete that checkout first, then run this again without --switch; to coordinate both, register NEW_ROOT with `langspan workspace add`. Nothing changed",
                        old.display()
                    ),
                ));
            }
            (true, true) => same_repository(old, &canonical, deadline)?,
            (false, false) => {}
        }
        let verified_overlaps = verified_overlaps(state, workspace_id, &canonical, deadline)
            .map_err(|message| ("workspace_conflict", message))?;
        Ok(Command::WorkspaceMoveVerified {
            workspace_id: workspace_id.into(),
            root: text,
            verified_overlaps,
        })
    }

    /// `old`'s `HEAD` commit is in `new`'s object database.
    fn same_repository(
        old: &Path,
        new: &Path,
        deadline: Instant,
    ) -> Result<(), (&'static str, String)> {
        let refused =
            |detail: String| ("different_repository", format!("{detail}. Nothing changed"));
        // As the observer runs Git: no fsmonitor program, no replace refs.
        const SAFE: [&str; 3] = ["--no-replace-objects", "-c", "core.fsmonitor=false"];
        let head = crate::git::output(
            old,
            &[
                &SAFE[..],
                &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            ]
            .concat(),
            deadline,
            256,
        )
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| text.trim().to_owned())
        .filter(|oid| matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| {
            refused(format!(
                "The old root {} has no commit to compare with NEW_ROOT; --switch needs one",
                old.display()
            ))
        })?;
        let found = crate::observer::holds_commit(new, &head, deadline)
            .map_err(|error| refused(format!("cannot read NEW_ROOT's objects: {error}")))?;
        if found {
            Ok(())
        } else {
            Err(refused(format!(
                "NEW_ROOT {} does not hold the old root's HEAD commit {head}, so it is not a checkout of the same repository",
                new.display()
            )))
        }
    }

    fn prepare(state: &State, command: &Command) -> Result<Command, crate::Error> {
        match command {
            Command::WorkspaceRegister { workspace_id, root } => {
                if root.len() > 4096 {
                    return Err("workspace root exceeds 4096 bytes".into());
                }
                let canonical = fs::canonicalize(root)
                    .map_err(|e| format!("cannot resolve workspace root: {e}"))?;
                if !canonical.is_dir() {
                    return Err("workspace root must be a directory".into());
                }
                // The registered root reached another way, such as through a
                // link left where the checkout used to be: the same
                // registration, kept as given.
                if let Some(existing) = state.workspaces.get(workspace_id)
                    && crate::observer::same_root(Path::new(&existing.root), &canonical)
                {
                    return Ok(Command::WorkspaceRegisterVerified {
                        workspace_id: workspace_id.clone(),
                        root: existing.root.clone(),
                        verified_overlaps: existing.verified_overlaps.clone(),
                    });
                }
                let deadline = Instant::now() + Duration::from_millis(250);
                let verified_overlaps =
                    verified_overlaps(state, workspace_id, &canonical, deadline)?;
                Ok(Command::WorkspaceRegisterVerified {
                    workspace_id: workspace_id.clone(),
                    root: canonical
                        .to_str()
                        .ok_or("workspace root must be UTF-8")?
                        .into(),
                    verified_overlaps,
                })
            }
            Command::IntentSet {
                session_id,
                paths,
                summary,
            } => Ok(Command::IntentSet {
                session_id: session_id.clone(),
                paths: prepare_paths(state, session_id, paths)?,
                summary: summary.clone(),
            }),
            Command::Admit {
                session_id,
                paths,
                ttl_ms,
            } => Ok(Command::Admit {
                session_id: session_id.clone(),
                paths: prepare_paths(state, session_id, paths)?,
                ttl_ms: *ttl_ms,
            }),
            Command::AdmitTool {
                session_id,
                paths,
                ttl_ms,
                operation,
                base,
                regions,
                overwrites,
            } => Ok(Command::AdmitTool {
                session_id: session_id.clone(),
                paths: prepare_paths(state, session_id, paths)?,
                ttl_ms: *ttl_ms,
                operation: operation.clone(),
                base: prepare_files(state, session_id, base)?,
                regions: prepare_regions(state, session_id, regions)?,
                overwrites: prepare_overwrites(state, session_id, overwrites)?,
            }),
            Command::Holders {
                session_id,
                paths,
                base,
                regions,
            } => Ok(Command::Holders {
                session_id: session_id.clone(),
                paths: prepare_paths(state, session_id, paths)?,
                base: prepare_files(state, session_id, base)?,
                regions: prepare_regions(state, session_id, regions)?,
            }),
            Command::RecordWait {
                session_id,
                paths,
                holder_session_id,
                waited_ms,
                result,
                changed,
                input_digest,
            } => Ok(Command::RecordWait {
                session_id: session_id.clone(),
                paths: prepare_paths(state, session_id, paths)?,
                holder_session_id: holder_session_id.clone(),
                waited_ms: *waited_ms,
                result: *result,
                changed: prepare_files(state, session_id, changed)?,
                input_digest: input_digest.clone(),
            }),
            Command::ObserveSource {
                session_id,
                files,
                origin,
            } => Ok(Command::ObserveSource {
                session_id: session_id.clone(),
                files: prepare_files(state, session_id, files)?,
                origin: *origin,
            }),
            Command::Observe {
                session_id,
                operation_id,
                source,
                tool,
                paths,
                result,
                lease_id,
            } => Ok(Command::Observe {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                source: source.clone(),
                tool: tool.clone(),
                paths: prepare_paths(state, session_id, paths)?,
                result: *result,
                lease_id: lease_id.clone(),
            }),
            Command::ObserveTool {
                session_id,
                operation,
                paths,
                result,
                lease_id,
            } => Ok(Command::ObserveTool {
                session_id: session_id.clone(),
                operation: operation.clone(),
                paths: prepare_paths(state, session_id, paths)?,
                result: *result,
                lease_id: lease_id.clone(),
            }),
            Command::ObserveSessionLiveness {
                session_id,
                process,
                ..
            } => Ok(Command::ObserveSessionLiveness {
                session_id: session_id.clone(),
                process: process.clone(),
                // The same incarnation seen alive within the poll's
                // coalescing window is not probed again on the writer: on
                // macOS each probe starts a process listing.
                status: match state.session_liveness.get(session_id) {
                    Some(prior)
                        if prior.process == *process
                            && prior.generation == state.generation
                            && prior.status == langspan_core::LivenessStatus::Alive
                            && now_ms().saturating_sub(prior.observed_at_ms)
                                < LIVENESS_COALESCE_MS =>
                    {
                        langspan_core::LivenessStatus::Alive
                    }
                    _ => crate::process::probe(process),
                },
            }),
            Command::StatusLine { root, from_ms: ms } => {
                Ok(crate::status_line::resolved(root, *ms))
            }
            _ => Ok(command.clone()),
        }
    }
    /// Each observed version is keyed by the same canonical file key an
    /// admission of that path would use.
    fn prepare_files(
        state: &State,
        session_id: &str,
        files: &[langspan_core::ObservedFile],
    ) -> Result<Vec<langspan_core::ObservedFile>, crate::Error> {
        if files.len() > 64 {
            return Err("supply at most 64 observed file versions".into());
        }
        files
            .iter()
            .map(|file| {
                let key = prepare_paths(state, session_id, std::slice::from_ref(&file.path))?
                    .pop()
                    .ok_or("observed file has no key")?;
                Ok(langspan_core::ObservedFile {
                    path: key,
                    state: file.state.clone(),
                })
            })
            .collect()
    }

    /// Regions name their path as the hook did; map it through the same
    /// canonical file-key checks, so it matches the admitted key.
    fn prepare_regions(
        state: &State,
        session_id: &str,
        regions: &[langspan_core::PathRegions],
    ) -> Result<Vec<langspan_core::PathRegions>, crate::Error> {
        if regions.len() > 64 {
            return Err("supply regions for at most 64 files".into());
        }
        regions
            .iter()
            .map(|located| {
                let key = prepare_paths(state, session_id, std::slice::from_ref(&located.path))?
                    .pop()
                    .ok_or("located file has no key")?;
                Ok(langspan_core::PathRegions {
                    path: key,
                    ..located.clone()
                })
            })
            .collect()
    }

    /// Written lines name their path as the hook did, like regions.
    fn prepare_overwrites(
        state: &State,
        session_id: &str,
        overwrites: &[langspan_core::WrittenLines],
    ) -> Result<Vec<langspan_core::WrittenLines>, crate::Error> {
        if overwrites.len() > langspan_core::MAX_WRITTEN {
            return Err("supply at most 64 written-lines entries".into());
        }
        overwrites
            .iter()
            .map(|entry| {
                let key = prepare_paths(state, session_id, std::slice::from_ref(&entry.path))?
                    .pop()
                    .ok_or("written file has no key")?;
                Ok(langspan_core::WrittenLines {
                    path: key,
                    ..entry.clone()
                })
            })
            .collect()
    }

    fn prepare_paths(
        state: &State,
        session_id: &str,
        paths: &[String],
    ) -> Result<Vec<String>, crate::Error> {
        let paths: std::collections::BTreeSet<_> = paths.iter().collect();
        if paths.is_empty() || paths.len() > 64 {
            return Err("supply 1..64 relative file paths".into());
        }
        let Some(session) = state.sessions.get(session_id) else {
            return Ok(paths.into_iter().cloned().collect());
        };
        let Some(workspace) = state.workspaces.get(&session.workspace_id) else {
            return Err("session workspace is missing".into());
        };
        let root = Path::new(&workspace.root);
        // A root reached through a link is the checkout the link leads to,
        // unless the scan found that to be another repository.
        let current_root = fs::canonicalize(root)?;
        if current_root != root
            && state
                .scan_failures
                .get(&session.workspace_id)
                .is_some_and(|failure| failure.detail == crate::store::IDENTITY_CHANGED)
        {
            return Err("workspace root identity changed; use a new workspace registration".into());
        }
        // Other registered roots, resolved like this one, so a root behind
        // a link still bounds its own files. A root that no longer resolves
        // keeps its stored form.
        let others: Vec<std::path::PathBuf> = state
            .workspaces
            .iter()
            .filter(|(id, _)| **id != session.workspace_id)
            .map(|(_, workspace)| {
                let stored = Path::new(&workspace.root);
                fs::canonicalize(stored).unwrap_or_else(|_| stored.to_path_buf())
            })
            .collect();
        // Another workspace ID that now resolves to this very root, as when
        // a registered clone's root was replaced by a link to another
        // registered clone: both would admit the same physical files while
        // the core keeps their leases apart, so neither admits any.
        if others.contains(&current_root) {
            return Err("workspace root now resolves to another registered workspace's root; move or remove one of them".into());
        }
        let mut keys = paths
            .iter()
            .map(|path| -> Result<String, crate::Error> {
                // Resolved paths are compared with the resolved root.
                let key = file_key(&current_root, path)?;
                let full = current_root.join(&key);
                if others.iter().any(|descendant| {
                    *descendant != current_root
                        && descendant.starts_with(&current_root)
                        && full.starts_with(descendant)
                }) {
                    return Err("file belongs to a registered descendant workspace; attach that physical root separately".into());
                }
                Ok(key)
            })
            .collect::<Result<Vec<_>, _>>()?;
        keys.sort();
        keys.dedup();
        Ok(keys)
    }
    fn file_key(root: &Path, key: &str) -> Result<String, crate::Error> {
        if key.is_empty()
            || key.len() > 512
            || key.contains(['\\', '\0'])
            || key
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(
                "file keys must be normalized relative paths of 1..512 bytes without traversal"
                    .into(),
            );
        }
        let full = root.join(key);
        let mut ancestor = full.clone();
        let mut suffix = Vec::new();
        let metadata = loop {
            match fs::symlink_metadata(&ancestor) {
                Ok(metadata) => break metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    suffix.push(
                        ancestor
                            .file_name()
                            .ok_or("cannot resolve file parent")?
                            .to_owned(),
                    );
                    if !ancestor.pop() {
                        return Err("cannot resolve file parent".into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
        };
        let mut resolved = fs::canonicalize(&ancestor)?;
        if !resolved.starts_with(root) {
            return Err("resolved file path leaves the registered workspace".into());
        }
        let target = fs::metadata(&resolved)?;
        if suffix.is_empty() {
            if !target.is_file() || target.nlink() > 1 {
                return Err("admission supports regular files with a single hard link".into());
            }
        } else if !target.is_dir() || (!metadata.is_dir() && !metadata.file_type().is_symlink()) {
            return Err("new file parent must be a directory".into());
        }
        for part in suffix.iter().rev() {
            resolved.push(part);
        }
        if resolved.parent().is_some_and(|parent| {
            parent
                .ancestors()
                .take_while(|directory| *directory != root)
                .any(|directory| directory.join(".git").try_exists().unwrap_or(true))
        }) {
            return Err(
                "file belongs to a nested Git workspace; attach that physical root separately"
                    .into(),
            );
        }
        let relative = resolved
            .strip_prefix(root)?
            .to_str()
            .ok_or("file path must be UTF-8")?
            .to_string();
        if relative.len() > 512 {
            return Err("resolved file key exceeds 512 bytes".into());
        }
        Ok(relative)
    }

    #[cfg(test)]
    mod frame_tests {
        use super::*;

        #[cfg(debug_assertions)]
        #[test]
        fn the_debug_idle_limit_defaults_to_five_minutes_and_zero_turns_it_off() {
            assert_eq!(idle_limit_of(None), Some(DEBUG_IDLE_LIMIT));
            assert_eq!(idle_limit_of(Some("soon")), Some(DEBUG_IDLE_LIMIT));
            assert_eq!(idle_limit_of(Some("0")), None);
            assert_eq!(idle_limit_of(Some("5")), Some(Duration::from_millis(100)));
            assert_eq!(
                idle_limit_of(Some(" 2500 ")),
                Some(Duration::from_millis(2500))
            );
        }

        #[cfg(not(debug_assertions))]
        #[test]
        fn a_release_build_has_no_idle_limit() {
            assert_eq!(idle_limit(), None);
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    mod tests {
        use super::*;
        #[test]
        fn a_registered_child_boundary_survives_removal_of_its_git_marker() {
            let directory = std::env::temp_dir()
                .join(format!("langspan-worktree-boundary-{}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            let root = fs::canonicalize(&directory).unwrap();
            let child = root.join("nested");
            fs::create_dir(&child).unwrap();
            fs::write(child.join("a.rs"), "fixture").unwrap();
            let mut state = State::default();
            for (id, path, links) in [
                ("parent", &root, vec![]),
                ("child", &child, vec!["parent".into()]),
            ] {
                state.workspaces.insert(
                    id.into(),
                    langspan_core::Workspace {
                        workspace_id: id.into(),
                        root: path.to_str().unwrap().into(),
                        verified_overlaps: links,
                    },
                );
                state.sessions.insert(
                    id.into(),
                    langspan_core::Session {
                        session_id: id.into(),
                        workspace_id: id.into(),
                        generation: 0,
                        parent: None,
                        headless: false,
                    },
                );
            }
            let parent = prepare_paths(&state, "parent", &["nested/a.rs".into()]);
            let own_child = prepare_paths(&state, "child", &["a.rs".into()]);
            fs::remove_dir_all(directory).unwrap();
            assert!(
                parent
                    .unwrap_err()
                    .to_string()
                    .contains("registered descendant")
            );
            assert_eq!(own_child.unwrap(), ["a.rs"]);
        }
        #[test]
        fn process_exit_is_recorded_without_releasing_admission_authority() {
            let directory = std::env::temp_dir()
                .join(format!("langspan-liveness-owner-{}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            let mut store = Store::open(&directory.join("store.sqlite")).unwrap();
            let mut commands = vec![
                Command::WorkspaceRegister {
                    workspace_id: "work".into(),
                    root: "/work".into(),
                },
                Command::SessionRegister {
                    session_id: "session".into(),
                    workspace_id: "work".into(),
                    parent: None,
                    headless: false,
                },
                Command::Admit {
                    session_id: "session".into(),
                    paths: vec!["a.rs".into()],
                    ttl_ms: 1000,
                },
            ];
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            let identity = crate::process::identify(child.id()).unwrap();
            commands.push(Command::ObserveSessionLiveness {
                session_id: "session".into(),
                process: identity,
                status: langspan_core::LivenessStatus::Alive,
            });
            for (index, command) in commands.iter().enumerate() {
                let execution = store
                    .execute(&format!("fixture-{index}"), command, command, 1)
                    .unwrap();
                assert!(!matches!(execution.outcome, Outcome::Error { .. }));
            }
            child.kill().unwrap();
            child.wait().unwrap();
            observe_processes(&mut store, &mut None, 2);
            let actual = store.state().session_liveness["session"].status;
            let lease = store.state().leases["fixture-2"].status.clone();
            drop(store);
            fs::remove_dir_all(directory).unwrap();
            assert_eq!(actual, langspan_core::LivenessStatus::Exited);
            assert_eq!(
                lease,
                langspan_core::LeaseStatus::Active,
                "liveness evidence must not revoke an admission"
            );
        }
    }

    #[cfg(test)]
    mod registration_tests {
        use super::*;
        /// The state owner's check for every worker report: a report about a
        /// registration is handled only while the workspace still has it.
        #[test]
        fn a_report_about_a_registration_the_workspace_left_is_refused() {
            let directory =
                std::env::temp_dir().join(format!("langspan-registration-{}", std::process::id()));
            let _ = fs::remove_dir_all(&directory);
            fs::create_dir(&directory).unwrap();
            let mut store = Store::open(&directory.join("store.sqlite")).unwrap();
            let mut step = 0;
            let mut run = |store: &mut Store, command: Command, prepared: Command| {
                step += 1;
                let execution = store
                    .execute(&format!("fixture-{step}"), &command, &prepared, 1)
                    .unwrap();
                assert!(
                    !matches!(execution.outcome, Outcome::Error { .. }),
                    "{:?}",
                    execution.outcome
                );
            };
            let register = |root: &str| Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: root.into(),
            };
            let report = |of: &Registration| {
                let (reply, _) = oneshot::channel();
                Job {
                    request: Request::new("report", Command::Status),
                    reply,
                    of: Some(of.clone()),
                }
            };
            let refused = |store: &Store, of: &Registration| {
                stale_report(store.state(), &report(of)).map(|response| response.outcome)
            };
            run(&mut store, register("/work"), register("/work"));
            let first = Registration::of(store.state(), "work").unwrap();
            assert!(refused(&store, &first).is_none());
            // Switched to another checkout.
            run(
                &mut store,
                Command::WorkspaceMove {
                    workspace_id: "work".into(),
                    root: "/other".into(),
                    switch: true,
                },
                Command::WorkspaceMoveVerified {
                    workspace_id: "work".into(),
                    root: "/other".into(),
                    verified_overlaps: Vec::new(),
                },
            );
            assert!(matches!(
                refused(&store, &first),
                Some(Outcome::Error { code, .. }) if code == "workspace_changed"
            ));
            let switched = Registration::of(store.state(), "work").unwrap();
            assert!(refused(&store, &switched).is_none());
            // Switched back to the first checkout: a report from before
            // both switches is of an earlier registration all the same.
            run(
                &mut store,
                Command::WorkspaceMove {
                    workspace_id: "work".into(),
                    root: "/work".into(),
                    switch: true,
                },
                Command::WorkspaceMoveVerified {
                    workspace_id: "work".into(),
                    root: "/work".into(),
                    verified_overlaps: Vec::new(),
                },
            );
            let back = Registration::of(store.state(), "work").unwrap();
            assert_eq!(back.root, first.root);
            assert_ne!(back, first);
            assert!(matches!(
                refused(&store, &first),
                Some(Outcome::Error { code, .. }) if code == "workspace_changed"
            ));
            assert!(refused(&store, &switched).is_some());
            run(
                &mut store,
                Command::WorkspaceMove {
                    workspace_id: "work".into(),
                    root: "/other".into(),
                    switch: true,
                },
                Command::WorkspaceMoveVerified {
                    workspace_id: "work".into(),
                    root: "/other".into(),
                    verified_overlaps: Vec::new(),
                },
            );
            let moved = Registration::of(store.state(), "work").unwrap();
            assert!(refused(&store, &moved).is_none());
            // Removed: no registration is current.
            let remove = Command::WorkspaceRemove {
                workspace_id: "work".into(),
                apply: true,
            };
            run(&mut store, remove.clone(), remove);
            assert!(refused(&store, &moved).is_some());
            // Registered again at the same root: a new lifetime.
            run(&mut store, register("/other"), register("/other"));
            let again = Registration::of(store.state(), "work").unwrap();
            assert_eq!(again.root, moved.root);
            assert_ne!(again, moved);
            assert!(refused(&store, &moved).is_some());
            assert!(refused(&store, &again).is_none());
            drop(store);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[cfg(test)]
    mod capability_tests {
        use super::*;
        use std::io::Write;

        /// ADR 0037, against this daemon on a real socket: it grants the
        /// names it knows and says which only when it granted fewer than
        /// were named, including to a request it could not read.
        #[test]
        fn the_daemon_names_what_it_granted_only_when_fewer_than_asked() {
            let directory = std::env::temp_dir().join(format!("ls-caps-{}", std::process::id()));
            let _ = fs::remove_dir_all(&directory);
            let state_dir = directory.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                let _ = runtime.block_on(run(state_dir));
            });
            let socket = directory.join("daemon.sock");
            let started = Instant::now();
            while !socket.exists() {
                assert!(started.elapsed() < Duration::from_secs(20), "no socket");
                std::thread::sleep(Duration::from_millis(20));
            }
            let ask = |request: serde_json::Value| -> serde_json::Value {
                let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                writeln!(stream, "{request}").unwrap();
                serde_json::from_slice(&protocol::read_frame(stream).unwrap()).unwrap()
            };
            let all = protocol::Capabilities::ALL.names();
            let with = |command: serde_json::Value, names: &[String]| {
                serde_json::json!({"version": PROTOCOL_VERSION, "request_id": "caps",
                    "command": command, "capabilities": names})
            };
            let version = serde_json::json!({"op": "version"});

            let reply = ask(with(version.clone(), &all));
            assert!(reply.get("capabilities").is_none(), "{reply}");
            let reply = ask(serde_json::json!({"version": PROTOCOL_VERSION,
                "request_id": "caps", "command": version}));
            assert!(reply.get("capabilities").is_none(), "{reply}");

            let mut later = all.clone();
            later.push("from_a_later_build".into());
            let reply = ask(with(version, &later));
            assert_eq!(reply["capabilities"], serde_json::json!(all), "{reply}");
            assert_eq!(reply["outcome"]["type"], "version", "{reply}");

            // A field of a capability this daemon lacks fails the request.
            let unread = serde_json::json!({"op": "version", "from_a_later_build": true});
            let fewer = &all[1..];
            let reply = ask(with(unread, fewer));
            assert_eq!(reply["outcome"]["code"], "invalid_request", "{reply}");
            assert!(reply.get("capabilities").is_none(), "{reply}");
            let reply = ask(serde_json::json!({"version": PROTOCOL_VERSION,
                "request_id": "caps", "command": {"op": "version", "x": 1},
                "capabilities": later}));
            assert_eq!(reply["outcome"]["code"], "invalid_request", "{reply}");
            assert_eq!(reply["capabilities"], serde_json::json!(all), "{reply}");
            // Without the request ID, as the client's fallback tests
            // answer (review comment 4715859499).
            assert_eq!(reply["request_id"], "", "{reply}");
            fs::remove_dir_all(&directory).unwrap();
        }
    }
}

#[cfg(unix)]
pub use unix::run;
