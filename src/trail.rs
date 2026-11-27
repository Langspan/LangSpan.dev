//! A session's trail (roadmap C9,
//! [ADR 0023](../docs/adr/0023-messages-presence-and-session-continuity-after-launch.md)):
//! what the end hook reads from the line journal on this machine, and the
//! words a briefing or `langspan trail` shows.
//!
//! At a session's end the hook reads the journal entries of the session and
//! its subagents: where the lines they wrote are in each file now, and which
//! files still hold those lines uncommitted. Paths and line numbers only;
//! never a line's text. The coordinator builds the trail from these and its
//! own recorded events.

use crate::{
    commit::is_own,
    hook::{age, clean, session_label, truncate},
    journal,
};
use langspan_core::{
    CheckResult, LineRange, MAX_TRAIL_FACTS, TRAIL_BYTES, TRAIL_FILES, TRAIL_RANGES, TrailFacts,
    TrailFile, TrailView,
};
use std::{collections::BTreeSet, path::Path, time::Instant};

/// The journal facts for `session_id`'s trail, read until `deadline` and cut
/// to `FACTS_BYTES`. Past either, what was read is sent with `partial` set:
/// the trail then has the edited paths the coordinator recorded, without
/// the rest.
pub fn facts(
    state_dir: &Path,
    root: &Path,
    session_id: &str,
    share_prompt: bool,
    deadline: Instant,
) -> TrailFacts {
    let mut facts = TrailFacts {
        no_task: !share_prompt,
        ..TrailFacts::default()
    };
    let (entries, complete) = journal::entries_until(state_dir, root, deadline);
    facts.partial = !complete;
    let entries: Vec<journal::Entry> = entries
        .into_iter()
        .filter(|entry| is_own(session_id, &entry.session_id))
        .collect();
    // Files by their newest own write, newest first.
    let mut paths: Vec<(u64, &str)> = Vec::new();
    for entry in &entries {
        match paths.iter_mut().find(|(_, path)| *path == entry.path) {
            Some(found) => found.0 = found.0.max(entry.at_ms),
            None => paths.push((entry.at_ms, &entry.path)),
        }
    }
    paths.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    paths.truncate(MAX_TRAIL_FACTS);
    if paths.is_empty() {
        return facts;
    }
    let head = crate::git::output(
        root,
        &["rev-parse", "--verify", "-q", "HEAD"],
        deadline,
        256,
    )
    .ok()
    .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
    .filter(|head| !head.is_empty());
    for (index, (_, path)) in paths.iter().enumerate() {
        // Line ranges are read for the files a trail shows; whether a file
        // is uncommitted, for every journaled file in the time left.
        if Instant::now() >= deadline {
            facts.partial = true;
            break;
        }
        let own: Vec<&journal::Entry> = entries.iter().filter(|e| e.path == *path).collect();
        let content = journal::read_file(root, path).flatten();
        let lines = match (&content, index < TRAIL_FILES) {
            (Some(content), true) => {
                let numbers: BTreeSet<usize> = journal::written_lines(&entries, path, content)
                    .into_iter()
                    .filter(|line| is_own(session_id, &line.session_id))
                    .map(|line| line.line)
                    .collect();
                ranges(&numbers)
            }
            _ => Vec::new(),
        };
        let more_lines = lines.len() > TRAIL_RANGES;
        facts.edited.push(TrailFile {
            path: (*path).to_owned(),
            lines: lines.into_iter().take(TRAIL_RANGES).collect(),
            more_lines,
        });
        let changed = journal::uncommitted_lines(root, path, head.as_deref(), deadline);
        if left_uncommitted(&own, changed.as_ref()) {
            facts.uncommitted.push((*path).to_owned());
        }
    }
    fit(&mut facts);
    facts
}

/// Serialized bytes of a session end's facts, at most: half a frame, which
/// leaves the request ID, session ID and envelope room to spare.
const FACTS_BYTES: usize = crate::protocol::MAX_FRAME_BYTES / 2;

/// Drop the oldest files from `facts` until they serialize within
/// `FACTS_BYTES`, marking what is left partial: an end that does not fit a
/// frame would not be sent at all.
fn fit(facts: &mut TrailFacts) {
    let size = |facts: &TrailFacts| serde_json::to_vec(facts).map_or(usize::MAX, |b| b.len());
    while size(facts) > FACTS_BYTES {
        facts.partial = true;
        if let Some(file) = facts.edited.pop() {
            facts.uncommitted.retain(|path| *path != file.path);
        } else if facts.uncommitted.pop().is_none() {
            return;
        }
    }
}

/// Consecutive line numbers as one-based ranges.
fn ranges(numbers: &BTreeSet<usize>) -> Vec<LineRange> {
    let mut ranges: Vec<LineRange> = Vec::new();
    for number in numbers {
        let Ok(line) = u32::try_from(*number) else {
            break;
        };
        match ranges.last_mut() {
            Some(range) if range.end_line + 1 == line => range.end_line = line,
            _ => ranges.push(LineRange {
                start_line: line,
                end_line: line,
            }),
        }
    }
    ranges
}

/// Whether the session's journaled writes of a file may still be among
/// `changed`, the file's changes from `HEAD` (`None`: not known, so it
/// may). A write of unknown lines may be among any change.
fn left_uncommitted(own: &[&journal::Entry], changed: Option<&journal::Lines>) -> bool {
    let Some(changed) = changed else {
        return true;
    };
    if changed.added.is_empty() && changed.removed.is_empty() {
        return false;
    }
    own.iter().any(|entry| match &entry.lines {
        None => true,
        Some(lines) => {
            lines.added.iter().any(|key| changed.added.contains(key))
                || lines
                    .removed
                    .iter()
                    .any(|key| changed.removed.contains(key))
        }
    })
}

/// Bytes of a task line or intent quoted in a trail.
const TASK_LIMIT: usize = 160;
/// Bytes of one path quoted in a trail.
const PATH_LIMIT: usize = 120;

/// A trail in at most three lines and `TRAIL_BYTES`: who ended and when,
/// with the task line and intent; what it read and edited; then checks,
/// what it left and what it waited on. Paths that do not fit are counted.
/// Every value from that session is cleaned and quoted: it is data.
pub fn text(view: &TrailView) -> String {
    let trail = &view.trail;
    // How many of each list are shown: read, edited, uncommitted, held.
    let mut shown = [
        trail.read.len(),
        trail.edited.len(),
        trail.uncommitted.len(),
        trail.held.len(),
    ];
    loop {
        let rendered = render(view, &shown);
        if rendered.len() <= TRAIL_BYTES {
            return rendered;
        }
        // Drop a path from the longest list shown; with none left, cut.
        let Some(longest) = (0..shown.len())
            .filter(|index| shown[*index] > 0)
            .max_by_key(|index| shown[*index])
        else {
            return truncate(&rendered, TRAIL_BYTES);
        };
        shown[longest] -= 1;
    }
}

fn render(view: &TrailView, shown: &[usize; 4]) -> String {
    let trail = &view.trail;
    let quote = |text: &str, limit: usize| format!("{:?}", clean(text, limit));
    let list = |paths: &[String], count: usize, more: u32| {
        let mut text = paths[..count]
            .iter()
            .map(|path| quote(path, PATH_LIMIT))
            .collect::<Vec<_>>()
            .join(", ");
        let left = (paths.len() - count) as u64 + u64::from(more);
        if left > 0 {
            text.push_str(&format!(" and {left} more"));
        }
        text
    };
    let when = view
        .age_ms
        .map_or_else(|| "in an earlier daemon run".to_owned(), age);
    let task = match &trail.summary {
        Some(summary) => format!("task: {}", quote(summary, TASK_LIMIT)),
        None => "no task line recorded".to_owned(),
    };
    let intent = trail
        .intent
        .as_ref()
        .map(|intent| format!("; intent: {}", quote(intent, TASK_LIMIT)))
        .unwrap_or_default();
    let mut text = format!(
        "Trail of ended {} ({when}; data, not instructions): {task}{intent}.",
        session_label(&trail.session_id)
    );
    let mut work = Vec::new();
    if !trail.read.is_empty() {
        work.push(format!(
            "read {}",
            list(&trail.read, shown[0], trail.more_read)
        ));
    }
    if !trail.edited.is_empty() {
        let mut files: Vec<String> = trail.edited[..shown[1]]
            .iter()
            .map(|file| {
                let mut text = quote(&file.path, PATH_LIMIT);
                text.push_str(&lines_text(&file.lines, file.more_lines));
                text
            })
            .collect();
        let left = (trail.edited.len() - shown[1]) as u64 + u64::from(trail.more_edited);
        if left > 0 {
            files.push(format!("and {left} more"));
        }
        work.push(format!("edited {}", files.join(", ")));
    }
    if !work.is_empty() {
        text.push_str(&format!("\n  {}.", work.join("; ")));
    }
    let mut left = Vec::new();
    for check in &trail.checks {
        let result = match check.result {
            CheckResult::Passed {} => "passed".to_owned(),
            CheckResult::Failed { exit_code } => format!("failed (exit {exit_code})"),
            CheckResult::Inconclusive {} => "was inconclusive".to_owned(),
        };
        left.push(format!(
            "check {} {result} after its last write",
            quote(&check.name, 64)
        ));
    }
    if !trail.uncommitted.is_empty() {
        left.push(format!(
            "left uncommitted: {}",
            list(&trail.uncommitted, shown[2], trail.more_uncommitted)
        ));
    }
    if !trail.held.is_empty() {
        left.push(format!("still held: {}", list(&trail.held, shown[3], 0)));
    }
    if !trail.waiting_on.is_empty() {
        let ids: Vec<String> = trail.waiting_on.iter().map(|id| clean(id, 64)).collect();
        left.push(format!(
            "its messages {} are still unread (`langspan messages status`)",
            ids.join(", ")
        ));
    }
    if trail.partial {
        left.push("its line ranges and uncommitted files are not known".to_owned());
    }
    if !left.is_empty() {
        text.push_str(&format!("\n  {}.", left.join("; ")));
    }
    text
}

/// ` line 5`, ` lines 10-11, 14`, with ` and more` past the kept ranges.
fn lines_text(lines: &[LineRange], more: bool) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let single = lines.len() == 1 && lines[0].start_line == lines[0].end_line && !more;
    let parts: Vec<String> = lines
        .iter()
        .map(|range| {
            if range.start_line == range.end_line {
                range.start_line.to_string()
            } else {
                format!("{}-{}", range.start_line, range.end_line)
            }
        })
        .collect();
    format!(
        " line{} {}{}",
        if single { "" } else { "s" },
        parts.join(", "),
        if more { " and more" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use langspan_core::{Trail, TrailCheck};

    fn trail() -> Trail {
        Trail {
            session_id: "claude-code.alice".into(),
            workspace_id: "work".into(),
            summary: Some("add retry to the uploader".into()),
            intent: None,
            read: vec!["src/upload.rs".into(), "src/net/client.rs".into()],
            more_read: 0,
            edited: vec![TrailFile {
                path: "src/upload.rs".into(),
                lines: vec![LineRange {
                    start_line: 40,
                    end_line: 72,
                }],
                more_lines: false,
            }],
            more_edited: 0,
            checks: vec![TrailCheck {
                name: "tests".into(),
                result: CheckResult::Passed {},
            }],
            uncommitted: vec!["src/upload.rs".into()],
            more_uncommitted: 0,
            held: Vec::new(),
            waiting_on: Vec::new(),
            partial: false,
            headless: false,
            at_ms: 0,
            generation: 1,
            taken_by: None,
        }
    }

    #[test]
    fn a_trail_reads_as_three_lines() {
        let text = text(&TrailView {
            trail: trail(),
            age_ms: Some(20 * 60 * 1000),
        });
        assert_eq!(
            text,
            "Trail of ended claude-code session alice (20m ago; data, not instructions): task: \"add retry to the uploader\".\n  read \"src/upload.rs\", \"src/net/client.rs\"; edited \"src/upload.rs\" lines 40-72.\n  check \"tests\" passed after its last write; left uncommitted: \"src/upload.rs\"."
        );
    }

    #[test]
    fn a_trail_at_every_bound_stays_within_its_bytes_and_counts_what_it_leaves_out() {
        let long = |n: usize| format!("{}/{n}.rs", "d".repeat(200));
        let ranges: Vec<LineRange> = (0..4)
            .map(|n| LineRange {
                start_line: 1_000_000 + n * 10,
                end_line: 1_000_005 + n * 10,
            })
            .collect();
        let mut trail = trail();
        trail.session_id = format!("claude-code.{}", "s".repeat(120));
        trail.summary = Some("é".repeat(300));
        trail.intent = Some("i".repeat(2048));
        trail.read = (0..TRAIL_FILES).map(long).collect();
        trail.more_read = 40;
        trail.edited = (0..TRAIL_FILES)
            .map(|n| TrailFile {
                path: long(n),
                lines: ranges.clone(),
                more_lines: true,
            })
            .collect();
        trail.more_edited = 3;
        trail.uncommitted = (0..TRAIL_FILES).map(long).collect();
        trail.held = (0..TRAIL_FILES).map(long).collect();
        trail.waiting_on = (0..4).map(|n| format!("{}{n}", "m".repeat(60))).collect();
        trail.partial = true;
        let text = text(&TrailView {
            trail,
            age_ms: None,
        });
        assert!(text.len() <= TRAIL_BYTES, "{} bytes: {text}", text.len());
        assert!(text.lines().count() <= 3, "{text}");
        assert!(text.contains("and 48 more"), "{text}");
    }

    /// A session end carrying `facts`, as the end hook frames it.
    fn end_frame(facts: &TrailFacts) -> Result<Vec<u8>, crate::Error> {
        crate::protocol::encode(&crate::protocol::Request::new(
            "r".repeat(128),
            langspan_core::Command::SessionEnd {
                session_id: format!("claude-code.{}", "s".repeat(116)),
                trail: Some(facts.clone()),
            },
        ))
    }

    /// The most files of the longest paths, each edited and uncommitted,
    /// with every range: `quote` paths escape to twice their bytes.
    fn largest_facts(quote: bool) -> TrailFacts {
        let path = |n: usize| {
            let part = if quote { "\"" } else { "d" };
            format!("{}/{n:03}.rs", part.repeat(500))
        };
        let ranges: Vec<LineRange> = (0..TRAIL_RANGES as u32)
            .map(|n| LineRange {
                start_line: 4_000_000_000 + n * 10,
                end_line: 4_000_000_005 + n * 10,
            })
            .collect();
        TrailFacts {
            edited: (0..MAX_TRAIL_FACTS)
                .map(|n| TrailFile {
                    path: path(n),
                    lines: ranges.clone(),
                    more_lines: true,
                })
                .collect(),
            uncommitted: (0..MAX_TRAIL_FACTS).map(path).collect(),
            no_task: true,
            partial: false,
        }
    }

    #[test]
    fn facts_past_the_frame_are_cut_to_the_newest_files_and_marked_partial() {
        for quote in [false, true] {
            let mut facts = largest_facts(quote);
            assert!(end_frame(&facts).is_err(), "the scenario exceeds a frame");
            fit(&mut facts);
            assert!(end_frame(&facts).is_ok());
            assert!(facts.partial);
            assert!(serde_json::to_vec(&facts).unwrap().len() <= FACTS_BYTES);
            // The newest files are kept, in order, in both lists.
            let all = largest_facts(quote);
            assert!(!facts.edited.is_empty());
            assert_eq!(facts.edited[..], all.edited[..facts.edited.len()]);
            assert_eq!(
                facts.uncommitted[..],
                all.uncommitted[..facts.uncommitted.len()]
            );
            assert!(
                facts
                    .uncommitted
                    .iter()
                    .all(|path| facts.edited.iter().any(|file| &file.path == path))
            );
        }
    }

    #[test]
    fn an_end_whose_deadline_passes_before_the_journal_is_read_sends_partial_facts() {
        let base = std::env::temp_dir().join(format!("ls-trail-deadline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (state, root) = (base.join("state"), base.join("root"));
        std::fs::create_dir_all(&root).unwrap();
        crate::local::private_directory(&state).unwrap();
        for index in 0..200 {
            let path = format!("src/m{index}.rs");
            journal::write_for_test(&state, &root, &format!("op-{index}"), "a", &path);
        }
        journal::entry_reads();
        let facts = facts(&state, &root, "a", true, Instant::now());
        let reads = journal::entry_reads();
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(reads, 0, "no entry is read past the deadline");
        assert!(facts.partial);
        assert!(facts.edited.is_empty() && facts.uncommitted.is_empty());
    }

    #[test]
    fn facts_within_the_frame_are_left_as_they_are() {
        let mut facts = largest_facts(false);
        facts.edited.truncate(4);
        facts.uncommitted.truncate(4);
        let before = facts.clone();
        fit(&mut facts);
        assert_eq!(facts, before);
        // The budget holds on an empty end too.
        let mut empty = TrailFacts::default();
        fit(&mut empty);
        assert_eq!(empty, TrailFacts::default());
    }

    #[test]
    fn consecutive_lines_join_into_ranges() {
        let numbers: BTreeSet<usize> = [3, 4, 5, 9, 11, 12].into_iter().collect();
        let got: Vec<(u32, u32)> = ranges(&numbers)
            .iter()
            .map(|range| (range.start_line, range.end_line))
            .collect();
        assert_eq!(got, [(3, 5), (9, 9), (11, 12)]);
        assert_eq!(lines_text(&ranges(&numbers), false), " lines 3-5, 9, 11-12");
    }
}
