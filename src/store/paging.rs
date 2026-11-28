//! Read answers that page within a frame: state records, events and the
//! workspace list.

use super::{MIN_PAGE_BYTES, PAGE_BUDGET_BYTES, Store, StoreResult, shown_event};
use langspan_core::{Event, Outcome, RecordedEvent, StatusRecord, status_records};
use rusqlite::params;

/// The events a page scans beyond its limit for a client that cannot read
/// some of them: bounds the rows read and held per page (`event_page`).
const MAX_UNREAD_PER_PAGE: i64 = 10_000;

/// Bytes a status part adds around its text: the record's fields with the
/// largest offset, and the separator.
const PART_OVERHEAD_BYTES: usize = 96;

/// Bytes `text` takes as a JSON string's contents, at most: a quote or
/// backslash is escaped in two, and a control character in at most six.
fn escaped_len(text: &str) -> usize {
    text.chars().map(escaped_char_len).sum()
}

fn escaped_char_len(c: char) -> usize {
    match c {
        '"' | '\\' => 2,
        c if u32::from(c) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// Where a part of `text` that starts at `start` ends, on a character
/// boundary, so that the part fits in `room` bytes of a page.
fn part_end(text: &str, start: usize, room: usize) -> usize {
    let mut left = room.saturating_sub(PART_OVERHEAD_BYTES);
    let mut end = start;
    for c in text[start..].chars() {
        let size = escaped_char_len(c);
        if size > left {
            break;
        }
        left -= size;
        end += c.len_utf8();
    }
    end
}

/// A `workspace list` page within `PAGE_BUDGET_BYTES`: the core bounds it
/// by count, and a root can be 4 KiB, so it may end sooner here, with the
/// cursor moved back to its last workspace. It always keeps one, which a
/// root's bound lets fit.
pub(super) fn workspace_page(
    mut workspaces: Vec<langspan_core::WorkspaceSummary>,
    mut next: Option<String>,
) -> Outcome {
    let mut bytes = 0usize;
    let keep = workspaces
        .iter()
        .take_while(|workspace| {
            let size = serde_json::to_vec(workspace).map_or(usize::MAX, |json| json.len() + 1);
            let first = bytes == 0;
            bytes = bytes.saturating_add(size);
            first || bytes <= PAGE_BUDGET_BYTES
        })
        .count();
    if keep < workspaces.len() {
        workspaces.truncate(keep);
        next = workspaces.last().map(|last| last.workspace_id.clone());
    }
    Outcome::Workspaces { workspaces, next }
}

/// Identifies one record's JSON across pages, so that a page resumes inside
/// a record only if the record has not changed. Not a security control: a
/// mismatch (or a different build) starts the record again from its start.
fn digest(text: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

impl Store {
    /// One page of state records from `after`, within `budget` bytes
    /// (`PAGE_BUDGET_BYTES` unless the reader asked for less, never under
    /// `MIN_PAGE_BYTES`; `workspace_page` bounds a `workspace list` page by
    /// `PAGE_BUDGET_BYTES`). A record that fits a page is never split; one
    /// that does not (an entry at its limits can be larger than a frame) is
    /// given as parts of its JSON, so that every valid state can be read. A
    /// page holds at least one record or part when any remain.
    ///
    /// `encode` gives each record's JSON, the text a part is cut from: the
    /// daemon gives a client of an earlier protocol version the records in
    /// that version's shape (`protocol::encode_record_for`), since a part
    /// cannot be reshaped once cut. A record that fits is given whole and
    /// reshaped with the rest of the reply.
    pub fn status_page(
        &self,
        after: Option<&str>,
        page_bytes: Option<u32>,
        encode: impl Fn(&StatusRecord) -> Result<String, String>,
    ) -> Outcome {
        let budget = page_bytes.map_or(PAGE_BUDGET_BYTES, |bytes| {
            usize::try_from(bytes)
                .unwrap_or(PAGE_BUDGET_BYTES)
                .clamp(MIN_PAGE_BYTES, PAGE_BUDGET_BYTES)
        });
        self.status_records(after, budget, encode)
    }

    fn status_records(
        &self,
        after: Option<&str>,
        budget: usize,
        encode: impl Fn(&StatusRecord) -> Result<String, String>,
    ) -> Outcome {
        let (mut resume, records) = match status_records(&self.state, after) {
            Ok(read) => read,
            Err(error) => {
                return Outcome::Error {
                    code: error.code,
                    message: error.message,
                };
            }
        };
        let mut page = Vec::new();
        let mut bytes = 0;
        let mut next = None;
        for (position, record) in records {
            let text = match encode(&record) {
                Ok(text) => text,
                Err(error) => {
                    return Outcome::Error {
                        code: "internal".into(),
                        message: format!("a state record could not be encoded: {error}"),
                    };
                }
            };
            let digest = digest(&text);
            // The rest of a record the last page began, if it is unchanged.
            let start = match resume.take() {
                Some((at, point))
                    if at == position
                        && point.digest == digest
                        && point.offset < text.len()
                        && text.is_char_boundary(point.offset) =>
                {
                    point.offset
                }
                _ => 0,
            };
            if start == 0 && text.len() < budget {
                if bytes + text.len() + 1 > budget {
                    return Outcome::StatusRecords {
                        records: page,
                        next,
                    };
                }
                bytes += text.len() + 1;
                page.push(record);
                next = Some(position.after());
                continue;
            }
            let end = part_end(&text, start, budget.saturating_sub(bytes));
            if end == start {
                return Outcome::StatusRecords {
                    records: page,
                    next,
                };
            }
            let last = end == text.len();
            page.push(StatusRecord::Part {
                text: text[start..end].into(),
                offset: start as u64,
                last,
            });
            if !last {
                return Outcome::StatusRecords {
                    records: page,
                    next: Some(position.within(end, digest)),
                };
            }
            bytes += PART_OVERHEAD_BYTES + escaped_len(&text[start..end]);
            next = Some(position.after());
        }
        Outcome::StatusRecords {
            records: page,
            next: None,
        }
    }

    pub(super) fn events(&self, after: u64, limit: u32) -> StoreResult<Outcome> {
        self.event_page(after, limit, |_| true)
    }

    /// A page of events after `after` for a client that reads the events
    /// `reads` accepts (`protocol::reads_event`). An event it does not read
    /// stays in the page, so the reply can say how many it left out, but
    /// counts against neither `limit` nor the page's bytes: the client
    /// pages past it (issue #259). A page scans at most `limit` plus
    /// `MAX_UNREAD_PER_PAGE` rows, so a longer run of unread events ends
    /// it there, and a client that cannot say `withheld_events` sees an
    /// empty page.
    pub(crate) fn event_page(
        &self,
        after: u64,
        limit: u32,
        reads: impl Fn(&Event) -> bool,
    ) -> StoreResult<Outcome> {
        if !(1..=1000).contains(&limit) {
            return Ok(Outcome::Error {
                code: "invalid_limit".into(),
                message: "event limit must be between 1 and 1000".into(),
            });
        }
        let after = i64::try_from(after).unwrap_or(i64::MAX);
        let mut statement = self
            .connection
            .prepare("SELECT seq, event_json FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
        let scan = i64::from(limit) + MAX_UNREAD_PER_PAGE;
        let mut rows = statement.query(params![after, scan])?;
        let mut events = Vec::new();
        let (mut page_bytes, mut read, mut ends) = (0, 0, 0);
        while read < limit
            && let Some(row) = rows.next()?
        {
            let sequence: i64 = row.get(0)?;
            let stored: String = row.get(1)?;
            let (event, json) = shown_event(serde_json::from_str(&stored)?, stored)?;
            if !reads(&event) {
                events.push(RecordedEvent {
                    seq: u64::try_from(sequence)?,
                    event,
                });
                continue;
            }
            if page_bytes + json.len() + 64 > 48 * 1024 {
                if read == 0 {
                    return Err("one event exceeds the transport page budget".into());
                }
                break;
            }
            page_bytes += json.len() + 64;
            events.push(RecordedEvent {
                seq: u64::try_from(sequence)?,
                event,
            });
            read += 1;
            ends = events.len();
        }
        // The client asks next from the last event it read, so unread events
        // after it are left to that page, which tells of them once.
        if read > 0 {
            events.truncate(ends);
        }
        Ok(Outcome::Events { events })
    }
}
