//! Call sites and parameter counts, for the write-time break alert (ADR 0029).
//!
//! Both are facts about text, not about what a call reaches: a call of `f`
//! is a call of every definition named `f`. A count that cannot be stated
//! exactly (a spread argument, a signature cut at its bound, two definitions
//! of one name) is `None`, and callers say nothing about it.

use crate::{Language, MAX_SOURCE_BYTES, NAME_BYTES, end_line, has_child_kind, line, text};
use tree_sitter::{Node, Parser};

/// Calls returned for one file at most; the rest are not reported.
pub const MAX_CALLS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The called name as written, without any path or receiver.
    pub name: String,
    /// One-based line of the name.
    pub line: u32,
    /// One-based lines the whole call spans, from its callee to its closing
    /// parenthesis: a write to any of them may change the call.
    pub lines: (u32, u32),
    /// Arguments as written. `None` when a spread or splat makes the count
    /// unknowable from the text.
    pub args: Option<u32>,
    /// Written after a receiver or a module, as in `a.f(x)` (not `a::f`).
    pub member: bool,
}

/// How many arguments a function takes, from its signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arity {
    /// Parameters without a default, not variadic, not the receiver.
    pub min: u32,
    /// All parameters but the receiver; `None` when one is variadic.
    pub max: Option<u32>,
    /// The first parameter is an explicit receiver (Rust `self`, Python
    /// `self` or `cls`), passed by `a.f(...)` and written out by `T::f(a, ...)`.
    /// Whether a call fits is decided in `langspan_core::Arity::fits`.
    pub receiver: bool,
}

/// The call sites in `source`, in order of position. Fails only when
/// nothing can be said: the input is too large or the parser cannot be
/// prepared. A file with syntax errors yields the calls that parsed.
pub fn calls(language: Language, source: &[u8]) -> Result<Vec<Call>, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!("source exceeds {MAX_SOURCE_BYTES} bytes"));
    }
    let tree = parse(language, source)?;
    let mut found = Vec::new();
    // An explicit stack: nesting depth is attacker-controlled.
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(call) = call_site(language, node, source) {
            found.push(call);
            if found.len() >= MAX_CALLS {
                break;
            }
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    Ok(found)
}

fn parse(language: Language, source: &[u8]) -> Result<tree_sitter::Tree, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&language.grammar())
        .map_err(|error| format!("cannot load the {} grammar: {error}", language.name()))?;
    parser
        .parse(source, None)
        .ok_or_else(|| "the parser returned no tree".into())
}

/// A Java method call (`a.f(x)`, `f(x)`) or constructor call (`new T(x)`,
/// named by the type's last identifier). `this(x)` and `super(x)` name no
/// constructor by their text and are left out.
fn java_call_site(node: Node, source: &[u8]) -> Option<Call> {
    let (name, member) = match node.kind() {
        "method_invocation" => (
            node.child_by_field_name("name")?,
            node.child_by_field_name("object").is_some(),
        ),
        "object_creation_expression" => {
            let mut named = node.child_by_field_name("type")?;
            // `ArrayList<String>`, then `java.util.ArrayList`: the type's
            // own name.
            loop {
                named = match named.kind() {
                    "generic_type" => named.named_child(0)?,
                    "scoped_type_identifier" => {
                        let mut cursor = named.walk();
                        named.named_children(&mut cursor).last()?
                    }
                    "type_identifier" => break,
                    _ => return None,
                };
            }
            (named, false)
        }
        _ => return None,
    };
    let name_text = text(name, source);
    if name_text.is_empty() || name_text.len() > NAME_BYTES {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    Some(Call {
        name: name_text.to_owned(),
        line: line(name),
        lines: (line(node), end_line(arguments)),
        args: count_arguments(arguments),
        member,
    })
}

/// A C# name as called: `F` of `F` or `F<T>`.
fn csharp_simple_name(node: Node) -> Option<Node> {
    match node.kind() {
        "identifier" => Some(node),
        "generic_name" => node.named_child(0),
        _ => None,
    }
}

/// A C# method call (`a.F(x)`, `a?.F(x)`, `F<T>(x)`) or constructor call
/// (`new T(x)`, named by the type's last identifier). A target-typed
/// `new(x)` names no type, and `new T { ... }` without parentheses has no
/// argument list; both are left out, as is a `: base(x)` or `: this(x)`
/// initializer.
fn csharp_call_site(node: Node, source: &[u8]) -> Option<Call> {
    let (name, member) = match node.kind() {
        "invocation_expression" => {
            let function = node.child_by_field_name("function")?;
            match function.kind() {
                "member_access_expression" => (
                    csharp_simple_name(function.child_by_field_name("name")?)?,
                    true,
                ),
                "conditional_access_expression" => {
                    let mut cursor = function.walk();
                    let binding = function
                        .named_children(&mut cursor)
                        .find(|child| child.kind() == "member_binding_expression")?;
                    (
                        csharp_simple_name(binding.child_by_field_name("name")?)?,
                        true,
                    )
                }
                _ => (csharp_simple_name(function)?, false),
            }
        }
        "object_creation_expression" => {
            let mut named = node.child_by_field_name("type")?;
            // `System.Text.StringBuilder`, then `List<int>`: the type's own
            // name.
            while named.kind() == "qualified_name" {
                named = named.child_by_field_name("name")?;
            }
            (csharp_simple_name(named)?, false)
        }
        _ => return None,
    };
    let name_text = text(name, source);
    if name_text.is_empty() || name_text.len() > NAME_BYTES {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    Some(Call {
        name: name_text.to_owned(),
        line: line(name),
        lines: (line(node), end_line(arguments)),
        args: count_arguments(arguments),
        member,
    })
}

fn call_site(language: Language, node: Node, source: &[u8]) -> Option<Call> {
    if language == Language::Java {
        return java_call_site(node, source);
    }
    if language == Language::CSharp {
        return csharp_call_site(node, source);
    }
    let call_kind = match language {
        Language::Python => "call",
        _ => "call_expression",
    };
    if node.kind() != call_kind {
        return None;
    }
    let mut function = node.child_by_field_name("function")?;
    // `g::<u8>()` in Rust wraps the callee.
    if function.kind() == "generic_function" {
        function = function.child_by_field_name("function")?;
    }
    let (name, member) = match function.kind() {
        "identifier" => (function, false),
        // Rust `a::f`: a path, so the receiver, if any, is written out.
        "scoped_identifier" => (function.child_by_field_name("name")?, false),
        "field_expression" => (function.child_by_field_name("field")?, true),
        "attribute" => (function.child_by_field_name("attribute")?, true),
        "member_expression" => (function.child_by_field_name("property")?, true),
        "selector_expression" => (function.child_by_field_name("field")?, true),
        _ => return None,
    };
    let name_text = text(name, source);
    if name_text.is_empty() || name_text.len() > NAME_BYTES {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let args = match arguments.kind() {
        // Python `f(x for x in y)`: one argument without its own parentheses.
        "generator_expression" => Some(1),
        "arguments" | "argument_list" => count_arguments(arguments),
        // A tagged template and anything else: not an argument list.
        _ => return None,
    };
    Some(Call {
        name: name_text.to_owned(),
        line: line(name),
        lines: (line(node), end_line(node)),
        args,
        member,
    })
}

fn count_arguments(arguments: Node) -> Option<u32> {
    let mut count = 0u32;
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        match argument.kind() {
            kind if kind.contains("comment") => {}
            "attribute_item" => {}
            "spread_element" | "list_splat" | "dictionary_splat" | "variadic_argument" => {
                return None;
            }
            _ => count += 1,
        }
    }
    Some(count)
}

/// The parameter count of a function signature as the index stores it (the
/// declaration up to its body, on one line). `None` when the text is not
/// exactly one function's declaration: a signature cut at its bound, two
/// definitions joined, or a type, constant or module.
pub fn arity(language: Language, signature: &str) -> Option<Arity> {
    if signature.is_empty() || signature.ends_with('…') || signature.len() > 4 * 1024 {
        return None;
    }
    let candidates: Vec<String> = match language {
        Language::Rust | Language::Go => vec![format!("{signature} {{}}")],
        Language::Python => vec![format!("{signature}:\n    pass\n")],
        Language::TypeScript | Language::Tsx => vec![
            format!("{signature} {{}}"),
            format!("{signature} => {{}}"),
            format!("class L {{ {signature} {{}} }}"),
        ],
        // A method or constructor is only ever a member of a type.
        Language::Java | Language::CSharp => vec![format!("class L {{ {signature} {{}} }}")],
    };
    candidates
        .iter()
        .find_map(|candidate| declared_arity(language, candidate.as_bytes()))
}

fn declared_arity(language: Language, source: &[u8]) -> Option<Arity> {
    let tree = parse(language, source).ok()?;
    if tree.root_node().has_error() {
        return None;
    }
    let mut functions = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if is_function(language, node.kind()) {
            functions.push(node);
            // Nested functions are part of a body, and a stub has none.
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    let [function] = functions[..] else {
        return None;
    };
    parameters(language, function, source)
}

fn is_function(language: Language, kind: &str) -> bool {
    match language {
        Language::Rust => kind == "function_item",
        Language::Python => kind == "function_definition",
        Language::TypeScript | Language::Tsx => matches!(
            kind,
            "function_declaration"
                | "generator_function_declaration"
                | "arrow_function"
                | "function_expression"
                | "function"
                | "method_definition"
        ),
        Language::Go => matches!(kind, "function_declaration" | "method_declaration"),
        Language::Java | Language::CSharp => {
            matches!(kind, "method_declaration" | "constructor_declaration")
        }
    }
}

/// A C# parameter list. `params T[] items` is not wrapped in a `parameter`
/// node: the `params` keyword is followed by its type and name, so every
/// node after it belongs to that last parameter.
fn csharp_parameters(list: Node, source: &[u8]) -> Option<Arity> {
    let mut arity = Arity {
        min: 0,
        max: Some(0),
        receiver: false,
    };
    let mut params = false;
    let mut cursor = list.walk();
    for child in list.children(&mut cursor) {
        let kind = child.kind();
        match kind {
            "(" | ")" | "," | "attribute_list" => {}
            _ if kind.contains("comment") => {}
            "params" => {
                params = true;
                arity.max = None;
            }
            _ if params => {}
            "parameter" => {
                let mut inner = child.walk();
                let mut words = child.children(&mut inner);
                // `this Cart cart` makes an extension method, called as
                // `cart.F()` or as `Carts.F(cart)`: both are member calls,
                // so no count fits both.
                if words.any(|word| word.kind() == "modifier" && text(word, source) == "this") {
                    return None;
                }
                let optional = has_child_kind(child, "=");
                if !optional {
                    arity.min += 1;
                }
                if let Some(max) = arity.max.as_mut() {
                    *max += 1;
                }
            }
            // `__arglist` and anything else: the count would be a guess.
            _ => return None,
        }
    }
    Some(arity)
}

fn parameters(language: Language, function: Node, source: &[u8]) -> Option<Arity> {
    let mut arity = Arity {
        min: 0,
        max: Some(0),
        receiver: false,
    };
    let add = |required: bool, arity: &mut Arity| {
        if required {
            arity.min += 1;
        }
        if let Some(max) = arity.max.as_mut() {
            *max += 1;
        }
    };
    // A TypeScript arrow function with one bare parameter: `x => ...`.
    if function.kind() == "arrow_function" && function.child_by_field_name("parameter").is_some() {
        add(true, &mut arity);
        return Some(arity);
    }
    let list = function.child_by_field_name("parameters")?;
    if language == Language::CSharp {
        return csharp_parameters(list, source);
    }
    let mut cursor = list.walk();
    for (index, parameter) in list.named_children(&mut cursor).enumerate() {
        let kind = parameter.kind();
        if kind.contains("comment") || kind == "attribute_item" {
            continue;
        }
        match (language, kind) {
            (Language::Rust, "self_parameter") => arity.receiver = true,
            (Language::Rust, "parameter") => add(true, &mut arity),
            (Language::Rust, "variadic_parameter") => arity.max = None,
            (Language::Python, "keyword_separator" | "positional_separator") => {}
            (Language::Python, "list_splat_pattern" | "dictionary_splat_pattern") => {
                arity.max = None
            }
            (Language::Python, "identifier" | "typed_parameter") => {
                let splat = parameter.named_child(0).is_some_and(|inner| {
                    matches!(
                        inner.kind(),
                        "list_splat_pattern" | "dictionary_splat_pattern"
                    )
                });
                let receiver_name = matches!(
                    text(parameter.named_child(0).unwrap_or(parameter), source),
                    "self" | "cls"
                );
                if splat {
                    arity.max = None;
                } else if index == 0 && receiver_name {
                    arity.receiver = true;
                } else {
                    add(true, &mut arity);
                }
            }
            (Language::Python, "default_parameter" | "typed_default_parameter") => {
                add(false, &mut arity)
            }
            (Language::TypeScript | Language::Tsx, "required_parameter") => {
                let pattern = parameter.child_by_field_name("pattern");
                match pattern.map(|pattern| pattern.kind()) {
                    Some("rest_pattern") => arity.max = None,
                    // `this: T` declares the receiver's type; it is never passed.
                    Some("this") => {}
                    _ => add(parameter.child_by_field_name("value").is_none(), &mut arity),
                }
            }
            (Language::TypeScript | Language::Tsx, "optional_parameter") => add(false, &mut arity),
            (Language::TypeScript | Language::Tsx, "identifier") => add(true, &mut arity),
            (Language::TypeScript | Language::Tsx, "assignment_pattern") => add(false, &mut arity),
            (Language::TypeScript | Language::Tsx, "rest_pattern") => arity.max = None,
            (Language::Go, "parameter_declaration") => {
                let mut names = parameter.walk();
                let named = parameter
                    .children_by_field_name("name", &mut names)
                    .count()
                    .max(1);
                for _ in 0..named {
                    add(true, &mut arity);
                }
            }
            (Language::Go, "variadic_parameter_declaration") => arity.max = None,
            (Language::Java, "formal_parameter") => add(true, &mut arity),
            (Language::Java, "spread_parameter") => arity.max = None,
            // `Orders this` names the type of the receiver; it is never passed.
            (Language::Java, "receiver_parameter") => {}
            // Anything else: the count would be a guess.
            _ => return None,
        }
    }
    Some(arity)
}
