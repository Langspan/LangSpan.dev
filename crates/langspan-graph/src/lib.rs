//! Structural extraction from source text: the symbols a file defines, the
//! names it refers to and what it imports. No I/O. A result describes exactly
//! the bytes it was given; the caller records which bytes those were.
//!
//! Matching is by name. A reference to `open` is a reference to every symbol
//! named `open`; nothing here resolves scopes, types or modules. Callers must
//! present results as name matches, never as proven uses.

use tree_sitter::{Node, Parser};

mod calls;
mod csharp;
pub use calls::{Arity, Call, MAX_CALLS, arity, calls};

/// Larger inputs are refused rather than partly parsed.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_SYMBOLS: usize = 2_000;
pub const MAX_REFERENCES: usize = 20_000;
pub const MAX_IMPORTS: usize = 500;
const SIGNATURE_BYTES: usize = 240;
const NAME_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    Go,
    Java,
    CSharp,
}

impl Language {
    /// Every indexed language, in the order `langspan doctor` lists them.
    pub const ALL: [Self; 7] = [
        Self::Rust,
        Self::Python,
        Self::TypeScript,
        Self::Tsx,
        Self::Go,
        Self::Java,
        Self::CSharp,
    ];

    /// Chosen by file extension only. Content is never sniffed.
    pub fn for_path(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1;
        Some(match extension {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => Self::TypeScript,
            "tsx" | "jsx" => Self::Tsx,
            "go" => Self::Go,
            "java" => Self::Java,
            "cs" => Self::CSharp,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolKind {
    Function,
    Method,
    Type,
    Interface,
    Constant,
    Module,
}

impl SymbolKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Type => "type",
            Self::Interface => "interface",
            Self::Constant => "constant",
            Self::Module => "module",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Function,
            Self::Method,
            Self::Type,
            Self::Interface,
            Self::Constant,
            Self::Module,
        ]
        .into_iter()
        .find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// One-based, inclusive.
    pub line: u32,
    pub end_line: u32,
    /// The declaration up to its body, on one line. Two versions of a symbol
    /// with the same signature text may still differ in behaviour.
    pub signature: String,
    /// Visible outside its file by the language's own rule: `pub` in Rust,
    /// `export` in TypeScript, a capital in Go, no leading underscore in
    /// Python, `public` or `protected` in Java (see `java_exported`) and in
    /// C# (see `csharp::exported`).
    pub exported: bool,
    /// The definition's own bytes in the source it was extracted from,
    /// without leading comments or attributes.
    pub bytes: std::ops::Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileGraph {
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
    /// Import targets as written: a module path or a quoted specifier.
    pub imports: Vec<Reference>,
    /// The parser reported a syntax error, or a bound was reached. What was
    /// extracted is still reported.
    pub partial: bool,
}

/// Extract the structure of `source`. Fails only when nothing can be said:
/// the input is too large or the parser cannot be prepared.
pub fn extract(language: Language, source: &[u8]) -> Result<FileGraph, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source exceeds {MAX_SOURCE_BYTES} bytes; it is not indexed"
        ));
    }
    let mut parser = Parser::new();
    parser
        .set_language(&language.grammar())
        .map_err(|error| format!("cannot load the {} grammar: {error}", language.name()))?;
    let tree = parser
        .parse(source, None)
        .ok_or("the parser returned no tree")?;
    let mut graph = FileGraph {
        partial: tree.root_node().has_error(),
        ..FileGraph::default()
    };
    // An explicit stack: source nesting depth is attacker-controlled.
    let mut stack = vec![(tree.root_node(), false)];
    while let Some((node, in_type)) = stack.pop() {
        let defined = visit(language, node, in_type, source, &mut graph);
        let child_in_type = in_type || is_type_body(language, node.kind());
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            // The node that names a definition is not also a reference to it.
            if defined.is_some_and(|name| name == child.id()) {
                continue;
            }
            stack.push((child, child_in_type));
        }
        if graph.symbols.len() >= MAX_SYMBOLS
            || graph.references.len() >= MAX_REFERENCES
            || graph.imports.len() >= MAX_IMPORTS
        {
            graph.partial = true;
            break;
        }
    }
    graph
        .symbols
        .sort_by(|a, b| (a.line, &a.name).cmp(&(b.line, &b.name)));
    graph
        .references
        .sort_by(|a, b| (a.line, &a.name).cmp(&(b.line, &b.name)));
    graph.references.dedup();
    Ok(graph)
}

fn is_type_body(language: Language, kind: &str) -> bool {
    match language {
        Language::Rust => matches!(kind, "impl_item" | "trait_item"),
        Language::Python => kind == "class_definition",
        Language::TypeScript | Language::Tsx => {
            matches!(
                kind,
                "class_declaration" | "interface_declaration" | "class"
            )
        }
        // Every Java and C# method and constructor belongs to a type, so its
        // kind needs no context.
        Language::Go | Language::Java | Language::CSharp => false,
    }
}

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn line(node: Node) -> u32 {
    u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX)
}

fn end_line(node: Node) -> u32 {
    u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX)
}

fn one_line(text: &str, limit: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.len() > limit {
            break;
        }
    }
    if out.len() > limit {
        let mut end = limit;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

/// The declaration without its body: everything before the body node.
fn signature(node: Node, source: &[u8]) -> String {
    let end = ["body", "value"]
        .into_iter()
        .find_map(|field| node.child_by_field_name(field))
        .map(|body| body.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let start = node.start_byte();
    let slice = source.get(start..end.max(start)).unwrap_or(&[]);
    let text = String::from_utf8_lossy(slice);
    one_line(
        text.trim_end_matches(['{', ':', '=', ';', ' ', '\n', '\t']),
        SIGNATURE_BYTES,
    )
}

fn has_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| child.kind() == kind)
}

fn exported(language: Language, node: Node, name: &str) -> bool {
    match language {
        Language::Rust => has_child_kind(node, "visibility_modifier"),
        Language::Python => !name.starts_with('_'),
        Language::Go => name.chars().next().is_some_and(char::is_uppercase),
        Language::TypeScript | Language::Tsx => {
            let mut current = node;
            for _ in 0..3 {
                match current.parent() {
                    Some(parent) if parent.kind() == "export_statement" => return true,
                    Some(parent) => current = parent,
                    None => break,
                }
            }
            false
        }
        Language::Java => java_exported(node),
        // `csharp::visit` extracts C# and judges its exports itself.
        Language::CSharp => false,
    }
}

/// Java's own rule: what another package can reach. `public` and
/// `protected` are exported; package-private and `private` are not. An
/// interface's or annotation type's members are public unless declared
/// `private`, and so is an enum constant and a module. A method of an
/// anonymous class or of an enum constant's body is no one's interface.
/// Each declaration is judged alone, as in Rust: a `public` method of a
/// package-private class is exported.
fn java_exported(node: Node) -> bool {
    if matches!(node.kind(), "enum_constant" | "module_declaration") {
        return true;
    }
    // A constant is named by its declarator; the modifiers are its field's.
    let declaration = match node.kind() {
        "variable_declarator" => match node.parent() {
            Some(parent) => parent,
            None => return false,
        },
        _ => node,
    };
    let modifier = |keyword: &str| {
        let mut cursor = declaration.walk();
        declaration
            .children(&mut cursor)
            .filter(|child| child.kind() == "modifiers")
            .any(|modifiers| has_child_kind(modifiers, keyword))
    };
    let container = declaration.parent();
    match container.map(|container| container.kind()) {
        Some("interface_body" | "annotation_type_body") => return !modifier("private"),
        Some("class_body") => {
            let owner = container.and_then(|container| container.parent());
            if owner.is_some_and(|owner| {
                matches!(owner.kind(), "object_creation_expression" | "enum_constant")
            }) {
                return false;
            }
        }
        _ => {}
    }
    modifier("public") || modifier("protected")
}

/// A Java field declares constants when it is both `static` and `final`;
/// an interface's fields always are.
fn java_constant(declarator: Node) -> bool {
    let Some(field) = declarator.parent() else {
        return false;
    };
    match field.kind() {
        "constant_declaration" => true,
        "field_declaration" => {
            let mut cursor = field.walk();
            field
                .children(&mut cursor)
                .filter(|child| child.kind() == "modifiers")
                .any(|modifiers| {
                    has_child_kind(modifiers, "static") && has_child_kind(modifiers, "final")
                })
        }
        _ => false,
    }
}

fn is_annotation(node: Node) -> bool {
    matches!(node.kind(), "annotation" | "marker_annotation") || node.kind().contains("comment")
}

/// The first node of a Java declaration's own text: after the annotations
/// and comments that lead its modifiers.
fn java_start(node: Node) -> Node {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        if child.kind() == "modifiers" {
            let mut inner = child.walk();
            let first = child.children(&mut inner).find(|m| !is_annotation(*m));
            if let Some(first) = first {
                return first;
            }
        } else if !is_annotation(child) {
            return child;
        }
    }
    node
}

/// The source between `start` and `end` without the annotations among a
/// declaration's modifiers, on one line. An annotation is metadata: adding
/// `@Deprecated` or `@Override` does not change what a caller writes.
fn java_text(declaration: Node, start: usize, end: usize, source: &[u8]) -> String {
    let mut holes = Vec::new();
    let mut cursor = declaration.walk();
    for child in declaration.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut inner = child.walk();
            holes.extend(
                child
                    .children(&mut inner)
                    .filter(|m| is_annotation(*m))
                    .map(|m| m.start_byte()..m.end_byte()),
            );
        }
    }
    // The end closes the last piece.
    holes.push(end..end);
    let mut text = String::new();
    let mut at = start;
    for hole in holes {
        if hole.start >= at && hole.start <= end {
            text.push_str(&String::from_utf8_lossy(
                source.get(at..hole.start).unwrap_or(&[]),
            ));
            text.push(' ');
            at = hole.end.min(end).max(at);
        }
    }
    one_line(
        text.trim_end_matches(['{', ';', '=', ' ', '\n', '\t', '\r']),
        SIGNATURE_BYTES,
    )
}

/// A Java definition's signature: the declaration up to its body, without
/// annotations. A constant's is its modifiers, type, name and any array
/// dimensions after the name, without the value; an enum constant's is its
/// name. An annotation element's ends before its `default` value, which a
/// caller does not write.
fn java_signature(node: Node, source: &[u8]) -> String {
    match node.kind() {
        "enum_constant" => node
            .child_by_field_name("name")
            .map(|name| text(name, source).to_owned())
            .unwrap_or_default(),
        "variable_declarator" => {
            let Some(field) = node.parent() else {
                return String::new();
            };
            let end = field
                .child_by_field_name("type")
                .map_or(field.start_byte(), |kind| kind.end_byte());
            let name = node
                .child_by_field_name("name")
                .map_or("", |name| text(name, source));
            // `int IDS[][]`: the brackets after one declarator's name are
            // part of its type.
            let dimensions = node
                .child_by_field_name("dimensions")
                .map_or(String::new(), |dimensions| {
                    one_line(text(dimensions, source), SIGNATURE_BYTES).replace(' ', "")
                });
            let prefix = java_text(field, java_start(field).start_byte(), end, source);
            one_line(&format!("{prefix} {name}{dimensions}"), SIGNATURE_BYTES)
        }
        _ => {
            let mut cursor = node.walk();
            let default = node
                .children(&mut cursor)
                .find(|child| child.kind() == "default")
                .map(|keyword| keyword.start_byte());
            let end = node
                .child_by_field_name("body")
                .map(|body| body.start_byte())
                .or(default)
                .unwrap_or(node.end_byte());
            java_text(node, java_start(node).start_byte(), end, source)
        }
    }
}

fn definition_kind(language: Language, kind: &str, in_type: bool) -> Option<SymbolKind> {
    use SymbolKind::*;
    let function = if in_type { Method } else { Function };
    let script = matches!(language, Language::TypeScript | Language::Tsx);
    Some(match (language, kind) {
        (Language::Rust, "function_item" | "function_signature_item") => function,
        (Language::Rust, "struct_item" | "enum_item" | "union_item" | "type_item") => Type,
        (Language::Rust, "trait_item") => Interface,
        (Language::Rust, "const_item" | "static_item") => Constant,
        (Language::Rust, "mod_item") => Module,
        (Language::Rust, "macro_definition") => Function,
        (Language::Python, "function_definition") => function,
        (Language::Python, "class_definition") => Type,
        (_, "function_declaration" | "generator_function_declaration") if script => Function,
        (_, "method_definition" | "method_signature" | "abstract_method_signature") if script => {
            Method
        }
        (
            _,
            "class_declaration"
            | "abstract_class_declaration"
            | "enum_declaration"
            | "type_alias_declaration",
        ) if script => Type,
        (_, "interface_declaration") if script => Interface,
        (_, "internal_module" | "module") if script => Module,
        (Language::Go, "function_declaration") => Function,
        (Language::Go, "method_declaration") => Method,
        (Language::Go, "type_spec" | "type_alias") => Type,
        (Language::Go, "const_spec") => Constant,
        (
            Language::Java,
            "method_declaration"
            | "constructor_declaration"
            | "annotation_type_element_declaration",
        ) => Method,
        (Language::Java, "class_declaration" | "record_declaration" | "enum_declaration") => Type,
        (Language::Java, "interface_declaration" | "annotation_type_declaration") => Interface,
        (Language::Java, "enum_constant") => Constant,
        (Language::Java, "module_declaration") => Module,
        _ => return None,
    })
}

fn is_reference(language: Language, kind: &str) -> bool {
    match language {
        Language::Python => kind == "identifier",
        Language::TypeScript | Language::Tsx => {
            matches!(
                kind,
                "identifier" | "type_identifier" | "property_identifier"
            )
        }
        Language::Rust | Language::Go => {
            matches!(kind, "identifier" | "type_identifier" | "field_identifier")
        }
        Language::Java => matches!(kind, "identifier" | "type_identifier"),
        Language::CSharp => kind == "identifier",
    }
}

fn import_target<'a>(language: Language, node: Node<'a>) -> Option<Node<'a>> {
    match (language, node.kind()) {
        (Language::Rust, "use_declaration") => node.child_by_field_name("argument"),
        (Language::Python, "import_from_statement") => node.child_by_field_name("module_name"),
        (Language::Python, "import_statement") => node.child_by_field_name("name"),
        (Language::TypeScript | Language::Tsx, "import_statement") => {
            node.child_by_field_name("source")
        }
        (Language::Go, "import_spec") => node.child_by_field_name("path"),
        _ => None,
    }
}

/// Java's imports and definitions. `None` when `node` is neither, so the
/// caller looks at it as a reference; otherwise what `visit` returns.
fn visit_java(node: Node, source: &[u8], graph: &mut FileGraph) -> Option<Option<usize>> {
    let kind = node.kind();
    if kind == "import_declaration" {
        // As written after `import` or `import static`: `java.util.*` keeps
        // its asterisk.
        let mut cursor = node.walk();
        let target = node
            .named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))?;
        let end = node.end_byte();
        let written = String::from_utf8_lossy(source.get(target.start_byte()..end).unwrap_or(&[]));
        graph.imports.push(Reference {
            name: one_line(
                written.trim_end_matches([';', ' ', '\n', '\t', '\r']),
                SIGNATURE_BYTES,
            ),
            line: line(node),
        });
        // The imported names are still references.
        return None;
    }
    let symbol_kind = match kind {
        "variable_declarator" if java_constant(node) => SymbolKind::Constant,
        "variable_declarator" => return None,
        _ => definition_kind(Language::Java, kind, false)?,
    };
    let name = node.child_by_field_name("name")?;
    let name_text = text(name, source);
    if name_text.is_empty() || name_text.len() > NAME_BYTES {
        return Some(Some(name.id()));
    }
    // A constant spans its whole field, so a change to its type is within it.
    let whole = match kind {
        "variable_declarator" => node.parent().unwrap_or(node),
        _ => node,
    };
    let start = java_start(whole);
    graph.symbols.push(Symbol {
        name: name_text.to_owned(),
        kind: symbol_kind,
        line: line(start),
        end_line: end_line(whole),
        signature: java_signature(node, source),
        exported: java_exported(node),
        bytes: start.start_byte()..whole.end_byte(),
    });
    Some(Some(name.id()))
}

/// Record what `node` is. Returns the ID of the node naming a definition, so
/// the walk does not also count it as a reference.
fn visit(
    language: Language,
    node: Node,
    in_type: bool,
    source: &[u8],
    graph: &mut FileGraph,
) -> Option<usize> {
    let kind = node.kind();
    if language == Language::Java {
        if let Some(defined) = visit_java(node, source, graph) {
            return defined;
        }
    } else if language == Language::CSharp {
        if let Some(defined) = csharp::visit(node, source, graph) {
            return defined;
        }
    } else if let Some(target) = import_target(language, node) {
        graph.imports.push(Reference {
            name: one_line(
                text(target, source).trim_matches(['"', '\'', '`']),
                SIGNATURE_BYTES,
            ),
            line: line(node),
        });
    }
    // `const f = (...) => ...` and `const f = function` declare a function.
    if matches!(language, Language::TypeScript | Language::Tsx)
        && kind == "variable_declarator"
        && let (Some(name), Some(value)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        )
        && name.kind() == "identifier"
        && matches!(
            value.kind(),
            "arrow_function" | "function_expression" | "function"
        )
    {
        let name_text = text(name, source);
        let end = value
            .child_by_field_name("body")
            .map(|body| body.start_byte())
            .unwrap_or_else(|| value.end_byte());
        graph.symbols.push(Symbol {
            name: name_text.to_owned(),
            kind: SymbolKind::Function,
            line: line(node),
            end_line: end_line(node),
            signature: one_line(
                String::from_utf8_lossy(source.get(node.start_byte()..end).unwrap_or(&[]))
                    .trim_end_matches(['{', '=', '>', ' ', '\n']),
                SIGNATURE_BYTES,
            ),
            exported: exported(language, node, name_text),
            bytes: node.start_byte()..node.end_byte(),
        });
        return Some(name.id());
    }
    if let Some(symbol_kind) = definition_kind(language, kind, in_type) {
        let name = node.child_by_field_name("name")?;
        let name_text = text(name, source);
        if name_text.is_empty() || name_text.len() > NAME_BYTES {
            return Some(name.id());
        }
        graph.symbols.push(Symbol {
            name: name_text.to_owned(),
            kind: symbol_kind,
            line: line(node),
            end_line: end_line(node),
            signature: signature(node, source),
            exported: exported(language, node, name_text),
            bytes: node.start_byte()..node.end_byte(),
        });
        return Some(name.id());
    }
    if is_reference(language, kind) {
        let name = text(node, source);
        if !name.is_empty() && name.len() <= NAME_BYTES {
            graph.references.push(Reference {
                name: name.to_owned(),
                line: line(node),
            });
        }
    }
    None
}

/// How one exported symbol differs between two versions of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// No exported symbol of this name and kind remains.
    Removed,
    Added,
    SignatureChanged {
        before: String,
        after: String,
    },
    /// More definitions share this name and kind than before, and every
    /// earlier signature is still among them. `signatures` are the added
    /// ones, each shown once; `before` and `after` count the definitions.
    DefinitionsAdded {
        signatures: String,
        before: u32,
        after: u32,
    },
    /// Fewer definitions share this name and kind, at least one remains,
    /// and every remaining signature was there before. `signatures` are the
    /// removed ones, each shown once.
    DefinitionsRemoved {
        signatures: String,
        before: u32,
        after: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolChange {
    pub name: String,
    pub kind: SymbolKind,
    pub change: Change,
}

/// A signature as compared, never as shown: whitespace next to punctuation
/// removed, and an optional trailing comma dropped before a closing bracket
/// or at the end. rustfmt, black, prettier and gofmt rewrap a long parameter
/// list one per line with a trailing comma, and change spacing inside braces;
/// none of that changes the interface. Whitespace between two words stays, so
/// `u32` and `u 32` still differ. The same rule holds for all four languages:
/// in each, a trailing comma in a parameter, type-parameter or `where` list is
/// optional and spacing around punctuation carries no meaning. One trailing
/// comma is not optional: the only comma of a parenthesised group that is not
/// a parameter or argument list, as in Python's `(1,)` or Rust's `(u32,)`,
/// makes it a one-element tuple, and `(1)` is something else. A parenthesis
/// right after a name or a closing `>` opens a parameter or argument list;
/// any other opens a group. Not covered: prettier adding parentheses around a
/// single arrow-function parameter, or changing the quotes of a default
/// string.
fn comparable(signature: &str) -> String {
    let words: Vec<&str> = signature.split_whitespace().collect();
    let mut out = String::with_capacity(signature.len());
    let word_char = |c: char| c.is_alphanumeric() || c == '_';
    for word in words {
        let joins = out.chars().next_back().is_some_and(word_char)
            && word.chars().next().is_some_and(word_char);
        if joins {
            out.push(' ');
        }
        out.push_str(word);
    }
    let mut result = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    // Per open bracket: whether it is a parenthesised group (not a list
    // after a name), and how many commas it holds so far.
    let mut open: Vec<(bool, usize)> = Vec::new();
    for (index, &c) in chars.iter().enumerate() {
        match c {
            '(' => {
                let before = chars[..index].iter().rev().find(|c| **c != ' ');
                let arrow = index >= 2 && matches!(chars[index - 2], '-' | '=');
                let list = before.is_some_and(|&b| word_char(b) || (b == '>' && !arrow));
                open.push((!list, 0));
            }
            '[' | '{' | '<' => open.push((false, 0)),
            // The `>` of `->` and `=>` closes nothing.
            '>' if index > 0 && matches!(chars[index - 1], '-' | '=') => {}
            ')' | ']' | '}' | '>' => {
                open.pop();
            }
            ',' => {
                let next = chars[index + 1..].iter().find(|c| **c != ' ');
                let single_in_group = next == Some(&')')
                    && open
                        .last()
                        .is_some_and(|&(group, commas)| group && commas == 0);
                if let Some(top) = open.last_mut() {
                    top.1 += 1;
                }
                if !single_in_group && next.is_none_or(|next| matches!(next, ')' | ']' | '}' | '>'))
                {
                    continue;
                }
            }
            _ => {}
        }
        result.push(c);
    }
    result
}

/// Compare the exported symbols of two versions of one file, by name and
/// kind. A rename shows as one removal and one addition: nothing here claims
/// that two differently named symbols are the same thing. A change to a body
/// alone, or a formatter's rewrap of a signature (see `comparable`), is not
/// reported. A notice shows each signature as written.
pub fn diff(before: &FileGraph, after: &FileGraph) -> Vec<SymbolChange> {
    use std::collections::BTreeMap;
    // Per name and kind: (compared form, text as written), sorted.
    type Signatures = Vec<(String, String)>;
    let exported = |graph: &FileGraph| {
        let mut map: BTreeMap<(String, SymbolKind), Signatures> = BTreeMap::new();
        for symbol in graph.symbols.iter().filter(|symbol| symbol.exported) {
            map.entry((symbol.name.clone(), symbol.kind))
                .or_default()
                .push((comparable(&symbol.signature), symbol.signature.clone()));
        }
        for signatures in map.values_mut() {
            signatures.sort();
        }
        map
    };
    let (old, new) = (exported(before), exported(after));
    let same = |a: &Signatures, b: &Signatures| a.iter().map(|s| &s.0).eq(b.iter().map(|s| &s.0));
    let shown = |signatures: &Signatures| {
        signatures
            .iter()
            .map(|s| s.1.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    // The signatures of `more` left over once each of `fewer` is matched
    // with one equal to it, or `None` when one of `fewer` has no match.
    // Both are sorted by compared form.
    fn beyond<'a>(more: &'a Signatures, fewer: &Signatures) -> Option<Vec<&'a (String, String)>> {
        let mut rest = Vec::new();
        let mut pending = fewer.iter().peekable();
        for signature in more {
            if pending.peek().is_some_and(|next| next.0 == signature.0) {
                pending.next();
            } else {
                rest.push(signature);
            }
        }
        pending.peek().is_none().then_some(rest)
    }
    // Each compared form once, as first written (input sorted).
    fn distinct(signatures: Vec<&(String, String)>) -> String {
        let mut seen: Vec<&(String, String)> = Vec::new();
        for signature in signatures {
            if seen.last().is_none_or(|last| last.0 != signature.0) {
                seen.push(signature);
            }
        }
        seen.iter()
            .map(|s| s.1.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }
    let count = |signatures: &Signatures| u32::try_from(signatures.len()).unwrap_or(u32::MAX);
    let mut changes = Vec::new();
    for (key, signatures) in &old {
        let change = match new.get(key) {
            None => Change::Removed,
            Some(current) if same(current, signatures) => continue,
            // Only the number of same-named definitions changed, each
            // earlier (or each remaining) signature still there: say so,
            // never "changed from X to X | X".
            Some(current) => match (beyond(current, signatures), beyond(signatures, current)) {
                (Some(added), _) => Change::DefinitionsAdded {
                    signatures: distinct(added),
                    before: count(signatures),
                    after: count(current),
                },
                (_, Some(removed)) => Change::DefinitionsRemoved {
                    signatures: distinct(removed),
                    before: count(signatures),
                    after: count(current),
                },
                (None, None) => Change::SignatureChanged {
                    before: shown(signatures),
                    after: shown(current),
                },
            },
        };
        changes.push(SymbolChange {
            name: key.0.clone(),
            kind: key.1,
            change,
        });
    }
    for key in new.keys().filter(|key| !old.contains_key(*key)) {
        changes.push(SymbolChange {
            name: key.0.clone(),
            kind: key.1,
            change: Change::Added,
        });
    }
    changes
}
