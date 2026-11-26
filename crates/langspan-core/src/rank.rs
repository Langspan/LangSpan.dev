//! The order of a name's definitions and of the files that mention it in a
//! code briefing (roadmap C2). Fixed rules over core state, listed in
//! `docs/adr/0008-derived-source-index.md`: no learned weight, no clock but
//! the one passed in, and nothing recorded.
//!
//! A definition ranks first when it is in a file the asking session edited
//! or read, then in a file a live peer is editing, then when it is
//! exported, then when the prompt names its path or a qualifier of it, then
//! in path order. A mentioning file ranks by the same rules (a file is not
//! exported), then by how often it mentions the name, then by path.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    BRIEF_HORIZON_MS, CONTEXT_NAMES, CoreError, LivenessStatus, State, SymbolRef, error,
    validate_id, work_paths,
};

/// Definitions, and mentioning files, read in path or count order before
/// ranking, besides every one in the session's own and its peers' files.
/// One past this further along the path order is counted, not ranked.
pub const RANK_CANDIDATES: u32 = 256;
/// The longest path or qualifier a prompt may name for ranking.
pub const QUALIFIER_BYTES: usize = 128;

/// Why a definition or a mentioning file ranks where it does. Listed in
/// rank order; an empty list means path order alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum RankReason {
    /// The asking session edited the file, or read it without editing it
    /// (its recorded versions, ADR 0005).
    OwnFile { edited: bool },
    /// A live peer holds the file or edited it within the briefing horizon.
    PeerEditing { session_id: String },
    /// The definition is exported. Never given to a file.
    Exported,
    /// The prompt names the file's path, its name or stem, or a directory
    /// on its path, such as `store` in `store::upload`.
    QualifiedInPrompt,
}

/// A file that mentions a definition's name, with how often. A name match:
/// the mention may be of another symbol of the same name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Dependent {
    pub path: String,
    pub mentions: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rank_reasons: Vec<RankReason>,
}

/// What ranking reads: the asking session's files, its live peers' files
/// and what the prompt named. Paths are workspace-relative.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RankInputs {
    /// Files the asking session edited (`true`) or only read (`false`).
    pub own: BTreeMap<String, bool>,
    /// Files a live peer is editing, with that peer's session ID: the most
    /// recent editor when several are.
    pub peers: BTreeMap<String, String>,
    pub qualifiers: Vec<String>,
}

impl RankInputs {
    /// Every path that ranks by ownership: the index reads these besides
    /// its first `RANK_CANDIDATES`.
    pub fn paths(&self) -> Vec<String> {
        let paths: BTreeSet<&String> = self.own.keys().chain(self.peers.keys()).collect();
        paths.into_iter().cloned().collect()
    }

    fn qualified(&self, path: &str) -> bool {
        self.qualifiers.iter().any(|qualifier| {
            if qualifier.contains('/') {
                // A whole-component run of the path: its end, or a
                // directory on it.
                let path = format!("/{path}");
                return path.ends_with(&format!("/{qualifier}"))
                    || path.contains(&format!("/{qualifier}/"));
            }
            let parts: Vec<&str> = path.split('/').collect();
            let (file, directories) = parts.split_last().unwrap_or((&"", &[]));
            let stem = file.split('.').next().unwrap_or(file);
            directories.contains(&qualifier.as_str()) || *file == qualifier || stem == qualifier
        })
    }

    fn file_reasons(&self, path: &str) -> Vec<RankReason> {
        let mut reasons = Vec::new();
        if let Some(edited) = self.own.get(path) {
            reasons.push(RankReason::OwnFile { edited: *edited });
        }
        if let Some(session_id) = self.peers.get(path) {
            reasons.push(RankReason::PeerEditing {
                session_id: session_id.clone(),
            });
        }
        if self.qualified(path) {
            reasons.push(RankReason::QualifiedInPrompt);
        }
        reasons
    }
}

/// The rank of a list of reasons: lower first. Each rule outranks every
/// rule after it.
fn class(reasons: &[RankReason]) -> [bool; 4] {
    let has = |wanted: fn(&RankReason) -> bool| !reasons.iter().any(wanted);
    [
        has(|r| matches!(r, RankReason::OwnFile { .. })),
        has(|r| matches!(r, RankReason::PeerEditing { .. })),
        has(|r| matches!(r, RankReason::Exported)),
        has(|r| matches!(r, RankReason::QualifiedInPrompt)),
    ]
}

/// Bounds of the ranking fields of a code-context or symbol query: a valid
/// session ID, and at most `CONTEXT_NAMES` distinct relative paths or
/// qualifiers of 1 to `QUALIFIER_BYTES` letters, digits, `_`, `-`, `.` and
/// `/`.
pub fn validate_rank(session_id: Option<&str>, qualifiers: &[String]) -> Result<(), CoreError> {
    let invalid = || {
        error(
            "invalid_graph_context",
            "Rank with a valid session ID and at most 12 distinct qualifiers, each a relative path or name of 1 to 128 ASCII letters, digits, underscores, hyphens, dots or slashes",
        )
    };
    if let Some(session_id) = session_id {
        validate_id(session_id, "session ID").map_err(|_| invalid())?;
    }
    let distinct: BTreeSet<&String> = qualifiers.iter().collect();
    if qualifiers.len() > CONTEXT_NAMES
        || distinct.len() != qualifiers.len()
        || !qualifiers.iter().all(|qualifier| is_qualifier(qualifier))
    {
        return Err(invalid());
    }
    Ok(())
}

/// A relative path or a name a prompt can qualify a definition with: 1 to
/// `QUALIFIER_BYTES` ASCII letters, digits, `_`, `-`, `.` and `/`, with no
/// empty or `..` component.
pub fn is_qualifier(qualifier: &str) -> bool {
    (1..=QUALIFIER_BYTES).contains(&qualifier.len())
        && qualifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./".contains(&byte))
        && !qualifier
            .split('/')
            .any(|part| part.is_empty() || part == "..")
}

/// What ranks the definitions of names in `workspace_id` for `session_id`.
/// Without a session, nothing is the session's own and every live session's
/// work is a peer's. A peer is live as in a briefing: registered in this
/// generation in the same workspace or a linked worktree of its repository,
/// not exited, not ended, and not the asking session's parent.
pub fn rank_inputs(
    state: &State,
    workspace_id: &str,
    session_id: Option<&str>,
    qualifiers: &[String],
    now_ms: u64,
) -> RankInputs {
    let mut inputs = RankInputs {
        qualifiers: qualifiers.to_vec(),
        ..RankInputs::default()
    };
    let me = session_id.and_then(|id| state.sessions.get(id));
    if let Some(me) = me.filter(|me| me.workspace_id == workspace_id) {
        let (editing, _, _) = work_paths(state, &me.session_id, now_ms);
        let activity = state.activity.get(&me.session_id);
        for path in activity.iter().flat_map(|activity| &activity.known) {
            inputs.own.insert(path.path.clone(), false);
        }
        let touched = activity.iter().flat_map(|activity| &activity.touched);
        for path in editing.iter().chain(touched.map(|touch| &touch.path)) {
            inputs.own.insert(path.clone(), true);
        }
    }
    let repository = |id: &str| {
        state
            .workspace_git
            .get(id)
            .map(|git| git.repository_id.as_str())
    };
    let mine = repository(workspace_id);
    // The latest time each peer worked on each path; a lease is now.
    let mut latest: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for peer in state.sessions.values() {
        let related = peer.workspace_id == workspace_id
            || (mine.is_some() && repository(&peer.workspace_id) == mine);
        let parent = me.and_then(|me| me.parent.as_deref()) == Some(peer.session_id.as_str());
        let exited = state
            .session_liveness
            .get(&peer.session_id)
            .is_some_and(|liveness| liveness.status == LivenessStatus::Exited);
        let ended = state
            .activity
            .get(&peer.session_id)
            .is_some_and(|activity| activity.ended);
        if Some(peer.session_id.as_str()) == session_id
            || peer.generation != state.generation
            || !related
            || parent
            || exited
            || ended
        {
            continue;
        }
        let (editing, recent, _) = work_paths(state, &peer.session_id, now_ms);
        let worked = editing.into_iter().map(|path| (path, now_ms)).chain(
            recent
                .into_iter()
                .filter(|recent| recent.age_ms <= BRIEF_HORIZON_MS)
                .map(|recent| (recent.path, now_ms.saturating_sub(recent.age_ms))),
        );
        for (path, at_ms) in worked {
            let entry = latest
                .entry(path)
                .or_insert((at_ms, peer.session_id.clone()));
            if at_ms > entry.0 {
                *entry = (at_ms, peer.session_id.clone());
            }
        }
    }
    inputs.peers = latest
        .into_iter()
        .map(|(path, (_, session_id))| (path, session_id))
        .collect();
    inputs
}

/// Give each definition its reasons and sort the list by them, then by path
/// and line.
pub fn rank_definitions(inputs: &RankInputs, definitions: &mut [SymbolRef]) {
    for symbol in definitions.iter_mut() {
        let mut reasons = inputs.file_reasons(&symbol.path);
        if symbol.exported {
            let at = reasons
                .iter()
                .position(|reason| matches!(reason, RankReason::QualifiedInPrompt))
                .unwrap_or(reasons.len());
            reasons.insert(at, RankReason::Exported);
        }
        symbol.rank_reasons = Some(reasons);
    }
    definitions.sort_by(|a, b| {
        let reasons = |symbol: &SymbolRef| class(symbol.rank_reasons.as_deref().unwrap_or(&[]));
        reasons(a)
            .cmp(&reasons(b))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

/// Mentioning files, each `(path, mentions)`, ranked: by the rules, then
/// most mentions first, then by path.
pub fn rank_dependents(inputs: &RankInputs, files: &[(String, u32)]) -> Vec<Dependent> {
    let mut ranked: Vec<Dependent> = files
        .iter()
        .map(|(path, mentions)| Dependent {
            path: path.clone(),
            mentions: *mentions,
            rank_reasons: inputs.file_reasons(path),
        })
        .collect();
    ranked.sort_by(|a, b| {
        class(&a.rank_reasons)
            .cmp(&class(&b.rank_reasons))
            .then_with(|| b.mentions.cmp(&a.mentions))
            .then_with(|| a.path.cmp(&b.path))
    });
    ranked
}
