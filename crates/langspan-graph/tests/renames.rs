//! A rename in one file transition (roadmap B10): an exported function or
//! method that goes, and one of the same kind, parameter list and body that
//! comes under a new name, is one rename, not a removal and an addition.
use langspan_graph::{Change, FileGraph, Language, SymbolChange, diff, digest_blocks, extract};

/// The caller supplies the digest; the crate never hashes. The text itself
/// stands in for one here.
fn version(language: Language, source: &str) -> FileGraph {
    let mut graph = extract(language, source.as_bytes()).unwrap();
    digest_blocks(&mut graph, source.as_bytes(), |bytes| {
        String::from_utf8_lossy(bytes).into_owned()
    });
    graph
}

fn changes(language: Language, before: &str, after: &str) -> Vec<SymbolChange> {
    diff(&version(language, before), &version(language, after))
}

fn renamed(to: &str, before: &str, after: &str) -> Change {
    Change::Renamed {
        to: to.into(),
        before: before.into(),
        after: after.into(),
    }
}

#[test]
fn a_rename_with_an_unchanged_body_is_one_rename_in_each_language() {
    let cases = [
        (
            Language::Rust,
            "pub fn fetch_user(id: u32) -> u32 {\n    id + 1\n}\n",
            "pub fn load_user(id: u32) -> u32 {\n    id + 1\n}\n",
            ("fetch_user", "pub fn fetch_user(id: u32) -> u32"),
            ("load_user", "pub fn load_user(id: u32) -> u32"),
        ),
        (
            Language::Python,
            "def fetch_user(id):\n    return id + 1\n",
            "def load_user(id):\n    return id + 1\n",
            ("fetch_user", "def fetch_user(id)"),
            ("load_user", "def load_user(id)"),
        ),
        (
            Language::TypeScript,
            "export function fetch_user(id: number): number {\n  return id + 1;\n}\n",
            "export function load_user(id: number): number {\n  return id + 1;\n}\n",
            ("fetch_user", "function fetch_user(id: number): number"),
            ("load_user", "function load_user(id: number): number"),
        ),
        (
            Language::Go,
            "package app\n\nfunc FetchUser(id int) int {\n\treturn id + 1\n}\n",
            "package app\n\nfunc LoadUser(id int) int {\n\treturn id + 1\n}\n",
            ("FetchUser", "func FetchUser(id int) int"),
            ("LoadUser", "func LoadUser(id int) int"),
        ),
    ];
    for (language, before, after, (old, old_signature), (new, new_signature)) in cases {
        let found = changes(language, before, after);
        assert_eq!(found.len(), 1, "{language:?}: {found:?}");
        assert_eq!(found[0].name, old, "{language:?}");
        assert_eq!(
            found[0].change,
            renamed(new, old_signature, new_signature),
            "{language:?}"
        );
    }
}

#[test]
fn a_renamed_method_and_arrow_function_are_renames_too() {
    let found = changes(
        Language::Rust,
        "pub struct S;\nimpl S {\n    pub fn get(&self) -> u8 { 1 }\n}\n",
        "pub struct S;\nimpl S {\n    pub fn fetch(&self) -> u8 { 1 }\n}\n",
    );
    assert_eq!(
        found.iter().map(|c| c.change.clone()).collect::<Vec<_>>(),
        [renamed(
            "fetch",
            "pub fn get(&self) -> u8",
            "pub fn fetch(&self) -> u8"
        )]
    );
    let found = changes(
        Language::TypeScript,
        "export const fetchUser = (id: number) => id + 1;\n",
        "export const loadUser = (id: number) => id + 1;\n",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        matches!(&found[0].change, Change::Renamed { to, .. } if to == "loadUser"),
        "{found:?}"
    );
}

#[test]
fn a_rename_with_a_changed_body_or_parameters_stays_a_removal_and_an_addition() {
    let kinds = |found: Vec<SymbolChange>| {
        let mut kinds: Vec<(String, Change)> =
            found.into_iter().map(|c| (c.name, c.change)).collect();
        kinds.sort_by(|a, b| a.0.cmp(&b.0));
        kinds
    };
    let split = [
        ("fetch_user".to_owned(), Change::Removed),
        ("load_user".to_owned(), Change::Added),
    ];
    // The body changed as well as the name.
    assert_eq!(
        kinds(changes(
            Language::Rust,
            "pub fn fetch_user(id: u32) -> u32 {\n    id + 1\n}\n",
            "pub fn load_user(id: u32) -> u32 {\n    id + 2\n}\n",
        )),
        split
    );
    assert_eq!(
        kinds(changes(
            Language::Python,
            "def fetch_user(id):\n    return id + 1\n",
            "def load_user(id):\n    return id + 2\n",
        )),
        split
    );
    // A parameter changed as well as the name.
    assert_eq!(
        kinds(changes(
            Language::TypeScript,
            "export function fetch_user(id: number): number {\n  return 1;\n}\n",
            "export function load_user(id: string): number {\n  return 1;\n}\n",
        )),
        split
    );
    // The return type changed as well as the name.
    assert_eq!(
        kinds(changes(
            Language::Go,
            "package app\n\nfunc fetch_user(id int) int {\n\treturn 1\n}\n",
            "package app\n\nfunc load_user(id int) int64 {\n\treturn 1\n}\n",
        ))
        .len(),
        0,
        "Go lower-case names are not exported"
    );
    let go = kinds(changes(
        Language::Go,
        "package app\n\nfunc FetchUser(id int) int {\n\treturn 1\n}\n",
        "package app\n\nfunc LoadUser(id int) int64 {\n\treturn 1\n}\n",
    ));
    assert_eq!(
        go,
        [
            ("FetchUser".to_owned(), Change::Removed),
            ("LoadUser".to_owned(), Change::Added)
        ]
    );
}

#[test]
fn an_ambiguous_or_undigested_pair_is_never_linked() {
    // Two removed functions with the same body and parameters: which one
    // became the new name cannot be told.
    let found = changes(
        Language::Rust,
        "pub fn a(x: u8) -> u8 { x }\npub fn b(x: u8) -> u8 { x }\n",
        "pub fn c(x: u8) -> u8 { x }\n",
    );
    assert!(
        found
            .iter()
            .all(|c| !matches!(c.change, Change::Renamed { .. })),
        "{found:?}"
    );
    // Without block digests, nothing is linked.
    let plain = |source: &str| extract(Language::Rust, source.as_bytes()).unwrap();
    let found = diff(
        &plain("pub fn fetch_user(id: u32) -> u32 { id }\n"),
        &plain("pub fn load_user(id: u32) -> u32 { id }\n"),
    );
    assert_eq!(found.len(), 2, "{found:?}");
    // A type is never a rename: only functions and methods are.
    let found = changes(
        Language::Rust,
        "pub struct A { x: u8 }\n",
        "pub struct B { x: u8 }\n",
    );
    assert_eq!(found.len(), 2, "{found:?}");
    // A trait method without a body has nothing to compare.
    let found = changes(
        Language::Rust,
        "pub trait T {\n    fn a(&self);\n}\n",
        "pub trait T {\n    fn b(&self);\n}\n",
    );
    assert!(
        found
            .iter()
            .all(|c| !matches!(c.change, Change::Renamed { .. })),
        "{found:?}"
    );
}
