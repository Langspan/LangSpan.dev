//! Bounded local transport; one dedicated owner applies and persists all state.

#[cfg(not(unix))]
use std::path::PathBuf;

#[cfg(not(unix))]
pub async fn run(_state_dir: PathBuf) -> Result<(), crate::Error> {
    Err("this initial daemon requires Unix sockets; Windows transport is not implemented".into())
}

#[cfg(unix)]
mod unix {
    use crate::{
        auto_check, local,
        protocol::{self, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response},
        store::{Execution, Store},
    };
    use langspan_core::{Command, Outcome, State, validate_request_id};
    use serde::Deserialize;
    use std::{
        fs,
        io::{self, Read},
        os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
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
        /// How often the agent hosts on this machine are checked for a
        /// LangSpan install (roadmap A7). The first check is at start.
        host_check_ms: u64,
    }
    impl Default for Config {
        fn default() -> Self {
            Self {
                io_timeout_ms: 2000,
                request_timeout_ms: 5000,
                max_clients: 32,
                remote_pass_ms: 5000,
                auto_check_debounce_ms: 2000,
                host_check_ms: 3_600_000,
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
                || !(100..=86_400_000).contains(&result.host_check_ms)
            {
                return Err(
                    "config limits: io_timeout_ms/request_timeout_ms 100..5000, max_clients 1..128, remote_pass_ms 200..600000, auto_check_debounce_ms 50..60000, host_check_ms 100..86400000"
                        .into(),
                );
            }
            Ok(result)
        }
    }
    struct Job {
        request: Request,
        reply: oneshot::Sender<Response>,
    }

    struct IndexJob {
        workspace_id: String,
        root: PathBuf,
        /// Its unobserved paths are kept in the index as they were, never
        /// treated as removed.
        scan: crate::graph::ScanView,
    }

    /// The index's single writer. It only ever reads working trees and its
    /// own file; what it learns reaches coordination state as a request to
    /// the state owner, like any other client's.
    fn index_worker(jobs: mpsc::Receiver<IndexJob>, sender: mpsc::SyncSender<Job>, path: PathBuf) {
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
        let mut last_failure: Option<String> = None;
        while let Ok(job) = jobs.recv() {
            let deadline = Instant::now() + Duration::from_secs(2);
            match index.update(&job.workspace_id, &job.root, &job.scan, now_ms(), deadline) {
                Ok(notices) => {
                    last_failure = None;
                    for notice in notices {
                        // One request per version of the file: redelivery is a replay.
                        ask(
                            &sender,
                            crate::hook::wire_id(
                                "interface",
                                &[
                                    &job.workspace_id,
                                    &notice.path,
                                    &notice.before_digest,
                                    &notice.digest,
                                ],
                            ),
                            Command::ObserveInterface {
                                workspace_id: job.workspace_id.clone(),
                                path: notice.path,
                                changes: notice.changes,
                                dependents: notice.dependents,
                            },
                        );
                    }
                }
                Err(error) => {
                    let detail = error.to_string();
                    if last_failure.as_ref() != Some(&detail) {
                        eprintln!(
                            "{}",
                            serde_json::json!({"level":"warn","event":"index_update_failed","workspace_id":job.workspace_id,"detail":detail})
                        );
                        last_failure = Some(detail);
                    }
                }
            }
        }
    }

    /// How long a detailed symbol lookup may spend comparing check trees
    /// with Git. What is not compared by then has no `last_check`.
    const LAST_CHECK_BUDGET: Duration = Duration::from_millis(300);

    /// Answer an index query. The index is derived and may lag the working
    /// tree; `coverage` in the answer says by how much.
    fn graph_query(
        graph: Option<&crate::graph::Index>,
        state: &State,
        command: &Command,
    ) -> Result<Outcome, String> {
        let (workspace_id, limit) = match command {
            Command::GraphSymbol {
                workspace_id,
                limit,
                ..
            }
            | Command::GraphReferences {
                workspace_id,
                limit,
                ..
            }
            | Command::GraphImpact {
                workspace_id,
                limit,
                ..
            } => (workspace_id, *limit),
            _ => return Err("not an index query".into()),
        };
        if !state.workspaces.contains_key(workspace_id) {
            return Err("workspace is not registered".into());
        }
        if !(1..=crate::graph::MAX_RESULTS).contains(&limit) {
            return Err("limit must be between 1 and 200".into());
        }
        let depth = match command {
            Command::GraphImpact { depth, .. } => *depth,
            _ => 1,
        };
        if !(1..=langspan_core::MAX_IMPACT_DEPTH).contains(&depth) {
            return Err(format!(
                "depth must be between 1 and {}",
                langspan_core::MAX_IMPACT_DEPTH
            ));
        }
        let graph = graph.ok_or("the index is not available yet; try again shortly")?;
        let failed = |error: Box<dyn std::error::Error + Send + Sync>| error.to_string();
        let detail = matches!(command, Command::GraphSymbol { detail: true, .. });
        let started = Instant::now();
        let ((mut symbols, mut references, levels), coverage) = graph
            .with_coverage(workspace_id, |graph| match command {
                Command::GraphSymbol { name, .. } => {
                    let mut symbols = graph.symbol(workspace_id, name, limit)?;
                    if detail {
                        for symbol in symbols.iter_mut().take(limit as usize) {
                            graph.add_detail(workspace_id, symbol)?;
                        }
                    }
                    Ok((symbols, Vec::new(), None))
                }
                Command::GraphReferences { name, .. } => Ok((
                    Vec::new(),
                    graph.references(workspace_id, name, None, limit)?,
                    None,
                )),
                Command::GraphImpact { path, .. } => {
                    let (symbols, references) = graph.impact(workspace_id, path, limit)?;
                    let levels = (depth > 1)
                        .then(|| {
                            graph.impact_levels(workspace_id, path, depth, started + IMPACT_BUDGET)
                        })
                        .transpose()?;
                    Ok((symbols, references, levels))
                }
                _ => unreachable!("matched above"),
            })
            .map_err(failed)?;
        let mut truncated = symbols.len() > limit as usize || references.len() > limit as usize;
        symbols.truncate(limit as usize);
        references.truncate(limit as usize);
        if detail {
            add_notes_and_checks(state, workspace_id, &mut symbols);
            truncated |= fit_frame(&mut symbols, &coverage);
        }
        let levels = levels.map(|mut levels| {
            fit_levels(&mut levels, &(&symbols, &references, &coverage));
            truncated |= levels.iter().any(|level| level.truncated);
            levels
        });
        Ok(Outcome::Graph {
            symbols,
            references,
            resolution: (detail || levels.is_some()).then(|| "name".into()),
            coverage,
            truncated,
            levels,
        })
    }

    /// How long `graph impact` may spend on its levels: well inside the
    /// client's 5 s call timeout, and short because requests behind it
    /// (hooks among them) wait. At most 150 indexed lookups usually take
    /// far less. Past it the levels so far are returned, marked truncated.
    const IMPACT_BUDGET: Duration = Duration::from_millis(500);

    /// Keep the leading impact files whose answer fits one response frame
    /// beside `rest` (the outcome's other lists); a level losing files and
    /// every level after it are marked `truncated`.
    fn fit_levels(levels: &mut [langspan_core::ImpactLevel], rest: &impl serde::Serialize) {
        // The envelope, the request ID and the outcome's scalar fields.
        const ROOM: usize = 1024;
        // A level's own fields, beside its files.
        const LEVEL: usize = 64;
        fn size(value: &impl serde::Serialize) -> usize {
            serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
        }
        let mut used = ROOM.saturating_add(size(rest));
        let mut cut = false;
        for level in levels {
            used = used.saturating_add(LEVEL);
            let keep = if cut {
                0
            } else {
                level
                    .files
                    .iter()
                    .take_while(|file| {
                        used = used.saturating_add(size(file) + 1);
                        used < MAX_FRAME_BYTES
                    })
                    .count()
            };
            if keep < level.files.len() {
                level.files.truncate(keep);
                level.truncated = true;
                cut = true;
            }
        }
    }

    /// What the coordinator, not the index, knows about each definition's
    /// file: notes that name it, and the newest conclusive check record
    /// whose tree holds its indexed content.
    fn add_notes_and_checks(
        state: &State,
        workspace_id: &str,
        symbols: &mut [langspan_core::SymbolRef],
    ) {
        let notes = state.notes.get(workspace_id).map(Vec::as_slice);
        let files: std::collections::BTreeMap<String, String> = symbols
            .iter()
            .map(|symbol| (symbol.path.clone(), symbol.digest.clone()))
            .collect();
        let checked = match (
            state.workspaces.get(workspace_id),
            state.checks.get(workspace_id),
        ) {
            (Some(workspace), Some(records)) => crate::graph::last_checks(
                Path::new(&workspace.root),
                &files,
                records,
                Instant::now() + LAST_CHECK_BUDGET,
            ),
            _ => Default::default(),
        };
        for symbol in symbols {
            symbol.notes = Some(
                notes
                    .unwrap_or_default()
                    .iter()
                    .filter(|note| note.paths.contains(&symbol.path))
                    .take(langspan_core::MAX_SYMBOL_NOTES)
                    .map(|note| langspan_core::SymbolNote {
                        id: note.note_id.clone(),
                        text: note.text.clone(),
                    })
                    .collect(),
            );
            symbol.last_check = checked.get(&symbol.path).cloned();
        }
    }

    /// Keep the leading definitions whose answer fits one response frame.
    /// Returns whether any was left out.
    fn fit_frame(
        symbols: &mut Vec<langspan_core::SymbolRef>,
        coverage: &langspan_core::GraphCoverage,
    ) -> bool {
        // The rest of the response: its envelope, the request ID and the
        // outcome's other fields.
        const ROOM: usize = 1024;
        fn size(value: &impl serde::Serialize) -> usize {
            serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
        }
        let mut used = ROOM + size(coverage);
        let keep = symbols
            .iter()
            .take_while(|symbol| {
                used = used.saturating_add(size(*symbol) + 1);
                used < MAX_FRAME_BYTES
            })
            .count();
        let cut = keep < symbols.len();
        symbols.truncate(keep);
        cut
    }

    /// Where the index is blind (roadmap D8): read-only, from one snapshot
    /// of the index, with the last scan failure if one is recorded.
    fn graph_blind_spots(
        graph: Option<&crate::graph::Index>,
        state: &State,
        workspace_id: &str,
        limit: u32,
    ) -> Result<Outcome, String> {
        if !state.workspaces.contains_key(workspace_id) {
            return Err("workspace is not registered".into());
        }
        if !(1..=crate::graph::MAX_RESULTS).contains(&limit) {
            return Err("limit must be between 1 and 200".into());
        }
        let graph = graph.ok_or("the index is not available yet; try again shortly")?;
        let (mut blind_spots, coverage) = graph
            .with_coverage(workspace_id, |graph| graph.blind_spots(workspace_id, limit))
            .map_err(|error| error.to_string())?;
        let scan_failure = state.scan_failures.get(workspace_id).cloned();
        let totals = &blind_spots.totals;
        let mut truncated = [
            (blind_spots.partial.len(), totals.partial),
            (blind_spots.never_clean.len(), totals.never_clean),
            (blind_spots.waiting.len(), totals.waiting),
            (blind_spots.too_large.len(), totals.too_large),
            (blind_spots.unscanned.len(), totals.unscanned),
        ]
        .iter()
        .any(|(listed, total)| *listed < *total as usize);
        truncated |= fit_blind_spots(&mut blind_spots, &coverage, scan_failure.as_ref());
        Ok(Outcome::GraphBlindSpots {
            blind_spots,
            coverage,
            scan_failure,
            truncated,
        })
    }

    /// Long paths can make five lists of 200 exceed one response frame. Keep
    /// the longest prefix, the same length for every list, that fits, so a
    /// trimmed answer is still the same on every call. Returns whether any
    /// path was left out.
    fn fit_blind_spots(
        spots: &mut langspan_core::BlindSpots,
        coverage: &langspan_core::GraphCoverage,
        failure: Option<&langspan_core::ScanFailure>,
    ) -> bool {
        const ROOM: usize = 1024;
        let size = |spots: &langspan_core::BlindSpots| {
            serde_json::to_vec(&(spots, coverage, failure)).map_or(usize::MAX, |bytes| bytes.len())
        };
        if size(spots) + ROOM < MAX_FRAME_BYTES {
            return false;
        }
        let cut = |spots: &langspan_core::BlindSpots, keep: usize| {
            let mut spots = spots.clone();
            spots.partial.truncate(keep);
            spots.never_clean.truncate(keep);
            spots.waiting.truncate(keep);
            spots.too_large.truncate(keep);
            spots.unscanned.truncate(keep);
            spots
        };
        // The largest prefix that fits; zero always does.
        let (mut low, mut high) = (0, crate::graph::MAX_RESULTS as usize);
        while low < high {
            let middle = (low + high).div_ceil(2);
            if size(&cut(spots, middle)) + ROOM < MAX_FRAME_BYTES {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        *spots = cut(spots, low);
        true
    }

    /// Definitions and mentioning files for names taken from a prompt. The
    /// names are used for this query only: never logged, recorded or kept.
    fn graph_context(
        graph: Option<&crate::graph::Index>,
        state: &State,
        workspace_id: &str,
        names: &[String],
        limit: u32,
    ) -> Result<Outcome, (&'static str, String)> {
        langspan_core::validate_graph_context(names, limit)
            .map_err(|error| ("invalid_graph_context", error.message))?;
        let unavailable = |message: String| ("index_unavailable", message);
        if !state.workspaces.contains_key(workspace_id) {
            return Err(unavailable("workspace is not registered".into()));
        }
        let graph = graph.ok_or_else(|| {
            unavailable("the index is not available yet; try again shortly".into())
        })?;
        let failed =
            |error: Box<dyn std::error::Error + Send + Sync>| unavailable(error.to_string());
        let (matches, coverage) = graph
            .with_coverage(workspace_id, |graph| {
                let mut matches = Vec::new();
                for name in names {
                    let mut definitions = graph.symbol(workspace_id, name, limit)?;
                    if definitions.is_empty() {
                        continue;
                    }
                    let files = langspan_core::CONTEXT_FILES;
                    let mut mentioned_in = graph.mentioned_in(workspace_id, name, files)?;
                    let truncated =
                        definitions.len() > limit as usize || mentioned_in.len() > files as usize;
                    definitions.truncate(limit as usize);
                    mentioned_in.truncate(files as usize);
                    matches.push(langspan_core::NameContext {
                        name: name.clone(),
                        definitions,
                        mentioned_in,
                        truncated,
                    });
                }
                Ok(matches)
            })
            .map_err(failed)?;
        Ok(Outcome::GraphContext { matches, coverage })
    }

    /// The write-time break check (ADR 0029). The calls are used for this
    /// answer only: never logged or recorded. What is recorded, when a
    /// break is found, is which notices the session was alerted to.
    fn check_calls(
        store: &mut Store,
        graph: Option<&crate::graph::Index>,
        request_id: String,
        session_id: &str,
        calls: &[langspan_core::CallSite],
    ) -> Response {
        let now = now_ms();
        let generation = store.state().generation;
        let answer = |outcome| Response {
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            replayed: false,
            generation,
            lease_valid: None,
            outcome,
        };
        let candidates = match langspan_core::call_candidates(store.state(), session_id, calls, now)
        {
            Ok(candidates) => candidates,
            Err(e) => return error(request_id, generation, &e.code, e.message),
        };
        let workspace_id = store.state().sessions[session_id].workspace_id.clone();
        let not_ready = |coverage: Option<langspan_core::GraphCoverage>| {
            answer(Outcome::CallBreaks {
                breaks: Vec::new(),
                coverage: coverage.unwrap_or_default(),
                ready: false,
            })
        };
        // Nothing is looked up before the index has a complete scan.
        let Some(graph) = graph else {
            return not_ready(None);
        };
        match graph.with_coverage(&workspace_id, |_| Ok(())) {
            Ok(((), coverage)) if coverage.scanned => {}
            Ok(((), coverage)) => return not_ready(Some(coverage)),
            Err(failure) => {
                return error(
                    request_id,
                    generation,
                    "index_unavailable",
                    failure.to_string(),
                );
            }
        }
        // The new signatures and the coverage they are judged with come from
        // one snapshot of the index.
        let looked_up = graph.with_coverage(&workspace_id, |graph| {
            let mut current = Vec::new();
            for name in &candidates {
                current.extend(graph.symbol(&workspace_id, name, crate::graph::MAX_RESULTS)?);
            }
            Ok(current)
        });
        let (current, coverage) = match looked_up {
            Ok((current, coverage)) if coverage.scanned => (current, coverage),
            Ok((_, coverage)) => return not_ready(Some(coverage)),
            Err(failure) => {
                return error(
                    request_id,
                    generation,
                    "index_unavailable",
                    failure.to_string(),
                );
            }
        };
        let breaks =
            match langspan_core::call_breaks(store.state(), session_id, calls, &current, now) {
                Ok(breaks) => breaks,
                Err(e) => return error(request_id, generation, &e.code, e.message),
            };
        // Only the notices the hook's alert will show are recorded: one
        // left out for room stays to be told at a later write. The hook
        // lays the alert out with the same function.
        let notice_ids = crate::breaks::shown_notices(&breaks);
        if !notice_ids.is_empty() {
            // Recorded before it is answered: an alert that could not be
            // remembered is not given, so none is given twice.
            let record = Command::AcknowledgeInterface {
                session_id: session_id.to_owned(),
                notice_ids,
            };
            let id = crate::hook::wire_id("interface-alert", &[&request_id]);
            match store.execute(&id, &record, &record, now) {
                Ok(Execution {
                    outcome: Outcome::Error { code, message },
                    ..
                }) => return error(request_id, generation, &code, message),
                Ok(_) => {}
                Err(failure) => {
                    return error(request_id, generation, "storage_error", failure.to_string());
                }
            }
        }
        answer(Outcome::CallBreaks {
            breaks,
            coverage,
            ready: true,
        })
    }

    struct ScanJob {
        workspace_id: String,
        root: PathBuf,
    }
    struct ScanResult {
        workspace_id: String,
        root: PathBuf,
        snapshot: Result<crate::observer::Snapshot, String>,
    }

    pub async fn run(state_dir: PathBuf) -> Result<(), crate::Error> {
        let _ = STARTED_MS.set(now_ms());
        let directory = local::prepare_directory(&state_dir)
            .map_err(|e| format!("prepare state directory: {e}"))?;
        let lock = local::private_file(&directory.join("daemon.lock"), true)?;
        lock.try_lock()
            .map_err(|error| format!("another daemon may own this state directory: {error}"))?;
        let config =
            Config::read(&directory).map_err(|e| format!("read daemon configuration: {e}"))?;
        local::private_file(&directory.join("state.sqlite3"), true)?;
        for name in ["state.sqlite3-wal", "state.sqlite3-shm"] {
            local::check_existing_file(&directory.join(name))?;
        }
        let mut store = Store::open(&directory.join("state.sqlite3"))
            .map_err(|e| format!("open coordination store: {e}"))?;
        let generation = store.state().generation;
        let socket = directory.join("daemon.sock");
        match fs::symlink_metadata(&socket) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == rustix::process::geteuid().as_raw() =>
            {
                fs::remove_file(&socket)?
            }
            Ok(_) => return Err("refusing to replace a non-socket or foreign daemon.sock".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener =
            UnixListener::bind(&socket).map_err(|e| format!("bind local Unix socket: {e}"))?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let (sender, receiver) = mpsc::sync_channel::<Job>(config.max_clients);
        let (scan_sender, scan_receiver) = mpsc::sync_channel::<ScanJob>(1);
        let (scan_result_sender, scan_results) = mpsc::sync_channel::<ScanResult>(1);
        std::thread::Builder::new()
            .name("langspan-git-reader".into())
            .spawn(move || {
                while let Ok(job) = scan_receiver.recv() {
                    let snapshot =
                        crate::observer::scan(&job.root, Instant::now() + Duration::from_secs(2));
                    if scan_result_sender
                        .send(ScanResult {
                            workspace_id: job.workspace_id,
                            root: job.root,
                            snapshot,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        let graph_path = directory.join("graph.sqlite3");
        local::private_file(&graph_path, true)?;
        let (index_sender, index_receiver) = mpsc::sync_channel::<IndexJob>(1);
        {
            let sender = sender.clone();
            let graph_path = graph_path.clone();
            std::thread::Builder::new()
                .name("langspan-indexer".into())
                .spawn(move || index_worker(index_receiver, sender, graph_path))?;
        }
        let (check_sender, check_receiver) = mpsc::sync_channel::<auto_check::Message>(256);
        {
            let sender = sender.clone();
            let check_sender = check_sender.clone();
            let debounce = Duration::from_millis(config.auto_check_debounce_ms);
            std::thread::Builder::new()
                .name("langspan-auto-checks".into())
                .spawn(move || {
                    auto_check::worker(check_sender, check_receiver, debounce, |id, command| {
                        ask(&sender, id, command)
                    })
                })?;
        }
        std::thread::Builder::new()
            .name("langspan-owner".into())
            .spawn(move || {
                // Holding the file handle keeps ownership for exactly the writer's lifetime.
                let _lock = lock;
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
                loop {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(job) => {
                            if graph_reader.is_none() {
                                graph_reader = crate::graph::Index::open_reader(&graph_path).ok();
                            }
                            let touched = touched_workspace(store.state(), &job.request.command);
                            let check_before = auto_check_of(store.state(), &job.request.command);
                            let boundary = boundary_session(&job.request.command);
                            let response = handle(&mut store, graph_reader.as_ref(), job.request);
                            if let Some(message) = auto_check_message(store.state(), check_before, boundary, &response) {
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
                    if let Ok(ScanResult {workspace_id, root, snapshot}) = scan_results.try_recv() {
                        scan_pending = false;
                        let still_registered = store.state().workspaces.get(&workspace_id)
                            .is_some_and(|workspace| Path::new(&workspace.root) == root);
                        let recorded = if still_registered {
                            snapshot.and_then(|snapshot| {
                                store.record_scan(&workspace_id, &snapshot, now).map_err(|e| e.to_string())?;
                                // Hand the scan to the indexer. If it is busy the
                                // next scan carries the same information.
                                let _ = index_sender.try_send(IndexJob {
                                    workspace_id: workspace_id.clone(),
                                    root: root.clone(),
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
                    if Instant::now() >= next_liveness {
                        observe_processes(&mut store, &mut previous_session, now);
                        next_liveness = Instant::now() + Duration::from_secs(1);
                    }
                    if !scan_pending && (!urgent.is_empty() || Instant::now() >= next_scan) {
                        let workspace = std::iter::from_fn(|| urgent.pop_front())
                            .find_map(|id| {
                                store.state().workspaces.get(&id).map(|workspace| (id, PathBuf::from(&workspace.root)))
                            })
                            .or_else(|| {
                                let next = next_workspace(store.state(), previous_workspace.as_deref());
                                if let Some((id, _)) = &next {
                                    previous_workspace = Some(id.clone());
                                }
                                next
                            });
                        if let Some((workspace_id, root)) = workspace {
                            last_scanned.insert(workspace_id.clone(), Instant::now());
                            match scan_sender.try_send(ScanJob {workspace_id, root}) {
                                Ok(()) => { scan_pending = true; }
                                Err(error) => eprintln!("{}", serde_json::json!({"level":"error","event":"git_worker_unavailable","detail":error.to_string()})),
                            }
                        }
                        next_scan = Instant::now() + Duration::from_secs(2);
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
        let slots = Arc::new(Semaphore::new(config.max_clients));
        loop {
            let (stream, _) = listener.accept().await?;
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let sender = sender.clone();
            let config = config.clone();
            tokio::spawn(async move {
                let _permit = permit;
                serve_connection(stream, sender, config).await;
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
        let (reply, receive) = oneshot::channel();
        sender
            .try_send(Job {
                request: Request {
                    version: PROTOCOL_VERSION,
                    request_id,
                    command,
                },
                reply,
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

    fn error(id: String, generation: u64, code: &str, message: impl Into<String>) -> Response {
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
    fn now_ms() -> u64 {
        crate::clock::now_ms()
    }
    /// When this daemon started, on the same clock, for the uptime shown.
    static STARTED_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    /// A workspace scanned less than this long ago is not moved up again.
    const URGENT_AFTER: Duration = Duration::from_secs(5);

    /// The workspace a request is about, when it is one where a session is
    /// acting: a registration, an edit, a prompt.
    fn touched_workspace(state: &State, command: &Command) -> Option<String> {
        match command {
            Command::WorkspaceRegister { workspace_id, .. } => Some(workspace_id.clone()),
            Command::AdmitTool { session_id, .. }
            | Command::ObserveTool { session_id, .. }
            | Command::SessionPrompt { session_id, .. } => state
                .sessions
                .get(session_id)
                .map(|session| session.workspace_id.clone()),
            _ => None,
        }
    }

    /// For a request that turns a workspace's automatic check on or off:
    /// that workspace and its check before the request.
    fn auto_check_of(
        state: &State,
        command: &Command,
    ) -> Option<(String, Option<langspan_core::AutoCheck>)> {
        match command {
            Command::AutoCheckEnable { workspace_id, .. }
            | Command::AutoCheckDisable { workspace_id } => Some((
                workspace_id.clone(),
                state.auto_checks.get(workspace_id).cloned(),
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
    /// the owner changed the check. A replayed request tells it nothing.
    fn auto_check_message(
        state: &State,
        check_before: Option<(String, Option<langspan_core::AutoCheck>)>,
        boundary: Option<String>,
        response: &Response,
    ) -> Option<auto_check::Message> {
        if response.replayed {
            return None;
        }
        if let Some((workspace_id, before)) = check_before {
            return (state.auto_checks.get(&workspace_id) != before.as_ref())
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
        Some(auto_check::Message::Wrote {
            check: check.clone(),
            root: PathBuf::from(&state.workspaces.get(workspace_id)?.root),
            session_id,
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
        for (index, prior) in observations.into_iter().enumerate() {
            *after = Some(prior.session_id.clone());
            let status = crate::process::probe(&prior.process);
            let same = prior.generation == store.state().generation && prior.status == status;
            // Dead incarnations remain dead. Other unchanged samples are
            // coalesced, while every transition is recorded at the next poll.
            if same
                && (status == langspan_core::LivenessStatus::Exited
                    || now.saturating_sub(prior.observed_at_ms) < 15_000)
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
        request: Request,
    ) -> Response {
        let reads_blind = reads_blind_in_scope(&request.command);
        let mut response = answer(store, graph, request);
        if !reads_blind {
            for coverage in coverages(&mut response.outcome) {
                coverage.blind_in_scope = 0;
            }
        }
        response
    }

    /// Whether the client asked for `GraphCoverage::blind_in_scope`. Earlier
    /// builds read coverage with `deny_unknown_fields`; a request that does
    /// not ask is answered in the shape they know, the field absent.
    fn reads_blind_in_scope(command: &Command) -> bool {
        matches!(
            command,
            Command::GraphBlindSpots { .. }
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
        request: Request,
    ) -> Response {
        let Request {
            version,
            request_id,
            command,
        } = request;
        if version != PROTOCOL_VERSION {
            return error(
                request_id,
                store.state().generation,
                "unsupported_version",
                "expected protocol version 3; restart the daemon after upgrading LangSpan",
            );
        }
        if let Err(e) = validate_request_id(&request_id) {
            return error(request_id, store.state().generation, &e.code, e.message);
        }
        // Every client, not only the CLI: the command is stored as it is.
        if let Command::AutoCheckEnable { argv, .. } = &command
            && let Err(message) = auto_check::refuse_secrets(argv)
        {
            return error(
                request_id,
                store.state().generation,
                "secret_in_command",
                message,
            );
        }
        if let Command::CheckCalls { session_id, calls } = &command {
            return check_calls(store, graph, request_id, session_id, calls);
        }
        if matches!(
            command,
            Command::GraphSymbol { .. }
                | Command::GraphReferences { .. }
                | Command::GraphImpact { .. }
                | Command::GraphBlindSpots { .. }
                | Command::GraphContext { .. }
        ) {
            let answer = match &command {
                Command::GraphBlindSpots {
                    workspace_id,
                    limit,
                } => graph_blind_spots(graph, store.state(), workspace_id, *limit)
                    .map_err(|message| ("index_unavailable", message)),
                Command::GraphContext {
                    workspace_id,
                    names,
                    limit,
                } => graph_context(graph, store.state(), workspace_id, names, *limit),
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
                existing.workspace_id != *workspace_id && Path::new(&existing.root) == canonical
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
        let prepared = match prepare(store.state(), &command) {
            Ok(command) => command,
            Err(e) => {
                return error(
                    request_id,
                    store.state().generation,
                    "invalid_path",
                    e.to_string(),
                );
            }
        };
        match store.execute(&request_id, &command, &prepared, now_ms()) {
            Ok(mut execution) => {
                if let Outcome::Dashboard { dashboard } = &mut execution.outcome {
                    complete_dashboard(dashboard, graph);
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
        graph: Option<&crate::graph::Index>,
    ) {
        dashboard.uptime_ms = STARTED_MS
            .get()
            .map(|started| now_ms().saturating_sub(*started));
        if let Some(graph) = graph {
            for workspace in &mut dashboard.workspaces {
                workspace.coverage = graph
                    .with_coverage(&workspace.workspace_id, |_| Ok(()))
                    .map(|(_, coverage)| coverage)
                    .ok();
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
        let reply = match timeout(Duration::from_millis(config.io_timeout_ms), read).await {
            Ok(Ok(bytes)) => match serde_json::from_slice::<Request>(&bytes) {
                Ok(mut request) => {
                    // Clients redact before sending; a client that did not
                    // still cannot have a secret recorded.
                    crate::hook::redact_command(&mut request.command);
                    let id = request.request_id.clone();
                    let (tx, rx) = oneshot::channel();
                    match sender.try_send(Job { request, reply: tx }) {
                        Ok(()) => {
                            match timeout(Duration::from_millis(config.request_timeout_ms), rx)
                                .await
                            {
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
                Err(_) => error(
                    String::new(),
                    0,
                    "invalid_request",
                    "expected a typed version 1 request; unknown fields are rejected",
                ),
            },
            Ok(Err(e)) => error(String::new(), 0, "invalid_frame", e.to_string()),
            Err(_) => error(
                String::new(),
                0,
                "read_timeout",
                "JSON request was not received before the I/O deadline",
            ),
        };
        let bytes=protocol::encode(&reply).or_else(|_|protocol::encode(&error(reply.request_id,reply.generation,"response_too_large","response exceeds 64 KiB; reduce the event page size, or page the state with `status --all`")));
        if let Ok(bytes) = bytes {
            let _ = timeout(
                Duration::from_millis(config.io_timeout_ms),
                writer.write_all(&bytes),
            )
            .await;
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
                let deadline = Instant::now() + Duration::from_millis(250);
                let mut verified_overlaps = Vec::new();
                for old in state.workspaces.values() {
                    let other = Path::new(&old.root);
                    if old.workspace_id != *workspace_id
                        && (canonical.starts_with(other) || other.starts_with(&canonical))
                    {
                        let linked = crate::git::identity(&canonical, deadline)
                            .and_then(|a| crate::git::identity(other, deadline).map(|b| (a, b)))
                            .is_ok_and(|(a, b)| {
                                a.root == canonical
                                    && b.root == other
                                    && a.root != b.root
                                    && a.common_dir == b.common_dir
                            });
                        if !linked {
                            return Err("workspace root overlaps an existing physical workspace; reuse its ID".into());
                        }
                        verified_overlaps.push(old.workspace_id.clone());
                    }
                }
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
                status: crate::process::probe(process),
            }),
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
        let current_root = fs::canonicalize(root)?;
        if current_root != root {
            return Err("workspace root identity changed; use a new workspace registration".into());
        }
        let mut keys = paths
            .iter()
            .map(|path| -> Result<String, crate::Error> {
                let key = file_key(root, path)?;
                let full = root.join(&key);
                if state.workspaces.values().any(|workspace| {
                    let descendant = Path::new(&workspace.root);
                    descendant != root && descendant.starts_with(root) && full.starts_with(descendant)
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
        use langspan_core::{GraphCoverage, SymbolNote, SymbolRef};

        #[test]
        fn a_detailed_answer_keeps_the_definitions_that_fit_one_frame() {
            let symbol = |index: usize| SymbolRef {
                path: format!("src/file{index}.rs"),
                name: "upload".into(),
                notes: Some(vec![
                    SymbolNote {
                        id: "note".into(),
                        text: "x".repeat(300),
                    };
                    4
                ]),
                ..SymbolRef::default()
            };
            let coverage = GraphCoverage::default();
            let mut few: Vec<SymbolRef> = (0..3).map(symbol).collect();
            assert!(!fit_frame(&mut few, &coverage));
            assert_eq!(few.len(), 3);

            let mut many: Vec<SymbolRef> = (0..200).map(symbol).collect();
            assert!(fit_frame(&mut many, &coverage), "left out, and said so");
            assert!(many.len() > 10, "{}", many.len());
            let response = Response {
                version: PROTOCOL_VERSION,
                request_id: "r".repeat(128),
                replayed: false,
                generation: u64::MAX,
                lease_valid: None,
                outcome: Outcome::Graph {
                    symbols: many,
                    references: Vec::new(),
                    resolution: Some("name".into()),
                    coverage,
                    truncated: true,
                    levels: None,
                },
            };
            protocol::encode(&response).expect("fits one frame");
        }

        #[test]
        fn impact_levels_with_long_paths_are_cut_to_one_frame_and_say_so() {
            use langspan_core::{ImpactFile, ImpactLevel};
            let file = |level: u32, n: u32| ImpactFile {
                path: format!("src/{level}/{n:03}/{}.rs", "x".repeat(200)),
                via: "name".into(),
                from: format!("src/{}.rs", "y".repeat(200)),
            };
            let mut levels: Vec<ImpactLevel> = (1..=3)
                .map(|level| ImpactLevel {
                    level,
                    files: (0..100).map(|n| file(level, n)).collect(),
                    truncated: false,
                })
                .collect();
            let coverage = GraphCoverage::default();
            fit_levels(&mut levels, &(&coverage,));
            assert!(!levels[0].truncated, "the first level fits whole");
            assert!(levels[1].truncated && levels[2].truncated);
            assert!(levels[1].files.len() < 100);
            assert_eq!(levels[2].files, []);
            let response = Response {
                version: PROTOCOL_VERSION,
                request_id: "r".repeat(128),
                replayed: false,
                generation: u64::MAX,
                lease_valid: None,
                outcome: Outcome::Graph {
                    symbols: Vec::new(),
                    references: Vec::new(),
                    resolution: Some("name".into()),
                    coverage,
                    truncated: true,
                    levels: Some(levels),
                },
            };
            protocol::encode(&response).expect("fits one frame");
        }

        #[test]
        fn blind_spots_with_long_paths_keep_one_common_prefix_that_fits_one_frame() {
            let paths = |list: &str| -> Vec<String> {
                (0..200)
                    .map(|n| format!("{list}/{n:03}/{}", "x".repeat(200)))
                    .collect()
            };
            let full = langspan_core::BlindSpots {
                partial: paths("p")
                    .into_iter()
                    .map(|path| langspan_core::PartialFile { path, since_ms: 1 })
                    .collect(),
                never_clean: paths("n"),
                waiting: paths("w"),
                too_large: paths("t"),
                unscanned: paths("u"),
                ..Default::default()
            };
            let coverage = GraphCoverage::default();
            let mut small = langspan_core::BlindSpots {
                waiting: vec!["src/a.rs".into()],
                ..Default::default()
            };
            assert!(!fit_blind_spots(&mut small, &coverage, None));
            assert_eq!(small.waiting, ["src/a.rs"]);

            let mut first = full.clone();
            assert!(fit_blind_spots(&mut first, &coverage, None), "said so");
            let kept = first.waiting.len();
            assert!(kept > 10 && kept < 200, "{kept}");
            assert_eq!(first.partial.len(), kept);
            assert_eq!(first.unscanned.len(), kept);
            assert_eq!(first.waiting[..], full.waiting[..kept], "a prefix");
            let mut again = full.clone();
            fit_blind_spots(&mut again, &coverage, None);
            assert_eq!(again, first, "the same on every call");
            let response = Response {
                version: PROTOCOL_VERSION,
                request_id: "r".repeat(128),
                replayed: false,
                generation: u64::MAX,
                lease_valid: None,
                outcome: Outcome::GraphBlindSpots {
                    blind_spots: first,
                    coverage,
                    scan_failure: None,
                    truncated: true,
                },
            };
            protocol::encode(&response).expect("fits one frame");
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
}

#[cfg(unix)]
pub use unix::run;
