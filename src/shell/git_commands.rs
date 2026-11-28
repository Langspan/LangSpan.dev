//! The Git commands in a shell line that stage or commit, and what each
//! would take ([ADR 0026](../../docs/adr/0026-commit-guard-and-commit-mine.md)).
//!
//! Unlike the write-target grammar, this reader is lenient: an agent's commit
//! usually carries its message in a quoted heredoc inside `$(…)`, and that
//! must not hide the commit. Quotes, escapes, command substitutions and
//! heredoc bodies are read only to be skipped. What it cannot be sure of is
//! an error, and an error never holds a command: a word with an expansion
//! where a path or directory is needed, `cd` to an unknown directory,
//! `--pathspec-from-file`, another repository or work tree.

use std::path::PathBuf;

/// A longer command is not read.
pub const MAX_GIT_COMMAND_BYTES: usize = 64 * 1024;

/// One Git command that stages or commits. `cwd` is the directory it runs
/// in, relative to the shell's starting directory (absolute after an
/// absolute `cd` or `-C`), `None` for the starting directory itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitStep {
    /// `git add`: the working-tree changes under `pathspecs` (the whole tree
    /// when empty), new files too unless `update` (`-u`).
    Add {
        cwd: Option<PathBuf>,
        pathspecs: Vec<String>,
        update: bool,
    },
    /// `git commit`: what is staged; with `all` (`-a`) also the tracked
    /// files' working-tree changes; with pathspecs, the working-tree
    /// content of those paths instead (and what is staged with `include`).
    Commit {
        cwd: Option<PathBuf>,
        all: bool,
        include: bool,
        pathspecs: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Word {
    pub(super) text: String,
    /// Holds an expansion, a substitution or an unquoted glob: its value
    /// is not the text.
    pub(super) expanded: bool,
}

impl Word {
    pub(super) fn literal(&self) -> Option<&str> {
        (!self.expanded).then_some(self.text.as_str())
    }
}

/// The Git staging and commit commands of `command`, in order. `Ok(empty)`
/// when it has none.
pub fn git_steps(command: &str) -> Result<Vec<GitStep>, &'static str> {
    if command.len() > MAX_GIT_COMMAND_BYTES {
        return Err("command longer than the read limit");
    }
    // Cheap exit for the common case: no Git command at all.
    if !command.contains("git") {
        return Ok(Vec::new());
    }
    let mut steps = Vec::new();
    let mut cwd: Option<PathBuf> = None;
    let mut cwd_known = true;
    for words in Lexer::new(command).segments()? {
        let words = command_words(&words);
        let Some(first) = words.first() else {
            continue;
        };
        if first.text == "cd" {
            match words {
                [_, dir]
                    if dir
                        .literal()
                        .is_some_and(|d| d != "-" && !d.starts_with('~')) =>
                {
                    let dir = dir.text.as_str();
                    cwd = Some(match cwd {
                        Some(cwd) => cwd.join(dir),
                        None => PathBuf::from(dir),
                    });
                }
                _ => cwd_known = false,
            }
            continue;
        }
        if !(first.text == "git" || first.text.ends_with("/git")) {
            continue;
        }
        let Some(step) = git_step(&words[1..], cwd.as_ref())? else {
            continue;
        };
        if !cwd_known {
            return Err("a Git command after `cd` to a directory that is not known");
        }
        steps.push(step);
    }
    Ok(steps)
}

/// Git subcommands that can change the checkout under the sessions in it:
/// its branch, its HEAD with files, or many files at once.
const CHECKOUT_SUBCOMMANDS: &[&str] = &[
    "am",
    "checkout",
    "cherry-pick",
    "commit",
    "merge",
    "pull",
    "rebase",
    "reset",
    "restore",
    "revert",
    "stash",
    "switch",
];

/// Whether `command` runs a Git operation that can change the checkout
/// (`git merge`, `pull`, `rebase`, `reset`, `checkout`, `switch`, `stash`
/// and the like) in the repository it starts in (issue #139). A command too
/// long or too unusual to read, or one naming another Git directory or work
/// tree, does not.
pub fn moves_checkout(command: &str) -> bool {
    if command.len() > MAX_GIT_COMMAND_BYTES || !command.contains("git") {
        return false;
    }
    let Ok(segments) = Lexer::new(command).segments() else {
        return false;
    };
    segments.iter().any(|words| match command_words(words) {
        [first, rest @ ..] if first.text == "git" || first.text.ends_with("/git") => {
            subcommand(rest).is_some_and(|name| CHECKOUT_SUBCOMMANDS.contains(&name))
        }
        _ => false,
    })
}

/// The subcommand of one `git …` command, after the word `git`.
fn subcommand(words: &[Word]) -> Option<&str> {
    let mut at = 0;
    while let Some(word) = words.get(at) {
        match word.text.as_str() {
            "-C" | "-c" => at += 2,
            text if text.starts_with("--git-dir") || text.starts_with("--work-tree") => {
                return None;
            }
            text if text.starts_with('-') => at += 1,
            text => return Some(text),
        }
    }
    None
}

/// A simple command's words from the command it runs, past the words that
/// run the command that follows: assignments, reserved words and the usual
/// wrappers (`if git …`, `env X=1 git …`).
pub(super) fn command_words(mut words: &[Word]) -> &[Word] {
    loop {
        match words.first().map(|word| word.text.as_str()) {
            Some(
                "if" | "then" | "elif" | "else" | "do" | "while" | "until" | "!" | "{" | "time"
                | "command" | "nohup" | "exec" | "env" | "sudo",
            ) => words = &words[1..],
            Some("timeout") if words.len() > 1 => words = &words[2..],
            Some(text) if text.split_once('=').is_some_and(|(name, _)| is_name(name)) => {
                words = &words[1..]
            }
            _ => return words,
        }
    }
}

fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// One `git …` command's step, after the word `git`.
fn git_step(words: &[Word], cwd: Option<&PathBuf>) -> Result<Option<GitStep>, &'static str> {
    let mut cwd = cwd.cloned();
    let mut at = 0;
    // Global options, before the subcommand.
    while let Some(word) = words.get(at) {
        match word.text.as_str() {
            "-C" => {
                let dir = words
                    .get(at + 1)
                    .and_then(Word::literal)
                    .ok_or("`git -C` with a directory that is not known")?;
                cwd = Some(match cwd {
                    Some(cwd) => cwd.join(dir),
                    None => PathBuf::from(dir),
                });
                at += 2;
            }
            "-c" => at += 2,
            "--no-pager"
            | "-p"
            | "--paginate"
            | "-P"
            | "--no-optional-locks"
            | "--literal-pathspecs"
            | "--no-replace-objects"
            | "--glob-pathspecs"
            | "--noglob-pathspecs"
            | "--icase-pathspecs" => at += 1,
            text if text.starts_with('-') => {
                // `--git-dir`, `--work-tree` and the rest: another repository
                // or a work tree this reader does not follow.
                let stages = words[at..]
                    .iter()
                    .any(|word| word.text == "add" || word.text == "commit");
                return if stages {
                    Err("a Git option this reader does not follow")
                } else {
                    Ok(None)
                };
            }
            _ => break,
        }
    }
    let Some(subcommand) = words.get(at) else {
        return Ok(None);
    };
    let rest = &words[at + 1..];
    match subcommand.text.as_str() {
        "add" | "stage" => add(rest, cwd),
        "commit" => commit(rest, cwd),
        _ => Ok(None),
    }
}

fn pathspec(word: &Word) -> Result<String, &'static str> {
    word.literal()
        .map(str::to_owned)
        .ok_or("a pathspec with an expansion or an unquoted glob")
}

fn add(words: &[Word], cwd: Option<PathBuf>) -> Result<Option<GitStep>, &'static str> {
    let mut all = false;
    let mut update = false;
    let mut skip = false;
    let mut pathspecs = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let text = word.text.as_str();
        match text {
            "--" => {
                for word in words.by_ref() {
                    pathspecs.push(pathspec(word)?);
                }
            }
            "-A" | "--all" | "--no-ignore-removal" => all = true,
            "-u" | "--update" => update = true,
            // Nothing staged now, or staged by a person choosing hunks.
            "-n" | "--dry-run" | "-p" | "--patch" | "-i" | "--interactive" | "-e" | "--edit"
            | "-N" | "--intent-to-add" | "--refresh" => skip = true,
            _ if text.starts_with("--pathspec-from-file") => {
                return Err("`--pathspec-from-file`");
            }
            _ if text.starts_with("--") => {}
            _ if text.starts_with('-') && text.len() > 1 => {
                for flag in text[1..].chars() {
                    match flag {
                        'A' => all = true,
                        'u' => update = true,
                        'n' | 'p' | 'i' | 'e' | 'N' => skip = true,
                        _ => {}
                    }
                }
            }
            _ => pathspecs.push(pathspec(word)?),
        }
    }
    if skip || (pathspecs.is_empty() && !all && !update) {
        return Ok(None);
    }
    Ok(Some(GitStep::Add {
        cwd,
        pathspecs,
        update: update && !all,
    }))
}

/// Long `git commit` options that take the next word as their value.
const COMMIT_VALUES: [&str; 11] = [
    "--message",
    "--file",
    "--author",
    "--date",
    "--reuse-message",
    "--reedit-message",
    "--fixup",
    "--squash",
    "--template",
    "--cleanup",
    "--trailer",
];

/// Long `git commit` options without a separate value.
const COMMIT_FLAGS: [&str; 26] = [
    "--amend",
    "--no-verify",
    "--verify",
    "--no-edit",
    "--edit",
    "--quiet",
    "--verbose",
    "--no-verbose",
    "--signoff",
    "--no-signoff",
    "--allow-empty",
    "--allow-empty-message",
    "--no-gpg-sign",
    "--gpg-sign",
    "--reset-author",
    "--status",
    "--no-status",
    "--only",
    "--porcelain",
    "--short",
    "--long",
    "--null",
    "--no-post-rewrite",
    "--untracked-files",
    "--branch",
    "--no-ahead-behind",
];

fn commit(words: &[Word], cwd: Option<PathBuf>) -> Result<Option<GitStep>, &'static str> {
    let mut all = false;
    let mut include = false;
    let mut skip = false;
    let mut pathspecs = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let text = word.text.as_str();
        at += 1;
        match text {
            "--" => {
                for word in &words[at..] {
                    pathspecs.push(pathspec(word)?);
                }
                break;
            }
            "-a" | "--all" => all = true,
            "-i" | "--include" => include = true,
            "--dry-run" | "--interactive" | "--patch" | "-p" => skip = true,
            _ if text.starts_with("--pathspec-from-file") => {
                return Err("`--pathspec-from-file`");
            }
            _ if text.starts_with("--") => {
                let name = text.split_once('=').map_or(text, |(name, _)| name);
                if COMMIT_VALUES.contains(&text) {
                    at += 1;
                } else if !COMMIT_FLAGS.contains(&name) && !COMMIT_VALUES.contains(&name) {
                    // An abbreviation or an option this reader does not
                    // know may take a value: the pathspecs are not known.
                    return Err("a `git commit` option this reader does not know");
                }
            }
            _ if text.starts_with('-') && text.len() > 1 => {
                let flags: Vec<char> = text[1..].chars().collect();
                for (index, flag) in flags.iter().enumerate() {
                    match flag {
                        'a' => all = true,
                        'i' => include = true,
                        'p' => skip = true,
                        // A value: the rest of the word, or the next word.
                        'm' | 'F' | 'C' | 'c' | 't' => {
                            if index + 1 == flags.len() {
                                at += 1;
                            }
                            break;
                        }
                        // An optional value, only ever attached.
                        'u' | 'S' => break,
                        _ => {}
                    }
                }
            }
            _ => pathspecs.push(pathspec(word)?),
        }
    }
    if skip {
        return Ok(None);
    }
    Ok(Some(GitStep::Commit {
        cwd,
        all,
        include,
        pathspecs,
    }))
}

// ---------------------------------------------------------------------------
// A lenient shell lexer: words and command boundaries, nothing more
// ---------------------------------------------------------------------------

pub(super) struct Lexer {
    chars: Vec<char>,
    at: usize,
    /// Heredoc delimiters whose bodies start after the current line: the
    /// delimiter, `<<-`, and whether it is quoted (the body not expanded).
    pending: Vec<(String, bool, bool)>,
    /// How many substitutions the lexer is inside.
    depth: usize,
    /// The text of each outermost substitution, unquoted heredoc bodies'
    /// included: commands the shell runs first.
    substituted: Vec<String>,
}

impl Lexer {
    pub(super) fn new(command: &str) -> Self {
        Self {
            chars: command.chars().collect(),
            at: 0,
            pending: Vec::new(),
            depth: 0,
            substituted: Vec::new(),
        }
    }

    /// The simple commands of the line, as words.
    pub(super) fn segments(self) -> Result<Vec<Vec<Word>>, &'static str> {
        self.read().map(|(segments, _)| segments)
    }

    /// Read the rest of a substitution with `skip`, keeping its text when it
    /// is outermost.
    fn substitute(
        &mut self,
        skip: fn(&mut Self) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let start = self.at;
        self.depth += 1;
        let skipped = skip(self);
        self.depth -= 1;
        skipped?;
        if self.depth == 0 {
            // Without the closing `)` or backquote.
            let end = self.at.saturating_sub(1).max(start);
            let text = self.chars.get(start..end).unwrap_or_default();
            self.substituted.push(text.iter().collect());
        }
        Ok(())
    }

    /// Read the rest of a backquote substitution, keeping its text as Bash
    /// runs it: without the backslash before `$`, a backquote or a
    /// backslash, or before `"` when `quoted` (inside double quotes).
    fn backquote(&mut self, quoted: bool) -> Result<(), &'static str> {
        let kept = self.substituted.len();
        self.substitute(Self::backquoted)?;
        if let Some(text) = self.substituted.get_mut(kept) {
            let mut decoded = String::with_capacity(text.len());
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\'
                    && let Some(&next) = chars.peek()
                    && (matches!(next, '$' | '`' | '\\') || (quoted && next == '"'))
                {
                    decoded.push(next);
                    chars.next();
                } else {
                    decoded.push(c);
                }
            }
            *text = decoded;
        }
        Ok(())
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    /// The simple commands of the line, as words, and the text of each
    /// substitution the shell runs before them.
    pub(super) fn read(mut self) -> Result<(Vec<Vec<Word>>, Vec<String>), &'static str> {
        let mut segments = vec![Vec::new()];
        let mut word: Option<Word> = None;
        let end_word = |segments: &mut Vec<Vec<Word>>, word: &mut Option<Word>| {
            if let Some(word) = word.take()
                && let Some(last) = segments.last_mut()
            {
                last.push(word);
            }
        };
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' => {
                    self.at += 1;
                    end_word(&mut segments, &mut word);
                }
                '\n' | ';' | '&' | '|' | '(' | ')' => {
                    self.at += 1;
                    end_word(&mut segments, &mut word);
                    if c == '\n' {
                        self.skip_heredoc_bodies()?;
                    }
                    if segments.last().is_some_and(|last| !last.is_empty()) {
                        segments.push(Vec::new());
                    }
                }
                // A line continuation: Bash removes it, between words too.
                '\\' if self.peek(1) == Some('\n') => self.at += 2,
                '#' if word.is_none() => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                '<' if self.peek(1) == Some('<') && self.peek(2) != Some('<') => {
                    end_word(&mut segments, &mut word);
                    self.at += 2;
                    self.heredoc_delimiter()?;
                }
                // A process substitution: part of a word.
                '<' | '>' if self.peek(1) == Some('(') => self.word_part(&mut word)?,
                '<' | '>' => {
                    // A redirection: its descriptor and its target are not
                    // words of the command.
                    if word.as_ref().is_some_and(|w| {
                        !w.text.is_empty() && w.text.bytes().all(|b| b.is_ascii_digit())
                    }) {
                        word = None;
                    }
                    end_word(&mut segments, &mut word);
                    self.at += 1;
                    while matches!(self.peek(0), Some('>' | '&' | '|' | '<')) {
                        self.at += 1;
                    }
                    while matches!(self.peek(0), Some(' ' | '\t')) {
                        self.at += 1;
                    }
                    let mut target = None;
                    self.word_part(&mut target)?;
                    while self.peek(0).is_some_and(|c| {
                        !matches!(
                            c,
                            ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
                        )
                    }) {
                        self.word_part(&mut target)?;
                    }
                }
                _ => self.word_part(&mut word)?,
            }
        }
        end_word(&mut segments, &mut word);
        segments.retain(|segment| !segment.is_empty());
        Ok((segments, self.substituted))
    }

    /// Read one piece of a word (a character, a quoted string, an escape or
    /// a substitution) into `word`.
    fn word_part(&mut self, word: &mut Option<Word>) -> Result<(), &'static str> {
        let Some(c) = self.peek(0) else {
            return Ok(());
        };
        let word = word.get_or_insert_with(|| Word {
            text: String::new(),
            expanded: false,
        });
        self.at += 1;
        match c {
            '\'' => loop {
                match self.peek(0) {
                    Some('\'') => {
                        self.at += 1;
                        break;
                    }
                    Some(c) => {
                        word.text.push(c);
                        self.at += 1;
                    }
                    None => return Err("an unterminated quote"),
                }
            },
            '"' => self.double_quoted(word)?,
            '\\' => match self.peek(0) {
                Some('\n') => self.at += 1,
                Some(c) => {
                    word.text.push(c);
                    self.at += 1;
                }
                None => {}
            },
            '$' => {
                word.expanded = true;
                self.dollar(false)?;
            }
            '`' => {
                word.expanded = true;
                self.backquote(false)?;
            }
            '<' | '>' if self.peek(0) == Some('(') => {
                word.expanded = true;
                self.at += 1;
                self.substitute(Self::substitution)?;
            }
            '*' | '?' | '[' | '~' | '{' => {
                word.expanded = true;
                word.text.push(c);
            }
            c => word.text.push(c),
        }
        Ok(())
    }

    fn double_quoted(&mut self, word: &mut Word) -> Result<(), &'static str> {
        loop {
            let Some(c) = self.peek(0) else {
                return Err("an unterminated quote");
            };
            self.at += 1;
            match c {
                '"' => return Ok(()),
                '\\' => {
                    if let Some(next) = self.peek(0) {
                        if !matches!(next, '"' | '\\' | '$' | '`' | '\n') {
                            word.text.push('\\');
                        }
                        if next != '\n' {
                            word.text.push(next);
                        }
                        self.at += 1;
                    }
                }
                '$' => {
                    word.expanded = true;
                    self.dollar(true)?;
                }
                '`' => {
                    word.expanded = true;
                    self.backquote(true)?;
                }
                c => word.text.push(c),
            }
        }
    }

    /// After a `$`: a substitution, a parameter or a lone dollar sign;
    /// `quoted` inside double quotes.
    fn dollar(&mut self, quoted: bool) -> Result<(), &'static str> {
        match self.peek(0) {
            Some('(') => {
                self.at += 1;
                self.substitute(Self::substitution)
            }
            Some('{') => {
                self.at += 1;
                self.parameter(quoted)
            }
            Some('\'') => {
                // `$'…'`: skip to the closing quote, honouring escapes.
                self.at += 1;
                while let Some(c) = self.peek(0) {
                    self.at += 1;
                    match c {
                        '\\' => self.at += 1,
                        '\'' => return Ok(()),
                        _ => {}
                    }
                }
                Err("an unterminated quote")
            }
            _ => {
                while self
                    .peek(0)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    self.at += 1;
                }
                Ok(())
            }
        }
    }

    /// After `${`: to the closing brace, keeping the substitutions in it
    /// (`${x:-$(cmd)}` runs `cmd`). Outside double quotes (`quoted` false),
    /// single quotes in it make their text literal, as Bash reads them.
    fn parameter(&mut self, quoted: bool) -> Result<(), &'static str> {
        let mut depth = 1_usize;
        let mut double = false;
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match c {
                '\\' => self.at += 1,
                '\'' if !quoted && !double => {
                    while self.peek(0).is_some_and(|c| c != '\'') {
                        self.at += 1;
                    }
                    if self.peek(0).is_none() {
                        return Err("an unterminated quote");
                    }
                    self.at += 1;
                }
                '"' => double = !double,
                '$' if self.peek(0) == Some('{') => {
                    self.at += 1;
                    depth += 1;
                }
                '$' if self.peek(0) == Some('(') => {
                    self.at += 1;
                    self.substitute(Self::substitution)?;
                }
                '`' => self.backquote(quoted || double)?,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err("an unterminated parameter expansion")
    }

    /// The substitutions in an unquoted heredoc's body, read as Bash expands
    /// it: quotes and `#` are text there, and a backslash escapes only `$`,
    /// a backquote, a backslash and a newline.
    fn heredoc_substitutions(body: &str) -> Result<Vec<String>, &'static str> {
        let mut lexer = Self::new(body);
        while let Some(c) = lexer.peek(0) {
            lexer.at += 1;
            match c {
                '\\' if matches!(lexer.peek(0), Some('$' | '`' | '\\' | '\n')) => lexer.at += 1,
                '$' if lexer.peek(0) == Some('(') => {
                    lexer.at += 1;
                    lexer.substitute(Self::substitution)?;
                }
                '$' if lexer.peek(0) == Some('{') => {
                    lexer.at += 1;
                    lexer.parameter(true)?;
                }
                '`' => lexer.backquote(false)?,
                _ => {}
            }
        }
        Ok(lexer.substituted)
    }

    fn backquoted(&mut self) -> Result<(), &'static str> {
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match c {
                '\\' => self.at += 1,
                '`' => return Ok(()),
                _ => {}
            }
        }
        Err("an unterminated backquote")
    }

    /// Skip a `$(…)` body, with its quotes, nested substitutions and
    /// heredocs, to the closing parenthesis.
    fn substitution(&mut self) -> Result<(), &'static str> {
        let mut depth = 1_usize;
        let mut word_start = true;
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match c {
                '\'' => {
                    while self.peek(0).is_some_and(|c| c != '\'') {
                        self.at += 1;
                    }
                    self.at += 1;
                }
                '"' => {
                    let mut scratch = Word {
                        text: String::new(),
                        expanded: false,
                    };
                    self.double_quoted(&mut scratch)?;
                }
                '\\' => self.at += 1,
                '`' => self.backquoted()?,
                '$' if self.peek(0) == Some('(') => {
                    self.at += 1;
                    self.substitution()?;
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                '<' if self.peek(0) == Some('<') && self.peek(1) != Some('<') => {
                    self.at += 1;
                    self.heredoc_delimiter()?;
                }
                '#' if word_start => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                '\n' => self.skip_heredoc_bodies()?,
                _ => {}
            }
            word_start = matches!(c, ' ' | '\t' | '\n' | ';' | '&' | '|' | '(');
        }
        Err("an unterminated command substitution")
    }

    /// After `<<`: the delimiter word, whose body follows the current line.
    fn heredoc_delimiter(&mut self) -> Result<(), &'static str> {
        let strip_tabs = self.peek(0) == Some('-');
        if strip_tabs {
            self.at += 1;
        }
        while matches!(self.peek(0), Some(' ' | '\t')) {
            self.at += 1;
        }
        let mut delimiter = String::new();
        let mut quoted = false;
        while let Some(c) = self.peek(0) {
            match c {
                '\'' | '"' => {
                    quoted = true;
                    self.at += 1;
                    while let Some(inner) = self.peek(0) {
                        self.at += 1;
                        if inner == c {
                            break;
                        }
                        delimiter.push(inner);
                    }
                }
                '\\' => {
                    quoted = true;
                    self.at += 1;
                }
                c if c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '<' | '>') => {
                    break;
                }
                c => {
                    delimiter.push(c);
                    self.at += 1;
                }
            }
        }
        if delimiter.is_empty() {
            return Err("a heredoc without a delimiter");
        }
        self.pending.push((delimiter, strip_tabs, quoted));
        Ok(())
    }

    /// At the start of a line: skip the bodies of the heredocs begun on the
    /// line before, keeping the substitutions of an unquoted one.
    fn skip_heredoc_bodies(&mut self) -> Result<(), &'static str> {
        for (delimiter, strip_tabs, quoted) in std::mem::take(&mut self.pending) {
            let mut body = String::new();
            loop {
                let start = self.at;
                while self.peek(0).is_some_and(|c| c != '\n') {
                    self.at += 1;
                }
                let line: String = self.chars[start..self.at].iter().collect();
                let at_end = self.peek(0).is_none();
                self.at = (self.at + 1).min(self.chars.len());
                let line = if strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if line == delimiter {
                    break;
                }
                body.push_str(line);
                body.push('\n');
                if at_end {
                    break;
                }
            }
            // Inside a substitution, the outermost one's text holds the body.
            if !quoted && self.depth == 0 {
                self.substituted.extend(Self::heredoc_substitutions(&body)?);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(command: &str) -> Vec<GitStep> {
        git_steps(command).unwrap_or_else(|e| panic!("{command}: {e}"))
    }

    fn commit(all: bool, pathspecs: &[&str]) -> GitStep {
        GitStep::Commit {
            cwd: None,
            all,
            include: false,
            pathspecs: pathspecs.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    fn add(pathspecs: &[&str], update: bool) -> GitStep {
        GitStep::Add {
            cwd: None,
            pathspecs: pathspecs.iter().map(|p| (*p).to_owned()).collect(),
            update,
        }
    }

    #[test]
    fn a_git_operation_that_changes_the_checkout_is_recognised() {
        for command in [
            "git merge origin/main",
            "git pull --rebase",
            "git rebase main",
            "git reset --hard HEAD~1",
            "git checkout other",
            "git switch -c topic",
            "git stash",
            "git -C . -c core.pager=cat merge --no-edit origin/main",
            "cargo build && git fetch origin && git merge origin/main",
            "if git merge origin/main; then echo ok; fi",
            "env GIT_EDITOR=true git rebase --continue",
            "/usr/bin/git cherry-pick abc123",
            "git commit -m \"$(cat <<'EOF'\nmerge it\nEOF\n)\"",
        ] {
            assert!(moves_checkout(command), "{command}");
        }
        for command in [
            "git status",
            "git log --oneline -- merge",
            "git fetch origin",
            "git diff origin/main",
            "echo git merge",
            "cargo test",
            "git --git-dir=/elsewhere/.git merge x",
            "git --work-tree /elsewhere checkout x",
            "",
        ] {
            assert!(!moves_checkout(command), "{command}");
        }
        let long = format!("git merge {}", "x".repeat(MAX_GIT_COMMAND_BYTES));
        assert!(!moves_checkout(&long), "a command past the read limit");
    }

    #[test]
    fn each_broad_form_is_recognised() {
        assert_eq!(steps("git add -A"), [add(&[], false)]);
        assert_eq!(steps("git add --all"), [add(&[], false)]);
        assert_eq!(steps("git add ."), [add(&["."], false)]);
        assert_eq!(steps("git add -u"), [add(&[], true)]);
        assert_eq!(steps("git add -Av src"), [add(&["src"], false)]);
        assert_eq!(steps("git commit -am 'msg'"), [commit(true, &[])]);
        assert_eq!(steps("git commit -a -m msg"), [commit(true, &[])]);
        assert_eq!(steps("git commit --all --message msg"), [commit(true, &[])]);
        assert_eq!(steps("git commit -m msg"), [commit(false, &[])]);
        assert_eq!(
            steps("git commit -mmsg file.rs"),
            [commit(false, &["file.rs"])]
        );
        assert_eq!(
            steps("git commit -m 'msg' -- a.txt b.txt"),
            [commit(false, &["a.txt", "b.txt"])]
        );
        assert_eq!(
            steps("git add -A && git commit -m x"),
            [add(&[], false), commit(false, &[])]
        );
    }

    #[test]
    fn a_heredoc_message_inside_a_substitution_does_not_hide_the_commit() {
        let command = "git commit -a -m \"$(cat <<'EOF'\nfix(core): a (message) with \"quotes\" and ) and `ticks`\n\nCo-Authored-By: x <y>\nEOF\n)\" && git log -1";
        assert_eq!(steps(command), [commit(true, &[])]);
        let unquoted = "git commit -F - <<EOF\nmessage; git add -A\nEOF\ngit status";
        assert_eq!(steps(unquoted), [commit(false, &[])]);
    }

    /// Review of PR #613: a single-quoted parameter default is literal, so
    /// an unpaired backquote in it does not hide the commit.
    #[test]
    fn a_single_quoted_parameter_default_is_literal() {
        for command in [
            "git commit -m ${msg:-'fix: `example'}",
            "git commit -m ${msg:-'a } and $(x'}",
        ] {
            assert_eq!(steps(command), [commit(false, &[])], "{command}");
        }
    }

    #[test]
    fn the_directory_follows_cd_and_git_c() {
        assert_eq!(
            steps("cd sub && git add ."),
            [GitStep::Add {
                cwd: Some("sub".into()),
                pathspecs: vec![".".into()],
                update: false
            }]
        );
        assert_eq!(
            steps("git -C /repo commit -a -m x"),
            [GitStep::Commit {
                cwd: Some("/repo".into()),
                all: true,
                include: false,
                pathspecs: vec![]
            }]
        );
        assert!(git_steps("cd $DIR && git commit -am x").is_err());
        assert!(git_steps("cd - && git commit -am x").is_err());
    }

    #[test]
    fn what_cannot_be_known_is_an_error_and_what_stages_nothing_is_not_a_step() {
        assert!(git_steps("git add *.rs").is_err());
        assert!(git_steps("git add \"$FILE\"").is_err());
        assert!(git_steps("git commit --pathspec-from-file=list").is_err());
        assert!(git_steps("git --git-dir=/x commit -am y").is_err());
        assert_eq!(steps("git add -p"), []);
        assert_eq!(steps("git add --dry-run ."), []);
        assert_eq!(steps("git add"), []);
        assert_eq!(steps("git commit --dry-run -a"), []);
        assert_eq!(steps("git status && git log"), []);
        assert_eq!(steps("echo git commit -am x"), []);
        assert_eq!(steps("langspan commit --mine -m x"), []);
        assert_eq!(steps("# git commit -am x"), []);
        // A quoted glob is Git's own pathspec, exactly what Git will use.
        assert_eq!(steps("git add '*.rs'"), [add(&["*.rs"], false)]);
    }

    #[test]
    fn redirections_and_assignments_are_not_pathspecs() {
        assert_eq!(
            steps("git commit -am x 2>&1 | tail -3"),
            [commit(true, &[])]
        );
        assert_eq!(
            steps("GIT_EDITOR=true git commit -a > /tmp/out"),
            [commit(true, &[])]
        );
    }

    #[test]
    fn wrappers_and_reserved_words_do_not_hide_a_commit() {
        for command in [
            "if true; then git commit -am x; fi",
            "{ git commit -am x; }",
            "time git commit -am x",
            "env GIT_EDITOR=true git commit -am x",
            "timeout 60 git commit -am x",
            "! git commit -am x",
        ] {
            assert_eq!(steps(command), [commit(true, &[])], "{command}");
        }
        assert_eq!(steps("git stage -A"), [add(&[], false)]);
        assert_eq!(
            steps("git commit --amend --no-edit -a"),
            [commit(true, &[])]
        );
        // An abbreviated option may take a value: not guessed.
        assert!(git_steps("git commit --mess x").is_err());
    }
}
