//! What is extracted from each supported language, and what a change to an
//! exported symbol looks like.
use langspan_graph::{Change, FileGraph, Language, SymbolKind, diff, extract};

fn names(graph: &FileGraph) -> Vec<(&str, SymbolKind, bool)> {
    graph
        .symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.kind, symbol.exported))
        .collect()
}

fn refers(graph: &FileGraph, name: &str) -> bool {
    graph
        .references
        .iter()
        .any(|reference| reference.name == name)
}

#[test]
fn rust_definitions_references_and_imports() {
    let source = r#"
use crate::store::Store;
pub struct Upload { pub path: String }
pub trait Gate { fn allow(&self, user: &str) -> bool; }
impl Upload {
    pub fn send(&self, store: &Store) -> bool { store.put(&self.path) }
    fn private(&self) {}
}
pub fn upload(path: &str, retries: u32) -> bool { helper(path) }
fn helper(_: &str) -> bool { true }
pub const LIMIT: usize = 255;
"#;
    let graph = extract(Language::Rust, source.as_bytes()).unwrap();
    assert!(!graph.partial);
    assert_eq!(
        names(&graph),
        [
            ("Upload", SymbolKind::Type, true),
            ("Gate", SymbolKind::Interface, true),
            ("allow", SymbolKind::Method, false),
            ("send", SymbolKind::Method, true),
            ("private", SymbolKind::Method, false),
            ("upload", SymbolKind::Function, true),
            ("helper", SymbolKind::Function, false),
            ("LIMIT", SymbolKind::Constant, true),
        ]
    );
    let upload = graph.symbols.iter().find(|s| s.name == "upload").unwrap();
    assert_eq!(
        upload.signature,
        "pub fn upload(path: &str, retries: u32) -> bool"
    );
    assert_eq!((upload.line, upload.end_line), (9, 9));
    assert!(refers(&graph, "helper") && refers(&graph, "Store") && refers(&graph, "put"));
    assert_eq!(graph.imports[0].name, "crate::store::Store");
    // A definition's own name is not a reference to it.
    assert_eq!(
        graph
            .references
            .iter()
            .filter(|r| r.name == "upload")
            .count(),
        0
    );
}

#[test]
fn python_typescript_and_go_follow_their_own_export_rules() {
    let python = extract(
        Language::Python,
        b"from app.store import Store\nclass Upload:\n    def send(self, store):\n        return store.put(self)\ndef _hidden():\n    pass\ndef upload(path, retries=3):\n    return _hidden()\n",
    )
    .unwrap();
    assert_eq!(
        names(&python),
        [
            ("Upload", SymbolKind::Type, true),
            ("send", SymbolKind::Method, true),
            ("_hidden", SymbolKind::Function, false),
            ("upload", SymbolKind::Function, true),
        ]
    );
    assert_eq!(python.symbols[3].signature, "def upload(path, retries=3)");
    assert_eq!(python.imports[0].name, "app.store");

    let typescript = extract(
        Language::TypeScript,
        b"import { Store } from './store';\nexport interface Gate { allow(user: string): boolean }\nexport function upload(path: string, retries: number): boolean { return helper(path) }\nfunction helper(p: string) { return true }\nexport const send = async (store: Store) => store.put();\nexport class Uploader { run(): void {} }\n",
    )
    .unwrap();
    assert_eq!(
        names(&typescript),
        [
            ("Gate", SymbolKind::Interface, true),
            ("allow", SymbolKind::Method, true),
            ("upload", SymbolKind::Function, true),
            ("helper", SymbolKind::Function, false),
            ("send", SymbolKind::Function, true),
            ("Uploader", SymbolKind::Type, true),
            ("run", SymbolKind::Method, true),
        ]
    );
    assert_eq!(typescript.imports[0].name, "./store");
    assert!(refers(&typescript, "helper") && refers(&typescript, "Store"));

    let go = extract(
        Language::Go,
        b"package app\nimport \"fmt\"\ntype Upload struct{ Path string }\nfunc (u Upload) Send() bool { return helper(u.Path) }\nfunc helper(p string) bool { fmt.Println(p); return true }\nfunc Upload2(path string, retries int) bool { return true }\n",
    )
    .unwrap();
    assert_eq!(
        names(&go),
        [
            ("Upload", SymbolKind::Type, true),
            ("Send", SymbolKind::Method, true),
            ("helper", SymbolKind::Function, false),
            ("Upload2", SymbolKind::Function, true),
        ]
    );
    assert_eq!(go.imports[0].name, "fmt");
    assert!(Language::for_path("src/app.tsx") == Some(Language::Tsx));
    assert!(Language::for_path("README.md").is_none() && Language::for_path("Makefile").is_none());
}

#[test]
fn a_signature_change_a_removal_and_a_rename_are_told_apart_from_a_body_edit() {
    let version = |source: &str| extract(Language::Rust, source.as_bytes()).unwrap();
    let before = version(
        "pub fn upload(path: &str) -> bool { true }\npub fn gate(user: &str) -> bool { user == \"a\" }\npub fn old_name() {}\nfn private(a: u8) {}\n",
    );
    let body_only = version(
        "pub fn upload(path: &str) -> bool { false }\npub fn gate(user: &str) -> bool { user == \"b\" }\npub fn old_name() {}\nfn private(a: u8) {}\n",
    );
    assert!(
        diff(&before, &body_only).is_empty(),
        "bodies are not interfaces"
    );

    let after = version(
        "pub fn upload(path: &str, retries: u32) -> bool { true }\npub fn new_name() {}\nfn private(a: u16) {}\n",
    );
    let changes = diff(&before, &after);
    let find = |name: &str| changes.iter().find(|change| change.name == name).unwrap();
    assert_eq!(
        find("upload").change,
        Change::SignatureChanged {
            before: "pub fn upload(path: &str) -> bool".into(),
            after: "pub fn upload(path: &str, retries: u32) -> bool".into(),
        }
    );
    assert_eq!(find("gate").change, Change::Removed);
    // A rename is a removal and an addition; no link between them is claimed.
    assert_eq!(find("old_name").change, Change::Removed);
    assert_eq!(find("new_name").change, Change::Added);
    assert!(
        changes.iter().all(|change| change.name != "private"),
        "a private symbol is not an interface"
    );
}

#[test]
fn broken_oversized_and_hostile_input_is_bounded_and_never_panics() {
    let broken = extract(Language::Rust, b"pub fn ok() {}\npub fn broken( {{{ \n").unwrap();
    assert!(broken.partial, "a syntax error is reported");
    assert!(broken.symbols.iter().any(|symbol| symbol.name == "ok"));
    assert!(
        extract(
            Language::Python,
            &vec![b'x'; langspan_graph::MAX_SOURCE_BYTES + 1]
        )
        .is_err()
    );
    // Deep nesting must not overflow the stack; many names must stay bounded.
    let deep = format!("fn f() {{ {} {} }}", "(".repeat(20_000), ")".repeat(20_000));
    extract(Language::Rust, deep.as_bytes()).unwrap();
    let many: String = (0..30_000).map(|n| format!("a{n};")).collect();
    let graph = extract(Language::TypeScript, many.as_bytes()).unwrap();
    assert!(graph.partial && graph.references.len() <= langspan_graph::MAX_REFERENCES + 16);
    for language in [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::Tsx,
        Language::Go,
    ] {
        extract(language, &[0xff, 0xfe, 0x00, b'{', 0x80]).unwrap();
        extract(language, b"").unwrap();
    }
    let long = format!("pub fn f({}) {{}}", "a: u8, ".repeat(200));
    let graph = extract(Language::Rust, long.as_bytes()).unwrap();
    assert!(graph.symbols[0].signature.len() <= 244);
}

#[test]
fn a_formatter_rewrap_of_a_signature_is_not_a_signature_change_and_a_parameter_change_is() {
    let changed = |language: Language, before: &str, after: &str| {
        let before = extract(language, before.as_bytes()).unwrap();
        let after = extract(language, after.as_bytes()).unwrap();
        diff(&before, &after)
    };
    // rustfmt: one line, then wrapped with a trailing comma, then a where
    // clause rewrapped with its own trailing comma.
    let rust =
        "pub fn upload<T: AsRef<str>>(path: T, retries: u32) -> bool where T: Clone { true }\n";
    let rustfmt = "pub fn upload<T: AsRef<str>>(\n    path: T,\n    retries: u32,\n) -> bool\nwhere\n    T: Clone,\n{\n    true\n}\n";
    assert_eq!(changed(Language::Rust, rust, rustfmt), [], "rustfmt");
    // black: wrapped arguments with a magic trailing comma.
    let python = "def upload(path: str, retries: int = 3) -> bool:\n    return True\n";
    let black = "def upload(\n    path: str,\n    retries: int = 3,\n) -> bool:\n    return True\n";
    assert_eq!(changed(Language::Python, python, black), [], "black");
    // prettier: wrapped parameters, trailing comma, spacing inside braces.
    let typescript =
        "export function upload(path: string, opts: {retries: number}): boolean { return true; }\n";
    let prettier = "export function upload(\n  path: string,\n  opts: { retries: number },\n): boolean {\n  return true;\n}\n";
    assert_eq!(
        changed(Language::TypeScript, typescript, prettier),
        [],
        "prettier"
    );
    // gofmt keeps a trailing comma when a parameter list is wrapped.
    let go = "package p\n\nfunc Upload(path string, retries int) bool { return true }\n";
    let gofmt =
        "package p\n\nfunc Upload(\n\tpath string,\n\tretries int,\n) bool {\n\treturn true\n}\n";
    assert_eq!(changed(Language::Go, go, gofmt), [], "gofmt");

    // A real parameter change through the same formatter still counts, and
    // the notice shows the signatures as written.
    let python_changed = "def upload(\n    path: str,\n    retries: int = 3,\n    timeout: float = 1.0,\n) -> bool:\n    return True\n";
    let changes = changed(Language::Python, python, python_changed);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(
        changes[0].change,
        Change::SignatureChanged {
            before: "def upload(path: str, retries: int = 3) -> bool".into(),
            after: "def upload( path: str, retries: int = 3, timeout: float = 1.0, ) -> bool"
                .into(),
        }
    );
    // Words stay apart: `u32` to `u 32` is not whitespace around punctuation.
    let renamed = "pub fn upload(path: &str, n: u32) -> bool { true }\n";
    let retyped = "pub fn upload(path: &str, n: u64) -> bool { true }\n";
    assert_eq!(changed(Language::Rust, renamed, retyped).len(), 1);
}

#[test]
fn a_one_element_tuple_losing_its_comma_is_a_signature_change_and_a_wrapped_one_is_not() {
    let changed = |language: Language, before: &str, after: &str| {
        let before = extract(language, before.as_bytes()).unwrap();
        let after = extract(language, after.as_bytes()).unwrap();
        diff(&before, &after)
    };
    // `(1,)` is a tuple and `(1)` is the integer 1: callers that omit the
    // argument get another type.
    let tuple = "def upload(values=(1,)):\n    return values\n";
    let scalar = "def upload(values=(1)):\n    return values\n";
    assert_eq!(changed(Language::Python, tuple, scalar).len(), 1, "python");
    // Rust's one-element tuple type, as a parameter and as a return type.
    let rust_tuple = "pub fn f(x: (u32,)) -> (u8,) { (0,) }\n";
    let rust_plain = "pub fn f(x: (u32)) -> (u8,) { (0,) }\n";
    let rust_return = "pub fn f(x: (u32,)) -> (u8) { 0 }\n";
    assert_eq!(changed(Language::Rust, rust_tuple, rust_plain).len(), 1);
    assert_eq!(changed(Language::Rust, rust_tuple, rust_return).len(), 1);
    // black wraps a single parameter with a magic trailing comma: the
    // parameter list's comma is optional, the tuple's is kept.
    let black = "def upload(\n    values=(1,),\n):\n    return values\n";
    assert_eq!(changed(Language::Python, tuple, black), [], "black");
    // A trailing comma in a list of two or more stays optional.
    let pair = "def upload(values=(1, 2)):\n    return values\n";
    let pair_wrapped = "def upload(values=(\n    1,\n    2,\n)):\n    return values\n";
    assert_eq!(changed(Language::Python, pair, pair_wrapped), []);
}
