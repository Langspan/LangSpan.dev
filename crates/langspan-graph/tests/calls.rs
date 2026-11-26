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

#[test]
fn java_method_calls_and_constructor_calls_count_their_arguments() {
    let source = r#"class Checkout {
    void submit() {
        Order order = orders.place(cart);
        helper(a, /* note */ b);
        Orders made = new Orders(cart);
        this.log();
        List.<String>of("a");
        super.close();
        new java.util.ArrayList<String>();
        new Runnable() { public void run() { tick(1); } };
        this(x);
    }
}
"#;
    assert_eq!(
        found(Language::Java, source),
        [
            call("place", 3, Some(1), true),
            call("helper", 4, Some(2), false),
            call("Orders", 5, Some(1), false),
            call("log", 6, Some(0), true),
            call("of", 7, Some(1), true),
            call("close", 8, Some(0), true),
            call("ArrayList", 9, Some(0), false),
            call("Runnable", 10, Some(0), false),
            call("tick", 10, Some(1), false),
        ],
        "`this(x)` names no constructor by its text"
    );
    // A call split over several lines spans all of them.
    let wrapped = calls(
        Language::Java,
        b"class A {\n  void f() {\n    orders.place(\n        cart,\n        coupon);\n  }\n}\n",
    )
    .unwrap();
    assert_eq!((wrapped[0].line, wrapped[0].lines), (3, (3, 5)));
}

#[test]
fn csharp_method_calls_and_constructor_calls_count_their_arguments() {
    let source = r#"class Checkout
{
    Order Submit()
    {
        var order = orders.Place(cart);
        Helper(a, /* note */ b);
        Orders made = new Orders(cart);
        this.Log();
        Enumerable.Empty<string>();
        base.Close();
        new System.Collections.Generic.List<string>(4);
        store?.Save(order, true);
        Convert<int>(x: 1, y: out var z);
        Orders implicitly = new(cart);
        var shape = new Shape { Size = 1 };
    }
}
"#;
    assert_eq!(
        found(Language::CSharp, source),
        [
            call("Place", 5, Some(1), true),
            call("Helper", 6, Some(2), false),
            call("Orders", 7, Some(1), false),
            call("Log", 8, Some(0), true),
            call("Empty", 9, Some(0), true),
            call("Close", 10, Some(0), true),
            call("List", 11, Some(1), false),
            call("Save", 12, Some(2), true),
            call("Convert", 13, Some(2), false),
        ],
        "a target-typed `new(...)` and an initializer without arguments name no constructor's count"
    );
    // A call split over several lines spans all of them.
    let wrapped = calls(
        Language::CSharp,
        b"class A\n{\n    void F()\n    {\n        orders.Place(\n            cart,\n            coupon);\n    }\n}\n",
    )
    .unwrap();
    assert_eq!((wrapped[0].line, wrapped[0].lines), (5, (5, 7)));
    // In a null-conditional chain, the call is named by its last member.
    assert_eq!(
        found(
            Language::CSharp,
            "class A\n{\n    void M()\n    {\n        a?.B?.G(1, 2);\n        a?.B.H(1, 2, 3);\n        a.B?.K();\n    }\n}\n"
        ),
        [
            call("G", 5, Some(2), true),
            call("H", 6, Some(3), true),
            call("K", 7, Some(0), true),
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
        (
            Java,
            "public Order place(Cart cart)",
            Some((1, Some(1), false)),
        ),
        (
            Java,
            "public Order place(Cart cart, Coupon coupon)",
            Some((2, Some(2), false)),
        ),
        (
            Java,
            "public static <T> List<T> of(String name, T... items)",
            Some((1, None, false)),
        ),
        (Java, "public Orders(Cart cart)", Some((1, Some(1), false))),
        (
            Java,
            "default void f(final int a, @Nullable String b) throws IOException",
            Some((2, Some(2), false)),
        ),
        (Java, "abstract int size()", Some((0, Some(0), false))),
        // A receiver parameter names the type of `this`; it is never passed.
        (
            Java,
            "void f(Orders this, int a)",
            Some((1, Some(1), false)),
        ),
        (
            Java,
            "public Order place(Cart cart) | public Order place(Cart cart, Coupon c)",
            None,
        ),
        (Java, "public class Orders", None),
        (Java, "public static final int LIMIT", None),
        (
            CSharp,
            "public Order Place(Cart cart)",
            Some((1, Some(1), false)),
        ),
        (
            CSharp,
            "public Order Place(Cart cart, Coupon coupon)",
            Some((2, Some(2), false)),
        ),
        // A default makes a parameter optional; `params` takes any number.
        (
            CSharp,
            "public static List<T> Of<T>(string name, int limit = 10, params T[] items) where T : class",
            Some((1, None, false)),
        ),
        (
            CSharp,
            "public bool TryGet(in Key key, ref int hint, out Value value)",
            Some((3, Some(3), false)),
        ),
        (
            CSharp,
            "public Orders(Cart cart)",
            Some((1, Some(1), false)),
        ),
        (CSharp, "Order Place(Cart cart)", Some((1, Some(1), false))),
        (
            CSharp,
            "public async Task<Order> PlaceAsync([NotNull] Cart cart)",
            Some((1, Some(1), false)),
        ),
        (
            CSharp,
            "void IPlacer.Cancel(Order order)",
            Some((1, Some(1), false)),
        ),
        // An extension method is called as `cart.Total()` or as
        // `Carts.Total(cart)`, and both are member calls: no single count.
        (CSharp, "public static int Total(this Cart cart)", None),
        (
            CSharp,
            "public Order Place(Cart cart) | public Order Place(Cart cart, Coupon c)",
            None,
        ),
        (CSharp, "public class Orders", None),
        (CSharp, "public record Line(string Sku, int Quantity)", None),
        (CSharp, "public const int Limit", None),
        (CSharp, "public int Count { get; set; }", None),
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

/// A symbol lookup with detail (roadmap D1) counts the calls inside one
/// definition by line and digests that definition's own bytes, so both
/// rest on these two facts.
#[test]
fn a_definitions_bytes_and_the_calls_within_its_lines() {
    let source = "use std::fs;\n\npub fn upload(path: &str) -> u8 {\n    let a = open(path);\n    let b = open(path);\n    hash(a, b)\n}\n\nfn other() { open(\"x\"); }\n";
    let graph = langspan_graph::extract(Language::Rust, source.as_bytes()).unwrap();
    let upload = graph
        .symbols
        .iter()
        .find(|symbol| symbol.name == "upload")
        .unwrap();
    assert_eq!(
        &source[upload.bytes.clone()],
        "pub fn upload(path: &str) -> u8 {\n    let a = open(path);\n    let b = open(path);\n    hash(a, b)\n}"
    );
    let within: Vec<(String, u32)> = calls(Language::Rust, source.as_bytes())
        .unwrap()
        .into_iter()
        .filter(|call| (upload.line..=upload.end_line).contains(&call.line))
        .map(|call| (call.name, call.line))
        .collect();
    assert_eq!(
        within,
        [("open".into(), 4), ("open".into(), 5), ("hash".into(), 6)]
    );
}
