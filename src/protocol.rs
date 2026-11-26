//! Version 6 of the local IPC contract. One bounded JSON line per connection.

use langspan_core::{Command, Outcome};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read};

/// Version 6 is version 5 with roadmap F5's `rebased` and `adapted` in a
/// landing and in the request that records one (`fits`). Version 5 is
/// version 4 with roadmap H9's `refusal_outcomes` in a report's counts.
/// Version 4 is version 3 with roadmap D7's rename fields in interface
/// changes, history entries and in-flight symbols. Apart from recording a
/// rebased or adapted landing, requests are the same in all four.
pub const PROTOCOL_VERSION: u16 = 6;
/// The earliest version the daemon still answers, in that version's shape
/// (`encode_for`), so that a client of an earlier build keeps working after
/// the daemon is upgraded.
pub const OLDEST_PROTOCOL_VERSION: u16 = 3;

/// Whether the daemon answers requests of `version`.
pub fn answers(version: u16) -> bool {
    (OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}

/// `response` as one frame for a client of `version`. Clients of earlier
/// versions read report counts, interface changes and history entries with
/// `deny_unknown_fields`, so an answer carrying a field a later version
/// added would fail to read as a whole: theirs is encoded in their
/// version's shape (`protocol_5_shape`, `protocol_4_shape`, then
/// `protocol_3_shape`).
pub fn encode_for(version: u16, response: &Response) -> Result<Vec<u8>, crate::Error> {
    if version >= PROTOCOL_VERSION {
        return encode(response);
    }
    let mut value = serde_json::to_value(response)?;
    protocol_5_shape(&mut value);
    if version < 5 {
        protocol_4_shape(&mut value);
    }
    if version < 4 {
        protocol_3_shape(&mut value);
    }
    encode(&value)
}

/// The first version that can record a rebased or adapted landing.
const REWRITES_SINCE: u16 = 6;

/// Whether a daemon answering `version` reads `command` as it is. An
/// earlier daemon refuses a request to record a landing with `rebased` or
/// `adapted` commits as unreadable, which is too late once the push is
/// done, so a landing asks this before it pushes (roadmap F5).
pub fn fits(version: u16, command: &Command) -> bool {
    match command {
        Command::RecordLanding {
            rebased, adapted, ..
        } => version >= REWRITES_SINCE || (rebased.is_empty() && adapted.is_empty()),
        _ => true,
    }
}

/// Why a request that does not fit a daemon of `version` is not sent.
pub fn older_daemon(version: u16) -> String {
    format!(
        "the running daemon speaks protocol version {version}, which cannot record a rebased or adapted landing; restart the daemon after upgrading LangSpan"
    )
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

/// The field version 5 added to a report's counts (`ReportCounts`: the only
/// objects of the protocol with both `edits_admitted` and
/// `retried_unchanged`).
const COUNTS_FIELD_SINCE_5: &str = "refusal_outcomes";

/// `value` without the field version 5 added.
fn protocol_4_shape(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(protocol_4_shape),
        Value::Object(fields) => {
            if fields.contains_key("edits_admitted") && fields.contains_key("retried_unchanged") {
                fields.shift_remove(COUNTS_FIELD_SINCE_5);
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
