//! Import targets resolved to workspace paths (roadmap D3). Resolution is
//! by path alone: the target as written, the importing file's path and the
//! files the workspace holds. Nothing reads a configuration file, a
//! package manifest or a type.
use langspan_graph::{Language, extract, resolve_import};

fn resolved(importer: &str, target: &str, files: &[&str]) -> Option<String> {
    resolve_import(importer, target, |path| files.contains(&path))
}

/// The import targets `extract` records for `source`, as written.
fn targets(language: Language, source: &str) -> Vec<String> {
    extract(language, source.as_bytes())
        .unwrap()
        .imports
        .into_iter()
        .map(|import| import.name)
        .collect()
}

#[test]
fn a_relative_typescript_import_resolves_to_the_file_it_names() {
    let written = targets(
        Language::TypeScript,
        "import { save } from \"./store\";\nexport function service() { save(); }\n",
    );
    assert_eq!(written, ["./store"]);
    let files = ["src/store.ts", "src/service.ts"];
    assert_eq!(
        resolved("src/service.ts", &written[0], &files).as_deref(),
        Some("src/store.ts")
    );
}

#[test]
fn typescript_tries_extensions_then_an_index_file_and_maps_js_to_ts() {
    let files = [
        "src/api/handler.ts",
        "src/lib/index.tsx",
        "src/util.js",
        "src/model.ts",
    ];
    let from = "src/api/handler.ts";
    assert_eq!(
        resolved(from, "../lib", &files).as_deref(),
        Some("src/lib/index.tsx")
    );
    assert_eq!(
        resolved(from, "../util", &files).as_deref(),
        Some("src/util.js")
    );
    // ESM TypeScript writes the emitted name: `./model.js` is `model.ts`.
    assert_eq!(
        resolved(from, "../model.js", &files).as_deref(),
        Some("src/model.ts")
    );
    assert_eq!(
        resolved(from, "./handler.ts", &files).as_deref(),
        Some(from)
    );
}

#[test]
fn a_bare_or_escaping_typescript_import_is_not_resolved() {
    let files = ["react.ts", "src/react.ts", "store.ts"];
    assert_eq!(resolved("src/a.ts", "react", &files), None);
    assert_eq!(resolved("src/a.ts", "@/store", &files), None);
    // Above the workspace root: no file there is one of the workspace's.
    assert_eq!(resolved("src/a.ts", "../../store", &files), None);
    assert_eq!(resolved("src/a.ts", "./missing", &files), None);
}

#[test]
fn python_relative_and_absolute_imports_resolve_to_modules_and_packages() {
    let written = targets(
        Language::Python,
        "from .store import save\nfrom ..core import base\nimport app.models\n",
    );
    assert_eq!(written, [".store", "..core", "app.models"]);
    let files = [
        "src/app/api/store.py",
        "src/app/core/__init__.py",
        "src/app/models.py",
        "src/app/api/handler.py",
    ];
    let from = "src/app/api/handler.py";
    assert_eq!(
        resolved(from, ".store", &files).as_deref(),
        Some("src/app/api/store.py")
    );
    assert_eq!(
        resolved(from, "..core", &files).as_deref(),
        Some("src/app/core/__init__.py")
    );
    // An absolute module is looked for under each directory above the file.
    assert_eq!(
        resolved(from, "app.models", &files).as_deref(),
        Some("src/app/models.py")
    );
    assert_eq!(resolved(from, "os.path", &files), None);
    assert_eq!(resolved("a.py", "...too.far", &files), None);
}

#[test]
fn rust_crate_super_and_self_paths_resolve_to_module_files() {
    let written = targets(
        Language::Rust,
        "use crate::store::save;\nuse super::api::{a, b};\nuse self::inner::x;\nuse std::fs;\n",
    );
    assert_eq!(
        written,
        [
            "crate::store::save",
            "super::api::{a, b}",
            "self::inner::x",
            "std::fs"
        ]
    );
    let files = [
        "src/lib.rs",
        "src/store.rs",
        "src/api/mod.rs",
        "src/service.rs",
        "src/service/inner.rs",
    ];
    let from = "src/service.rs";
    assert_eq!(
        resolved(from, "crate::store::save", &files).as_deref(),
        Some("src/store.rs")
    );
    assert_eq!(
        resolved(from, "super::api::{a, b}", &files).as_deref(),
        Some("src/api/mod.rs")
    );
    assert_eq!(
        resolved(from, "self::inner::x", &files).as_deref(),
        Some("src/service/inner.rs")
    );
    assert_eq!(resolved(from, "std::fs", &files), None);
    // No `lib.rs` or `main.rs` above the file: no crate root to start from.
    assert_eq!(resolved("tools/x.rs", "crate::store", &files), None);
}

#[test]
fn a_typescript_directory_import_of_the_workspace_root_resolves_to_its_index() {
    let files = ["index.ts", "a.ts", "src/a.ts"];
    assert_eq!(
        resolved("src/a.ts", "..", &files).as_deref(),
        Some("index.ts")
    );
    assert_eq!(resolved("a.ts", ".", &files).as_deref(), Some("index.ts"));
    assert_eq!(resolved("a.ts", "./", &files).as_deref(), Some("index.ts"));
    // Above the root is still nothing.
    assert_eq!(resolved("src/a.ts", "../..", &files), None);
}

#[test]
fn a_rust_crate_path_naming_no_module_file_resolves_to_the_crate_root() {
    let files = ["src/lib.rs", "src/store.rs", "src/service.rs"];
    let from = "src/service.rs";
    // `Language` is defined in `lib.rs` itself.
    assert_eq!(
        resolved(from, "crate::Language", &files).as_deref(),
        Some("src/lib.rs")
    );
    assert_eq!(
        resolved(from, "crate::{Language, extract}", &files).as_deref(),
        Some("src/lib.rs")
    );
    // A module file still wins.
    assert_eq!(
        resolved(from, "crate::store::save", &files).as_deref(),
        Some("src/store.rs")
    );
}

#[test]
fn a_rust_glob_import_resolves_to_its_module() {
    let written = targets(Language::Rust, "use crate::store::*;\nuse super::api::*;\n");
    assert_eq!(written, ["crate::store::*", "super::api::*"]);
    let files = [
        "src/lib.rs",
        "src/store.rs",
        "src/api/mod.rs",
        "src/service.rs",
    ];
    let from = "src/service.rs";
    assert_eq!(
        resolved(from, &written[0], &files).as_deref(),
        Some("src/store.rs")
    );
    assert_eq!(
        resolved(from, &written[1], &files).as_deref(),
        Some("src/api/mod.rs")
    );
}

#[test]
fn go_java_and_csharp_imports_are_left_as_written() {
    let files = ["store/store.go", "com/app/Store.java", "App/Store.cs"];
    assert_eq!(resolved("main.go", "example.com/app/store", &files), None);
    assert_eq!(resolved("Main.java", "com.app.Store", &files), None);
    assert_eq!(resolved("Main.cs", "App", &files), None);
}

#[test]
fn every_candidate_is_a_normalized_workspace_path() {
    // Whatever is written, `exists` is only ever asked about a relative path
    // without `.` or `..` parts, so it can be a plain lookup.
    for (importer, target) in [
        ("src/a.ts", "./../../../etc/passwd"),
        ("src/a.ts", "./x/../../y"),
        ("a/b/c.py", "....x"),
        ("src/a.rs", "super::super::super::x"),
        ("src/a.ts", "./\u{0}"),
    ] {
        resolve_import(importer, target, |path| {
            assert!(
                !path.starts_with('/')
                    && path
                        .split('/')
                        .all(|part| !part.is_empty() && part != "." && part != ".."),
                "{importer} {target}: asked about {path:?}"
            );
            false
        });
    }
}
