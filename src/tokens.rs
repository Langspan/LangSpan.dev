//! `langspan tokens` (#493): Claude Code tokens per merged change, by local
//! day, to watch the token overhead of the whole process over time.
//!
//! The method is deterministic: the same transcripts, history and zone give
//! the same answer, whatever order the files are read in.
//!
//! - Tokens are the `usage` of each `assistant` line in the `.jsonl`
//!   transcripts under the Claude Code folder's `projects`. Claude Code
//!   writes one line per content block, each repeating the message's usage,
//!   and a resumed session copies earlier lines into its new file, so each
//!   message ID counts once: each count its largest value, at the message's
//!   earliest time. Nothing but those counts, IDs and times is kept.
//! - A merged change is a first-parent commit of the branch, on the local
//!   day of its committer time. One that changes an `AGENTS.md`, a
//!   `CLAUDE.md` or a file under `.claude/` marks a process change.
//! - A day's figure per change is its tokens divided by its merged changes,
//!   rounded down; a day without one has none.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{Error, git, today, zone::Zone};

pub const DEFAULT_DAYS: i64 = 14;
pub const MAX_DAYS: i64 = 90;
/// Longer lines (large tool output) are skipped unread and counted.
const LINE_LIMIT: usize = 4 << 20;
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 4;
const GIT_LIMIT: usize = 16 << 20;
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
const DAY_MS: i64 = 86_400_000;

/// What the figures leave out or mix in; every answer carries them.
const CONFOUNDERS: [&str; 4] = [
    "Tokens are every Claude Code session in this folder, including work on other repositories and work never merged.",
    "A merged change is one first-parent commit; changes differ in size and difficulty.",
    "The trend reflects the whole process (models, prompts, tasks, people), not LangSpan alone; the coordination benchmark is the controlled measurement.",
    "Other hosts' sessions and deleted transcripts are not counted.",
];

/// One message's usage counts, as Claude Code records them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

impl Usage {
    /// Input not served from the cache: fresh input and cache writes.
    fn uncached_input(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
    }

    fn total(&self) -> u64 {
        self.uncached_input()
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.output_tokens)
    }

    fn max(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.max(other.input_tokens),
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .max(other.cache_creation_input_tokens),
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .max(other.cache_read_input_tokens),
            output_tokens: self.output_tokens.max(other.output_tokens),
        }
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    usage: Option<Usage>,
}

/// Each message's earliest time and usage, by message ID.
#[derive(Debug, Default)]
struct Tally {
    messages: BTreeMap<String, (u64, Usage)>,
    /// Lines with a usage that could not be read: not JSON (a line still
    /// being written), over the line limit, or without a message ID or time.
    unread_lines: u64,
}

impl Tally {
    /// Count one transcript line; `None` is a line over the limit.
    fn add(&mut self, line: Option<&[u8]>) {
        let Some(line) = line else {
            self.unread_lines += 1;
            return;
        };
        if !line.windows(7).any(|window| window == b"\"usage\"") {
            return;
        }
        let Ok(parsed) = serde_json::from_slice::<Line>(line) else {
            self.unread_lines += 1;
            return;
        };
        let Some(Message {
            id,
            usage: Some(usage),
        }) = parsed.message
        else {
            return;
        };
        if parsed.kind.as_deref() != Some("assistant") {
            return;
        }
        let at = parsed
            .timestamp
            .as_deref()
            .and_then(|time| today::parse_rfc3339(time).ok());
        let (Some(id), Some(at)) = (id, at) else {
            self.unread_lines += 1;
            return;
        };
        self.messages
            .entry(id)
            .and_modify(|(seen, counted)| {
                *seen = (*seen).min(at);
                *counted = counted.max(usage);
            })
            .or_insert((at, usage));
    }
}

/// A first-parent commit: its committer time and the process files it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Merge {
    at_ms: u64,
    process_files: Vec<String>,
}

/// `git log -z --format=%x00%ct --name-only` output as merges. Each commit
/// is an empty field, its time, then its paths, the first after a newline;
/// a path is never empty, so the empty field can only start a commit. The
/// final NUL leaves one more empty field, which is the end.
fn merges(log: &[u8]) -> Result<Vec<Merge>, Error> {
    let mut found: Vec<Merge> = Vec::new();
    let mut fields = log.split(|byte| *byte == 0).peekable();
    while let Some(field) = fields.next() {
        if field.is_empty() {
            if fields.peek().is_none() {
                break;
            }
            let seconds: u64 = fields
                .next()
                .and_then(|time| std::str::from_utf8(time).ok()?.parse().ok())
                .ok_or("Git's log had a commit without a committer time")?;
            found.push(Merge {
                at_ms: seconds.saturating_mul(1000),
                process_files: Vec::new(),
            });
            if let Some(first) = fields.next_if(|path| !path.is_empty()) {
                note(&mut found, first.strip_prefix(b"\n").unwrap_or(first));
            }
        } else {
            note(&mut found, field);
        }
    }
    Ok(found)
}

/// Add `path` to the last merge's process files if it is one.
fn note(found: &mut [Merge], path: &[u8]) {
    let path = String::from_utf8_lossy(path);
    let name = path.rsplit('/').next().unwrap_or(&path);
    let process = name == "AGENTS.md"
        || name == "CLAUDE.md"
        || path.starts_with(".claude/")
        || path.contains("/.claude/");
    if let (true, Some(merge)) = (process, found.last_mut()) {
        merge.process_files.push(path.into_owned());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct Tokens {
    total: u64,
    uncached_input: u64,
    output: u64,
}

/// One local day of the trend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Day {
    /// `YYYY-MM-DD`, local.
    day: String,
    merged: u64,
    tokens: Tokens,
    per_change: Option<Tokens>,
    process_changes: Vec<String>,
}

/// The days `first..=last` (days since 1970-01-01, local), with `day_of`
/// giving an instant's local day.
fn trend(
    tally: &Tally,
    merged: &[Merge],
    (first, last): (i64, i64),
    day_of: impl Fn(u64) -> i64,
) -> Vec<Day> {
    let mut usage: BTreeMap<i64, Usage> = BTreeMap::new();
    for (at, counted) in tally.messages.values() {
        let sum = usage.entry(day_of(*at)).or_default();
        sum.input_tokens = sum.input_tokens.saturating_add(counted.input_tokens);
        sum.cache_creation_input_tokens = sum
            .cache_creation_input_tokens
            .saturating_add(counted.cache_creation_input_tokens);
        sum.cache_read_input_tokens = sum
            .cache_read_input_tokens
            .saturating_add(counted.cache_read_input_tokens);
        sum.output_tokens = sum.output_tokens.saturating_add(counted.output_tokens);
    }
    let mut changes: BTreeMap<i64, (u64, BTreeSet<String>)> = BTreeMap::new();
    for merge in merged {
        let entry = changes.entry(day_of(merge.at_ms)).or_default();
        entry.0 += 1;
        entry.1.extend(merge.process_files.iter().cloned());
    }
    (first..=last)
        .map(|day| {
            let sum = usage.get(&day).copied().unwrap_or_default();
            let tokens = Tokens {
                total: sum.total(),
                uncached_input: sum.uncached_input(),
                output: sum.output_tokens,
            };
            let (merged, process) = changes.remove(&day).unwrap_or_default();
            let (year, month, date) = today::civil_from_days(day);
            Day {
                day: format!("{year:04}-{month:02}-{date:02}"),
                merged,
                tokens,
                per_change: (merged > 0).then(|| Tokens {
                    total: tokens.total / merged,
                    uncached_input: tokens.uncached_input / merged,
                    output: tokens.output / merged,
                }),
                process_changes: process.into_iter().collect(),
            }
        })
        .collect()
}

/// What `tokens --json` prints.
#[derive(Debug, Serialize)]
pub struct Trend {
    onto: String,
    transcripts: String,
    files: usize,
    messages: usize,
    unread_lines: u64,
    days: Vec<Day>,
    confounders: [&'static str; 4],
}

/// Read the transcripts under `claude_dir` and the first-parent history of
/// `onto` in `root`, for the `days` local days to the one holding `now_ms`.
pub fn measure(
    claude_dir: &Path,
    root: &Path,
    onto: &str,
    days: i64,
    now_ms: u64,
) -> Result<Trend, Error> {
    if onto.is_empty() || onto.starts_with('-') {
        return Err(format!("--onto {onto:?} is not a branch name").into());
    }
    let zone = Zone::load(None)
        .ok_or("the system's time zone rules could not be read, so no local day can be told")?;
    let day_of = |at: u64| {
        let local =
            i64::try_from(at).unwrap_or(i64::MAX) + i64::from(zone.offset_minutes(at)) * 60_000;
        local.div_euclid(DAY_MS)
    };
    let last = day_of(now_ms);
    let first = last - (days - 1);
    // A day before the window's first, so no local day is cut short.
    let since_ms = u64::try_from((first - 1) * DAY_MS).unwrap_or(0);
    let projects = claude_dir.join("projects");
    if !projects.is_dir() {
        return Err(format!(
            "no Claude Code transcripts at {}: set CLAUDE_CONFIG_DIR to the folder Claude Code signs in with",
            projects.display()
        )
        .into());
    }
    let since = UNIX_EPOCH + Duration::from_millis(since_ms);
    let mut files = Vec::new();
    collect(&projects, since, 0, &mut files)?;
    files.sort();
    let mut tally = Tally::default();
    for file in &files {
        let opened = std::fs::File::open(file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        each_line(BufReader::new(opened), |line| tally.add(line))
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    }
    // `--since-as-filter`, not `--since`: commit dates can go backwards, and
    // `--since` stops the walk at the first older one. `--no-renames` keeps a
    // renamed instruction file's old path; `-z` writes paths unquoted.
    let since_arg = format!("--since-as-filter=@{}", since_ms / 1000);
    let log = git::output(
        root,
        &[
            "log",
            "-z",
            "--first-parent",
            "--diff-merges=first-parent",
            "--no-renames",
            "--format=%x00%ct",
            "--name-only",
            &since_arg,
            "--end-of-options",
            onto,
            "--",
        ],
        Instant::now() + GIT_TIMEOUT,
        GIT_LIMIT,
    )
    .map_err(|error| {
        format!(
            "cannot read the history of {onto} in {}: {error}; check that --onto names a branch there and --root the repository",
            root.display()
        )
    })?;
    let days = trend(&tally, &merges(&log)?, (first, last), day_of);
    Ok(Trend {
        onto: onto.to_owned(),
        transcripts: projects.display().to_string(),
        files: files.len(),
        messages: tally.messages.len(),
        unread_lines: tally.unread_lines,
        days,
        confounders: CONFOUNDERS,
    })
}

/// The `.jsonl` files under `dir` changed at or after `since`, without
/// following links.
fn collect(
    dir: &Path,
    since: SystemTime,
    depth: usize,
    files: &mut Vec<PathBuf>,
) -> Result<(), Error> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot list {}: {error}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() && depth < MAX_DEPTH {
            collect(&path, since, depth + 1, files)?;
        } else if meta.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            && meta.modified()? >= since
        {
            if files.len() == MAX_FILES {
                return Err(format!(
                    "more than {MAX_FILES} transcripts changed in the window under {}; ask for fewer --days",
                    dir.display()
                )
                .into());
            }
            files.push(path);
        }
    }
    Ok(())
}

/// Call `line` with each line of `reader`, or `None` for one over
/// `LINE_LIMIT`, which is never held in memory whole.
fn each_line(mut reader: impl BufRead, mut line: impl FnMut(Option<&[u8]>)) -> io::Result<()> {
    let mut held = Vec::new();
    let mut over = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if over || !held.is_empty() {
                line((!over).then_some(held.as_slice()));
            }
            return Ok(());
        }
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let (chunk, _) = buffer.split_at(end.unwrap_or(buffer.len()));
        if !over && held.len() + chunk.len() > LINE_LIMIT {
            over = true;
            held.clear();
        } else if !over {
            held.extend_from_slice(chunk);
        }
        let used = chunk.len() + usize::from(end.is_some());
        reader.consume(used);
        if end.is_some() {
            line((!over).then_some(held.as_slice()));
            held.clear();
            over = false;
        }
    }
}

/// `25.4M`.
fn millions(count: u64) -> String {
    let tenths = count.saturating_add(50_000) / 100_000;
    format!("{}.{}M", tenths / 10, tenths % 10)
}

impl Trend {
    pub fn text(&self) -> String {
        let mut lines = vec![format!(
            "Claude Code tokens per merged change on {}, by local day",
            self.onto
        )];
        for day in &self.days {
            let mut line = match &day.per_change {
                Some(per) => format!(
                    "{}: {} merged, {} tokens per change ({} uncached input, {} output)",
                    day.day,
                    day.merged,
                    millions(per.total),
                    millions(per.uncached_input),
                    millions(per.output)
                ),
                None => format!(
                    "{}: no merged change, {} tokens",
                    day.day,
                    millions(day.tokens.total)
                ),
            };
            if !day.process_changes.is_empty() {
                line.push_str(&format!(
                    "  [process change: {}]",
                    day.process_changes.join(", ")
                ));
            }
            lines.push(line);
        }
        lines.push(format!(
            "Read the usage counts of {} messages in {} transcripts under {}; no prompts, code or credentials.{}",
            self.messages,
            self.files,
            self.transcripts,
            if self.unread_lines > 0 {
                format!(" {} lines with a usage could not be read.", self.unread_lines)
            } else {
                String::new()
            }
        ));
        lines.push("Confounders:".to_owned());
        lines.extend(CONFOUNDERS.iter().map(|text| format!("- {text}")));
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first four messages of a real Claude Code session of 2026-11-25
    /// (each written twice, as Claude Code does) and the user lines between
    /// them, with every field but the type, time, request and message IDs,
    /// model and usage removed.
    const SLICE: &str = include_str!("../tests/fixtures/tokens/claude-transcript-slice.jsonl");

    fn nov_25() -> i64 {
        today::days_from_civil(2026, 11, 25)
    }

    fn noon(day: i64) -> u64 {
        u64::try_from(day * DAY_MS + DAY_MS / 2).unwrap()
    }

    fn utc_day(at: u64) -> i64 {
        i64::try_from(at).unwrap_or(i64::MAX).div_euclid(DAY_MS)
    }

    fn tally(text: &str) -> Tally {
        let mut tally = Tally::default();
        each_line(text.as_bytes(), |line| tally.add(line)).unwrap();
        tally
    }

    #[test]
    fn recorded_slice_counts_each_message_once() {
        let tally = tally(SLICE);
        assert_eq!(tally.messages.len(), 4);
        assert_eq!(tally.unread_lines, 0);
        let merged = [
            Merge {
                at_ms: noon(nov_25()),
                process_files: vec![],
            },
            Merge {
                at_ms: noon(nov_25()) + 1,
                process_files: vec!["AGENTS.md".into()],
            },
            Merge {
                at_ms: noon(nov_25() + 1),
                process_files: vec![],
            },
        ];
        let days = trend(&tally, &merged, (nov_25() - 1, nov_25()), utc_day);
        assert_eq!(days.len(), 2);
        let day = &days[1];
        assert_eq!(day.day, "2026-11-25");
        assert_eq!(
            day.tokens,
            Tokens {
                total: 187_597,
                uncached_input: 61_997,
                output: 619
            }
        );
        assert_eq!(
            day.per_change,
            Some(Tokens {
                total: 93_798,
                uncached_input: 30_998,
                output: 309
            })
        );
        assert_eq!(day.process_changes, ["AGENTS.md"]);
        assert_eq!(days[0].merged, 0);
        assert_eq!(days[0].per_change, None);
    }

    #[test]
    fn file_order_and_a_resumed_copy_do_not_change_the_count() {
        let reversed: Vec<&str> = SLICE.lines().rev().collect();
        let twice = format!("{SLICE}{}\n", reversed.join("\n"));
        let window = (nov_25(), nov_25());
        let one = trend(&tally(SLICE), &[], window, utc_day);
        assert_eq!(one[0].tokens.total, 187_597);
        assert_eq!(trend(&tally(&twice), &[], window, utc_day), one);
    }

    #[test]
    fn unreadable_and_oversized_lines_are_counted_not_guessed() {
        let mut text = String::from("{\"type\":\"assistant\",\"usage\":\n");
        text.push_str(&format!("{{\"usage\":\"{}\"}}\n", "x".repeat(LINE_LIMIT)));
        text.push_str("{\"type\":\"user\",\"message\":{\"usage\":{\"output_tokens\":9}}}\n");
        let tally = tally(&text);
        assert_eq!(tally.unread_lines, 2);
        assert!(tally.messages.is_empty());
    }

    #[test]
    fn log_records_mark_instruction_changes() {
        // The layout `git log -z` writes: an empty commit, then paths that
        // are unquoted and may hold a newline, ending in a NUL.
        let log = "\x001795000002\0\x001795000000\0\nsrc/a.rs\0é/CLAUDE.md\0\
                   \x001795000001\0\n\n.claude/x\0src/AGENTS.mdx\0a\n/AGENTS.md\0.claude/settings.json\0";
        let found = merges(log.as_bytes()).unwrap();
        assert_eq!(found.len(), 3);
        assert!(found[0].process_files.is_empty());
        assert_eq!(found[0].at_ms, 1_795_000_002_000);
        assert_eq!(found[1].process_files, ["é/CLAUDE.md"]);
        assert_eq!(
            found[2].process_files,
            ["a\n/AGENTS.md", ".claude/settings.json"]
        );
        assert!(merges(b"").unwrap().is_empty());
        assert!(merges(b"\0not a time\0").is_err());
    }
}
