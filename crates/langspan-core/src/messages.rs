//! Messages and notes: messages, handoff, retraction, notes, anchors and
//! recaps: their records, rules, decisions and state changes. `decide` and
//! `apply` in `lib.rs` call the `decide_*` and `apply_*` functions here.

use crate::*;

/// When an event that has no time of its own was decided: the daemon's
/// clock and the generation it belongs to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub at_ms: u64,
    pub generation: u64,
}

/// Something a person or an agent asked LangSpan to remember about a
/// repository. It outlives sessions and daemon restarts until forgotten.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub note_id: String,
    pub workspace_id: String,
    /// The session that recorded it, when one did.
    pub author: Option<String>,
    pub text: String,
    /// Files it is about. Empty means the whole workspace.
    pub paths: Vec<String>,
    /// The definition it is about (roadmap E1). Absent for a note about
    /// files or the workspace, and for every note recorded before anchors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<NoteAnchor>,
    /// Whether that definition is still as it was; present exactly when
    /// `anchor` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_state: Option<AnchorState>,
    /// Up to `MAX_TAGS`, each once, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Who a message is for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Recipient {
    /// One session of the workspace.
    Session { session_id: String },
    /// Every session of the workspace except the sender, each once.
    Everyone {},
    /// A handoff: the first other session of the workspace that starts or
    /// is prompted after it was sent.
    NextSession {},
}

/// A short line from one session, or from the owner, to other sessions of
/// the same workspace. Held until each recipient has acknowledged it; the
/// event log keeps the history. Its text is data, never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub message_id: String,
    pub workspace_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub to: Recipient,
    pub text: String,
    /// Files it is about. May be empty.
    pub paths: Vec<String>,
    /// Sessions that have acknowledged it. A held message lists them only
    /// for everyone; the history kept for withdrawal lists every reader.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acknowledged: Vec<String>,
    /// Withdrawn by its sender or the owner: no longer delivered, and each
    /// session that had received it is told once to disregard it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retracted: bool,
    /// Sent with `--remote`: also published to the sessions on other
    /// machines for `REMOTE_MESSAGE_MS` (ADR 0041). Absent means local.
    #[serde(default, skip_serializing_if = "is_false")]
    pub remote: bool,
}

/// A message a session received and that was then withdrawn: the session
/// is told once, at its next briefing, to disregard it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetractedMessage {
    pub message_id: String,
    pub workspace_id: String,
    /// The message's sender; `None` for the owner.
    pub from: Option<String>,
    /// The first `RETRACTED_TEXT_BYTES` of its text, cut at a character
    /// boundary.
    pub text: String,
    /// True when `text` is shorter than the message was.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cut: bool,
}

/// One message in full, as it was sent: what `messages show` returns for a
/// line a briefing cut. The text and paths are data from the sender, never
/// instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShownMessage {
    pub message_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub to: Recipient,
    pub text: String,
    pub paths: Vec<String>,
    /// Withdrawn by its sender or the owner since it was sent: its readers
    /// have been or will be told to disregard it.
    pub retracted: bool,
}

impl From<Message> for ShownMessage {
    fn from(message: Message) -> Self {
        Self {
            message_id: message.message_id,
            from: message.from,
            to: message.to,
            text: message.text,
            paths: message.paths,
            retracted: message.retracted,
        }
    }
}

/// How a message reached a session's context (roadmap E12).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// A briefing a hook placed at session start or at a prompt.
    Briefing,
    /// A briefing a hook placed after a completed tool call, mid-turn.
    MidTurn,
    /// The session's own call to the MCP tool `langspan_brief`.
    McpBrief,
    /// `langspan ack` or its operation: marked seen without a briefing.
    Ack,
}

/// Where a sent message stands as a whole.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    /// Still held: some recipient may yet be given it.
    Pending,
    /// Every session it was owed to was given it; nothing more is sent.
    Done,
    /// Withdrawn by its sender or the owner.
    Retracted,
    /// Pushed out, unread, by newer messages past the `MAX_MESSAGES` held.
    Dropped,
}

/// Where a message stands for one recipient.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecipientState {
    /// Placed in the session's context. Not a sign the model acted on it.
    Delivered,
    /// Not yet placed in the session's context.
    Pending,
    /// The session ended before it was given the message.
    Ended,
}

/// One recipient of a message and whether it was given it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipientStatus {
    pub session_id: String,
    pub status: RecipientState,
    /// Since delivery, on this daemon's clock; `None` before delivery or
    /// when it was recorded before the daemon last started or before
    /// roadmap E12.
    pub age_ms: Option<u64>,
    /// `None` before delivery or in a record from before roadmap E12.
    pub via: Option<Delivery>,
}

/// Recipients a status lists at most; the rest are counted.
pub const STATUS_RECIPIENTS: usize = 64;

/// What `delivered` means, carried in every status reply.
pub const DELIVERED_MEANS: &str =
    "Delivered means placed in the session's context, not that it acted on it.";

/// Who was given a sent message: what `messages status` answers its sender
/// (roadmap E12). Read-only: asking records nothing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageStatus {
    pub message_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub to: Recipient,
    /// Since it was sent, on this daemon's clock; `None` when that is not
    /// known (sent before the daemon last started, or the record is gone).
    pub sent_age_ms: Option<u64>,
    pub state: MessageState,
    /// Delivered sessions first, in delivery order, then the rest by ID; at
    /// most `STATUS_RECIPIENTS`. A pending handoff has none: no session has
    /// taken it.
    pub recipients: Vec<RecipientStatus>,
    /// Recipients past `STATUS_RECIPIENTS`, counted and not listed.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub more_recipients: u64,
    /// Part of the message's record is no longer in the event log
    /// (retention removed it): the answer is what remains.
    #[serde(default, skip_serializing_if = "is_false")]
    pub partial: bool,
    /// `DELIVERED_MEANS`.
    pub delivered_means: String,
}

/// A message's sent record as the event log holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentRecord {
    pub message: Message,
    pub at_ms: Option<u64>,
    pub generation: Option<u64>,
}

/// One `message_acknowledged` record from the event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckRecord {
    pub session_id: String,
    pub via: Option<Delivery>,
    pub at_ms: Option<u64>,
    pub generation: Option<u64>,
}

/// The message `message_id` of the workspace, or with no ID the newest one
/// `from` sent (the owner's when `from` is `None`), for a status query. The
/// workspace and IDs are validated here; the message's own records are
/// read from the event log by the caller.
pub fn status_message_id(
    state: &State,
    workspace_id: &str,
    from: Option<&str>,
    message_id: Option<&str>,
) -> Result<String, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if let Some(from) = from {
        validate_id(from, "session ID")?;
    }
    match message_id {
        Some(id) => {
            validate_request_id(id)?;
            Ok(id.to_owned())
        }
        None => sent_message(state, workspace_id, from, None)
            .map(|message| message.message_id.clone())
            .ok_or_else(|| {
                error(
                    "unknown_message",
                    "This sender has none of the latest 64 messages sent in this workspace; ask about an older one by its ID",
                )
            }),
    }
}

/// Who was given message `message_id`: from the state's held messages and
/// history, and from its records in the event log, `sent` (absent when not
/// kept) and `acks` (in log order, after `sent`). `retracted` says a later
/// `message_retracted` record exists. `None` when neither the state nor
/// the log knows the message.
pub fn message_status(
    state: &State,
    workspace_id: &str,
    message_id: &str,
    sent: Option<&SentRecord>,
    acks: &[AckRecord],
    retracted: bool,
    now_ms: u64,
) -> Option<MessageStatus> {
    fn find<'a>(
        lists: &'a BTreeMap<String, Vec<Message>>,
        workspace_id: &str,
        message_id: &str,
    ) -> Option<&'a Message> {
        lists
            .get(workspace_id)
            .and_then(|list| list.iter().find(|message| message.message_id == message_id))
    }
    let held = find(&state.messages, workspace_id, message_id);
    let record = find(&state.message_history, workspace_id, message_id);
    let message = sent.map(|sent| &sent.message).or(record).or(held)?.clone();
    let age = |at_ms: Option<u64>, generation: Option<u64>| match (at_ms, generation) {
        (Some(at_ms), Some(generation)) if generation == state.generation => {
            Some(now_ms.saturating_sub(at_ms))
        }
        _ => None,
    };
    // Every reader the log and the state know, first delivery first.
    let mut delivered: Vec<RecipientStatus> = Vec::new();
    for ack in acks {
        if !delivered
            .iter()
            .any(|seen| seen.session_id == ack.session_id)
        {
            delivered.push(RecipientStatus {
                session_id: ack.session_id.clone(),
                status: RecipientState::Delivered,
                age_ms: age(ack.at_ms, ack.generation),
                via: ack.via,
            });
        }
    }
    for reader in record
        .into_iter()
        .chain(held)
        .flat_map(|message| &message.acknowledged)
    {
        if !delivered.iter().any(|seen| &seen.session_id == reader) {
            delivered.push(RecipientStatus {
                session_id: reader.clone(),
                status: RecipientState::Delivered,
                age_ms: None,
                via: None,
            });
        }
    }
    let dropped = state
        .dropped_messages
        .get(workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut partial = sent.is_none();
    let message_state = if retracted || record.is_some_and(|record| record.retracted) {
        MessageState::Retracted
    } else if held.is_some() {
        MessageState::Pending
    } else if record.is_some() {
        // The history outlasts every drop: a message the cap pushed out of
        // the held list was already out of the history.
        MessageState::Done
    } else if dropped.iter().any(|id| id == message_id) {
        MessageState::Dropped
    } else {
        match message.to {
            // These leave the held list only when taken, withdrawn or dropped.
            Recipient::Session { .. } | Recipient::NextSession {} => {
                if delivered.is_empty() {
                    MessageState::Dropped
                } else {
                    MessageState::Done
                }
            }
            Recipient::Everyone {} => {
                // The drop list keeps the latest `MAX_MESSAGES` drops; once
                // full, an older drop cannot be told from a completion.
                if dropped.len() >= MAX_MESSAGES {
                    partial = true;
                }
                MessageState::Done
            }
        }
    };
    let ended = |session_id: &str| {
        state
            .activity
            .get(session_id)
            .is_some_and(|activity| activity.ended)
    };
    let mut waiting: Vec<RecipientStatus> = Vec::new();
    let mut not_given = |session_id: &str| {
        if !delivered.iter().any(|seen| seen.session_id == session_id) {
            waiting.push(RecipientStatus {
                session_id: session_id.to_owned(),
                status: if ended(session_id) {
                    RecipientState::Ended
                } else {
                    RecipientState::Pending
                },
                age_ms: None,
                via: None,
            });
        }
    };
    match &message.to {
        Recipient::Session { session_id } => not_given(session_id),
        // Each current session it is still owed to. Once done, a session
        // that joined later was never owed it.
        Recipient::Everyone {} if message_state != MessageState::Done => {
            for session in state.sessions.values() {
                if session.workspace_id == workspace_id
                    && session.generation == state.generation
                    && message.from.as_deref() != Some(session.session_id.as_str())
                {
                    not_given(&session.session_id);
                }
            }
        }
        Recipient::Everyone {} | Recipient::NextSession {} => {}
    }
    waiting.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    let mut recipients = delivered;
    recipients.append(&mut waiting);
    let more_recipients = recipients.len().saturating_sub(STATUS_RECIPIENTS) as u64;
    recipients.truncate(STATUS_RECIPIENTS);
    Some(MessageStatus {
        message_id: message.message_id,
        from: message.from,
        to: message.to,
        sent_age_ms: sent.and_then(|sent| age(sent.at_ms, sent.generation)),
        state: message_state,
        recipients,
        more_recipients,
        partial,
        delivered_means: DELIVERED_MEANS.into(),
    })
}

/// One session in a recap: what it is and what it last did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecapSession {
    pub session_id: String,
    /// Registered since the daemon last started.
    pub current: bool,
    pub ended: bool,
    pub liveness: Option<LivenessStatus>,
    pub summary: Option<String>,
    pub intent: Option<String>,
    pub editing: Vec<String>,
    pub recent: Vec<RecentPath>,
    /// The session that took over this ended session's uncommitted work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted_by: Option<String>,
    /// Ended sessions whose uncommitted work this session took over, at
    /// most `RECAP_ITEMS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adopted: Vec<String>,
}

/// A bounded account of one workspace: who is here, what changed, what is
/// remembered, what evidence exists and what LangSpan cannot currently see.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Recap {
    pub workspace_id: String,
    pub root: String,
    pub branch_ref: Option<String>,
    pub sessions: Vec<RecapSession>,
    pub omitted_sessions: u32,
    /// Items left out after the per-list caps so the whole recap fits one
    /// transport frame. Counted, never silently dropped.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_items: u32,
    pub interface_changes: Vec<InterfaceNotice>,
    pub checks: Vec<CheckRecord>,
    pub landings: Vec<Landing>,
    pub notes: Vec<Note>,
    /// Messages still owed to at least one session, newest first, then
    /// recently withdrawn ones (`retracted`), newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// The newest trails ended sessions left here, at most `RECAP_TRAILS`
    /// (roadmap C9), whether or not a session was told them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trails: Vec<TrailView>,
    pub teammates: Vec<RemotePeer>,
    pub publishing: Option<RemoteLink>,
    /// Present when the last scan attempt failed.
    pub scan_failure: Option<ScanFailure>,
    /// Present when the last completed scan left something unversioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_coverage: Option<ScanCoverage>,
    /// Asked for with `since` (roadmap C10): what happened after that point.
    /// Absent when asked for "since away" and no such gap was found.
    /// Capability `recap_since`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Since>,
}

pub const MAX_NOTES: usize = 64;
pub const NOTE_BYTES: usize = 400;
pub const RECAP_SESSIONS: usize = 16;
const RECAP_ITEMS: usize = 8;
/// Trails in a recap; `langspan trail` lists up to `MAX_TRAILS`.
pub const RECAP_TRAILS: usize = 4;

/// Unacknowledged messages held per workspace; the oldest go first.
pub const MAX_MESSAGES: usize = 64;
pub const MESSAGE_BYTES: usize = 400;
/// Messages one briefing carries at most.
pub const BRIEF_MESSAGES: usize = 4;
/// The longest line an adapter renders for one message, before its
/// ellipsis. Selection below assumes adapters cut there.
pub const MESSAGE_LINE_LIMIT: usize = 700;
/// Rendered bytes the messages of one briefing may take together, so that
/// every message selected for a briefing fits its text and none is
/// acknowledged without being shown.
pub const MESSAGE_BRIEF_BYTES: usize = 1500;

/// Bytes of a withdrawn message's text quoted when its readers are told.
pub const RETRACTED_TEXT_BYTES: usize = 120;

fn validate_note(note: &Note) -> Result<(), CoreError> {
    validate_request_id(&note.note_id)?;
    if note.text.trim().is_empty()
        || note.text.len() > NOTE_BYTES
        || note.text.chars().any(char::is_control)
        || note.paths.len() > BRIEF_PATHS
        || !anchors::valid_note_anchor(note)
    {
        return Err(error(
            "invalid_note",
            "A note is one line of 1 to 400 bytes about at most 8 files",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sent(
    state: &State,
    request_id: &str,
    now_ms: u64,
    workspace_id: &str,
    (from, to): (Option<String>, Recipient),
    text: &str,
    paths: &[String],
    remote: bool,
) -> Result<Decision, CoreError> {
    let generation = state.generation;
    let message = Message {
        message_id: request_id.into(),
        workspace_id: workspace_id.into(),
        from,
        to,
        text: text.into(),
        paths: if paths.is_empty() {
            Vec::new()
        } else {
            normalized_paths(paths)?
        },
        acknowledged: Vec::new(),
        retracted: false,
        remote,
    };
    validate_message(&message)?;
    if remote {
        remote::remote_send(state, &message, now_ms)?;
    }
    Ok(Decision {
        events: vec![Event::MessageSent {
            message: message.clone(),
            at_ms: Some(now_ms),
            generation: Some(generation),
        }],
        outcome: Outcome::MessageSent { message },
    })
}

/// The sent message `message_id` of the workspace, or with no ID the newest
/// one `from` sent. The history is the latest messages sent, newest first,
/// and lists every reader, so it answers first. A message older than the
/// history but still held is found by its ID only: the sender's newest
/// message is in the history if any of its messages is, so `last` never
/// reaches past a newer message that was dropped.
fn sent_message<'a>(
    state: &'a State,
    workspace_id: &str,
    from: Option<&str>,
    message_id: Option<&str>,
) -> Option<&'a Message> {
    let list = |lists: &'a BTreeMap<String, Vec<Message>>| {
        lists.get(workspace_id).map(Vec::as_slice).unwrap_or(&[])
    };
    match message_id {
        Some(id) => list(&state.message_history)
            .iter()
            .chain(list(&state.messages))
            .find(|message| message.message_id == id),
        None => list(&state.message_history)
            .iter()
            .find(|message| message.from.as_deref() == from),
    }
}

pub(crate) fn retract(
    state: &State,
    workspace_id: &str,
    from: Option<&str>,
    message_id: Option<&str>,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if let Some(id) = message_id {
        validate_request_id(id)?;
    }
    if let Some(from) = from
        && known_session(state, from)?.workspace_id != workspace_id
    {
        return Err(error(
            "sender_not_in_workspace",
            "The withdrawing session belongs to another workspace; withdraw from the session that sent it, or leave out the sender to withdraw as the owner",
        ));
    }
    let Some(message) = sent_message(state, workspace_id, from, message_id) else {
        return Err(error(
            "unknown_message",
            match message_id {
                Some(_) => {
                    "No such message in this workspace: unknown, or dropped as the oldest of more than 64; `langspan recap WORKSPACE` lists those held"
                }
                None => {
                    "This sender has none of the latest 64 messages sent in this workspace; withdraw an older one still held by its ID"
                }
            },
        ));
    };
    if from.is_some() && message.from.as_deref() != from {
        return Err(error(
            "not_sender",
            "Only the session that sent a message, or the owner (without a session), may withdraw it",
        ));
    }
    if message.retracted {
        return Err(error(
            "already_retracted",
            "That message was already withdrawn; its readers have been or will be told",
        ));
    }
    let held = state.messages.get(workspace_id).and_then(|messages| {
        messages
            .iter()
            .find(|held| held.message_id == message.message_id)
    });
    let withdrawn_unread: Vec<String> = match held.map(|held| (held, &held.to)) {
        Some((_, Recipient::Session { session_id })) => vec![session_id.clone()],
        Some((held, Recipient::Everyone {})) => state
            .sessions
            .values()
            .filter(|session| {
                session.workspace_id == workspace_id
                    && session.generation == state.generation
                    && owed_to(held, &session.session_id, false)
            })
            .map(|session| session.session_id.clone())
            .collect(),
        Some((_, Recipient::NextSession {})) | None => Vec::new(),
    };
    Ok(Decision {
        events: vec![Event::MessageRetracted {
            workspace_id: workspace_id.into(),
            message_id: message.message_id.clone(),
            by: from.map(str::to_owned),
        }],
        outcome: Outcome::MessageRetracted {
            message_id: message.message_id.clone(),
            withdrawn_unread,
            told_to_disregard: readers(state, workspace_id, &message.message_id),
        },
    })
}

/// Every session that received the message, in ID order.
fn readers(state: &State, workspace_id: &str, message_id: &str) -> Vec<String> {
    let mut readers: Vec<String> = sent_message(state, workspace_id, None, Some(message_id))
        .map(|message| message.acknowledged.clone())
        .unwrap_or_default();
    readers.sort();
    readers.dedup();
    readers
}

/// The first `RETRACTED_TEXT_BYTES` of `text`, at a character boundary, and
/// whether anything was left out.
fn retracted_text(text: &str) -> (String, bool) {
    let mut end = text.len().min(RETRACTED_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), end < text.len())
}

/// An upper bound on the bytes an adapter renders to tell a session to
/// disregard a withdrawn message, counted like `message_cost`.
pub(crate) fn retracted_cost(retracted: &RetractedMessage) -> usize {
    (192 + 4 * retracted.text.len()).min(MESSAGE_LINE_LIMIT + 4)
}

fn validate_message(message: &Message) -> Result<(), CoreError> {
    validate_request_id(&message.message_id)?;
    if message.text.trim().is_empty()
        || message.text.len() > MESSAGE_BYTES
        || message.text.chars().any(char::is_control)
        || message.paths.len() > BRIEF_PATHS
    {
        return Err(error(
            "invalid_message",
            "A message is one line of 1 to 400 bytes about at most 8 files",
        ));
    }
    for path in &message.paths {
        validate_relative_path(path)?;
    }
    if let Some(from) = &message.from {
        validate_id(from, "session ID")?;
    }
    if let Recipient::Session { session_id } = &message.to {
        validate_id(session_id, "session ID")?;
    }
    Ok(())
}

/// Whether `me` may take `message` if it is a handoff. A handoff is meant
/// for the next session a person works in, so a headless session (`claude
/// -p`) takes one only when a headless session wrote it: then the next
/// headless run is the kind of session that will carry the work on, as in a
/// scripted pipeline. A person's handoff waits for an interactive session,
/// however many scripted runs start first. A subagent never takes one: it
/// works for its parent, which takes it if it is owed.
pub(crate) fn takes_handoff(state: &State, me: &Session, message: &Message) -> bool {
    if !matches!(message.to, Recipient::NextSession {}) {
        return true;
    }
    if me.parent.is_some() {
        return false;
    }
    !me.headless
        || message
            .from
            .as_deref()
            .and_then(|from| state.sessions.get(from))
            .is_some_and(|sender| sender.headless)
}

/// An upper bound on the bytes an adapter renders for one message: a label
/// of under 192 bytes (a session ID is at most 128), then text and paths
/// that quoting can grow at most fourfold, cut within `MESSAGE_LINE_LIMIT`
/// (ellipsis and `messages show` pointer included) plus its newline.
pub(crate) fn message_cost(message: &Message) -> usize {
    let raw = message.text.len()
        + message
            .paths
            .iter()
            .map(|path| path.len() + 4)
            .sum::<usize>();
    (192 + 4 * raw).min(MESSAGE_LINE_LIMIT + 4)
}

/// A bounded account of one workspace, from projected state only.
fn recap(state: &State, workspace_id: &str, tag: Option<&str>, now_ms: u64) -> Recap {
    let mut sessions: Vec<(u64, RecapSession)> = state
        .sessions
        .values()
        .filter(|session| session.workspace_id == workspace_id)
        .map(|session| {
            let activity = state.activity.get(&session.session_id);
            let in_generation = activity.filter(|activity| activity.generation == state.generation);
            let editing: Vec<String> = state
                .leases
                .values()
                .filter(|lease| {
                    lease.session_id == session.session_id && live_lease(state, lease, now_ms)
                })
                .flat_map(|lease| lease.paths.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(RECAP_ITEMS)
                .collect();
            (
                in_generation
                    .map(SessionActivity::last_seen_ms)
                    .unwrap_or(0),
                RecapSession {
                    session_id: session.session_id.clone(),
                    current: session.generation == state.generation,
                    ended: activity.is_some_and(|activity| activity.ended),
                    liveness: state
                        .session_liveness
                        .get(&session.session_id)
                        .map(|liveness| liveness.status),
                    summary: activity.and_then(|activity| activity.summary.clone()),
                    intent: state
                        .intents
                        .get(&session.session_id)
                        .map(|intent| intent.summary.clone()),
                    editing,
                    recent: in_generation
                        .map(|activity| {
                            activity
                                .touched
                                .iter()
                                .take(RECAP_ITEMS)
                                .map(|touch| RecentPath {
                                    path: touch.path.clone(),
                                    age_ms: now_ms.saturating_sub(touch.at_ms),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    adopted_by: state
                        .adoptions
                        .get(&session.session_id)
                        .map(|adoption| adoption.session_id.clone()),
                    adopted: state
                        .adoptions
                        .values()
                        .filter(|adoption| adoption.session_id == session.session_id)
                        .take(RECAP_ITEMS)
                        .map(|adoption| adoption.from.clone())
                        .collect(),
                },
            )
        })
        .collect();
    // Sessions that are here now first, then by most recent activity.
    sessions.sort_by(|a, b| {
        (!a.1.current, a.1.ended, u64::MAX - a.0, &a.1.session_id).cmp(&(
            !b.1.current,
            b.1.ended,
            u64::MAX - b.0,
            &b.1.session_id,
        ))
    });
    let total = sessions.len();
    let first = |items: Option<&Vec<_>>| -> Vec<_> {
        items
            .map(|items| items.iter().take(RECAP_ITEMS).cloned().collect())
            .unwrap_or_default()
    };
    Recap {
        workspace_id: workspace_id.into(),
        root: state
            .workspaces
            .get(workspace_id)
            .map(|workspace| workspace.root.clone())
            .unwrap_or_default(),
        branch_ref: state
            .workspace_git
            .get(workspace_id)
            .and_then(|git| git.branch_ref.clone()),
        omitted_sessions: u32::try_from(total.saturating_sub(RECAP_SESSIONS)).unwrap_or(u32::MAX),
        omitted_items: 0,
        sessions: sessions
            .into_iter()
            .take(RECAP_SESSIONS)
            .map(|(_, session)| session)
            .collect(),
        interface_changes: state
            .interface_notices
            .get(workspace_id)
            .map(|notices| notices.iter().take(RECAP_ITEMS).cloned().collect())
            .unwrap_or_default(),
        checks: first(state.checks.get(workspace_id)),
        landings: state
            .landings
            .get(workspace_id)
            .map(|landings| landings.iter().take(RECAP_ITEMS).cloned().collect())
            .unwrap_or_default(),
        notes: state
            .notes
            .get(workspace_id)
            .map(|notes| {
                notes
                    .iter()
                    .filter(|note| tag.is_none_or(|tag| note.tags.iter().any(|held| held == tag)))
                    .take(RECAP_SESSIONS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        messages: state
            .messages
            .get(workspace_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .take(RECAP_SESSIONS)
            .chain(
                state
                    .message_history
                    .get(workspace_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|message| message.retracted)
                    .take(RECAP_ITEMS),
            )
            .cloned()
            .collect(),
        trails: trail::listed(state, workspace_id, RECAP_TRAILS, now_ms),
        teammates: state
            .remote_peers
            .values()
            .filter(|peer| peer.workspace_id == workspace_id)
            .take(RECAP_ITEMS)
            .cloned()
            .collect(),
        publishing: state.remote_links.get(workspace_id).cloned(),
        scan_failure: state.scan_failures.get(workspace_id).cloned(),
        scan_coverage: state.scan_coverage.get(workspace_id).cloned(),
        since: None,
    }
}

impl Recap {
    /// Drop items, newest-last lists first, until `fits` says the recap is
    /// small enough. Each drop is counted in `omitted_items`; what was
    /// asked for with `since` goes last, counted in its own `omitted`.
    pub fn shrink_until(&mut self, mut fits: impl FnMut(&Recap) -> bool) {
        while !fits(self) {
            let dropped = self.notes.pop().is_some()
                || self.trails.pop().is_some()
                || self.teammates.pop().is_some()
                || self.checks.pop().is_some()
                || self.landings.pop().is_some()
                || self.interface_changes.pop().is_some()
                || self.sessions.pop().is_some();
            if dropped {
                self.omitted_items = self.omitted_items.saturating_add(1);
            } else if !self.since.as_mut().is_some_and(Since::pop) {
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_send_message(
    state: &State,
    workspace_id: &String,
    from: &Option<String>,
    to: &Option<String>,
    text: &str,
    paths: &[String],
    remote: bool,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if let Some(from) = from
        && known_session(state, from)?.workspace_id != *workspace_id
    {
        return Err(error(
            "sender_not_in_workspace",
            "The sending session belongs to another workspace; send from a session of this one, or leave out the sender to send as the owner",
        ));
    }
    let to = match to {
        Some(to) => {
            validate_id(to, "session ID")?;
            if state
                .sessions
                .get(to)
                .is_none_or(|session| session.workspace_id != *workspace_id)
            {
                return Err(error(
                    "recipient_not_found",
                    "The recipient is not a session of this workspace; `langspan recap WORKSPACE` lists them",
                ));
            }
            if from.as_ref() == Some(to) {
                return Err(error(
                    "invalid_message",
                    "A session cannot send a message to itself; use `remember` for a note",
                ));
            }
            Recipient::Session {
                session_id: to.clone(),
            }
        }
        None => Recipient::Everyone {},
    };
    sent(
        state,
        request_id,
        now_ms,
        workspace_id,
        (from.clone(), to),
        text,
        paths,
        remote,
    )
}

// The body clones the matched `String`s, as the arm did.
#[allow(clippy::ptr_arg)]
pub(crate) fn decide_handoff(
    state: &State,
    session_id: &String,
    text: &str,
    paths: &[String],
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let session = known_session(state, session_id)?;
    sent(
        state,
        request_id,
        now_ms,
        &session.workspace_id,
        (Some(session_id.clone()), Recipient::NextSession {}),
        text,
        paths,
        false,
    )
}

// The body clones the matched `String`s, as the arm did.
#[allow(clippy::ptr_arg)]
pub(crate) fn decide_ack_message(
    state: &State,
    session_id: &String,
    message_id: &String,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let session = known_session(state, session_id)?;
    validate_request_id(message_id)?;
    let owed = state
        .messages
        .get(&session.workspace_id)
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| &message.message_id == message_id)
        })
        .is_some_and(|message| owed_to(message, session_id, true));
    Ok(Decision {
        events: if owed {
            vec![Event::MessageAcknowledged {
                workspace_id: session.workspace_id.clone(),
                message_id: message_id.clone(),
                session_id: session_id.clone(),
                via: Some(Delivery::Ack),
                at_ms: Some(now_ms),
                generation: Some(state.generation),
            }]
        } else {
            Vec::new()
        },
        outcome: Outcome::MessageAcknowledged {
            message_id: message_id.clone(),
            acknowledged: owed,
        },
    })
}

pub(crate) fn decide_observe_note_anchors(
    state: &State,
    workspace_id: &str,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::NoteAnchorsObserved { changed: 0 },
    })
}

pub(crate) fn decide_note_anchors_prepared(
    state: &State,
    workspace_id: &String,
    index: &AnchorIndex,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    let events: Vec<Event> = state
        .notes
        .get(workspace_id)
        .into_iter()
        .flatten()
        .filter_map(|note| {
            let (anchor, held) = note.anchor.as_ref().zip(note.anchor_state)?;
            let (anchor_now, state_now) = anchors::observe(anchor, &note.paths, index);
            (anchor_now != *anchor || state_now != held).then(|| Event::NoteAnchorChanged {
                workspace_id: workspace_id.clone(),
                note_id: note.note_id.clone(),
                anchor: anchor_now,
                state: state_now,
            })
        })
        .collect();
    Ok(Decision {
        outcome: Outcome::NoteAnchorsObserved {
            changed: u32::try_from(events.len()).unwrap_or(u32::MAX),
        },
        events,
    })
}

// The body clones the matched `String`s, as the arm did.
#[allow(clippy::too_many_arguments, clippy::ptr_arg)]
pub(crate) fn decide_remember_prepared(
    state: &State,
    workspace_id: &String,
    author: &Option<String>,
    text: &String,
    paths: &[String],
    symbol: &Option<String>,
    tags: &[String],
    index: &Option<AnchorIndex>,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if let Some(author) = author {
        known_session(state, author)?;
    }
    let paths = if paths.is_empty() {
        Vec::new()
    } else {
        normalized_paths(paths)?
    };
    let tags = anchors::validate_tags(tags)?;
    let (anchor, anchor_state) = match symbol {
        Some(name) => {
            let (anchor, held) = anchors::resolve(name, &paths, index.as_ref())?;
            (Some(anchor), Some(held))
        }
        None => (None, None),
    };
    let note = Note {
        note_id: request_id.into(),
        workspace_id: workspace_id.clone(),
        author: author.clone(),
        text: text.clone(),
        paths,
        anchor,
        anchor_state,
        tags,
    };
    validate_note(&note)?;
    Ok(Decision {
        events: vec![Event::Remembered {
            note: note.clone(),
            at: Some(Stamp {
                at_ms: now_ms,
                generation: state.generation,
            }),
        }],
        outcome: Outcome::Remembered { note },
    })
}

pub(crate) fn decide_forget(
    state: &State,
    workspace_id: &String,
    note_id: &String,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    validate_request_id(note_id)?;
    let removed = state
        .notes
        .get(workspace_id)
        .is_some_and(|notes| notes.iter().any(|note| &note.note_id == note_id));
    Ok(Decision {
        events: if removed {
            vec![Event::Forgotten {
                workspace_id: workspace_id.clone(),
                note_id: note_id.clone(),
            }]
        } else {
            Vec::new()
        },
        outcome: Outcome::Forgotten {
            note_id: note_id.clone(),
            removed,
        },
    })
}

pub(crate) fn decide_recap(
    state: &State,
    workspace_id: &str,
    tag: &Option<String>,
    since: &Option<SinceFrom>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if let Some(tag) = tag {
        anchors::validate_tag(tag)?;
    }
    // What happened since is read from the event log by the store.
    since::validate(since.as_ref())?;
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::Recap {
            recap: recap(state, workspace_id, tag.as_deref(), now_ms),
        },
    })
}

pub(crate) fn decide_recall(
    state: &State,
    workspace_id: &str,
    query: &str,
    path: &Option<String>,
    limit: &u32,
) -> Result<Decision, CoreError> {
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::Recall {
            recall: recall_notes(state, workspace_id, query, path.as_deref(), *limit)?,
        },
    })
}

pub(crate) fn apply_remembered(state: &mut State, note: &Note) -> Result<(), CoreError> {
    validate_note(note)?;
    validate_known_workspace(state, &note.workspace_id)?;
    let notes = state.notes.entry(note.workspace_id.clone()).or_default();
    notes.retain(|held| held.note_id != note.note_id);
    notes.insert(0, note.clone());
    notes.truncate(MAX_NOTES);
    Ok(())
}

pub(crate) fn apply_forgotten(
    state: &mut State,
    workspace_id: &String,
    note_id: &String,
) -> Result<(), CoreError> {
    if let Some(notes) = state.notes.get_mut(workspace_id) {
        notes.retain(|note| &note.note_id != note_id);
    }
    Ok(())
}

pub(crate) fn apply_note_anchor_changed(
    state: &mut State,
    workspace_id: &String,
    note_id: &String,
    anchor: &NoteAnchor,
    anchor_state: &AnchorState,
) -> Result<(), CoreError> {
    // A note forgotten or dropped since is left alone.
    if let Some(note) = state
        .notes
        .get_mut(workspace_id)
        .and_then(|notes| notes.iter_mut().find(|note| &note.note_id == note_id))
        .filter(|note| note.anchor.is_some())
    {
        let mut changed = note.clone();
        changed.anchor = Some(anchor.clone());
        changed.anchor_state = Some(*anchor_state);
        validate_note(&changed)?;
        *note = changed;
    }
    Ok(())
}

pub(crate) fn apply_message_sent(
    state: &mut State,
    message: &Message,
    at_ms: Option<u64>,
) -> Result<(), CoreError> {
    validate_message(message)?;
    validate_known_workspace(state, &message.workspace_id)?;
    if !message.acknowledged.is_empty() || message.retracted {
        return Err(error(
            "invalid_event",
            "A message is sent unacknowledged and not withdrawn",
        ));
    }
    if message.remote {
        let at_ms = at_ms.ok_or_else(|| {
            error(
                "invalid_event",
                "A message sent to other machines records when it was sent",
            )
        })?;
        remote::apply_remote_sent(state, message, at_ms);
    }
    let history = state
        .message_history
        .entry(message.workspace_id.clone())
        .or_default();
    history.retain(|held| held.message_id != message.message_id);
    history.insert(0, message.clone());
    history.truncate(MAX_MESSAGES);
    let held = state
        .messages
        .entry(message.workspace_id.clone())
        .or_default();
    held.retain(|held| held.message_id != message.message_id);
    held.insert(0, message.clone());
    // The oldest held past the cap are dropped unread.
    let pushed_out: Vec<String> = held
        .split_off(held.len().min(MAX_MESSAGES))
        .into_iter()
        .map(|dropped| dropped.message_id)
        .collect();
    let dropped = state
        .dropped_messages
        .entry(message.workspace_id.clone())
        .or_default();
    dropped.retain(|id| *id != message.message_id);
    for id in pushed_out.into_iter().rev() {
        dropped.retain(|held| *held != id);
        dropped.insert(0, id);
    }
    dropped.truncate(MAX_MESSAGES);
    if dropped.is_empty() {
        state.dropped_messages.remove(&message.workspace_id);
    }
    Ok(())
}

pub(crate) fn apply_message_acknowledged(
    state: &mut State,
    workspace_id: &String,
    message_id: &String,
    session_id: &String,
) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    // Sessions of this generation that a message for everyone is
    // still owed to; once none is left it is done.
    let current: Vec<String> = state
        .sessions
        .values()
        .filter(|session| {
            &session.workspace_id == workspace_id && session.generation == state.generation
        })
        .map(|session| session.session_id.clone())
        .collect();
    if let Some(messages) = state.messages.get_mut(workspace_id) {
        // A message the cap already dropped has nothing left to mark.
        if let Some(index) = messages
            .iter()
            .position(|message| &message.message_id == message_id)
        {
            let message = &mut messages[index];
            // The history keeps every reader, for a withdrawal.
            if owed_to(message, session_id, true)
                && let Some(record) =
                    state
                        .message_history
                        .get_mut(workspace_id)
                        .and_then(|history| {
                            history
                                .iter_mut()
                                .find(|record| &record.message_id == message_id)
                        })
                && !record.acknowledged.contains(session_id)
            {
                record.acknowledged.push(session_id.clone());
            }
            let done = match message.to {
                Recipient::Everyone {} => {
                    if !message.acknowledged.contains(session_id) {
                        message.acknowledged.push(session_id.clone());
                    }
                    current
                        .iter()
                        .all(|session| !owed_to(message, session, false))
                }
                Recipient::Session { .. } | Recipient::NextSession {} => true,
            };
            if done {
                messages.remove(index);
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_message_retracted(
    state: &mut State,
    workspace_id: &String,
    message_id: &String,
    by: &Option<String>,
) -> Result<(), CoreError> {
    validate_known_workspace(state, workspace_id)?;
    validate_request_id(message_id)?;
    if let Some(by) = by {
        validate_id(by, "session ID")?;
    }
    let readers = readers(state, workspace_id, message_id);
    // It leaves the next publication; its send still counts.
    remote::apply_remote_retracted(state, workspace_id, message_id);
    let held = state.messages.get_mut(workspace_id).and_then(|messages| {
        let index = messages
            .iter()
            .position(|message| &message.message_id == message_id)?;
        Some(messages.remove(index))
    });
    if state.messages.get(workspace_id).is_some_and(Vec::is_empty) {
        state.messages.remove(workspace_id);
    }
    // The history stays the latest messages sent, in send order: a
    // held message older than it is withdrawn without joining it,
    // so the recap does not list that one.
    let record = state
        .message_history
        .get_mut(workspace_id)
        .and_then(|history| {
            history
                .iter_mut()
                .find(|record| &record.message_id == message_id)
        });
    let withdrawn = match record {
        Some(record) => {
            record.retracted = true;
            Some(record.clone())
        }
        None => held,
    };
    // A message no longer known has nothing left to withdraw.
    if let Some(withdrawn) = withdrawn {
        let (text, cut) = retracted_text(&withdrawn.text);
        let notice = RetractedMessage {
            message_id: message_id.clone(),
            workspace_id: workspace_id.clone(),
            from: withdrawn.from.clone(),
            text,
            cut,
        };
        for reader in readers {
            let notices = state.retracted_notices.entry(reader).or_default();
            notices.retain(|held| held.message_id != *message_id);
            notices.insert(0, notice.clone());
            notices.truncate(MAX_MESSAGES);
        }
    }
    Ok(())
}

pub(crate) fn apply_retraction_delivered(
    state: &mut State,
    session_id: &String,
    message_id: &String,
) -> Result<(), CoreError> {
    validate_id(session_id, "session ID")?;
    validate_request_id(message_id)?;
    if let Some(notices) = state.retracted_notices.get_mut(session_id) {
        notices.retain(|notice| notice.message_id != *message_id);
        if notices.is_empty() {
            state.retracted_notices.remove(session_id);
        }
    }
    Ok(())
}
