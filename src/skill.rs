//! The LangSpan skill: one text, `skill.md`, that teaches an agent how to
//! work alongside LangSpan, written in each host's own format
//! ([ADR 0034](../docs/adr/0034-langspan-skill.md)).
//!
//! Claude Code reads it as a user skill (`skills/langspan/SKILL.md`) and
//! Cursor as a project rule (`.cursor/rules/langspan.mdc`): each a whole file
//! that LangSpan writes and marks. Codex reads it from its user `AGENTS.md`
//! and Gemini CLI from its user `GEMINI.md`: there it is one section between
//! marker lines, and every other line is the owner's, kept byte for byte.
//! The same body is in every form, so no host's copy can drift from it.

use crate::install::{self, Vendor};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The text every host's form carries unchanged.
pub const SOURCE: &str = include_str!("skill.md");
/// The skill's name, which is also its folder name for Claude Code.
pub const NAME: &str = "langspan";
/// When a host should use it. No double quote or backslash: it is written
/// as a YAML double-quoted string.
pub const DESCRIPTION: &str = "How to work alongside the other agent sessions that LangSpan coordinates in this repository: what its briefings, refusals and alerts mean and how to respond, when to use langspan commit --mine and langspan land, and what never to do.";
const MARK: &str = "<!-- langspan-managed:skill:v1 -->";
const BEGIN: &str = "<!-- langspan-managed:skill:v1 begin -->";
const END: &str = "<!-- langspan-managed:skill:v1 end -->";

/// The opening words of every message the skill quotes, from the code that
/// writes them. A test requires the skill to quote exactly these.
pub const MESSAGES: [&str; 8] = [
    crate::hook::BRIEFING,
    crate::hook::STALE_SOURCE,
    crate::hook::UNREAD_LINES,
    crate::hook::CONFLICT,
    crate::breaks::ALERT,
    crate::hook::CHECK_RESULT,
    crate::commit::HELD,
    crate::formatter::NOTICE,
];

/// The most a host's form may hold. Claude Code's skill documentation and
/// Cursor's rules documentation each advise under 500 lines; Codex reads at
/// most 32 KiB of its combined AGENTS files (`project_doc_max_bytes`,
/// [`CODEX_CHAIN_MAX_BYTES`]), which the owner's text shares; Gemini CLI
/// documents no limit. LangSpan keeps every form within all of them, and
/// within 8 KiB of its own.
pub const MAX_LINES: usize = 500;
pub const MAX_BYTES: usize = 8 * 1024;

/// The hosts `langspan install --skill` writes the skill for, at user level.
/// Cursor's form is a project rule: `hooks install cursor --skill` writes it.
/// Antigravity reads it as a rule at either level.
pub const USER_HOSTS: [Vendor; 4] = [
    Vendor::ClaudeCode,
    Vendor::Codex,
    Vendor::Gemini,
    Vendor::Antigravity,
];

/// The hosts `hooks install HOST --skill` writes the skill for, in the
/// project.
pub const PROJECT_HOSTS: [Vendor; 2] = [Vendor::Cursor, Vendor::Antigravity];

/// Where `vendor` reads the skill. `dir` is the host's user configuration
/// directory (`~/.claude`, `~/.codex`, `~/.gemini`, `~/.gemini/config`, or
/// where the host's own variable moves it), or for Cursor the project root.
/// For a project's Antigravity rule, see [`project_path`].
pub fn path(vendor: Vendor, dir: &Path) -> PathBuf {
    match vendor {
        Vendor::ClaudeCode => dir.join("skills").join(NAME).join("SKILL.md"),
        Vendor::Codex => dir.join("AGENTS.md"),
        Vendor::Gemini => dir.join("GEMINI.md"),
        Vendor::Cursor => dir
            .join(".cursor")
            .join("rules")
            .join(format!("{NAME}.mdc")),
        // `~/.gemini/config/rules/*.md` and `.agents/rules/*.md`
        // (<https://antigravity.google/docs/rules/>).
        Vendor::Antigravity => dir.join("rules").join(format!("{NAME}.md")),
    }
}

/// Where `vendor` reads the skill as a rule of the project at `root`.
pub fn project_path(vendor: Vendor, root: &Path) -> PathBuf {
    match vendor {
        Vendor::Antigravity => path(vendor, &root.join(".agents")),
        _ => path(vendor, root),
    }
}

/// Whether LangSpan owns the whole file, rather than a section of it.
fn whole(vendor: Vendor) -> bool {
    matches!(
        vendor,
        Vendor::ClaudeCode | Vendor::Cursor | Vendor::Antigravity
    )
}

fn note(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::ClaudeCode => {
            "<!-- Written by `langspan install --skill`; `langspan uninstall` removes this file. -->"
        }
        Vendor::Cursor => {
            "<!-- Written by `langspan hooks install cursor --skill`; `langspan hooks remove cursor` removes this file. -->"
        }
        Vendor::Codex | Vendor::Gemini => {
            "<!-- Written by `langspan install --skill`; `langspan uninstall` removes this section. -->"
        }
        Vendor::Antigravity => {
            "<!-- Written by `langspan install --skill` or `langspan hooks install antigravity --skill`; `langspan uninstall` or `langspan hooks remove antigravity` removes this file. -->"
        }
    }
}

/// The skill in `vendor`'s format: the whole file for Claude Code and
/// Cursor, the marked section for Codex and Gemini CLI.
pub fn render(vendor: Vendor) -> String {
    let note = note(vendor);
    match vendor {
        Vendor::ClaudeCode => format!(
            "---\nname: {NAME}\ndescription: \"{DESCRIPTION}\"\n---\n{MARK}\n{note}\n\n{SOURCE}"
        ),
        Vendor::Cursor => format!(
            "---\ndescription: \"{DESCRIPTION}\"\nalwaysApply: true\n---\n{MARK}\n{note}\n\n{SOURCE}"
        ),
        Vendor::Codex | Vendor::Gemini => format!("{BEGIN}\n{note}\n\n{SOURCE}{END}\n"),
        // A rule file without valid frontmatter and a known `trigger` is
        // discarded silently; `always_on` puts it in every turn.
        Vendor::Antigravity => format!(
            "---\ntrigger: always_on\ndescription: \"{DESCRIPTION}\"\n---\n{MARK}\n{note}\n\n{SOURCE}"
        ),
    }
}

/// Whether the skill is in a file, as `doctor` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Absent,
    /// Exactly what this version writes.
    Current,
    /// LangSpan's, from another version or edited: install replaces it.
    Outdated,
    /// A symlink or other non-regular file, which LangSpan never writes
    /// through: it leaves it alone.
    Unmanaged,
}

/// The state of the skill in a file holding `existing` (`None`: absent).
/// A whole file without LangSpan's mark, or a section file whose markers
/// are broken, is an error: it is not LangSpan's to change.
pub fn state(vendor: Vendor, existing: Option<&str>) -> Result<State, String> {
    let Some(text) = existing else {
        return Ok(State::Absent);
    };
    let ours = if whole(vendor) {
        if !text.lines().any(|line| line == MARK) {
            return Err(foreign(vendor));
        }
        text
    } else {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let Some((begin, end)) = section(&lines)? else {
            return Ok(State::Absent);
        };
        return Ok(if lines[begin..=end].concat() == render(vendor) {
            State::Current
        } else {
            State::Outdated
        });
    };
    Ok(if ours == render(vendor) {
        State::Current
    } else {
        State::Outdated
    })
}

fn foreign(vendor: Vendor) -> String {
    format!(
        "this file exists and was not written by LangSpan, so {} would not be LangSpan's skill; nothing was changed. Rename or remove it, then run this again",
        vendor.name()
    )
}

/// LangSpan's section, by line index (end inclusive), if there is one.
fn section(lines: &[&str]) -> Result<Option<(usize, usize)>, String> {
    let mut begin = None;
    let mut found = None;
    for (index, line) in lines.iter().enumerate() {
        match line.trim_end_matches(['\n', '\r']).trim() {
            BEGIN if begin.is_some() || found.is_some() => {
                return Err("the file has more than one LangSpan skill section; nothing was changed. Remove the extra one by hand".into());
            }
            BEGIN => begin = Some(index),
            END => match begin.take() {
                Some(start) => found = Some((start, index)),
                None => {
                    return Err("the file has a LangSpan skill end marker without its start; nothing was changed".into());
                }
            },
            _ => {}
        }
    }
    if begin.is_some() {
        return Err(
            "the file has a LangSpan skill start marker without its end; nothing was changed"
                .into(),
        );
    }
    Ok(found)
}

/// The file's content with the skill written (`install`) or removed, from
/// what it holds now (`existing`, `None`: absent). `None` means the file
/// should not exist. Outside LangSpan's file or section nothing changes.
pub fn plan(
    vendor: Vendor,
    existing: Option<&str>,
    install: bool,
) -> Result<Option<String>, String> {
    let current = state(vendor, existing);
    if whole(vendor) {
        return match (current, install) {
            (Err(error), true) => Err(error),
            (Err(_), false) => Ok(existing.map(str::to_owned)),
            (Ok(_), true) => Ok(Some(render(vendor))),
            (Ok(_), false) => Ok(None),
        };
    }
    current?;
    let text = existing.unwrap_or("");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let planned = match (section(&lines)?, install) {
        (Some((begin, end)), true) => format!(
            "{}{}{}",
            lines[..begin].concat(),
            render(vendor),
            lines[end + 1..].concat()
        ),
        (Some((begin, end)), false) => {
            let mut before = lines[..begin].concat();
            let after = lines[end + 1..].concat();
            // Undo the blank separator install adds before a section at the end.
            if after.is_empty() && before.ends_with("\n\n") {
                before.pop();
            }
            before + &after
        }
        (None, true) => {
            let mut out = text.to_owned();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out + &render(vendor)
        }
        (None, false) => return Ok(existing.map(str::to_owned)),
    };
    // A file that held only LangSpan's section goes with it.
    Ok((!planned.is_empty()).then_some(planned))
}

/// One file's planned change, made from the bytes it held when planned.
pub struct FilePlan {
    pub vendor: Vendor,
    pub path: PathBuf,
    pub install: bool,
    pub original: Option<Vec<u8>>,
    pub planned: Option<String>,
}

impl FilePlan {
    pub fn unchanged(&self) -> bool {
        self.original.as_deref() == self.planned.as_deref().map(str::as_bytes)
    }

    /// What a dry run prints for this change.
    pub fn shown(&self) -> String {
        match &self.planned {
            Some(text) => format!(
                "{} would become ({} bytes):\n{}",
                self.path.display(),
                text.len(),
                text.trim_end()
            ),
            None => format!("{} would be removed.", self.path.display()),
        }
    }

    /// Write or remove the file, unless it changed since it was planned.
    /// Returns false when it already matched.
    pub fn apply(&self) -> Result<bool, crate::Error> {
        let path = &self.path;
        if self.planned.is_some() {
            let replan = |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
                let existing = install::parse_text(path, now)?;
                match plan(self.vendor, existing.as_deref(), self.install)
                    .map_err(|e| format!("{}: {e}", path.display()))?
                {
                    Some(text) => Ok(text.into_bytes()),
                    None => Err(format!(
                        "{} changed while LangSpan was writing it; nothing was written. Run this again",
                        path.display()
                    )
                    .into()),
                }
            };
            return install::write_planned(path, self.original.as_deref(), &replan);
        }
        if self.original.is_none() {
            return Ok(false);
        }
        if install::read_bytes(path)? != self.original {
            return Err(format!(
                "{} changed while LangSpan was removing it; nothing was removed. Run this again",
                path.display()
            )
            .into());
        }
        fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        if self.vendor == Vendor::ClaudeCode
            && let Some(folder) = path.parent()
        {
            // The skill's own folder goes too, but only when nothing else is
            // in it: `remove_dir` refuses a folder that is not empty, and
            // then it is left as it is.
            let _ = fs::remove_dir(folder);
        }
        Ok(true)
    }
}

/// Whether `path` is a symlink or another non-regular file. LangSpan never
/// writes its skill through one, so such a file cannot hold LangSpan's
/// file or section: uninstall and `doctor` leave it alone.
fn unmanaged(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file())
}

/// Plan the skill's file for `vendor` at `path`. Every refusal is raised
/// here, before anything is written. Installing through a symlink is
/// refused; removing leaves a symlink untouched.
pub fn plan_file(vendor: Vendor, path: &Path, install: bool) -> Result<FilePlan, crate::Error> {
    if !install && unmanaged(path) {
        return Ok(FilePlan {
            vendor,
            path: path.to_owned(),
            install,
            original: None,
            planned: None,
        });
    }
    let original = install::read_bytes(path)?;
    let existing = install::parse_text(path, original.as_deref())?;
    let planned = plan(vendor, existing.as_deref(), install)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(FilePlan {
        vendor,
        path: path.to_owned(),
        install,
        original,
        planned,
    })
}

/// The state of the skill in the file at `path`, for `doctor`.
pub fn file_state(vendor: Vendor, path: &Path) -> Result<State, String> {
    if unmanaged(path) {
        return Ok(State::Unmanaged);
    }
    let text = install::read_text(path).map_err(|e| e.to_string())?;
    state(vendor, text.as_deref()).map_err(|e| format!("{}: {e}", path.display()))
}

/// The most of its instruction chain Codex reads by default
/// (`project_doc_max_bytes`). The limit is for the combined chain, every
/// AGENTS file Codex reads together, so LangSpan's section shares it with
/// the owner's own text.
pub const CODEX_CHAIN_MAX_BYTES: usize = 32 * 1024;

/// How many bytes of `text` come before the end of LangSpan's section, if
/// it holds one.
pub fn bytes_through_section(text: &str) -> Option<usize> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (_, end) = section(&lines).ok()??;
    Some(lines[..=end].iter().map(|line| line.len()).sum())
}

/// Why Codex would not read all of LangSpan's section in its user
/// `AGENTS.md` under `dir`, if it would not: `AGENTS.override.md` is read
/// instead, or the text up to the section's end is past the chain's
/// default limit on its own. Project files Codex adds to the chain share
/// the same limit; this does not measure them.
pub fn codex_unread(dir: &Path) -> Option<String> {
    let path = path(Vendor::Codex, dir);
    let text = install::read_text(&path).ok()??;
    let through = bytes_through_section(&text)?;
    let instead = dir.join("AGENTS.override.md");
    if fs::symlink_metadata(&instead).is_ok() {
        return Some(format!(
            "Codex reads {} instead of {}, so it does not read the LangSpan skill there; move the skill's section into the override file by hand, or remove the override",
            instead.display(),
            path.display()
        ));
    }
    (through > CODEX_CHAIN_MAX_BYTES).then(|| {
        format!(
            "{} holds {through} bytes up to the end of the LangSpan skill, past the 32 KiB Codex reads of its combined AGENTS files by default (`project_doc_max_bytes`), so the skill is cut off; shorten the text before it, or raise `project_doc_max_bytes`",
            path.display()
        )
    })
}

/// Every span between backticks in the skill.
fn code_spans() -> impl Iterator<Item = &'static str> {
    SOURCE.split('`').skip(1).step_by(2)
}

/// The `langspan` command lines the skill names.
pub fn commands() -> Vec<&'static str> {
    code_spans()
        .filter(|span| span.starts_with("langspan "))
        .collect()
}

/// The MCP tools the skill names.
pub fn tools() -> Vec<&'static str> {
    code_spans()
        .filter(|span| span.starts_with("langspan_"))
        .collect()
}

/// The messages the skill quotes: each `"LangSpan…"` in double quotes.
pub fn quotes() -> Vec<String> {
    let flat = SOURCE.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.split('"')
        .skip(1)
        .step_by(2)
        .filter(|quote| quote.starts_with("LangSpan"))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "# My notes\n\nUse tabs.\n";

    #[test]
    fn the_bytes_codex_must_read_end_with_langspans_section() {
        assert_eq!(bytes_through_section(OWNER), None);
        let section = render(Vendor::Codex);
        let after = "# Later\n";
        let text = format!("{OWNER}\n{section}{after}");
        assert_eq!(bytes_through_section(&text), Some(text.len() - after.len()));
    }

    #[test]
    fn every_form_carries_the_one_source_text_unchanged() {
        for vendor in Vendor::ALL {
            let text = render(vendor);
            assert_eq!(text.matches(SOURCE).count(), 1, "{}", vendor.name());
            // Nothing but the host's own wrapper is added around it.
            let wrapper = text.replace(SOURCE, "");
            assert!(wrapper.lines().count() <= 8, "{}: {wrapper}", vendor.name());
        }
        assert!(!DESCRIPTION.contains(['"', '\\']));
        assert!(render(Vendor::ClaudeCode).starts_with("---\nname: langspan\ndescription: \""));
        assert!(render(Vendor::Cursor).contains("\nalwaysApply: true\n---\n"));
    }

    #[test]
    fn every_form_stays_within_each_hosts_documented_limit() {
        for vendor in Vendor::ALL {
            let text = render(vendor);
            assert!(text.lines().count() <= MAX_LINES, "{}", vendor.name());
            assert!(
                text.len() <= MAX_BYTES,
                "{}: {} bytes",
                vendor.name(),
                text.len()
            );
        }
        // Claude Code truncates a skill's description in its listing at
        // 1,536 characters.
        assert!(DESCRIPTION.len() <= 1536);
    }

    #[test]
    fn the_skill_quotes_exactly_the_messages_the_code_gives() {
        let mut quoted = quotes();
        quoted.sort();
        let mut expected: Vec<String> = MESSAGES.iter().map(|m| (*m).to_owned()).collect();
        expected.sort();
        assert_eq!(quoted, expected);
    }

    #[test]
    fn the_refusals_the_skill_quotes_begin_with_its_words() {
        use crate::hook::{deny, deny_stale, deny_unread};
        for (reply, words) in [
            (deny_stale(&[], "", |_| None), crate::hook::STALE_SOURCE),
            (deny_unread(&[]), crate::hook::UNREAD_LINES),
            (deny(&[], &[]), crate::hook::CONFLICT),
        ] {
            let stdout: serde_json::Value = serde_json::from_str(&reply.stdout).unwrap();
            let reason = stdout["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap();
            assert!(reason.starts_with(words), "{reason}");
        }
    }

    /// Every formatter command the skill names is read by the formatter
    /// notice as the skill describes it: the ones it advises reach only the
    /// files given, the ones it warns against reach the whole tree, and a
    /// check rewrites nothing (issue #126).
    #[test]
    fn the_formatter_commands_the_skill_names_are_read_as_it_says() {
        use crate::shell::{Reach, formatters};
        use std::path::PathBuf;
        let named: Vec<&str> = code_spans()
            .filter(|span| {
                ["cargo fmt", "rustfmt", "prettier"]
                    .iter()
                    .any(|tool| span.starts_with(tool))
            })
            .collect();
        // (command, what it reaches with FILE... given as `web/a.ts`):
        // `None` rewrites nothing.
        let expected: [(&str, Option<Reach>); 5] = [
            (
                "rustfmt FILE...",
                Some(Reach::Paths(vec!["web/a.ts".into()])),
            ),
            (
                "prettier --write FILE...",
                Some(Reach::Paths(vec!["web/a.ts".into()])),
            ),
            ("cargo fmt", Some(Reach::Workspace)),
            (
                "prettier --write .",
                Some(Reach::Paths(vec![PathBuf::from(".")])),
            ),
            ("cargo fmt --check", None),
        ];
        let mut listed: Vec<&str> = expected.iter().map(|(command, _)| *command).collect();
        listed.sort();
        let mut sorted = named.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted, listed,
            "a formatter command in the skill is not covered here"
        );
        for (command, reach) in expected {
            let found: Vec<Reach> = formatters(&command.replace("FILE...", "web/a.ts"))
                .into_iter()
                .map(|formatting| formatting.reach)
                .collect();
            assert_eq!(found, reach.into_iter().collect::<Vec<_>>(), "{command}");
        }
    }

    #[test]
    fn whole_files_are_written_replaced_and_removed_only_when_ours() {
        for vendor in [Vendor::ClaudeCode, Vendor::Cursor, Vendor::Antigravity] {
            let written = plan(vendor, None, true).unwrap().unwrap();
            assert_eq!(state(vendor, Some(&written)), Ok(State::Current));
            // An older or edited copy of LangSpan's is replaced.
            let older = written.replace("Never edit", "Do not edit");
            assert_eq!(state(vendor, Some(&older)), Ok(State::Outdated));
            assert_eq!(
                plan(vendor, Some(&older), true).unwrap(),
                Some(written.clone())
            );
            assert_eq!(plan(vendor, Some(&older), false).unwrap(), None);
            assert_eq!(plan(vendor, None, false).unwrap(), None);
            // Someone else's file of the same name stops the install and is
            // never removed.
            let error = plan(vendor, Some(OWNER), true).unwrap_err();
            assert!(error.contains("not written by LangSpan"), "{error}");
            assert_eq!(
                plan(vendor, Some(OWNER), false).unwrap(),
                Some(OWNER.into())
            );
            assert!(state(vendor, Some(OWNER)).is_err());
        }
    }

    #[test]
    fn a_section_keeps_every_other_line_and_is_removed_byte_for_byte() {
        for vendor in [Vendor::Codex, Vendor::Gemini] {
            assert_eq!(state(vendor, Some(OWNER)), Ok(State::Absent));
            let installed = plan(vendor, Some(OWNER), true).unwrap().unwrap();
            assert!(installed.starts_with(OWNER));
            assert_eq!(state(vendor, Some(&installed)), Ok(State::Current));
            assert_eq!(
                plan(vendor, Some(&installed), true).unwrap(),
                Some(installed.clone())
            );
            assert_eq!(
                plan(vendor, Some(&installed), false).unwrap(),
                Some(OWNER.into())
            );
            // A section the owner moved is replaced where it is.
            let moved = format!("{}\n{OWNER}", render(vendor)).replace("Never edit", "Do not edit");
            assert_eq!(state(vendor, Some(&moved)), Ok(State::Outdated));
            assert_eq!(
                plan(vendor, Some(&moved), true).unwrap().unwrap(),
                format!("{}\n{OWNER}", render(vendor))
            );
            // A file that held only the section is removed with it.
            let fresh = plan(vendor, None, true).unwrap().unwrap();
            assert_eq!(fresh, render(vendor));
            assert_eq!(plan(vendor, Some(&fresh), false).unwrap(), None);
            assert_eq!(
                plan(vendor, Some(OWNER), false).unwrap(),
                Some(OWNER.into())
            );
            assert_eq!(plan(vendor, None, false).unwrap(), None);
            for broken in [
                format!("{BEGIN}\nhalf\n"),
                format!("{END}\n"),
                format!("{}{}", render(vendor), render(vendor)),
            ] {
                assert!(plan(vendor, Some(&broken), true).is_err(), "{broken}");
                assert!(plan(vendor, Some(&broken), false).is_err(), "{broken}");
            }
        }
    }

    #[test]
    fn each_host_reads_the_skill_from_its_documented_place() {
        let dir = Path::new("/c");
        assert_eq!(
            path(Vendor::ClaudeCode, dir),
            Path::new("/c/skills/langspan/SKILL.md")
        );
        assert_eq!(path(Vendor::Codex, dir), Path::new("/c/AGENTS.md"));
        assert_eq!(path(Vendor::Gemini, dir), Path::new("/c/GEMINI.md"));
        assert_eq!(
            path(Vendor::Cursor, dir),
            Path::new("/c/.cursor/rules/langspan.mdc")
        );
        assert_eq!(
            path(Vendor::Antigravity, Path::new("/h/.gemini/config")),
            Path::new("/h/.gemini/config/rules/langspan.md")
        );
        assert_eq!(
            project_path(Vendor::Antigravity, Path::new("/r")),
            Path::new("/r/.agents/rules/langspan.md")
        );
        assert_eq!(project_path(Vendor::Cursor, dir), path(Vendor::Cursor, dir));
    }

    #[test]
    fn antigravitys_rule_has_the_frontmatter_without_which_it_is_discarded() {
        // A rule file without valid frontmatter and a known trigger is
        // silently discarded (<https://antigravity.google/docs/rules/>).
        let text = render(Vendor::Antigravity);
        assert!(
            text.starts_with("---\ntrigger: always_on\ndescription: \""),
            "{text}"
        );
        let frontmatter = text.split("\n---\n").next().unwrap();
        assert_eq!(frontmatter.lines().count(), 3, "{frontmatter}");
        // Antigravity truncates a rule file past 24,000 bytes.
        assert!(text.len() <= 24_000);
    }
}
