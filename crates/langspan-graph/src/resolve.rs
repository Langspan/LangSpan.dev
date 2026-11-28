//! Import targets resolved to workspace paths (roadmap D3).
//!
//! By path alone: the target as written, the importing file's path and
//! whether a candidate file is one the workspace holds. No configuration
//! file, package manifest, search path or type is read, so an alias
//! (`@/store`), a `paths` mapping or a module found through a manifest stays
//! unresolved. Go, Java and C# name packages and namespaces, not files, and
//! are never resolved here.

use crate::Language;

/// TypeScript and JavaScript extensions, in the order a bare relative
/// specifier tries them.
const SCRIPT: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// The workspace file an import of `target`, written in the workspace file
/// `importer`, names: the first candidate for which `exists` holds. `exists`
/// is asked only about relative paths without empty, `.` or `..` parts. The
/// rules, by the importer's language:
///
/// - TypeScript and JavaScript: a specifier starting `./` or `../`, joined to
///   the importer's directory; as written, then (for `.js`, `.jsx`, `.mjs`
///   and `.cjs`) with the TypeScript extension it is emitted from, then with
///   each extension in `SCRIPT`, then as a directory's `index` file (the
///   workspace root's, for `..` from `src/a.ts`).
/// - Python: a relative module (`.store`, `..core`) from the importer's
///   package; an absolute one (`app.models`) under each directory above the
///   importer, nearest first. A module is `NAME.py`, `NAME.pyi` or
///   `NAME/__init__.py`.
/// - Rust: a `use` path starting `crate::` (from the nearest directory above
///   the importer that holds `lib.rs` or `main.rs`, and from `main.rs` itself
///   when it is the importer, whose crate is the binary), `super::` or `self::`,
///   or a bare path, which is taken as relative to the importer's module.
///   The longest prefix of the path that names a module file (`NAME.rs` or
///   `NAME/mod.rs`) wins, so `crate::store::save` is `src/store.rs`. A
///   `crate::` path that names no module file (`crate::Language`) is the
///   crate root's file. A glob (`crate::store::*`) is its module's path.
pub fn resolve_import(
    importer: &str,
    target: &str,
    mut exists: impl FnMut(&str) -> bool,
) -> Option<String> {
    let candidates = match Language::for_path(importer)? {
        Language::TypeScript | Language::Tsx => script(importer, target),
        Language::Python => python(importer, target),
        Language::Rust => return rust(importer, target, &mut exists),
        Language::Go | Language::Java | Language::CSharp => Vec::new(),
    };
    candidates.into_iter().find(|candidate| exists(candidate))
}

/// The directory parts of `path`.
fn directory(path: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    parts.pop();
    parts
}

/// `parts` joined, after applying each `.` and `..` of `relative` to them.
/// `None` above the workspace root, or for a part holding a control
/// character or a backslash.
fn joined<'a>(mut parts: Vec<&'a str>, relative: &'a str) -> Option<String> {
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ if part.chars().any(|c| c.is_control() || c == '\\') => return None,
            _ => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn script(importer: &str, target: &str) -> Vec<String> {
    let relative =
        target == "." || target == ".." || target.starts_with("./") || target.starts_with("../");
    if !relative {
        return Vec::new();
    }
    let Some(base) = joined(directory(importer), target) else {
        // `..` from `src/a.ts` names the workspace root: only its `index`.
        let root = joined(directory(importer), &format!("{target}/index"));
        return match root.as_deref() {
            Some("index") => SCRIPT.iter().map(|ext| format!("index.{ext}")).collect(),
            _ => Vec::new(),
        };
    };
    let mut candidates = Vec::new();
    let file = base.rsplit('/').next().unwrap_or(&base);
    if let Some((stem, extension)) = base.rsplit_once('.')
        && file.contains('.')
        && SCRIPT.contains(&extension)
    {
        candidates.push(base.clone());
        let emitted_from: &[&str] = match extension {
            "js" => &["ts", "tsx"],
            "jsx" => &["tsx"],
            "mjs" => &["mts"],
            "cjs" => &["cts"],
            _ => &[],
        };
        candidates.extend(emitted_from.iter().map(|ts| format!("{stem}.{ts}")));
    }
    candidates.extend(SCRIPT.iter().map(|extension| format!("{base}.{extension}")));
    candidates.extend(
        SCRIPT
            .iter()
            .map(|extension| format!("{base}/index.{extension}")),
    );
    candidates
}

/// `NAME.py`, `NAME.pyi` and `NAME/__init__.py` for the module at `base`.
fn python_module(base: &str) -> [String; 3] {
    [
        format!("{base}.py"),
        format!("{base}.pyi"),
        format!("{base}/__init__.py"),
    ]
}

fn python(importer: &str, target: &str) -> Vec<String> {
    // `import a.b as c`: the module is `a.b`.
    let target = target.split(" as ").next().unwrap_or(target).trim();
    let dots = target.chars().take_while(|c| *c == '.').count();
    let module = target[dots..].replace('.', "/");
    let mut package = directory(importer);
    if dots > 0 {
        // One dot is the importer's own package; each more is one up.
        for _ in 1..dots {
            if package.pop().is_none() {
                return Vec::new();
            }
        }
        return match joined(package, &module) {
            Some(base) if !module.is_empty() => python_module(&base).into(),
            Some(base) => vec![format!("{base}/__init__.py")],
            None if module.is_empty() => vec!["__init__.py".into()],
            None => Vec::new(),
        };
    }
    if module.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    loop {
        if let Some(base) = joined(package.clone(), &module) {
            candidates.extend(python_module(&base));
        }
        if package.pop().is_none() {
            break;
        }
    }
    candidates
}

/// The directory a Rust file's child modules live in: its own directory
/// for `mod.rs`, `lib.rs` and `main.rs`, else one named by its stem.
fn rust_module_directory(importer: &str) -> Vec<&str> {
    let mut parts = directory(importer);
    let file = importer.rsplit('/').next().unwrap_or(importer);
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    if !matches!(stem, "mod" | "lib" | "main") && !stem.is_empty() {
        parts.push(stem);
    }
    parts
}

fn rust(importer: &str, target: &str, exists: &mut impl FnMut(&str) -> bool) -> Option<String> {
    // `a::{b, c}`, `a::*` and `a::b as c`: the path before the group, the
    // glob or the alias.
    let path = target.split('{').next().unwrap_or(target);
    let path = path
        .split(" as ")
        .next()
        .unwrap_or(path)
        .trim()
        .trim_end_matches('*')
        .trim_end_matches(':');
    let mut segments: Vec<&str> = path.split("::").map(str::trim).collect();
    if segments.iter().any(|segment| {
        segment.is_empty() || !segment.chars().all(|c| c.is_alphanumeric() || c == '_')
    }) {
        return None;
    }
    // An item of the crate root itself (`crate::Language`) is in its file.
    let mut crate_root = None;
    let base = match segments.first().copied() {
        Some("crate") => {
            segments.remove(0);
            let mut parts = directory(importer);
            // `main.rs` is the binary crate's own root, beside any `lib.rs`.
            let first = if importer.rsplit('/').next() == Some("main.rs") {
                ["main.rs", "lib.rs"]
            } else {
                ["lib.rs", "main.rs"]
            };
            loop {
                let root = first.into_iter().find_map(|name| {
                    let mut path = parts.clone();
                    path.push(name);
                    let path = path.join("/");
                    exists(&path).then_some(path)
                });
                if root.is_some() {
                    crate_root = root;
                    break parts;
                }
                parts.pop()?;
            }
        }
        Some("self") => {
            segments.remove(0);
            rust_module_directory(importer)
        }
        Some("super") => {
            let mut parts = rust_module_directory(importer);
            while segments.first() == Some(&"super") {
                segments.remove(0);
                parts.pop()?;
            }
            parts
        }
        _ => rust_module_directory(importer),
    };
    (1..=segments.len())
        .rev()
        .find_map(|length| {
            let stem = joined(base.clone(), &segments[..length].join("/"))?;
            [format!("{stem}.rs"), format!("{stem}/mod.rs")]
                .into_iter()
                .find(|candidate| exists(candidate))
        })
        .or(crate_root)
}
