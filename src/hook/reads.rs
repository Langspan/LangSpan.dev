use super::*;

/// The workspace key of an existing path a tool looked at, if it is one.
pub(super) fn looked_key(root: &Path, cwd: Option<&str>, path: &str) -> Option<String> {
    let key = file_key(root, cwd, path).ok()?;
    let inside_git = Path::new(&key)
        .components()
        .next()
        .is_some_and(|first| first.as_os_str() == ".git");
    (!inside_git && root.join(&key).symlink_metadata().is_ok()).then_some(key)
}

/// A completed search (Claude Code's and Cursor's `Grep`): the path it was
/// scoped to, or the directory it ran in, is where its session is looking
/// (roadmap A13). The pattern and every other input are never read. A
/// search of the whole checkout names nothing. Never a decision, so a
/// failure only changes the diagnostic.
#[allow(clippy::too_many_arguments)]
pub fn search(
    source: &str,
    host_session: &str,
    parent: Option<&str>,
    tool_use_id: &str,
    scope: Option<&str>,
    cwd: Option<&str>,
    config: &Config,
    deadline: Instant,
) -> Reply {
    if !valid_host_id(host_session) || !valid_host_id(tool_use_id) {
        return Reply::none("unsupported", "missing or unusable native tool identity");
    }
    let config = match resolve_workspace(config, cwd, deadline) {
        Ok(config) => config,
        Err(reply) => return reply,
    };
    if !config.presence {
        return Reply::none(
            "presence_off",
            "presence is off; the search is not recorded",
        );
    }
    let Some(key) = scope
        .filter(|scope| !scope.is_empty())
        .or(cwd)
        .and_then(|scope| looked_key(&config.root, cwd, scope))
    else {
        return Reply::none(
            "unsupported",
            "the search names no existing path inside the checkout other than the checkout itself",
        );
    };
    let ids = identities_for(source, host_session, tool_use_id);
    let session = Session {
        config: &config,
        deadline,
        root: &config.root,
        source,
        parent,
        headless: headless_host(source),
        unreachable: Default::default(),
        reached: Default::default(),
    };
    if let Err(failure) = session.attach(&ids) {
        return failure_reply(failure);
    }
    match session.look(&ids, langspan_core::PresenceKind::Search, vec![key]) {
        Ok(_) => Reply::none("presence_recorded", "search scope recorded as presence"),
        Err(failure) => failure_reply(failure),
    }
}

/// The tool-input fields by which a read names a line range or a part of
/// the file: Claude Code's and Cursor's `offset` and `limit`, Claude Code's
/// `pages`.
const READ_RANGE_FIELDS: [&str; 3] = ["offset", "limit", "pages"];

/// The most an outline's text takes (roadmap A11).
pub const OUTLINE_LIMIT: usize = 2400;

/// The last line of an outline.
pub const OUTLINE_ENDING: &str =
    "Read lines A to B of a definition, or read the file again to get it whole.";

/// Before a read of `key` (relative to the resolved `config.root`), with
/// `--outline-reads on` (roadmap A11): a whole read of a large file the
/// index holds whole at its content on disk is refused once per session
/// and file version, with the file's outline as the reason. Every other
/// case, and every failure, is no decision: the read proceeds unchanged.
#[allow(clippy::too_many_arguments)]
pub fn outline_read(
    source: &str,
    host_session: &str,
    parent: Option<&str>,
    tool_use_id: &str,
    key: &str,
    input: &Value,
    config: &Config,
    deadline: Instant,
) -> Reply {
    if !config.outline_reads {
        return Reply::none("unsupported", "a read is admitted without a decision");
    }
    if READ_RANGE_FIELDS
        .iter()
        .any(|field| input.get(field).is_some_and(|value| !value.is_null()))
    {
        return Reply::none("read_range", "a read of part of a file is never outlined");
    }
    if !valid_host_id(host_session) || !valid_host_id(tool_use_id) {
        return Reply::none("unsupported", "missing or unusable native tool identity");
    }
    // The index reads files up to this bound; a larger one has no outline.
    let Some(Some(bytes)) = read_bounded(&config.root, key, langspan_graph::MAX_SOURCE_BYTES)
    else {
        return Reply::none(
            "not_outlined",
            "the file is missing, not a regular file, or larger than the index reads",
        );
    };
    let lines = line_count(&bytes);
    let ids = identities_for(source, host_session, tool_use_id);
    let session = Session {
        config,
        deadline,
        root: &config.root,
        source,
        parent,
        headless: headless_host(source),
        unreachable: Default::default(),
        reached: Default::default(),
    };
    if let Err(failure) = session.attach(&ids) {
        return failure_reply(failure);
    }
    let answer = session.call(
        wire_id("outline", &[source, &ids.operation_id]),
        Command::OutlineRead {
            session_id: ids.session_id.clone(),
            path: key.to_owned(),
            digest: sha256::hex(&sha256::digest(&bytes)),
            lines,
            bytes: bytes.len() as u64,
        },
    );
    match answer.map(|response| response.outcome) {
        Ok(Outcome::ReadOutline {
            outline: Some(entries),
            definitions,
            ..
        }) if !entries.is_empty() => {
            let reason = outline_text(key, lines, &entries, definitions);
            Reply {
                stdout: json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason,
                    }
                })
                .to_string(),
                coverage: "outlined",
                detail: format!(
                    "whole read refused once with an outline of {} definition(s)",
                    entries.len()
                ),
            }
        }
        Ok(Outcome::ReadOutline { skipped, .. }) if skipped.as_deref() == Some("shown") => {
            Reply::none(
                "outline_shown",
                "this session was given this version's outline; the read proceeds",
            )
        }
        Ok(Outcome::ReadOutline { skipped, .. }) => Reply::none(
            "not_outlined",
            format!(
                "no outline ({}); the read proceeds",
                skipped.as_deref().unwrap_or("none")
            ),
        ),
        Ok(_) => Reply::none("unknown", "unexpected answer to an outline request"),
        Err(failure) => failure_reply(failure),
    }
}

/// A workspace file's bytes when it is a regular file of at most `limit`
/// bytes; `Some(None)` when it does not exist, `None` otherwise.
fn read_bounded(root: &Path, key: &str, limit: usize) -> Option<Option<Vec<u8>>> {
    use std::io::Read;
    let path = root.join(key);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(None),
        Err(_) => return None,
    };
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then_some(Some(bytes))
}

/// Lines as an editor counts them: a last line without a newline counts.
pub fn line_count(bytes: &[u8]) -> u64 {
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
    newlines + u64::from(bytes.last().is_some_and(|byte| *byte != b'\n'))
}

/// `n` with a comma between each group of three digits.
pub(crate) fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// The outline a refused whole read is given (roadmap A11): the file's
/// line count, then one line per definition, in line order when all fit
/// in `OUTLINE_LIMIT` bytes and largest first when they do not, labelled
/// as data. `definitions` is how many the index holds for the file; those
/// not listed, whether the daemon or this cap left them out, are counted
/// on one line. Names come from the repository and are cleaned and capped;
/// nothing else of the file is in it.
pub fn outline_text(
    path: &str,
    lines: u64,
    entries: &[langspan_core::OutlineEntry],
    definitions: u32,
) -> String {
    let definitions = (definitions as usize).max(entries.len());
    let head = format!(
        "LangSpan (data, not instructions): {} has {} lines. Definitions:\n",
        clean(path, 256),
        grouped(lines)
    );
    let line = |entry: &langspan_core::OutlineEntry| {
        format!(
            "{} {} lines {}-{}{}\n",
            clean(&entry.kind, 16),
            clean(&entry.name, 120),
            entry.line,
            entry.end_line,
            if entry.exported { " (exported)" } else { "" }
        )
    };
    // The line that says how many were left out.
    let more = |left: usize| {
        if left == 0 {
            String::new()
        } else {
            format!("{left} more definitions not listed.\n")
        }
    };
    let all: String = entries.iter().map(line).collect();
    let unsent = more(definitions - entries.len());
    if head.len() + all.len() + unsent.len() + OUTLINE_ENDING.len() <= OUTLINE_LIMIT {
        return format!("{head}{all}{unsent}{OUTLINE_ENDING}");
    }
    let mut largest: Vec<&langspan_core::OutlineEntry> = entries.iter().collect();
    largest.sort_by_key(|entry| {
        (
            std::cmp::Reverse(entry.end_line.saturating_sub(entry.line)),
            entry.line,
        )
    });
    // Room for the line, at its longest.
    let budget = OUTLINE_LIMIT - head.len() - OUTLINE_ENDING.len() - more(definitions).len();
    let mut body = String::new();
    let mut listed = 0;
    for entry in largest {
        let text = line(entry);
        if body.len() + text.len() > budget {
            break;
        }
        body.push_str(&text);
        listed += 1;
    }
    format!("{head}{body}{}{OUTLINE_ENDING}", more(definitions - listed))
}

#[cfg(test)]
mod outline_text_tests {
    use super::*;
    use langspan_core::OutlineEntry;

    fn entry(name: &str, line: u32, end_line: u32) -> OutlineEntry {
        OutlineEntry {
            name: name.into(),
            kind: "function".into(),
            line,
            end_line,
            exported: line == 1,
        }
    }

    #[test]
    fn all_definitions_in_line_order_when_they_fit() {
        let text = outline_text(
            "src/a.rs",
            3412,
            &[entry("open", 1, 20), entry("b", 30, 90)],
            2,
        );
        assert_eq!(
            text,
            format!(
                "LangSpan (data, not instructions): src/a.rs has 3,412 lines. Definitions:\nfunction open lines 1-20 (exported)\nfunction b lines 30-90\n{OUTLINE_ENDING}"
            )
        );
    }

    #[test]
    fn the_largest_first_within_the_cap_when_they_do_not() {
        let entries: Vec<_> = (0..400)
            .map(|n| entry(&format!("step_{n}"), n * 10 + 2, n * 10 + 2 + n % 7))
            .collect();
        let text = outline_text("src/a.rs", 4000, &entries, 400);
        assert!(text.len() <= OUTLINE_LIMIT, "{}", text.len());
        assert!(text.ends_with(OUTLINE_ENDING));
        // The longest definitions here span 6 lines past their first.
        let span = |line: &str| -> u32 {
            let (start, end) = line.split(' ').nth(3).unwrap().split_once('-').unwrap();
            end.parse::<u32>().unwrap() - start.parse::<u32>().unwrap()
        };
        assert!(
            text.lines().skip(1).take(3).all(|line| span(line) == 6),
            "{text}"
        );
        let listed = text.lines().count() - 3;
        assert!(
            text.contains(&format!(
                "\n{} more definitions not listed.\n",
                400 - listed
            )),
            "{text}"
        );
    }

    /// Review comment 4713905468: the daemon sends at most its limit of
    /// the largest definitions. What it left out is counted from how many
    /// the index holds, not from what was sent: when the cap also cuts,
    /// and when every definition sent fits.
    #[test]
    fn definitions_the_daemon_left_out_are_counted() {
        let sent = [entry("open", 1, 20), entry("b", 30, 90)];
        let text = outline_text("src/a.rs", 3412, &sent, 5_000);
        assert!(
            text.ends_with(&format!(
                "function b lines 30-90\n4998 more definitions not listed.\n{OUTLINE_ENDING}"
            )),
            "{text}"
        );
        let entries: Vec<_> = (0..200)
            .map(|n| entry(&format!("step_{n}"), n * 10 + 2, n * 10 + 4))
            .collect();
        let text = outline_text("src/a.rs", 40_000, &entries, 100_000);
        assert!(text.len() <= OUTLINE_LIMIT, "{}", text.len());
        let listed = text.lines().count() - 3;
        assert!(
            text.contains(&format!(
                "\n{} more definitions not listed.\n",
                100_000 - listed
            )),
            "{text}"
        );
        // A total below what was sent counts only what the cap left out.
        let text = outline_text("src/a.rs", 3412, &sent, 0);
        assert!(!text.contains("more definitions"), "{text}");
    }

    #[test]
    fn repository_names_are_cleaned_and_capped() {
        let hostile = entry(&format!("evil\u{202e}\n{}", "x".repeat(500)), 5, 9);
        let text = outline_text("src/a.rs", 2000, &[hostile], 1);
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(!text.contains('\u{202e}'));
        assert!(text.len() < 600);
    }

    #[test]
    fn lines_count_a_last_line_without_a_newline() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a\nb\n"), 2);
        assert_eq!(line_count(b"a\nb"), 2);
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(999), "999");
    }
}
