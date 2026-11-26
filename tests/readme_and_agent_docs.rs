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
