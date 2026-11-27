//! Notes anchored to a definition, and their tags (roadmap E1).
//!
//! The daemon's source index finds definitions; this module decides what
//! they mean for a note. A note is `current` only while the index reads its
//! definition's file at its present content and finds the definition's own
//! bytes unchanged. Anything else it cannot vouch for reads `changed`,
//! `gone` or `unknown`, never `current`.

use crate::{CoreError, Note, SymbolRef, error};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Tags one note carries at most.
pub const MAX_TAGS: usize = 4;
/// Bytes of one tag at most.
pub const TAG_BYTES: usize = 32;
/// Bytes of an anchored definition's name at most.
pub const SYMBOL_BYTES: usize = 200;
/// Definitions listed when a name is ambiguous or not found.
pub const ANCHOR_CANDIDATES: usize = 8;

/// The definition a note was written about, as the index held it then.
/// Before the first complete scan only `name` is known.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NoteAnchor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// SHA-256 of the definition's own bytes when the note was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_digest: Option<String>,
}

impl NoteAnchor {
    fn resolved(&self) -> bool {
        self.path.is_some() && self.body_digest.is_some()
    }

    fn unresolved(name: &str) -> Self {
        Self {
            name: name.into(),
            path: None,
            kind: None,
            line: None,
            body_digest: None,
        }
    }

    fn of(symbol: &SymbolRef) -> Self {
        Self {
            name: symbol.name.clone(),
            path: Some(symbol.path.clone()),
            kind: Some(symbol.kind.clone()),
            line: Some(symbol.line),
            body_digest: symbol.body_digest.clone(),
        }
    }
}

/// Whether the code a note describes is still the code it was written
/// about.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AnchorState {
    /// The definition's own bytes are as they were.
    Current,
    /// The definition is there with different bytes.
    Changed,
    /// No definition of that name and kind remains in that file.
    Gone,
    /// Not known: no complete scan yet, or the index cannot read the file
    /// at its present content.
    Unknown,
}

impl AnchorState {
    /// The state as the wire names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Changed => "changed",
            Self::Gone => "gone",
            Self::Unknown => "unknown",
        }
    }

    /// Briefing order: what still holds first.
    pub(crate) fn rank(state: Option<Self>) -> u8 {
        match state {
            None | Some(Self::Current) => 0,
            Some(Self::Unknown) => 1,
            Some(Self::Changed | Self::Gone) => 2,
        }
    }
}

/// What the index holds for anchored names, from one snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnchorIndex {
    /// Each definition of each name once: every one in each file its
    /// `AnchorLookup` names, and for a workspace lookup the first ones in
    /// path and line order up to the daemon's cap.
    pub definitions: Vec<SymbolRef>,
    /// Of the files asked about, those indexed at their present content
    /// without a syntax error or a bound reached.
    pub fresh: BTreeSet<String>,
    /// Of the files asked about, those the index holds no row for: deleted,
    /// moved or never read.
    pub absent: BTreeSet<String>,
    /// Names whose workspace lookup reached the cap, so more definitions
    /// may exist than `definitions` lists.
    pub truncated: BTreeSet<String>,
    /// The index reads every supported file of the workspace in full
    /// (`GraphCoverage::blind_in_scope` is zero).
    pub complete: bool,
}

/// Where the daemon looks one anchored name up: in each of `paths` in
/// full, and across the workspace (capped) when `workspace` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnchorLookup {
    pub paths: BTreeSet<String>,
    pub workspace: bool,
}

/// Unique paths one command names at most (`normalized_paths`); one more
/// is looked up so the command is still refused for naming too many.
const LOOKUP_PATHS: usize = 64 + 1;

fn add_lookup(lookups: &mut BTreeMap<String, AnchorLookup>, name: &str, scope: &[&str]) {
    let lookup = lookups.entry(name.to_owned()).or_default();
    if scope.is_empty() {
        lookup.workspace = true;
    }
    for path in scope {
        if lookup.paths.len() >= LOOKUP_PATHS {
            break;
        }
        lookup.paths.insert((*path).to_owned());
    }
}

/// What the daemon looks up for `remember TEXT [PATHS] --symbol NAME`:
/// each named file in full, and the workspace as well, so a name not
/// found there can be listed where it is defined.
pub fn remember_lookups(name: &str, paths: &[&str]) -> BTreeMap<String, AnchorLookup> {
    let mut lookups = Default::default();
    add_lookup(&mut lookups, name, paths);
    add_lookup(&mut lookups, name, &[]);
    lookups
}

/// Tags as recorded: each once, in order. Refused unless each is 1 to
/// `TAG_BYTES` bytes of lower-case letters, digits and `-`, at most
/// `MAX_TAGS`.
pub fn validate_tags(tags: &[String]) -> Result<Vec<String>, CoreError> {
    let refused = || {
        error(
            "invalid_tag",
            "A note has at most 4 tags, each 1 to 32 bytes of lower-case letters, digits and '-'",
        )
    };
    let mut held: Vec<String> = Vec::new();
    for tag in tags {
        validate_tag(tag).map_err(|_| refused())?;
        if !held.contains(tag) {
            held.push(tag.clone());
        }
    }
    if held.len() > MAX_TAGS {
        return Err(refused());
    }
    held.sort();
    Ok(held)
}

/// One tag, as `validate_tags` takes it.
pub fn validate_tag(tag: &str) -> Result<(), CoreError> {
    if tag.is_empty()
        || tag.len() > TAG_BYTES
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(error(
            "invalid_tag",
            "A tag is 1 to 32 bytes of lower-case letters, digits and '-'",
        ));
    }
    Ok(())
}

pub(crate) fn validate_symbol(name: &str) -> Result<(), CoreError> {
    if name.is_empty()
        || name.len() > SYMBOL_BYTES
        || name.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(error(
            "invalid_symbol",
            "A symbol is one name of 1 to 200 bytes, without spaces",
        ));
    }
    Ok(())
}

/// Whether a held note's anchor and tags are as recording leaves them.
pub(crate) fn valid_note_anchor(note: &Note) -> bool {
    let anchor_ok = match (&note.anchor, note.anchor_state) {
        (None, None) => true,
        (Some(anchor), Some(state)) => {
            validate_symbol(&anchor.name).is_ok()
                && anchor.path.as_ref().is_none_or(|path| path.len() <= 4096)
                && (anchor.resolved()
                    || state == AnchorState::Unknown
                    || state == AnchorState::Gone)
        }
        _ => false,
    };
    anchor_ok && validate_tags(&note.tags).is_ok_and(|tags| tags == note.tags)
}

fn listed(definitions: &[&SymbolRef]) -> String {
    let mut text = definitions
        .iter()
        .take(ANCHOR_CANDIDATES)
        .map(|symbol| format!("{:?} line {}", symbol.path, symbol.line))
        .collect::<Vec<_>>()
        .join(", ");
    if definitions.len() > ANCHOR_CANDIDATES {
        text.push_str(&format!(
            " and {} more",
            definitions.len() - ANCHOR_CANDIDATES
        ));
    }
    text
}

/// The anchor a new note records for `name`, in the files `scope` (the
/// whole workspace when empty). Without an index, or while the one
/// definition's file is not read at its present content, the anchor holds
/// the name only and is `unknown` until the next complete scan.
pub(crate) fn resolve(
    name: &str,
    scope: &[String],
    index: Option<&AnchorIndex>,
) -> Result<(NoteAnchor, AnchorState), CoreError> {
    validate_symbol(name)?;
    let Some(index) = index else {
        return Ok((NoteAnchor::unresolved(name), AnchorState::Unknown));
    };
    let named: Vec<&SymbolRef> = index
        .definitions
        .iter()
        .filter(|symbol| symbol.name == name)
        .collect();
    let found: Vec<&SymbolRef> = named
        .iter()
        .copied()
        .filter(|symbol| scope.is_empty() || scope.contains(&symbol.path))
        .collect();
    match found.as_slice() {
        [one] if index.fresh.contains(&one.path) && one.body_digest.is_some() => {
            Ok((NoteAnchor::of(one), AnchorState::Current))
        }
        [_] => Ok((NoteAnchor::unresolved(name), AnchorState::Unknown)),
        [] => Err(error(
            "symbol_not_found",
            &if named.is_empty() {
                format!(
                    "No indexed definition of {name:?} {}; check the name, or wait for the index to read the file",
                    if scope.is_empty() {
                        "in the workspace".to_owned()
                    } else {
                        format!("in {scope:?}")
                    }
                )
            } else {
                format!(
                    "No indexed definition of {name:?} in {scope:?}; it is defined in {}",
                    listed(&named)
                )
            },
        )),
        several => Err(error(
            "symbol_ambiguous",
            &format!(
                "{name:?} is defined {}{} times: {}; name the file it is in",
                if scope.is_empty() && index.truncated.contains(name) {
                    "at least "
                } else {
                    ""
                },
                several.len(),
                listed(several)
            ),
        )),
    }
}

/// Whether a briefing of a session working on `concerned` carries `note`:
/// a plain note about the workspace or one of those files; an anchored
/// note about one of them, its definition's file, or a file of them that
/// mentions its name (`mentioned`, found by the daemon's index).
pub(crate) fn note_reaches(
    note: &Note,
    concerned: &BTreeSet<String>,
    mentioned: &BTreeSet<String>,
) -> bool {
    match &note.anchor {
        None => note.paths.is_empty() || note.paths.iter().any(|path| concerned.contains(path)),
        Some(anchor) => {
            note.paths
                .iter()
                .chain(&anchor.path)
                .any(|path| concerned.contains(path))
                || mentioned.contains(&anchor.name)
        }
    }
}

/// The anchors of a workspace's held notes, for the daemon to look up: a
/// resolved anchor in its own file, an unresolved one in the files its
/// note names, or across the workspace when it names none. Each looked-up
/// file's freshness and presence decide a state.
pub fn anchored(state: &crate::State, workspace_id: &str) -> BTreeMap<String, AnchorLookup> {
    let mut lookups = Default::default();
    for note in state.notes.get(workspace_id).into_iter().flatten() {
        if let Some(anchor) = &note.anchor {
            let scope: Vec<&str> = match (&anchor.path, anchor.resolved()) {
                (Some(path), true) => vec![path.as_str()],
                _ => note.paths.iter().map(String::as_str).collect(),
            };
            add_lookup(&mut lookups, &anchor.name, &scope);
        }
    }
    lookups
}

/// What a complete scan makes of a held anchor: the anchor (filled in
/// when it held the name only) and its state.
pub(crate) fn observe(
    anchor: &NoteAnchor,
    scope: &[String],
    index: &AnchorIndex,
) -> (NoteAnchor, AnchorState) {
    let (Some(path), Some(body)) = (&anchor.path, &anchor.body_digest) else {
        return match resolve(&anchor.name, scope, Some(index)) {
            Ok(resolved) => resolved,
            Err(refused) if refused.code == "symbol_not_found" && index.complete => {
                (anchor.clone(), AnchorState::Gone)
            }
            Err(_) => (anchor.clone(), AnchorState::Unknown),
        };
    };
    if !index.fresh.contains(path) {
        // A complete scan holds a row for every supported file it can see:
        // without one, the file was deleted or moved (review comment
        // 4713922957). A row it cannot read now says nothing.
        let gone = index.complete && index.absent.contains(path);
        return (
            anchor.clone(),
            if gone {
                AnchorState::Gone
            } else {
                AnchorState::Unknown
            },
        );
    }
    let mut same = index.definitions.iter().filter(|symbol| {
        symbol.name == anchor.name
            && &symbol.path == path
            && anchor.kind.as_ref().is_none_or(|kind| &symbol.kind == kind)
    });
    let state = match same
        .clone()
        .any(|symbol| symbol.body_digest.as_ref() == Some(body))
    {
        true => AnchorState::Current,
        false if same.next().is_some() => AnchorState::Changed,
        false => AnchorState::Gone,
    };
    (anchor.clone(), state)
}
