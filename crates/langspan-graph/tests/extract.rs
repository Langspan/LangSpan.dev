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

/// A Java source file as a Maven or Gradle project lays it out, with every
/// kind of definition the language has. Java has no free functions: every
/// method and constructor belongs to a type.
const ORDERS_JAVA: &str = r#"package shop;

import java.util.List;
import java.util.*;
import static java.util.Objects.requireNonNull;

/** Places orders. */
@Service
public class Orders extends Base implements Placer {
    public static final int LIMIT = 255;
    static final String PREFIX = "o-", SUFFIX = "-x";
    private int count;

    public Orders(Cart cart) {
        this.count = 0;
    }

    @Override
    public Order place(Cart cart) {
        return helper(cart);
    }

    protected <T> List<T> history(int limit) throws java.io.IOException {
        return List.of();
    }

    private Order helper(Cart cart) {
        return new Order(requireNonNull(cart));
    }

    void packagePrivate() {}

    public enum Status {
        OPEN,
        CLOSED;

        public boolean done() {
            return this == CLOSED;
        }
    }

    public record Line(String sku, int quantity) {}

    public void run() {
        Runnable task = new Runnable() {
            public void tick() {}
        };
    }
}

interface Placer {
    int MAX = 3;

    Order place(Cart cart);

    private void secret() {}
}

public @interface Audited {
    String value();
}
"#;

#[test]
fn java_definitions_of_each_kind_with_lines_signatures_references_and_imports() {
    let graph = extract(Language::Java, ORDERS_JAVA.as_bytes()).unwrap();
    assert!(!graph.partial);
    let found: Vec<(&str, SymbolKind, u32, u32, &str)> = graph
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.name.as_str(),
                symbol.kind,
                symbol.line,
                symbol.end_line,
                symbol.signature.as_str(),
            )
        })
        .collect();
    use SymbolKind::{Constant, Interface, Method, Type};
    assert_eq!(
        found,
        [
            // Line 9: the class starts after its doc comment and annotation.
            (
                "Orders",
                Type,
                9,
                49,
                "public class Orders extends Base implements Placer"
            ),
            ("LIMIT", Constant, 10, 10, "public static final int LIMIT"),
            // One field, two constants.
            ("PREFIX", Constant, 11, 11, "static final String PREFIX"),
            ("SUFFIX", Constant, 11, 11, "static final String SUFFIX"),
            ("Orders", Method, 14, 16, "public Orders(Cart cart)"),
            // Line 19: after `@Override`, which is not in the signature.
            ("place", Method, 19, 21, "public Order place(Cart cart)"),
            (
                "history",
                Method,
                23,
                25,
                "protected <T> List<T> history(int limit) throws java.io.IOException"
            ),
            ("helper", Method, 27, 29, "private Order helper(Cart cart)"),
            ("packagePrivate", Method, 31, 31, "void packagePrivate()"),
            ("Status", Type, 33, 40, "public enum Status"),
            ("OPEN", Constant, 34, 34, "OPEN"),
            ("CLOSED", Constant, 35, 35, "CLOSED"),
            ("done", Method, 37, 39, "public boolean done()"),
            (
                "Line",
                Type,
                42,
                42,
                "public record Line(String sku, int quantity)"
            ),
            ("run", Method, 44, 48, "public void run()"),
            ("tick", Method, 46, 46, "public void tick()"),
            ("Placer", Interface, 51, 57, "interface Placer"),
            ("MAX", Constant, 52, 52, "int MAX"),
            ("place", Method, 54, 54, "Order place(Cart cart)"),
            ("secret", Method, 56, 56, "private void secret()"),
            ("Audited", Interface, 59, 61, "public @interface Audited"),
            ("value", Method, 60, 60, "String value()"),
        ]
    );
    // A definition's bytes start after its annotations and doc comment.
    let place = &graph.symbols[5];
    assert!(ORDERS_JAVA[place.bytes.clone()].starts_with("public Order place(Cart cart) {"));
    assert!(ORDERS_JAVA[place.bytes.clone()].ends_with('}'));
    assert!(refers(&graph, "helper") && refers(&graph, "Cart") && refers(&graph, "requireNonNull"));
    assert!(
        graph.references.iter().all(|r| r.name != "packagePrivate"),
        "a definition's own name is not a reference to it"
    );
    let imports: Vec<&str> = graph.imports.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        imports,
        [
            "java.util.List",
            "java.util.*",
            "java.util.Objects.requireNonNull"
        ]
    );

    // module-info.java declares a module.
    let module = extract(
        Language::Java,
        b"module shop.core {\n    requires java.sql;\n    exports shop;\n}\n",
    )
    .unwrap();
    assert_eq!(
        names(&module),
        [("shop.core", SymbolKind::Module, true)],
        "{module:?}"
    );
    assert_eq!(module.symbols[0].signature, "module shop.core");
    assert_eq!(
        Language::for_path("src/main/java/shop/Orders.java"),
        Some(Language::Java)
    );
    assert_eq!(Language::Java.name(), "java");
}

#[test]
fn java_exports_what_another_package_can_reach() {
    let graph = extract(Language::Java, ORDERS_JAVA.as_bytes()).unwrap();
    let exported = |name: &str, line: u32| {
        graph
            .symbols
            .iter()
            .find(|symbol| symbol.name == name && symbol.line == line)
            .unwrap_or_else(|| panic!("{name} at {line}"))
            .exported
    };
    // `public` and `protected` are reachable from another package.
    assert!(exported("Orders", 9) && exported("Orders", 14) && exported("place", 19));
    assert!(
        exported("history", 23),
        "protected reaches subclasses elsewhere"
    );
    assert!(exported("LIMIT", 10) && exported("Status", 33) && exported("Line", 42));
    // Package-private and private are not.
    assert!(!exported("PREFIX", 11) && !exported("SUFFIX", 11));
    assert!(!exported("helper", 27) && !exported("packagePrivate", 31));
    assert!(!exported("Placer", 51), "a package-private interface");
    // Enum constants are public; an interface's members are public unless
    // declared private.
    assert!(exported("OPEN", 34) && exported("CLOSED", 35));
    assert!(exported("MAX", 52) && exported("place", 54));
    assert!(!exported("secret", 56));
    assert!(exported("value", 60), "an annotation element is public");
    // A method of an anonymous class is no one's interface.
    assert!(!exported("tick", 46));
}

#[test]
fn a_java_rewrap_or_annotation_is_not_a_signature_change_and_a_type_change_is() {
    let changed = |before: &str, after: &str| {
        let before = extract(Language::Java, before.as_bytes()).unwrap();
        let after = extract(Language::Java, after.as_bytes()).unwrap();
        diff(&before, &after)
    };
    let one_line = "public class Orders {\n    public Order place(Cart cart, Coupon coupon, int quantity) {\n        return null;\n    }\n}\n";
    // google-java-format wraps a long parameter list after the opening
    // parenthesis with a continuation indent. Java allows no trailing comma
    // in a parameter list.
    let wrapped = "public class Orders {\n  public Order place(\n      Cart cart,\n      Coupon coupon,\n      int quantity) {\n    return null;\n  }\n}\n";
    assert_eq!(changed(one_line, wrapped), [], "google-java-format");
    // An annotation is not part of the signature.
    let annotated = "public class Orders {\n    @Deprecated\n    @Override public Order place(Cart cart, Coupon coupon, int quantity) {\n        return null;\n    }\n}\n";
    assert_eq!(changed(one_line, annotated), [], "annotation");
    // A parameter's type is.
    let retyped = "public class Orders {\n    public Order place(Basket cart, Coupon coupon, int quantity) {\n        return null;\n    }\n}\n";
    assert_eq!(
        changed(one_line, retyped),
        [langspan_graph::SymbolChange {
            name: "place".into(),
            kind: SymbolKind::Method,
            change: Change::SignatureChanged {
                before: "public Order place(Cart cart, Coupon coupon, int quantity)".into(),
                after: "public Order place(Basket cart, Coupon coupon, int quantity)".into(),
            },
        }]
    );
    // The issue's example: a parameter added.
    let before = "public class Orders {\n    public Order place(Cart cart) { return null; }\n}\n";
    let after = "public class Orders {\n    public Order place(Cart cart, Coupon coupon) { return null; }\n}\n";
    assert_eq!(
        changed(before, after)[0].change,
        Change::SignatureChanged {
            before: "public Order place(Cart cart)".into(),
            after: "public Order place(Cart cart, Coupon coupon)".into(),
        }
    );
    // A body edit and a private method's change are not interface changes.
    let body = "public class Orders {\n    public Order place(Cart cart) { return cart.order(); }\n    private void hidden(int a) {}\n}\n";
    let body_after = "public class Orders {\n    public Order place(Cart cart) { return null; }\n    private void hidden(long a) {}\n}\n";
    assert_eq!(changed(body, body_after), []);
}

#[test]
fn a_java_constant_array_dimension_after_its_name_is_part_of_its_signature() {
    let signatures = |source: &str| -> Vec<(String, String)> {
        extract(Language::Java, source.as_bytes())
            .unwrap()
            .symbols
            .into_iter()
            .map(|symbol| (symbol.name, symbol.signature))
            .collect()
    };
    let pair = |name: &str, signature: &str| (name.to_owned(), signature.to_owned());
    let before = "public class Ids {\n    public static final int IDS[] = null;\n}\n";
    let after = "public class Ids {\n    public static final int IDS[][] = null;\n}\n";
    assert_eq!(
        signatures(before)[1],
        pair("IDS", "public static final int IDS[]")
    );
    let changes = diff(
        &extract(Language::Java, before.as_bytes()).unwrap(),
        &extract(Language::Java, after.as_bytes()).unwrap(),
    );
    assert_eq!(
        changes,
        [langspan_graph::SymbolChange {
            name: "IDS".into(),
            kind: SymbolKind::Constant,
            change: Change::SignatureChanged {
                before: "public static final int IDS[]".into(),
                after: "public static final int IDS[][]".into(),
            },
        }]
    );
    // One field, differently dimensioned constants.
    let mixed =
        "public class Ids {\n    public static final int A = 1, B[] = null, C[][] = null;\n}\n";
    assert_eq!(
        signatures(mixed)[1..],
        [
            pair("A", "public static final int A"),
            pair("B", "public static final int B[]"),
            pair("C", "public static final int C[][]"),
        ]
    );
}

#[test]
fn a_java_annotation_element_change_or_removal_is_an_interface_change() {
    let graph = |source: &str| extract(Language::Java, source.as_bytes()).unwrap();
    let before =
        graph("public @interface Audited {\n    String value();\n    int level() default 1;\n}\n");
    let level = before.symbols.iter().find(|s| s.name == "level").unwrap();
    assert_eq!(
        (level.kind, level.exported, level.signature.as_str()),
        (SymbolKind::Method, true, "int level()"),
        "the default value is not what a caller writes"
    );
    let retyped = graph(
        "public @interface Audited {\n    String[] value();\n    int level() default 1;\n}\n",
    );
    assert_eq!(
        diff(&before, &retyped),
        [langspan_graph::SymbolChange {
            name: "value".into(),
            kind: SymbolKind::Method,
            change: Change::SignatureChanged {
                before: "String value()".into(),
                after: "String[] value()".into(),
            },
        }]
    );
    let removed = graph("public @interface Audited {\n    int level() default 1;\n}\n");
    let changes = diff(&before, &removed);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].name, "value");
    assert!(matches!(changes[0].change, Change::Removed), "{changes:?}");
}

/// `langspan doctor` lists `Language::ALL`: each entry is chosen by its
/// own extension and named once. The match makes a new variant name its
/// extension here.
#[test]
fn every_listed_language_is_chosen_by_its_extension_and_named_once() {
    let extension = |language: Language| match language {
        Language::Rust => "rs",
        Language::Python => "py",
        Language::TypeScript => "ts",
        Language::Tsx => "tsx",
        Language::Go => "go",
        Language::Java => "java",
    };
    for language in Language::ALL {
        assert_eq!(
            Language::for_path(&format!("src/a.{}", extension(language))),
            Some(language)
        );
    }
    let mut names: Vec<&str> = Language::ALL.iter().map(|l| l.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Language::ALL.len());
}

#[test]
fn a_java_file_that_does_not_parse_is_indexed_as_far_as_it_parsed() {
    let graph = extract(
        Language::Java,
        b"public class Ok {\n    public void fine() {}\n}\nclass Broken {\n    void half( {{{\n",
    )
    .unwrap();
    assert!(graph.partial);
    assert!(graph.symbols.iter().any(|symbol| symbol.name == "fine"));
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
    // Without block digests a rename is a removal and an addition; no link
    // between them is claimed (renames with digests: `tests/renames.rs`).
    assert_eq!(find("old_name").change, Change::Removed);
    assert_eq!(find("new_name").change, Change::Added);
    assert!(
        changes.iter().all(|change| change.name != "private"),
        "a private symbol is not an interface"
    );
}

#[test]
fn a_second_same_named_definition_is_an_addition_and_losing_one_is_a_removal() {
    let version = |source: &str| extract(Language::Rust, source.as_bytes()).unwrap();
    let one = version(
        "pub struct A;\npub struct B;\nimpl A {\n    pub fn text(&self) -> String { String::new() }\n}\n",
    );
    let two = version(
        "pub struct A;\npub struct B;\nimpl A {\n    pub fn text(&self) -> String { String::new() }\n}\nimpl B {\n    pub fn text(&self) -> String { \"b\".into() }\n}\n",
    );
    let text = |changes: Vec<langspan_graph::SymbolChange>| {
        changes
            .into_iter()
            .filter(|change| change.name == "text")
            .map(|change| change.change)
            .collect::<Vec<_>>()
    };
    // The same signature twice is not a changed signature: one more
    // definition has that name.
    assert_eq!(
        text(diff(&one, &two)),
        [Change::DefinitionsAdded {
            signatures: "pub fn text(&self) -> String".into(),
            before: 1,
            after: 2,
        }]
    );
    assert_eq!(
        text(diff(&two, &one)),
        [Change::DefinitionsRemoved {
            signatures: "pub fn text(&self) -> String".into(),
            before: 2,
            after: 1,
        }]
    );
    // Order is not a change, and bodies are not interfaces.
    let swapped = version(
        "pub struct A;\npub struct B;\nimpl B {\n    pub fn text(&self) -> String { String::new() }\n}\nimpl A {\n    pub fn text(&self) -> String { \"x\".into() }\n}\n",
    );
    assert_eq!(text(diff(&two, &swapped)), []);
    // An added definition with its own signature keeps every earlier one:
    // still an addition, naming only the new signature.
    let other = version(
        "pub struct A;\npub struct B;\nimpl A {\n    pub fn text(&self) -> String { String::new() }\n}\nimpl B {\n    pub fn text(&self, limit: usize) -> String { String::new() }\n}\n",
    );
    assert_eq!(
        text(diff(&one, &other)),
        [Change::DefinitionsAdded {
            signatures: "pub fn text(&self, limit: usize) -> String".into(),
            before: 1,
            after: 2,
        }]
    );
    // A real change to one of two is still a changed signature.
    let changed = version(
        "pub struct A;\npub struct B;\nimpl A {\n    pub fn text(&self) -> String { String::new() }\n}\nimpl B {\n    pub fn text(&self, limit: usize) -> String { String::new() }\n}\n",
    );
    assert_eq!(
        text(diff(&two, &changed)),
        [Change::SignatureChanged {
            before: "pub fn text(&self) -> String | pub fn text(&self) -> String".into(),
            after: "pub fn text(&self) -> String | pub fn text(&self, limit: usize) -> String"
                .into(),
        }]
    );
    // From two to none is a removal of the name, as for one.
    let none = version("pub struct A;\npub struct B;\n");
    assert_eq!(text(diff(&two, &none)), [Change::Removed]);
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
    for language in Language::ALL {
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
