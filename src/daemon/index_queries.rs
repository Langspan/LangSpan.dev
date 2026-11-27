//! Source-index query answering: ranking, fitting an answer to one frame,
//! work in flight in other worktrees, briefing enrichment, and the write and
//! call checks.

use super::Registration;
use super::unix::{
    OUTLINE_MIN, OUTLINE_MIN_BYTES, OUTLINE_MIN_LINES, RECENT_WINDOW_MS, error, now_ms,
};
use crate::{
    protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION, Response},
    store::{Execution, Store},
};
use langspan_core::{Command, Outcome, State};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// How long a detailed symbol lookup may spend comparing check trees
/// with Git. What is not compared by then has no `last_check`.
const LAST_CHECK_BUDGET: Duration = Duration::from_millis(300);

/// Answer an index query. The index is derived and may lag the working
/// tree; `coverage` in the answer says by how much.
pub(super) fn graph_query(
    graph: Option<&crate::graph::Index>,
    state: &State,
    command: &Command,
) -> Result<Outcome, String> {
    let (workspace_id, limit) = match command {
        Command::GraphSymbol {
            workspace_id,
            limit,
            ..
        }
        | Command::GraphReferences {
            workspace_id,
            limit,
            ..
        }
        | Command::GraphImpact {
            workspace_id,
            limit,
            ..
        } => (workspace_id, *limit),
        _ => return Err("not an index query".into()),
    };
    let of = Registration::of(state, workspace_id).ok_or("workspace is not registered")?;
    if !(1..=crate::graph::MAX_RESULTS).contains(&limit) {
        return Err("limit must be between 1 and 200".into());
    }
    let depth = match command {
        Command::GraphImpact { depth, .. } => *depth,
        _ => 1,
    };
    if !(1..=langspan_core::MAX_IMPACT_DEPTH).contains(&depth) {
        return Err(format!(
            "depth must be between 1 and {}",
            langspan_core::MAX_IMPACT_DEPTH
        ));
    }
    let graph = graph.ok_or("the index is not available yet; try again shortly")?;
    let failed = |error: Box<dyn std::error::Error + Send + Sync>| error.to_string();
    let detail = matches!(command, Command::GraphSymbol { detail: true, .. });
    let mut ranked_total = None;
    let inputs = match command {
        Command::GraphSymbol {
            rank: true,
            session_id,
            ..
        } => {
            langspan_core::validate_rank(session_id.as_deref(), &[])
                .map_err(|error| error.message)?;
            Some(langspan_core::rank_inputs(
                state,
                workspace_id,
                session_id.as_deref(),
                &[],
                now_ms(),
            ))
        }
        Command::GraphSymbol {
            session_id: Some(_),
            ..
        } => return Err("session_id ranks a query: send it with rank set".into()),
        _ => None,
    };
    // Roadmap D7: an old name asked for with `--renamed` is looked up
    // under the newest name its recorded renames lead to.
    let mut renamed = match command {
        Command::GraphSymbol {
            name,
            renamed: true,
            ..
        } => langspan_core::renamed_to(state, workspace_id, name),
        _ => None,
    };
    let started = Instant::now();
    let ((mut symbols, mut references, levels), coverage) = graph
        .with_coverage(&of, |graph| match command {
            Command::GraphSymbol { name, .. } => {
                if let Some(hops) = renamed.as_mut() {
                    defined_again(graph, workspace_id, hops)?;
                }
                renamed = renamed.take().filter(|hops| !hops.is_empty());
                let name = renamed
                    .as_ref()
                    .and_then(|hops| hops.last())
                    .map_or(name, |hop| &hop.after_name);
                let mut symbols = match &inputs {
                    Some(inputs) => {
                        let (symbols, total) =
                            ranked_definitions(graph, inputs, workspace_id, name, limit)?;
                        ranked_total = Some(total);
                        symbols
                    }
                    None => graph.symbol(workspace_id, name, limit)?,
                };
                if detail {
                    for symbol in symbols.iter_mut().take(limit as usize) {
                        graph.add_detail(workspace_id, symbol)?;
                    }
                }
                Ok((symbols, Vec::new(), None))
            }
            Command::GraphReferences { name, .. } => Ok((
                Vec::new(),
                graph.references(workspace_id, name, None, limit)?,
                None,
            )),
            Command::GraphImpact { path, .. } => {
                let (symbols, references) = graph.impact(workspace_id, path, limit)?;
                let levels = (depth > 1)
                    .then(|| {
                        graph.impact_levels(workspace_id, path, depth, started + IMPACT_BUDGET)
                    })
                    .transpose()?;
                Ok((symbols, references, levels))
            }
            _ => Err("not a source index query".into()),
        })
        .map_err(failed)?;
    let mut truncated = symbols.len() > limit as usize
        || references.len() > limit as usize
        || ranked_total.is_some_and(|total| total > limit);
    symbols.truncate(limit as usize);
    references.truncate(limit as usize);
    if detail {
        add_notes_and_checks(state, workspace_id, &mut symbols);
    }
    if detail || inputs.is_some() {
        truncated |= fit_frame(&mut symbols, &coverage);
    }
    let levels = levels.map(|mut levels| {
        fit_levels(&mut levels, &(&symbols, &references, &coverage));
        truncated |= levels.iter().any(|level| level.truncated);
        levels
    });
    Ok(Outcome::Graph {
        symbols,
        references,
        resolution: (detail || levels.is_some()).then(|| "name".into()),
        coverage,
        truncated,
        levels,
        renamed,
    })
}

/// How long `graph impact` may spend on its levels: well inside the
/// client's 5 s call timeout, and short because requests behind it
/// (hooks among them) wait. At most 150 indexed lookups usually take
/// far less. Past it the levels so far are returned, marked truncated.
const IMPACT_BUDGET: Duration = Duration::from_millis(500);

/// Keep the leading impact files whose answer fits one response frame
/// beside `rest` (the outcome's other lists); a level losing files and
/// every level after it are marked `truncated`.
fn fit_levels(levels: &mut [langspan_core::ImpactLevel], rest: &impl serde::Serialize) {
    // The envelope, the request ID and the outcome's scalar fields.
    const ROOM: usize = 1024;
    // A level's own fields, beside its files.
    const LEVEL: usize = 64;
    fn size(value: &impl serde::Serialize) -> usize {
        serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
    }
    let mut used = ROOM.saturating_add(size(rest));
    let mut cut = false;
    for level in levels {
        used = used.saturating_add(LEVEL);
        let keep = if cut {
            0
        } else {
            level
                .files
                .iter()
                .take_while(|file| {
                    used = used.saturating_add(size(file) + 1);
                    used < MAX_FRAME_BYTES
                })
                .count()
        };
        if keep < level.files.len() {
            level.files.truncate(keep);
            level.truncated = true;
            cut = true;
        }
    }
}

/// Cut `hops` at the first rename whose old name the index defines
/// again, exported and of the same kind (roadmap D7). A file's first
/// indexing tells no addition, so a name a new file brings back is
/// found here, not from the notices.
fn defined_again(
    graph: &crate::graph::Index,
    workspace_id: &str,
    hops: &mut Vec<langspan_core::Rename>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for (at, hop) in hops.iter().enumerate() {
        let defined = graph
            .symbol(workspace_id, &hop.name, crate::graph::MAX_RESULTS)?
            .iter()
            .any(|symbol| symbol.exported && symbol.kind == hop.kind);
        if defined {
            hops.truncate(at);
            break;
        }
    }
    Ok(())
}

/// What the coordinator, not the index, knows about each definition's
/// file: notes that name it, and the newest conclusive check record
/// whose tree holds its indexed content.
fn add_notes_and_checks(
    state: &State,
    workspace_id: &str,
    symbols: &mut [langspan_core::SymbolRef],
) {
    let notes = state.notes.get(workspace_id).map(Vec::as_slice);
    let files: std::collections::BTreeMap<String, String> = symbols
        .iter()
        .map(|symbol| (symbol.path.clone(), symbol.digest.clone()))
        .collect();
    let checked = match (
        state.workspaces.get(workspace_id),
        state.checks.get(workspace_id),
    ) {
        (Some(workspace), Some(records)) => crate::graph::last_checks(
            Path::new(&workspace.root),
            &files,
            records,
            Instant::now() + LAST_CHECK_BUDGET,
        ),
        _ => Default::default(),
    };
    for symbol in symbols {
        symbol.notes = Some(
            notes
                .unwrap_or_default()
                .iter()
                // A line has no room for a state: an anchored note is
                // listed only with its own definition, found by its
                // anchor rather than the note's paths, which may be
                // empty (review comment 4713922971), and only while
                // that is unchanged (roadmap E1).
                .filter(|note| match (&note.anchor, note.anchor_state) {
                    (None, _) => note.paths.contains(&symbol.path),
                    (Some(anchor), state) => {
                        state == Some(langspan_core::AnchorState::Current)
                            && anchor.name == symbol.name
                            && anchor.path.as_ref() == Some(&symbol.path)
                            && anchor.kind.as_ref().is_none_or(|kind| kind == &symbol.kind)
                            && anchor.body_digest.is_some()
                            && anchor.body_digest == symbol.body_digest
                    }
                })
                .take(langspan_core::MAX_SYMBOL_NOTES)
                .map(|note| langspan_core::SymbolNote {
                    id: note.note_id.clone(),
                    text: note.text.clone(),
                })
                .collect(),
        );
        symbol.last_check = checked.get(&symbol.path).cloned();
    }
}

/// Keep the leading definitions whose answer fits one response frame.
/// Returns whether any was left out.
fn fit_frame(
    symbols: &mut Vec<langspan_core::SymbolRef>,
    coverage: &langspan_core::GraphCoverage,
) -> bool {
    // The rest of the response: its envelope, the request ID and the
    // outcome's other fields.
    const ROOM: usize = 1024;
    fn size(value: &impl serde::Serialize) -> usize {
        serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
    }
    let mut used = ROOM + size(coverage);
    let keep = symbols
        .iter()
        .take_while(|symbol| {
            used = used.saturating_add(size(*symbol) + 1);
            used < MAX_FRAME_BYTES
        })
        .count();
    let cut = keep < symbols.len();
    symbols.truncate(keep);
    cut
}

/// Where the index is blind (roadmap D8): read-only, from one snapshot
/// of the index, with the last scan failure if one is recorded.
pub(super) fn graph_blind_spots(
    graph: Option<&crate::graph::Index>,
    state: &State,
    workspace_id: &str,
    limit: u32,
) -> Result<Outcome, String> {
    let of = Registration::of(state, workspace_id).ok_or("workspace is not registered")?;
    if !(1..=crate::graph::MAX_RESULTS).contains(&limit) {
        return Err("limit must be between 1 and 200".into());
    }
    let graph = graph.ok_or("the index is not available yet; try again shortly")?;
    let (mut blind_spots, coverage) = graph
        .with_coverage(&of, |graph| graph.blind_spots(workspace_id, limit))
        .map_err(|error| error.to_string())?;
    let scan_failure = state.scan_failures.get(workspace_id).cloned();
    let totals = &blind_spots.totals;
    let mut truncated = [
        (blind_spots.partial.len(), totals.partial),
        (blind_spots.never_clean.len(), totals.never_clean),
        (blind_spots.waiting.len(), totals.waiting),
        (blind_spots.too_large.len(), totals.too_large),
        (blind_spots.unscanned.len(), totals.unscanned),
    ]
    .iter()
    .any(|(listed, total)| *listed < *total as usize);
    truncated |= fit_blind_spots(&mut blind_spots, &coverage, scan_failure.as_ref());
    Ok(Outcome::GraphBlindSpots {
        blind_spots,
        coverage,
        scan_failure,
        truncated,
    })
}

/// Long paths can make five lists of 200 exceed one response frame. Keep
/// the longest prefix, the same length for every list, that fits, so a
/// trimmed answer is still the same on every call. Returns whether any
/// path was left out.
fn fit_blind_spots(
    spots: &mut langspan_core::BlindSpots,
    coverage: &langspan_core::GraphCoverage,
    failure: Option<&langspan_core::ScanFailure>,
) -> bool {
    const ROOM: usize = 1024;
    let size = |spots: &langspan_core::BlindSpots| {
        serde_json::to_vec(&(spots, coverage, failure)).map_or(usize::MAX, |bytes| bytes.len())
    };
    if size(spots) + ROOM < MAX_FRAME_BYTES {
        return false;
    }
    let cut = |spots: &langspan_core::BlindSpots, keep: usize| {
        let mut spots = spots.clone();
        spots.partial.truncate(keep);
        spots.never_clean.truncate(keep);
        spots.waiting.truncate(keep);
        spots.too_large.truncate(keep);
        spots.unscanned.truncate(keep);
        spots
    };
    // The largest prefix that fits; zero always does.
    let (mut low, mut high) = (0, crate::graph::MAX_RESULTS as usize);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if size(&cut(spots, middle)) + ROOM < MAX_FRAME_BYTES {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    *spots = cut(spots, low);
    true
}

/// Definitions and mentioning files for names taken from a prompt. The
/// names are used for this query only: never logged, recorded or kept.
pub(super) fn graph_context(
    graph: Option<&crate::graph::Index>,
    state: &State,
    command: &Command,
) -> Result<Outcome, (&'static str, String)> {
    let Command::GraphContext {
        workspace_id,
        names,
        limit,
        rank,
        session_id,
        qualifiers,
    } = command
    else {
        return Err(("invalid_graph_context", "not a code-context query".into()));
    };
    let limit = *limit;
    langspan_core::validate_graph_context(names, limit)
        .map_err(|error| ("invalid_graph_context", error.message))?;
    langspan_core::validate_rank(session_id.as_deref(), qualifiers)
        .map_err(|error| ("invalid_graph_context", error.message))?;
    if !rank && (session_id.is_some() || !qualifiers.is_empty()) {
        return Err((
            "invalid_graph_context",
            "session_id and qualifiers rank a query: send them with rank set".into(),
        ));
    }
    let inputs = rank.then(|| {
        langspan_core::rank_inputs(
            state,
            workspace_id,
            session_id.as_deref(),
            qualifiers,
            now_ms(),
        )
    });
    let unavailable = |message: String| ("index_unavailable", message);
    let of = Registration::of(state, workspace_id)
        .ok_or_else(|| unavailable("workspace is not registered".into()))?;
    let graph = graph
        .ok_or_else(|| unavailable("the index is not available yet; try again shortly".into()))?;
    let failed = |error: Box<dyn std::error::Error + Send + Sync>| unavailable(error.to_string());
    let (mut matches, coverage) = graph
        .with_coverage(&of, |graph| {
            let mut matches = Vec::new();
            for name in names {
                if let Some(inputs) = &inputs {
                    let (definitions, total) =
                        ranked_definitions(graph, inputs, workspace_id, name, limit)?;
                    if definitions.is_empty() {
                        continue;
                    }
                    let files = langspan_core::CONTEXT_FILES;
                    let mut mentioned_in = graph.mentioned_in(workspace_id, name, files)?;
                    let truncated = total > limit || mentioned_in.len() > files as usize;
                    mentioned_in.truncate(files as usize);
                    matches.push(langspan_core::NameContext {
                        name: name.clone(),
                        definitions,
                        mentioned_in,
                        truncated,
                        definition_count: Some(total),
                    });
                    continue;
                }
                let mut definitions = graph.symbol(workspace_id, name, limit)?;
                if definitions.is_empty() {
                    continue;
                }
                let files = langspan_core::CONTEXT_FILES;
                let mut mentioned_in = graph.mentioned_in(workspace_id, name, files)?;
                let truncated =
                    definitions.len() > limit as usize || mentioned_in.len() > files as usize;
                definitions.truncate(limit as usize);
                mentioned_in.truncate(files as usize);
                matches.push(langspan_core::NameContext {
                    name: name.clone(),
                    definitions,
                    mentioned_in,
                    truncated,
                    definition_count: None,
                });
            }
            Ok(matches)
        })
        .map_err(failed)?;
    fit_context(&mut matches, &coverage);
    Ok(Outcome::GraphContext { matches, coverage })
}

/// Cut a code-context answer until it fits one response frame (review
/// comment 4709803050): first the dependents under every definition,
/// one fewer at a time; then the mentioning files of every name; then
/// every name's lowest-ranked definitions down to one each; then the
/// last names. Counts stay as they were, so what is left out is still
/// counted, and a name that loses a definition or a file is marked
/// `truncated`. Returns whether anything was cut.
fn fit_context(
    matches: &mut Vec<langspan_core::NameContext>,
    coverage: &langspan_core::GraphCoverage,
) -> bool {
    // The envelope, the request ID and the outcome's other fields.
    const ROOM: usize = 1024;
    let fits = |matches: &Vec<langspan_core::NameContext>| {
        serde_json::to_vec(&(matches, coverage))
            .is_ok_and(|bytes| bytes.len() + ROOM < MAX_FRAME_BYTES)
    };
    if fits(matches) {
        return false;
    }
    fn most(
        matches: &[langspan_core::NameContext],
        count: fn(&langspan_core::NameContext) -> usize,
    ) -> usize {
        matches.iter().map(count).max().unwrap_or(0)
    }
    let dependents = most(matches, |matched| {
        let lengths = matched.definitions.iter();
        lengths
            .map(|symbol| symbol.dependents.as_ref().map_or(0, Vec::len))
            .max()
            .unwrap_or(0)
    });
    for keep in (0..dependents).rev() {
        for symbol in matches.iter_mut().flat_map(|m| &mut m.definitions) {
            if let Some(dependents) = &mut symbol.dependents {
                dependents.truncate(keep);
            }
        }
        if fits(matches) {
            return true;
        }
    }
    for keep in (0..most(matches, |matched| matched.mentioned_in.len())).rev() {
        for matched in matches.iter_mut() {
            if matched.mentioned_in.len() > keep {
                matched.mentioned_in.truncate(keep);
                matched.truncated = true;
            }
        }
        if fits(matches) {
            return true;
        }
    }
    for keep in (1..most(matches, |matched| matched.definitions.len())).rev() {
        for matched in matches.iter_mut() {
            if matched.definitions.len() > keep {
                matched.definitions.truncate(keep);
                matched.truncated = true;
            }
        }
        if fits(matches) {
            return true;
        }
    }
    while !matches.is_empty() && !fits(matches) {
        matches.pop();
    }
    true
}

/// The first `limit` definitions of `name` in rank order (roadmap C2),
/// each with its reasons and its ranked dependents, and how many
/// definitions the name has. Ranked among the first `RANK_CANDIDATES`
/// in path order and the first `RANK_CANDIDATES` in rank order in the
/// files the session or a peer works on; the rest are counted.
fn ranked_definitions(
    graph: &crate::graph::Index,
    inputs: &langspan_core::RankInputs,
    workspace_id: &str,
    name: &str,
    limit: u32,
) -> Result<(Vec<langspan_core::SymbolRef>, u32), Box<dyn std::error::Error + Send + Sync>> {
    let paths = inputs.paths();
    let (mut definitions, total) = graph.rank_candidates(workspace_id, name, inputs)?;
    if definitions.is_empty() {
        return Ok((definitions, total));
    }
    langspan_core::rank_definitions(inputs, &mut definitions);
    definitions.truncate(limit as usize);
    // The shown definitions' own files are read too, so each one's
    // count of other files is exact.
    let mut asked = paths;
    asked.extend(definitions.iter().map(|symbol| symbol.path.clone()));
    asked.sort_unstable();
    asked.dedup();
    let (files, mentioning) = graph.mention_counts(workspace_id, name, &asked)?;
    for symbol in &mut definitions {
        let own = files.iter().any(|(path, _)| *path == symbol.path);
        let others: Vec<(String, u32)> = files
            .iter()
            .filter(|(path, _)| *path != symbol.path)
            .cloned()
            .collect();
        let mut dependents = langspan_core::rank_dependents(inputs, &others);
        dependents.truncate(langspan_core::CONTEXT_FILES as usize);
        symbol.dependents = Some(dependents);
        symbol.dependent_count = Some(mentioning.saturating_sub(u32::from(own)));
    }
    Ok((definitions, total))
}

/// In-flight symbols `graph inflight` reads from one worktree's index,
/// at most.
const INFLIGHT_PER_WORKTREE: usize = 128;
/// In-flight symbols compared with the asking workspace's files, at most:
/// for `graph inflight` across worktrees, and for one briefing.
const INFLIGHT_OFFERED: usize = 256;

/// A place in the in-flight rows of a repository's worktrees, ordered by
/// worktree workspace and then as `Index::inflight` orders a worktree's.
type InflightAt = (String, crate::graph::InflightRow);

/// Per session, the last in-flight row its briefings went past without
/// finding anything to offer before it (review comment 4704678374).
/// Each briefing reads at most `INFLIGHT_OFFERED` rows from there, and
/// from the first row again after the last, so every row is read in
/// turn, however many there are. Only the request thread uses it. A
/// restart loses it, which costs a briefing or two, never a notice: what
/// a session was told is recorded in the store.
static BRIEF_WINDOWS: std::sync::Mutex<std::collections::BTreeMap<String, InflightAt>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The other worktrees of `workspace_id`'s repository whose index has a
/// complete scan, in workspace order, with their registrations.
fn inflight_worktrees<'a>(state: &'a State, workspace_id: &str) -> Vec<(&'a String, Registration)> {
    let Some(git) = state.workspace_git.get(workspace_id) else {
        return Vec::new();
    };
    state
        .workspace_git
        .iter()
        .filter(|(other, other_git)| {
            *other != workspace_id && other_git.repository_id == git.repository_id
        })
        .filter_map(|(other, _)| Some((other, Registration::of(state, other)?)))
        .collect()
}

/// After a scan of `workspace_id`: its files that the index finds
/// mention a name changed by a held notice of another worktree of its
/// repository are added to that notice's `elsewhere` (issue #194). Only
/// notices of this generation within the briefing horizon are read, and
/// the notice's own file is left out, as in its own worktree. The index
/// lags the scan by a pass, so a file is found at a later scan than the
/// one that saw it change. Nothing is found while the workspace's index
/// has no complete scan; a failure is logged and the next scan tries
/// again.
pub(super) fn observe_elsewhere(
    store: &mut Store,
    graph: Option<&crate::graph::Index>,
    workspace_id: &str,
    now: u64,
) {
    let state = store.state();
    let (Some(graph), Some(of), Some(git)) = (
        graph,
        Registration::of(state, workspace_id),
        state.workspace_git.get(workspace_id),
    ) else {
        return;
    };
    let wanted: Vec<&langspan_core::InterfaceNotice> = state
        .interface_notices
        .iter()
        .filter(|(other, _)| {
            *other != workspace_id
                && state
                    .workspace_git
                    .get(*other)
                    .is_some_and(|other| other.repository_id == git.repository_id)
        })
        .flat_map(|(_, notices)| notices)
        .filter(|notice| {
            notice.generation == state.generation
                && now.saturating_sub(notice.at_ms) <= langspan_core::BRIEF_HORIZON_MS
                && notice.elsewhere.len() < langspan_core::NOTICE_DEPENDENTS
        })
        .collect();
    if wanted.is_empty() {
        return;
    }
    let looked_up = graph.with_coverage(&of, |graph| {
        let mut found = Vec::new();
        for notice in &wanted {
            let names: Vec<&str> = notice
                .changes
                .iter()
                .filter(|change| change.change != "added")
                .map(|change| change.name.as_str())
                .collect();
            let paths: Vec<String> = graph
                .dependents(workspace_id, Some(&notice.path), &names)?
                .into_iter()
                .filter(|path| {
                    !notice
                        .elsewhere
                        .iter()
                        .any(|file| file.workspace_id == workspace_id && file.path == *path)
                })
                .collect();
            if !paths.is_empty() {
                found.push(Command::ObserveInterfaceDependents {
                    notice_workspace_id: notice.workspace_id.clone(),
                    notice_id: notice.notice_id.clone(),
                    workspace_id: workspace_id.to_owned(),
                    paths,
                });
            }
        }
        Ok(found)
    });
    let commands = match looked_up {
        Ok((commands, coverage)) if coverage.scanned => commands,
        Ok(_) => return,
        Err(failure) => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"warn","event":"elsewhere_dependents_unavailable","workspace_id":workspace_id,"detail":failure.to_string()})
            );
            return;
        }
    };
    for command in commands {
        let Command::ObserveInterfaceDependents {
            notice_id, paths, ..
        } = &command
        else {
            continue;
        };
        let mut parts = vec![notice_id.as_str(), workspace_id];
        parts.extend(paths.iter().map(String::as_str));
        let id = crate::hook::wire_id("interface-elsewhere", &parts);
        let failure = match store.execute(&id, &command, &command, now) {
            Ok(Execution {
                outcome: Outcome::Error { message, .. },
                ..
            }) => Some(message),
            Ok(_) => None,
            Err(failure) => Some(failure.to_string()),
        };
        if let Some(detail) = failure {
            eprintln!(
                "{}",
                serde_json::json!({"level":"warn","event":"elsewhere_dependents_not_recorded","workspace_id":workspace_id,"detail":detail})
            );
        }
    }
}

/// Up to `budget` in-flight rows of other worktrees after `after`, in
/// order, whether they run to the last row, and whether a worktree it
/// came to had no complete scan, so its rows could not be read.
#[allow(clippy::type_complexity)]
fn inflight_after(
    graph: &crate::graph::Index,
    state: &State,
    workspace_id: &str,
    after: Option<&InflightAt>,
    budget: usize,
) -> Result<(Vec<InflightAt>, bool, bool), Box<dyn std::error::Error + Send + Sync>> {
    let mut rows = Vec::new();
    let mut unscanned = false;
    for (other, at) in inflight_worktrees(state, workspace_id) {
        if after.is_some_and(|(from, _)| other < from) {
            continue;
        }
        let from = after.filter(|(from, _)| from == other).map(|(_, row)| row);
        let left = budget - rows.len();
        // One more than is left, to tell the last row from a cut.
        let (read, coverage) =
            graph.with_coverage(&at, |graph| graph.inflight_after(other, from, left + 1))?;
        if !coverage.scanned {
            unscanned = true;
            continue;
        }
        let cut = read.len() > left;
        rows.extend(read.into_iter().take(left).map(|row| (other.clone(), row)));
        if cut {
            return Ok((rows, false, unscanned));
        }
    }
    Ok((rows, true, unscanned))
}

/// Which of `offered` the asking workspace's files mention: the files in
/// `among` for a briefing, any file for `graph inflight`. Each found
/// item comes with its place in `offered`. `keep` decides which count
/// toward `limit`; the flag says more were found. The coverage is the
/// asking workspace's, and an asking workspace without a complete scan
/// finds nothing.
#[allow(clippy::type_complexity)]
fn inflight_mentioned(
    graph: &crate::graph::Index,
    of: &Registration,
    workspace_id: &str,
    among: Option<&std::collections::BTreeSet<String>>,
    offered: Vec<InflightAt>,
    limit: usize,
    keep: impl Fn(&langspan_core::InflightSymbol) -> bool,
) -> Result<
    (
        Vec<(usize, langspan_core::InflightSymbol)>,
        langspan_core::GraphCoverage,
        bool,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    if offered.is_empty() {
        // Nothing to look up, but the answer still says whether the
        // asking workspace is indexed.
        let ((), coverage) = graph.with_coverage(of, |_| Ok(()))?;
        return Ok((Vec::new(), coverage, false));
    }
    let ((found, more), coverage) = graph.with_coverage(of, |graph| {
        let imports = graph.imports_of(workspace_id, among)?;
        let mut found = Vec::new();
        for (place, (other, (path, name, kind, signature, renamed_from))) in
            offered.into_iter().enumerate()
        {
            // A rename (roadmap D7) is told to files that still use the
            // old name as well as to those that use the new one.
            let mut mentioned_in = std::collections::BTreeSet::new();
            for name in std::iter::once(&name).chain(renamed_from.as_ref()) {
                mentioned_in.extend(graph.mentioning(
                    workspace_id,
                    (&path, name),
                    &imports,
                    among,
                    langspan_core::INFLIGHT_MENTIONS,
                )?);
            }
            let mentioned_in: Vec<String> = mentioned_in
                .into_iter()
                .take(langspan_core::INFLIGHT_MENTIONS)
                .collect();
            if mentioned_in.is_empty() {
                continue;
            }
            let item = langspan_core::InflightSymbol {
                workspace_id: other,
                path,
                name,
                kind,
                signature,
                mentioned_in,
                renamed_from,
            };
            if !keep(&item) {
                continue;
            }
            if found.len() == limit {
                return Ok((found, true));
            }
            found.push((place, item));
        }
        Ok((found, false))
    })?;
    if !coverage.scanned {
        return Ok((Vec::new(), coverage, false));
    }
    Ok((found, coverage, more))
}

/// What other worktrees have in flight (roadmap C7) that the session's
/// files mention and it was not told, for its briefing: from the rows
/// after its window's place, at most `INFLIGHT_OFFERED` of them, wrapping
/// to the first row after the last. The window then starts just before
/// the first item offered, or after the last row read when none was, so
/// an item not told this time is read again first, and rows past it are
/// reached once what precedes them is told or not mentioned.
///
/// The flag says the index had no complete scan of the session's
/// workspace or of another worktree it came to, so what is in flight
/// there could not be compared (issue #262): the briefing says so
/// rather than nothing.
#[allow(clippy::type_complexity)]
fn brief_inflight(
    graph: &crate::graph::Index,
    state: &State,
    session_id: &str,
    workspace_id: &str,
    concerned: &std::collections::BTreeSet<String>,
) -> Result<(Vec<langspan_core::InflightSymbol>, bool), Box<dyn std::error::Error + Send + Sync>> {
    let Some(of) = Registration::of(state, workspace_id) else {
        return Ok((Vec::new(), false));
    };
    // With no other worktree there is nothing in flight to miss.
    if inflight_worktrees(state, workspace_id).is_empty() {
        return Ok((Vec::new(), false));
    }
    let mut windows = BRIEF_WINDOWS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    windows.retain(|session, _| state.sessions.contains_key(session));
    let from = windows.get(session_id).cloned();
    let (mut rows, end, mut unscanned) =
        inflight_after(graph, state, workspace_id, from.as_ref(), INFLIGHT_OFFERED)?;
    if let Some(from) = from.as_ref().filter(|_| end) {
        let (first, _, before) = inflight_after(
            graph,
            state,
            workspace_id,
            None,
            INFLIGHT_OFFERED - rows.len(),
        )?;
        unscanned |= before;
        rows.extend(first.into_iter().take_while(|row| row <= from));
    }
    let place = |at: usize| at.checked_sub(1).map(|at| rows[at].clone());
    let last = rows.last().cloned();
    let read = rows.len();
    let (found, coverage, _) = inflight_mentioned(
        graph,
        &of,
        workspace_id,
        Some(concerned),
        rows.clone(),
        langspan_core::INFLIGHT_CANDIDATES,
        |item| !langspan_core::inflight_told(state, session_id, item),
    )?;
    let next = match found.first() {
        Some((0, _)) => from,
        Some((first, _)) => place(*first),
        None if read == 0 => None,
        None => last,
    };
    match next {
        Some(next) => windows.insert(session_id.to_owned(), next),
        None => windows.remove(session_id),
    };
    Ok((
        found.into_iter().map(|(_, item)| item).collect(),
        unscanned || !coverage.scanned,
    ))
}

/// `Brief` with what the index finds in flight in other worktrees for
/// the session's own files. An index that cannot be read leaves it out.
pub(super) fn prepare_brief(
    state: &State,
    graph: Option<&crate::graph::Index>,
    command: &Command,
) -> Command {
    let Command::Brief {
        session_id,
        max_peers,
        deliver,
        mid_turn,
        left,
        recent,
        recent_window_ms,
        mcp,
        changes_only,
    } = command
    else {
        return command.clone();
    };
    // The window is the daemon's configuration, whatever a client
    // sent (roadmap C3).
    let recent_window_ms = if *recent {
        RECENT_WINDOW_MS.get().copied()
    } else {
        *recent_window_ms
    };
    let (inflight, inflight_partial) = graph
            .zip(langspan_core::briefing_concern(state, session_id, now_ms()))
            .filter(|(_, (_, concerned))| !concerned.is_empty())
            .and_then(|(graph, (workspace_id, concerned))| {
                brief_inflight(graph, state, session_id, &workspace_id, &concerned)
                .inspect_err(|error| {
                    eprintln!(
                        "{}",
                        serde_json::json!({"level":"warn","event":"inflight_unavailable","detail":error.to_string()})
                    );
                })
                .ok()
            })
            .unwrap_or_default();
    let mentioned = graph
            .zip(langspan_core::briefing_concern(state, session_id, now_ms()))
            .filter(|(_, (_, concerned))| !concerned.is_empty())
            .and_then(|(graph, (workspace_id, concerned))| {
                mentioned_anchors(graph, state, &workspace_id, &concerned)
                    .inspect_err(|error| {
                        eprintln!(
                            "{}",
                            serde_json::json!({"level":"warn","event":"note_mentions_unavailable","detail":error.to_string()})
                        );
                    })
                    .ok()
            })
            .unwrap_or_default();
    Command::BriefPrepared {
        session_id: session_id.clone(),
        max_peers: *max_peers,
        deliver: *deliver,
        mid_turn: *mid_turn,
        recent: *recent,
        recent_window_ms,
        mcp: *mcp,
        left: left.clone(),
        changes_only: *changes_only,
        inflight,
        inflight_partial,
        mentioned,
    }
}

/// The names of the workspace's anchored notes that one of `concerned`
/// mentions, as the index finds them (roadmap E1). A name match, as
/// `langspan_references`. Empty without a scan of the current
/// registration; at most `MAX_NOTES` names are looked up.
fn mentioned_anchors(
    graph: &crate::graph::Index,
    state: &State,
    workspace_id: &str,
    concerned: &std::collections::BTreeSet<String>,
) -> Result<std::collections::BTreeSet<String>, Box<dyn std::error::Error + Send + Sync>> {
    let lookups = langspan_core::anchored(state, workspace_id);
    let Some(of) = Registration::of(state, workspace_id).filter(|_| !lookups.is_empty()) else {
        return Ok(Default::default());
    };
    let (found, _) = graph.with_coverage(&of, |graph| {
        let mut found = std::collections::BTreeSet::new();
        for name in lookups.keys() {
            if !graph
                .mentioning(workspace_id, ("", name), &[], Some(concerned), 1)?
                .is_empty()
            {
                found.insert(name.clone());
            }
        }
        Ok(found)
    })?;
    Ok(found)
}

/// What the index holds of anchored names where `lookups` says to look,
/// from one snapshot (roadmap E1). Each named file is read in full
/// before the capped workspace lookup, so a name defined many times
/// elsewhere never hides a file's own definition (review comment
/// 4713922965). `None` without a scan of the workspace's current
/// registration.
fn anchor_index(
    graph: Option<&crate::graph::Index>,
    state: &State,
    workspace_id: &str,
    lookups: &std::collections::BTreeMap<String, langspan_core::AnchorLookup>,
) -> Result<Option<langspan_core::AnchorIndex>, Box<dyn std::error::Error + Send + Sync>> {
    let (Some(graph), Some(of)) = (graph, Registration::of(state, workspace_id)) else {
        return Ok(None);
    };
    let (found, coverage) = graph.with_coverage(&of, |graph| {
        let mut found = langspan_core::AnchorIndex::default();
        let mut seen = std::collections::BTreeSet::new();
        let mut asked = std::collections::BTreeSet::new();
        for (name, lookup) in lookups {
            let mut rows = Vec::new();
            for path in &lookup.paths {
                // A file holds at most `MAX_SYMBOLS` definitions.
                rows.extend(graph.anchor_definitions(
                    workspace_id,
                    Some(path),
                    name,
                    langspan_graph::MAX_SYMBOLS as u32,
                )?);
                asked.insert(path.clone());
            }
            if lookup.workspace {
                let mut wide = graph.anchor_definitions(
                    workspace_id,
                    None,
                    name,
                    crate::graph::MAX_RESULTS,
                )?;
                if wide.len() > crate::graph::MAX_RESULTS as usize {
                    wide.truncate(crate::graph::MAX_RESULTS as usize);
                    found.truncated.insert(name.clone());
                }
                rows.extend(wide);
            }
            for symbol in rows {
                // A file's definition found by both lookups is one.
                let key = (
                    symbol.path.clone(),
                    symbol.line,
                    symbol.end_line,
                    symbol.kind.clone(),
                    symbol.name.clone(),
                );
                if seen.insert(key) {
                    asked.insert(symbol.path.clone());
                    found.definitions.push(symbol);
                }
            }
        }
        found.fresh = graph.fresh(workspace_id, &asked)?;
        found.absent = graph.absent(workspace_id, &asked)?;
        Ok(found)
    })?;
    Ok(coverage.scanned.then_some(langspan_core::AnchorIndex {
        complete: coverage.blind_in_scope == 0,
        ..found
    }))
}

/// `Remember` with what the index holds of its symbol, and
/// `ObserveNoteAnchors` with what it holds of every anchored note
/// (roadmap E1). A recheck without a complete scan is answered as one
/// that changed nothing.
pub(super) fn prepare_anchors(
    state: &State,
    graph: Option<&crate::graph::Index>,
    command: &Command,
) -> Result<Command, (&'static str, String)> {
    let unavailable =
        |error: Box<dyn std::error::Error + Send + Sync>| ("index_unavailable", error.to_string());
    match command {
        Command::Remember {
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
        } => {
            let index = match symbol {
                Some(name) => {
                    let named: Vec<&str> = paths.iter().map(String::as_str).collect();
                    anchor_index(
                        graph,
                        state,
                        workspace_id,
                        &langspan_core::remember_lookups(name, &named),
                    )
                    .map_err(unavailable)?
                }
                None => None,
            };
            Ok(Command::RememberPrepared {
                workspace_id: workspace_id.clone(),
                author: author.clone(),
                text: text.clone(),
                paths: paths.clone(),
                symbol: symbol.clone(),
                tags: tags.clone(),
                index,
            })
        }
        Command::ObserveNoteAnchors { workspace_id } => {
            let lookups = langspan_core::anchored(state, workspace_id);
            if lookups.is_empty() {
                return Ok(command.clone());
            }
            match anchor_index(graph, state, workspace_id, &lookups).map_err(unavailable)? {
                Some(index) => Ok(Command::NoteAnchorsPrepared {
                    workspace_id: workspace_id.clone(),
                    index,
                }),
                None => Ok(command.clone()),
            }
        }
        _ => Ok(command.clone()),
    }
}

/// What `graph inflight` compares: the first `INFLIGHT_PER_WORKTREE`
/// rows of each other worktree, at most `INFLIGHT_OFFERED` in all. The
/// flag says a limit left rows unread or more were found than are
/// listed, so the comparison was partial (review comment 4704149471).
#[allow(clippy::type_complexity)]
fn listed_inflight(
    graph: &crate::graph::Index,
    state: &State,
    workspace_id: &str,
) -> Result<
    (
        Vec<(usize, langspan_core::InflightSymbol)>,
        langspan_core::GraphCoverage,
        bool,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let Some(of) = Registration::of(state, workspace_id) else {
        return Ok((Vec::new(), langspan_core::GraphCoverage::default(), false));
    };
    let mut offered = Vec::new();
    // One more row than the limit is read to tell a full worktree from
    // a cut one.
    let mut capped = false;
    for (other, at) in inflight_worktrees(state, workspace_id) {
        let (mut rows, coverage) = graph.with_coverage(&at, |graph| {
            graph.inflight(other, INFLIGHT_PER_WORKTREE + 1)
        })?;
        if !coverage.scanned {
            continue;
        }
        if rows.len() > INFLIGHT_PER_WORKTREE {
            rows.truncate(INFLIGHT_PER_WORKTREE);
            capped = true;
        }
        offered.extend(rows.into_iter().map(|row| (other.clone(), row)));
        if offered.len() > INFLIGHT_OFFERED {
            offered.truncate(INFLIGHT_OFFERED);
            capped = true;
            break;
        }
    }
    let (found, coverage, more) = inflight_mentioned(
        graph,
        &of,
        workspace_id,
        None,
        offered,
        crate::graph::MAX_RESULTS as usize,
        |_| true,
    )?;
    let partial = coverage.scanned && (more || capped);
    Ok((found, coverage, partial))
}

/// `graph inflight`: what other worktrees have in flight that any of
/// the workspace's files mention.
pub(super) fn graph_inflight(
    graph: Option<&crate::graph::Index>,
    state: &State,
    workspace_id: &str,
) -> Result<Outcome, String> {
    if !state.workspaces.contains_key(workspace_id) {
        return Err("workspace is not registered".into());
    }
    let graph = graph.ok_or("the index is not available yet; try again shortly")?;
    let (found, coverage, truncated) =
        listed_inflight(graph, state, workspace_id).map_err(|error| error.to_string())?;
    let mut inflight: Vec<langspan_core::InflightNotice> = found
        .iter()
        .filter_map(|(_, item)| langspan_core::inflight_notice(state, item))
        .collect();
    let cut = fit_items(&mut inflight, &coverage);
    Ok(Outcome::GraphInflight {
        inflight,
        coverage,
        truncated: truncated || cut,
    })
}

/// Keep the leading `items` whose answer fits one response frame beside
/// `rest` (the outcome's other lists and its coverage). Returns whether
/// any was left out.
fn fit_items<T: serde::Serialize>(items: &mut Vec<T>, rest: &impl serde::Serialize) -> bool {
    // The envelope, the request ID and the outcome's other fields.
    const ROOM: usize = 1024;
    fn size(value: &impl serde::Serialize) -> usize {
        serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
    }
    let mut used = ROOM + size(rest);
    let keep = items
        .iter()
        .take_while(|item| {
            used = used.saturating_add(size(*item) + 1);
            used < MAX_FRAME_BYTES
        })
        .count();
    let cut = keep < items.len();
    items.truncate(keep);
    cut
}

/// How long `graph path` may search: as `graph impact`'s levels, well
/// inside the client's call timeout and short because requests behind
/// it wait. Past it the chains found so far are returned, truncated.
const PATH_BUDGET: Duration = Duration::from_millis(500);

/// `graph path`: the shortest chains of calls between two names
/// (roadmap D3).
pub(super) fn graph_path(
    graph: Option<&crate::graph::Index>,
    state: &State,
    (workspace_id, from, to, max_hops): (&str, &str, &str, u32),
) -> Result<Outcome, String> {
    let of = Registration::of(state, workspace_id).ok_or("workspace is not registered")?;
    if !(1..=langspan_core::MAX_PATH_HOPS).contains(&max_hops) {
        return Err(format!(
            "max_hops must be between 1 and {}",
            langspan_core::MAX_PATH_HOPS
        ));
    }
    for name in [from, to] {
        let recordable = (1..=langspan_graph::NAME_BYTES).contains(&name.len())
            && !name.chars().any(|c| c.is_whitespace() || c.is_control());
        if !recordable {
            return Err(format!(
                "{name:?} is not a name the index records: 1 to 128 bytes without whitespace or control characters"
            ));
        }
    }
    let graph = graph.ok_or("the index is not available yet; try again shortly")?;
    let deadline = Instant::now() + PATH_BUDGET;
    let ((mut paths, edges_visited, truncated), coverage) = graph
        .with_coverage(&of, |graph| {
            graph.symbol_paths(
                workspace_id,
                (from, to),
                max_hops,
                langspan_core::MAX_PATH_EDGES,
                deadline,
            )
        })
        .map_err(|error| error.to_string())?;
    let cut = fit_items(&mut paths, &coverage);
    Ok(Outcome::GraphPath {
        from: from.into(),
        to: to.into(),
        max_hops,
        paths,
        edges_visited,
        coverage,
        truncated: truncated || cut,
    })
}

/// `graph map`: the workspace's areas and the edges between them
/// (roadmap D3).
pub(super) fn graph_map(
    graph: Option<&crate::graph::Index>,
    state: &State,
    workspace_id: &str,
    depth: u32,
) -> Result<Outcome, String> {
    let of = Registration::of(state, workspace_id).ok_or("workspace is not registered")?;
    if !(1..=langspan_core::MAX_MAP_DEPTH).contains(&depth) {
        return Err(format!(
            "depth must be between 1 and {}",
            langspan_core::MAX_MAP_DEPTH
        ));
    }
    let graph = graph.ok_or("the index is not available yet; try again shortly")?;
    let ((mut areas, mut edges, truncated), coverage) = graph
        .with_coverage(&of, |graph| graph.map(workspace_id, depth))
        .map_err(|error| error.to_string())?;
    let mut cut = fit_items(&mut areas, &coverage);
    cut |= fit_items(&mut edges, &(&areas, &coverage));
    Ok(Outcome::GraphMap {
        depth,
        areas,
        edges,
        coverage,
        truncated: truncated || cut,
    })
}

/// How long one write check may spend asking Git where other worktrees'
/// changes stand (issue #217). A notice Git has not answered for by then
/// keeps the uncommitted rules.
const STANDING_BUDGET: Duration = Duration::from_millis(250);

/// Git's answer, for each of the write's `standing_questions`, to where
/// that worktree's change stands against the session's checkout. A
/// failure or the budget running out leaves a notice out, and is logged.
fn standings(
    state: &State,
    session_id: &str,
    calls: &[langspan_core::CallSite],
    now: u64,
) -> Result<langspan_core::Standings, langspan_core::CoreError> {
    let questions = langspan_core::standing_questions(state, session_id, calls, now)?;
    let mut standings = langspan_core::Standings::new();
    let base = state
        .sessions
        .get(session_id)
        .and_then(|session| state.workspaces.get(&session.workspace_id));
    let (Some(base), false) = (base, questions.is_empty()) else {
        return Ok(standings);
    };
    let deadline = Instant::now() + STANDING_BUDGET;
    for question in questions {
        let Some(changed) = state.workspaces.get(&question.workspace_id) else {
            continue;
        };
        match crate::git::standing(
            Path::new(&changed.root),
            &question.path,
            Path::new(&base.root),
            deadline,
        ) {
            Ok(standing) => {
                standings.insert((question.workspace_id, question.notice_id), standing);
            }
            Err(detail) => eprintln!(
                "{}",
                serde_json::json!({"level":"info","event":"change_standing_unknown","workspace_id":question.workspace_id,"detail":detail})
            ),
        }
    }
    Ok(standings)
}

/// The write-time break check (ADR 0029). The calls are used for this
/// answer only: never logged or recorded. What is recorded, when a
/// break is found, is which notices the session was alerted to.
pub(super) fn check_calls(
    store: &mut Store,
    graph: Option<&crate::graph::Index>,
    request_id: String,
    session_id: &str,
    calls: &[langspan_core::CallSite],
) -> Response {
    let now = now_ms();
    let generation = store.state().generation;
    let answer = |outcome| Response {
        version: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        replayed: false,
        generation,
        lease_valid: None,
        outcome,
    };
    let standings = match standings(store.state(), session_id, calls, now) {
        Ok(standings) => standings,
        Err(e) => return error(request_id, generation, &e.code, e.message),
    };
    let candidates =
        match langspan_core::call_candidates(store.state(), session_id, calls, &standings, now) {
            Ok(candidates) => candidates,
            Err(e) => return error(request_id, generation, &e.code, e.message),
        };
    let workspace_id = store.state().sessions[session_id].workspace_id.clone();
    let not_ready = |coverage: Option<langspan_core::GraphCoverage>| {
        answer(Outcome::CallBreaks {
            breaks: Vec::new(),
            coverage: coverage.unwrap_or_default(),
            ready: false,
        })
    };
    // Nothing is looked up before the index has a complete scan of the
    // workspace's current registration.
    let (Some(graph), Some(of)) = (graph, Registration::of(store.state(), &workspace_id)) else {
        return not_ready(None);
    };
    match graph.with_coverage(&of, |_| Ok(())) {
        Ok(((), coverage)) if coverage.scanned => {}
        Ok(((), coverage)) => return not_ready(Some(coverage)),
        Err(failure) => {
            return error(
                request_id,
                generation,
                "index_unavailable",
                failure.to_string(),
            );
        }
    }
    // Each workspace's new signatures and the coverage they are judged
    // with come from one snapshot of its index. A change in another
    // worktree is judged in that worktree's index (issue #194), and
    // only once it has a complete scan.
    let mut current = std::collections::BTreeMap::new();
    let mut coverage = None;
    for (notice_workspace, names) in &candidates {
        let own = *notice_workspace == workspace_id;
        let registration = if own {
            Some(of.clone())
        } else {
            Registration::of(store.state(), notice_workspace)
        };
        let Some(registration) = registration else {
            continue;
        };
        let looked_up = graph.with_coverage(&registration, |graph| {
            let mut found = Vec::new();
            for name in names {
                found.extend(graph.symbol(notice_workspace, name, crate::graph::MAX_RESULTS)?);
            }
            Ok(found)
        });
        match looked_up {
            Ok((definitions, scanned)) if scanned.scanned => {
                current.insert(
                    notice_workspace.clone(),
                    langspan_core::IndexedNames {
                        definitions,
                        // An absent name is gone only when the index
                        // reads every file.
                        complete: scanned.blind_in_scope == 0,
                    },
                );
                if own {
                    coverage = Some(scanned);
                }
            }
            Ok((_, scanned)) if own => return not_ready(Some(scanned)),
            Ok(_) => {}
            Err(failure) => {
                return error(
                    request_id,
                    generation,
                    "index_unavailable",
                    failure.to_string(),
                );
            }
        }
    }
    // The answer's coverage is the asking workspace's.
    let coverage = match coverage {
        Some(coverage) => coverage,
        None => match graph.with_coverage(&of, |_| Ok(())) {
            Ok(((), coverage)) if coverage.scanned => coverage,
            Ok(((), coverage)) => return not_ready(Some(coverage)),
            Err(failure) => {
                return error(
                    request_id,
                    generation,
                    "index_unavailable",
                    failure.to_string(),
                );
            }
        },
    };
    let breaks = match langspan_core::call_breaks(
        store.state(),
        session_id,
        calls,
        &standings,
        &current,
        now,
    ) {
        Ok(breaks) => breaks,
        Err(e) => return error(request_id, generation, &e.code, e.message),
    };
    // Only the notices the hook's alert will show are recorded: one
    // left out for room stays to be told at a later write. The hook
    // lays the alert out with the same function.
    let notice_ids = crate::breaks::shown_notices(&breaks);
    if !notice_ids.is_empty() {
        // Recorded before it is answered: an alert that could not be
        // remembered is not given, so none is given twice.
        let record = Command::AcknowledgeInterface {
            session_id: session_id.to_owned(),
            notice_ids,
        };
        let id = crate::hook::wire_id("interface-alert", &[&request_id]);
        match store.execute(&id, &record, &record, now) {
            Ok(Execution {
                outcome: Outcome::Error { code, message },
                ..
            }) => return error(request_id, generation, &code, message),
            Ok(_) => {}
            Err(failure) => {
                return error(request_id, generation, "storage_error", failure.to_string());
            }
        }
    }
    answer(Outcome::CallBreaks {
        breaks,
        coverage,
        ready: true,
    })
}

/// Bytes a write check's answer takes at most, so that the MCP result,
/// which carries it twice, fits one frame.
const WRITE_CHECK_BYTES: usize = 24 * 1024;

/// A write check (roadmap B26), answered and never recorded: no event,
/// no receipt, no sighting of the session. Reads the asked paths and the
/// files of the definitions found, each at most 512 KiB and at most
/// twice `WRITE_CHECK_PATHS` files in all, their line-journal entries,
/// and one snapshot of the index, on the owner thread. Latency budget
/// (**Target**, not measured): under 100 ms, as `check_calls` plus
/// those bounded reads. Failure mode: an index error is a tool error;
/// no complete scan answers for the paths only.
/// At most this many definitions in an outline (roadmap A11): the
/// largest, when a file has more. The hook's text holds fewer.
const OUTLINE_ENTRIES: u32 = 200;

/// A whole read of a large file (roadmap A11): its outline, when the
/// index holds the file whole at the digest the hook read and the
/// session was not given this version's outline before. Recorded as
/// `read_outlined` only then; every other answer records nothing.
pub(super) fn outline_read(
    store: &mut Store,
    graph: Option<&crate::graph::Index>,
    request_id: String,
    command: Command,
) -> Response {
    let generation = store.state().generation;
    let Command::OutlineRead {
        session_id,
        path,
        digest,
        lines,
        bytes,
    } = &command
    else {
        return error(request_id, generation, "internal", "not an outline read");
    };
    let answer =
        |outline: Option<(u32, Vec<langspan_core::OutlineEntry>)>, skipped: &str| Response {
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            replayed: false,
            generation,
            lease_valid: None,
            outcome: Outcome::ReadOutline {
                definitions: outline.as_ref().map_or(0, |(total, _)| *total),
                outline: outline.map(|(_, entries)| entries),
                skipped: (!skipped.is_empty()).then(|| skipped.into()),
            },
        };
    let (min_lines, min_bytes) = OUTLINE_MIN
        .get()
        .copied()
        .unwrap_or((OUTLINE_MIN_LINES, OUTLINE_MIN_BYTES));
    if *lines < min_lines && *bytes < min_bytes {
        return answer(None, "small");
    }
    if langspan_core::was_outlined(store.state(), session_id, path, digest) {
        return answer(None, "shown");
    }
    let Some(of) = store
        .state()
        .sessions
        .get(session_id)
        .and_then(|session| Registration::of(store.state(), &session.workspace_id))
    else {
        return error(
            request_id,
            generation,
            "session_not_found",
            "the session is not registered in a known workspace",
        );
    };
    let found = graph.map(|graph| {
        graph.with_coverage(&of, |graph| {
            graph.outline(&of.workspace_id, path, digest, OUTLINE_ENTRIES)
        })
    });
    // The largest definitions, in line order, and how many there are.
    let (total, symbols) = match found {
        Some(Ok((Some((total, symbols)), coverage))) if coverage.scanned && !symbols.is_empty() => {
            (total, symbols)
        }
        Some(Ok(_)) | None => return answer(None, "not_indexed"),
        Some(Err(failure)) => {
            return error(
                request_id,
                generation,
                "index_unavailable",
                failure.to_string(),
            );
        }
    };
    let entries = symbols
        .into_iter()
        .map(|symbol| langspan_core::OutlineEntry {
            name: symbol.name,
            kind: symbol.kind,
            line: symbol.line,
            end_line: symbol.end_line,
            exported: symbol.exported,
        })
        .collect();
    match store.execute(&request_id, &command, &command, now_ms()) {
        Ok(execution) => match execution.outcome {
            Outcome::ReadOutline {
                outline: Some(_), ..
            } if !execution.replayed => answer(Some((total, entries)), ""),
            Outcome::ReadOutline { .. } => answer(None, "shown"),
            outcome @ Outcome::Error { .. } => Response {
                outcome,
                ..answer(None, "")
            },
            _ => answer(None, "shown"),
        },
        Err(e) => error(request_id, generation, "storage_error", e.to_string()),
    }
}

pub(super) fn check_write(
    store: &Store,
    graph: Option<&crate::graph::Index>,
    state_dir: &Path,
    request_id: String,
    (session_id, paths, symbols): (&str, &[String], &[String]),
) -> Response {
    use langspan_core::{
        DefinitionFacts, IndexFacts, LineRange, PathFacts, WRITE_CHECK_DEFINITIONS,
        WRITE_CHECK_PATHS,
    };
    let now = now_ms();
    let state = store.state();
    let generation = state.generation;
    let request = match langspan_core::write_check_request(state, session_id, paths, symbols) {
        Ok(request) => request,
        Err(e) => return error(request_id, generation, &e.code, e.message),
    };
    let Some(of) = Registration::of(state, &request.workspace_id) else {
        return error(
            request_id,
            generation,
            "workspace_not_found",
            "the session's workspace is not registered",
        );
    };
    // Definitions from one snapshot of the index: by name, or without
    // names, those in the asked paths, each read one past the bound so
    // truncation shows; and the definitions at each place (path and name)
    // a recorded signature change or removal names, read whatever that
    // bound left out. None without a complete scan.
    let noticed = langspan_core::write_check_noticed(state, session_id, &request, now);
    let definitions = match graph.map(|graph| {
        graph.with_coverage(&of, |graph| {
            let bound = WRITE_CHECK_DEFINITIONS as usize;
            let mut found = Vec::new();
            let mut truncated = false;
            let mut keep = |mut read: Vec<langspan_core::SymbolRef>| {
                truncated |= read.len() > bound;
                read.truncate(bound);
                found.extend(read);
            };
            if request.names.is_empty() {
                for path in &request.paths {
                    keep(graph.symbols_in(&of.workspace_id, path, WRITE_CHECK_DEFINITIONS + 1)?);
                }
            } else {
                for name in &request.names {
                    // `symbol` reads one past its limit.
                    keep(graph.symbol(&of.workspace_id, name, WRITE_CHECK_DEFINITIONS)?);
                }
            }
            let mut at = Vec::new();
            for (path, name) in &noticed {
                at.extend(graph.symbols_at(
                    &of.workspace_id,
                    path,
                    name,
                    WRITE_CHECK_DEFINITIONS,
                )?);
            }
            Ok((found, truncated, at))
        })
    }) {
        Some(Ok((found, coverage))) if coverage.scanned => Some(found),
        Some(Ok(_)) | None => None,
        Some(Err(failure)) => {
            return error(
                request_id,
                generation,
                "index_unavailable",
                failure.to_string(),
            );
        }
    };
    // The asked paths first, then the definitions' files, up to the bound.
    let mut files: Vec<String> = request.paths.clone();
    for symbol in definitions.iter().flat_map(|(found, _, _)| found) {
        if files.len() < 2 * WRITE_CHECK_PATHS && !files.contains(&symbol.path) {
            files.push(symbol.path.clone());
        }
    }
    // Each file's version and content, `None` when it could not be read.
    type Read = Option<(langspan_core::FileState, Option<Vec<u8>>)>;
    let read: std::collections::BTreeMap<&str, Read> = files
        .iter()
        .map(|path| (path.as_str(), crate::journal::current_file(&of.root, path)))
        .collect();
    let entries = crate::journal::entries_for(state_dir, &of.root, &files);
    let wall = crate::shell::now_ms();
    let path_facts: Vec<PathFacts> = request
        .paths
        .iter()
        .map(|path| {
            let found = read.get(path.as_str()).and_then(Option::as_ref);
            PathFacts {
                path: path.clone(),
                current: found.map(|(version, _)| version.clone()),
                written: found
                    .and_then(|(_, content)| content.as_deref())
                    .map(|content| {
                        crate::lines::overwritten(&entries, session_id, path, content, None, wall)
                    })
                    .unwrap_or_default(),
            }
        })
        .collect();
    let index = definitions.map(|(found, truncated, noticed)| IndexFacts {
        definitions: found
            .into_iter()
            .map(|symbol| {
                // Lines are placed only in the content the index read.
                let content = read
                    .get(symbol.path.as_str())
                    .and_then(Option::as_ref)
                    .and_then(|(version, content)| match version {
                        langspan_core::FileState::Present { digest, .. }
                            if *digest == symbol.digest =>
                        {
                            content.as_deref()
                        }
                        _ => None,
                    });
                let span = [LineRange {
                    start_line: symbol.line,
                    end_line: symbol.end_line.max(symbol.line),
                }];
                DefinitionFacts {
                    body_writers: content.map(|content| {
                        crate::lines::overwritten(
                            &entries,
                            session_id,
                            &symbol.path,
                            content,
                            Some(&span),
                            wall,
                        )
                    }),
                    symbol,
                }
            })
            .collect(),
        truncated,
        noticed,
    });
    let mut check = langspan_core::write_check(
        state,
        session_id,
        &request,
        &path_facts,
        index.as_ref(),
        now,
    );
    check.shrink_until(|check| {
        serde_json::to_vec(check).is_ok_and(|bytes| bytes.len() <= WRITE_CHECK_BYTES)
    });
    Response {
        version: PROTOCOL_VERSION,
        request_id,
        replayed: false,
        generation,
        lease_valid: None,
        outcome: Outcome::WriteCheck { check },
    }
}

#[cfg(test)]
mod frame_tests;

#[cfg(test)]
mod inflight_tests;
