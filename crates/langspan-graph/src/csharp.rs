//! C# (`.cs`, roadmap D6): definitions, the export rule, signatures and
//! imports. Call sites and parameter counts are in `calls.rs`.

use crate::{
    FileGraph, NAME_BYTES, Reference, SIGNATURE_BYTES, Symbol, SymbolKind, end_line, line,
    one_line, text,
};
use tree_sitter::Node;

/// C#'s imports and definitions. `None` when `node` is neither, so the
/// caller looks at it as a reference; otherwise what `visit` returns.
pub(crate) fn visit(node: Node, source: &[u8], graph: &mut FileGraph) -> Option<Option<usize>> {
    let kind = node.kind();
    match kind {
        "using_directive" => {
            // The namespace or type as written after `using`, `using static`
            // or an alias's `=`.
            let mut cursor = node.walk();
            let target = node
                .named_children(&mut cursor)
                .filter(|child| !child.kind().contains("comment"))
                .last()?;
            graph.imports.push(Reference {
                name: one_line(text(target, source), SIGNATURE_BYTES),
                line: line(node),
            });
            // The imported names are still references.
            return None;
        }
        // Not a symbol, as a Java package is not: every file in a namespace
        // declares it. Its name is not a mention of a type of the same name.
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            return Some(node.child_by_field_name("name").map(|name| name.id()));
        }
        _ => {}
    }
    let symbol_kind = match kind {
        "variable_declarator" if constant(node, source) => SymbolKind::Constant,
        "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "enum_declaration"
        | "delegate_declaration" => SymbolKind::Type,
        "interface_declaration" => SymbolKind::Interface,
        "method_declaration" | "constructor_declaration" | "property_declaration" => {
            SymbolKind::Method
        }
        "enum_member_declaration" => SymbolKind::Constant,
        _ => return None,
    };
    let name = node.child_by_field_name("name")?;
    let name_text = text(name, source);
    if name_text.is_empty() || name_text.len() > NAME_BYTES {
        return Some(Some(name.id()));
    }
    let whole = declaration(node);
    let start = start(whole);
    graph.symbols.push(Symbol {
        name: name_text.to_owned(),
        kind: symbol_kind,
        line: line(start),
        end_line: end_line(whole),
        signature: signature(node, source),
        exported: exported(node, source),
        bytes: start.start_byte()..whole.end_byte(),
    });
    Some(Some(name.id()))
}

/// The declaration a definition's modifiers belong to: a constant is named
/// by its declarator, inside its field's `variable_declaration`.
fn declaration(node: Node) -> Node {
    if node.kind() == "variable_declarator"
        && let Some(field) = node.parent().and_then(|declaration| declaration.parent())
    {
        return field;
    }
    node
}

/// The keywords of a declaration's own modifiers, as written.
fn modifiers<'a>(declaration: Node, source: &'a [u8]) -> Vec<&'a str> {
    let mut cursor = declaration.walk();
    declaration
        .children(&mut cursor)
        .filter(|child| child.kind() == "modifier")
        .map(|modifier| text(modifier, source))
        .collect()
}

/// A field declares constants when it is `const`, or `static readonly`.
fn constant(declarator: Node, source: &[u8]) -> bool {
    let field = declaration(declarator);
    if field.kind() != "field_declaration" {
        return false;
    }
    let keywords = modifiers(field, source);
    keywords.contains(&"const") || (keywords.contains(&"static") && keywords.contains(&"readonly"))
}

/// C#'s own rule: what another assembly can reach. `public`, `protected`
/// and `protected internal` are exported; `internal`, `private protected`,
/// `private`, `file` and no modifier are not, since a type defaults to
/// `internal` and a member to `private`. An interface's members default to
/// `public`, and an enum's members are always public. An explicit interface
/// implementation (`void IPlacer.Cancel()`) is reached only through its
/// interface, whose own member is exported. Each declaration is judged
/// alone, as in Rust and Java: a `public` method of an `internal` class is
/// exported.
fn exported(node: Node, source: &[u8]) -> bool {
    if node.kind() == "enum_member_declaration" {
        return true;
    }
    let declaration = declaration(node);
    let mut cursor = declaration.walk();
    if declaration
        .children(&mut cursor)
        .any(|child| child.kind() == "explicit_interface_specifier")
    {
        return false;
    }
    let modifiers = modifiers(declaration, source);
    let has = |keyword: &str| modifiers.contains(&keyword);
    let in_interface = declaration
        .parent()
        .filter(|body| body.kind() == "declaration_list")
        .and_then(|body| body.parent())
        .is_some_and(|owner| owner.kind() == "interface_declaration");
    if in_interface {
        return !has("private") && (!has("internal") || has("protected"));
    }
    has("public") || (has("protected") && !has("private"))
}

/// The first node of a declaration's own text: after the attributes and
/// comments that lead it.
fn start(node: Node) -> Node {
    let mut cursor = node.walk();
    let first = node
        .children(&mut cursor)
        .find(|child| child.kind() != "attribute_list" && !child.kind().contains("comment"));
    first.unwrap_or(node)
}

fn between(source: &[u8], start: usize, end: usize) -> String {
    let text = String::from_utf8_lossy(source.get(start..end.max(start)).unwrap_or(&[]));
    one_line(
        text.trim_end_matches(['{', ';', '=', ' ', '\n', '\t', '\r']),
        SIGNATURE_BYTES,
    )
}

/// A C# definition's signature: the declaration up to its body, without the
/// attributes before it. A constant's is its modifiers, type and name,
/// without the value; an enum member's is its name. A constructor's ends
/// before its `: base(...)` or `: this(...)` initializer, which runs as the
/// body's first step. A property's is its declaration and accessors without
/// their bodies or an initial value: `public int Count { get; private set; }`,
/// and `{ get; }` for an expression-bodied property.
fn signature(node: Node, source: &[u8]) -> String {
    let from = start(declaration(node)).start_byte();
    match node.kind() {
        "enum_member_declaration" => node
            .child_by_field_name("name")
            .map(|name| text(name, source).to_owned())
            .unwrap_or_default(),
        "variable_declarator" => {
            let kind_end = node
                .parent()
                .and_then(|declaration| declaration.child_by_field_name("type"))
                .map_or(from, |kind| kind.end_byte());
            let name = node
                .child_by_field_name("name")
                .map_or("", |name| text(name, source));
            one_line(
                &format!("{} {name}", between(source, from, kind_end)),
                SIGNATURE_BYTES,
            )
        }
        "property_declaration" => {
            let accessors = node.child_by_field_name("accessors");
            let arrow = node
                .child_by_field_name("value")
                .filter(|value| value.kind() == "arrow_expression_clause");
            let end = accessors
                .or(arrow)
                .map_or(node.end_byte(), |part| part.start_byte());
            let mut listed = Vec::new();
            if let Some(accessors) = accessors {
                let mut cursor = accessors.walk();
                for accessor in accessors.named_children(&mut cursor) {
                    if accessor.kind() != "accessor_declaration" {
                        continue;
                    }
                    let mut words = modifiers(accessor, source);
                    if let Some(name) = accessor.child_by_field_name("name") {
                        words.push(text(name, source));
                    }
                    listed.push(format!("{};", words.join(" ")));
                }
            } else if arrow.is_some() {
                listed.push("get;".to_owned());
            }
            let declared = between(source, from, end);
            if listed.is_empty() {
                return declared;
            }
            one_line(
                &format!("{declared} {{ {} }}", listed.join(" ")),
                SIGNATURE_BYTES,
            )
        }
        _ => {
            let mut cursor = node.walk();
            let initializer = node
                .children(&mut cursor)
                .find(|child| child.kind() == "constructor_initializer")
                .map(|initializer| initializer.start_byte());
            let end = initializer
                .or_else(|| {
                    node.child_by_field_name("body")
                        .map(|body| body.start_byte())
                })
                .unwrap_or(node.end_byte());
            between(source, from, end)
        }
    }
}
