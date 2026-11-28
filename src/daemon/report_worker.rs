//! Reports, read off the state owner on a connection of their own (issue
//! #581): a report of a long history took the owner for as long as it
//! read, and an admission sent meanwhile waited for all of it.

use super::unix::{Job, error, store_failed, unsupported_version};
use crate::protocol::{self, PROTOCOL_VERSION, Response};
use crate::store::{ReportReader, Store, wall_now_ms};
use langspan_core::{Command, Outcome, validate_request_id};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};
use tokio::sync::oneshot;

/// A report the owner has checked against the state, for the reader.
pub(super) struct ReportJob {
    request_id: String,
    generation: u64,
    workspace_id: String,
    from_ms: u64,
    wall_ms: u64,
    reply: oneshot::Sender<Response>,
}

/// Reports waiting for the reader; one more is refused, never queued on
/// the owner.
const REPORT_QUEUE: usize = 4;

pub(super) fn is_report(command: &Command) -> bool {
    matches!(command, Command::Report { .. })
}

fn answer(request_id: String, generation: u64, outcome: Outcome) -> Response {
    Response {
        version: PROTOCOL_VERSION,
        request_id,
        replayed: false,
        generation,
        lease_valid: None,
        outcome,
    }
}

/// Check a report request against the state and hand it to the reader;
/// anything refused is answered here, on the owner.
pub(super) fn forward_report(store: &Store, reports: &mpsc::SyncSender<ReportJob>, job: Job) {
    let generation = store.state().generation;
    let request_id = job.request.request_id;
    let Command::Report {
        workspace_id,
        from_ms,
    } = job.request.command
    else {
        return;
    };
    let refuse = |code: &str, message: String| error(request_id.clone(), generation, code, message);
    let wall_ms = wall_now_ms();
    let refused = if !protocol::answers(job.request.version) {
        Some(refuse("unsupported_version", unsupported_version()))
    } else if let Err(e) = validate_request_id(&request_id) {
        Some(refuse(&e.code, e.message))
    } else {
        store
            .report_refused(&workspace_id, from_ms, wall_ms)
            .map(|outcome| answer(request_id.clone(), generation, outcome))
    };
    if let Some(refused) = refused {
        let _ = job.reply.send(refused);
        return;
    }
    let report = ReportJob {
        request_id: request_id.clone(),
        generation,
        workspace_id,
        from_ms,
        wall_ms,
        reply: job.reply,
    };
    if let Err(error) = reports.try_send(report) {
        let (code, message, report) = match error {
            mpsc::TrySendError::Full(report) => (
                "report_busy",
                "other reports are being read; ask again when they finish",
                report,
            ),
            mpsc::TrySendError::Disconnected(report) => (
                "report_unavailable",
                "the report reader has stopped; restart the coordinator with `langspan daemon`",
                report,
            ),
        };
        let _ = report.reply.send(refuse(code, message.into()));
    }
}

/// Opens the report reader on the store in `state_dir`, which the owner
/// has opened, and starts its thread; reports are sent to it.
pub(super) fn start_report_reader(
    state_dir: &Path,
) -> Result<mpsc::SyncSender<ReportJob>, crate::Error> {
    let reader = ReportReader::open(&state_dir.join("state.sqlite3"))
        .map_err(|e| format!("open the report reader on the coordination store: {e}"))?;
    let (sender, jobs) = mpsc::sync_channel(REPORT_QUEUE);
    let state_dir = state_dir.to_path_buf();
    std::thread::Builder::new()
        .name("langspan-reports".into())
        .spawn(move || report_worker(jobs, reader, state_dir))?;
    Ok(sender)
}

/// Reads each report handed to it, in order, and replies itself.
fn report_worker(jobs: mpsc::Receiver<ReportJob>, mut reader: ReportReader, state_dir: PathBuf) {
    while let Ok(job) = jobs.recv() {
        let outcome = reader
            .report(&job.workspace_id, job.from_ms, job.wall_ms)
            .unwrap_or_else(|e| Outcome::Error {
                code: "storage_error".into(),
                message: store_failed(&state_dir, e),
            });
        let _ = job
            .reply
            .send(answer(job.request_id, job.generation, outcome));
    }
}
