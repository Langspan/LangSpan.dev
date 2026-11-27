//! Workspaces and sessions: registration, liveness, presence, prompts,
//! intents and adoption. Their record types and rules, and the
//! `decide_checked` and `apply` arms of their commands and events, which
//! call these functions. A workspace's later lifecycle is in `workspaces.rs`.

// The arm functions take what the arm's pattern bound, as bound (`&String`,
// `&Vec<_>`), so that each body moved out of the match unchanged.
#![allow(clippy::ptr_arg)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    CONTEXT_DEFINITIONS, CONTEXT_NAMES, Command, CoreError, Decision, Event, KnownVersion,
    LeaseStatus, MAX_STOPPED_PROMPTS, Outcome, Recipient, State, ToldNotice, TrailFacts,
    activity_mut, current_session, error, is_false, is_zero_u64, known_session, look,
    normalized_paths, normalized_workspace_links, presence_paths, presence_setting,
    prompt_target_paths, trail, validate_canonical_paths, validate_failure, validate_id,
    validate_presence_setting, validate_prompt_summary, validate_request_id,
    validate_session_liveness, validate_session_registration, validate_summary, validate_workspace,
    validate_workspace_link_targets, workspaces, wrote_since_boundary,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub workspace_id: String,
    pub root: String,
    /// Existing workspace IDs whose distinct nested worktree boundary was
    /// verified by the filesystem adapter when this workspace was registered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_overlaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub session_id: String,
    pub workspace_id: String,
    pub generation: u64,
    /// The session that started this one, for a host subagent: it shares the
    /// parent's conversation, so the two are related, not strangers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Started without a person at the terminal (`claude -p`). A headless
    /// session takes a handoff only under the rule in `takes_handoff`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub headless: bool,
}

/// An OS process incarnation, not merely a recyclable process number. The
/// adapter's start token includes both OS boot and process start identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_token: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LivenessStatus {
    Alive,
    Exited,
    Unknown,
}

/// Point-in-time process evidence. It neither reattaches a session nor changes
/// its admission authority. The timestamp is meaningful within its generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLiveness {
    pub session_id: String,
    pub process: ProcessIdentity,
    pub status: LivenessStatus,
    pub observed_at_ms: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub session_id: String,
    pub workspace_id: String,
    pub paths: Vec<String>,
    pub summary: String,
    pub generation: u64,
}

/// Immutable identity of one native tool call. The adapter hashes canonical
/// semantic input so receipts can reject changed content without storing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub operation_id: String,
    pub source: String,
    pub tool: String,
    pub input_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TouchedPath {
    pub path: String,
    pub at_ms: u64,
}

/// What one session was last seen doing. Derived from recorded events only;
/// times are meaningful within `generation`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionActivity {
    pub session_id: String,
    pub generation: u64,
    pub last_ms: u64,
    /// The last recorded hook run of any kind that reached the daemon for
    /// this session (a read, a failed call, a batch, a stop): at most one
    /// `SessionSeen` per `SESSION_SEEN_INTERVAL_MS`. Zero when none was.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub last_hook_ms: u64,
    /// The owner's own words for the current task, capped by the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Most recent successful edits, newest first, at most `MAX_TOUCHED`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched: Vec<TouchedPath>,
    /// File versions this session last read or wrote, newest first, at most
    /// `MAX_KNOWN`. A path that falls off the end is unknown again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known: Vec<KnownVersion>,
    /// Signature of the last briefing delivered to this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub briefed: Option<String>,
    /// Keys of the briefing items this session was last told, in briefing
    /// order, at most `MAX_TOLD` (issue #222). A briefing asked for with
    /// `changes_only` leaves out an item whose key is here. An item past
    /// the bound is not remembered, so it is told again: never dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub told: Vec<u64>,
    /// When a briefing last placed items in this session's context: a
    /// safety item is told again when the session read or edited one of
    /// its files after this.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub told_at_ms: u64,
    /// The context LangSpan's hooks added to this session in this
    /// generation, as they measured it (issue #222).
    #[serde(default, skip_serializing_if = "ContextUse::is_empty")]
    pub context: ContextUse,
    /// Interface notices this session was alerted to at a write, newest
    /// first, at most `MAX_ALERTED`. Each is alerted once (ADR 0029).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alerted: Vec<String>,
    /// Interface notices told to this session by a briefing or between
    /// steps, newest first, at most `MAX_TOLD_NOTICES` (issue #214).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices_told: Vec<ToldNotice>,
    /// Replies the session finished in this generation: a between-step
    /// watcher started in one turn stops once this moves (issue #214).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub stops: u64,
    /// The host's IDs of the prompts whose replies finished in this
    /// generation, newest first, at most `MAX_STOPPED_PROMPTS`: a watcher
    /// started for one of them has no turn left (issue #214).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped_prompts: Vec<String>,
    /// In-flight signatures told to this session, newest first, at most
    /// `MAX_INFLIGHT_TOLD` (roadmap C7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inflight_told: Vec<String>,
    /// The host reported the session over. Its recent edits stay relevant to
    /// peers until they age out; they are labelled as a finished session's.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ended: bool,
    /// The session wrote a file since its last tool batch or stop: the next
    /// one is a point to run the workspace's automatic check.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrote_since_boundary: bool,
    /// Where the definitions named in the session's last prompt are, from
    /// the source index (roadmap C8). Paths only; never the prompt's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_targets: Option<PromptTargets>,
    /// Where the session was last seen looking: files it read, the path
    /// scope of its searches, and paths its shell commands named. Newest
    /// first, one entry per path, at most `MAX_LOOKING` (roadmap A13). Paths
    /// only: never a search pattern or a command's text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub looking: Vec<LookedPath>,
    /// Digests of the leading non-empty lines of the session's last prompt,
    /// at most `MAX_PROMPT_LINES`: compared with live peers' to tell prompts
    /// started from one template apart (issue #114). Never the words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_digests: Vec<String>,
    /// File versions whose whole read this session was given an outline
    /// for instead (roadmap A11), newest first, one per path, at most
    /// `MAX_OUTLINED`. Its next whole read of the same version goes through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlined: Vec<OutlinedRead>,
}

/// A file version a session was given an outline of (roadmap A11).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutlinedRead {
    pub path: String,
    pub digest: String,
}

/// How many outlined file versions a session keeps. One past it is
/// forgotten, and its next whole read is outlined again.
pub const MAX_OUTLINED: usize = 64;

/// One definition in a file's outline (roadmap A11): what the source index
/// holds, without its signature or any other text of the file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutlineEntry {
    pub name: String,
    pub kind: String,
    /// One-based, inclusive.
    pub line: u32,
    pub end_line: u32,
    pub exported: bool,
}

/// How a session came to look at a path, for presence (roadmap A13).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PresenceKind {
    /// The host reported a completed read of the file.
    Read,
    /// A search (Claude Code's or Cursor's `Grep`) scoped to this path.
    Search,
    /// A shell command named this existing path as an operand.
    Shell,
}

/// One path a session looked at, and when.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LookedPath {
    pub path: String,
    pub kind: PresenceKind,
    pub at_ms: u64,
}

/// One path a peer was looking at, as a briefing shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresencePath {
    pub path: String,
    pub kind: PresenceKind,
    pub age_s: u64,
}

/// A session's own presence setting, kept only when it differs from the
/// default (shown, `PRESENCE_WINDOW_MS`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PresenceSetting {
    /// False: this session's reads, searches and shell paths are never
    /// shown to peers (`--presence off`).
    pub shown: bool,
    /// How old a peer's presence this session is still shown.
    pub window_ms: u64,
}

/// Looked-at paths kept per session.
pub const MAX_LOOKING: usize = 16;
/// Looked-at paths shown per peer, and named by one presence observation.
pub const PRESENCE_PATHS: usize = 4;
/// Presence older than this is not shown, unless the asking session chose
/// another window (at most `BRIEF_HORIZON_MS`).
pub const PRESENCE_WINDOW_MS: u64 = 10 * 60 * 1000;
/// The shortest presence window a session can choose.
pub const MIN_PRESENCE_WINDOW_MS: u64 = 60 * 1000;

/// Briefing item keys remembered per session, at most.
pub const MAX_TOLD: usize = 256;

/// The context a session's hooks added to it: every briefing, code section
/// and mid-turn note, in bytes of text placed in its context.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextUse {
    /// Turns: a session start or resume, and each prompt.
    pub turns: u64,
    /// Bytes added over those turns, mid-turn notes included.
    pub bytes: u64,
    /// The most one turn added, its own context and the notes after it.
    #[serde(default)]
    pub largest_turn_bytes: u64,
    /// Bytes added since the current turn began.
    #[serde(default)]
    pub turn_bytes: u64,
}

/// Bytes of text per token, for an estimate only: English prose and code
/// average about four bytes per token in current models' tokenizers.
pub const BYTES_PER_TOKEN: u64 = 4;

/// The most one context record may name: far above any hook's cap.
pub const MAX_CONTEXT_RECORD_BYTES: u64 = 1024 * 1024;

impl ContextUse {
    pub fn is_empty(&self) -> bool {
        self.turns == 0 && self.bytes == 0
    }
    /// Count `bytes` added, at the start of a new turn when `turn`.
    fn add(&mut self, bytes: u64, turn: bool) {
        if turn {
            self.turns = self.turns.saturating_add(1);
            self.turn_bytes = 0;
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.turn_bytes = self.turn_bytes.saturating_add(bytes);
        self.largest_turn_bytes = self.largest_turn_bytes.max(self.turn_bytes);
    }
    /// Bytes per turn on average, rounded down; zero before the first turn.
    pub fn bytes_per_turn(&self) -> u64 {
        self.bytes.checked_div(self.turns).unwrap_or(0)
    }
    /// An estimate of `bytes` in tokens (`BYTES_PER_TOKEN`), rounded up.
    pub fn estimated_tokens(bytes: u64) -> u64 {
        bytes.div_ceil(BYTES_PER_TOKEN)
    }
}

impl SessionActivity {
    /// When the session was last known to be working: its newest edit,
    /// prompt or recorded hook run. Whether it is current compares this, not
    /// `last_ms` alone, with `BRIEF_HORIZON_MS` (roadmap J12).
    pub fn last_seen_ms(&self) -> u64 {
        self.last_ms.max(self.last_hook_ms)
    }
}

/// A hook run is recorded as `SessionSeen` at most this often per session,
/// so a burst of tool calls writes one event, not one per call.
pub const SESSION_SEEN_INTERVAL_MS: u64 = 30 * 1000;

/// The session a request is made on behalf of, when it is that session at
/// work: every request a hook run sends for it, and the same requests from
/// the CLI or MCP. A look from outside (`Brief` without `deliver`, a status
/// read) and the end of a session are not. The daemon records a
/// `SessionSeen` after each such request (roadmap J12).
pub fn acting_session(command: &Command) -> Option<&str> {
    match command {
        Command::SessionRegister { session_id, .. }
        | Command::ObserveSessionLiveness { session_id, .. }
        | Command::IntentSet { session_id, .. }
        | Command::IntentEnd { session_id }
        | Command::Admit { session_id, .. }
        | Command::AdmitTool { session_id, .. }
        | Command::ObserveSource { session_id, .. }
        | Command::ObservePresence { session_id, .. }
        | Command::SetPresence { session_id, .. }
        | Command::AckMessage { session_id, .. }
        | Command::Handoff { session_id, .. }
        | Command::RecordRefusal { session_id, .. }
        | Command::RecordWait { session_id, .. }
        | Command::CheckCalls { session_id, .. }
        | Command::Complete { session_id, .. }
        | Command::Observe { session_id, .. }
        | Command::ObserveTool { session_id, .. }
        | Command::SessionPrompt { session_id, .. }
        | Command::SetPromptTargets { session_id, .. }
        | Command::SessionStop { session_id, .. }
        | Command::ObserveToolBatch { session_id, .. }
        | Command::Adopt { session_id, .. }
        | Command::Brief {
            session_id,
            deliver: true,
            ..
        } => Some(session_id),
        _ => None,
    }
}

/// Whether a hook run of `session_id` at `now_ms` is recorded: the session
/// is current and its last record is at least `SESSION_SEEN_INTERVAL_MS`
/// old, or there is none in this generation.
pub fn session_seen_due(state: &State, session_id: &str, now_ms: u64) -> bool {
    if current_session(state, session_id).is_err() {
        return false;
    }
    let last = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .map_or(0, |activity| activity.last_hook_ms);
    last == 0 || now_ms.saturating_sub(last) >= SESSION_SEEN_INTERVAL_MS
}

/// At most this many leading prompt lines are compared by digest.
pub const MAX_PROMPT_LINES: usize = 32;
/// Hex characters in one prompt line digest, at most.
pub const MAX_PROMPT_LINE_DIGEST: usize = 64;

/// The definition paths a prompt's code briefing found, and when. Compared
/// with other sessions' targets and work while within `BRIEF_HORIZON_MS`;
/// never a hold, an admission or a publication.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromptTargets {
    pub paths: Vec<String>,
    pub at_ms: u64,
}

/// At most this many prompt target paths: every definition a code briefing
/// can find, `CONTEXT_DEFINITIONS` for each of `CONTEXT_NAMES` names.
pub const MAX_PROMPT_TARGETS: usize = CONTEXT_NAMES * CONTEXT_DEFINITIONS as usize;

/// Bytes in a session's prompt summary.
pub const MAX_PROMPT_SUMMARY_BYTES: usize = 512;
/// Bytes in an intent's summary.
pub const MAX_INTENT_SUMMARY_BYTES: usize = 2048;

/// Why a session took over an ended session's uncommitted work, when
/// LangSpan can tell (issue #161). Shown, never required: the person or
/// agent decides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdoptReason {
    /// The ended session sent the adopter this message: addressed to it,
    /// or a handoff the adopter took.
    Handoff { message_id: String },
    /// Both sessions have the same task line.
    SameTask {},
}

/// One session took over an ended session's journaled, uncommitted lines
/// in the same workspace (issue #161). The line journal credits them to
/// `session_id` from then on; this record names the transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Adoption {
    /// The ended session whose lines were taken over.
    pub from: String,
    /// The session that took them over.
    pub session_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<AdoptReason>,
    /// The daemon's clock, within `generation`.
    pub at_ms: u64,
    pub generation: u64,
}

/// An ended session that left uncommitted work in the briefed session's
/// checkout and that nobody has adopted: the briefing suggests `langspan
/// adopt` (issue #161). Its task line is data from that session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeftWork {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Ended sessions one briefing names as having left work, at most.
pub const BRIEF_LEFT_WORK: usize = 4;

fn validate_line_digests(digests: &[String]) -> Result<(), CoreError> {
    if digests.len() > MAX_PROMPT_LINES
        || digests.iter().any(|digest| {
            digest.is_empty()
                || digest.len() > MAX_PROMPT_LINE_DIGEST
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(error(
            "invalid_line_digests",
            &format!(
                "Prompt line digests are at most {MAX_PROMPT_LINES} lower-case hex strings of 1 to {MAX_PROMPT_LINE_DIGEST} characters"
            ),
        ));
    }
    Ok(())
}

/// How many leading lines of `digests` the latest prompt of another live
/// session in `session_id`'s checkout or repository begins with, at most:
/// a session registered in this generation, not ended and not known to have
/// exited. Zero when the session is unknown or nobody shares a line.
fn shared_prompt_lines(state: &State, session_id: &str, digests: &[String]) -> u32 {
    let Some(me) = state.sessions.get(session_id) else {
        return 0;
    };
    let repository = |workspace_id: &str| {
        state
            .workspace_git
            .get(workspace_id)
            .map(|git| git.repository_id.as_str())
    };
    let mine = repository(&me.workspace_id);
    state
        .sessions
        .values()
        .filter(|peer| peer.session_id != session_id && peer.generation == state.generation)
        .filter(|peer| {
            peer.workspace_id == me.workspace_id
                || (mine.is_some() && repository(&peer.workspace_id) == mine)
        })
        .filter(|peer| {
            state
                .session_liveness
                .get(&peer.session_id)
                .is_none_or(|liveness| liveness.status != LivenessStatus::Exited)
        })
        .filter_map(|peer| state.activity.get(&peer.session_id))
        .filter(|activity| activity.generation == state.generation && !activity.ended)
        .map(|activity| {
            digests
                .iter()
                .zip(&activity.line_digests)
                .take_while(|(mine, theirs)| mine == theirs)
                .count()
        })
        .max()
        .unwrap_or(0) as u32
}

/// Whether a registered session is over: the host reported it ended, or its
/// process is known to have exited. A session LangSpan cannot tell is over
/// (killed while its liveness was unknown) counts as live.
pub(crate) fn finished(state: &State, session_id: &str) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.ended)
        || state
            .session_liveness
            .get(session_id)
            .is_some_and(|liveness| liveness.status == LivenessStatus::Exited)
}

/// Sessions a briefing's `left` may name, at most.
pub const MAX_LEFT: usize = 64;

/// `session_id` takes over `from`'s uncommitted work (issue #161).
pub(crate) fn adopt(
    state: &State,
    session_id: &str,
    from: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    // The adopter is the caller, at work: registered, perhaps not yet again
    // since the coordinator restarted.
    let me = known_session(state, session_id)?;
    let theirs = known_session(state, from)?;
    if session_id == from {
        return Err(error(
            "invalid_adoption",
            "A session cannot adopt its own work; name the ended session whose work to take over",
        ));
    }
    if finished(state, session_id) {
        return Err(error(
            "session_ended",
            "This session has ended; adopt from the session that continues the task",
        ));
    }
    if theirs.workspace_id != me.workspace_id {
        return Err(error(
            "other_workspace",
            &format!(
                "That session worked in workspace {:?}, not this session's {:?}; only an ended session's work in the same checkout can be adopted",
                theirs.workspace_id, me.workspace_id
            ),
        ));
    }
    if !finished(state, from) {
        return Err(error(
            "session_live",
            "That session has not ended: LangSpan has neither its end nor its process's exit. Ask it to commit its own work, or adopt once it has ended",
        ));
    }
    // An adopter that ended after its adoption was recorded, before its
    // adapter credited the lines, leaves them under the session it adopted;
    // that session cannot be adopted again. Passing the adoption on credits
    // them too: every session whose adoptions lead to `from`. A session that
    // has resumed since it was adopted owns what it writes again, so it is
    // left out, and so is what its own adoptions lead to.
    let mut earlier: Vec<String> = Vec::new();
    let mut pending = vec![from.to_owned()];
    while let Some(adopter) = pending.pop() {
        for adoption in state.adoptions.values() {
            if adoption.session_id != adopter
                || adoption.workspace_id != me.workspace_id
                || adoption.from == from
                || earlier.contains(&adoption.from)
                || !finished(state, &adoption.from)
            {
                continue;
            }
            if earlier.len() >= MAX_LEFT {
                return Err(error(
                    "adoption_chain",
                    &format!(
                        "More than {MAX_LEFT} earlier adoptions lead to that session; commit part of the work, then adopt again"
                    ),
                ));
            }
            earlier.push(adoption.from.clone());
            pending.push(adoption.from.clone());
        }
    }
    earlier.sort();
    // Ending a session does not end its subagents: one still running keeps
    // its lines.
    let live: Vec<String> = state
        .sessions
        .values()
        .filter(|session| {
            session.workspace_id == me.workspace_id
                && [from]
                    .into_iter()
                    .chain(earlier.iter().map(String::as_str))
                    .any(|source| {
                        session.parent.as_deref() == Some(source)
                            || session
                                .session_id
                                .strip_prefix(source)
                                .is_some_and(|rest| rest.starts_with('.'))
                    })
                && !finished(state, &session.session_id)
        })
        .map(|session| session.session_id.clone())
        .collect();
    if live.len() > MAX_LEFT {
        return Err(error(
            "session_live",
            &format!(
                "More than {MAX_LEFT} subagents of that session have not ended; adopt once they have"
            ),
        ));
    }
    // A subagent another session adopted is that session's to credit, even
    // when its adapter has not yet rewritten the journal.
    let mut kept: Vec<String> = state
        .adoptions
        .values()
        .filter(|adoption| {
            adoption.workspace_id == me.workspace_id
                && adoption.session_id != session_id
                && !earlier.contains(&adoption.from)
                && [from]
                    .into_iter()
                    .chain(earlier.iter().map(String::as_str))
                    .any(|source| {
                        adoption
                            .from
                            .strip_prefix(source)
                            .is_some_and(|rest| rest.starts_with('.'))
                    })
        })
        .map(|adoption| adoption.from.clone())
        .collect();
    kept.sort();
    if kept.len() > MAX_LEFT {
        return Err(error(
            "adoption_chain",
            &format!(
                "Other sessions adopted more than {MAX_LEFT} subagents of that session; commit part of the work, then adopt again"
            ),
        ));
    }
    let root = state
        .workspaces
        .get(&me.workspace_id)
        .map(|workspace| workspace.root.clone())
        .unwrap_or_default();
    if let Some(adoption) = state.adoptions.get(from) {
        if adoption.session_id != session_id {
            return Err(error(
                "already_adopted",
                &format!(
                    "Session {} already adopted that session's work; adopt from {} once it has ended",
                    adoption.session_id, adoption.session_id
                ),
            ));
        }
        return Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::Adopted {
                adoption: adoption.clone(),
                root,
                recorded: false,
                live,
                earlier,
                kept,
            },
        });
    }
    let adoption = Adoption {
        from: from.to_owned(),
        session_id: session_id.to_owned(),
        workspace_id: me.workspace_id.clone(),
        reason: adopt_reason(state, &me.workspace_id, session_id, from),
        at_ms: now_ms,
        generation: state.generation,
    };
    Ok(Decision {
        events: vec![Event::SessionAdopted {
            adoption: adoption.clone(),
        }],
        outcome: Outcome::Adopted {
            adoption,
            root,
            recorded: true,
            live,
            earlier,
            kept,
        },
    })
}

/// Why `session_id` may be continuing `from`'s task, newest evidence
/// first: a message `from` sent it (addressed to it, or a handoff it took),
/// then the same task line.
fn adopt_reason(
    state: &State,
    workspace_id: &str,
    session_id: &str,
    from: &str,
) -> Option<AdoptReason> {
    let held = state.messages.get(workspace_id).into_iter().flatten();
    let history = state
        .message_history
        .get(workspace_id)
        .into_iter()
        .flatten();
    let handoff = held.chain(history).find(|message| {
        message.from.as_deref() == Some(from)
            && !message.retracted
            && match &message.to {
                Recipient::Session { session_id: to } => to == session_id,
                Recipient::NextSession {} => {
                    message.acknowledged.iter().any(|taker| taker == session_id)
                }
                Recipient::Everyone {} => false,
            }
    });
    if let Some(message) = handoff {
        return Some(AdoptReason::Handoff {
            message_id: message.message_id.clone(),
        });
    }
    let task = |id: &str| {
        state
            .activity
            .get(id)
            .and_then(|activity| activity.summary.as_deref())
    };
    (task(from).is_some() && task(from) == task(session_id)).then_some(AdoptReason::SameTask {})
}

/// Of `left`, the sessions a briefing names as having left uncommitted
/// work: of `session_id`'s workspace, over, not adopted, and neither the
/// session nor one of its own subagents.
pub(crate) fn left_work(
    state: &State,
    session_id: &str,
    left: &[String],
) -> Result<Vec<LeftWork>, CoreError> {
    if left.len() > MAX_LEFT {
        return Err(error(
            "invalid_left",
            &format!("A briefing names at most {MAX_LEFT} sessions that left work"),
        ));
    }
    for id in left {
        validate_id(id, "session ID")?;
    }
    if left.is_empty() {
        return Ok(Vec::new());
    }
    let me = known_session(state, session_id)?;
    let mut found: Vec<LeftWork> = Vec::new();
    for id in left {
        let Some(session) = state.sessions.get(id) else {
            continue;
        };
        if id == session_id
            || session.parent.as_deref() == Some(session_id)
            || session.workspace_id != me.workspace_id
            || !finished(state, id)
            || state.adoptions.contains_key(id)
            || found.iter().any(|work| &work.session_id == id)
        {
            continue;
        }
        found.push(LeftWork {
            session_id: id.clone(),
            summary: state
                .activity
                .get(id)
                .and_then(|activity| activity.summary.clone()),
        });
        if found.len() == BRIEF_LEFT_WORK {
            break;
        }
    }
    Ok(found)
}

impl Default for PresenceSetting {
    fn default() -> Self {
        Self {
            shown: true,
            window_ms: PRESENCE_WINDOW_MS,
        }
    }
}

pub(crate) fn decide_workspace_register(
    state: &State,
    command: &Command,
    workspace_id: &String,
    root: &String,
) -> Result<Decision, CoreError> {
    let verified_overlaps = if let Command::WorkspaceRegisterVerified {
        verified_overlaps, ..
    } = command
    {
        let links = normalized_workspace_links(verified_overlaps)?;
        validate_workspace_link_targets(state, workspace_id, root, &links)?;
        links
    } else {
        Vec::new()
    };
    let workspace = Workspace {
        workspace_id: workspace_id.clone(),
        root: root.clone(),
        verified_overlaps: state
            .workspaces
            .get(workspace_id)
            .map(|old| old.verified_overlaps.clone())
            .unwrap_or(verified_overlaps),
    };
    validate_workspace(state, &workspace)?;
    let events = if state.workspaces.contains_key(workspace_id) {
        Vec::new()
    } else {
        vec![Event::WorkspaceRegistered {
            workspace: workspace.clone(),
        }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::Workspace { workspace },
    })
}

pub(crate) fn decide_workspace_list(
    state: &State,
    after: &Option<String>,
) -> Result<Decision, CoreError> {
    let (workspaces, next) = workspaces::list(state, after.as_deref())?;
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::Workspaces { workspaces, next },
    })
}

pub(crate) fn decide_session_register(
    state: &State,
    session_id: &String,
    workspace_id: &String,
    parent: &Option<String>,
    headless: &bool,
) -> Result<Decision, CoreError> {
    let session = Session {
        session_id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        generation: state.generation,
        parent: parent.clone(),
        headless: *headless,
    };
    validate_session_registration(state, &session)?;
    let events = if state.sessions.get(session_id) == Some(&session) {
        Vec::new()
    } else {
        vec![Event::SessionRegistered {
            session: session.clone(),
        }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::Session { session },
    })
}

pub(crate) fn decide_observe_session_liveness(
    state: &State,
    session_id: &String,
    process: &ProcessIdentity,
    status: &LivenessStatus,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let liveness = SessionLiveness {
        session_id: session_id.clone(),
        process: process.clone(),
        status: *status,
        observed_at_ms: now_ms,
        generation: state.generation,
    };
    validate_session_liveness(state, &liveness)?;
    let events = if state.session_liveness.get(session_id) == Some(&liveness) {
        Vec::new()
    } else {
        vec![Event::SessionLivenessObserved {
            liveness: liveness.clone(),
        }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::SessionLiveness { liveness },
    })
}

pub(crate) fn decide_session_seen(
    state: &State,
    session_id: &String,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    let due = session_seen_due(state, session_id, now_ms);
    let last = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .map_or(0, |activity| activity.last_hook_ms);
    let events = if due {
        vec![Event::SessionSeen {
            session_id: session_id.clone(),
            at_ms: now_ms,
            generation: state.generation,
        }]
    } else {
        Vec::new()
    };
    Ok(Decision {
        events,
        outcome: Outcome::SessionSeen {
            session_id: session_id.clone(),
            last_hook_ms: if due { now_ms } else { last },
        },
    })
}

pub(crate) fn decide_record_context(
    state: &State,
    session_id: &String,
    bytes: &u64,
    turn: &bool,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    if *bytes > MAX_CONTEXT_RECORD_BYTES || (*bytes == 0 && !*turn) {
        return Err(error(
            "invalid_request",
            "A context record names at most 1 MiB, and zero bytes only at a turn's start",
        ));
    }
    let event = Event::ContextAdded {
        session_id: session_id.clone(),
        bytes: *bytes,
        turn: *turn,
        generation: state.generation,
    };
    let mut after = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .map(|activity| activity.context)
        .unwrap_or_default();
    after.add(*bytes, *turn);
    Ok(Decision {
        events: vec![event],
        outcome: Outcome::ContextRecorded {
            session_id: session_id.clone(),
            context: after,
        },
    })
}

pub(crate) fn decide_forget_told(
    state: &State,
    session_id: &String,
    keys: &Vec<u64>,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    if keys.len() > MAX_TOLD {
        return Err(error(
            "invalid_request",
            "Forget at most 256 briefing item keys at once",
        ));
    }
    let named: BTreeSet<u64> = keys.iter().copied().collect();
    let told: Vec<u64> = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .map(|activity| {
            activity
                .told
                .iter()
                .copied()
                .filter(|key| named.contains(key))
                .collect()
        })
        .unwrap_or_default();
    let forgotten = u32::try_from(told.len()).unwrap_or(u32::MAX);
    Ok(Decision {
        // A retry, or keys never told, record nothing.
        events: if told.is_empty() {
            Vec::new()
        } else {
            vec![Event::ToldForgotten {
                session_id: session_id.clone(),
                keys: told,
            }]
        },
        outcome: Outcome::ToldForgotten {
            session_id: session_id.clone(),
            forgotten,
        },
    })
}

pub(crate) fn decide_intent_set(
    state: &State,
    session_id: &String,
    paths: &Vec<String>,
    summary: &String,
) -> Result<Decision, CoreError> {
    let session = current_session(state, session_id)?;
    validate_summary(summary)?;
    let intent = Intent {
        session_id: session_id.clone(),
        workspace_id: session.workspace_id.clone(),
        paths: normalized_paths(paths)?,
        summary: summary.clone(),
        generation: state.generation,
    };
    let events = if state.intents.get(session_id) == Some(&intent) {
        Vec::new()
    } else {
        vec![Event::IntentSet {
            intent: intent.clone(),
        }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::Intent { intent },
    })
}

pub(crate) fn decide_intent_end(state: &State, session_id: &String) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    let events = if state.intents.contains_key(session_id) {
        vec![Event::IntentEnded {
            session_id: session_id.clone(),
        }]
    } else {
        Vec::new()
    };
    Ok(Decision {
        events,
        outcome: Outcome::IntentEnded {
            session_id: session_id.clone(),
        },
    })
}

// The summary is the adapter's one line; the digests let it learn,
// without any peer's words, whether a live peer's prompt began the
// same way, so that it can name this prompt by where it differs.
pub(crate) fn decide_session_prompt(
    state: &State,
    session_id: &String,
    summary: &String,
    line_digests: &Vec<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    validate_prompt_summary(summary)?;
    validate_line_digests(line_digests)?;
    Ok(Decision {
        events: vec![Event::SessionPrompted {
            session_id: session_id.clone(),
            summary: summary.clone(),
            line_digests: line_digests.clone(),
            at_ms: now_ms,
            generation: state.generation,
        }],
        outcome: Outcome::SessionPrompted {
            session_id: session_id.clone(),
            shared_lines: shared_prompt_lines(state, session_id, line_digests),
        },
    })
}

pub(crate) fn decide_set_prompt_targets(
    state: &State,
    session_id: &String,
    paths: &Vec<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    let paths = prompt_target_paths(paths)?;
    // Recorded again when unchanged: naming the same code again
    // renews how recent the targets are.
    Ok(Decision {
        events: vec![Event::PromptTargetsSet {
            session_id: session_id.clone(),
            paths: paths.clone(),
            at_ms: now_ms,
            generation: state.generation,
        }],
        outcome: Outcome::PromptTargets {
            session_id: session_id.clone(),
            paths,
        },
    })
}

pub(crate) fn decide_observe_presence(
    state: &State,
    session_id: &String,
    kind: &PresenceKind,
    paths: &Vec<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    let paths = presence_paths(*kind, paths)?;
    if !presence_setting(state, session_id).shown {
        return Err(error(
            "presence_off",
            "This session's presence is off; nothing it looks at is recorded",
        ));
    }
    Ok(Decision {
        events: vec![Event::PresenceObserved {
            session_id: session_id.clone(),
            kind: *kind,
            paths: paths.clone(),
            at_ms: now_ms,
            generation: state.generation,
        }],
        outcome: Outcome::PresenceObserved {
            session_id: session_id.clone(),
            paths: u32::try_from(paths.len()).unwrap_or(u32::MAX),
        },
    })
}

pub(crate) fn decide_set_presence(
    state: &State,
    session_id: &String,
    shown: &bool,
    window_ms: &u64,
) -> Result<Decision, CoreError> {
    known_session(state, session_id)?;
    let setting = PresenceSetting {
        shown: *shown,
        window_ms: *window_ms,
    };
    validate_presence_setting(&setting)?;
    let events = if presence_setting(state, session_id) == setting {
        Vec::new()
    } else {
        vec![Event::PresenceSet {
            session_id: session_id.clone(),
            setting,
        }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::Presence {
            session_id: session_id.clone(),
            setting,
        },
    })
}

pub(crate) fn decide_session_end(
    state: &State,
    session_id: &String,
    trail: &Option<TrailFacts>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let session = known_session(state, session_id)?;
    if let Some(facts) = trail {
        trail::validate_facts(facts)?;
    }
    let ending = state
        .activity
        .get(session_id)
        .is_none_or(|activity| !activity.ended);
    let mut events = if ending || state.intents.contains_key(session_id) {
        vec![Event::SessionEnded {
            session_id: session_id.clone(),
        }]
    } else {
        Vec::new()
    };
    // A trail is built once per end, from the state before it.
    if ending && let Some(trail) = trail::build(state, session, trail.as_ref(), now_ms) {
        events.push(Event::TrailRecorded { trail });
    }
    Ok(Decision {
        events,
        outcome: Outcome::SessionEnded {
            session_id: session_id.clone(),
        },
    })
}

pub(crate) fn decide_session_stop(
    state: &State,
    session_id: &String,
    failure: &Option<String>,
    prompt_id: &Option<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    known_session(state, session_id)?;
    if let Some(failure) = failure {
        validate_failure(failure)?;
    }
    if let Some(prompt_id) = prompt_id {
        validate_request_id(prompt_id)?;
    }
    // Only what the session's own tool calls hold, in this
    // generation, and not already past its deadline (that is the
    // expiry sweep's). An admission asked for by hand is not part of
    // a reply and stays until it is completed or expires.
    let released: Vec<String> = state
        .leases
        .values()
        .filter(|lease| {
            lease.session_id == *session_id
                && lease.status == LeaseStatus::Active
                && lease.generation == state.generation
                && lease.operation.is_some()
                && lease.expires_at_ms > now_ms
        })
        .map(|lease| lease.lease_id.clone())
        .collect();
    let mut events: Vec<Event> = released
        .iter()
        .map(|lease_id| Event::LeaseCompleted {
            lease_id: lease_id.clone(),
        })
        .collect();
    events.push(Event::SessionStopped {
        session_id: session_id.clone(),
        released: released.clone(),
        failure: failure.clone(),
        at_ms: now_ms,
        generation: state.generation,
        prompt_id: prompt_id.clone(),
    });
    Ok(Decision {
        events,
        outcome: Outcome::SessionStopped {
            session_id: session_id.clone(),
            released,
            wrote: wrote_since_boundary(state, session_id),
        },
    })
}

pub(crate) fn apply_workspace_registered(
    state: &mut State,
    workspace: &Workspace,
) -> Result<(), CoreError> {
    validate_workspace(state, workspace)?;
    state
        .workspaces
        .insert(workspace.workspace_id.clone(), workspace.clone());
    Ok(())
}

pub(crate) fn apply_session_registered(
    state: &mut State,
    session: &Session,
) -> Result<(), CoreError> {
    validate_session_registration(state, session)?;
    // A new registration, or one in another workspace, has written
    // nothing yet; a daemon restart's registers again in place.
    if state
        .sessions
        .get(&session.session_id)
        .is_none_or(|old| old.workspace_id != session.workspace_id)
    {
        state.first_writes.remove(&session.session_id);
    }
    state
        .sessions
        .insert(session.session_id.clone(), session.clone());
    Ok(())
}

pub(crate) fn apply_session_liveness_observed(
    state: &mut State,
    liveness: &SessionLiveness,
) -> Result<(), CoreError> {
    validate_session_liveness(state, liveness)?;
    state
        .session_liveness
        .insert(liveness.session_id.clone(), liveness.clone());
    Ok(())
}

pub(crate) fn apply_session_seen(
    state: &mut State,
    session_id: &String,
    at_ms: &u64,
    generation: &u64,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if *generation != state.generation {
        return Err(error(
            "invalid_event",
            "A hook-run record must belong to the current daemon generation",
        ));
    }
    let activity = activity_mut(state, session_id);
    activity.last_hook_ms = activity.last_hook_ms.max(*at_ms);
    Ok(())
}

pub(crate) fn apply_intent_set(state: &mut State, intent: &Intent) -> Result<(), CoreError> {
    let session = current_session(state, &intent.session_id)?;
    if intent.workspace_id != session.workspace_id || intent.generation != state.generation {
        return Err(error(
            "invalid_event",
            "Intent must match its session's workspace and current generation",
        ));
    }
    validate_canonical_paths(&intent.paths)?;
    validate_summary(&intent.summary)?;
    state
        .intents
        .insert(intent.session_id.clone(), intent.clone());
    Ok(())
}

pub(crate) fn apply_intent_ended(state: &mut State, session_id: &String) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if !state.intents.contains_key(session_id) {
        return Err(error(
            "invalid_event",
            "Intent-ended event references no existing intent",
        ));
    }
    state.intents.remove(session_id);
    Ok(())
}

pub(crate) fn apply_session_prompted(
    state: &mut State,
    session_id: &String,
    summary: &String,
    line_digests: &Vec<String>,
    at_ms: &u64,
    generation: &u64,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    validate_prompt_summary(summary)?;
    validate_line_digests(line_digests)?;
    if *generation != state.generation {
        return Err(error(
            "invalid_event",
            "A prompt record must belong to the current daemon generation",
        ));
    }
    let activity = activity_mut(state, session_id);
    activity.summary = Some(summary.clone());
    activity.line_digests = line_digests.clone();
    activity.last_ms = activity.last_ms.max(*at_ms);
    Ok(())
}

pub(crate) fn apply_prompt_targets_set(
    state: &mut State,
    session_id: &String,
    paths: &Vec<String>,
    at_ms: &u64,
    generation: &u64,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if prompt_target_paths(paths)? != *paths {
        return Err(error(
            "invalid_event",
            "Prompt targets must be recorded sorted and without repeats",
        ));
    }
    if *generation != state.generation {
        return Err(error(
            "invalid_event",
            "A prompt targets record must belong to the current daemon generation",
        ));
    }
    let activity = activity_mut(state, session_id);
    activity.prompt_targets = Some(PromptTargets {
        paths: paths.clone(),
        at_ms: *at_ms,
    });
    activity.last_ms = activity.last_ms.max(*at_ms);
    Ok(())
}

pub(crate) fn apply_presence_observed(
    state: &mut State,
    session_id: &String,
    kind: &PresenceKind,
    paths: &Vec<String>,
    at_ms: &u64,
    generation: &u64,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if presence_paths(*kind, paths)? != *paths || *generation != state.generation {
        return Err(error(
            "invalid_event",
            "A presence record names sorted, unique paths in the current generation",
        ));
    }
    // Recorded only while shown; a record kept from before the
    // session turned presence off adds nothing.
    if presence_setting(state, session_id).shown {
        look(activity_mut(state, session_id), *kind, paths, *at_ms);
    }
    Ok(())
}

pub(crate) fn apply_presence_set(
    state: &mut State,
    session_id: &String,
    setting: &PresenceSetting,
) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    validate_presence_setting(setting)?;
    if !setting.shown {
        // Off forgets what was recorded: turning it on again later
        // never shows a path looked at while it was off or before.
        if let Some(activity) = state.activity.get_mut(session_id) {
            activity.looking.clear();
        }
    }
    if *setting == PresenceSetting::default() {
        state.presence.remove(session_id);
    } else {
        state.presence.insert(session_id.clone(), *setting);
    }
    Ok(())
}

pub(crate) fn apply_session_ended(state: &mut State, session_id: &String) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    activity_mut(state, session_id).ended = true;
    state.intents.remove(session_id);
    Ok(())
}

pub(crate) fn apply_session_adopted(
    state: &mut State,
    adoption: &Adoption,
) -> Result<(), CoreError> {
    let adopter = known_session(state, &adoption.session_id)?;
    let ended = known_session(state, &adoption.from)?;
    if adoption.from == adoption.session_id
        || adopter.workspace_id != adoption.workspace_id
        || ended.workspace_id != adoption.workspace_id
    {
        return Err(error(
            "invalid_event",
            "An adoption names two different sessions of its workspace",
        ));
    }
    if let Some(AdoptReason::Handoff { message_id }) = &adoption.reason {
        validate_request_id(message_id)?;
    }
    state
        .adoptions
        .insert(adoption.from.clone(), adoption.clone());
    Ok(())
}

// Boundaries in the log. The admissions a stop released ended in
// their own `LeaseCompleted` events.
pub(crate) fn apply_session_stopped(
    state: &mut State,
    session_id: &String,
    failure: &Option<String>,
    generation: &u64,
    prompt_id: &Option<String>,
) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    if let Some(failure) = failure {
        validate_failure(failure)?;
    }
    if let Some(prompt_id) = prompt_id {
        validate_request_id(prompt_id)?;
    }
    if *generation != state.generation {
        return Err(error(
            "invalid_event",
            "A stop record must belong to the current daemon generation",
        ));
    }
    // Only the mark and the count: a boundary does not bring an
    // ended session back or start an activity record.
    if let Some(activity) = state.activity.get_mut(session_id) {
        activity.wrote_since_boundary = false;
        activity.stops = activity.stops.saturating_add(1);
        if let Some(prompt_id) = prompt_id {
            let stopped = &mut activity.stopped_prompts;
            stopped.retain(|held| held != prompt_id);
            stopped.insert(0, prompt_id.clone());
            stopped.truncate(MAX_STOPPED_PROMPTS);
        }
    }
    Ok(())
}

pub(crate) fn apply_context_added(
    state: &mut State,
    session_id: &String,
    bytes: &u64,
    turn: &bool,
    generation: &u64,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if *generation != state.generation || *bytes > MAX_CONTEXT_RECORD_BYTES {
        return Err(error(
            "invalid_event",
            "A context record belongs to the current daemon generation and names at most 1 MiB",
        ));
    }
    activity_mut(state, session_id).context.add(*bytes, *turn);
    Ok(())
}

pub(crate) fn apply_told_forgotten(
    state: &mut State,
    session_id: &String,
    keys: &Vec<u64>,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if keys.len() > MAX_TOLD {
        return Err(error(
            "invalid_event",
            "A forget names at most 256 briefing item keys",
        ));
    }
    activity_mut(state, session_id)
        .told
        .retain(|key| !keys.contains(key));
    Ok(())
}
