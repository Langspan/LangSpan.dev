//! `langspan statusline`: one line for the person (roadmap H2), run by
//! Claude Code's `statusLine` setting after each message, or by a shell
//! prompt or terminal multiplexer with `--root DIR`.
//!
//! It asks the running coordinator once, within its deadline, and prints the
//! answer. It never starts the coordinator, never touches the network, and
//! writes nothing: when the coordinator is not running or does not answer in
//! time it prints the unavailable line and exits 0. Nothing it prints reaches
//! the model, and it shows counts, never paths.

use crate::protocol::{PROTOCOL_VERSION, Request};
use langspan_core::{Command, Outcome, StatusLine};
use serde_json::{Value, json};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The default time the whole command may take.
pub const DEFAULT_DEADLINE_MS: u64 = 200;
/// The deadline's allowed range, as for other coordinator calls.
pub const DEADLINE_RANGE_MS: std::ops::RangeInclusive<u64> = 100..=5000;
/// The most status-line input read from the host.
const MAX_INPUT_BYTES: usize = 64 * 1024;
/// What is printed when the coordinator cannot answer.
pub const UNAVAILABLE: &str = "langspan · coordinator not running";

/// The answer: the line, or that the coordinator could not give one.
pub enum Answer {
    Line(StatusLine),
    Unavailable,
}

impl Answer {
    pub fn text(&self) -> String {
        match self {
            Self::Line(line) => line.text(),
            Self::Unavailable => UNAVAILABLE.into(),
        }
    }

    pub fn json(&self) -> Value {
        match self {
            Self::Line(line) => json!({
                "workspace_id": line.workspace_id,
                "sessions_here": line.sessions_here,
                "refusals_today": line.refusals_today,
                "coordinator": "running",
            }),
            Self::Unavailable => json!({
                "workspace_id": null,
                "sessions_here": null,
                "refusals_today": null,
                "coordinator": "unavailable",
            }),
        }
    }
}

/// The directory the line is for: `--root`, else the host's `cwd` from its
/// status-line input on stdin (Claude Code sends `workspace.current_dir`
/// and `cwd`), else the current directory. Stdin is read only when it is not
/// a terminal and `--root` is absent, and never past the deadline.
pub fn directory(root: Option<PathBuf>, deadline: Instant) -> Option<PathBuf> {
    if let Some(root) = root {
        return Some(root);
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal()
        && let Ok(bytes) = crate::hook::read_stdin_until(deadline)
        && bytes.len() <= MAX_INPUT_BYTES
        && let Some(cwd) = input_directory(&bytes)
    {
        return Some(cwd);
    }
    std::env::current_dir().ok()
}

/// The working directory named in a host's status-line input, if any.
pub fn input_directory(bytes: &[u8]) -> Option<PathBuf> {
    let input: Value = serde_json::from_slice(bytes).ok()?;
    let named = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty() && text.len() <= 4096)
            .map(PathBuf::from)
    };
    named(input.pointer("/workspace/current_dir")).or_else(|| named(input.get("cwd")))
}

/// Ask the coordinator for the line of `dir`, counting refusals from
/// `from_ms` (wall clock). Any failure, including the deadline, is
/// `Unavailable`.
pub fn ask(state_dir: &Path, dir: &Path, from_ms: u64, deadline: Instant) -> Answer {
    let root = std::fs::canonicalize(dir)
        .or_else(|_| std::path::absolute(dir))
        .unwrap_or_else(|_| dir.to_path_buf());
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::StatusLine {
            root: root.to_string_lossy().into_owned(),
            from_ms,
        },
    };
    match crate::client::call_until(state_dir, &request, deadline) {
        Ok(response) => match response.outcome {
            Outcome::StatusLine { line } => Answer::Line(line),
            _ => Answer::Unavailable,
        },
        Err(_) => Answer::Unavailable,
    }
}

/// The whole command: the answer within `budget` of `started`.
pub fn run(state_dir: &Path, root: Option<PathBuf>, budget: Duration, started: Instant) -> Answer {
    let deadline = started + budget;
    let Some(dir) = directory(root, deadline) else {
        return Answer::Unavailable;
    };
    let now = crate::store::wall_now_ms();
    let from_ms = midnight_ms(now, local_offset_minutes(deadline).unwrap_or(0));
    ask(state_dir, &dir, from_ms, deadline)
}

/// The local offset from UTC in minutes, from `date +%z` (`+0100`,
/// `-0530`): the system's own time-zone rules, honouring `TZ`. The child is
/// stopped at the deadline.
pub fn local_offset_minutes(deadline: Instant) -> Option<i32> {
    let mut child = std::process::Command::new("date")
        .arg("+%z")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut text).ok()?;
    parse_offset(text.trim())
}

/// `+HHMM` or `-HHMM` as minutes east of UTC.
pub fn parse_offset(text: &str) -> Option<i32> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 60 + minutes))
}

/// The last local midnight at or before `now_ms`, at a fixed offset.
pub fn midnight_ms(now_ms: u64, offset_minutes: i32) -> u64 {
    let offset = i64::from(offset_minutes) * 60_000;
    let local = i64::try_from(now_ms)
        .unwrap_or(i64::MAX)
        .saturating_add(offset);
    let midnight = local - local.rem_euclid(86_400_000) - offset;
    u64::try_from(midnight).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_parse_and_midnight_is_local() {
        assert_eq!(parse_offset("+0100"), Some(60));
        assert_eq!(parse_offset("-0530"), Some(-330));
        assert_eq!(parse_offset("+0000"), Some(0));
        for bad in ["", "0100", "+01", "+01000", "+2460", "+0160", "+01a0"] {
            assert_eq!(parse_offset(bad), None, "{bad:?}");
        }
        // 2026-11-25 10:00 UTC.
        let now = 1_795_600_800_000;
        let utc_midnight = 1_795_564_800_000;
        assert_eq!(midnight_ms(now, 0), utc_midnight);
        // At UTC-06:00 it is 04:00 on the same day: midnight is 06:00 UTC.
        assert_eq!(midnight_ms(now, -360), utc_midnight + 6 * 3_600_000);
        // At UTC+14:00 it is 00:00 on the next day exactly.
        assert_eq!(midnight_ms(now, 840), now);
        // One millisecond before that it is still the previous day.
        assert_eq!(midnight_ms(now - 1, 840), now - 86_400_000);
    }

    #[test]
    fn the_input_directory_prefers_the_workspace_then_cwd() {
        let both = br#"{"cwd":"/a","workspace":{"current_dir":"/b"}}"#;
        assert_eq!(input_directory(both), Some(PathBuf::from("/b")));
        assert_eq!(
            input_directory(br#"{"cwd":"/a"}"#),
            Some(PathBuf::from("/a"))
        );
        for none in [&b"{}"[..], b"not json", br#"{"cwd":""}"#, br#"{"cwd":7}"#] {
            assert_eq!(input_directory(none), None);
        }
    }

    #[test]
    fn the_unavailable_answer_has_no_counts() {
        let answer = Answer::Unavailable;
        assert_eq!(answer.text(), UNAVAILABLE);
        assert_eq!(answer.json()["coordinator"], "unavailable");
        assert!(answer.json()["sessions_here"].is_null());
    }
}
