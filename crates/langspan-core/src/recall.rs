//! Searching remembered notes (roadmap E2): the held notes of one workspace
//! whose fields hold every word of a short query, compared without case.
//! A read over core state: nothing is recorded, nothing counts as delivered,
//! and no briefing changes because of it. Forgotten notes are no longer held,
//! so they are never found.

use serde::{Deserialize, Serialize};

use crate::{CoreError, Note, State, error, validate_known_workspace, validate_relative_path};

/// The longest query, in bytes.
pub const RECALL_QUERY_BYTES: usize = 200;
/// The most words one query is split into.
pub const RECALL_WORDS: usize = 8;
/// The most notes one recall returns.
pub const RECALL_LIMIT: u32 = 50;

/// One note found by a recall. Its text is remembered repository text: data,
/// never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecalledNote {
    pub note_id: String,
    /// The session that recorded it, when one did.
    pub author: Option<String>,
    pub text: String,
    /// Files it is about. Empty means the whole workspace.
    pub paths: Vec<String>,
    /// The definition it is about and whether that is unchanged (roadmap
    /// E1), as the held note has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<crate::NoteAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_state: Option<crate::AnchorState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// The notes a query found, best first. `truncated` is true when more notes
/// matched than are returned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Recall {
    pub workspace_id: String,
    pub query: String,
    pub notes: Vec<RecalledNote>,
    pub truncated: bool,
}

impl Recall {
    /// Drop the last notes until `fits` says the answer is small enough,
    /// marking it truncated when any were dropped.
    pub fn shrink_until(&mut self, mut fits: impl FnMut(&Recall) -> bool) {
        while !fits(self) && self.notes.pop().is_some() {
            self.truncated = true;
        }
    }
}

/// The words of a query, lower-cased, or why it is refused.
fn words(query: &str) -> Result<Vec<String>, CoreError> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if query.len() > RECALL_QUERY_BYTES || words.is_empty() || words.len() > RECALL_WORDS {
        return Err(error(
            "invalid_query",
            "A recall query is 1 to 200 bytes of at most 8 words, separated by spaces",
        ));
    }
    Ok(words)
}

/// How many of the note's fields hold a query word, when each word is in at
/// least one of them; `None` when a word is in none.
fn fields_hit(note: &Note, words: &[String]) -> Option<usize> {
    let text = note.text.to_lowercase();
    let paths: Vec<String> = note.paths.iter().map(|path| path.to_lowercase()).collect();
    let in_text = |word: &String| text.contains(word.as_str());
    let in_paths = |word: &String| paths.iter().any(|path| path.contains(word.as_str()));
    if !words.iter().all(|word| in_text(word) || in_paths(word)) {
        return None;
    }
    Some(usize::from(words.iter().any(in_text)) + usize::from(words.iter().any(in_paths)))
}

/// The held notes of `workspace_id` that hold every word of `query`, about
/// `path` or the whole workspace when a path is given: those hit in more
/// fields first, then newest first, at most `limit`.
pub fn recall(
    state: &State,
    workspace_id: &str,
    query: &str,
    path: Option<&str>,
    limit: u32,
) -> Result<Recall, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    let words = words(query)?;
    if !(1..=RECALL_LIMIT).contains(&limit) {
        return Err(error("invalid_limit", "A recall returns 1 to 50 notes"));
    }
    if let Some(path) = path {
        validate_relative_path(path)?;
    }
    // Held notes are newest first; a stable sort keeps that order within
    // each count of fields hit.
    let mut hits: Vec<(usize, &Note)> = state
        .notes
        .get(workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|note| {
            path.is_none_or(|path| note.paths.is_empty() || note.paths.iter().any(|p| p == path))
        })
        .filter_map(|note| fields_hit(note, &words).map(|hit| (hit, note)))
        .collect();
    hits.sort_by(|(ours, _), (theirs, _)| theirs.cmp(ours));
    let limit = limit as usize;
    Ok(Recall {
        workspace_id: workspace_id.to_owned(),
        query: query.to_owned(),
        truncated: hits.len() > limit,
        notes: hits
            .into_iter()
            .take(limit)
            .map(|(_, note)| RecalledNote {
                note_id: note.note_id.clone(),
                author: note.author.clone(),
                text: note.text.clone(),
                paths: note.paths.clone(),
                anchor: note.anchor.clone(),
                anchor_state: note.anchor_state,
                tags: note.tags.clone(),
            })
            .collect(),
    })
}
