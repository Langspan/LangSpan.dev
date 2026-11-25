//! Call sites and parameter counts, for the write-time break alert
//! (ADR 0029). Both are name-level facts: nothing here resolves which
//! definition a call reaches.
use langspan_graph::{Arity, Call, Language, arity, calls};

fn found(language: Language, source: &str) -> Vec<(String, u32, Option<u32>, bool)> {
    calls(language, source.as_bytes())
        .unwrap()
        .into_iter()
        .map(
            |Call {
                 name,
                 line,
                 args,
                 member,
                 ..
             }| (name, line, args, member),
        )
        .collect()
}

fn call(
    name: &str,
    line: u32,
    args: Option<u32>,
    member: bool,
) -> (String, u32, Option<u32>, bool) {
    (name.into(), line, args, member)
}

#[test]
fn rust_calls_count_their_arguments_and_say_whether_they_name_a_receiver() {
    let source = "fn main() {\n    let a = f(x);\n    store.put(a, /* note */ b);\n    api::upload(\"p\", 3);\n    g::<u8>();\n    println!(\"{}\", h(1, 2));\n}\n";
    assert_eq!(
        found(Language::Rust, source),
        [
            call("f", 2, Some(1), false),
            call("put", 3, Some(2), true),
            call("upload", 4, Some(2), false),
            call("g", 5, Some(0), false),
        ],
        "a macro's arguments are tokens, not parsed calls"
    );
}

#[test]
fn python_typescript_and_go_calls() {
    assert_eq!(
        found(
            Language::Python,
            "def main():\n    f(x)\n    api.upload(p, retries=3)\n    g(*args)\n    h(x for x in y)\n"
        ),
        [
            call("f", 2, Some(1), false),
            call("upload", 3, Some(2), true),
            call("g", 4, None, false),
            call("h", 5, Some(1), false),
        ]
    );
    assert_eq!(
        found(
            Language::TypeScript,
            "export function main() {\n  f(x);\n  api.upload(p, 3);\n  g(...rest);\n  k<number>(1);\n}\n"
        ),
        [
            call("f", 2, Some(1), false),
            call("upload", 3, Some(2), true),
            call("g", 4, None, false),
            call("k", 5, Some(1), false),
        ]
    );
    assert_eq!(
        found(
            Language::Go,
            "package main\nfunc main() {\n\tF(x)\n\tapi.Upload(p, 3)\n\tG(xs...)\n}\n"
        ),
        [
            call("F", 3, Some(1), false),
            call("Upload", 4, Some(2), true),
            call("G", 5, None, false),
        ]
    );
}

fn counts(language: Language, signature: &str) -> Option<(u32, Option<u32>, bool)> {
    arity(language, signature).map(|Arity { min, max, receiver }| (min, max, receiver))
}

#[test]
fn parameter_counts_of_signatures_as_the_index_stores_them() {
    use Language::*;
    for (language, signature, expected) in [
        (Rust, "pub fn f(a: u8)", Some((1, Some(1), false))),
        (
            Rust,
            "pub fn f(a: u8, b: u8) -> bool",
            Some((2, Some(2), false)),
        ),
        (
            Rust,
            "pub fn send(&self, store: &Store) -> bool",
            Some((1, Some(1), true)),
        ),
        (Rust, "pub fn new() -> Self", Some((0, Some(0), false))),
        (Python, "def f(a)", Some((1, Some(1), false))),
        (
            Python,
            "def upload(path, retries=3)",
            Some((1, Some(2), false)),
        ),
        (
            Python,
            "def send(self, store, *, timeout: float = 1.0) -> bool",
            Some((1, Some(2), true)),
        ),
        (
            Python,
            "async def f(a, *rest, **options)",
            Some((1, None, false)),
        ),
        (Python, "def f(a, /, b)", Some((2, Some(2), false))),
        (
            TypeScript,
            "export function f(a: number)",
            Some((1, Some(1), false)),
        ),
        (
            TypeScript,
            "export function f(a: number, b?: string, c = 2): void",
            Some((1, Some(3), false)),
        ),
        (
            TypeScript,
            "export function f(...rest: number[])",
            Some((0, None, false)),
        ),
        (
            TypeScript,
            "export const f = (a: number, b: number)",
            Some((2, Some(2), false)),
        ),
        (
            TypeScript,
            "send(store: Store): boolean",
            Some((1, Some(1), false)),
        ),
        (Go, "func F(a int)", Some((1, Some(1), false))),
        (Go, "func F(a, b int) error", Some((2, Some(2), false))),
        (
            Go,
            "func (s *Store) Put(key string, values ...string)",
            Some((1, None, false)),
        ),
        (Go, "func F(int, string)", Some((2, Some(2), false))),
        // Two definitions of one name, or a signature cut at its bound,
        // have no single count.
        (Rust, "pub fn f(a: u8) | pub fn f(a: u8, b: u8)", None),
        (Rust, "pub fn f(a: u8, b: u…", None),
        // Not a function.
        (Rust, "pub struct Upload", None),
    ] {
        assert_eq!(counts(language, signature), expected, "{signature}");
    }
}
