//! Source graph and interface notices. Their record types and rules, and the
//! `decide_checked` and `apply` arms of their commands and events, which call
//! these functions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::*;

/// A symbol definition found by structural indexing. `digest` is the content
/// of the file it was read from; a match is by name and kind only.
///
/// The fields after `digest` are present only when a lookup asked for
/// detail (`GraphSymbol { detail: true }`); otherwise the record is the one
/// every earlier version sent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolRef {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub end_line: u32,
    pub signature: String,
    pub exported: bool,
    pub digest: String,
    /// SHA-256 of the definition's own bytes as indexed: a change elsewhere
    /// in the file leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_digest: Option<String>,
    /// Distinct names called within the definition's lines, first call
    /// first, at most `MAX_SYMBOL_CALLS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<Vec<CallCount>>,
    /// Call sites outside this definition whose callee has this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callers: Option<Callers>,
    /// Notes whose paths include this file, newest first, at most
    /// `MAX_SYMBOL_NOTES`. Their text is data, never instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<SymbolNote>>,
    /// The newest conclusive check record whose tree holds this file's
    /// indexed content. Absent when none was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<LastCheck>,
    /// In a ranked answer, why this definition ranks where it does, in
    /// rank order; empty for path order alone (roadmap C2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_reasons: Option<Vec<RankReason>>,
    /// In a ranked answer, the files other than its own that mention this
    /// definition's name, ranked, at most `CONTEXT_FILES`. Found by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependents: Option<Vec<Dependent>>,
    /// In a ranked answer, how many files other than its own mention the
    /// name: `dependents` and those left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependent_count: Option<u32>,
}

/// Distinct callees listed for one definition.
pub const MAX_SYMBOL_CALLS: u32 = 32;
/// Caller locations listed for one definition; all of them are counted.
pub const MAX_CALLER_SITES: u32 = 8;
/// Notes listed for one definition.
pub const MAX_SYMBOL_NOTES: usize = 4;

/// One name called within a definition: how often, and the line of the
/// first call. A name, not a resolved target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallCount {
    pub name: String,
    pub count: u32,
    pub line: u32,
}

/// Call sites of a name, found by name only. `sites` are `path:line`, in
/// path and line order, at most `MAX_CALLER_SITES`; `count` is all of them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Callers {
    pub count: u32,
    pub sites: Vec<String>,
}

/// A remembered note, quoted for a symbol lookup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolNote {
    pub id: String,
    pub text: String,
}

/// A check record named by a symbol lookup: its name, `passed` or
/// `failed`, and the tree it ran on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastCheck {
    pub name: String,
    pub result: String,
    pub tree: String,
}

/// A place a name is mentioned. Not a resolved use: any symbol of that name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NameRef {
    pub path: String,
    pub name: String,
    pub line: u32,
    pub digest: String,
}

/// The deepest `graph impact` level a client may ask for (roadmap D2).
pub const MAX_IMPACT_DEPTH: u32 = 3;

/// One level of a multi-level impact answer (roadmap D2): the files that
/// mention a name the previous level's files export. Found by name, not
/// resolved, so each level compounds the false positives of the one before.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImpactLevel {
    /// From 1: level 1 mentions an export of the queried file.
    pub level: u32,
    pub files: Vec<ImpactFile>,
    /// A bound, the frame size or the request deadline cut this level.
    pub truncated: bool,
}

/// A file reached at its first level, and how.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImpactFile {
    pub path: String,
    /// The exported name of `from` that this file mentions.
    pub via: String,
    /// The file at the previous level (the queried file for level 1).
    pub from: String,
}

/// The most `calls` steps a `graph path` chain may have, and the number
/// asked for when none is given (roadmap D3).
pub const MAX_PATH_HOPS: u32 = 6;
pub const DEFAULT_PATH_HOPS: u32 = 4;
/// Chains one `graph path` answer lists at most.
pub const MAX_PATHS: usize = 5;
/// Edges one `graph path` search visits at most: one per definition a call
/// may reach, and one per called name with no definition.
pub const MAX_PATH_EDGES: u32 = 10_000;
/// The deepest `graph map` area, and the areas and edges it lists at most.
pub const MAX_MAP_DEPTH: u32 = 3;
pub const MAX_MAP_AREAS: usize = 64;
pub const MAX_MAP_EDGES: usize = 256;

/// One step of a `graph path` chain (roadmap D3): a definition, and the
/// call in it that the next step's definition answers to, or, for the last
/// step, the definition alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathStep {
    pub path: String,
    /// The call's line, or for the last step the definition's first line.
    pub line: u32,
    /// The definition's name.
    pub name: String,
    /// `calls`, or `defines` for the last step.
    pub edge: String,
    /// The called name, which the next step defines. Absent on the last step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee: Option<String>,
    /// How this step's definition was found: `name` (every function,
    /// method or type of the name; the first step is always one) or `path`
    /// (only those in files the calling file imports, resolved by path).
    pub resolution: String,
    /// The name had more than one such definition, so this step may be one
    /// the program never takes.
    pub ambiguous: bool,
}

/// One chain of calls from `graph path`'s first name to its second.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolPath {
    pub steps: Vec<PathStep>,
    /// Any step is `ambiguous`.
    pub ambiguous: bool,
}

/// One area of a `graph map` (roadmap D3): the files whose module path
/// starts with `path`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MapArea {
    pub path: String,
    pub files: u32,
    /// Definitions visible outside their file.
    pub exported: u32,
    /// Indexed languages of its files, in name order.
    pub languages: Vec<String>,
}

/// Edges of one kind from one `graph map` area to another, counted.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MapEdge {
    pub from: String,
    pub to: String,
    /// `imports` (resolved by path) or `calls` (to a name with exactly one
    /// function, method or type definition, found by name).
    pub kind: String,
    pub resolution: String,
    pub count: u32,
}

/// How much of a workspace the index covers, so a short answer is not read
/// as a complete one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphCoverage {
    /// Files in the last complete scan in a supported language.
    pub indexable_files: u32,
    /// Of those, indexed at exactly their scanned content.
    pub current_files: u32,
    /// Indexed with a syntax error or a bound reached.
    pub partial_files: u32,
    /// False when no complete scan exists: nothing is known.
    pub scanned: bool,
    /// Supported-language files the index cannot read in full at their
    /// current content (partial, waiting, too large, left out of the scan).
    /// Every query matches names across the whole workspace, so any of them
    /// may hold a match the answer lacks; above zero, an empty or short
    /// answer is weak. `graph blind-spots` names them. Absent when zero,
    /// and sent only to a client that asks for it (`blind_in_scope` on a
    /// lookup, or `graph_blind_spots`): earlier builds read coverage
    /// strictly and reject a field they do not know.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blind_in_scope: u32,
}

/// A file indexed with a syntax error or a bound reached.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartialFile {
    pub path: String,
    /// Wall-clock milliseconds since which its indexed versions have been
    /// partial, on the coordinator's machine.
    pub since_ms: u64,
}

/// Other source files the index has no grammar for, by extension.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanguageCount {
    pub extension: String,
    pub files: u32,
}

/// Full counts of each blind-spot list, however many paths are returned.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlindTotals {
    pub partial: u32,
    pub never_clean: u32,
    pub waiting: u32,
    pub too_large: u32,
    pub unscanned: u32,
}

/// Where the index is blind, by reason. A file can be in more than one
/// list (a partial file can also be waiting for its newer version).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlindSpots {
    /// Indexed with a syntax error or a bound reached.
    pub partial: Vec<PartialFile>,
    /// Indexed, but never parsed cleanly: no interface baseline, so no
    /// interface notice can come from it.
    pub never_clean: Vec<String>,
    /// The scanned content is not indexed yet.
    pub waiting: Vec<String>,
    /// Over the index's 1 MiB bound (or the scan's 4 MiB one): never read.
    pub too_large: Vec<String>,
    /// Left out by the scan's file or byte bound.
    pub unscanned: Vec<String>,
    /// Other source files, most first, then by extension.
    pub unsupported_languages: Vec<LanguageCount>,
    pub totals: BlindTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterfaceChange {
    pub name: String,
    pub kind: String,
    /// `added`, `removed`, `signature_changed`, `renamed`, `definition_added`
    /// or `definition_removed`. `added` is a new exported name with its
    /// signature in `after`: kept in the log for a definition's history
    /// (roadmap D4), never held for briefings. A removal recorded since
    /// then carries the removed signature in `before`. `renamed` means the
    /// only definition of `name` went and one with the same signature but
    /// for the name came as `after_name`, with the evidence `evidence`
    /// names; with `file_moved`, the definition kept its name and moved with
    /// its whole file from `before_path` to the notice's path. The last two
    /// mean only the number of definitions sharing the name and kind
    /// changed: `after` (added) or `before` (removed) holds the signatures
    /// that came or went, and the counts say how many there were and are.
    pub change: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// The new name, for `renamed` only (roadmap B10). Absent from notices
    /// recorded before renames were told apart from removals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_name: Option<String>,
    /// The old name, for `renamed` only: always `name` (roadmap D7). Absent
    /// from notices recorded before it was kept. This field, `evidence`,
    /// `before_path` and `private` are left out of replies to protocol
    /// version 3 clients, and so is a change with `private` set
    /// (`protocol::encode_for` in the daemon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_name: Option<String>,
    /// What links the two, for `renamed` only (roadmap D7): `same_body`
    /// (the same body digest), `same_signature` (a changed body at the same
    /// first and last line) or `file_moved` (the whole file moved, with
    /// exactly the same content). Absent from renames recorded before it
    /// was kept, which were all `same_body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Where the file was before, for `file_moved` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_path: Option<String>,
    /// A definition that is not exported, for `file_moved` only: what the
    /// notice of a file with no exports lists, so that the file's notes and
    /// history follow it (roadmap D7). History only, like `added`: never
    /// held for briefings, recaps, break alerts or the dashboard.
    #[serde(default, skip_serializing_if = "is_false")]
    pub private: bool,
    /// Parameter counts of `before` and `after`, when each is exactly one
    /// function's signature (ADR 0029). Absent from notices recorded
    /// before they were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_arity: Option<Arity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_arity: Option<Arity>,
    /// Definitions with this name and kind before and after, for
    /// `definition_added` and `definition_removed` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_count: Option<u32>,
}

/// One recorded rename of an exported definition (roadmap D7): `name` went
/// from `path` and `after_name` came, linked by `evidence` (absent for a
/// rename recorded before it was kept).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rename {
    pub name: String,
    pub after_name: String,
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// The rename's new signature.
    pub after: String,
}

/// Renames remembered per workspace for `graph symbol --renamed`.
pub const MAX_RENAMES: usize = 256;
/// Renames one lookup follows at most, old name to newest.
pub const MAX_RENAME_HOPS: usize = 8;

/// The renames that took `name` to its newest name in `workspace_id`, in
/// order, at most `MAX_RENAME_HOPS`; `None` when no rename of it is
/// recorded. Each hop after the first is of the first one's kind, so an
/// unrelated definition's rename never continues the chain. A hop back to a
/// name already passed ends the chain.
pub fn renamed_to(state: &State, workspace_id: &str, name: &str) -> Option<Vec<Rename>> {
    let renames = state.renames.get(workspace_id)?;
    let mut hops: Vec<Rename> = Vec::new();
    let mut current = name;
    while hops.len() < MAX_RENAME_HOPS {
        let Some(hop) = renames.iter().find(|rename| {
            rename.name == current && hops.first().is_none_or(|first| first.kind == rename.kind)
        }) else {
            break;
        };
        if hop.after_name == name || hops.iter().any(|seen| seen.name == hop.after_name) {
            break;
        }
        hops.push(hop.clone());
        current = &hop.after_name;
    }
    (!hops.is_empty()).then_some(hops)
}

/// Definitions sharing one name and kind that a notice counts at most.
pub const MAX_DEFINITION_COUNT: u32 = 65_535;

/// How many arguments a function takes, read from its signature text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Arity {
    /// Parameters without a default, not variadic, not the receiver.
    pub min: u32,
    /// All parameters but the receiver; `None` when one is variadic.
    pub max: Option<u32>,
    /// The first parameter is an explicit receiver (Rust `self`, Python
    /// `self` or `cls`): passed by `a.f(...)`, written out by `T::f(a, ...)`.
    pub receiver: bool,
}

/// The largest parameter count an arity may state.
pub const MAX_ARITY: u32 = 255;

impl Arity {
    /// Whether a call with `args` arguments can be a call of this function.
    /// A call through a receiver (`member`) does not pass it; any other call
    /// may or may not, so both readings are accepted.
    pub fn fits(&self, args: u32, member: bool) -> bool {
        let within =
            |extra: u32| args >= self.min + extra && self.max.is_none_or(|max| args <= max + extra);
        within(0) || (self.receiver && !member && within(1))
    }
}

/// One call written by a tool call that has not landed yet: where, which
/// name, and how many arguments, from the hook's parse of the new text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallSite {
    pub path: String,
    pub line: u32,
    pub name: String,
    pub args: u32,
    /// Written after a receiver or module (`a.f(x)`).
    pub member: bool,
}

/// A written call that another live session's recent change makes wrong: its
/// argument count does not fit the new signature, or the function it names
/// was renamed or removed. A name match, not a resolved call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallBreak {
    pub notice_id: String,
    pub call: CallSite,
    /// The interface change: `signature_changed`, `renamed` or `removed`.
    pub change: String,
    /// The file whose exported signature changed.
    pub defined_in: String,
    /// The root of the other worktree of the repository whose index holds
    /// the change, when it is not the asking session's own workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub by_session: String,
    pub before: String,
    /// Empty for `removed`.
    pub after: String,
    /// The new name, for `renamed` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_name: Option<String>,
    pub before_arity: Option<Arity>,
    /// Always present for `signature_changed`.
    pub after_arity: Option<Arity>,
    pub age_ms: u64,
}

/// Calls one write check names at most.
pub const MAX_CALL_SITES: usize = 64;
/// Breaks one answer carries at most.
pub const MAX_CALL_BREAKS: usize = 8;
/// Notices remembered as alerted per session.
pub const MAX_ALERTED: usize = 64;
/// Notices remembered as told per session, by a briefing or between steps
/// (issue #214). A briefing tells at most `BRIEF_INTERFACE_NOTICES` at once.
pub const MAX_TOLD_NOTICES: usize = 64;
/// Interface notices one briefing or one between-step delivery tells.
pub const BRIEF_INTERFACE_NOTICES: usize = 4;
/// Prompts remembered per session as finished, for between-step watchers
/// (issue #214). A watcher lives within one turn, so a few suffice.
pub const MAX_STOPPED_PROMPTS: usize = 8;

/// How an interface notice reached a session's context (issue #214).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeVia {
    /// In a briefing at a session's start, a prompt, after a tool call or
    /// from `langspan_brief`.
    Briefing,
    /// While the session's turn ran, through a host's documented
    /// between-step mechanism, with no hook call of the session's own.
    BetweenSteps,
}

/// An interface notice a session was told, and how.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToldNotice {
    pub notice_id: String,
    pub via: NoticeVia,
}

/// An exported symbol of a file changed between two indexed versions, with
/// the files that mention the name. A rename shows as a removal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterfaceNotice {
    pub notice_id: String,
    pub workspace_id: String,
    pub path: String,
    pub changes: Vec<InterfaceChange>,
    /// Other files in the workspace that mention a changed name.
    pub dependents: Vec<String>,
    /// Files of other worktrees of the repository that mention a changed
    /// name, as their indexes find them after the notice, at most
    /// `NOTICE_DEPENDENTS` (issue #194). Absent from notices recorded
    /// before it was kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elsewhere: Vec<WorktreeFile>,
    /// The session that most recently reported editing `path`. A
    /// correlation, not proof of who made the change.
    pub by_session: Option<String>,
    pub at_ms: u64,
    pub generation: u64,
}

/// A file of one workspace, named by its path relative to that workspace's
/// root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct WorktreeFile {
    pub workspace_id: String,
    pub path: String,
}

/// An interface change that touches the asking session's own files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceImpact {
    pub path: String,
    pub by_session: Option<String>,
    pub changes: Vec<InterfaceChange>,
    /// The asking session's files that mention a changed name.
    pub affected: Vec<String>,
    pub age_ms: u64,
}

/// In-flight lines one briefing carries at most (roadmap C7).
pub const INFLIGHT_LINES: usize = 3;
/// Rendered bytes the in-flight lines of one briefing take together, at
/// most, by `inflight_cost`: their share of the briefing cap.
pub const INFLIGHT_BYTES: usize = 600;
/// In-flight symbols the daemon offers one briefing, at most.
pub const INFLIGHT_CANDIDATES: usize = 32;
/// The asking side's files named for one in-flight symbol, at most.
pub const INFLIGHT_MENTIONS: usize = 4;
/// In-flight signatures remembered as told per session. The daemon's
/// briefing window reads every in-flight row in turn, so this bounds how
/// many signatures still in flight a session hears once each; past it the
/// oldest is forgotten and told again.
pub const MAX_INFLIGHT_TOLD: usize = 1024;

/// An exported symbol that another worktree of the repository added, or
/// whose signature it changed, in a file that differs from that worktree's
/// `HEAD`: work not yet committed there (roadmap C7). Found by the daemon
/// in its source index by name, with the asking workspace's files that
/// mention the name or import a path ending in it or in the file's module.
/// The signature is repository data, never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InflightSymbol {
    /// The worktree's workspace, never the asking one.
    pub workspace_id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub mentioned_in: Vec<String>,
    /// The name `HEAD` gave this symbol, when the worktree renamed it
    /// (roadmap D7): files that mention either name are told. Left out of
    /// replies to protocol version 3 clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

/// An in-flight symbol as a briefing or `graph inflight` tells it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InflightNotice {
    pub path: String,
    /// The worktree's root.
    pub worktree: String,
    /// The session in that worktree that most recently reported editing
    /// `path`. A correlation, not proof of who made the change.
    pub session: Option<String>,
    pub name: String,
    pub kind: String,
    pub signature: String,
    /// The asking side's files that mention it.
    pub mentioned_in: Vec<String>,
    /// See [`InflightSymbol::renamed_from`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

/// What a symbol's in-flight line costs of `INFLIGHT_BYTES`: at least what
/// an adapter renders for it, quoting included, and never more than the
/// whole share, so the first line always fits.
pub fn inflight_cost(notice: &InflightNotice) -> usize {
    let quoted = |text: &str| text.len() + text.matches(['"', '\\']).count() + 4;
    let raw = [
        &notice.path,
        &notice.worktree,
        &notice.name,
        &notice.kind,
        &notice.signature,
    ]
    .into_iter()
    .map(|text| quoted(text))
    .sum::<usize>()
        + notice.session.as_deref().map_or(0, quoted)
        // ` (renamed from "…")`.
        + notice.renamed_from.as_deref().map_or(0, |from| quoted(from) + 16)
        + notice
            .mentioned_in
            .iter()
            .map(|path| quoted(path))
            .sum::<usize>();
    (200 + raw).min(INFLIGHT_BYTES)
}

/// Whether this signature of `item` was already told to `session_id`.
pub fn inflight_told(state: &State, session_id: &str, item: &InflightSymbol) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.inflight_told.contains(&inflight_key(item)))
}

/// The worktree root and reporting session of an in-flight symbol, from
/// projected state; `None` when its workspace is not registered.
pub fn inflight_notice(state: &State, item: &InflightSymbol) -> Option<InflightNotice> {
    let workspace = state.workspaces.get(&item.workspace_id)?;
    let session = state
        .activity
        .values()
        .filter(|activity| activity.generation == state.generation)
        .filter(|activity| {
            state
                .sessions
                .get(&activity.session_id)
                .is_some_and(|session| session.workspace_id == item.workspace_id)
        })
        .filter_map(|activity| {
            let touch = activity
                .touched
                .iter()
                .find(|touch| touch.path == item.path)?;
            Some((touch.at_ms, &activity.session_id))
        })
        .max()
        .map(|(_, session)| session.clone());
    Some(InflightNotice {
        path: item.path.clone(),
        worktree: workspace.root.clone(),
        session,
        name: item.name.clone(),
        kind: item.kind.clone(),
        signature: item.signature.clone(),
        mentioned_in: item.mentioned_in.clone(),
        renamed_from: item.renamed_from.clone(),
    })
}

pub const MAX_NOTICES: usize = 32;
pub const NOTICE_CHANGES: usize = 16;
pub const NOTICE_DEPENDENTS: usize = 32;

/// A code-context query names at most this many identifiers.
pub const CONTEXT_NAMES: usize = 12;
/// Definitions returned per name, at most.
pub const CONTEXT_DEFINITIONS: u32 = 3;
/// Distinct files that mention a name, returned per name, at most.
pub const CONTEXT_FILES: u32 = 5;

/// One name asked about in a code-context query, with where it is defined
/// and which files mention it. A name match, not a resolved symbol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NameContext {
    pub name: String,
    pub definitions: Vec<SymbolRef>,
    /// Distinct paths, in path order.
    pub mentioned_in: Vec<String>,
    /// True when more definitions or mentioning files exist than are listed.
    pub truncated: bool,
    /// In a ranked answer, how many definitions the name has: those listed
    /// and those left out (roadmap C2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_count: Option<u32>,
}

/// What a notice's renames and moves change in the state (roadmap D7). A
/// rename is remembered under its old name and kind, replacing an earlier
/// rename of that name and kind; a name that comes back as the
/// same kind (added, or the new name of another rename) is forgotten as
/// renamed. A file moved
/// whole takes its notes along: each note about the old path is about the
/// new one instead.
fn follow_renames(state: &mut State, notice: &InterfaceNotice) {
    for change in &notice.changes {
        let moved = change.evidence.as_deref() == Some(FILE_MOVED);
        let renames = state
            .renames
            .entry(notice.workspace_id.clone())
            .or_default();
        let back = match (change.change.as_str(), &change.after_name) {
            ("added", _) => Some(&change.name),
            ("renamed", Some(to)) if !moved => Some(to),
            _ => None,
        };
        if let Some(back) = back {
            // Only a definition of the renamed one's kind takes its name
            // back, as the lookup's own check does.
            renames.retain(|rename| rename.name != *back || rename.kind != change.kind);
        }
        if let ("renamed", Some(to), false) = (change.change.as_str(), &change.after_name, moved) {
            renames.retain(|rename| rename.name != change.name || rename.kind != change.kind);
            renames.insert(
                0,
                Rename {
                    name: change.name.clone(),
                    after_name: to.clone(),
                    kind: change.kind.clone(),
                    path: notice.path.clone(),
                    evidence: change.evidence.clone(),
                    after: change.after.clone().unwrap_or_default(),
                },
            );
            renames.truncate(MAX_RENAMES);
        }
        if renames.is_empty() {
            state.renames.remove(&notice.workspace_id);
        }
        let Some(from) = change.before_path.as_ref().filter(|_| moved) else {
            continue;
        };
        for note in state
            .notes
            .get_mut(&notice.workspace_id)
            .into_iter()
            .flatten()
        {
            // Sorted and once each, as a note's paths are recorded.
            if let Some(at) = note.paths.iter().position(|path| path == from) {
                note.paths[at].clone_from(&notice.path);
                note.paths.sort();
                note.paths.dedup();
            }
        }
    }
}

/// What links a rename's two names (roadmap D7).
pub const RENAME_EVIDENCE: [&str; 3] = ["same_body", "same_signature", FILE_MOVED];
/// The evidence of a definition that moved with its whole file.
pub const FILE_MOVED: &str = "file_moved";

/// A rename's evidence in words, as a notice ends its rename: ` (same
/// body)`, ` (same signature)`, or nothing for a rename recorded before its
/// evidence was kept and for a moved file, whose text says so itself.
pub fn rename_evidence_words(evidence: Option<&str>) -> &'static str {
    match evidence {
        Some("same_body") => " (same body)",
        Some("same_signature") => " (same signature)",
        _ => "",
    }
}

/// A rename's old name, evidence and old path (roadmap D7): each only on a
/// rename, the old name always `name`, and an old path exactly when the
/// file moved, other than the notice's path. A rename without them was
/// recorded before they were kept and is read as it was.
pub(crate) fn validate_rename_record(
    notice: &InterfaceNotice,
    change: &InterfaceChange,
) -> Result<(), CoreError> {
    let renamed = change.change == "renamed";
    let moved = change.evidence.as_deref() == Some(FILE_MOVED);
    let shaped = change
        .before_name
        .as_ref()
        .is_none_or(|name| renamed && *name == change.name)
        && change
            .evidence
            .as_deref()
            .is_none_or(|evidence| renamed && RENAME_EVIDENCE.contains(&evidence))
        && match &change.before_path {
            Some(path) => moved && *path != notice.path,
            None => !moved,
        }
        && (!change.private || moved);
    if !shaped {
        return Err(error(
            "invalid_notice",
            "A rename's `before_name` is its name, its `evidence` is `same_body`, `same_signature` or `file_moved`, and only `file_moved` has a `before_path`, another file than the notice's, or is `private`",
        ));
    }
    if let Some(path) = &change.before_path {
        validate_relative_path(path)?;
    }
    Ok(())
}

/// The identifier grammar the index is asked about: an ASCII letter or
/// underscore, then 2 to 63 ASCII letters, digits or underscores.
pub fn is_graph_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (3..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

/// Bounds of a code-context query: 1 to `CONTEXT_NAMES` distinct names in
/// the identifier grammar, and 1 to `CONTEXT_DEFINITIONS` definitions each.
pub fn validate_graph_context(names: &[String], limit: u32) -> Result<(), CoreError> {
    let distinct: BTreeSet<&String> = names.iter().collect();
    if names.is_empty()
        || names.len() > CONTEXT_NAMES
        || distinct.len() != names.len()
        || !names.iter().all(|name| is_graph_name(name))
        || !(1..=CONTEXT_DEFINITIONS).contains(&limit)
    {
        return Err(error(
            "invalid_graph_context",
            "Ask about 1 to 12 distinct names, each 3 to 64 ASCII letters, digits or underscores not starting with a digit, with a limit of 1 to 3 definitions per name",
        ));
    }
    Ok(())
}

fn validate_alerted(notice_ids: &[String]) -> Result<(), CoreError> {
    if notice_ids.is_empty() || notice_ids.len() > MAX_CALL_BREAKS {
        return Err(error(
            "invalid_alert",
            "An interface alert names 1 to 8 notices",
        ));
    }
    notice_ids.iter().try_for_each(|id| validate_request_id(id))
}

/// The notices one briefing or one between-step delivery told: 1 to
/// `BRIEF_INTERFACE_NOTICES` valid IDs (issue #214).
fn validate_outlined(path: &str, digest: &str) -> Result<(), CoreError> {
    validate_relative_path(path)?;
    if !is_hex_digest(digest, 64) {
        return Err(error(
            "invalid_digest",
            "A file digest must contain exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

/// Whether the session was given the outline of this file version
/// (roadmap A11).
pub fn was_outlined(state: &State, session_id: &str, path: &str, digest: &str) -> bool {
    state.activity.get(session_id).is_some_and(|activity| {
        activity
            .outlined
            .iter()
            .any(|held| held.path == path && held.digest == digest)
    })
}

fn validate_told(notice_ids: &[String]) -> Result<(), CoreError> {
    if notice_ids.is_empty() || notice_ids.len() > BRIEF_INTERFACE_NOTICES {
        return Err(error(
            "invalid_told",
            "A delivery tells 1 to 4 interface notices",
        ));
    }
    notice_ids.iter().try_for_each(|id| validate_request_id(id))
}

pub(crate) fn validate_arity(arity: &Arity) -> Result<(), CoreError> {
    if arity.min > MAX_ARITY
        || arity
            .max
            .is_some_and(|max| max < arity.min || max > MAX_ARITY)
    {
        return Err(error(
            "invalid_notice",
            "A parameter count is at most 255, and its minimum is at most its maximum",
        ));
    }
    Ok(())
}

/// The calls of one write check: 1 to `MAX_CALL_SITES`, each in a relative
/// path, on a line from 1 to 2,000,000, naming 1 to 128 bytes without
/// control characters, with at most `MAX_ARITY` arguments.
pub fn validate_calls(calls: &[CallSite]) -> Result<(), CoreError> {
    let invalid = || {
        error(
            "invalid_calls",
            "A write check names 1 to 64 calls, each in a workspace-relative path, on a line from 1 to 2000000, a name of 1 to 128 bytes without control characters, and at most 255 arguments",
        )
    };
    if calls.is_empty() || calls.len() > MAX_CALL_SITES {
        return Err(invalid());
    }
    for call in calls {
        validate_relative_path(&call.path).map_err(|_| invalid())?;
        if !(1..=2_000_000).contains(&call.line)
            || call.name.is_empty()
            || call.name.len() > 128
            || call.name.chars().any(char::is_control)
            || call.args > MAX_ARITY
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Where a notice of another worktree stands against the asking session's
/// checkout, as Git answers it (issue #217).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The notice's file differs from its worktree's `HEAD`: not committed.
    Uncommitted,
    /// Committed on its worktree's branch, in a commit the asking
    /// checkout's `HEAD` does not contain.
    Unmerged,
    /// The asking checkout's `HEAD` contains the commit.
    Merged,
}

/// Standings by `(workspace ID, notice ID)`. A notice missing from it, such
/// as one Git could not answer for in time, keeps the uncommitted rules.
pub type Standings = BTreeMap<(String, String), Standing>;

/// Notices of other worktrees whose standing one write check asks Git for,
/// at most.
pub const MAX_STANDING_QUESTIONS: usize = 8;

/// A notice of another worktree whose standing a write check asks Git for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingQuestion {
    pub workspace_id: String,
    pub notice_id: String,
    /// The notice's file, relative to that worktree's root.
    pub path: String,
}

/// The notices of other worktrees that `calls` could be alerted to if they
/// were committed and not merged: those the uncommitted rules would also
/// keep, and those they leave out only because the change is older than the
/// briefing horizon or its session has ended. At most
/// `MAX_STANDING_QUESTIONS`, each workspace's newest first. Usually none, and
/// then Git is not asked.
pub fn standing_questions(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    now_ms: u64,
) -> Result<Vec<StandingQuestion>, CoreError> {
    validate_calls(calls)?;
    let own = &current_session(state, session_id)?.workspace_id;
    let mut questions: Vec<StandingQuestion> = Vec::new();
    for (notice, change, _) in alertable(state, session_id, now_ms, None)? {
        let asked = questions.iter().any(|question| {
            question.workspace_id == notice.workspace_id && question.notice_id == notice.notice_id
        });
        if notice.workspace_id == *own
            || asked
            || !calls.iter().any(|call| call.name == change.name)
        {
            continue;
        }
        if questions.len() == MAX_STANDING_QUESTIONS {
            break;
        }
        questions.push(StandingQuestion {
            workspace_id: notice.workspace_id.clone(),
            notice_id: notice.notice_id.clone(),
            path: notice.path.clone(),
        });
    }
    Ok(questions)
}

/// Changes to functions that another session made in this session's
/// workspace or in another worktree of its repository (issue #194), in this
/// generation, that this session has not been alerted to: `(notice, change,
/// the peer)`. A signature change counts only with the new parameter count;
/// a rename and a removal always count. The session's own workspace first,
/// each workspace's newest notice first.
///
/// An uncommitted change counts within the briefing horizon while its
/// session is live. Another worktree's change that `standings` says is
/// committed and not merged into this session's checkout counts whatever
/// its age and whether or not its session has ended, and one it says is
/// merged never counts (issue #217). With `standings` `None` (asking which
/// notices Git should answer for), every other worktree's change counts
/// as if committed and not merged.
fn alertable<'a>(
    state: &'a State,
    session_id: &str,
    now_ms: u64,
    standings: Option<&Standings>,
) -> Result<Vec<(&'a InterfaceNotice, &'a InterfaceChange, &'a str)>, CoreError> {
    let me = current_session(state, session_id)?;
    let alerted = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .map(|activity| activity.alerted.as_slice())
        .unwrap_or(&[]);
    let live = |peer: &str| {
        state
            .sessions
            .get(peer)
            .is_some_and(|session| session.generation == state.generation)
            && state
                .session_liveness
                .get(peer)
                .is_none_or(|liveness| liveness.status != LivenessStatus::Exited)
            && state
                .activity
                .get(peer)
                .is_none_or(|activity| !activity.ended)
    };
    let mut found = Vec::new();
    let own = state.interface_notices.get(&me.workspace_id);
    let others = state
        .interface_notices
        .iter()
        .filter(|(workspace_id, _)| {
            **workspace_id != me.workspace_id
                && related_workspace(state, &me.workspace_id, workspace_id)
        })
        .map(|(_, notices)| notices);
    for notice in own.into_iter().chain(others).flatten() {
        let Some(peer) = notice.by_session.as_deref() else {
            continue;
        };
        let standing = if notice.workspace_id == me.workspace_id {
            Some(Standing::Uncommitted)
        } else {
            match standings {
                None => Some(Standing::Unmerged),
                Some(standings) => standings
                    .get(&(notice.workspace_id.clone(), notice.notice_id.clone()))
                    .copied(),
            }
        };
        let committed = match standing {
            Some(Standing::Merged) => continue,
            Some(Standing::Unmerged) => true,
            Some(Standing::Uncommitted) | None => false,
        };
        if notice.generation != state.generation
            || peer == session_id
            || alerted.contains(&notice.notice_id)
            || (!committed
                && (now_ms.saturating_sub(notice.at_ms) > BRIEF_HORIZON_MS || !live(peer)))
        {
            continue;
        }
        for change in &notice.changes {
            let alerts = match change.change.as_str() {
                "signature_changed" => change.after_arity.is_some(),
                "renamed" => change.after_name.is_some(),
                "removed" => true,
                _ => false,
            };
            if alerts && matches!(change.kind.as_str(), "function" | "method") {
                found.push((notice, change, peer));
            }
        }
    }
    Ok(found)
}

/// The names among `calls` that a write check must look up in the index,
/// by the workspace whose index holds the change: those of a change it
/// could alert to, and a renamed function's new name. `standings` holds
/// Git's answers for `standing_questions`. Usually none, and then nothing
/// is looked up.
pub fn call_candidates(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    standings: &Standings,
    now_ms: u64,
) -> Result<BTreeMap<String, BTreeSet<String>>, CoreError> {
    validate_calls(calls)?;
    let mut candidates: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (notice, change, _) in alertable(state, session_id, now_ms, Some(standings))? {
        if calls.iter().any(|call| call.name == change.name) {
            candidates
                .entry(notice.workspace_id.clone())
                .or_default()
                .extend(
                    [&change.name]
                        .into_iter()
                        .chain(&change.after_name)
                        .cloned(),
                );
        }
    }
    Ok(candidates)
}

/// What one workspace's index holds of a write check's candidate names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexedNames {
    /// Every definition of a candidate name.
    pub definitions: Vec<SymbolRef>,
    /// The index reads every supported file of the workspace at its current
    /// content (`GraphCoverage::blind_in_scope` is zero).
    pub complete: bool,
}

/// The calls a peer's recent change makes wrong, at most `MAX_CALL_BREAKS`,
/// from notices this session was not alerted to. `current` holds, by
/// workspace, the index's definitions of every candidate name
/// (`call_candidates`), and a change counts only while it is still true in
/// the index of the workspace it was made in, so a notice the index has
/// moved past is never used, and a notice of a workspace missing from
/// `current` is not used at all:
///
/// - a signature change, while its file holds the new signature; a call
///   whose count does not fit it breaks;
/// - a rename, while its file holds the new name's signature and no
///   definition of the old name is left anywhere; every call of the old
///   name breaks;
/// - a removal, while no definition of the name is left anywhere; every
///   call of it breaks.
///
/// "No definition left anywhere" is read from that workspace's index, so a
/// rename or a removal counts only when it is `complete`. A name moved into
/// a file not indexed yet, partial or too large is otherwise missing from
/// `current` without being gone. A change made in another worktree is
/// judged in that worktree: the session's own checkout still defines the
/// old name until the branches meet, which is the break being told.
///
/// A same-count change is not alerted: calls are matched by name, and
/// without the argument types at a call site nothing tells a right call
/// from a wrong one (ADR 0029). `standings` is as for `call_candidates`.
/// Pure; the caller records the alert.
pub fn call_breaks(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    standings: &Standings,
    current: &BTreeMap<String, IndexedNames>,
    now_ms: u64,
) -> Result<Vec<CallBreak>, CoreError> {
    validate_calls(calls)?;
    let own_workspace = &current_session(state, session_id)?.workspace_id;
    let mut breaks = Vec::new();
    for (notice, change, peer) in alertable(state, session_id, now_ms, Some(standings))? {
        let Some(indexed) = current.get(&notice.workspace_id) else {
            continue;
        };
        let gone = |name: &str| {
            indexed.complete && !indexed.definitions.iter().any(|symbol| symbol.name == name)
        };
        let holds = |path: &str, name: &str, signature: &str| {
            indexed.definitions.iter().any(|symbol| {
                symbol.path == path && symbol.name == name && symbol.signature == signature
            })
        };
        let worktree = (notice.workspace_id != *own_workspace)
            .then(|| state.workspaces.get(&notice.workspace_id))
            .flatten()
            .map(|workspace| workspace.root.clone());
        let after = change.after.as_deref().unwrap_or("");
        // `Some`: a call breaks when its count does not fit; `None`: every
        // call of the name breaks.
        let fit: Option<Arity> = match change.change.as_str() {
            "signature_changed" => match change.after_arity {
                Some(arity) if holds(&notice.path, &change.name, after) => Some(arity),
                _ => continue,
            },
            "renamed" => match &change.after_name {
                Some(to) if holds(&notice.path, to, after) && gone(&change.name) => None,
                _ => continue,
            },
            "removed" if gone(&change.name) => None,
            _ => continue,
        };
        for call in calls.iter().filter(|call| {
            call.name == change.name && fit.is_none_or(|arity| !arity.fits(call.args, call.member))
        }) {
            if breaks.len() == MAX_CALL_BREAKS {
                return Ok(breaks);
            }
            breaks.push(CallBreak {
                notice_id: notice.notice_id.clone(),
                call: call.clone(),
                change: change.change.clone(),
                defined_in: notice.path.clone(),
                worktree: worktree.clone(),
                by_session: peer.to_owned(),
                before: change.before.clone().unwrap_or_default(),
                after: after.to_owned(),
                after_name: change.after_name.clone(),
                before_arity: change.before_arity,
                after_arity: change.after_arity,
                age_ms: now_ms.saturating_sub(notice.at_ms),
            });
        }
    }
    Ok(breaks)
}

#[allow(clippy::ptr_arg)]
pub(crate) fn decide_acknowledge_interface(
    state: &State,
    session_id: &String,
    notice_ids: &Vec<String>,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    validate_alerted(notice_ids)?;
    Ok(Decision {
        events: vec![Event::InterfaceAlerted {
            session_id: session_id.clone(),
            notice_ids: notice_ids.clone(),
        }],
        outcome: Outcome::InterfaceAcknowledged {
            session_id: session_id.clone(),
            notice_ids: notice_ids.clone(),
        },
    })
}

#[allow(clippy::ptr_arg)]
pub(crate) fn decide_interface_shown(
    state: &State,
    session_id: &String,
    notice_ids: &Vec<String>,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    validate_told(notice_ids)?;
    // A notice told before, either way, is not recorded twice.
    let newly_told: Vec<String> = notice_ids
        .iter()
        .filter(|id| !told_notices(state, session_id).any(|told| told.notice_id == **id))
        .cloned()
        .collect();
    let events = if newly_told.is_empty() {
        Vec::new()
    } else {
        vec![Event::InterfaceNoticesTold {
            session_id: session_id.clone(),
            notice_ids: newly_told,
            via: NoticeVia::Briefing,
        }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::InterfaceAcknowledged {
            session_id: session_id.clone(),
            notice_ids: notice_ids.clone(),
        },
    })
}

// The daemon sends this only for a file it found large and indexed
// whole at `digest`; here the session's own record decides.
#[allow(clippy::ptr_arg)]
pub(crate) fn decide_outline_read(
    state: &State,
    session_id: &String,
    path: &String,
    digest: &String,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    validate_outlined(path, digest)?;
    if was_outlined(state, session_id, path, digest) {
        return Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::ReadOutline {
                outline: None,
                definitions: 0,
                skipped: Some("shown".into()),
            },
        });
    }
    Ok(Decision {
        events: vec![Event::ReadOutlined {
            session_id: session_id.clone(),
            path: path.clone(),
            digest: digest.clone(),
        }],
        outcome: Outcome::ReadOutline {
            outline: Some(Vec::new()),
            definitions: 0,
            skipped: None,
        },
    })
}

pub(crate) fn decide_between_steps(
    state: &State,
    session_id: &String,
    stops: &Option<u64>,
    prompt_id: &Option<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let me = known_session(state, session_id)?;
    if let Some(prompt_id) = prompt_id {
        validate_request_id(prompt_id)?;
    }
    let activity = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation);
    let now_stops = activity.map_or(0, |activity| activity.stops);
    // The watcher's turn is over once its own prompt's reply
    // finished, whenever its first answer came, or once any reply
    // finished after its first ask; an ended or earlier-generation
    // session has none.
    let open = me.generation == state.generation
        && !activity.is_some_and(|activity| activity.ended)
        && !prompt_id.as_ref().is_some_and(|prompt| {
            activity.is_some_and(|activity| activity.stopped_prompts.contains(prompt))
        })
        && stops.is_none_or(|stops| stops == now_stops);
    let (notice_ids, interface_changes): (Vec<String>, Vec<InterfaceImpact>) = if open {
        let alerted = activity.map_or(&[][..], |activity| activity.alerted.as_slice());
        let concerned = concerned_paths(state, session_id, now_ms);
        interface_impacts(state, me, &concerned, now_ms)
            .filter(|(id, _)| {
                !alerted.contains(id)
                    && !told_notices(state, session_id).any(|told| told.notice_id == *id)
            })
            .take(BRIEF_INTERFACE_NOTICES)
            .unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    let events = if notice_ids.is_empty() {
        Vec::new()
    } else {
        vec![Event::InterfaceNoticesTold {
            session_id: session_id.clone(),
            notice_ids,
            via: NoticeVia::BetweenSteps,
        }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::BetweenSteps {
            session_id: session_id.clone(),
            open,
            stops: now_stops,
            interface_changes,
        },
    })
}

#[allow(clippy::ptr_arg)]
pub(crate) fn decide_observe_interface(
    state: &State,
    workspace_id: &String,
    path: &String,
    changes: &Vec<InterfaceChange>,
    dependents: &Vec<String>,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    let notice = InterfaceNotice {
        notice_id: request_id.into(),
        workspace_id: workspace_id.clone(),
        path: path.clone(),
        changes: changes.clone(),
        dependents: dependents.clone(),
        elsewhere: Vec::new(),
        by_session: last_peer_edit(state, "", workspace_id, path, 0)
            .map(|(session, _)| session.to_owned()),
        at_ms: now_ms,
        generation: state.generation,
    };
    validate_notice(&notice)?;
    Ok(Decision {
        events: vec![Event::InterfaceChanged {
            notice: notice.clone(),
        }],
        outcome: Outcome::InterfaceObserved { notice },
    })
}

pub(crate) fn decide_observe_interface_dependents(
    state: &State,
    notice_workspace_id: &String,
    notice_id: &String,
    workspace_id: &String,
    paths: &Vec<String>,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, notice_workspace_id)?;
    validate_known_workspace(state, workspace_id)?;
    if workspace_id == notice_workspace_id
        || !related_workspace(state, notice_workspace_id, workspace_id)
    {
        return Err(error(
            "invalid_notice",
            "Dependents found elsewhere are files of another worktree of the notice's repository",
        ));
    }
    if paths.len() > NOTICE_DEPENDENTS {
        return Err(error(
            "invalid_notice",
            "At most 32 dependent files are reported at once",
        ));
    }
    for path in paths {
        validate_relative_path(path)?;
    }
    let notice = state
        .interface_notices
        .get(notice_workspace_id)
        .and_then(|notices| notices.iter().find(|n| n.notice_id == *notice_id))
        .ok_or_else(|| {
            error(
                "unknown_notice",
                "The interface notice is not held: a newer one for its file replaced it, or it was never recorded",
            )
        })?;
    let room = NOTICE_DEPENDENTS.saturating_sub(notice.elsewhere.len());
    let files: Vec<WorktreeFile> = paths
        .iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|path| WorktreeFile {
            workspace_id: workspace_id.clone(),
            path: path.clone(),
        })
        .filter(|file| !notice.elsewhere.contains(file))
        .take(room)
        .collect();
    Ok(Decision {
        outcome: Outcome::InterfaceDependentsObserved {
            notice_id: notice_id.clone(),
            added: files.len() as u32,
        },
        events: if files.is_empty() {
            Vec::new()
        } else {
            vec![Event::InterfaceDependentsFound {
                notice_workspace_id: notice_workspace_id.clone(),
                notice_id: notice_id.clone(),
                files,
            }]
        },
    })
}

pub(crate) fn apply_interface_changed(
    state: &mut State,
    notice: &InterfaceNotice,
) -> Result<(), CoreError> {
    validate_notice(notice)?;
    validate_known_workspace(state, &notice.workspace_id)?;
    // An added export is history only (roadmap D4): a new name is
    // news to no dependent, so it is not held for briefings, recaps,
    // break alerts or the dashboard, and an addition alone does not
    // replace the file's held notice or open a workspace's entry.
    // A private definition moved with its file is history only too.
    follow_renames(state, notice);
    let mut held = notice.clone();
    held.changes
        .retain(|change| change.change != "added" && !change.private);
    if !held.changes.is_empty() {
        let notices = state
            .interface_notices
            .entry(notice.workspace_id.clone())
            .or_default();
        // A newer notice for the same file replaces the older one.
        notices.retain(|older| older.path != held.path);
        notices.insert(0, held);
        notices.truncate(MAX_NOTICES);
    }
    Ok(())
}

pub(crate) fn apply_interface_dependents_found(
    state: &mut State,
    notice_workspace_id: &String,
    notice_id: &String,
    files: &Vec<WorktreeFile>,
) -> Result<(), CoreError> {
    if files.len() > NOTICE_DEPENDENTS {
        return Err(error(
            "invalid_event",
            "At most 32 dependent files are found at once",
        ));
    }
    for file in files {
        validate_relative_path(&file.path)?;
    }
    // A notice replaced or dropped since then has nothing to add to.
    if let Some(notice) = state
        .interface_notices
        .get_mut(notice_workspace_id)
        .and_then(|notices| notices.iter_mut().find(|n| n.notice_id == *notice_id))
    {
        for file in files {
            if notice.elsewhere.len() < NOTICE_DEPENDENTS && !notice.elsewhere.contains(file) {
                notice.elsewhere.push(file.clone());
            }
        }
    }
    Ok(())
}

#[allow(clippy::ptr_arg)]
pub(crate) fn apply_interface_alerted(
    state: &mut State,
    session_id: &String,
    notice_ids: &Vec<String>,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    validate_alerted(notice_ids)?;
    let alerted = &mut activity_mut(state, session_id).alerted;
    alerted.retain(|held| !notice_ids.contains(held));
    for id in notice_ids.iter().rev() {
        alerted.insert(0, id.clone());
    }
    alerted.truncate(MAX_ALERTED);
    Ok(())
}

#[allow(clippy::ptr_arg)]
pub(crate) fn apply_interface_notices_told(
    state: &mut State,
    session_id: &String,
    notice_ids: &Vec<String>,
    via: &NoticeVia,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    validate_told(notice_ids).map_err(|failure| error("invalid_event", &failure.message))?;
    let told = &mut activity_mut(state, session_id).notices_told;
    told.retain(|held| !notice_ids.contains(&held.notice_id));
    for id in notice_ids.iter().rev() {
        told.insert(
            0,
            ToldNotice {
                notice_id: id.clone(),
                via: *via,
            },
        );
    }
    told.truncate(MAX_TOLD_NOTICES);
    Ok(())
}

#[allow(clippy::ptr_arg)]
pub(crate) fn apply_read_outlined(
    state: &mut State,
    session_id: &String,
    path: &String,
    digest: &String,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    validate_outlined(path, digest).map_err(|failure| error("invalid_event", &failure.message))?;
    let outlined = &mut activity_mut(state, session_id).outlined;
    outlined.retain(|held| held.path != *path);
    outlined.insert(
        0,
        OutlinedRead {
            path: path.clone(),
            digest: digest.clone(),
        },
    );
    outlined.truncate(MAX_OUTLINED);
    Ok(())
}

#[allow(clippy::ptr_arg)]
pub(crate) fn apply_inflight_delivered(
    state: &mut State,
    session_id: &String,
    keys: &Vec<String>,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if keys.is_empty()
        || keys.len() > INFLIGHT_LINES
        || keys
            .iter()
            .any(|key| key.is_empty() || key.len() > 64 || !key.is_ascii())
    {
        return Err(error(
            "invalid_event",
            "A delivery names 1 to 3 in-flight keys of at most 64 ASCII bytes",
        ));
    }
    let told = &mut activity_mut(state, session_id).inflight_told;
    told.retain(|held| !keys.contains(held));
    for key in keys.iter().rev() {
        told.insert(0, key.clone());
    }
    told.truncate(MAX_INFLIGHT_TOLD);
    Ok(())
}
