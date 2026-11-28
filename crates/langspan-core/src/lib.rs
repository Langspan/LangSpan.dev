//! Pure coordination decisions and event replay. Time and identifiers are supplied
//! by the caller; this crate performs no I/O and cannot fence filesystem writers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

mod admission;
mod anchors;
mod authority;
mod briefing;
mod checks;
mod classes;
mod command;
mod count;
mod dashboard;
mod error_code;
mod event;
mod flaky;
mod history;
mod lines;
mod messages;
mod notices;
mod observation;
mod outcome;
mod policy;
mod rank;
mod recall;
mod recent;
mod refusal_outcome;
mod remote;
mod rename_holds;
mod report;
mod route;
mod sessions;
mod shared;
mod since;
mod state;
mod status;
mod status_line;
mod today;
mod trail;
mod validate;
mod workspaces;
mod write_check;
pub use admission::*;
pub use anchors::{
    ANCHOR_CANDIDATES, AnchorIndex, AnchorLookup, AnchorState, MAX_TAGS, NoteAnchor, SYMBOL_BYTES,
    TAG_BYTES, anchored, remember_lookups, validate_tag, validate_tags,
};
pub use authority::{Authority, authority, owner_grant_effect};
pub use briefing::*;
pub use checks::*;
pub use classes::{
    FailureClass, KeptTests, MAX_FIXED_NAMED, MAX_WRITE_RUNS, MAX_WRITERS_NAMED, WriteRun,
};
pub use command::Command;
pub use count::{
    COUNT_DAYS, COUNT_KINDS, COUNT_REFUSALS, COUNT_WORKSPACES, CountQuery, RefusalCount,
    count_refusals, validate_count_days,
};
pub use dashboard::{
    AgentRow, AgentState, ChangeRow, CheckRow, DASHBOARD_AGENTS, DASHBOARD_ITEMS, DASHBOARD_PATHS,
    DASHBOARD_TEXT, DENIAL_WINDOW, Dashboard, DashboardEvent, DashboardEvents, DenialRecord,
    DenialRow, Denials, EditRow, LandingRow, MessageRow, NoteRow, NoticeRow, OverlapRow,
    PublishRow, RecentFile, ScanRow, WorkspaceRow, dashboard_denials,
};
pub use error_code::ErrorCode;
pub use event::Event;
pub use flaky::{
    FLAKY_DECAY_RUNS, FLAKY_MIN_FLIPS, FLAKY_WINDOW_RUNS, FlakyCount, FlakyTest, LastTests,
    MAX_FLAKY_LISTED, MAX_LEARNED_CHECKS, MAX_LEARNED_TESTS, MAX_TEST_NAME_BYTES, MAX_TEST_NAMES,
    MAX_TEST_RESULT_BYTES, TestFailure, TestHistory, TestLearning, TestResults,
};
pub use history::{
    HISTORY_ATTRIBUTION, HISTORY_EVENTS, HISTORY_LIMIT, HISTORY_MOVES, HISTORY_NAME_BYTES,
    HISTORY_RELATED, HISTORY_RELATED_MS, History, HistoryEntry, HistoryMessage, HistoryNote,
    HistoryQuery, history as definition_history,
};
pub use messages::*;
pub use notices::*;
pub use observation::*;
pub use outcome::Outcome;
pub use policy::{
    DEFAULT_WAIT_MS, MAX_UNSETTLED, MAX_WAIT_MS, Policy, PolicyCase, PolicyRow, SettledOutcome,
    UnsettledRefusal, WaitResult,
};
pub use rank::{
    Dependent, QUALIFIER_BYTES, RANK_CANDIDATES, RankInputs, RankReason, is_qualifier,
    rank_definitions, rank_dependents, rank_inputs, validate_rank,
};
pub use recall::{
    RECALL_LIMIT, RECALL_QUERY_BYTES, RECALL_WORDS, Recall, RecalledNote, recall as recall_notes,
};
pub use recent::{
    ChangedPath, ContestedPath, LeftUncommitted, MAX_PATH_REFUSALS, MAX_RECENT_WINDOW_MS,
    MIN_RECENT_WINDOW_MS, PathRefusal, RECENT_BYTES, RECENT_CHANGED, RECENT_CONTESTED,
    RECENT_INTERFACES, RECENT_LEFT, RECENT_LEFT_CANDIDATES, RECENT_NOTICE_CHANGES,
    RECENT_WINDOW_MS, RecentChanges, RecentInterfaceChange,
};
pub use refusal_outcome::{PENDING, REFUSAL_WINDOW_MS, RefusalOutcome};
pub use remote::*;
pub use rename_holds::{MAX_RENAMED, PeerCallers, live_sessions, validate_renamed};
pub use report::{
    OTHER_HOST, REPORT_HISTORY_EVENTS, REPORT_SESSIONS, Report, ReportCounts, ReportHost,
    ReportKnown, ReportQuery, ReportSession, host_kind, known_sessions, open_intents, report,
    session_workspaces,
};
pub use route::Route;
pub use sessions::*;
pub(crate) use shared::*;
pub use since::{
    AWAY_AFTER_MS, MAX_AWAY_AFTER_MS, SINCE_EVENTS, SINCE_ITEMS, Since, SinceChecks, SinceFailure,
    SinceFrom, SinceInterface, SinceLanding, SinceMessage, SinceQuery, since,
};
pub use state::{SnapshotRecord, State};
pub use status::{
    STATUS_RECENT_MS, StatusPosition, StatusRecord, StatusResume, StatusSummary, restore_status,
    shrink_status, status_records, status_view,
};
pub use status_line::{
    ClockAnchor, MAX_REFUSALS, REFUSAL_PATHS, REFUSAL_RETENTION_MS, RecentRefusal, RefusalKind,
    StatusLine, status_line,
};
pub use today::{
    OpenAdmission, OpenCheck, OpenIntent, OpenMessage, TODAY_EVENTS, TODAY_FILES, TODAY_OPEN,
    TODAY_SESSIONS, Today, TodayChecks, TodayLandings, TodayOpen, TodayQuery, TodaySession,
    TodayWindow, today,
};
pub use trail::{
    MAX_TRAIL_FACTS, MAX_TRAILS, TRAIL_BYTES, TRAIL_CHECKS, TRAIL_FILES, TRAIL_JSON_BYTES,
    TRAIL_RANGES, TRAIL_WAITING, Trail, TrailCheck, TrailFacts, TrailFile, TrailView,
};
pub use validate::{MAX_BATCH_OPERATIONS, validate_observed_file, validate_request_id};
pub(crate) use validate::{
    current_session, error, is_hex_digest, known_session, normalized_paths, normalized_related_ids,
    normalized_workspace_links, presence_paths, prompt_target_paths, validate_batch,
    validate_branch_observation, validate_canonical_paths, validate_checkout,
    validate_checkout_run, validate_coverage, validate_disk_observation, validate_fact_header,
    validate_failure, validate_host_name, validate_id, validate_presence_setting,
    validate_relative_path, validate_session_liveness, validate_session_registration,
    validate_summary, validate_tool_identity, validate_workspace, validate_workspace_link_targets,
};
pub use workspaces::{
    MAX_ENDING_SESSIONS, MAX_WORKSPACE_PAGE, WorkspaceEnding, WorkspaceKept, WorkspaceSummary,
};
pub use write_check::{
    DefinitionFacts, IndexFacts, PathFacts, UnreadLines, WRITE_CHECK_DEFINITIONS,
    WRITE_CHECK_LABEL, WRITE_CHECK_PATHS, WRITE_CHECK_SYMBOLS, WRITE_CHECK_WRITERS, WriteCheck,
    WriteCheckPath, WriteCheckRequest, WriteCheckSymbol, write_check, write_check_noticed,
    write_check_request,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedEvent {
    pub seq: u64,
    pub event: Event,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreError {
    pub code: String,
    pub message: String,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CoreError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub events: Vec<Event>,
    pub outcome: Outcome,
}

/// Validate a command and describe its complete transition without changing state.
/// The caller persists the returned events and receipt atomically before applying
/// them. Receipt replay and request-id ownership are storage responsibilities.
pub fn decide(state: &State, command: &Command, request_id: &str, now_ms: u64) -> Decision {
    match decide_checked(state, command, request_id, now_ms) {
        Ok(mut decision) => {
            let settled = policy::settle(state, &decision.events, now_ms);
            decision.events.extend(settled);
            decision
        }
        Err(error) => Decision {
            events: Vec::new(),
            outcome: Outcome::Error {
                code: error.code,
                message: error.message,
            },
        },
    }
}

fn decide_checked(
    state: &State,
    command: &Command,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_request_id(request_id)?;
    match command.route() {
        Route::Store | Route::Index | Route::Internal => return Err(route::answered_elsewhere()),
        Route::Decide | Route::Read => {}
    }
    match command {
        Command::SendMessage {
            workspace_id,
            from,
            to,
            text,
            paths,
        } => messages::decide_send_message(
            state,
            workspace_id,
            from,
            to,
            text,
            paths,
            request_id,
            now_ms,
        ),
        Command::Handoff {
            session_id,
            text,
            paths,
        } => messages::decide_handoff(state, session_id, text, paths, request_id, now_ms),
        Command::AckMessage {
            session_id,
            message_id,
        } => messages::decide_ack_message(state, session_id, message_id, now_ms),
        Command::RetractMessage {
            workspace_id,
            from,
            message_id,
        } => retract(state, workspace_id, from.as_deref(), message_id.as_deref()),
        Command::Remember {
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
        } => decide_checked(
            state,
            &Command::RememberPrepared {
                workspace_id: workspace_id.clone(),
                author: author.clone(),
                text: text.clone(),
                paths: paths.clone(),
                symbol: symbol.clone(),
                tags: tags.clone(),
                index: None,
            },
            request_id,
            now_ms,
        ),
        Command::ObserveNoteAnchors { workspace_id } => {
            messages::decide_observe_note_anchors(state, workspace_id)
        }
        Command::NoteAnchorsPrepared {
            workspace_id,
            index,
        } => messages::decide_note_anchors_prepared(state, workspace_id, index),
        Command::RememberPrepared {
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
            index,
        } => messages::decide_remember_prepared(
            state,
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
            index,
            request_id,
            now_ms,
        ),
        Command::Forget {
            workspace_id,
            note_id,
        } => messages::decide_forget(state, workspace_id, note_id),
        Command::Recap {
            workspace_id,
            tag,
            since,
        } => messages::decide_recap(state, workspace_id, tag, since, now_ms),
        Command::Recall {
            workspace_id,
            query,
            path,
            limit,
        } => messages::decide_recall(state, workspace_id, query, path, limit),
        Command::RemoteEnable {
            workspace_id,
            remote,
            participant,
            name,
            share_task,
            publish_as_branch,
        } => remote::decide_remote_enable(
            state,
            workspace_id,
            remote,
            participant,
            name,
            share_task,
            publish_as_branch,
        ),
        Command::RemoteDisable { workspace_id } => {
            remote::decide_remote_disable(state, workspace_id)
        }
        Command::RemoteOutbox { workspace_id } => {
            remote::decide_remote_outbox(state, workspace_id, now_ms)
        }
        Command::RemotePublished {
            workspace_id,
            sequence,
            signature,
            payload,
            published_at_ms,
            as_branch,
        } => remote::decide_remote_published(
            state,
            workspace_id,
            sequence,
            signature,
            payload,
            published_at_ms,
            as_branch,
        ),
        Command::ObserveRemote {
            workspace_id,
            publication,
        } => remote::decide_observe_remote(state, workspace_id, publication, now_ms),
        Command::RemoteWithdrawn { workspace_id } => {
            remote::decide_remote_withdrawn(state, workspace_id)
        }
        Command::ForgetRemote {
            workspace_id,
            participant,
        } => remote::decide_forget_remote(state, workspace_id, participant),
        Command::RecordCheck {
            workspace_id,
            name,
            command,
            tree_oid,
            head_oid,
            inputs,
            result,
            duration_ms,
            report,
            tests,
            rerun,
            writes_before,
            command_id,
            symbol_findings,
        } => checks::decide_record_check(
            state,
            request_id,
            workspace_id,
            name,
            command,
            tree_oid,
            head_oid,
            inputs,
            result,
            duration_ms,
            report,
            tests,
            rerun,
            writes_before,
            command_id,
            symbol_findings,
            now_ms,
        ),
        Command::FlakyTests { workspace_id, name } => {
            checks::decide_flaky_tests(state, workspace_id, name, now_ms)
        }
        Command::ForgetFlakyTest {
            workspace_id,
            name,
            test,
        } => checks::decide_forget_flaky_test(state, workspace_id, name, test),
        Command::AutoCheckEnable {
            workspace_id,
            name,
            argv,
            timeout_s,
            on_interface_change,
        } => checks::decide_auto_check_enable(
            state,
            workspace_id,
            name,
            argv,
            timeout_s,
            on_interface_change,
        ),
        Command::AutoCheckDisable { workspace_id } => {
            checks::decide_auto_check_disable(state, workspace_id)
        }
        Command::CheckStatus {
            workspace_id,
            tree_oid,
        } => checks::decide_check_status(state, workspace_id, tree_oid),
        Command::LandingReadiness {
            workspace_id,
            name,
            tree_oid,
        } => checks::decide_landing_readiness(state, workspace_id, name, tree_oid),
        Command::RecordLanding {
            workspace_id,
            onto,
            commit_oid,
            tree_oid,
            inputs,
            check_id,
            rebased,
            adapted,
        } => checks::decide_record_landing(
            state,
            request_id,
            workspace_id,
            onto,
            commit_oid,
            tree_oid,
            inputs,
            check_id,
            rebased,
            adapted,
        ),
        Command::TakeTurn {
            workspace_id,
            onto,
            session_id,
            ttl_ms,
            turn_id,
            queue,
        } => checks::decide_take_turn(
            state,
            request_id,
            workspace_id,
            onto,
            session_id,
            ttl_ms,
            turn_id,
            queue,
            now_ms,
        ),
        Command::LeaveTurnQueue {
            workspace_id,
            onto,
            ticket,
            reason,
        } => checks::decide_leave_turn_queue(state, workspace_id, onto, ticket, reason),
        Command::TurnQueue { workspace_id, onto } => {
            checks::decide_turn_queue(state, workspace_id, onto, now_ms)
        }
        Command::ReleaseTurn {
            workspace_id,
            onto,
            turn_id,
            attempts,
        } => checks::decide_release_turn(state, workspace_id, onto, turn_id, attempts),
        Command::Observe {
            session_id,
            operation_id,
            source,
            tool,
            paths,
            result,
            lease_id,
        } => decide_observation(
            state,
            ObservationInput {
                session_id,
                operation_id,
                source,
                tool,
                paths,
                result: *result,
                lease_id: lease_id.as_deref(),
                operation: None,
            },
            request_id,
            now_ms,
        ),
        Command::ObserveTool {
            session_id,
            operation,
            paths,
            result,
            lease_id,
        } => decide_observation(
            state,
            ObservationInput {
                session_id,
                operation_id: &operation.operation_id,
                source: &operation.source,
                tool: &operation.tool,
                paths,
                result: *result,
                lease_id: lease_id.as_deref(),
                operation: Some(operation),
            },
            request_id,
            now_ms,
        ),
        Command::ObserveDisk {
            workspace_id,
            path,
            before,
            after,
            related_operation_ids,
        } => observation::decide_observe_disk(
            state,
            workspace_id,
            path,
            before,
            after,
            related_operation_ids,
            request_id,
            now_ms,
        ),
        Command::ObserveBranch {
            workspace_id,
            branch_ref,
            head_oid,
            tree_oid,
        } => observation::decide_observe_branch(
            state,
            workspace_id,
            branch_ref,
            head_oid,
            tree_oid,
            request_id,
            now_ms,
        ),
        Command::ObserveWorkspaceGit {
            workspace_id,
            repository_id,
            branch_ref,
        } => observation::decide_observe_workspace_git(
            state,
            workspace_id,
            repository_id,
            branch_ref,
        ),
        Command::ObserveScan {
            workspace_id,
            failure,
            coverage,
        } => observation::decide_observe_scan(state, workspace_id, failure, coverage, now_ms),
        Command::ObserveHosts { not_installed } => {
            observation::decide_observe_hosts(state, not_installed, now_ms)
        }
        Command::ObserveCheckout {
            workspace_id,
            from_branch,
            to_branch,
            from_head,
            to_head,
            files,
        } => observation::decide_observe_checkout(
            state,
            workspace_id,
            from_branch,
            to_branch,
            from_head,
            to_head,
            files,
            request_id,
            now_ms,
        ),
        Command::ObserveCheckoutRun {
            session_id,
            command,
            to_branch,
            to_head,
            ran_ms,
            finished_ago_ms,
            files_unchanged,
        } => observation::decide_observe_checkout_run(
            state,
            session_id,
            command,
            to_branch,
            to_head,
            ran_ms,
            finished_ago_ms,
            files_unchanged,
            request_id,
            now_ms,
        ),
        Command::ObserveUncoordinated {
            workspace_id,
            files,
            scanned_from_ms,
        } => observation::decide_observe_uncoordinated(
            state,
            workspace_id,
            files,
            scanned_from_ms,
            request_id,
            now_ms,
        ),
        Command::AcknowledgeInterface {
            session_id,
            notice_ids,
        } => notices::decide_acknowledge_interface(state, session_id, notice_ids),
        Command::InterfaceShown {
            session_id,
            notice_ids,
        } => notices::decide_interface_shown(state, session_id, notice_ids),
        Command::OutlineRead {
            session_id,
            path,
            digest,
            ..
        } => notices::decide_outline_read(state, session_id, path, digest),
        Command::BetweenSteps {
            session_id,
            stops,
            prompt_id,
        } => notices::decide_between_steps(state, session_id, stops, prompt_id, now_ms),
        Command::ObserveInterface {
            workspace_id,
            path,
            changes,
            dependents,
        } => notices::decide_observe_interface(
            state,
            workspace_id,
            path,
            changes,
            dependents,
            request_id,
            now_ms,
        ),
        Command::ObserveInterfaceDependents {
            notice_workspace_id,
            notice_id,
            workspace_id,
            paths,
        } => notices::decide_observe_interface_dependents(
            state,
            notice_workspace_id,
            notice_id,
            workspace_id,
            paths,
        ),
        Command::WorkspaceRegister { workspace_id, root }
        | Command::WorkspaceRegisterVerified {
            workspace_id, root, ..
        } => sessions::decide_workspace_register(state, command, workspace_id, root),
        Command::WorkspaceList { after } => sessions::decide_workspace_list(state, after),
        Command::WorkspaceMove {
            workspace_id, root, ..
        } => workspaces::decide_move(state, workspace_id, root, &[], now_ms),
        Command::WorkspaceMoveVerified {
            workspace_id,
            root,
            verified_overlaps,
        } => workspaces::decide_move(state, workspace_id, root, verified_overlaps, now_ms),
        Command::WorkspaceRemove {
            workspace_id,
            apply,
        } => workspaces::decide_remove(state, workspace_id, *apply, now_ms),
        Command::SessionRegister {
            session_id,
            workspace_id,
            parent,
            headless,
        } => sessions::decide_session_register(state, session_id, workspace_id, parent, headless),
        Command::ObserveSessionLiveness {
            session_id,
            process,
            status,
        } => sessions::decide_observe_session_liveness(state, session_id, process, status, now_ms),
        Command::SessionSeen { session_id } => {
            sessions::decide_session_seen(state, session_id, now_ms)
        }
        Command::RecordContext {
            session_id,
            bytes,
            turn,
        } => sessions::decide_record_context(state, session_id, bytes, turn),
        Command::ForgetTold { session_id, keys } => {
            sessions::decide_forget_told(state, session_id, keys)
        }
        Command::ConfirmTold { session_id, keys } => {
            sessions::decide_confirm_told(state, session_id, keys)
        }
        Command::IntentSet {
            session_id,
            paths,
            summary,
        } => sessions::decide_intent_set(state, session_id, paths, summary),
        Command::IntentEnd { session_id } => sessions::decide_intent_end(state, session_id),
        // The summary is the adapter's one line; the digests let it learn,
        // without any peer's words, whether a live peer's prompt began the
        // same way, so that it can name this prompt by where it differs.
        Command::SessionPrompt {
            session_id,
            summary,
            line_digests,
        } => sessions::decide_session_prompt(state, session_id, summary, line_digests, now_ms),
        Command::SetPromptTargets { session_id, paths } => {
            sessions::decide_set_prompt_targets(state, session_id, paths, now_ms)
        }
        Command::ObservePresence {
            session_id,
            kind,
            paths,
        } => sessions::decide_observe_presence(state, session_id, kind, paths, now_ms),
        Command::SetPresence {
            session_id,
            shown,
            window_ms,
        } => sessions::decide_set_presence(state, session_id, shown, window_ms),
        Command::SessionEnd { session_id, trail } => {
            sessions::decide_session_end(state, session_id, trail, now_ms)
        }
        Command::SessionStop {
            session_id,
            failure,
            prompt_id,
        } => sessions::decide_session_stop(state, session_id, failure, prompt_id, now_ms),
        Command::Adopt { session_id, from } => adopt(state, session_id, from, now_ms),
        Command::Admit {
            session_id,
            paths,
            ttl_ms,
        }
        | Command::AdmitTool {
            session_id,
            paths,
            ttl_ms,
            ..
        } => admission::decide_admit(
            state, command, session_id, paths, ttl_ms, request_id, now_ms,
        ),
        Command::Complete {
            session_id,
            lease_id,
        } => admission::decide_complete(state, session_id, lease_id, now_ms),
        Command::ObserveToolBatch {
            session_id,
            operation_ids,
            calls,
        } => admission::decide_observe_tool_batch(state, session_id, operation_ids, calls, now_ms),
        Command::ObserveSource {
            session_id,
            files,
            origin,
        } => admission::decide_observe_source(state, session_id, files, origin, now_ms),
        Command::Policy { workspace_id } => admission::decide_policy(state, workspace_id),
        Command::SetPolicy {
            workspace_id,
            case,
            policy,
            wait_ms,
        } => policy::decide_set(state, workspace_id, *case, *policy, *wait_ms),
        Command::Holders {
            session_id,
            paths,
            base,
            regions,
        } => admission::decide_holders(state, session_id, paths, base, regions, now_ms),
        Command::RecordWait {
            session_id,
            paths,
            holder_session_id,
            waited_ms,
            result,
            changed,
            input_digest,
        } => policy::decide_wait(
            state,
            policy::WaitReport {
                session_id,
                paths,
                holder_session_id: holder_session_id.as_ref(),
                waited_ms: *waited_ms,
                result: *result,
                changed,
                input_digest: input_digest.as_ref(),
            },
            now_ms,
        ),
        Command::RecordRefusal {
            session_id,
            kind,
            paths,
            peer_session_id,
        } => admission::decide_record_refusal(
            state,
            session_id,
            kind,
            paths,
            peer_session_id,
            now_ms,
        ),
        Command::Brief {
            session_id,
            max_peers,
            deliver,
            mid_turn,
            left,
            recent,
            recent_window_ms,
            mcp,
            changes_only,
            ..
        }
        | Command::BriefPrepared {
            session_id,
            max_peers,
            deliver,
            mid_turn,
            left,
            recent,
            recent_window_ms,
            mcp,
            changes_only,
            ..
        } => briefing::decide_brief(
            state,
            command,
            session_id,
            max_peers,
            deliver,
            mid_turn,
            left,
            recent,
            recent_window_ms,
            mcp,
            changes_only,
            now_ms,
        ),
        Command::Trails {
            workspace_id,
            limit,
        } => briefing::decide_trails(state, workspace_id, limit, now_ms),
        Command::Dashboard => Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::Dashboard {
                dashboard: dashboard::dashboard(state, now_ms),
            },
        }),
        Command::Status => {
            let (view, summary) = status_view(state, now_ms);
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Status {
                    state: view,
                    summary: Some(summary),
                },
            })
        }
        Command::StatusLine { root, from_ms } => {
            if !root.starts_with('/') || root.len() > 4096 {
                return Err(error(
                    "invalid_root",
                    "A status line names an absolute directory of at most 4096 bytes",
                ));
            }
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::StatusLine {
                    line: status_line(state, root, *from_ms, now_ms),
                },
            })
        }
        // Refused above by route; named so a new decided command needs an arm.
        Command::Events { .. }
        | Command::Today { .. }
        | Command::Report { .. }
        | Command::Count { .. }
        | Command::History { .. }
        | Command::StatusRecords { .. }
        | Command::Changes { .. }
        | Command::RemoteSent { .. }
        | Command::Compact { .. }
        | Command::Version
        | Command::ShowMessage { .. }
        | Command::MessageStatus { .. }
        | Command::GraphSymbol { .. }
        | Command::GraphReferences { .. }
        | Command::GraphImpact { .. }
        | Command::GraphBlindSpots { .. }
        | Command::GraphPath { .. }
        | Command::GraphMap { .. }
        | Command::GraphContext { .. }
        | Command::GraphInflight { .. }
        | Command::GraphExport { .. }
        | Command::GraphImport { .. }
        | Command::CheckWrite { .. }
        | Command::CheckCalls { .. } => Err(route::answered_elsewhere()),
    }
}

/// Replay an event, rejecting invalid references or transitions before mutation.
/// Events carry decisions rather than a clock: expiry ordering is established by
/// the writer calling `expiry_events` before command admission and persistence.
pub fn apply(state: &mut State, event: &Event) -> Result<(), CoreError> {
    match event {
        Event::Remembered { note, .. } => messages::apply_remembered(state, note)?,
        Event::Forgotten {
            workspace_id,
            note_id,
        } => messages::apply_forgotten(state, workspace_id, note_id)?,
        Event::NoteAnchorChanged {
            workspace_id,
            note_id,
            anchor,
            state: anchor_state,
        } => {
            messages::apply_note_anchor_changed(state, workspace_id, note_id, anchor, anchor_state)?
        }
        Event::MessageSent { message, .. } => messages::apply_message_sent(state, message)?,
        Event::MessageAcknowledged {
            workspace_id,
            message_id,
            session_id,
            ..
        } => messages::apply_message_acknowledged(state, workspace_id, message_id, session_id)?,
        Event::MessageRetracted {
            workspace_id,
            message_id,
            by,
        } => messages::apply_message_retracted(state, workspace_id, message_id, by)?,
        Event::RetractionDelivered {
            session_id,
            message_id,
        } => messages::apply_retraction_delivered(state, session_id, message_id)?,
        Event::RemoteEnabled { link } => remote::apply_remote_enabled(state, link)?,
        Event::RemoteDisabled { workspace_id } => {
            remote::apply_remote_disabled(state, workspace_id)?
        }
        Event::RemotePublished {
            workspace_id,
            sequence,
            signature,
            ..
        } => remote::apply_remote_published(state, workspace_id, sequence, signature)?,
        Event::RemoteObserved { peer } => remote::apply_remote_observed(state, peer)?,
        Event::RemoteWithdrawn { workspace_id } => {
            remote::apply_remote_withdrawn(state, workspace_id)?
        }
        Event::RemoteForgotten {
            workspace_id,
            participant,
        } => remote::apply_remote_forgotten(state, workspace_id, participant)?,
        Event::CheckRecorded { check, tests } => checks::apply_check_recorded(state, check, tests)?,
        Event::AutoCheckEnabled { check } => checks::apply_auto_check_enabled(state, check)?,
        Event::AutoCheckDisabled { workspace_id } => {
            checks::apply_auto_check_disabled(state, workspace_id)?
        }
        Event::CheckReported {
            session_ids,
            notice,
        } => checks::apply_check_reported(state, event, session_ids, notice)?,
        Event::CheckDelivered {
            session_id,
            check_id,
        } => checks::apply_check_delivered(state, session_id, check_id)?,
        Event::TestsRecorded {
            workspace_id,
            check_id,
            name,
            tree_oid,
            command,
            command_id,
            results,
            rerun,
            at_ms,
        } => checks::apply_tests_recorded(
            state,
            workspace_id,
            check_id,
            name,
            tree_oid,
            command,
            command_id,
            results,
            rerun,
            at_ms,
        )?,
        Event::FlakyTestForgotten {
            workspace_id,
            name,
            test,
        } => checks::apply_flaky_test_forgotten(state, workspace_id, name, test)?,
        Event::Landed { landing } => checks::apply_landed(state, landing)?,
        Event::TurnTaken { turn } => checks::apply_turn_taken(state, turn)?,
        Event::TurnQueueJoined { waiter } => checks::apply_turn_queue_joined(state, waiter)?,
        Event::TurnQueueSeen {
            workspace_id,
            onto,
            ticket,
            seen_until_ms,
        } => checks::apply_turn_queue_seen(state, workspace_id, onto, ticket, seen_until_ms)?,
        Event::TurnQueueLeft {
            workspace_id,
            onto,
            ticket,
            ..
        } => checks::apply_turn_queue_left(state, workspace_id, onto, ticket)?,
        Event::TurnReleased {
            workspace_id,
            onto,
            turn_id,
            attempts,
        } => checks::apply_turn_released(state, workspace_id, onto, turn_id, attempts)?,
        Event::TurnRenewed {
            workspace_id,
            onto,
            turn_id,
            expires_at_ms,
        } => checks::apply_turn_renewed(state, workspace_id, onto, turn_id, expires_at_ms)?,
        Event::TurnExpired {
            workspace_id,
            onto,
            turn_id,
        } => checks::apply_turn_expired(state, workspace_id, onto, turn_id)?,
        Event::OperationObserved { observation } => {
            observation::apply_operation_observed(state, observation)?
        }
        Event::WorkspaceGitObserved { git } => {
            observation::apply_workspace_git_observed(state, git)?
        }
        Event::ScanFailed { failure } => observation::apply_scan_failed(state, failure)?,
        Event::ScanRecovered { workspace_id } => {
            observation::apply_scan_recovered(state, workspace_id)?
        }
        Event::ScanCoverageObserved {
            workspace_id,
            coverage,
        } => observation::apply_scan_coverage_observed(state, workspace_id, coverage)?,
        Event::HostNotInstalled { host } => observation::apply_host_not_installed(state, host)?,
        Event::HostCleared { host } => observation::apply_host_cleared(state, host)?,
        Event::CheckoutChanged { change } => observation::apply_checkout_changed(state, change)?,
        Event::CheckoutRunObserved { run } => observation::apply_checkout_run_observed(state, run)?,
        Event::UncoordinatedChanged { change } => {
            observation::apply_uncoordinated_changed(state, change)?
        }
        Event::UncoordinatedDelivered {
            workspace_id,
            change_id,
            session_id,
        } => {
            observation::apply_uncoordinated_delivered(state, workspace_id, change_id, session_id)?
        }
        Event::DiskObserved { observation } => {
            observation::apply_disk_observed(state, observation)?
        }
        Event::BranchObserved { observation } => {
            observation::apply_branch_observed(state, observation)?
        }
        Event::InterfaceChanged { notice } => notices::apply_interface_changed(state, notice)?,
        Event::InterfaceDependentsFound {
            notice_workspace_id,
            notice_id,
            files,
        } => {
            notices::apply_interface_dependents_found(state, notice_workspace_id, notice_id, files)?
        }
        Event::InterfaceAlerted {
            session_id,
            notice_ids,
        } => notices::apply_interface_alerted(state, session_id, notice_ids)?,
        Event::InterfaceNoticesTold {
            session_id,
            notice_ids,
            via,
        } => notices::apply_interface_notices_told(state, session_id, notice_ids, via)?,
        Event::ReadOutlined {
            session_id,
            path,
            digest,
        } => notices::apply_read_outlined(state, session_id, path, digest)?,
        Event::InflightDelivered { session_id, keys } => {
            notices::apply_inflight_delivered(state, session_id, keys)?
        }
        Event::WorkspaceRegistered { workspace } => {
            sessions::apply_workspace_registered(state, workspace)?
        }
        Event::WorkspaceMoved {
            workspace,
            from_root,
        } => workspaces::apply_moved(state, workspace, from_root)?,
        Event::WorkspaceRemoved { workspace_id } => {
            workspaces::apply_removed(state, workspace_id)?;
        }
        Event::SessionRegistered { session } => sessions::apply_session_registered(state, session)?,
        Event::SessionLivenessObserved { liveness } => {
            sessions::apply_session_liveness_observed(state, liveness)?
        }
        Event::SessionSeen {
            session_id,
            at_ms,
            generation,
        } => sessions::apply_session_seen(state, session_id, at_ms, generation)?,
        Event::IntentSet { intent } => sessions::apply_intent_set(state, intent)?,
        Event::IntentEnded { session_id } => sessions::apply_intent_ended(state, session_id)?,
        Event::SessionPrompted {
            session_id,
            summary,
            line_digests,
            at_ms,
            generation,
        } => sessions::apply_session_prompted(
            state,
            session_id,
            summary,
            line_digests,
            at_ms,
            generation,
        )?,
        Event::PromptTargetsSet {
            session_id,
            paths,
            at_ms,
            generation,
        } => sessions::apply_prompt_targets_set(state, session_id, paths, at_ms, generation)?,
        Event::PresenceObserved {
            session_id,
            kind,
            paths,
            at_ms,
            generation,
        } => sessions::apply_presence_observed(state, session_id, kind, paths, at_ms, generation)?,
        Event::PresenceSet {
            session_id,
            setting,
        } => sessions::apply_presence_set(state, session_id, setting)?,
        Event::SessionEnded { session_id } => sessions::apply_session_ended(state, session_id)?,
        Event::SessionAdopted { adoption } => sessions::apply_session_adopted(state, adoption)?,
        // Boundaries in the log. The admissions a stop released ended in
        // their own `LeaseCompleted` events.
        Event::SessionStopped {
            session_id,
            failure,
            generation,
            prompt_id,
            ..
        } => sessions::apply_session_stopped(state, session_id, failure, generation, prompt_id)?,
        Event::ContextAdded {
            session_id,
            bytes,
            turn,
            generation,
        } => sessions::apply_context_added(state, session_id, bytes, turn, generation)?,
        Event::ToldForgotten { session_id, keys } => {
            sessions::apply_told_forgotten(state, session_id, keys)?
        }
        Event::WriteRefused {
            session_id,
            workspace_id,
            kind,
            paths,
            peer_session_id,
            input_digest,
            at_ms,
            generation,
        } => admission::apply_write_refused(
            state,
            session_id,
            workspace_id,
            kind,
            paths,
            peer_session_id,
            input_digest,
            at_ms,
            generation,
        )?,
        Event::WriteWaited {
            session_id,
            workspace_id,
            paths,
            holder_session_id,
            waited_ms,
            ..
        } => policy::validate_waited(
            state,
            session_id,
            workspace_id,
            paths,
            holder_session_id.as_ref(),
            *waited_ms,
        )?,
        Event::RefusalSettled {
            session_id,
            kind,
            paths,
            refused_at_ms,
            ..
        } => policy::apply_settled(state, session_id, *kind, paths, *refused_at_ms)?,
        Event::PolicySet {
            workspace_id,
            case,
            policy,
            wait_ms,
        } => policy::apply_set(state, workspace_id, *case, *policy, *wait_ms)?,
        Event::LeaseGranted { lease } => admission::apply_lease_granted(state, lease)?,
        Event::ToolBatchObserved {
            session_id,
            operation_ids,
            calls,
            generation,
            ..
        } => admission::apply_tool_batch_observed(
            state,
            session_id,
            operation_ids,
            calls,
            generation,
        )?,
        Event::SourceObserved {
            session_id,
            files,
            origin,
            at_ms,
            generation,
        } => admission::apply_source_observed(state, session_id, files, origin, at_ms, generation)?,
        Event::LeaseCompleted { lease_id } | Event::LeaseExpired { lease_id } => {
            admission::apply_lease_ended(state, event, lease_id)?
        }
        Event::TrailRecorded { trail } => briefing::apply_trail_recorded(state, trail)?,
        Event::TrailTaken {
            workspace_id,
            from,
            session_id,
        } => briefing::apply_trail_taken(state, workspace_id, from, session_id)?,
        Event::BriefingDelivered {
            session_id,
            signature,
            told,
            told_at_ms,
        } => briefing::apply_briefing_delivered(state, session_id, signature, told, told_at_ms)?,
        Event::Booted { generation } => {
            if *generation <= state.generation {
                return Err(error(
                    "invalid_event",
                    "A boot event must advance the daemon generation",
                ));
            }
            state.generation = *generation;
            // The new generation's anchor, if any, is the next event.
            state.clock_anchor = None;
            // A refusal is settled within the run it was given in.
            state.unsettled.clear();
            for lease in state.leases.values_mut() {
                if lease.status == LeaseStatus::Active {
                    lease.status = LeaseStatus::Invalidated;
                }
            }
            for liveness in state.session_liveness.values_mut() {
                liveness.status = LivenessStatus::Unknown;
            }
            // Times do not carry across generations: any edit a peer reports
            // from now on is after every read made before the restart.
            for activity in state.activity.values_mut() {
                for version in &mut activity.known {
                    version.at_ms = 0;
                }
            }
            // What teammates published is current only once received again.
            state.remote_peers.clear();
            // A landing that was under way is no longer known to be alive,
            // nor is one that was waiting.
            state.turns.clear();
            state.turn_queues.clear();
        }
        Event::ClockAnchored { anchor } => {
            if anchor.generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A clock anchor names the generation that is starting",
                ));
            }
            state.clock_anchor = Some(*anchor);
        }
    }
    Ok(())
}

/// Return stable, lease-ID ordered expiration events; do not change state.
pub fn expiry_events(state: &State, now_ms: u64) -> Vec<Event> {
    state
        .leases
        .values()
        .filter(|lease| {
            lease.status == LeaseStatus::Active
                && lease.generation == state.generation
                && lease.expires_at_ms <= now_ms
        })
        .map(|lease| Event::LeaseExpired {
            lease_id: lease.lease_id.clone(),
        })
        .chain(
            state
                .turns
                .values()
                .filter(|turn| turn.expires_at_ms <= now_ms)
                .map(|turn| Event::TurnExpired {
                    workspace_id: turn.workspace_id.clone(),
                    onto: turn.onto.clone(),
                    turn_id: turn.turn_id.clone(),
                }),
        )
        .chain(
            state
                .turn_queues
                .values()
                .flatten()
                .filter(|waiter| !still_waiting(waiter, state.generation, now_ms))
                .map(|waiter| Event::TurnQueueLeft {
                    workspace_id: waiter.workspace_id.clone(),
                    onto: waiter.onto.clone(),
                    ticket: waiter.ticket.clone(),
                    reason: if waiter.deadline_ms <= now_ms {
                        LeftQueue::TimedOut
                    } else {
                        LeftQueue::Stopped
                    },
                }),
        )
        .collect()
}
