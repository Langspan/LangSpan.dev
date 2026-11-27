//! The source graph and write checks.

use langspan_core::{GraphCoverage, ImpactLevel, InflightNotice, NameRef, Rename, SymbolRef};

/// `graph impact --depth N`: one line per level, paths and names quoted
/// because they are repository text. An empty level never claims that
/// nothing is affected.
fn print_impact_levels(
    levels: &[langspan_core::ImpactLevel],
    coverage: &langspan_core::GraphCoverage,
) {
    let blind = coverage.blind_in_scope > 0 || coverage.current_files < coverage.indexable_files;
    for level in levels {
        let files: Vec<String> = level
            .files
            .iter()
            .map(|file| {
                if level.level == 1 {
                    format!("{:?} via {:?}", file.path, file.via)
                } else {
                    format!("{:?} via {:?} from {:?}", file.path, file.via, file.from)
                }
            })
            .collect();
        let heading = match (level.level, level.files.first()) {
            (1, Some(file)) => format!("Level 1 (mentions an export of {:?})", file.from),
            (n, _) => format!("Level {n}"),
        };
        let found = if files.is_empty() {
            if blind {
                "none found by name, but the index is not complete, so this is no claim that none is affected".to_owned()
            } else {
                "none found by name".to_owned()
            }
        } else {
            files.join(", ")
        };
        println!(
            "{heading}: {found}{}",
            if level.truncated {
                " (cut by a bound; more may exist)"
            } else {
                ""
            }
        );
    }
}

/// The closing line of a `graph path` or `graph map` answer when the index
/// cannot vouch for it: no complete scan, or files it cannot read in full.
fn print_index_caveat(coverage: &langspan_core::GraphCoverage) {
    if !coverage.scanned {
        println!("Nothing is indexed for this workspace yet; an empty answer means nothing.");
    } else if coverage.blind_in_scope > 0 || coverage.current_files < coverage.indexable_files {
        println!(
            "Index: {} of {} supported files at their current content; `langspan graph blind-spots` names the rest.",
            coverage.current_files, coverage.indexable_files
        );
    }
}

/// `graph path`: each chain one step per line, path quoted because it is
/// repository text, then how its steps were found (roadmap D3).
pub(super) fn print_paths(
    (from, to, max_hops): (&str, &str, u32),
    paths: &[langspan_core::SymbolPath],
    coverage: &langspan_core::GraphCoverage,
    truncated: bool,
) {
    if paths.is_empty() && coverage.scanned {
        println!(
            "No chain of calls from {from} to {to} within {max_hops} calls was found by name or import path."
        );
    }
    for (index, chain) in paths.iter().enumerate() {
        if paths.len() > 1 {
            println!("Chain {} of {}:", index + 1, paths.len());
        }
        for (number, step) in chain.steps.iter().enumerate() {
            let what = match &step.callee {
                Some(callee) => format!("{} calls {callee}", step.name),
                None => format!("{} (defined)", step.name),
            };
            println!(
                "{}. {:?}:{} {what}{}",
                number + 1,
                step.path,
                step.line,
                if step.ambiguous {
                    " (ambiguous: the name has other definitions)"
                } else {
                    ""
                }
            );
        }
        let by_path = chain
            .steps
            .iter()
            .filter(|step| step.resolution == "path")
            .count();
        let by_name = chain.steps.len() - by_path;
        let steps = |n: usize| {
            if n == 1 {
                "1 step".to_owned()
            } else {
                format!("{n} steps")
            }
        };
        if by_path == 0 {
            println!("Found by name ({}); not type-checked.", steps(by_name));
        } else {
            println!(
                "Found by name ({}) and by import path ({}); not type-checked.",
                steps(by_name),
                steps(by_path)
            );
        }
    }
    if truncated {
        println!("The search stopped at a bound; a chain may still exist.");
    }
    print_index_caveat(coverage);
}

/// `graph map`: one line per area, then one per edge, paths quoted
/// (roadmap D3).
pub(super) fn print_map(
    depth: u32,
    areas: &[langspan_core::MapArea],
    edges: &[langspan_core::MapEdge],
    coverage: &langspan_core::GraphCoverage,
    truncated: bool,
) {
    for area in areas {
        println!(
            "{:?}: {} files, {} exported, {}",
            area.path,
            area.files,
            area.exported,
            area.languages.join(", ")
        );
    }
    for edge in edges {
        println!(
            "{:?} -> {:?}: {} {} (by {})",
            edge.from, edge.to, edge.count, edge.kind, edge.resolution
        );
    }
    if coverage.scanned {
        println!(
            "Areas at depth {depth}. Imports are resolved by path; calls are counted by name, only for a name with one definition; not type-checked."
        );
    }
    if truncated {
        println!("Some areas or edges are left out by a bound.");
    }
    print_index_caveat(coverage);
}

/// `graph blind-spots`: one line per non-empty list, paths quoted, with the
/// full count in parentheses.
pub(super) fn print_blind_spots(
    spots: &langspan_core::BlindSpots,
    coverage: &langspan_core::GraphCoverage,
    failure: Option<&langspan_core::ScanFailure>,
    truncated: bool,
) {
    if let Some(failure) = failure {
        println!(
            "The last scan failed ({:?}); this is as of the last complete one.",
            failure.detail
        );
    }
    if !coverage.scanned {
        println!("No complete scan of this workspace yet: nothing is known.");
        return;
    }
    let now = crate::clock::now_ms();
    let since = |at: u64| {
        let seconds = now.saturating_sub(at) / 1000;
        match seconds {
            0..=89 => format!("{seconds}s"),
            90..=5399 => format!("{}m", seconds / 60),
            5400..=129_599 => format!("{}h", seconds / 3600),
            _ => format!("{}d", seconds / 86_400),
        }
    };
    let line = |label: &str, total: u32, paths: Vec<String>| {
        if total > 0 {
            let more = total as usize - paths.len().min(total as usize);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            println!("{label} ({total}): {}{more}", paths.join(", "));
        }
    };
    let quoted = |paths: &[String]| paths.iter().map(|path| format!("{path:?}")).collect();
    let totals = &spots.totals;
    line(
        "partial",
        totals.partial,
        spots
            .partial
            .iter()
            .map(|file| format!("{:?} (since {})", file.path, since(file.since_ms)))
            .collect(),
    );
    line(
        "never parsed cleanly",
        totals.never_clean,
        quoted(&spots.never_clean),
    );
    line("waiting", totals.waiting, quoted(&spots.waiting));
    line("too large", totals.too_large, quoted(&spots.too_large));
    line(
        "left out of the scan",
        totals.unscanned,
        quoted(&spots.unscanned),
    );
    if !spots.unsupported_languages.is_empty() {
        println!(
            "not indexed, other languages: {}",
            spots
                .unsupported_languages
                .iter()
                .map(|count| format!("{} {}", count.extension, count.files))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if coverage.blind_in_scope == 0 && spots.unsupported_languages.is_empty() {
        println!("The index reads every supported file at its current content.");
    }
    if truncated {
        println!("More paths exist than the limit allows.");
    }
}

/// The lines `graph symbol --detail` adds under a definition. Note text is
/// another session's: quoted, so it reads as data.
/// A ranked definition's reasons and dependents (roadmap C2), under it.
fn print_symbol_rank(symbol: &langspan_core::SymbolRef) {
    use langspan_core::RankReason;
    let reason = |reason: &RankReason| match reason {
        RankReason::OwnFile { edited: true } => "the session edited this file".to_owned(),
        RankReason::OwnFile { edited: false } => "the session read this file".to_owned(),
        RankReason::PeerEditing { session_id } => format!("session {session_id} is editing it"),
        RankReason::Exported => "exported".to_owned(),
        RankReason::QualifiedInPrompt => "the prompt names its path".to_owned(),
    };
    if let Some(reasons) = symbol.rank_reasons.as_ref().filter(|r| !r.is_empty()) {
        let reasons: Vec<String> = reasons.iter().map(reason).collect();
        println!("  ranked by: {}", reasons.join("; "));
    }
    let Some(dependents) = &symbol.dependents else {
        return;
    };
    let listed: Vec<String> = dependents
        .iter()
        .map(|dependent| {
            let mut parts = vec![format!("{} mention(s)", dependent.mentions)];
            parts.extend(dependent.rank_reasons.iter().map(reason));
            format!("{} ({})", dependent.path, parts.join(", "))
        })
        .collect();
    let more = symbol
        .dependent_count
        .unwrap_or(0)
        .saturating_sub(dependents.len() as u32);
    match (listed.is_empty(), more) {
        (true, _) => println!("  used by: no other indexed file"),
        (false, 0) => println!("  used by: {}", listed.join(", ")),
        (false, more) => println!("  used by: {} and {more} more file(s)", listed.join(", ")),
    }
}

fn print_symbol_detail(symbol: &langspan_core::SymbolRef) {
    if let Some(digest) = &symbol.body_digest {
        println!("  body digest {digest}");
    }
    if let Some(calls) = &symbol.calls {
        let shown: Vec<String> = calls
            .iter()
            .map(|call| format!("{} ×{} (line {})", call.name, call.count, call.line))
            .collect();
        println!(
            "  calls: {}",
            if shown.is_empty() {
                "none found".into()
            } else {
                shown.join(", ")
            }
        );
    }
    if let Some(callers) = &symbol.callers {
        let more = callers.count as usize > callers.sites.len();
        println!(
            "  called by name at {} site(s){}{}{}",
            callers.count,
            if callers.sites.is_empty() { "" } else { ": " },
            callers.sites.join(", "),
            if more { ", …" } else { "" }
        );
    }
    for note in symbol.notes.iter().flatten() {
        println!("  note {}: {:?}", note.id, note.text);
    }
    if let Some(check) = &symbol.last_check {
        println!(
            "  last check on this file's content: {} {} on tree {}",
            check.name, check.result, check.tree
        );
    }
}

/// Line ranges as `1-2, 6`.
fn line_ranges(ranges: &[langspan_core::LineRange]) -> String {
    ranges
        .iter()
        .map(|range| {
            if range.start_line == range.end_line {
                range.start_line.to_string()
            } else {
                format!("{}-{}", range.start_line, range.end_line)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A write check (roadmap B26). Paths, names and signatures are quoted:
/// repository text, shown as data.
pub(super) fn print_write_check(check: &langspan_core::WriteCheck) -> Result<(), crate::Error> {
    println!("{}", langspan_core::WRITE_CHECK_LABEL);
    let seconds = |age_ms: u64| format!("{}s ago", age_ms / 1000);
    for path in &check.paths {
        let mut parts = vec![match &path.held_by {
            Some(holder) => format!("held by {holder}"),
            None => "not held".to_owned(),
        }];
        parts.push(
            match path.stale {
                Some(true) => "changed since this session last read it",
                Some(false) => "as this session last read it",
                None => "no read by this session recorded",
            }
            .to_owned(),
        );
        for unread in &path.unread_lines {
            parts.push(if unread.lines.is_empty() {
                format!(
                    "{} wrote lines not known {}",
                    unread.session_id,
                    seconds(unread.age_ms)
                )
            } else {
                format!(
                    "{} wrote lines {} {}",
                    unread.session_id,
                    line_ranges(&unread.lines),
                    seconds(unread.age_ms)
                )
            });
        }
        if path.omitted_writers > 0 {
            parts.push(format!("{} more writer(s) left out", path.omitted_writers));
        }
        println!(
            "{}: {}.",
            serde_json::to_string(&path.path)?,
            parts.join("; ")
        );
    }
    if !check.index_ready {
        println!(
            "Symbols not checked: the source index has no complete scan of this workspace yet."
        );
        return Ok(());
    }
    for symbol in &check.symbols {
        let by = symbol.by_session.as_deref().unwrap_or("an unknown session");
        let age = symbol.age_ms.map(seconds).unwrap_or_default();
        let what = match symbol.change.as_str() {
            "signature" => format!(
                "signature changed by {by} {age}, from {} to {}",
                serde_json::to_string(symbol.before.as_deref().unwrap_or_default())?,
                serde_json::to_string(symbol.after.as_deref().unwrap_or_default())?
            ),
            "removed" => match &symbol.renamed_to {
                Some(new) => format!("renamed to {} by {by} {age}", serde_json::to_string(new)?),
                None => format!("removed by {by} {age}"),
            },
            _ => format!(
                "body lines {} written by {by} {age}",
                line_ranges(&symbol.lines)
            ),
        };
        let intents = if symbol.intents.is_empty() {
            String::new()
        } else {
            format!("; intent of {} names it", symbol.intents.join(", "))
        };
        println!(
            "{} ({} in {}): {what}{intents}.",
            serde_json::to_string(&symbol.name)?,
            symbol.kind,
            serde_json::to_string(&symbol.path)?
        );
    }
    if check.symbols.is_empty() {
        println!("No recent change by another session to these symbols is recorded.");
    }
    if check.omitted_symbols > 0 {
        println!("{} more symbol change(s) left out.", check.omitted_symbols);
    }
    if check.unchecked_bodies > 0 {
        println!(
            "{} definition(s) not compared with recent writes: their file is not indexed at its current content, or another session's write changed lines the journal does not know.",
            check.unchecked_bodies
        );
    }
    if check.definitions_truncated {
        println!(
            "More definitions matched than one check compares ({}): body changes to the rest are not shown.",
            langspan_core::WRITE_CHECK_DEFINITIONS
        );
    }
    Ok(())
}

pub(super) fn print_graph_inflight(
    inflight: &[InflightNotice],
    coverage: &GraphCoverage,
    truncated: &bool,
) {
    if !coverage.scanned {
        println!("The index has no complete scan of this workspace yet.");
    } else if inflight.is_empty() {
        println!(
            "No other worktree of this repository has an uncommitted signature that this workspace mentions."
        );
    }
    for item in inflight {
        let session = item
            .session
            .as_deref()
            .map(|session| format!(", session {session}"))
            .unwrap_or_default();
        println!(
            "{} {} in {} (worktree {}{session}, not committed): {} — mentioned in {}",
            item.kind,
            item.name,
            item.path,
            item.worktree,
            item.signature,
            item.mentioned_in.join(", ")
        );
    }
    if *truncated {
        println!("More results exist than the limit allows.");
    }
    if !inflight.is_empty() {
        println!("Matches are by name, not resolved.");
    }
}

pub(super) fn print_graph_imported(
    workspace_id: &str,
    applied: &bool,
    loaded: &u32,
    different: &u32,
    kept: &u32,
    over_limits: &u32,
    malformed: &u32,
) {
    let mut parts = vec![if *applied {
        format!("Loaded {loaded} files into workspace {workspace_id}")
    } else {
        format!(
            "Dry run: {loaded} files have the same content here and can be loaded into workspace {workspace_id}"
        )
    }];
    parts.push(format!(
        "{different} differ or are missing here and are left to the indexer"
    ));
    if *kept > 0 {
        parts.push(format!("{kept} are already indexed here and are kept"));
    }
    if *over_limits > 0 {
        parts.push(format!(
            "{over_limits} lines over a parse bound were skipped"
        ));
    }
    if *malformed > 0 {
        parts.push(format!("{malformed} malformed lines were skipped"));
    }
    println!(
        "{}.{}",
        parts.join("; "),
        if *applied { "" } else { " Add --apply." }
    );
}

pub(super) fn print_graph(
    symbols: &[SymbolRef],
    references: &[NameRef],
    coverage: &GraphCoverage,
    truncated: &bool,
    levels: &Option<Vec<ImpactLevel>>,
    renamed: &Option<Vec<Rename>>,
) -> Result<(), crate::Error> {
    // Roadmap D7: the renames followed, names and path quoted.
    for hop in renamed.iter().flatten() {
        println!(
            "{} was renamed to {} in {}{}",
            serde_json::to_string(&hop.name)?,
            serde_json::to_string(&hop.after_name)?,
            serde_json::to_string(&hop.path)?,
            langspan_core::rename_evidence_words(hop.evidence.as_deref())
        );
    }
    for symbol in symbols {
        println!(
            "{}:{} {} {}{} — {}",
            symbol.path,
            symbol.line,
            symbol.kind,
            symbol.name,
            if symbol.exported { " (exported)" } else { "" },
            symbol.signature
        );
        print_symbol_rank(symbol);
        print_symbol_detail(symbol);
    }
    match levels {
        // Level 1 lists the files of `references`, once each.
        Some(levels) => print_impact_levels(levels, coverage),
        None => {
            for reference in references {
                println!(
                    "{}:{} mentions {}",
                    reference.path, reference.line, reference.name
                );
            }
        }
    }
    if *truncated {
        println!("More results exist than the limit allows.");
    }
    if !coverage.scanned {
        println!("Nothing is indexed for this workspace yet; an empty answer means nothing.");
    } else {
        println!(
            "{}. Index: {} of {} supported files at their current content{}.",
            if levels.is_some() {
                "Found by name, not resolved; each level adds false positives"
            } else {
                "Matches are by name, not resolved uses"
            },
            coverage.current_files,
            coverage.indexable_files,
            if coverage.partial_files > 0 {
                format!(", {} only partly parsed", coverage.partial_files)
            } else {
                String::new()
            }
        );
        if coverage.blind_in_scope > 0 {
            println!(
                "{} supported file(s) cannot be read in full, so an empty answer is weak; `langspan graph blind-spots` names them.",
                coverage.blind_in_scope
            );
        }
    }
    Ok(())
}
