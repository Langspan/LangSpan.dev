//! Holding each tool result to one frame: the encoded size, the cut pages
//! and their notes, and the error that replaces an answer too large to send.

use langspan_core::{History, Outcome, Recall};
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::json;

use super::{HISTORY_LABEL, RECALL_LABEL};
use crate::protocol::{MAX_FRAME_BYTES, Response};

/// Room left in one frame for the JSON-RPC envelope around a tool result:
/// `{"jsonrpc":"2.0","id":…,"result":…}` and the newline.
pub const ENVELOPE_ROOM_BYTES: usize = 512;

/// The start of the note a tool result carries, after its JSON text, when
/// the page in it was cut to fit one frame. The rest of the note says how
/// much was left out and how to read the rest.
pub const CUT_NOTE: &str = "LangSpan cut this page to fit one MCP reply";

/// The most bytes one tool result may take: a frame, less room for the
/// JSON-RPC envelope around it.
pub(super) const REPLY_BYTES: usize = MAX_FRAME_BYTES - ENVELOPE_ROOM_BYTES;

/// Bytes `result` takes as the adapter sends it.
pub(super) fn encoded_len(result: &CallToolResult) -> usize {
    serde_json::to_vec(result).map_or(usize::MAX, |bytes| bytes.len())
}

/// Whether `result`, as the adapter sends it, leaves `ENVELOPE_ROOM_BYTES`
/// of one frame. The one measure every tool's reply is held to.
pub fn fits_one_frame(result: &CallToolResult) -> bool {
    encoded_len(result) <= REPLY_BYTES
}

/// The tool result carrying a daemon response, as structured content and as
/// its JSON text.
pub(super) fn tool_result(response: Response) -> Result<CallToolResult, ErrorData> {
    let is_error = matches!(&response.outcome, Outcome::Error { .. });
    let value = serde_json::to_value(response).map_err(|error| {
        ErrorData::internal_error(
            format!("Could not encode the daemon response: {error}"),
            None,
        )
    })?;
    // A conflict is an ordinary coordination result. Domain failures are
    // tool errors; retain the entire shared response, including replay and
    // current lease-validity information, in either case.
    Ok(if is_error {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    })
}

/// `result` with `note` as a last text block, when there is one. The JSON
/// text stays the first block, so the structured answer is unchanged.
pub(super) fn with_note(mut result: CallToolResult, note: Option<String>) -> CallToolResult {
    if let Some(note) = note {
        result.content.push(ContentBlock::text(note));
    }
    result
}

/// The daemon response as a tool result within one frame. The daemon keeps
/// each answer within 48 KiB, but the result carries it twice, the second
/// time as a JSON string that escapes every quotation mark and backslash
/// again, so text full of them can need three times the bytes. A page that
/// does not fit is cut, by the tool's own rule, to the most that fits, and
/// says so in a note naming how to read the rest. Anything else that does
/// not fit is replaced by `within_one_frame`'s error.
pub(super) fn bounded_result(response: Response) -> Result<CallToolResult, ErrorData> {
    let request_id = response.request_id.clone();
    let whole = tool_result(response.clone())?;
    if fits_one_frame(&whole) {
        return Ok(whole);
    }
    let with = |outcome: Outcome| Response {
        outcome,
        ..response.clone()
    };
    let cut = match &response.outcome {
        Outcome::Events { events } => fitting_prefix(events, |events| {
            let note = continue_after(events.last(), events.len(), "langspan_events");
            Ok(with_note(
                tool_result(with(Outcome::Events {
                    events: events.to_vec(),
                }))?,
                note,
            ))
        })?,
        Outcome::Changes { events, scan } => fitting_prefix(events, |events| {
            let note = continue_after(events.last(), events.len(), "langspan_changes");
            Ok(with_note(
                tool_result(with(Outcome::Changes {
                    events: events.to_vec(),
                    scan: scan.clone(),
                }))?,
                note,
            ))
        })?,
        Outcome::Status {
            state,
            summary: Some(summary),
        } => {
            let note = || {
                Some(format!(
                    "{CUT_NOTE}: summary counts and names what was left out. \
                     Every record is in langspan_status with all."
                ))
            };
            let render = |state: &langspan_core::State, summary: &langspan_core::StatusSummary| {
                tool_result(with(Outcome::Status {
                    state: state.clone(),
                    summary: Some(summary.clone()),
                }))
                .map(|result| with_note(result, note()))
            };
            let (mut state, mut summary) = (state.clone(), summary.clone());
            langspan_core::shrink_status(&mut state, &mut summary, |state, summary| {
                render(state, summary).is_ok_and(|result| fits_one_frame(&result))
            });
            Some(render(&state, &summary)?)
        }
        Outcome::Recap { recap } => {
            let render = |recap: &langspan_core::Recap| {
                tool_result(with(Outcome::Recap {
                    recap: recap.clone(),
                }))
                .map(|result| {
                    with_note(
                        result,
                        Some(format!(
                            "{CUT_NOTE}: omitted_items counts what was left out. \
                             Every record is in langspan_status with all."
                        )),
                    )
                })
            };
            let mut recap = recap.clone();
            recap.shrink_until(|recap| render(recap).is_ok_and(|result| fits_one_frame(&result)));
            Some(render(&recap)?)
        }
        _ => None,
    };
    Ok(within_one_frame(cut.unwrap_or(whole), &request_id))
}

/// The note for a page of sequenced events cut to `count`, continuing after
/// the last of them.
fn continue_after(
    last: Option<&langspan_core::RecordedEvent>,
    count: usize,
    tool: &str,
) -> Option<String> {
    last.map(|last| {
        format!(
            "{CUT_NOTE}: {count} events, fewer than the CLI's page. For the rest, \
             call {tool} again with after {}.",
            last.seq
        )
    })
}

/// The result of the longest non-empty prefix of `items` whose result fits
/// one frame, found by halving; `None` when not even one item fits. Fitting
/// grows no easier with more items, so the search is exact.
pub(super) fn fitting_prefix<T>(
    items: &[T],
    render: impl Fn(&[T]) -> Result<CallToolResult, ErrorData>,
) -> Result<Option<CallToolResult>, ErrorData> {
    // The first `low` items fit (vacuously when `low` is 0); the first
    // `high` do not, and at the start `high` is the whole page, which did
    // not fit.
    let (mut low, mut high) = (0, items.len());
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if fits_one_frame(&render(&items[..middle])?) {
            low = middle;
        } else {
            high = middle;
        }
    }
    if low == 0 {
        return Ok(None);
    }
    render(&items[..low]).map(Some)
}

/// `result` if it fits one frame. Otherwise an error that does, saying how
/// large it was, what the operation reported (its `exit_code` and outcome
/// type, so a finished mutation is not mistaken for a failed one) and how to
/// read the whole answer instead.
pub(super) fn within_one_frame(result: CallToolResult, request_id: &str) -> CallToolResult {
    if fits_one_frame(&result) {
        return result;
    }
    let bytes = encoded_len(&result);
    let value = result.structured_content.as_ref();
    let field = |name: &str| value.and_then(|value| value.get(name)).cloned();
    let outcome = value
        .and_then(|value| {
            value
                .pointer("/outcome/type")
                .or_else(|| value.get("result"))
        })
        .filter(|outcome| outcome.is_string())
        .cloned();
    CallToolResult::structured_error(json!({
        "type": "adapter_error",
        "code": "reply_too_large",
        "request_id": request_id,
        "bytes": bytes,
        "limit": REPLY_BYTES,
        "outcome": outcome,
        "exit_code": field("exit_code"),
        "message": format!(
            "The answer is {bytes} bytes as an MCP reply, more than one frame of \
             {REPLY_BYTES} holds, so it was not sent. The operation itself is not undone: \
             `outcome` and `exit_code` say how it ended. Ask for less (a smaller limit or a \
             narrower request), or run the same operation with the langspan CLI and --json, \
             which carries the answer once."
        ),
    }))
}

/// The recall tool's result: the label, then the response. The daemon keeps
/// a recall within its own frame, but this result carries it twice, once as
/// a JSON string that escapes every quotation mark and backslash again, so
/// notes full of them can need more than twice the bytes. The last notes are
/// dropped, and the recall marked truncated, until the result passes the
/// shared bound, `fits_one_frame`.
pub(super) fn recall_result(response: Response) -> Result<CallToolResult, ErrorData> {
    let request_id = response.request_id.clone();
    let Outcome::Recall { recall } = &response.outcome else {
        return labelled(response, RECALL_LABEL)
            .map(|result| within_one_frame(result, &request_id));
    };
    let with = |recall: &Recall| {
        response.with_outcome(Outcome::Recall {
            recall: recall.clone(),
        })
    };
    let mut found = recall.clone();
    found.shrink_until(|recall| {
        labelled(with(recall), RECALL_LABEL).is_ok_and(|result| fits_one_frame(&result))
    });
    labelled(with(&found), RECALL_LABEL).map(|result| within_one_frame(result, &request_id))
}

/// The history tool's result, held to one frame as the recall's is: the
/// oldest entries are dropped, a whole event's at a time, and the history
/// marked truncated with a `next_seq`, until the result passes
/// `fits_one_frame`.
pub(super) fn history_result(response: Response) -> Result<CallToolResult, ErrorData> {
    let request_id = response.request_id.clone();
    let Outcome::History { history } = &response.outcome else {
        return labelled(response, HISTORY_LABEL)
            .map(|result| within_one_frame(result, &request_id));
    };
    let with = |history: &History| {
        response.with_outcome(Outcome::History {
            history: history.clone(),
        })
    };
    let mut found = history.clone();
    found.shrink_until(|history| {
        labelled(with(history), HISTORY_LABEL).is_ok_and(|result| fits_one_frame(&result))
    });
    labelled(with(&found), HISTORY_LABEL).map(|result| within_one_frame(result, &request_id))
}

/// A read's tool result with `label` placed first, unless it is an error.
pub(super) fn labelled(
    response: Response,
    label: &'static str,
) -> Result<CallToolResult, ErrorData> {
    tool_result(response).map(|mut result| {
        if result.is_error != Some(true) {
            result.content.insert(0, ContentBlock::text(label));
        }
        result
    })
}
