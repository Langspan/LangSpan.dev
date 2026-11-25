//! The code formatters in a shell line and which files each may rewrite
//! ([ADR 0011](../../docs/adr/0011-shell-writes.md), issue #126).
//!
//! A formatter run on a whole tree rewrites every file it formats there,
//! including another session's uncommitted ones. This reader recognises a few
//! formatters and says how far each reaches, so the pre-tool hook can tell the
//! session whose files it may rewrite. It only informs: nothing is refused on
//! what it finds, and a command it cannot read is simply not recognised. Where
//! it is unsure how far a formatter reaches, it says the whole workspace.
//!
//! - `cargo fmt` formats every target of the package or workspace, whatever
//!   files are given after `--`: those are added, not substituted (observed
//!   with rustfmt 1.11.0 on a crate whose root declares another module).
//! - `rustfmt FILE...` formats those files and the modules they declare.
//! - `prettier --write PATH...` formats those files, and in a directory or a
//!   glob every file of the languages it supports.

use super::git_commands::{Lexer, Word, command_words};
use std::path::PathBuf;

/// A longer command is not read.
pub const MAX_FORMATTER_COMMAND_BYTES: usize = 64 * 1024;

/// How far one formatter run reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Every file of its languages in the workspace.
    Workspace,
    /// Each path as given (relative to the directory the shell starts in, or
    /// absolute): a file named, or every file of its languages under a
    /// directory.
    Paths(Vec<PathBuf>),
}

/// One formatter run a shell line makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatting {
    /// The formatter, as a notice names it.
    pub tool: &'static str,
    /// The file name extensions it formats under a directory. A file named
    /// explicitly is formatted whatever its extension.
    pub extensions: &'static [&'static str],
    pub reach: Reach,
}

const RUST: &[&str] = &["rs"];
/// The extensions Prettier 3 formats without a plugin.
const PRETTIER: &[&str] = &[
    "js",
    "jsx",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "mts",
    "cts",
    "json",
    "json5",
    "jsonc",
    "css",
    "scss",
    "less",
    "html",
    "htm",
    "vue",
    "md",
    "markdown",
    "mdx",
    "yaml",
    "yml",
    "graphql",
    "gql",
    "hbs",
    "handlebars",
];

/// The formatter runs of `command` that may rewrite files, in order.
/// Checks (`--check`) and runs that print rather than write are left out.
/// A line that cannot be read has none.
pub fn formatters(command: &str) -> Vec<Formatting> {
    if command.len() > MAX_FORMATTER_COMMAND_BYTES
        || !(command.contains("fmt") || command.contains("prettier"))
    {
        return Vec::new();
    }
    let Ok(segments) = Lexer::new(command).segments() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    // The directory the shell is in, relative to where it started; `None`
    // after a `cd` this reader cannot follow.
    let mut cwd = Some(PathBuf::new());
    for words in &segments {
        let words = runner_words(command_words(words));
        let Some(first) = words.first() else {
            continue;
        };
        if first.text == "cd" {
            cwd = match (cwd, words) {
                (Some(cwd), [_, dir])
                    if dir
                        .literal()
                        .is_some_and(|d| d != "-" && !d.starts_with('~')) =>
                {
                    Some(cwd.join(&dir.text))
                }
                _ => None,
            };
            continue;
        }
        let Some(mut formatting) = formatter(words) else {
            continue;
        };
        if let Reach::Paths(paths) = &mut formatting.reach {
            match &cwd {
                Some(cwd) => {
                    for path in paths.iter_mut() {
                        *path = cwd.join(&*path);
                    }
                }
                None => formatting.reach = Reach::Workspace,
            }
        }
        found.push(formatting);
    }
    found
}

/// Past a package runner (`npx prettier`, `pnpm exec prettier`) to the
/// command it runs.
fn runner_words(words: &[Word]) -> &[Word] {
    fn skip_options(mut words: &[Word]) -> &[Word] {
        while words.first().is_some_and(|word| word.text.starts_with('-')) {
            words = &words[1..];
        }
        words
    }
    match words.first().map(|word| word.text.as_str()) {
        Some("npx" | "bunx" | "pnpx") => skip_options(&words[1..]),
        Some("pnpm" | "yarn" | "npm" | "bun")
            if words
                .get(1)
                .is_some_and(|word| matches!(word.text.as_str(), "exec" | "dlx" | "x")) =>
        {
            skip_options(&words[2..])
        }
        Some("yarn") if words.get(1).is_some_and(|word| is(word, "prettier")) => &words[1..],
        _ => words,
    }
}

/// Whether `word` names the program `name`: by itself, by a path ending in
/// it, or with a version (`prettier@3`).
fn is(word: &Word, name: &str) -> bool {
    let text = word.text.rsplit('/').next().unwrap_or_default();
    text == name
        || text
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('@'))
}

fn formatter(words: &[Word]) -> Option<Formatting> {
    let (name, args) = words.split_first()?;
    if is(name, "cargo") {
        let args: Vec<&Word> = args
            .iter()
            .skip_while(|word| word.text.starts_with('+'))
            .collect();
        let (subcommand, rest) = args.split_first()?;
        let checks = rest.iter().any(|word| word.text == "--check");
        return (subcommand.text == "fmt" && !checks).then_some(rust_workspace("cargo fmt"));
    }
    if is(name, "rustfmt") {
        return rustfmt(args);
    }
    if is(name, "prettier") {
        return prettier(args);
    }
    None
}

/// The operands of a formatter's arguments, past its options; `takes_value`
/// says which options are followed by a value. `Err` when one is an
/// expansion, whose value is not known.
fn operands<'a>(
    args: &'a [Word],
    takes_value: &[&str],
) -> Result<(Vec<&'a str>, Vec<&'a str>), ()> {
    let mut options = Vec::new();
    let mut files = Vec::new();
    let mut args = args.iter();
    let mut ended = false;
    while let Some(word) = args.next() {
        let text = word.text.as_str();
        if !ended && text == "--" {
            ended = true;
        } else if !ended && text.starts_with('-') && text.len() > 1 {
            options.push(text);
            if takes_value.contains(&text) {
                args.next();
            }
        } else {
            // A quoted glob is the formatter's to expand, so it is kept; an
            // expansion's value is not known.
            files.push(word.literal().ok_or(())?);
        }
    }
    Ok((options, files))
}

fn rustfmt(args: &[Word]) -> Option<Formatting> {
    let takes_value = [
        "--edition",
        "--style-edition",
        "--config",
        "--config-path",
        "--color",
        "--emit",
        "--file-lines",
    ];
    let Ok((options, files)) = operands(args, &takes_value) else {
        return Some(rust_workspace("rustfmt"));
    };
    let emit = args
        .windows(2)
        .find(|pair| pair[0].text == "--emit")
        .map(|pair| pair[1].text.as_str())
        .or_else(|| {
            options
                .iter()
                .find_map(|option| option.strip_prefix("--emit="))
        });
    // `--check` and an `--emit` other than `files` print; with no file it
    // formats standard input to standard output.
    let prints = options.contains(&"--check") || emit.is_some_and(|emit| emit != "files");
    if files.is_empty() || prints {
        return None;
    }
    let mut paths = Vec::new();
    for file in files {
        if file.contains(['*', '?', '[']) {
            return Some(rust_workspace("rustfmt"));
        }
        let path = PathBuf::from(file);
        // The modules a file declares are formatted with it: those of a
        // crate root or `mod.rs` sit beside it, others in a directory named
        // after the file.
        let children = match path.file_name().and_then(|name| name.to_str()) {
            Some("main.rs" | "lib.rs" | "mod.rs") => path.parent().map(PathBuf::from),
            Some(name) if name.ends_with(".rs") => Some(path.with_extension("")),
            _ => None,
        };
        paths.push(path);
        paths.extend(children);
    }
    Some(Formatting {
        tool: "rustfmt",
        extensions: RUST,
        reach: Reach::Paths(paths),
    })
}

fn rust_workspace(tool: &'static str) -> Formatting {
    Formatting {
        tool,
        extensions: RUST,
        reach: Reach::Workspace,
    }
}

fn prettier(args: &[Word]) -> Option<Formatting> {
    let takes_value = [
        "--config",
        "--ignore-path",
        "--plugin",
        "--parser",
        "--log-level",
        "--cache-location",
        "--cache-strategy",
        "--config-precedence",
        "--stdin-filepath",
        "--print-width",
        "--tab-width",
        "--trailing-comma",
        "--arrow-parens",
        "--end-of-line",
        "--prose-wrap",
        "--quote-props",
        "--html-whitespace-sensitivity",
        "--embedded-language-formatting",
        "--object-wrap",
        "--experimental-operator-position",
    ];
    let workspace = || Formatting {
        tool: "prettier",
        extensions: PRETTIER,
        reach: Reach::Workspace,
    };
    let Ok((options, files)) = operands(args, &takes_value) else {
        return Some(workspace());
    };
    if !options
        .iter()
        .any(|option| matches!(*option, "--write" | "-w"))
        || files.is_empty()
    {
        return None;
    }
    let mut paths = Vec::new();
    for file in files {
        // A glob reaches what is under its literal directory.
        match file.find(['*', '?', '[', '{']) {
            Some(at) => match file[..at].rfind('/') {
                Some(slash) => paths.push(PathBuf::from(&file[..slash])),
                None => return Some(workspace()),
            },
            None => paths.push(PathBuf::from(file)),
        }
    }
    Some(Formatting {
        reach: Reach::Paths(paths),
        ..workspace()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reach(command: &str) -> Vec<(&'static str, Reach)> {
        formatters(command)
            .into_iter()
            .map(|formatting| (formatting.tool, formatting.reach))
            .collect()
    }

    fn paths(paths: &[&str]) -> Reach {
        Reach::Paths(paths.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn cargo_fmt_reaches_the_whole_workspace_whatever_files_follow() {
        for command in [
            "cargo fmt",
            "cargo fmt --all",
            "cargo +nightly fmt --all",
            "cargo fmt -p langspan",
            "cargo fmt -- src/lib.rs",
            "cd crates/core && cargo fmt",
            "cargo build && cargo fmt --all 2>&1 | tail -5",
            "FOO=1 timeout 60 cargo fmt",
        ] {
            assert_eq!(
                reach(command),
                vec![("cargo fmt", Reach::Workspace)],
                "{command}"
            );
        }
    }

    #[test]
    fn a_check_or_a_print_rewrites_nothing() {
        for command in [
            "cargo fmt --check",
            "cargo fmt --all -- --check",
            "rustfmt --check src/lib.rs",
            "rustfmt --emit stdout src/lib.rs",
            "rustfmt",
            "prettier --check .",
            "prettier src/a.ts",
            "prettier --write",
            "cargo build",
            "git commit -m 'run cargo fmt'",
            "echo prettier --write .",
        ] {
            assert_eq!(reach(command), vec![], "{command}");
        }
    }

    #[test]
    fn rustfmt_reaches_its_files_and_the_modules_they_declare() {
        assert_eq!(
            reach("rustfmt --edition 2024 src/a.rs src/b/mod.rs"),
            vec![(
                "rustfmt",
                paths(&["src/a.rs", "src/a", "src/b/mod.rs", "src/b"])
            )]
        );
        assert_eq!(
            reach("rustfmt src/lib.rs"),
            vec![("rustfmt", paths(&["src/lib.rs", "src"]))]
        );
        // Relative to where a `cd` leaves the shell.
        assert_eq!(
            reach("cd crates && rustfmt x.rs"),
            vec![("rustfmt", paths(&["crates/x.rs", "crates/x"]))]
        );
        // A `cd` that cannot be followed reaches anything.
        assert_eq!(
            reach("cd $DIR && rustfmt x.rs"),
            vec![("rustfmt", Reach::Workspace)]
        );
        assert_eq!(reach("rustfmt $FILES"), vec![("rustfmt", Reach::Workspace)]);
    }

    #[test]
    fn prettier_reaches_its_files_directories_and_globs() {
        for command in [
            "prettier --write .",
            "npx prettier --write .",
            "npx --yes prettier@3 -w .",
            "pnpm exec prettier --write .",
            "yarn prettier --write .",
            "node_modules/.bin/prettier --write .",
        ] {
            assert_eq!(
                reach(command),
                vec![("prettier", paths(&["."]))],
                "{command}"
            );
        }
        assert_eq!(
            reach("prettier --config .prettierrc --write web/a.ts 'web/b/**/*.ts'"),
            vec![("prettier", paths(&["web/a.ts", "web/b"]))]
        );
        assert_eq!(
            reach("prettier --write '**/*.md'"),
            vec![("prettier", Reach::Workspace)]
        );
    }
}
