//! Golden decisions (issues #318 and #319): for a fixed sequence of commands, the
//! `(events, outcome)` that `decide` returns, kept in files so a change to
//! any decision shows as a readable difference.
//!
//! Each domain has `tests/golden/decisions/<domain>.steps.jsonl`, one step
//! per line: `name` (also the request ID), `now_ms` (the fixed clock) and
//! either `command`, a wire command, or `prepared`, an adapter input that
//! never crosses the wire. A domain starts from a booted, empty state and
//! applies each decision's events before the next step. What `decide`
//! returned for each step is in `<domain>.golden`; for a command the daemon's
//! stores answer, that is the core's routing error. Every command maps to
//! its domain in `cases!`, an exhaustive match.
//!
//! To accept a changed decision on purpose, run
//! `cargo nextest run -p langspan-core --test decision_golden --run-ignored only -E 'test(bless)'`
//! and review the difference in the golden files before committing it.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use langspan_core::{AnchorIndex, Command, Event, InflightSymbol, State, SymbolRef, apply, decide};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    name: String,
    now_ms: u64,
    #[serde(default)]
    command: Option<Command>,
    #[serde(default)]
    prepared: Option<AdapterInput>,
}

/// The adapter inputs the daemon prepares from a wire command. They have no
/// wire form, so the test reads them through this mirror.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum AdapterInput {
    WorkspaceRegisterVerified {
        workspace_id: String,
        root: String,
        verified_overlaps: Vec<String>,
    },
    WorkspaceMoveVerified {
        workspace_id: String,
        root: String,
        verified_overlaps: Vec<String>,
    },
    BriefPrepared {
        session_id: String,
        max_peers: u32,
        #[serde(default)]
        deliver: bool,
        #[serde(default)]
        mid_turn: bool,
        #[serde(default)]
        recent: bool,
        #[serde(default)]
        recent_window_ms: Option<u64>,
        #[serde(default)]
        mcp: bool,
        #[serde(default)]
        left: Vec<String>,
        #[serde(default)]
        changes_only: bool,
        #[serde(default)]
        confirm: bool,
        #[serde(default)]
        inflight: Vec<InflightSymbol>,
        #[serde(default)]
        inflight_partial: bool,
        #[serde(default)]
        mentioned: BTreeSet<String>,
    },
    RememberPrepared {
        workspace_id: String,
        #[serde(default)]
        author: Option<String>,
        text: String,
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        symbol: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        index: Option<Index>,
    },
    NoteAnchorsPrepared {
        workspace_id: String,
        index: Index,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    #[serde(default)]
    definitions: Vec<SymbolRef>,
    #[serde(default)]
    fresh: BTreeSet<String>,
    #[serde(default)]
    absent: BTreeSet<String>,
    #[serde(default)]
    truncated: BTreeSet<String>,
    complete: bool,
}

impl From<Index> for AnchorIndex {
    fn from(index: Index) -> Self {
        AnchorIndex {
            definitions: index.definitions,
            fresh: index.fresh,
            absent: index.absent,
            truncated: index.truncated,
            complete: index.complete,
        }
    }
}

impl From<AdapterInput> for Command {
    fn from(prepared: AdapterInput) -> Self {
        match prepared {
            AdapterInput::WorkspaceRegisterVerified {
                workspace_id,
                root,
                verified_overlaps,
            } => Command::WorkspaceRegisterVerified {
                workspace_id,
                root,
                verified_overlaps,
            },
            AdapterInput::WorkspaceMoveVerified {
                workspace_id,
                root,
                verified_overlaps,
            } => Command::WorkspaceMoveVerified {
                workspace_id,
                root,
                verified_overlaps,
            },
            AdapterInput::BriefPrepared {
                session_id,
                max_peers,
                deliver,
                mid_turn,
                recent,
                recent_window_ms,
                mcp,
                left,
                changes_only,
                confirm,
                inflight,
                inflight_partial,
                mentioned,
            } => Command::BriefPrepared {
                session_id,
                max_peers,
                deliver,
                mid_turn,
                recent,
                recent_window_ms,
                mcp,
                left,
                changes_only,
                confirm,
                inflight,
                inflight_partial,
                mentioned,
            },
            AdapterInput::RememberPrepared {
                workspace_id,
                author,
                text,
                paths,
                symbol,
                tags,
                index,
            } => Command::RememberPrepared {
                workspace_id,
                author,
                text,
                paths,
                symbol,
                tags,
                index: index.map(AnchorIndex::from),
            },
            AdapterInput::NoteAnchorsPrepared {
                workspace_id,
                index,
            } => Command::NoteAnchorsPrepared {
                workspace_id,
                index: index.into(),
            },
        }
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/decisions")
}

/// Each step of a domain's file with its command, in order.
fn steps(domain: &str) -> Vec<(String, u64, Command)> {
    let file = format!("{domain}.steps.jsonl");
    let text = fs::read_to_string(golden_dir().join(&file))
        .unwrap_or_else(|error| panic!("cannot read {file}: {error}"));
    let mut names = BTreeSet::new();
    let mut steps = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = format!("{file}:{}", index + 1);
        let step: Step =
            serde_json::from_str(line).unwrap_or_else(|error| panic!("{at}: not a step: {error}"));
        assert!(
            names.insert(step.name.clone()),
            "{at}: step name used twice"
        );
        let command = match (step.command, step.prepared) {
            (Some(command), None) => command,
            (None, Some(prepared)) => prepared.into(),
            _ => panic!("{at}: give exactly one of command and prepared"),
        };
        steps.push((step.name, step.now_ms, command));
    }
    steps
}

/// Runs a domain's steps from a booted state and renders each decision.
fn decisions(domain: &str) -> String {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).expect("first boot");
    let mut rendered = String::new();
    for (name, now_ms, command) in steps(domain) {
        let decision = decide(&state, &command, &name, now_ms);
        for event in &decision.events {
            apply(&mut state, event).unwrap_or_else(|error| {
                panic!("{domain}/{name}: core refused its own event: {error:?}")
            });
        }
        let json = serde_json::to_string_pretty(&decision).expect("a decision serializes");
        writeln!(rendered, "## {name}\n{json}").expect("write to a string");
    }
    rendered
}

/// Splits rendered decisions into steps by their `## name` header.
fn blocks(text: &str) -> BTreeMap<String, (usize, Vec<&str>)> {
    let mut blocks = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            let order = blocks.len();
            blocks.insert(name.to_owned(), (order, Vec::new()));
            current = Some(name.to_owned());
        } else if let Some(name) = &current {
            blocks.get_mut(name).expect("block").1.push(line);
        }
    }
    blocks
}

/// A line diff of one step, `-` for the golden and `+` for this run.
fn line_diff(expected: &[&str], actual: &[&str]) -> String {
    let (n, m) = (expected.len(), actual.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if expected[i] == actual[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, String::new());
    while i < n || j < m {
        if i < n && j < m && expected[i] == actual[j] {
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            writeln!(out, "- {}", expected[i]).expect("write to a string");
            i += 1;
        } else {
            writeln!(out, "+ {}", actual[j]).expect("write to a string");
            j += 1;
        }
    }
    out
}

const BLESS: &str = "If the change is intended, run cargo nextest run -p langspan-core \
                     --test decision_golden --run-ignored only -E 'test(bless)' \
                     and review the golden diff.";

/// Compares a domain's decisions with its golden file, and says which
/// steps decided differently and how.
fn check(domain: &str) -> Result<(), String> {
    let actual = decisions(domain);
    let path = golden_dir().join(format!("{domain}.golden"));
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("{domain}: cannot read {domain}.golden: {error}"))?;
    if expected == actual {
        return Ok(());
    }
    let (want, got) = (blocks(&expected), blocks(&actual));
    let mut report = String::new();
    for (name, (_, lines)) in &got {
        match want.get(name) {
            None => writeln!(report, "step {name}: not in the golden file").expect("write"),
            Some((_, old)) if old != lines => writeln!(
                report,
                "step {name} decided differently:\n{}",
                line_diff(old, lines)
            )
            .expect("write"),
            Some(_) => {}
        }
    }
    for name in want.keys().filter(|name| !got.contains_key(*name)) {
        writeln!(report, "step {name}: in the golden file but no longer run").expect("write");
    }
    let order = |blocks: &BTreeMap<String, (usize, Vec<&str>)>| {
        let mut names: Vec<_> = blocks
            .iter()
            .map(|(name, (at, _))| (*at, name.clone()))
            .collect();
        names.sort();
        names
    };
    if report.is_empty() {
        report.push_str(if order(&want) != order(&got) {
            "the steps run in a different order than the golden file records\n"
        } else {
            "the golden file's layout differs from what this run writes\n"
        });
    }
    Err(format!(
        "{domain}: decisions differ from {domain}.golden\n{report}"
    ))
}

macro_rules! cases {
    ($($domain:literal => [$($variant:ident),* $(,)?],)*) => {
        /// The domain whose steps hold each command's golden decision. The
        /// match is exhaustive, so a new command fails to compile here until
        /// it has a case; a prepared adapter input is a case of its own.
        fn case(command: &Command) -> (&'static str, &'static str) {
            match command {
                $($(Command::$variant { .. } => ($domain, stringify!($variant)),)*)*
            }
        }

        const CASES: &[(&str, &str)] = &[$($(($domain, stringify!($variant)),)*)*];
        const DOMAINS: &[&str] = &[$($domain),*];
    };
}

cases! {
    "sessions" => [
        WorkspaceRegister, WorkspaceRegisterVerified, WorkspaceList, WorkspaceMove,
        WorkspaceMoveVerified, WorkspaceRemove, SessionRegister, ObserveSessionLiveness,
        SessionSeen, RecordContext, SessionPrompt, SetPromptTargets, ObservePresence,
        SetPresence, SessionStop, SessionEnd, Adopt,
    ],
    "admission" => [
        IntentSet, IntentEnd, Admit, AdmitTool, Holders, RecordWait, RecordRefusal, Policy,
        SetPolicy, ObserveSource, OutlineRead, Observe, ObserveTool, ObserveToolBatch, Complete,
    ],
    "observation" => [
        ObserveWorkspaceGit, ObserveBranch, ObserveDisk, ObserveScan, ObserveUncoordinated,
        ObserveCheckout, ObserveCheckoutRun,
    ],
    "messages" => [
        SendMessage, AckMessage, Handoff, RetractMessage, ShowMessage, MessageStatus, Remember,
        RememberPrepared, ObserveNoteAnchors, NoteAnchorsPrepared, Recall, Recap, Forget,
    ],
    "remote" => [
        RemoteEnable, RemoteDisable, RemoteOutbox, RemotePublished, RemoteSent, ObserveRemote,
        ForgetRemote, RemoteWithdrawn,
    ],
    "checks" => [
        RecordCheck, CheckStatus, FlakyTests, ForgetFlakyTest, AutoCheckEnable,
        AutoCheckDisable, LandingReadiness, RecordLanding, TakeTurn, TurnQueue, ReleaseTurn,
        LeaveTurnQueue,
    ],
    "graph" => [
        ObserveInterface, ObserveInterfaceDependents, InterfaceShown, BetweenSteps,
        AcknowledgeInterface, GraphSymbol, GraphReferences, GraphImpact, GraphBlindSpots,
        GraphPath, GraphMap, GraphContext, GraphInflight, GraphExport, GraphImport, CheckWrite,
        CheckCalls,
    ],
    "briefing" => [Brief, BriefPrepared, ForgetTold, ConfirmTold, Trails],
    "hosts" => [ObserveHosts],
    "projections" => [
        Status, Dashboard, StatusLine, StatusRecords, Version, Events, Changes, Compact, Today,
        Report, Count, History,
    ],
}

/// Rewrites every golden file from this run's decisions.
#[test]
#[ignore = "rewrites the golden files; run on purpose, then review the diff"]
fn bless() {
    for domain in DOMAINS {
        let path = golden_dir().join(format!("{domain}.golden"));
        fs::write(&path, decisions(domain)).expect("write the golden file");
    }
}

#[test]
fn decisions_match_their_golden() {
    let failures: Vec<String> = DOMAINS
        .iter()
        .filter_map(|domain| check(domain).err())
        .collect();
    assert!(failures.is_empty(), "{}\n{BLESS}", failures.join("\n"));
}

/// Every command, prepared inputs included, has at least one step in its
/// own domain's file, and the directory holds only those domains' files.
#[test]
fn every_command_has_a_golden_decision() {
    let mut covered = BTreeSet::new();
    for domain in DOMAINS {
        for (_, _, command) in steps(domain) {
            let (owner, variant) = case(&command);
            if owner == *domain {
                covered.insert(variant);
            }
        }
    }
    let missing: Vec<_> = CASES
        .iter()
        .filter(|(_, variant)| !covered.contains(variant))
        .map(|(domain, variant)| format!("{variant} (in {domain}.steps.jsonl)"))
        .collect();
    assert!(
        missing.is_empty(),
        "commands without a golden decision: {missing:?}"
    );

    let expected: BTreeSet<String> = DOMAINS
        .iter()
        .flat_map(|domain| [format!("{domain}.steps.jsonl"), format!("{domain}.golden")])
        .collect();
    let present: BTreeSet<String> = fs::read_dir(golden_dir())
        .expect("read the golden directory")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        present, expected,
        "the golden directory holds one steps and one golden file per domain"
    );
}
