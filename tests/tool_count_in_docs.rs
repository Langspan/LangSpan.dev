//! No document states how many MCP tools there are. The set of tools is
//! written once, as `langspan::mcp::TOOL_NAMES`; a count written in prose goes
//! stale with the next tool and makes every branch that adds one conflict.
//! The tool table in `docs/usage.md` is held to that list by name.

use std::fs;
use std::path::{Path, PathBuf};

/// Written in batches by the maintainers, not by a pull request that adds a
/// tool; their stated counts are theirs to remove.
const MAINTAINER_FILES: &[&str] = &["docs/IMPLEMENTATION_STATUS.md", "docs/HANDOFF.md"];

const NUMBER_WORDS: &[&str] = &[
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
    "twentieth",
    "thirtieth",
];

fn is_digits(word: &str) -> bool {
    let digits = word.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

fn is_number_word(word: &str) -> bool {
    word.split('-')
        .all(|part| NUMBER_WORDS.contains(&part) || is_digits(part))
}

fn is_tool(word: &str) -> bool {
    word == "tool" || word == "tools"
}

/// Phrases that state a count of tools: a numeral before "tools" or "MCP
/// tools" ("24 tools", "(21 tools)"), or a number word or ordinal before "MCP
/// tool(s)" ("Twenty-three MCP tools", "the ninth MCP tool").
fn stated_counts(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
                .to_lowercase()
        })
        .collect();
    let mut found = Vec::new();
    for (index, word) in words.iter().enumerate() {
        let next = words.get(index + 1).map(String::as_str);
        let after = words.get(index + 2).map(String::as_str);
        let states = if is_digits(word) {
            next.is_some_and(is_tool) || (next == Some("mcp") && after.is_some_and(is_tool))
        } else {
            is_number_word(word) && next == Some("mcp") && after.is_some_and(is_tool)
        };
        if states {
            found.push(words[index..(index + 3).min(words.len())].join(" "));
        }
    }
    found
}

fn markdown_files(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                markdown_files(&path, found);
            }
        } else if name.ends_with(".md") {
            found.push(path);
        }
    }
}

#[test]
fn the_count_finder_catches_the_forms_documents_have_used() {
    for stated in [
        "The MCP server has 24 tools (`src/mcp.rs`,",
        "| CLI + MCP | Twenty-three MCP tools, with CLI access",
        "A new MCP tool, `langspan_commit_mine` (21 tools); later",
        "and the ninth MCP\ntool, `langspan_changes`, return",
        "All 24 tools are always listed.",
    ] {
        assert!(!stated_counts(stated).is_empty(), "{stated:?}");
    }
    for unstated in [
        "Both tools need a timeout.",
        "Session was asked, with tools forbidden, whether",
        "The MCP briefing tool counts as delivery",
        "a new MCP tool, `langspan_land_cancel`",
        "exit code 2 tells the agent",
    ] {
        assert_eq!(
            stated_counts(unstated),
            Vec::<String>::new(),
            "{unstated:?}"
        );
    }
}

#[test]
fn the_usage_tool_table_names_exactly_the_listed_tools() {
    let usage =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/usage.md")).unwrap();
    let section = usage
        .split("\n## MCP\n")
        .nth(1)
        .expect("docs/usage.md has an MCP section");
    let section = section.split("\n## ").next().unwrap();
    let rows: Vec<&str> = section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|line| line.split('`').next())
        .collect();
    if let Err(problem) = langspan::mcp::compare_tool_names(rows) {
        panic!("the tool table under \"## MCP\" in docs/usage.md: {problem}");
    }
}

#[test]
fn no_document_states_how_many_tools_there_are() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    markdown_files(root, &mut files);
    files.sort();
    assert!(files.iter().any(|path| path.ends_with("README.md")));
    let mut problems = Vec::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap().to_string_lossy();
        if MAINTAINER_FILES.contains(&relative.as_ref()) {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for stated in stated_counts(&text) {
            problems.push(format!("{relative}: \"{stated}\""));
        }
    }
    assert!(
        problems.is_empty(),
        "These state a number of MCP tools, which goes stale when a tool is \
         added. Name the tools or point at TOOL_NAMES in src/mcp.rs instead:\n{}",
        problems.join("\n")
    );
}
