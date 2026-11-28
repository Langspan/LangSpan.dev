//! The store's partial indexes and the lookups that must match them to use
//! them: each index has one owner with its lookups.

/// A generation marker: a `booted` or `clock_anchored` event. One owner for
/// the index over them and the lookup that must match it to use it.
macro_rules! generation_marker {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         IN ('booted', 'clock_anchored')"
    };
}

/// A partial index of the generation markers, a few rows per daemon start,
/// so the anchor lookup does not read the generation's other events.
pub(super) const GENERATION_MARKER_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_generation_marker ON events(seq) WHERE ",
    generation_marker!()
);

/// The newest generation marker before a sequence number: the anchor of the
/// generation the event there belongs to. Read through the partial index.
pub(super) const ANCHOR_LOOKUP: &str = concat!(
    "SELECT event_json FROM events WHERE seq < ?1 AND ",
    generation_marker!(),
    " ORDER BY seq DESC LIMIT 1"
);

/// An event that sets or ends a session's intent, or binds or unbinds a
/// session's workspace.
macro_rules! intent_event {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         IN ('intent_set', 'intent_ended', 'session_ended', 'workspace_removed',
             'session_registered')"
    };
}

/// A partial index of the intent events, the workspace removals that end
/// intents and the session registrations, so the report can find which
/// sessions held an intent, and which workspace each session belonged to,
/// before the events it reads without reading the rest of the older log.
/// `events_intent` and `events_intent_state` were earlier forms of this
/// index with fewer kinds; the new name replaces them in a database an
/// earlier build opened.
pub(super) const INTENT_EVENT_INDEX: &str = concat!(
    "DROP INDEX IF EXISTS events_intent; ",
    "DROP INDEX IF EXISTS events_intent_state; ",
    "CREATE INDEX IF NOT EXISTS events_report_state ON events(seq) WHERE ",
    intent_event!()
);

/// The newest intent and membership events after the first sequence number
/// and before the second, newest first, at most the third parameter. Read
/// through the partial index, so it visits no more index entries than that.
pub(super) const INTENT_EVENTS_BEFORE: &str = concat!(
    "SELECT event_json FROM events WHERE seq > ?1 AND seq < ?2 AND ",
    intent_event!(),
    " ORDER BY seq DESC LIMIT ?3"
);

/// A `write_refused` row, and the index the monthly count reads them by.
macro_rules! refusal_row {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         = 'write_refused'"
    };
}

/// A `lease_granted` row and its session: the index a count's retry lookup
/// reads, so finding a session's next admission does not scan the log.
macro_rules! grant_row {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         = 'lease_granted'"
    };
}
macro_rules! grant_session {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.lease.session_id') END"
    };
}

/// Partial indexes of refusals and of admissions by session (roadmap H5).
pub(super) const COUNT_INDEXES: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_write_refused ON events(seq) WHERE ",
    refusal_row!(),
    ";\nCREATE INDEX IF NOT EXISTS events_grant_session ON events(",
    grant_session!(),
    ", seq) WHERE ",
    grant_row!(),
    ";"
);

/// The newest `write_refused` rows, newest first.
pub(super) const REFUSAL_ROWS: &str = concat!(
    "SELECT seq, event_json FROM events WHERE ",
    refusal_row!(),
    " ORDER BY seq DESC LIMIT ?1"
);

/// Whether a `write_refused` row precedes a sequence number.
pub(super) const REFUSAL_EARLIER: &str = concat!(
    "SELECT EXISTS(SELECT 1 FROM events WHERE seq < ?1 AND ",
    refusal_row!(),
    ")"
);

/// The kinds of event a definition's history reads (roadmap D4). One owner
/// for the partial index over them and the lookups that must match it.
macro_rules! history_kinds {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END
         IN ('interface_changed', 'session_prompted', 'remembered', 'forgotten', 'message_sent')"
    };
}

/// A partial index of those events, so a history reads them and not the
/// rest of the log.
pub(super) const HISTORY_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_history ON events(seq) WHERE ",
    history_kinds!()
);

/// The newest of those events before a sequence number.
pub(super) const HISTORY_BEFORE: &str = concat!(
    "SELECT seq, event_json FROM events WHERE seq < ?1 AND ",
    history_kinds!(),
    " ORDER BY seq DESC LIMIT ?2"
);

/// Notes, forgettings and messages from a sequence number on, oldest first:
/// those that follow a page's changes.
pub(super) const HISTORY_FOLLOWING: &str = concat!(
    "SELECT seq, event_json FROM events WHERE seq >= ?1 AND ",
    history_kinds!(),
    " AND json_extract(event_json, '$.type') IN ('remembered', 'forgotten', 'message_sent')
     ORDER BY seq LIMIT ?2"
);

/// The notices of files moved whole (roadmap D7): each change of one has
/// `file_moved`, so the first says it.
macro_rules! moved_kind {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') = 'interface_changed'
         AND json_extract(event_json, '$.notice.changes[0].evidence') = 'file_moved' END"
    };
}

/// A partial index of those notices, so every page of a history follows a
/// moved file whatever its cursor.
pub(super) const MOVES_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_moves ON events(seq) WHERE ",
    moved_kind!()
);

/// The newest of those notices.
pub(super) const MOVES_LOOKUP: &str = concat!(
    "SELECT seq, event_json FROM events WHERE ",
    moved_kind!(),
    " ORDER BY seq DESC LIMIT ?1"
);

/// The forgettings, one row per forgotten note.
macro_rules! forgotten_kind {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END = 'forgotten'"
    };
}

/// A partial index of the forgettings by note, so a history finds whether a
/// note it would show was forgotten however far after it that happened.
pub(super) const FORGOTTEN_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_forgotten ON events(json_extract(event_json, '$.note_id')) WHERE ",
    forgotten_kind!()
);

/// The forgettings of one note.
pub(super) const FORGETTING_LOOKUP: &str = concat!(
    "SELECT seq, event_json FROM events WHERE ",
    forgotten_kind!(),
    " AND json_extract(event_json, '$.note_id') = ?1"
);

/// The withdrawals, one row per withdrawn message.
macro_rules! retracted_kind {
    () => {
        "CASE WHEN json_valid(event_json) THEN json_extract(event_json, '$.type') END = 'message_retracted'"
    };
}

/// A partial index of the withdrawals by message, so a history finds
/// whether a message it would show was withdrawn however far after it that
/// happened.
pub(super) const RETRACTED_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS events_retracted ON events(json_extract(event_json, '$.message_id')) WHERE ",
    retracted_kind!()
);

/// The withdrawals of one message.
pub(super) const RETRACTION_LOOKUP: &str = concat!(
    "SELECT seq, event_json FROM events WHERE ",
    retracted_kind!(),
    " AND json_extract(event_json, '$.message_id') = ?1"
);

/// Every generation's clock anchor.
pub(super) const ANCHORS: &str = concat!(
    "SELECT event_json FROM events WHERE ",
    generation_marker!(),
    " ORDER BY seq"
);

/// A session's admissions after a sequence number, oldest first.
pub(super) const GRANTS_AFTER: &str = concat!(
    "SELECT event_json FROM events WHERE ",
    grant_session!(),
    " = ?1 AND seq > ?2 AND ",
    grant_row!(),
    " ORDER BY seq LIMIT ?3"
);
