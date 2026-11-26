//! The LangSpan block for `AGENTS.md`, `GEMINI.md` and rules files
//! (roadmap I7, [ADR 0018](../docs/adr/0018-other-hosts-after-launch.md)).
//!
//! An agent on a host without LangSpan's hooks, or whose hooks are not yet
//! trusted, reads the repository's guidance file but is told nothing else.
//! This block tells it that LangSpan coordinates the repository and which
//! MCP tools to call. It is advice: nothing is held or refused through it.
//!
//! `langspan guidance` prints it; `guidance install` writes it into a file
//! of the owner's repository between two marker lines, and `guidance
//! remove` takes exactly those lines out again. Every other byte of the file
//! is the owner's and is kept as it is.

use crate::install;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// The block's text between its markers.
pub const BODY: &str = include_str!("guidance.md");
pub const BEGIN: &str = "<!-- langspan:begin -->";
pub const END: &str = "<!-- langspan:end -->";
/// The most the block, markers included, may hold, so that it costs little
/// of a host's instruction budget (Codex reads 32 KiB of its combined
/// AGENTS files by default).
pub const MAX_BYTES: usize = 1500;
/// Cursor's rule front matter, before the block, for `--format cursor-rules`.
const CURSOR_RULE: &str = "---\ndescription: \"LangSpan coordinates the agent sessions working in this repository.\"\nalwaysApply: true\n---\n";

/// The forms `langspan guidance --format` prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `AGENTS.md`, read by Codex, Cursor and other hosts.
    Agents,
    /// Gemini CLI's `GEMINI.md`: the same Markdown.
    Gemini,
    /// A Cursor project rule (`.cursor/rules/*.mdc`): front matter, then
    /// the block.
    CursorRules,
}

impl Format {
    pub const USAGE: &str = "agents|gemini|cursor-rules";

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "agents" => Some(Self::Agents),
            "gemini" => Some(Self::Gemini),
            "cursor-rules" => Some(Self::CursorRules),
            _ => None,
        }
    }
}

/// The block, markers included, exactly as `install` writes it.
pub fn block() -> String {
    format!("{BEGIN}\n{BODY}{END}\n")
}

/// The text `langspan guidance --format` prints.
pub fn render(format: Format) -> String {
    match format {
        Format::Agents | Format::Gemini => block(),
        Format::CursorRules => format!("{CURSOR_RULE}{}", block()),
    }
}

/// The MCP tools the block names: every `langspan_*` span between backticks.
pub fn tools() -> Vec<&'static str> {
    BODY.split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("langspan_"))
        .collect()
}

/// Where LangSpan's block is in a file, by line index (end inclusive).
/// A marker without its partner, a second block, a marker line LangSpan
/// does not write, or a block whose text is not LangSpan's stops here, with
/// the line numbers, before anything is written.
fn find(lines: &[&str]) -> Result<Option<(usize, usize)>, String> {
    let mut open: Option<usize> = None;
    let mut found: Option<(usize, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let trimmed = line.trim();
        if trimmed == BEGIN {
            if let Some(start) = open {
                return Err(format!(
                    "line {number} begins a LangSpan block inside the one begun at line {}",
                    start + 1
                ));
            }
            if let Some((start, _)) = found {
                return Err(format!(
                    "a second LangSpan block begins at line {number}; the first begins at line {}. Delete one by hand",
                    start + 1
                ));
            }
            open = Some(index);
        } else if trimmed == END {
            match open.take() {
                Some(start) => found = Some((start, index)),
                None => {
                    return Err(format!(
                        "line {number} ends a LangSpan block that no line begins"
                    ));
                }
            }
        } else if trimmed.starts_with("<!--")
            && (trimmed.contains("langspan:begin") || trimmed.contains("langspan:end"))
        {
            return Err(format!(
                "line {number} is a changed LangSpan marker; LangSpan writes exactly {BEGIN} and {END}"
            ));
        }
    }
    if let Some(start) = open {
        return Err(format!(
            "line {} begins a LangSpan block that no line ends",
            start + 1
        ));
    }
    if let Some((start, end)) = found {
        // A checkout that turned line endings into CRLF has not edited it.
        let text = lines[start..=end].concat().replace("\r\n", "\n");
        if text != block() {
            return Err(format!(
                "lines {}-{} hold a LangSpan block whose text LangSpan did not write (edited by hand, or from another version). Replace those lines with what `langspan guidance` prints, or delete them, and run this again",
                start + 1,
                end + 1
            ));
        }
    }
    Ok(found)
}

/// How many leading lines are YAML front matter: a first line `---`, up to
/// and including the next `---` line. A rule file's host reads its settings
/// (`globs`, `alwaysApply`) only there, so the block goes after it. A closing
/// line without a newline cannot be followed by the block without changing
/// the owner's bytes, and is refused.
fn front_matter(lines: &[&str]) -> Result<usize, String> {
    let fence = |line: &&str| line.trim_end_matches(['\n', '\r']) == "---";
    if !lines.first().is_some_and(fence) {
        return Ok(0);
    }
    match lines.iter().skip(1).position(fence) {
        None => Ok(0),
        Some(index) if !lines[index + 1].ends_with('\n') => Err(format!(
            "the front matter's closing line {} has no newline after it; add one so the block can follow the front matter",
            index + 2
        )),
        Some(index) => Ok(index + 2),
    }
}

/// The file's content with the block written (`install`) or removed, from
/// what it holds now (`None`: absent). `Ok(None)` means the file should not
/// exist. Outside LangSpan's block nothing changes.
///
/// A new block goes at the top, after any front matter, followed by one
/// blank line, so that an agent reads it first, the host still finds the
/// front matter first, and removing it gives back the owner's bytes
/// exactly, whatever the file's last line was.
pub fn plan(existing: Option<&str>, install: bool) -> Result<Option<String>, String> {
    let text = existing.unwrap_or("");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let found = find(&lines).map_err(|error| format!("{error}; nothing was changed"))?;
    let planned = match (found, install) {
        (Some(_), true) | (None, false) => return Ok(existing.map(str::to_owned)),
        (None, true) if text.is_empty() => block(),
        (None, true) => {
            let top = front_matter(&lines).map_err(|e| format!("{e}; nothing was changed"))?;
            let (front, rest) = lines.split_at(top);
            format!("{}{}\n{}", front.concat(), block(), rest.concat())
        }
        (Some((start, end)), false) => {
            let mut after = end + 1;
            // The blank line install put after a block at the top.
            let top = front_matter(&lines[..start]).unwrap_or(0);
            if start == top && matches!(lines.get(after), Some(&"\n" | &"\r\n")) {
                after += 1;
            }
            format!("{}{}", lines[..start].concat(), lines[after..].concat())
        }
    };
    // A file that held only LangSpan's block goes with it.
    Ok((!planned.is_empty()).then_some(planned))
}

/// The lines that differ between `old` and `new`, as `- ` and `+ ` lines.
/// The change is always one run of lines, so the common lines at each end
/// are left out.
pub fn diff(old: &str, new: &str) -> String {
    let old: Vec<&str> = old.split_inclusive('\n').collect();
    let new: Vec<&str> = new.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = String::new();
    let mut line = |sign: char, text: &str| {
        let text = text.trim_end_matches(['\n', '\r']);
        out.push(sign);
        if !text.is_empty() {
            out.push(' ');
            out.push_str(text);
        }
        out.push('\n');
    };
    for text in &old[prefix..old.len() - suffix] {
        line('-', text);
    }
    for text in &new[prefix..new.len() - suffix] {
        line('+', text);
    }
    out
}

/// The file `--file` names under `root`: a relative path inside the root,
/// whose folder exists and is inside the root once symlinks are resolved.
pub fn target(root: &Path, file: &str) -> Result<PathBuf, crate::Error> {
    let relative = Path::new(file);
    if file.is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "--file {file:?} must be a path inside --root, such as AGENTS.md, without `..`; nothing was changed"
        )
        .into());
    }
    let root = fs::canonicalize(root)
        .map_err(|e| format!("--root {}: {e}; nothing was changed", root.display()))?;
    if !root.is_dir() {
        return Err(format!(
            "--root {} is not a directory; nothing was changed",
            root.display()
        )
        .into());
    }
    let path = root.join(relative);
    let parent = path.parent().unwrap_or(&root);
    let folder = fs::canonicalize(parent).map_err(|e| {
        format!(
            "{}: {e}; create the folder first. Nothing was changed",
            parent.display()
        )
    })?;
    if !folder.starts_with(&root) {
        return Err(format!(
            "{} leads outside --root {} through a symlink; nothing was changed",
            path.display(),
            root.display()
        )
        .into());
    }
    Ok(path)
}

/// One file's planned change, made from the bytes it held when planned.
pub struct FilePlan {
    pub path: PathBuf,
    pub install: bool,
    pub original: Option<Vec<u8>>,
    pub planned: Option<String>,
}

impl FilePlan {
    pub fn unchanged(&self) -> bool {
        self.original.as_deref() == self.planned.as_deref().map(str::as_bytes)
    }

    /// The change as `- ` and `+ ` lines.
    pub fn diff(&self) -> String {
        let old = self
            .original
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        diff(&old, self.planned.as_deref().unwrap_or(""))
    }

    /// Write or remove the file, unless it changed since it was planned.
    pub fn apply(&self) -> Result<(), crate::Error> {
        let path = &self.path;
        if self.planned.is_some() {
            let install = self.install;
            let replan = |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
                let existing = install::parse_text(path, now)?;
                match plan(existing.as_deref(), install)
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
            install::write_planned(path, self.original.as_deref(), &replan)?;
            return Ok(());
        }
        if self.original.is_none() {
            return Ok(());
        }
        if install::read_bytes(path)? != self.original {
            return Err(format!(
                "{} changed while LangSpan was removing its block; nothing was removed. Run this again",
                path.display()
            )
            .into());
        }
        fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        Ok(())
    }
}

/// Plan the block's change to the file at `path`. Every refusal is raised
/// here, before anything is written: a symlink or other non-regular file,
/// more than 1 MiB, not UTF-8, broken or edited markers, and a missing file
/// to install into without `create`.
pub fn plan_file(path: &Path, install: bool, create: bool) -> Result<FilePlan, crate::Error> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(format!(
            "{} is a symlink or not a regular file; LangSpan never writes through one. Nothing was changed",
            path.display()
        )
        .into());
    }
    let original = install::read_bytes(path)?;
    if install && original.is_none() && !create {
        return Err(format!(
            "{} does not exist; add --create to create it with only LangSpan's block. Nothing was changed",
            path.display()
        )
        .into());
    }
    let existing = install::parse_text(path, original.as_deref())?;
    let planned =
        plan(existing.as_deref(), install).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(FilePlan {
        path: path.to_owned(),
        install,
        original,
        planned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "# Notes\n\nUse tabs.";

    #[test]
    fn the_block_stays_small_and_names_its_markers_once() {
        let text = block();
        assert!(text.len() <= MAX_BYTES, "{} bytes", text.len());
        assert_eq!(text.matches(BEGIN).count(), 1);
        assert_eq!(text.matches(END).count(), 1);
        assert!(!BODY.contains("langspan:"));
        for format in [Format::Agents, Format::Gemini, Format::CursorRules] {
            assert!(render(format).len() <= MAX_BYTES);
        }
    }

    #[test]
    fn the_block_names_only_tools_the_server_lists() {
        let named = tools();
        for tool in [
            "langspan_brief",
            "langspan_intent_set",
            "langspan_admit",
            "langspan_complete",
            "langspan_commit_mine",
        ] {
            assert!(named.contains(&tool), "{tool} missing");
        }
        for tool in named {
            assert!(
                crate::mcp::TOOL_NAMES.contains(&tool),
                "{tool} is not a LangSpan tool"
            );
        }
    }

    #[test]
    fn install_then_remove_gives_back_every_byte() {
        for original in [OWNER, "", "\n", "x\n", "\r\nA\r\n", "\n\nB"] {
            let installed = plan(Some(original), true).unwrap().unwrap();
            assert!(installed.starts_with(&block()));
            assert_eq!(plan(Some(&installed), true).unwrap().unwrap(), installed);
            let removed = plan(Some(&installed), false).unwrap();
            assert_eq!(removed.as_deref().unwrap_or(""), original, "{original:?}");
        }
        assert_eq!(plan(None, true).unwrap().unwrap(), block());
        assert_eq!(plan(None, false).unwrap(), None);
        assert_eq!(plan(Some(&block()), false).unwrap(), None);
    }

    #[test]
    fn a_block_in_crlf_is_still_langspans() {
        let crlf = format!("{}\r\n{OWNER}", block().replace('\n', "\r\n"));
        assert_eq!(plan(Some(&crlf), true).unwrap().unwrap(), crlf);
        assert_eq!(plan(Some(&crlf), false).unwrap().unwrap(), OWNER);
    }

    #[test]
    fn the_diff_shows_only_the_changed_lines() {
        let installed = plan(Some(OWNER), true).unwrap().unwrap();
        let shown = diff(OWNER, &installed);
        assert!(shown.starts_with(&format!("+ {BEGIN}\n")), "{shown}");
        assert!(shown.ends_with(&format!("+ {END}\n+\n")), "{shown}");
        assert_eq!(shown.lines().count(), block().lines().count() + 1);
        let back = diff(&installed, OWNER);
        assert!(back.lines().all(|line| line.starts_with('-')), "{back}");
        assert_eq!(diff(OWNER, OWNER), "");
    }
}
