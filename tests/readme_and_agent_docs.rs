//! Holds the README and the always-loaded agent file to the budgets in
//! `docs/writing-style.md` ("The README" and "Docs for agents"). A README
//! that grows past its budget, or pushes the quickstart down, loses the
//! person deciding whether to try LangSpan; every line of `AGENTS.md` costs
//! context on every agent turn.

use std::fs;
use std::path::Path;

/// The README's upper bound in words of rendered text.
const README_MAX_WORDS: usize = 1_500;
/// `langspan setup` appears within this many words of the README's start.
const QUICKSTART_WITHIN_WORDS: usize = 200;
/// The always-loaded agent file's upper bound in words.
const AGENTS_MAX_WORDS: usize = 600;

fn read(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)).unwrap()
}

/// The words a reader sees: HTML tags, link targets and badge lines removed.
fn rendered_words(markdown: &str) -> Vec<String> {
    let mut text = String::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("[![") {
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                for skipped in chars.by_ref() {
                    if skipped == '>' {
                        break;
                    }
                }
                out.push(' ');
            }
            ']' if chars.peek() == Some(&'(') => {
                for skipped in chars.by_ref() {
                    if skipped == ')' {
                        break;
                    }
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect()
}

#[test]
fn rendered_words_skip_markup_and_count_text() {
    let words = rendered_words(
        "[![badge](https://x)](https://y)\n<img alt=\"a b\" src=\"c\">\n# Two words\n\
         See [the guide](docs/usage.md) now. `langspan setup` ---\n",
    );
    assert_eq!(
        words,
        [
            "Two",
            "words",
            "See",
            "[the",
            "guide",
            "now.",
            "`langspan",
            "setup`"
        ]
    );
}

#[test]
fn the_readme_stays_within_its_word_budget() {
    let words = rendered_words(&read("README.md"));
    assert!(
        words.len() <= README_MAX_WORDS,
        "README.md has {} words; the budget is {README_MAX_WORDS}. Move detail \
         to a linked doc (docs/writing-style.md, \"The README\").",
        words.len()
    );
}

#[test]
fn the_readme_reaches_setup_within_its_first_words() {
    let words = rendered_words(&read("README.md"));
    let position = words
        .windows(2)
        .position(|pair| pair[0].ends_with("langspan") && pair[1].starts_with("setup"));
    assert!(
        position.is_some_and(|at| at <= QUICKSTART_WITHIN_WORDS),
        "`langspan setup` is at word {position:?} of README.md; the quickstart \
         belongs within the first {QUICKSTART_WITHIN_WORDS} words."
    );
}

/// The first fenced `sh` block after `heading` in `markdown`, without fences.
fn sh_block_after(markdown: &str, heading: &str) -> String {
    let rest = &markdown[markdown
        .find(heading)
        .unwrap_or_else(|| panic!("no {heading:?} heading"))..];
    let start = rest.find("```sh\n").expect("no sh block after the heading") + "```sh\n".len();
    let len = rest[start..].find("```").expect("unclosed sh block");
    rest[start..start + len].to_owned()
}

/// The release targets named in the first table after `heading`.
fn target_rows(markdown: &str, heading: &str) -> Vec<String> {
    let rest = &markdown[markdown.find(heading).unwrap()..];
    rest.lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .filter_map(|line| {
            let cell = line.split('|').nth(2)?.trim();
            let target = cell.strip_prefix('`')?.split('`').next()?;
            target.contains("-").then(|| target.to_owned())
        })
        .collect()
}

const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
];

#[test]
fn the_quickstart_names_one_target_per_platform() {
    for (doc, heading) in [
        ("README.md", "## Quickstart"),
        ("docs/install.md", "# Install"),
    ] {
        assert_eq!(
            target_rows(&read(doc), heading),
            TARGETS,
            "{doc} needs a four-row table of release targets under {heading:?}"
        );
    }
}

#[test]
fn the_quickstart_runs_top_to_bottom_without_gh() {
    let block = sh_block_after(&read("README.md"), "## Quickstart");
    // Each step a stranger needs, in order: download, verify, extract,
    // install, keep PATH for later shells, set up, check.
    let steps = [
        "target=",
        "curl -fsSLO \"$url/SHA256SUMS\"",
        "curl -fsSLO \"$url/$archive\"",
        "shasum -a 256 -c",
        "tar -xzf \"$archive\"",
        "cd \"${archive%.tar.gz}\"",
        "install -m 0755 langspan \"$HOME/.local/bin/langspan\"",
        ">> \"$HOME/.",
        "langspan setup",
        "langspan doctor",
    ];
    let mut from = 0;
    for step in steps {
        let at = block[from..]
            .find(step)
            .unwrap_or_else(|| panic!("README quickstart lacks {step:?} after byte {from}"));
        from += at + step.len();
    }
    assert!(
        !block.contains("gh "),
        "the quickstart must not need the GitHub CLI"
    );
    let parsed = std::process::Command::new("sh")
        .args(["-n", "-c", &block])
        .status()
        .unwrap();
    assert!(parsed.success(), "the README quickstart is not valid sh");
}

#[test]
fn the_install_guide_repeats_the_readme_quickstart() {
    assert_eq!(
        sh_block_after(&read("docs/install.md"), "## Install"),
        sh_block_after(&read("README.md"), "## Quickstart"),
        "docs/install.md and the README must give the same install commands"
    );
}

#[test]
fn the_readme_states_each_host_status_once() {
    let readme = read("README.md");
    assert!(!readme.contains("pending"), "README.md calls a run pending");
    let antigravity_status = readme
        .lines()
        .filter(|line| line.contains("Antigravity") && line.to_lowercase().contains("verified"))
        .count();
    assert_eq!(
        antigravity_status, 1,
        "state Antigravity's status in one README line"
    );
}

#[test]
fn the_agent_file_stays_within_its_word_budget() {
    let words = rendered_words(&read("AGENTS.md"));
    assert!(
        words.len() <= AGENTS_MAX_WORDS,
        "AGENTS.md has {} words; the budget is {AGENTS_MAX_WORDS}. Keep only \
         what an agent can't infer from the code, and link the rest \
         (docs/writing-style.md, \"Docs for agents\").",
        words.len()
    );
}
