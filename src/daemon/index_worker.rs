//! The source index's single writer, and the exports and imports the state
//! owner hands it.

use super::Registration;
use super::unix::{Job, ask_as, error, now_ms, unsupported_version};
use crate::protocol::{self, PROTOCOL_VERSION, Response};
use langspan_core::{Command, Outcome, State, validate_request_id};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

pub(super) struct IndexJob {
    /// The registration the scan was of. A root or registration revision
    /// other than the one last indexed starts the index again.
    pub(super) of: Registration,
    /// Its unobserved paths are kept in the index as they were, never
    /// treated as removed.
    pub(super) scan: crate::graph::ScanView,
}

/// An export or import of the source index (roadmap D5), handed by
/// the state owner to the indexer, which writes the index alone and
/// replies itself.
pub(super) struct Transfer {
    of: Registration,
    request_id: String,
    generation: u64,
    command: Command,
    reply: oneshot::Sender<Response>,
}

/// How long a connection waits for an export or import, and how long
/// the indexer may spend on one. An import of the largest file reads up
/// to 256 MiB and every listed source file.
pub(super) const TRANSFER_WAIT: Duration = Duration::from_secs(120);
const TRANSFER_BUDGET: Duration = Duration::from_secs(100);

pub(super) fn is_transfer(command: &Command) -> bool {
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
pub(super) fn forward_transfer(state: &State, transfers: &mpsc::SyncSender<Transfer>, job: Job) {
    let version = job.request.version;
    let request_id = job.request.request_id;
    let command = job.request.command;
    let refuse =
        |code: &str, message: String| error(request_id.clone(), state.generation, code, message);
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
            format!("workspace {workspace_id} is not registered; run `langspan workspace list`"),
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

/// The indexer's index, opened with its schema complete. The daemon calls
/// this before its owner thread answers a request, so no request's reader
/// finds the file without its tables or locked while they are created.
///
/// For tests only: `LANGSPAN_TEST_INDEX_OPEN_DELAY_MS`, at most 5 s,
/// waits that long first, so a test can show that nothing is answered
/// before the schema exists. Not a setting: it is undocumented for users.
pub(super) fn open_index(path: &Path) -> Result<crate::graph::Index, OpenFailure> {
    let delay = std::env::var("LANGSPAN_TEST_INDEX_OPEN_DELAY_MS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .map_or(Duration::ZERO, |ms| Duration::from_millis(ms.min(5000)));
    std::thread::sleep(delay);
    crate::graph::Index::open(path)
}

/// Why the index could not be opened, as the index reports it.
pub(super) type OpenFailure = Box<dyn std::error::Error + Send + Sync>;

/// The index's single writer. It only ever reads working trees and its
/// own file; what it learns reaches coordination state as a request to
/// the state owner, like any other client's.
pub(super) fn index_worker(
    jobs: mpsc::Receiver<IndexJob>,
    transfers: mpsc::Receiver<Transfer>,
    sender: mpsc::SyncSender<Job>,
    index: Result<crate::graph::Index, OpenFailure>,
) {
    super::remote_worker::panic_in_thread_for_test();
    let mut index = match index {
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
                        crate::ids::wire_id(
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
                    crate::ids::wire_id(
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

/// `forward_transfer`'s refusals, in process: a channel stands for the
/// indexer, so no daemon, socket or index file is involved.
#[cfg(test)]
mod tests {
    use super::*;

    fn registered() -> State {
        let mut state = State::default();
        let register = Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/work".into(),
        };
        let decision = langspan_core::decide(&state, &register, "register", 1);
        for event in &decision.events {
            langspan_core::apply(&mut state, event).unwrap();
        }
        state
    }

    fn export(workspace_id: &str, path: &str) -> Command {
        Command::GraphExport {
            workspace_id: workspace_id.into(),
            path: path.into(),
        }
    }

    /// The reply `forward_transfer` gives a job carrying `command` at
    /// `version`, or None when it handed the job on without one.
    fn answer(
        transfers: &mpsc::SyncSender<Transfer>,
        command: Command,
        version: u16,
    ) -> Option<Outcome> {
        let (jobs, received) = mpsc::sync_channel(1);
        let asker = std::thread::spawn(move || ask_as(&jobs, None, "transfer-1".into(), command));
        let mut job = received.recv().unwrap();
        job.request.version = version;
        forward_transfer(&registered(), transfers, job);
        asker.join().unwrap()
    }

    fn refusal(outcome: Option<Outcome>) -> (String, String) {
        match outcome {
            Some(Outcome::Error { code, message }) => (code, message),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_busy_indexer_refuses_the_transfer() {
        // No receiver is waiting on a rendezvous channel: `Full`.
        let (transfers, _indexer) = mpsc::sync_channel(0);
        let (code, message) = refusal(answer(
            &transfers,
            export("work", "/out.idx"),
            PROTOCOL_VERSION,
        ));
        assert_eq!(code, "index_unavailable");
        assert!(message.contains("handling another export"), "{message}");
    }

    #[test]
    fn a_stopped_indexer_refuses_the_transfer() {
        let (transfers, indexer) = mpsc::sync_channel(1);
        drop(indexer);
        let (code, message) = refusal(answer(
            &transfers,
            export("work", "/out.idx"),
            PROTOCOL_VERSION,
        ));
        assert_eq!(code, "index_unavailable");
        assert!(message.contains("index is not available"), "{message}");
    }

    #[test]
    fn a_checked_transfer_reaches_the_indexer_unanswered() {
        let (transfers, indexer) = mpsc::sync_channel(1);
        let import = Command::GraphImport {
            workspace_id: "work".into(),
            path: "/in.idx".into(),
            apply: false,
        };
        let asked = std::thread::spawn(move || answer(&transfers, import, PROTOCOL_VERSION));
        let transfer = indexer.recv().unwrap();
        assert_eq!(transfer.of.workspace_id, "work");
        assert_eq!(transfer.request_id, "transfer-1");
        assert!(is_transfer(&transfer.command));
        drop(transfer);
        assert_eq!(asked.join().unwrap(), None);
    }

    #[test]
    fn an_unchecked_transfer_is_refused_with_its_reason() {
        let long = format!("/{}", "a".repeat(4096));
        let cases = [
            (export("work", "/out.idx"), 0, "unsupported_version"),
            (export("work", "out.idx"), PROTOCOL_VERSION, "invalid_path"),
            (export("work", &long), PROTOCOL_VERSION, "invalid_path"),
            (
                export("work", "/out\0.idx"),
                PROTOCOL_VERSION,
                "invalid_path",
            ),
            (
                export("other", "/out.idx"),
                PROTOCOL_VERSION,
                "unknown_workspace",
            ),
        ];
        for (command, version, expected) in cases {
            let (transfers, _indexer) = mpsc::sync_channel(1);
            let (code, message) = refusal(answer(&transfers, command, version));
            assert_eq!(code, expected, "{message}");
        }
    }
}
