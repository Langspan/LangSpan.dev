use super::*;
use crate::protocol;
use langspan_core::{GraphCoverage, SymbolNote, SymbolRef};

#[test]
fn a_detailed_answer_keeps_the_definitions_that_fit_one_frame() {
    let symbol = |index: usize| SymbolRef {
        path: format!("src/file{index}.rs"),
        name: "upload".into(),
        notes: Some(vec![
            SymbolNote {
                id: "note".into(),
                text: "x".repeat(300),
            };
            4
        ]),
        ..SymbolRef::default()
    };
    let coverage = GraphCoverage::default();
    let mut few: Vec<SymbolRef> = (0..3).map(symbol).collect();
    assert!(!fit_frame(&mut few, &coverage));
    assert_eq!(few.len(), 3);

    let mut many: Vec<SymbolRef> = (0..200).map(symbol).collect();
    assert!(fit_frame(&mut many, &coverage), "left out, and said so");
    assert!(many.len() > 10, "{}", many.len());
    let response = Response {
        version: PROTOCOL_VERSION,
        request_id: "r".repeat(128),
        replayed: false,
        generation: u64::MAX,
        lease_valid: None,
        outcome: Outcome::Graph {
            symbols: many,
            references: Vec::new(),
            resolution: Some("name".into()),
            coverage,
            truncated: true,
            levels: None,
            renamed: None,
        },
    };
    protocol::encode(&response).expect("fits one frame");
}

/// Twelve names of `definitions` definitions, each with `dependents`
/// dependents and five mentioning files, every path `long` bytes.
fn context(definitions: usize, dependents: usize, long: usize) -> Vec<langspan_core::NameContext> {
    let path = |what: &str, n: usize| format!("{what}/{n:03}/{}.rs", "x".repeat(long));
    (0..12)
        .map(|n| langspan_core::NameContext {
            name: format!("name_{n}"),
            definitions: (0..definitions)
                .map(|d| SymbolRef {
                    path: path("defs", n * 10 + d),
                    name: format!("name_{n}"),
                    dependents: Some(
                        (0..dependents)
                            .map(|c| langspan_core::Dependent {
                                path: path("callers", c),
                                mentions: 1,
                                rank_reasons: Vec::new(),
                            })
                            .collect(),
                    ),
                    dependent_count: Some(dependents as u32 + 4),
                    ..SymbolRef::default()
                })
                .collect(),
            mentioned_in: (0..5).map(|c| path("callers", c)).collect(),
            truncated: false,
            definition_count: Some(definitions as u32 + 1),
        })
        .collect()
}

fn encodes(matches: Vec<langspan_core::NameContext>) {
    let response = Response {
        version: PROTOCOL_VERSION,
        request_id: "r".repeat(128),
        replayed: false,
        generation: u64::MAX,
        lease_valid: None,
        outcome: Outcome::GraphContext {
            matches,
            coverage: GraphCoverage::default(),
        },
    };
    protocol::encode(&response).expect("fits one frame");
}

#[test]
fn a_code_context_that_fits_one_frame_is_left_whole() {
    let coverage = GraphCoverage::default();
    let whole = context(3, 5, 20);
    let mut fitted = whole.clone();
    assert!(!fit_context(&mut fitted, &coverage));
    assert_eq!(fitted, whole);
}

#[test]
fn a_code_context_cuts_dependents_first_and_keeps_their_counts() {
    // Review comment 4709803050: twelve names, three definitions,
    // five dependents of about 300 bytes each.
    let coverage = GraphCoverage::default();
    let mut fitted = context(3, 5, 300);
    assert!(fit_context(&mut fitted, &coverage), "cut, and said so");
    assert_eq!(fitted.len(), 12, "every name kept");
    for matched in &fitted {
        assert_eq!(matched.definitions.len(), 3, "every definition kept");
        assert_eq!(matched.mentioned_in.len(), 5);
        assert!(!matched.truncated, "only dependents were cut");
        assert_eq!(matched.definition_count, Some(4));
        for symbol in &matched.definitions {
            let kept = symbol.dependents.as_ref().unwrap().len();
            assert!(kept < 5, "{kept}");
            assert_eq!(symbol.dependent_count, Some(9), "still counted");
        }
    }
    encodes(fitted.clone());
    let mut again = context(3, 5, 300);
    fit_context(&mut again, &coverage);
    assert_eq!(again, fitted, "the same on every call");
}

#[test]
fn a_code_context_cuts_mentioning_files_then_definitions_then_names() {
    let coverage = GraphCoverage::default();
    // Without dependents (an answer that is not ranked), long
    // mentioning files are cut next and the name is marked.
    let mut unranked = context(3, 0, 800);
    for matched in &mut unranked {
        matched.definition_count = None;
        for symbol in &mut matched.definitions {
            symbol.dependents = None;
            symbol.dependent_count = None;
        }
    }
    assert!(fit_context(&mut unranked, &coverage));
    assert_eq!(unranked.len(), 12);
    assert!(
        unranked
            .iter()
            .all(|m| m.truncated && m.mentioned_in.len() < 5)
    );
    assert!(unranked.iter().all(|m| m.definitions.len() == 3));
    encodes(unranked);

    // Longer still, definitions go down to one each, lowest-ranked
    // first.
    let whole = context(3, 5, 1900);
    let mut fewer = whole.clone();
    assert!(fit_context(&mut fewer, &coverage));
    assert_eq!(fewer.len(), 12);
    for (matched, whole) in fewer.iter().zip(&whole) {
        assert!(matched.truncated && matched.mentioned_in.is_empty());
        assert!(matched.definitions.len() < 3);
        assert_eq!(matched.definitions[0].path, whole.definitions[0].path);
        assert_eq!(matched.definition_count, Some(4), "still counted");
    }
    encodes(fewer);

    // A single definition per name that cannot all fit loses the
    // last names.
    let mut names = context(1, 0, 8000);
    assert!(fit_context(&mut names, &coverage));
    assert!(!names.is_empty() && names.len() < 12, "{}", names.len());
    assert_eq!(names[0].name, "name_0");
    encodes(names);
}

#[test]
fn impact_levels_with_long_paths_are_cut_to_one_frame_and_say_so() {
    use langspan_core::{ImpactFile, ImpactLevel};
    let file = |level: u32, n: u32| ImpactFile {
        path: format!("src/{level}/{n:03}/{}.rs", "x".repeat(200)),
        via: "name".into(),
        from: format!("src/{}.rs", "y".repeat(200)),
    };
    let mut levels: Vec<ImpactLevel> = (1..=3)
        .map(|level| ImpactLevel {
            level,
            files: (0..100).map(|n| file(level, n)).collect(),
            truncated: false,
        })
        .collect();
    let coverage = GraphCoverage::default();
    fit_levels(&mut levels, &(&coverage,));
    assert!(!levels[0].truncated, "the first level fits whole");
    assert!(levels[1].truncated && levels[2].truncated);
    assert!(levels[1].files.len() < 100);
    assert_eq!(levels[2].files, []);
    let response = Response {
        version: PROTOCOL_VERSION,
        request_id: "r".repeat(128),
        replayed: false,
        generation: u64::MAX,
        lease_valid: None,
        outcome: Outcome::Graph {
            symbols: Vec::new(),
            references: Vec::new(),
            resolution: Some("name".into()),
            coverage,
            truncated: true,
            levels: Some(levels),
            renamed: None,
        },
    };
    protocol::encode(&response).expect("fits one frame");
}

#[test]
fn blind_spots_with_long_paths_keep_one_common_prefix_that_fits_one_frame() {
    let paths = |list: &str| -> Vec<String> {
        (0..200)
            .map(|n| format!("{list}/{n:03}/{}", "x".repeat(200)))
            .collect()
    };
    let full = langspan_core::BlindSpots {
        partial: paths("p")
            .into_iter()
            .map(|path| langspan_core::PartialFile { path, since_ms: 1 })
            .collect(),
        never_clean: paths("n"),
        waiting: paths("w"),
        too_large: paths("t"),
        unscanned: paths("u"),
        ..Default::default()
    };
    let coverage = GraphCoverage::default();
    let mut small = langspan_core::BlindSpots {
        waiting: vec!["src/a.rs".into()],
        ..Default::default()
    };
    assert!(!fit_blind_spots(&mut small, &coverage, None));
    assert_eq!(small.waiting, ["src/a.rs"]);

    let mut first = full.clone();
    assert!(fit_blind_spots(&mut first, &coverage, None), "said so");
    let kept = first.waiting.len();
    assert!(kept > 10 && kept < 200, "{kept}");
    assert_eq!(first.partial.len(), kept);
    assert_eq!(first.unscanned.len(), kept);
    assert_eq!(first.waiting[..], full.waiting[..kept], "a prefix");
    let mut again = full.clone();
    fit_blind_spots(&mut again, &coverage, None);
    assert_eq!(again, first, "the same on every call");
    let response = Response {
        version: PROTOCOL_VERSION,
        request_id: "r".repeat(128),
        replayed: false,
        generation: u64::MAX,
        lease_valid: None,
        outcome: Outcome::GraphBlindSpots {
            blind_spots: first,
            coverage,
            scan_failure: None,
            truncated: true,
        },
    };
    protocol::encode(&response).expect("fits one frame");
}
