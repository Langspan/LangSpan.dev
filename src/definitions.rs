//! Definition findings for a combined check ([ADR 0020], row D9): what the
//! combined branches changed about the same definitions that Git's clean
//! combination does not show. Each branch's changed files in supported
//! languages are read from the object database and indexed in memory,
//! before and after, against where the branch started from the target.
//! Nothing is written: not the source index, not a checkout, not a ref.
//!
//! Findings are name matches, as the rest of the source index is: a call
//! of `save` is a call of every definition named `save` in that language.
//! They are advisory and never change a check's result.
//!
//! [ADR 0020]: ../docs/adr/0020-source-graph-depth-after-launch.md

use crate::git;
use langspan_core::{
    Arity, MAX_ARITY, MAX_SYMBOL_FINDING_TEXT, MAX_SYMBOL_FINDINGS, MAX_SYMBOL_FINDINGS_NOTE,
    SymbolFinding, SymbolFindingKind, SymbolFindings,
};
use langspan_graph::{Change, FileGraph, Language, MAX_SOURCE_BYTES, SymbolKind};
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};

/// The time indexing may take before findings are skipped.
pub const BUDGET: Duration = Duration::from_secs(10);
/// Changed files of one branch looked at, at most; the rest are counted as
/// skipped.
const MAX_FILES: usize = 1000;
/// Serialized findings kept, at most: a check record travels in one frame
/// with its command and inputs.
const FINDINGS_BYTES: usize = crate::protocol::MAX_FRAME_BYTES / 2;

/// The budget, or a test-only shorter one.
pub fn budget() -> Duration {
    std::env::var("LANGSPAN_TEST_SYMBOL_BUDGET_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(BUDGET)
}

/// A function's parameter counts from its signature, within `MAX_ARITY`:
/// the rule the write-time break check uses ([ADR 0029]).
///
/// [ADR 0029]: ../docs/adr/0029-break-alerts-at-write-time.md
pub fn arity(language: Language, signature: &str) -> Option<Arity> {
    let arity = langspan_graph::arity(language, signature)?;
    Some(Arity {
        min: arity.min,
        max: arity.max,
        receiver: arity.receiver,
    })
    .filter(|arity| arity.min <= MAX_ARITY && arity.max.is_none_or(|max| max <= MAX_ARITY))
}

/// One exported definition a branch changed or removed.
#[derive(Debug, Clone)]
struct Definition {
    path: String,
    language: Language,
    name: String,
    kind: SymbolKind,
    /// On the branch, or where it was for a removal.
    line: u32,
    before: String,
    /// `None` for a removal or rename.
    after: Option<String>,
    renamed_to: Option<String>,
}

/// A name on a line the branch added.
#[derive(Debug, Clone)]
struct Use {
    path: String,
    language: Language,
    name: String,
    line: u32,
    /// For a call: its argument count, and whether it is written after a
    /// receiver.
    call: Option<(u32, bool)>,
}

/// What one branch changed, against where it started from the target.
#[derive(Debug, Default)]
struct Branch {
    name: String,
    changed: Vec<Definition>,
    uses: Vec<Use>,
    /// Names of definitions the branch added or renamed something to: its
    /// own lines may mean them.
    added: BTreeSet<String>,
}

/// TypeScript and TSX files call each other.
fn family(language: Language) -> Language {
    match language {
        Language::Tsx => Language::TypeScript,
        other => other,
    }
}

/// Repository text as a finding holds it: redacted, on one line, at most
/// `limit` bytes.
fn clean(text: &str, limit: usize) -> String {
    let mut out = String::new();
    for c in crate::hook::redact(text).chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > limit {
            break;
        }
        out.push(c);
    }
    out
}

/// Compare the definitions of `branches` (each with its commit), all of
/// which combine cleanly onto `target`. Never fails: a Git error or the
/// time limit gives no findings and a note saying why.
pub fn findings(
    root: &Path,
    target: &str,
    branches: &[(String, String)],
    budget: Duration,
) -> SymbolFindings {
    let deadline = Instant::now() + budget;
    let mut counts = (0_u32, 0_u32);
    let read = branches
        .iter()
        .map(|(name, commit)| branch(root, target, name, commit, deadline, &mut counts))
        .collect::<Result<Vec<_>, _>>();
    let (files_indexed, files_skipped) = counts;
    let skipped = |why: String| SymbolFindings {
        findings: Vec::new(),
        more: 0,
        files_indexed,
        files_skipped,
        note: Some(clean(
            &format!("definition findings were skipped: {why}; the check ran as without them"),
            MAX_SYMBOL_FINDINGS_NOTE,
        )),
    };
    let branches = match read {
        Ok(branches) => branches,
        Err(error) => return skipped(error),
    };
    if Instant::now() >= deadline {
        return skipped(OVER_BUDGET.into());
    }
    let (findings, more) = compare(&branches);
    SymbolFindings {
        findings,
        more,
        files_indexed,
        files_skipped,
        note: None,
    }
}

const OVER_BUDGET: &str = "indexing the changed files took longer than its time limit";

/// Index what `commit` changed since it and `target` parted.
fn branch(
    root: &Path,
    target: &str,
    name: &str,
    commit: &str,
    deadline: Instant,
    (indexed, skipped): &mut (u32, u32),
) -> Result<Branch, String> {
    let mut branch = Branch {
        name: name.to_owned(),
        ..Branch::default()
    };
    let base = text(root, &["merge-base", target, commit], deadline)?;
    let listing = git::output(
        root,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-status",
            &base,
            commit,
        ],
        deadline,
        1024 * 1024,
    )?;
    let mut fields = listing.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    let mut seen = 0;
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let Ok(path) = std::str::from_utf8(path) else {
            continue;
        };
        let Some(language) = Language::for_path(path) else {
            continue;
        };
        seen += 1;
        if seen > MAX_FILES {
            *skipped += 1;
            continue;
        }
        if Instant::now() >= deadline {
            return Err(OVER_BUDGET.into());
        }
        let status = status.first().copied();
        let before = match status {
            Some(b'A') => Some(Vec::new()),
            _ => blob(root, &base, path, deadline)?,
        };
        let after = match status {
            Some(b'D') => Some(Vec::new()),
            _ => blob(root, commit, path, deadline)?,
        };
        let (Some(before), Some(after)) = (before, after) else {
            *skipped += 1;
            continue;
        };
        let added = match status {
            Some(b'A') => None,
            _ => Some(added_lines(root, &base, commit, path, deadline)?),
        };
        index(&mut branch, path, language, &before, &after, added.as_ref())?;
        *indexed += 1;
    }
    Ok(branch)
}

fn text(root: &Path, args: &[&str], deadline: Instant) -> Result<String, String> {
    let bytes = git::output(root, args, deadline, 64 * 1024)?;
    let text = String::from_utf8(bytes).map_err(|_| "Git output is not UTF-8")?;
    Ok(text.trim().to_owned())
}

/// The file at `commit:path`, or `None` when it is larger than the
/// parser's bound or not text.
fn blob(
    root: &Path,
    commit: &str,
    path: &str,
    deadline: Instant,
) -> Result<Option<Vec<u8>>, String> {
    let object = format!("{commit}:{path}");
    let size: usize = text(root, &["cat-file", "-s", &object], deadline)?
        .parse()
        .map_err(|_| "Git did not report a file's size")?;
    if size > MAX_SOURCE_BYTES {
        return Ok(None);
    }
    let bytes = git::output(
        root,
        &["cat-file", "blob", &object],
        deadline,
        MAX_SOURCE_BYTES,
    )?;
    Ok((!bytes.contains(&0)).then_some(bytes))
}

/// The lines of `path` at `commit` that are not at `base`, from Git's diff
/// without context.
fn added_lines(
    root: &Path,
    base: &str,
    commit: &str,
    path: &str,
    deadline: Instant,
) -> Result<BTreeSet<u32>, String> {
    let output = git::output(
        root,
        &[
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "-U0",
            base,
            commit,
            "--",
            path,
        ],
        deadline,
        4 * MAX_SOURCE_BYTES,
    )?;
    let mut lines = BTreeSet::new();
    for line in String::from_utf8_lossy(&output).lines() {
        let Some(header) = line.strip_prefix("@@ -") else {
            continue;
        };
        let Some(new) = header
            .split(' ')
            .nth(1)
            .and_then(|new| new.strip_prefix('+'))
        else {
            continue;
        };
        let (start, count) = new.split_once(',').unwrap_or((new, "1"));
        let (Ok(start), Ok(count)) = (start.parse::<u32>(), count.parse::<u32>()) else {
            return Err("Git's diff of a changed file was not readable".into());
        };
        lines.extend(start..start.saturating_add(count));
    }
    Ok(lines)
}

/// Add one file's changes and new uses to `branch`. `added` is `None` when
/// every line is new.
fn index(
    branch: &mut Branch,
    path: &str,
    language: Language,
    before: &[u8],
    after: &[u8],
    added: Option<&BTreeSet<u32>>,
) -> Result<(), String> {
    let graph = |source: &[u8]| -> Result<FileGraph, String> {
        let mut graph = langspan_graph::extract(language, source)?;
        langspan_graph::digest_blocks(&mut graph, source, |block| {
            crate::sha256::hex(&crate::sha256::digest(block))
        });
        Ok(graph)
    };
    let (old, new) = (graph(before)?, graph(after)?);
    let line_of = |graph: &FileGraph, name: &str, kind: SymbolKind| {
        graph
            .symbols
            .iter()
            .find(|symbol| symbol.exported && symbol.name == name && symbol.kind == kind)
            .map_or(1, |symbol| symbol.line)
    };
    // A partial parse is not trusted to say a definition changed or went.
    let changes = if old.partial || new.partial {
        Vec::new()
    } else {
        langspan_graph::diff(&old, &new)
    };
    for change in changes {
        let (line, before, after, renamed_to) = match change.change {
            Change::SignatureChanged { before, after } => (
                line_of(&new, &change.name, change.kind),
                before,
                Some(after),
                None,
            ),
            Change::Removed => {
                let before = old
                    .symbols
                    .iter()
                    .find(|s| s.exported && s.name == change.name && s.kind == change.kind)
                    .map(|s| s.signature.clone())
                    .unwrap_or_default();
                (line_of(&old, &change.name, change.kind), before, None, None)
            }
            Change::Renamed { to, before, .. } => {
                // The new name is the branch's own definition, as an
                // addition is.
                branch.added.insert(to.clone());
                (line_of(&old, &change.name, change.kind), before, None, Some(to))
            }
            Change::Added => {
                branch.added.insert(change.name);
                continue;
            }
            // Several definitions share the name: which one a call means
            // is not known from the text.
            Change::DefinitionsAdded { .. } | Change::DefinitionsRemoved { .. } => continue,
        };
        branch.changed.push(Definition {
            path: path.to_owned(),
            language,
            name: change.name,
            kind: change.kind,
            line,
            before,
            after,
            renamed_to,
        });
    }
    let new_line = |line: u32| added.is_none_or(|added| added.contains(&line));
    let calls = langspan_graph::calls(language, after)?;
    for call in &calls {
        let written = (call.lines.0..=call.lines.1).any(new_line);
        // A count beyond what an arity states says nothing about a signature.
        if let (true, Some(args)) = (written, call.args.filter(|args| *args <= MAX_ARITY)) {
            branch.uses.push(Use {
                path: path.to_owned(),
                language,
                name: call.name.clone(),
                line: call.line,
                call: Some((args, call.member)),
            });
        }
    }
    for reference in new.references.iter().filter(|r| new_line(r.line)) {
        branch.uses.push(Use {
            path: path.to_owned(),
            language,
            name: reference.name.clone(),
            line: reference.line,
            call: None,
        });
    }
    Ok(())
}

/// The findings between every two branches, at most `MAX_SYMBOL_FINDINGS`
/// in branch, path and line order, and how many more there were.
fn compare(branches: &[Branch]) -> (Vec<SymbolFinding>, u32) {
    let mut all = Vec::new();
    let mut seen = BTreeSet::new();
    let finding =
        |kind, a: &Branch, d: &Definition, b: &Branch, path: &str, line: u32| SymbolFinding {
            finding: kind,
            name: clean(&d.name, MAX_SYMBOL_FINDING_TEXT),
            branch: clean(&a.name, MAX_SYMBOL_FINDING_TEXT),
            path: clean(&d.path, MAX_SYMBOL_FINDING_TEXT),
            line: d.line.max(1),
            before: clean(&d.before, MAX_SYMBOL_FINDING_TEXT),
            after: d
                .after
                .as_deref()
                .map(|after| clean(after, MAX_SYMBOL_FINDING_TEXT)),
            renamed_to: d
                .renamed_to
                .as_deref()
                .map(|to| clean(to, MAX_SYMBOL_FINDING_TEXT)),
            other_branch: clean(&b.name, MAX_SYMBOL_FINDING_TEXT),
            other_path: clean(path, MAX_SYMBOL_FINDING_TEXT),
            other_line: line.max(1),
            other_after: None,
            args: None,
            takes: None,
        };
    for (i, a) in branches.iter().enumerate() {
        for (j, b) in branches.iter().enumerate() {
            if i == j {
                continue;
            }
            let touches = |name: &str| {
                b.added.contains(name) || b.changed.iter().any(|other| other.name == name)
            };
            for d in a.changed.iter().filter(|d| !d.before.is_empty()) {
                match &d.after {
                    Some(after) => {
                        // Each pair once, under the branch named first.
                        if i < j {
                            for other in &b.changed {
                                if let Some(other_after) = &other.after
                                    && other.path == d.path
                                    && other.name == d.name
                                    && other.kind == d.kind
                                    && !langspan_graph::same_signature(after, other_after)
                                {
                                    let mut f = finding(
                                        SymbolFindingKind::BothChanged,
                                        a,
                                        d,
                                        b,
                                        &other.path,
                                        other.line,
                                    );
                                    f.other_after =
                                        Some(clean(other_after, MAX_SYMBOL_FINDING_TEXT));
                                    all.push(f);
                                }
                            }
                        }
                        let (Some(old), Some(new)) =
                            (arity(d.language, &d.before), arity(d.language, after))
                        else {
                            continue;
                        };
                        if touches(&d.name) {
                            continue;
                        }
                        for used in &b.uses {
                            if let Some((args, member)) = used.call
                                && used.name == d.name
                                && family(used.language) == family(d.language)
                                && old.fits(args, member)
                                && !new.fits(args, member)
                                && seen.insert((i, j, d.name.clone(), used.path.clone(), used.line))
                            {
                                let mut f = finding(
                                    SymbolFindingKind::ChangedAndCalled,
                                    a,
                                    d,
                                    b,
                                    &used.path,
                                    used.line,
                                );
                                f.args = Some(args);
                                f.takes = Some(new);
                                all.push(f);
                            }
                        }
                    }
                    None => {
                        if touches(&d.name) {
                            continue;
                        }
                        for used in &b.uses {
                            if used.name == d.name
                                && family(used.language) == family(d.language)
                                && seen.insert((i, j, d.name.clone(), used.path.clone(), used.line))
                            {
                                all.push(finding(
                                    SymbolFindingKind::RemovedAndUsed,
                                    a,
                                    d,
                                    b,
                                    &used.path,
                                    used.line,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    // At most `MAX_SYMBOL_FINDINGS`, and only while they fit their share of
    // the frame; the rest are counted.
    let total = all.len();
    let mut bytes = 0;
    let keep = all
        .iter()
        .take(MAX_SYMBOL_FINDINGS)
        .take_while(|finding| {
            bytes += serde_json::to_vec(finding).map_or(FINDINGS_BYTES, |json| json.len() + 1);
            bytes <= FINDINGS_BYTES
        })
        .count();
    all.truncate(keep);
    (all, u32::try_from(total - keep).unwrap_or(u32::MAX))
}

fn arguments(count: u32) -> &'static str {
    if count == 1 { "argument" } else { "arguments" }
}

fn takes(arity: &Arity) -> String {
    match arity.max {
        Some(max) if max == arity.min => format!("{max} {}", arguments(max)),
        Some(max) => format!("{} to {max} arguments", arity.min),
        None => format!("at least {} {}", arity.min, arguments(arity.min)),
    }
}

/// The findings as a person reads them, one line each; repository text is
/// quoted.
pub fn lines(found: &SymbolFindings) -> Vec<String> {
    let mut lines = Vec::new();
    for f in &found.findings {
        let at = format!("({}, {:?}:{})", f.branch, f.path, f.line);
        let other = format!("{:?}:{}", f.other_path, f.other_line);
        lines.push(match f.finding {
            SymbolFindingKind::BothChanged => format!(
                "both_changed: {:?} changed to {:?} {at} and to {:?} on {} at {other}",
                f.name,
                f.after.as_deref().unwrap_or_default(),
                f.other_after.as_deref().unwrap_or_default(),
                f.other_branch
            ),
            SymbolFindingKind::ChangedAndCalled => format!(
                "changed_and_called: {:?} now takes {} {at}; {} calls it with {} at {other}",
                f.name,
                f.takes.as_ref().map(takes).unwrap_or_default(),
                f.other_branch,
                f.args.unwrap_or_default()
            ),
            SymbolFindingKind::RemovedAndUsed => format!(
                "removed_and_used: {:?} {} {at}; {} mentions it at {other}",
                f.name,
                match &f.renamed_to {
                    Some(to) => format!("was renamed to {to:?}"),
                    None => "was removed".to_owned(),
                },
                f.other_branch
            ),
        });
    }
    if found.more > 0 {
        lines.push(format!("{} more not shown", found.more));
    }
    if let Some(note) = &found.note {
        lines.push(note.clone());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, files: &[(&str, &str, &str)]) -> Branch {
        let mut branch = Branch {
            name: name.into(),
            ..Branch::default()
        };
        for (path, before, after) in files {
            let language = Language::for_path(path).unwrap();
            index(
                &mut branch,
                path,
                language,
                before.as_bytes(),
                after.as_bytes(),
                None,
            )
            .unwrap();
        }
        branch
    }

    #[test]
    fn findings_stop_at_their_bound_and_count_the_rest() {
        let base = "pub fn save(x: u32) {}\n";
        let sync = branch(
            "sync",
            &[("src/s.rs", base, "pub fn save(x: u32, y: u32) {}\n")],
        );
        let calls: String = (0..40).map(|_| "fn c() { save(1); }\n").collect();
        let import = branch("import", &[("src/i.rs", "", &calls)]);
        let (findings, more) = compare(&[sync, import]);
        assert_eq!(findings.len(), MAX_SYMBOL_FINDINGS);
        assert_eq!(more, 40 - MAX_SYMBOL_FINDINGS as u32);
        assert!(
            findings
                .iter()
                .all(|f| f.finding == SymbolFindingKind::ChangedAndCalled)
        );
    }

    #[test]
    fn a_branch_that_changes_or_defines_the_name_itself_is_not_told_about_it() {
        let base = "pub fn save(x: u32) {}\n";
        let sync = branch(
            "sync",
            &[("src/s.rs", base, "pub fn save(x: u32, y: u32) {}\n")],
        );
        // It adds a call that fits its own new version of `save`, and a
        // call with one argument alongside a definition of its own.
        let own = branch(
            "own",
            &[
                ("src/s.rs", base, "pub fn save(x: u32, z: u8) {}\n"),
                ("src/i.rs", "", "fn c() { save(1); }\n"),
            ],
        );
        let (findings, _) = compare(&[sync, own]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].finding, SymbolFindingKind::BothChanged);
    }

    #[test]
    fn a_call_that_fits_both_signatures_or_another_language_is_not_a_finding() {
        let base = "pub fn save(x: u32) {}\n";
        let retry = branch("retry", &[("src/s.rs", base, "pub fn save(x: u64) {}\n")]);
        let sync = branch(
            "sync",
            &[("src/s.rs", base, "pub fn save(x: u32, y: u32) {}\n")],
        );
        let rust = branch("rust", &[("src/i.rs", "", "fn c() { save(1); }\n")]);
        let script = branch("script", &[("web/i.ts", "", "save(1);\n")]);
        assert_eq!(compare(&[retry, rust]).0, Vec::new());
        assert_eq!(compare(&[sync, script]).0, Vec::new());
    }

    #[test]
    fn text_is_cut_on_a_character_boundary_and_kept_on_one_line() {
        assert_eq!(clean("a\nb", 10), "a b");
        assert_eq!(clean("ééé", 5), "éé");
    }

    // Review follow-ups #265.

    #[test]
    fn a_call_with_more_arguments_than_an_arity_states_is_not_a_finding() {
        let base = "export function save(...rest: number[]) {}\n";
        let one = branch(
            "one",
            &[("web/s.ts", base, "export function save(a: number) {}\n")],
        );
        let call = format!("save({});\n", vec!["1"; MAX_ARITY as usize + 1].join(", "));
        let wide = branch("wide", &[("web/i.ts", "", &call)]);
        assert_eq!(compare(&[one, wide]).0, Vec::new());
    }

    #[test]
    fn a_name_the_branch_renamed_a_definition_to_is_its_own() {
        let base = "pub fn save(x: u32) {}\n";
        let sync = branch(
            "sync",
            &[("src/s.rs", base, "pub fn save(x: u32, y: u32) {}\n")],
        );
        // It renames `store` to `save` and calls its own `save`.
        let own = branch(
            "own",
            &[
                (
                    "src/m.rs",
                    "pub fn store(x: u32) {}\n",
                    "pub fn save(x: u32) {}\n",
                ),
                ("src/i.rs", "", "fn c() { save(1); }\n"),
            ],
        );
        assert_eq!(compare(&[sync, own]).0, Vec::new());
    }

    #[test]
    fn findings_are_kept_only_while_they_fit_half_a_frame() {
        let long = "\"".repeat(MAX_SYMBOL_FINDING_TEXT);
        let fns = |params: &str| -> String {
            (0..MAX_SYMBOL_FINDINGS)
                .map(|i| format!("pub fn f{i}({params}) {{}}\n"))
                .collect()
        };
        let path = format!("src/{long}.rs");
        let (base, a, b) = (fns("x: u32"), fns("x: u32, y: u32"), fns("x: u64"));
        let (findings, more) = compare(&[
            branch(&long, &[(&path, &base, &a)]),
            branch(&long, &[(&path, &base, &b)]),
        ]);
        let bytes = serde_json::to_vec(&findings).unwrap().len();
        assert!(
            bytes <= FINDINGS_BYTES && !findings.is_empty(),
            "{bytes} bytes"
        );
        assert_eq!(findings.len() + more as usize, MAX_SYMBOL_FINDINGS);
    }

    #[test]
    fn a_file_that_did_not_parse_completely_changes_no_definition() {
        // Its parse finds nothing defined, which is not evidence `save` went.
        let edit = branch(
            "edit",
            &[("src/s.rs", "pub fn save(x: u32) {}\n", ")\n")],
        );
        let caller = branch("caller", &[("src/i.rs", "", "fn c() { save(1); }\n")]);
        assert_eq!(compare(&[edit, caller]).0, Vec::new());
    }
}
