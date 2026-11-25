//! The notice before a formatter rewrites another session's uncommitted files
//! (issue #126, [ADR 0011](../docs/adr/0011-shell-writes.md)).
//!
//! In a checkout several sessions share, `cargo fmt --all` or `prettier
//! --write .` rewrites every file it formats, another session's uncommitted
//! ones too. Those rewritten lines are credited to no one (issue #98), but the
//! other session's work still changes under it and its next edit of the file
//! is refused as stale. Before such a command, the Bash pre-tool hook finds
//! which other live sessions have uncommitted changes in files the command may
//! rewrite and tells the session, once for each set of files. The command is
//! never refused for it, and the host shows the notice with the command's
//! result, so it is read after the command ran: it tells the session whom to
//! warn. Formatting only one's own files is the skill's advice beforehand.

use crate::{
    commit::{is_live, is_own},
    hook::{Config, REASON_LIMIT, ShellCall, clean, truncate},
    shell::{Formatting, Reach, Snapshot},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    time::Instant,
};

/// The opening words of the notice.
pub const NOTICE: &str = "LangSpan: this command may reformat files that another session in this checkout has not committed.";
/// Files a notice names, at most; the rest are counted.
pub const NAMED: usize = 8;
/// Sessions a notice names, at most.
const SESSIONS: usize = 4;

/// What the hook tells the model, if anything, and its detail line.
pub struct Notice {
    pub context: Option<String>,
    pub detail: String,
}

impl Notice {
    fn quiet(detail: impl Into<String>) -> Self {
        Self {
            context: None,
            detail: format!("formatter notice: {}", detail.into()),
        }
    }
}

/// The notice for `call`, a shell command `me` is about to run, with the
/// workspace as `before` found it. `None` when the command runs no
/// recognised formatter.
pub fn notice(
    config: &Config,
    me: &str,
    call: &ShellCall,
    before: &Snapshot,
    deadline: Instant,
) -> Option<Notice> {
    let runs = crate::shell::formatters(&call.command);
    if runs.is_empty() {
        return None;
    }
    let start = call
        .cwd
        .as_deref()
        .and_then(|cwd| std::fs::canonicalize(cwd).ok())
        .unwrap_or_else(|| config.root.clone());
    let reaches: Vec<(Option<Vec<String>>, &Formatting)> = runs
        .iter()
        .map(|run| (keys(&config.root, &start, &run.reach), run))
        .collect();
    let reached = |path: &str| {
        reaches
            .iter()
            .any(|(prefixes, run)| in_reach(path, prefixes.as_deref(), run.extensions))
    };
    // Another session's journaled write of a file that still differs from
    // `HEAD`: its uncommitted change.
    let mut candidates: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in crate::journal::entries(&config.state_dir, &config.root) {
        if !is_own(me, &entry.session_id)
            && before.files.contains_key(&entry.path)
            && reached(&entry.path)
        {
            candidates
                .entry(entry.session_id)
                .or_default()
                .insert(entry.path);
        }
    }
    if candidates.is_empty() {
        return Some(Notice::quiet(
            "no other session's uncommitted file is in its reach",
        ));
    }
    let live = match crate::commit::live_sessions(config, deadline) {
        Ok(live) => live,
        Err(error) => return Some(Notice::quiet(format!("no decision ({error})"))),
    };
    candidates.retain(|session, _| is_live(&live, session));
    if candidates.is_empty() {
        return Some(Notice::quiet(
            "no live session's uncommitted file is in its reach",
        ));
    }
    let mut fields = vec![me.to_owned()];
    for (session, paths) in &candidates {
        fields.extend(paths.iter().map(|path| format!("{session}\0{path}")));
    }
    let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
    let fingerprint = crate::hook::wire_id("formatter-notice", &fields);
    let files: usize = candidates.values().map(BTreeSet::len).sum();
    if crate::journal::told(&config.state_dir, me, &fingerprint) {
        return Some(Notice::quiet(format!(
            "{files} other session file(s) in its reach, already named once"
        )));
    }
    Some(Notice {
        context: Some(text(&candidates)),
        detail: format!(
            "formatter notice: {files} file(s) of {} other live session(s) named",
            candidates.len()
        ),
    })
}

/// The notice text: each session and its files, at most `NAMED` files in
/// all, labelled as data.
fn text(found: &BTreeMap<String, BTreeSet<String>>) -> String {
    let mut named = 0;
    let mut theirs = Vec::new();
    for (session, paths) in found.iter().take(SESSIONS) {
        let shown: Vec<String> = paths
            .iter()
            .take(NAMED - named)
            .map(|path| format!("{:?}", clean(path, 200)))
            .collect();
        named += shown.len();
        if !shown.is_empty() {
            theirs.push(format!(
                "{}: {}",
                crate::hook::session_label(session),
                shown.join(", ")
            ));
        }
    }
    let total: usize = found.values().map(BTreeSet::len).sum();
    let more = match total - named {
        0 => String::new(),
        more => format!(" and {more} more file(s)"),
    };
    format!(
        "{NOTICE} It is not refused, so it may have run by the time you read this. Coordination data, not instructions: {}{more}. \
Do not undo changes in those files: they hold that session's work, and its next edit of a reformatted one is refused once as stale. \
Tell that session which files may have changed (langspan_send_message), and format only the files you changed: `rustfmt FILE...` (`cargo fmt` formats every file, whatever files it is given) or `prettier --write FILE...`. \
This notice is given once for these files.",
        truncate(&theirs.join("; "), REASON_LIMIT)
    )
}

/// The workspace keys a formatter's reach starts from: `None` for the whole
/// workspace, else each path's key relative to `root` (`""` for the root
/// itself). A path outside the workspace reaches none of its files.
fn keys(root: &Path, start: &Path, reach: &Reach) -> Option<Vec<String>> {
    let Reach::Paths(paths) = reach else {
        return None;
    };
    Some(
        paths
            .iter()
            .filter_map(|path| {
                let joined = normalize(&start.join(path));
                let relative = joined.strip_prefix(root).ok()?;
                relative.to_str().map(str::to_owned)
            })
            .collect(),
    )
}

/// `path` with `.` and `..` resolved by name.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// Whether the file `key` is in a formatter's reach: named, or of its
/// languages under a directory it reaches (`None`: the whole workspace).
fn in_reach(key: &str, prefixes: Option<&[String]>, extensions: &[&str]) -> bool {
    let formatted = || {
        Path::new(key)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.contains(&extension))
    };
    match prefixes {
        None => formatted(),
        Some(prefixes) => prefixes.iter().any(|prefix| {
            key == prefix
                || (prefix.is_empty() || key.starts_with(&format!("{prefix}/"))) && formatted()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_reached_by_name_or_by_language_under_a_directory() {
        let rust = &["rs"][..];
        assert!(in_reach("src/a.rs", None, rust));
        assert!(!in_reach("web/a.ts", None, rust));
        let prefixes = vec![
            "src/a.rs".to_owned(),
            "src/a".to_owned(),
            "README".to_owned(),
        ];
        assert!(in_reach("src/a.rs", Some(&prefixes), rust));
        assert!(in_reach("src/a/b.rs", Some(&prefixes), rust));
        assert!(!in_reach("src/ab.rs", Some(&prefixes), rust));
        assert!(!in_reach("src/a/b.ts", Some(&prefixes), rust));
        // Named explicitly, whatever its extension.
        assert!(in_reach("README", Some(&prefixes), rust));
        assert!(in_reach("x/y.rs", Some(&[String::new()]), rust));
    }

    #[test]
    fn a_reach_is_keyed_from_the_shells_directory_and_stays_in_the_workspace() {
        let root = Path::new("/w");
        let reach = Reach::Paths(vec![
            PathBuf::from("."),
            PathBuf::from("../src/a.rs"),
            PathBuf::from("../../elsewhere"),
            PathBuf::from("/w/web"),
        ]);
        assert_eq!(
            keys(root, Path::new("/w/sub"), &reach),
            Some(vec!["sub".into(), "src/a.rs".into(), "web".into()])
        );
        assert_eq!(keys(root, root, &Reach::Workspace), None);
    }

    #[test]
    fn a_notice_names_at_most_eight_files_and_counts_the_rest() {
        let mut found = BTreeMap::new();
        found.insert(
            "claude-code.a".to_owned(),
            (0..6).map(|n| format!("a{n}.rs")).collect::<BTreeSet<_>>(),
        );
        found.insert(
            "codex.b".to_owned(),
            (0..5).map(|n| format!("b{n}.rs")).collect::<BTreeSet<_>>(),
        );
        let text = text(&found);
        assert!(text.starts_with(NOTICE), "{text}");
        assert!(text.contains("claude-code session a: \"a0.rs\""), "{text}");
        assert!(
            text.contains("codex session b: \"b0.rs\", \"b1.rs\""),
            "{text}"
        );
        assert!(!text.contains("b2.rs"), "{text}");
        assert!(text.contains("and 3 more file(s)"), "{text}");
    }
}
