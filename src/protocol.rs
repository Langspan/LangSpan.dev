//! Version 10 of the local IPC contract. One bounded JSON line per connection.

use langspan_core::{
    Command, ContextUse, Event, Outcome, RecordedEvent, SessionActivity, StatusRecord,
};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read};

/// Version 10 is version 9 with roadmap A11's outline reads: a session
/// activity's `outlined`, the `read_outlined` event, and the `outline_read`
/// request with its `read_outline` answer. Version 9 is version 8 with
/// roadmap E1's `anchor`, `anchor_state` and `tags` in a note, `symbol` and
/// `tags` in `remember` and `tag` in `recap` (`fits`). Version 8 is version
/// 7 with issue #262's `inflight_partial` in a briefing.
/// Version 7 is version 6 with issue #222's briefing memory and context
/// measurement: a session activity's `told`, `told_at_ms` and `context`, a
/// briefing's `unchanged`, a briefing delivery's `told` and `told_at_ms`,
/// and the `context_added` and `told_forgotten` events (with the
/// `forget_told` request that records the second). Version 6 is version 5
/// with roadmap F5's `rebased` and `adapted` in a landing and in the
/// request that records one (`fits`). Version 5 is version 4 with roadmap
/// H9's `refusal_outcomes` in a report's counts and issue #214's
/// `interface_notice_ids` in a briefing. Version 4 is version 3 with
/// roadmap D7's rename fields in interface changes, history entries and
/// in-flight symbols. Apart from recording a rebased or adapted landing and
/// a note's symbol and tags (`fits`), and a briefing of changes only
/// (`for_version`), requests of an earlier version are requests of this
/// one.
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

/// Whether the daemon answers requests of `version`: every version from
/// the oldest to this build's.
pub fn answers(version: u16) -> bool {
    (OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}

/// `response` as one frame for a client of `version`. Clients of an
/// earlier version read the objects later versions extended with
/// `deny_unknown_fields`, so an answer carrying a field they lack would fail
/// to read as a whole: theirs is encoded without what later versions added
/// (`protocol_9_outcome`, `protocol_8_shape`, `protocol_7_outcome`,
/// `protocol_6_outcome`, then `protocol_5_shape`, `protocol_4_shape` and
/// `protocol_3_shape`). A state record given in parts is encoded by
/// `encode_record_for` before it is cut.
pub fn encode_for(version: u16, response: &Response) -> Result<Vec<u8>, crate::Error> {
    if version >= PROTOCOL_VERSION {
        return encode(response);
    }
    let mut earlier = response.clone();
    if version < PROTOCOL_VERSION_10 {
        protocol_9_outcome(&mut earlier.outcome);
    }
    if version < PROTOCOL_VERSION_8 {
        protocol_7_outcome(&mut earlier.outcome);
    }
    if version < PROTOCOL_VERSION_7 {
        protocol_6_outcome(&mut earlier.outcome);
    }
    let mut value = serde_json::to_value(&earlier)?;
    earlier_shape(version, &mut value);
    encode(&value)
}

/// `record`'s JSON as a client of `version` reads it: the text the store
/// cuts into parts when the record is too large for a page. Parts are
/// pieces of text, so `encode_for` cannot reshape them; the store gives
/// each client the parts of its own version's record.
pub fn encode_record_for(version: u16, record: &StatusRecord) -> Result<String, crate::Error> {
    if version >= PROTOCOL_VERSION {
        return Ok(serde_json::to_string(record)?);
    }
    let mut earlier = record.clone();
    if let StatusRecord::Activity { activity, .. } = &mut earlier {
        if version < PROTOCOL_VERSION_10 {
            protocol_9_activity(activity);
        }
        if version < PROTOCOL_VERSION_7 {
            protocol_6_activity(activity);
        }
    }
    let mut value = serde_json::to_value(&earlier)?;
    earlier_shape(version, &mut value);
    Ok(serde_json::to_string(&value)?)
}

/// `value` without the fields versions after `version` added to it, apart
/// from issue #222's (`protocol_6_outcome`, `protocol_6_activity`).
fn earlier_shape(version: u16, value: &mut serde_json::Value) {
    if version < PROTOCOL_VERSION_9 {
        protocol_8_shape(value);
    }
    if version < PROTOCOL_VERSION_6 {
        protocol_5_shape(value);
    }
    if version < PROTOCOL_VERSION_5 {
        protocol_4_shape(value);
    }
    if version < PROTOCOL_VERSION_4 {
        protocol_3_shape(value);
    }
}

/// `outcome` without what version 8 (issue #262) added: a briefing's
/// `inflight_partial`, which is skipped when false.
fn protocol_7_outcome(outcome: &mut Outcome) {
    if let Outcome::Briefing { briefing } = outcome {
        briefing.inflight_partial = false;
    }
}

/// `activity` without what version 7 (issue #222) added. Every such field is skipped
/// when empty, so emptying it leaves it out.
fn protocol_6_activity(activity: &mut SessionActivity) {
    activity.told = Vec::new();
    activity.told_at_ms = 0;
    activity.context = ContextUse::default();
}

/// `activity` without what version 10 (roadmap A11) added: `outlined`,
/// skipped when empty.
fn protocol_9_activity(activity: &mut SessionActivity) {
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

/// `outcome` without what version 10 (roadmap A11) added: a session
/// activity's `outlined`, and the `read_outlined` event, which version 9
/// has no variant for, left out of an event page. No earlier client sends
/// `outline_read`, so its `read_outline` answer needs no earlier shape.
fn protocol_9_outcome(outcome: &mut Outcome) {
    reshape(outcome, protocol_9_activity, |page| {
        page.retain(|recorded| !matches!(recorded.event, Event::ReadOutlined { .. }));
    });
}

/// `outcome` without what version 7 (issue #222) added. Every field it
/// added is skipped when empty, so emptying it leaves it out; the
/// `context_added` and `told_forgotten` events, which version 6 has no
/// variant for, are left out of an event page.
fn protocol_6_outcome(outcome: &mut Outcome) {
    if let Outcome::Briefing { briefing } = outcome {
        briefing.unchanged = 0;
    }
    reshape(outcome, protocol_6_activity, |events| {
        events.retain(|recorded| {
            !matches!(
                recorded.event,
                Event::ContextAdded { .. } | Event::ToldForgotten { .. }
            )
        });
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

/// The first version that can record a rebased or adapted landing.
const REWRITES_SINCE: u16 = PROTOCOL_VERSION_6;
/// The first version that reads a note's symbol and tags (roadmap E1).
const ANCHORS_SINCE: u16 = PROTOCOL_VERSION_9;

/// Whether a daemon answering `version` reads `command` as it is. An
/// earlier daemon refuses a request to record a landing with `rebased` or
/// `adapted` commits as unreadable, which is too late once the push is
/// done, so a landing asks this before it pushes (roadmap F5). A note with
/// a symbol or tags, and a recap by tag, are not sent to a daemon that
/// cannot read them (roadmap E1).
pub fn fits(version: u16, command: &Command) -> bool {
    match command {
        Command::RecordLanding {
            rebased, adapted, ..
        } => version >= REWRITES_SINCE || (rebased.is_empty() && adapted.is_empty()),
        Command::Remember { symbol, tags, .. } => {
            version >= ANCHORS_SINCE || (symbol.is_none() && tags.is_empty())
        }
        Command::Recap { tag, .. } => version >= ANCHORS_SINCE || tag.is_none(),
        _ => true,
    }
}

/// The first version that can give a briefing of changes only.
const CHANGES_ONLY_SINCE: u16 = PROTOCOL_VERSION_7;

/// `command` as a daemon answering `version` reads it. A briefing of
/// changes only (issue #222) is new in version 7, and an earlier daemon
/// refuses the field as unknown: it is asked for the full view instead,
/// which is all that daemon gives. A daemon of version 7 or later is
/// asked for changes only, as the caller asked.
pub fn for_version(version: u16, command: &mut Command) {
    if version < CHANGES_ONLY_SINCE
        && let Command::Brief { changes_only, .. } = command
    {
        *changes_only = false;
    }
}

/// Why a request that does not fit a daemon of `version` is not sent.
pub fn older_daemon(version: u16) -> String {
    format!(
        "the running daemon speaks protocol version {version}, which cannot read this request (a rebased or adapted landing, or a note's symbol or tags); restart the daemon after upgrading LangSpan"
    )
}

/// The fields version 9 added to a note (`Note` and a recall's
/// `RecalledNote`: the only objects of the protocol with `note_id`, `text`
/// and `paths`), roadmap E1.
const NOTE_FIELDS_SINCE_9: [&str; 3] = ["anchor", "anchor_state", "tags"];

fn is_note(fields: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["note_id", "text", "paths"]
        .iter()
        .all(|key| fields.contains_key(*key))
}

/// `value` without the fields version 9 added. An earlier client would
/// show an anchored note without its state, so one whose definition is not
/// known to be unchanged is left out; a current one is shown as a plain
/// note about its files.
fn protocol_8_shape(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => {
            items.retain(|item| match item {
                Value::Object(fields) if is_note(fields) => fields
                    .get("anchor_state")
                    .is_none_or(|held| held == "current"),
                _ => true,
            });
            items.iter_mut().for_each(protocol_8_shape);
        }
        Value::Object(fields) => {
            if is_note(fields) {
                for key in NOTE_FIELDS_SINCE_9 {
                    fields.shift_remove(key);
                }
            }
            fields.values_mut().for_each(protocol_8_shape);
        }
        _ => {}
    }
}

/// The fields version 6 added to a landing (`Landing`: the only objects of
/// the protocol with both `landing_id` and `check_id`).
const LANDING_FIELDS_SINCE_6: [&str; 2] = ["rebased", "adapted"];

/// `value` without the fields version 6 added.
fn protocol_5_shape(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(protocol_5_shape),
        Value::Object(fields) => {
            if fields.contains_key("landing_id") && fields.contains_key("check_id") {
                for key in LANDING_FIELDS_SINCE_6 {
                    fields.shift_remove(key);
                }
            }
            fields.values_mut().for_each(protocol_5_shape);
        }
        _ => {}
    }
}

/// The field roadmap H9 added to a report's counts in version 5
/// (`ReportCounts`: the only objects of the protocol with both
/// `edits_admitted` and `retried_unchanged`).
const COUNTS_FIELD_SINCE_5: &str = "refusal_outcomes";
/// The field version 5 added to a briefing (`Briefing`, the only object
/// with `changed` and `omitted_peers`): its interface notices' IDs, which
/// a hook reports back once shown (issue #214).
const BRIEFING_FIELD_SINCE_5: &str = "interface_notice_ids";

/// `value` without the fields version 5 added.
fn protocol_4_shape(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(protocol_4_shape),
        Value::Object(fields) => {
            if fields.contains_key("edits_admitted") && fields.contains_key("retried_unchanged") {
                fields.shift_remove(COUNTS_FIELD_SINCE_5);
            }
            if fields.contains_key("changed") && fields.contains_key("omitted_peers") {
                fields.shift_remove(BRIEFING_FIELD_SINCE_5);
            }
            fields.values_mut().for_each(protocol_4_shape);
        }
        _ => {}
    }
}

/// The fields version 4 added to an interface change (`InterfaceChange`,
/// which has `name`) and to a history entry (`HistoryEntry`, which has
/// `seq`).
const CHANGE_FIELDS_SINCE_4: [&str; 4] = ["before_name", "evidence", "before_path", "private"];
const ENTRY_FIELDS_SINCE_4: [&str; 3] = ["after_name", "evidence", "before_path"];
/// The field version 4 added to an in-flight symbol (`InflightSymbol`,
/// `InflightNotice`: the only objects with `signature` and `mentioned_in`).
const INFLIGHT_FIELD_SINCE_4: &str = "renamed_from";
/// The trail a briefing carries (`Briefing`, the only object with
/// `changed` and `omitted_peers`) and the trails of a recap (`Recap`, with
/// `sessions` and `omitted_sessions`), roadmap C9. A version 3 client is
/// told neither; a trail taken by its briefing is not offered again.
const BRIEFING_FIELD_SINCE_4: &str = "trail";
const RECAP_FIELD_SINCE_4: &str = "trails";

/// An interface change or a history entry: the objects of the protocol
/// with all of `kind`, `change`, `before` and `after`. A write check's
/// signature change (`WriteCheckSymbol`, roadmap B26) has them too; it
/// carries none of the fields version 4 added, so it passes unchanged.
fn is_change(fields: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["kind", "change", "before", "after"]
        .iter()
        .all(|key| fields.contains_key(*key))
}

/// `value` without the fields version 4 added, and without interface
/// changes of definitions that are not exported (`private`, history only).
fn protocol_3_shape(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => {
            items.retain(|item| {
                !matches!(item, Value::Object(fields)
                    if is_change(fields) && fields.get("private") == Some(&Value::Bool(true)))
            });
            items.iter_mut().for_each(protocol_3_shape);
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
            if fields.contains_key("signature") && fields.contains_key("mentioned_in") {
                fields.shift_remove(INFLIGHT_FIELD_SINCE_4);
            }
            if fields.contains_key("changed") && fields.contains_key("omitted_peers") {
                fields.shift_remove(BRIEFING_FIELD_SINCE_4);
            }
            if fields.contains_key("sessions") && fields.contains_key("omitted_sessions") {
                fields.shift_remove(RECAP_FIELD_SINCE_4);
            }
            fields.values_mut().for_each(protocol_3_shape);
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
