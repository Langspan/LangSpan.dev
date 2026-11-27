//! Version 10 of the local IPC contract, with named capabilities (ADR 0037).
//! One bounded JSON line per connection.

use langspan_core::{
    Command, ContextUse, Event, Outcome, RecordedEvent, SessionActivity, StatusRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::{self, BufRead, BufReader, Read};

/// The version changes only for a breaking change: a field removed or
/// redefined, or a capability withdrawn. A new wire field adds a line to
/// `CAPABILITIES` instead (ADR 0037). Versions 3 to 10 are frozen shapes,
/// each mapped to its capability set by that table.
pub const PROTOCOL_VERSION: u16 = PROTOCOL_VERSION_10;
/// Roadmap A11's version, which added the outline reads.
pub const PROTOCOL_VERSION_10: u16 = 10;
/// Roadmap E1's version, answered in its own shape.
pub const PROTOCOL_VERSION_9: u16 = 9;
/// Issue #262's version, answered in its own shape.
pub const PROTOCOL_VERSION_8: u16 = 8;
/// Issue #222's version, answered in its own shape.
pub const PROTOCOL_VERSION_7: u16 = 7;
/// Roadmap F5's version, answered in its own shape.
pub const PROTOCOL_VERSION_6: u16 = 6;
/// The version of roadmap H9 and issue #214, answered in its own shape.
pub const PROTOCOL_VERSION_5: u16 = 5;
/// Roadmap D7's version, answered in its own shape.
pub const PROTOCOL_VERSION_4: u16 = 4;
/// The earliest version the daemon still answers, in that version's shape
/// (`encode_for`), so that a client of an earlier build keeps working after
/// the daemon is upgraded.
pub const OLDEST_PROTOCOL_VERSION: u16 = 3;

/// Every capability this build knows, one line each in name order, with the
/// first version whose clients have it without naming it (ADR 0037). A
/// capability added from now on has `None`: only a client that names it
/// gets it. The ADR's table lists the fields each one gates and the item
/// that added it. No line carries a trailing comment: rustfmt aligns those
/// across lines, so a new one would touch its neighbours.
pub const CAPABILITIES: &[(&str, Option<u16>)] = &[
    ("anchored_notes", Some(PROTOCOL_VERSION_9)),
    ("briefing_memory", Some(PROTOCOL_VERSION_7)),
    ("inflight_partial", Some(PROTOCOL_VERSION_8)),
    ("interface_notices", Some(PROTOCOL_VERSION_5)),
    ("landing_rewrites", Some(PROTOCOL_VERSION_6)),
    ("outline_reads", Some(PROTOCOL_VERSION_10)),
    ("recap_since", None),
    ("refusal_outcomes", Some(PROTOCOL_VERSION_5)),
    ("rename_holds", None),
    ("renames", Some(PROTOCOL_VERSION_4)),
    ("symbol_findings", Some(PROTOCOL_VERSION_6)),
    ("tolerant_responses", None),
    ("trails", Some(PROTOCOL_VERSION_4)),
    ("withheld_events", None),
];

const _: () = assert!(CAPABILITIES.len() <= 64, "Capabilities is a 64-bit set");

/// A set of capabilities from `CAPABILITIES`: what a request is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities(u64);

impl Capabilities {
    /// Every capability this build knows.
    pub const ALL: Self = Self(u64::MAX >> (64 - CAPABILITIES.len()));

    /// What a request of `version` that names `named` is granted: the named
    /// capabilities this build knows, or its version's when it names none.
    /// Names this build does not know are ignored.
    pub fn negotiate(version: u16, named: Option<&[String]>) -> Self {
        let mut granted = 0;
        for (bit, (name, since)) in CAPABILITIES.iter().enumerate() {
            let has = match named {
                Some(named) => named.iter().any(|asked| asked == name),
                None => since.is_some_and(|since| version >= since),
            };
            if has {
                granted |= 1 << bit;
            }
        }
        Self(granted)
    }

    /// Whether `name`, a capability of `CAPABILITIES`, is granted.
    pub fn has(self, name: &str) -> bool {
        let bit = CAPABILITIES.iter().position(|(known, _)| *known == name);
        debug_assert!(bit.is_some(), "{name} is not in CAPABILITIES");
        bit.is_some_and(|bit| self.0 & (1 << bit) != 0)
    }

    /// Whether a client granted this set reads what `needs` names. One of
    /// version V that names none holds exactly the capabilities whose
    /// "since" is at most V, and each version from 4 to 10 added one, so
    /// holding all of them is holding version V or later.
    fn reads(self, needs: Needs) -> bool {
        match needs {
            Needs::Version(version) => {
                let of = Self::negotiate(version, None).0;
                self.0 & of == of
            }
            Needs::Capability(name) => self.has(name),
        }
    }

    /// The granted names, in the table's order.
    pub fn names(self) -> Vec<String> {
        CAPABILITIES
            .iter()
            .filter(|(name, _)| self.has(name))
            .map(|(name, _)| (*name).to_string())
            .collect()
    }
}

/// Whether the daemon answers requests of `version`: every version from
/// the oldest to this build's.
pub fn answers(version: u16) -> bool {
    (OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}

/// `response` as one frame for a client of `version` that names no
/// capabilities: in that version's shape.
pub fn encode_for(version: u16, response: &Response) -> Result<Vec<u8>, crate::Error> {
    encode_with(Capabilities::negotiate(version, None), None, response)
}

/// `response` as one frame for a client granted `granted`, which named
/// `named`. Clients read the objects with `deny_unknown_fields`, so an
/// answer carrying a field they lack would fail to read as a whole: theirs
/// is encoded without the fields of each capability it lacks. An event
/// page leaves out each event whose type the client cannot read
/// (`event_needs`); a client with `withheld_events` is told how many in
/// `withheld`. When the client named one this build does not know, the
/// frame also carries `capabilities`, the names granted. A state record
/// given in parts is encoded by `encode_record_with` before it is cut.
pub fn encode_with(
    granted: Capabilities,
    named: Option<&[String]>,
    response: &Response,
) -> Result<Vec<u8>, crate::Error> {
    let names = granted.names();
    let answer = named.is_some_and(|named| named.iter().any(|name| !names.contains(name)));
    if granted == Capabilities::ALL && !answer {
        return encode(response);
    }
    let mut earlier = response.clone();
    let withheld = without_unread_events(granted, &mut earlier.outcome);
    if !granted.has("outline_reads") {
        without_outline_reads(&mut earlier.outcome);
    }
    if !granted.has("inflight_partial")
        && let Outcome::Briefing { briefing } = &mut earlier.outcome
    {
        briefing.inflight_partial = false;
    }
    if !granted.has("briefing_memory") {
        without_briefing_memory(&mut earlier.outcome);
    }
    if !granted.has("recap_since")
        && let Outcome::Recap { recap } = &mut earlier.outcome
    {
        recap.since = None;
    }
    if !granted.has("rename_holds")
        && let Outcome::CallBreaks { callers, .. } = &mut earlier.outcome
    {
        callers.clear();
    }
    let mut value = serde_json::to_value(&earlier)?;
    earlier_shape(granted, &mut value);
    if let Value::Object(fields) = &mut value {
        if answer {
            fields.insert("capabilities".into(), names.into());
        }
        if withheld > 0 && granted.has("withheld_events") {
            fields.insert("withheld".into(), withheld.into());
        }
    }
    encode(&value)
}

/// `outcome`'s event page without the events `granted` cannot read: how
/// many were left out. A client of a frozen version reads events with
/// `deny_unknown_fields`, so one type it lacks would fail the whole page.
fn without_unread_events(granted: Capabilities, outcome: &mut Outcome) -> usize {
    let (Outcome::Events { events } | Outcome::Changes { events, .. }) = outcome else {
        return 0;
    };
    let before = events.len();
    events.retain(|recorded| granted.reads(event_needs(&recorded.event)));
    before - events.len()
}

/// `record`'s JSON as a client of `version` that names no capabilities
/// reads it.
pub fn encode_record_for(version: u16, record: &StatusRecord) -> Result<String, crate::Error> {
    encode_record_with(Capabilities::negotiate(version, None), record)
}

/// `record`'s JSON as a client granted `granted` reads it: the text the
/// store cuts into parts when the record is too large for a page. Parts are
/// pieces of text, so `encode_with` cannot reshape them; the store gives
/// each client the parts of its own record.
pub fn encode_record_with(
    granted: Capabilities,
    record: &StatusRecord,
) -> Result<String, crate::Error> {
    if granted == Capabilities::ALL {
        return Ok(serde_json::to_string(record)?);
    }
    let mut earlier = record.clone();
    if let StatusRecord::Activity { activity, .. } = &mut earlier {
        if !granted.has("outline_reads") {
            activity_without_outline_reads(activity);
        }
        if !granted.has("briefing_memory") {
            activity_without_briefing_memory(activity);
        }
    }
    let mut value = serde_json::to_value(&earlier)?;
    earlier_shape(granted, &mut value);
    Ok(serde_json::to_string(&value)?)
}

/// `value` without the fields of each capability `granted` lacks, apart
/// from those reshaped before it is a value (`without_outline_reads`,
/// `without_briefing_memory`, a briefing's `inflight_partial`).
fn earlier_shape(granted: Capabilities, value: &mut Value) {
    if !granted.has("anchored_notes") {
        without_anchored_notes(value);
    }
    if !granted.has("landing_rewrites") {
        strip(value, is_landing, &LANDING_REWRITES);
    }
    if !granted.has("symbol_findings") {
        strip(value, is_check, &["symbol_findings"]);
    }
    if !granted.has("refusal_outcomes") {
        strip(value, is_counts, &["refusal_outcomes"]);
    }
    if !granted.has("interface_notices") {
        strip(value, is_briefing, &["interface_notice_ids"]);
    }
    if !granted.has("trails") {
        strip(value, is_briefing, &["trail"]);
        strip(value, is_recap, &["trails"]);
    }
    if !granted.has("renames") {
        without_renames(value);
    }
}

/// What a client needs to read an event type (issue #331).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needs {
    /// A client of this version or later; every client that names
    /// capabilities is.
    Version(u16),
    /// A client granted this capability of `CAPABILITIES`.
    Capability(&'static str),
}

/// The "since" table: what a client needs to read `event`'s type. A type
/// gets the first version whose every build has it, or the capability that
/// added it; one added from now on gets a capability of its own, which no
/// frozen version has. The match is exhaustive, so a new type does not
/// compile until it has an entry. `fixtures/event-types/` holds the types
/// each release reads.
fn event_needs(event: &Event) -> Needs {
    use Event as E;
    match event {
        E::ContextAdded { .. } | E::ToldForgotten { .. } => Needs::Capability("briefing_memory"),
        E::NoteAnchorChanged { .. } => Needs::Capability("anchored_notes"),
        E::ReadOutlined { .. } => Needs::Capability("outline_reads"),
        // Added at version 5 without a new version (#214).
        E::InterfaceNoticesTold { .. } => Needs::Version(6),
        // Added at version 4 without a new version (roadmap B28 and C9).
        E::WriteWaited { .. }
        | E::RefusalSettled { .. }
        | E::PolicySet { .. }
        | E::TrailRecorded { .. }
        | E::TrailTaken { .. } => Needs::Version(5),
        // Added in 0.2.0, which still spoke version 3.
        E::WorkspaceMoved { .. }
        | E::WorkspaceRemoved { .. }
        | E::SessionSeen { .. }
        | E::PresenceObserved { .. }
        | E::PresenceSet { .. }
        | E::SessionAdopted { .. }
        | E::InflightDelivered { .. }
        | E::TestsRecorded { .. }
        | E::FlakyTestForgotten { .. }
        | E::InterfaceDependentsFound { .. }
        | E::MessageRetracted { .. }
        | E::RetractionDelivered { .. }
        | E::CheckoutRunObserved { .. }
        | E::UncoordinatedChanged { .. }
        | E::UncoordinatedDelivered { .. } => Needs::Version(4),
        // Release 0.1.0's.
        E::Booted { .. }
        | E::ClockAnchored { .. }
        | E::WriteRefused { .. }
        | E::WorkspaceRegistered { .. }
        | E::SessionRegistered { .. }
        | E::SessionLivenessObserved { .. }
        | E::IntentSet { .. }
        | E::IntentEnded { .. }
        | E::LeaseGranted { .. }
        | E::LeaseCompleted { .. }
        | E::LeaseExpired { .. }
        | E::OperationObserved { .. }
        | E::DiskObserved { .. }
        | E::BranchObserved { .. }
        | E::WorkspaceGitObserved { .. }
        | E::SessionPrompted { .. }
        | E::PromptTargetsSet { .. }
        | E::SessionEnded { .. }
        | E::SessionStopped { .. }
        | E::ToolBatchObserved { .. }
        | E::BriefingDelivered { .. }
        | E::InterfaceAlerted { .. }
        | E::SourceObserved { .. }
        | E::RemoteEnabled { .. }
        | E::RemoteDisabled { .. }
        | E::RemotePublished { .. }
        | E::RemoteObserved { .. }
        | E::RemoteForgotten { .. }
        | E::RemoteWithdrawn { .. }
        | E::CheckRecorded { .. }
        | E::AutoCheckEnabled { .. }
        | E::AutoCheckDisabled { .. }
        | E::CheckReported { .. }
        | E::CheckDelivered { .. }
        | E::Landed { .. }
        | E::TurnTaken { .. }
        | E::TurnReleased { .. }
        | E::TurnRenewed { .. }
        | E::TurnExpired { .. }
        | E::TurnQueueJoined { .. }
        | E::TurnQueueSeen { .. }
        | E::TurnQueueLeft { .. }
        | E::InterfaceChanged { .. }
        | E::Remembered { .. }
        | E::Forgotten { .. }
        | E::MessageSent { .. }
        | E::MessageAcknowledged { .. }
        | E::ScanFailed { .. }
        | E::ScanRecovered { .. }
        | E::ScanCoverageObserved { .. }
        | E::CheckoutChanged { .. }
        | E::HostNotInstalled { .. }
        | E::HostCleared { .. } => Needs::Version(OLDEST_PROTOCOL_VERSION),
    }
}

/// `value` without `keys` in each object `is` picks.
fn strip(value: &mut Value, is: fn(&Map<String, Value>) -> bool, keys: &[&str]) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(|item| strip(item, is, keys)),
        Value::Object(fields) => {
            if is(fields) {
                for key in keys {
                    fields.shift_remove(*key);
                }
            }
            fields.values_mut().for_each(|field| strip(field, is, keys));
        }
        _ => {}
    }
}

fn has_all(fields: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().all(|key| fields.contains_key(*key))
}

/// `activity` without `briefing_memory` (issue #222). Every such field is
/// skipped when empty, so emptying it leaves it out.
fn activity_without_briefing_memory(activity: &mut SessionActivity) {
    activity.told = Vec::new();
    activity.told_at_ms = 0;
    activity.context = ContextUse::default();
}

/// `activity` without `outline_reads` (roadmap A11): `outlined`, skipped
/// when empty.
fn activity_without_outline_reads(activity: &mut SessionActivity) {
    activity.outlined = Vec::new();
}

/// Apply `activity` to each session activity `outcome` carries, and
/// `events` to each event page.
fn reshape(
    outcome: &mut Outcome,
    activity: fn(&mut SessionActivity),
    events: fn(&mut Vec<RecordedEvent>),
) {
    match outcome {
        Outcome::Status { state, .. } => state.activity.values_mut().for_each(activity),
        Outcome::StatusRecords { records, .. } => {
            for record in records {
                if let StatusRecord::Activity { activity: a, .. } = record {
                    activity(a);
                }
            }
        }
        Outcome::Events { events: page } | Outcome::Changes { events: page, .. } => events(page),
        _ => {}
    }
}

/// `outcome` without `outline_reads` (roadmap A11): a session activity's
/// `outlined`. Its `read_outlined` event is left out of an event page by
/// `without_unread_events`. No such client sends `outline_read`, so its
/// `read_outline` answer needs no earlier shape.
fn without_outline_reads(outcome: &mut Outcome) {
    reshape(outcome, activity_without_outline_reads, |_| {});
}

/// `outcome` without `briefing_memory` (issue #222). Every field it added
/// is skipped when empty, so emptying it leaves it out. Its
/// `context_added` and `told_forgotten` events are left out of an event
/// page by `without_unread_events`.
fn without_briefing_memory(outcome: &mut Outcome) {
    if let Outcome::Briefing { briefing } = outcome {
        briefing.unchanged = 0;
    }
    reshape(outcome, activity_without_briefing_memory, |events| {
        for recorded in events {
            if let Event::BriefingDelivered {
                told, told_at_ms, ..
            } = &mut recorded.event
            {
                *told = Vec::new();
                *told_at_ms = None;
            }
        }
    });
}

/// Whether a daemon that granted `granted` reads `command` once reshaped by
/// `for_capabilities`. A daemon without `landing_rewrites` refuses a
/// request to record a landing with `rebased` or `adapted` commits as
/// unreadable, which is too late once the push is done, so a landing asks
/// this before it pushes (roadmap F5). A note with a symbol or tags, and a
/// recap by tag, are not sent to a daemon without `anchored_notes`
/// (roadmap E1), nor a recap asked for `since` to one without
/// `recap_since` (roadmap C10). A check of renamed names alone is not sent
/// to a daemon without `rename_holds` (issue #213).
pub fn fits_with(granted: Capabilities, command: &Command) -> bool {
    match command {
        Command::RecordLanding {
            rebased, adapted, ..
        } => granted.has("landing_rewrites") || (rebased.is_empty() && adapted.is_empty()),
        Command::Remember { symbol, tags, .. } => {
            granted.has("anchored_notes") || (symbol.is_none() && tags.is_empty())
        }
        Command::Recap { tag, since, .. } => {
            (granted.has("anchored_notes") || tag.is_none())
                && (granted.has("recap_since") || since.is_none())
        }
        Command::CheckCalls { calls, renamed, .. } => {
            granted.has("rename_holds") || !calls.is_empty() || renamed.is_empty()
        }
        _ => true,
    }
}

/// Whether a daemon answering `version`, to a request naming no
/// capabilities, reads `command` (`fits_with` of that version's).
pub fn fits(version: u16, command: &Command) -> bool {
    fits_with(Capabilities::negotiate(version, None), command)
}

/// `command` as a daemon that granted `granted` reads it. A briefing of
/// changes only (`briefing_memory`, issue #222) is asked of a daemon
/// without it as the full view, which is all that daemon gives. A check is
/// recorded without its findings (`symbol_findings`, issue #265), which
/// are advisory, by a daemon that cannot read them. A write check asks a
/// daemon without `rename_holds` about its calls only, so it holds nothing
/// (issue #213).
pub fn for_capabilities(granted: Capabilities, command: &mut Command) {
    match command {
        Command::Brief { changes_only, .. } if !granted.has("briefing_memory") => {
            *changes_only = false;
        }
        Command::RecordCheck {
            symbol_findings, ..
        } if !granted.has("symbol_findings") => *symbol_findings = None,
        Command::CheckCalls { renamed, .. } if !granted.has("rename_holds") => renamed.clear(),
        _ => {}
    }
}

/// Why a request that does not fit a daemon of `version` is not sent.
pub fn older_daemon(version: u16) -> String {
    format!(
        "the running daemon speaks protocol version {version}, which cannot read this request (a rebased or adapted landing, a note's symbol or tags, a recap's since, or a rename check); restart the daemon after upgrading LangSpan"
    )
}

/// The fields `anchored_notes` gates in a note (`Note` and a recall's
/// `RecalledNote`: the only objects of the protocol with `note_id`, `text`
/// and `paths`), roadmap E1.
const ANCHORED_NOTE_FIELDS: [&str; 3] = ["anchor", "anchor_state", "tags"];

fn is_note(fields: &Map<String, Value>) -> bool {
    has_all(fields, &["note_id", "text", "paths"])
}

/// `value` without `anchored_notes`. A client without it would show an
/// anchored note without its state, so one whose definition is not known
/// to be unchanged is left out; a current one is shown as a plain note
/// about its files.
fn without_anchored_notes(value: &mut Value) {
    match value {
        Value::Array(items) => {
            items.retain(|item| match item {
                Value::Object(fields) if is_note(fields) => fields
                    .get("anchor_state")
                    .is_none_or(|held| held == "current"),
                _ => true,
            });
            items.iter_mut().for_each(without_anchored_notes);
        }
        Value::Object(fields) => {
            if is_note(fields) {
                for key in ANCHORED_NOTE_FIELDS {
                    fields.shift_remove(key);
                }
            }
            fields.values_mut().for_each(without_anchored_notes);
        }
        _ => {}
    }
}

/// The fields `landing_rewrites` gates in a landing (`Landing`: the only
/// objects of the protocol with both `landing_id` and `check_id`).
const LANDING_REWRITES: [&str; 2] = ["rebased", "adapted"];

fn is_landing(fields: &Map<String, Value>) -> bool {
    has_all(fields, &["landing_id", "check_id"])
}

/// A check record (`CheckRecord`: the only objects of the protocol with
/// both `check_id` and `duration_ms`).
fn is_check(fields: &Map<String, Value>) -> bool {
    has_all(fields, &["check_id", "duration_ms"])
}

/// A report's counts (`ReportCounts`: the only objects of the protocol
/// with both `edits_admitted` and `retried_unchanged`).
fn is_counts(fields: &Map<String, Value>) -> bool {
    has_all(fields, &["edits_admitted", "retried_unchanged"])
}

/// A briefing (`Briefing`, the only object with `changed` and
/// `omitted_peers`). Its `trail` (roadmap C9) is not offered again once
/// taken, so a client without `trails` is told none.
fn is_briefing(fields: &Map<String, Value>) -> bool {
    has_all(fields, &["changed", "omitted_peers"])
}

/// A recap (`Recap`, with `sessions` and `omitted_sessions`).
fn is_recap(fields: &Map<String, Value>) -> bool {
    has_all(fields, &["sessions", "omitted_sessions"])
}

/// The fields `renames` gates in an interface change (`InterfaceChange`,
/// which has `name`) and in a history entry (`HistoryEntry`, which has
/// `seq`).
const CHANGE_FIELDS_SINCE_4: [&str; 4] = ["before_name", "evidence", "before_path", "private"];
const ENTRY_FIELDS_SINCE_4: [&str; 3] = ["after_name", "evidence", "before_path"];
/// The field `renames` gates in an in-flight symbol (`InflightSymbol`,
/// `InflightNotice`: the only objects with `signature` and `mentioned_in`).
const INFLIGHT_FIELD_SINCE_4: &str = "renamed_from";

/// An interface change or a history entry: the objects of the protocol
/// with all of `kind`, `change`, `before` and `after`. A write check's
/// signature change (`WriteCheckSymbol`, roadmap B26) has them too; it
/// carries none of the fields `renames` gates, so it passes unchanged.
fn is_change(fields: &Map<String, Value>) -> bool {
    has_all(fields, &["kind", "change", "before", "after"])
}

/// `value` without `renames` (roadmap D7), and without interface changes
/// of definitions that are not exported (`private`, history only).
fn without_renames(value: &mut Value) {
    match value {
        Value::Array(items) => {
            items.retain(|item| {
                !matches!(item, Value::Object(fields)
                    if is_change(fields) && fields.get("private") == Some(&Value::Bool(true)))
            });
            items.iter_mut().for_each(without_renames);
        }
        Value::Object(fields) => {
            if is_change(fields) {
                let added: &[&str] = if fields.contains_key("seq") {
                    &ENTRY_FIELDS_SINCE_4
                } else {
                    &CHANGE_FIELDS_SINCE_4
                };
                for key in added {
                    fields.shift_remove(*key);
                }
            }
            if has_all(fields, &["signature", "mentioned_in"]) {
                fields.shift_remove(INFLIGHT_FIELD_SINCE_4);
            }
            fields.values_mut().for_each(without_renames);
        }
        _ => {}
    }
}
pub const MAX_FRAME_BYTES: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub request_id: String,
    pub command: Command,
    /// The capabilities the client reads (ADR 0037). A client of version 3
    /// to 10 from before the handshake names none and is granted its
    /// version's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
}

impl Request {
    /// A request at the current protocol version, naming every capability
    /// this build reads. Build every request through this, so a new field
    /// is added here and not at each call site.
    pub fn new(request_id: impl Into<String>, command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id: request_id.into(),
            command,
            capabilities: Some(Capabilities::ALL.names()),
        }
    }

    /// The same request at another protocol version, as a client of that
    /// version from before capabilities would send it: naming none.
    #[must_use]
    pub fn with_version(mut self, version: u16) -> Self {
        self.version = version;
        self.capabilities = None;
        self
    }
}

/// An answer. On the wire, the answer to a request that named a
/// capability the daemon does not know also carries `capabilities`, the
/// names granted (`encode_with`); the client reads it before the response.
/// A client ignores the fields it does not know in a response, its outcome
/// and the objects only responses carry (`tolerant_responses`, ADR 0015).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Response {
    pub version: u16,
    pub request_id: String,
    pub replayed: bool,
    pub generation: u64,
    pub lease_valid: Option<bool>,
    pub outcome: Outcome,
}

impl Response {
    /// The same response carrying another outcome: an answer cut to fit.
    pub fn with_outcome(&self, outcome: Outcome) -> Self {
        Self {
            version: self.version,
            request_id: self.request_id.clone(),
            replayed: self.replayed,
            generation: self.generation,
            lease_valid: self.lease_valid,
            outcome,
        }
    }
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, crate::Error> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err("JSON message exceeds the 64 KiB frame limit; narrow the request".into());
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn read_frame(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take((MAX_FRAME_BYTES + 1) as u64)).read_until(b'\n', &mut bytes)?;
    if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected one newline-terminated JSON frame of at most 64 KiB",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_request_is_at_the_current_version_and_keeps_its_wire_shape() {
        let request = Request::new("r-1", Command::Status);
        assert_eq!(request.version, PROTOCOL_VERSION);
        assert_eq!(request.request_id, "r-1");
        assert_eq!(request.command, Command::Status);
        let wire: serde_json::Value = serde_json::from_slice(&encode(&request).unwrap()).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "version": PROTOCOL_VERSION,
                "request_id": "r-1",
                "command": serde_json::to_value(Command::Status).unwrap(),
                "capabilities": Capabilities::ALL.names(),
            })
        );
        let older = request.with_version(3);
        assert_eq!(older.version, 3);
        assert_eq!(older.request_id, "r-1");
        assert_eq!(older.capabilities, None);
    }

    /// Issue #331: every event type has an entry in the "since" table
    /// (`event_needs`, an exhaustive match, so a new type does not compile
    /// without one). A type the newest release does not read needs a
    /// capability no frozen version has, so it reaches only a client that
    /// names it.
    #[test]
    fn an_event_type_no_release_reads_needs_a_capability_of_its_own() {
        let newest: Vec<&str> = include_str!("../fixtures/event-types/0.5.1.txt")
            .lines()
            .collect();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.to_string_lossy().ends_with("-event-v10.json") {
                continue;
            }
            let event: Event = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let name = serde_json::to_value(&event).unwrap()["type"].clone();
            let needs = event_needs(&event);
            if let Needs::Capability(capability) = needs {
                assert!(
                    CAPABILITIES.iter().any(|(known, _)| *known == capability),
                    "{name}: {capability}"
                );
            }
            if !newest.contains(&name.as_str().unwrap()) {
                assert!(
                    matches!(needs, Needs::Capability(capability)
                        if CAPABILITIES.contains(&(capability, None))),
                    "{name} is read by no release: {needs:?}"
                );
            }
        }
    }
}
