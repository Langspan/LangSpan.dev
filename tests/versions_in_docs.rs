//! The threat model, `docs/architecture.md` and `docs/usage.md` state the
//! wire protocol version, the oldest version still answered and the SQLite
//! schema marker. Each statement is held to the constant it repeats:
//! `PROTOCOL_VERSION` and `OLDEST_PROTOCOL_VERSION` in `src/protocol.rs`, and
//! `SCHEMA_VERSION` in `src/store.rs`. Changing a constant without these
//! documents fails here. A document split into pages (`docs/usage.md` and the
//! pages under `docs/usage/`) is read as one.

use std::fs;
use std::path::Path;

const DOCUMENTS: &[&str] = &[
    "docs/security/threat-model.md",
    "docs/architecture.md",
    "docs/usage.md",
];

#[derive(Clone, Copy)]
struct Versions {
    protocol: u16,
    oldest: u16,
    schema: u16,
}

/// `SCHEMA_VERSION` is private to the store, so it is read from its source
/// line, `const SCHEMA_VERSION: i64 = N;`.
fn schema_version() -> u16 {
    let store = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store.rs"))
        .expect("read src/store.rs");
    let line = store
        .lines()
        .find_map(|line| line.trim().strip_prefix("const SCHEMA_VERSION: i64 = "))
        .expect("src/store.rs declares `const SCHEMA_VERSION: i64 = N;`");
    line.trim_end_matches(';')
        .parse()
        .expect("SCHEMA_VERSION is a number")
}

fn code_versions() -> Versions {
    Versions {
        protocol: langspan::protocol::PROTOCOL_VERSION,
        oldest: langspan::protocol::OLDEST_PROTOCOL_VERSION,
        schema: schema_version(),
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect()
}

fn number(word: Option<&String>) -> Option<u16> {
    word.and_then(|word| word.parse().ok())
}

/// What a document says, in the forms the three documents use:
/// "Protocol 10" or "wire protocol is version 10"; "schema marker version
/// 4"; and the answered range, as "versions 3 to 10" or as the list
/// "version 9, 8, 7, 6, 5, 4 and 3 clients".
#[derive(Default)]
struct Stated {
    protocol: Vec<u16>,
    schema: Vec<u16>,
    ranges: Vec<(u16, u16)>,
    lists: Vec<Vec<u16>>,
}

fn stated(text: &str) -> Stated {
    let words = words(text);
    let at = |index: usize| words.get(index);
    let is = |index: usize, word: &str| at(index).is_some_and(|found| found == word);
    let mut found = Stated::default();
    for index in 0..words.len() {
        if is(index, "protocol") {
            if let Some(version) = number(at(index + 1)) {
                found.protocol.push(version);
            } else if is(index + 1, "is") && is(index + 2, "version") {
                found.protocol.extend(number(at(index + 3)));
            }
        }
        if is(index, "schema") && is(index + 1, "marker") && is(index + 2, "version") {
            found.schema.extend(number(at(index + 3)));
        }
        if is(index, "versions")
            && is(index + 2, "to")
            && let (Some(from), Some(to)) = (number(at(index + 1)), number(at(index + 3)))
        {
            found.ranges.push((from, to));
        }
        if is(index, "version") {
            let mut list = Vec::new();
            let mut next = index + 1;
            while let Some(version) = number(at(next)) {
                list.push(version);
                next += 1;
                if is(next, "and") {
                    next += 1;
                }
            }
            if list.len() > 1 && is(next, "clients") {
                found.lists.push(list);
            }
        }
    }
    found
}

fn problems(name: &str, text: &str, code: Versions) -> Vec<String> {
    let found = stated(text);
    let mut problems = Vec::new();
    if found.protocol.is_empty() {
        problems.push(format!("{name} states no protocol version"));
    }
    for version in found.protocol {
        if version != code.protocol {
            problems.push(format!(
                "{name} says protocol {version}; PROTOCOL_VERSION is {}",
                code.protocol
            ));
        }
    }
    if found.schema.is_empty() {
        problems.push(format!("{name} states no schema marker version"));
    }
    for version in found.schema {
        if version != code.schema {
            problems.push(format!(
                "{name} says schema marker version {version}; SCHEMA_VERSION is {}",
                code.schema
            ));
        }
    }
    if found.ranges.is_empty() && found.lists.is_empty() {
        problems.push(format!("{name} states no oldest answered version"));
    }
    for (from, to) in found.ranges {
        if (from, to) != (code.oldest, code.protocol) {
            problems.push(format!(
                "{name} says versions {from} to {to}; the code answers {} to {}",
                code.oldest, code.protocol
            ));
        }
    }
    let answered: Vec<u16> = (code.oldest..code.protocol).rev().collect();
    for list in found.lists {
        if list != answered {
            problems.push(format!(
                "{name} lists versions {list:?} as answered; the code answers {answered:?} \
                 below {}",
                code.protocol
            ));
        }
    }
    problems
}

/// The document's text: the file, then each page in the directory of the
/// same name (`docs/usage/` for `docs/usage.md`), in file-name order.
fn document(root: &Path, name: &str) -> String {
    let mut text = fs::read_to_string(root.join(name)).expect("read the document");
    let Ok(entries) = fs::read_dir(root.join(name.trim_end_matches(".md"))) else {
        return text;
    };
    let mut pages: Vec<_> = entries
        .map(|entry| entry.expect("list the document's pages").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    pages.sort();
    for page in pages {
        text.push('\n');
        text.push_str(&fs::read_to_string(&page).expect("read a page of the document"));
    }
    text
}

fn check(code: Versions) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    DOCUMENTS
        .iter()
        .flat_map(|name| problems(name, &document(root, name), code))
        .collect()
}

#[test]
fn the_finder_reads_the_forms_the_documents_use() {
    let text = "Protocol 10 (versions 3 to 10 answered). The current wire protocol \
        is version 10 and the SQLite schema\nmarker version 4. (version 9, 8, 7, 6, 5, \
        4 and 3 clients are still answered). A client of protocol version 7 or \
        earlier is told; version 3 clients get less.";
    let found = stated(text);
    assert_eq!(found.protocol, [10, 10]);
    assert_eq!(found.schema, [4]);
    assert_eq!(found.ranges, [(3, 10)]);
    assert_eq!(found.lists, [vec![9, 8, 7, 6, 5, 4, 3]]);
}

#[test]
fn a_split_document_includes_its_pages() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = document(root, "docs/usage.md");
    let mcp = fs::read_to_string(root.join("docs/usage/mcp.md")).unwrap();
    assert!(text.contains(&mcp), "docs/usage/mcp.md is not read");
}

#[test]
fn the_documents_state_the_versions_in_the_code() {
    let problems = check(code_versions());
    assert!(
        problems.is_empty(),
        "Update these to match the constants in src/protocol.rs and src/store.rs:\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_changed_constant_fails_for_every_document() {
    let code = code_versions();
    for changed in [
        Versions {
            protocol: code.protocol + 1,
            ..code
        },
        Versions {
            oldest: code.oldest + 1,
            ..code
        },
        Versions {
            schema: code.schema + 1,
            ..code
        },
    ] {
        let problems = check(changed);
        for name in DOCUMENTS {
            assert!(
                problems.iter().any(|problem| problem.starts_with(name)),
                "{name} would not fail: {problems:?}"
            );
        }
    }
}
