//! Helpers that several domains use: serde defaults, fingerprints, and the
//! reads of a session's work and activity that briefing, notices, sessions
//! and admission share.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    BRIEF_HORIZON_MS, BRIEF_PATHS, CoreError, DEFAULT_PATH_HOPS, FILE_MOVED, InflightSymbol,
    InterfaceImpact, InterfaceNotice, KnownVersion, Lease, LeaseStatus, LookedPath,
    MAX_DEFINITION_COUNT, MAX_LOOKING, MAX_PROMPT_SUMMARY_BYTES, Message, NOTICE_CHANGES,
    NOTICE_DEPENDENTS, ObservedFile, PresenceKind, PresenceSetting, RecentPath, Recipient, Session,
    SessionActivity, State, ToldNotice, error, smallest_id_first, validate_arity, validate_id,
    validate_observed_file, validate_relative_path, validate_rename_record, validate_request_id,
};

pub(crate) fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(crate) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn one() -> u32 {
    1
}

pub(crate) fn default_path_hops() -> u32 {
    DEFAULT_PATH_HOPS
}

pub(crate) fn is_one(value: &u32) -> bool {
    *value == 1
}

/// FNV-1a, 64 bits. It only tells one briefing from the next for the same
/// session; a collision would withhold one repeat briefing and nothing else.
pub(crate) fn fingerprint(text: &str) -> String {
    format!("{:016x}", fingerprint_u64(text))
}

pub(crate) fn fingerprint_u64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One in-flight signature, as remembered once told to a session: a change
/// of signature is a new key, so it is told again.
pub(crate) fn inflight_key(item: &InflightSymbol) -> String {
    fingerprint(&format!(
        "{}\n{}\n{}\n{}\n{}",
        item.workspace_id, item.path, item.kind, item.name, item.signature
    ))
}

/// Whether `session_id` is still owed `message`. A handoff is owed to any
/// other session until one takes it; `handoffs` is false mid-turn.
pub(crate) fn owed_to(message: &Message, session_id: &str, handoffs: bool) -> bool {
    message.from.as_deref() != Some(session_id)
        && !message.acknowledged.iter().any(|held| held == session_id)
        && match &message.to {
            Recipient::Session { session_id: to } => to == session_id,
            Recipient::Everyone {} => true,
            Recipient::NextSession {} => handoffs,
        }
}

pub(crate) fn validate_notice(notice: &InterfaceNotice) -> Result<(), CoreError> {
    validate_request_id(&notice.notice_id)?;
    validate_relative_path(&notice.path)?;
    if notice.changes.is_empty()
        || notice.changes.len() > NOTICE_CHANGES
        || notice.dependents.len() > NOTICE_DEPENDENTS
        || notice.elsewhere.len() > NOTICE_DEPENDENTS
    {
        return Err(error(
            "invalid_notice",
            "An interface notice names 1 to 16 changes and at most 32 dependent files in its own worktree and 32 in others",
        ));
    }
    for path in notice
        .dependents
        .iter()
        .chain(notice.elsewhere.iter().map(|file| &file.path))
    {
        validate_relative_path(path)?;
    }
    for change in &notice.changes {
        change
            .before_arity
            .iter()
            .chain(&change.after_arity)
            .try_for_each(validate_arity)?;
        let texts = [
            Some(&change.name),
            change.before.as_ref(),
            change.after.as_ref(),
            change.after_name.as_ref(),
        ];
        validate_rename_record(notice, change)?;
        let counts = match (change.before_count, change.after_count) {
            (Some(before), Some(after)) => Some((before, after)),
            (None, None) => None,
            _ => Some((0, 0)),
        };
        // Only a rename has a new name, other than the old one, and both
        // signatures. A count change names its counts, in the direction it
        // says, and carries the signatures that came or went; nothing else
        // has counts.
        // A definition moved with its file keeps its name (roadmap D7).
        let renamed = change.change == "renamed";
        let moved = change.evidence.as_deref() == Some(FILE_MOVED);
        let named = match &change.after_name {
            Some(to) => {
                renamed && !to.is_empty() && to.len() <= 128 && ((*to == change.name) == moved)
            }
            None => !renamed,
        };
        let shaped = named
            && match (change.change.as_str(), counts) {
                ("removed" | "signature_changed", None) => true,
                ("renamed", None) => change.before.is_some() && change.after.is_some(),
                // A new export: its signature, and no parameter counts.
                ("added", None) => {
                    change.before.is_none()
                        && change.after.is_some()
                        && change.before_arity.is_none()
                        && change.after_arity.is_none()
                }
                ("definition_added", Some((before, after))) => {
                    before >= 1
                        && before < after
                        && after <= MAX_DEFINITION_COUNT
                        && change.before.is_none()
                        && change.after.is_some()
                }
                ("definition_removed", Some((before, after))) => {
                    after >= 1
                        && after < before
                        && before <= MAX_DEFINITION_COUNT
                        && change.before.is_some()
                        && change.after.is_none()
                }
                _ => false,
            };
        if change.name.is_empty()
            || change.name.len() > 128
            || !shaped
            || change.kind.len() > 16
            || texts
                .into_iter()
                .flatten()
                .any(|text| text.len() > 256 || text.chars().any(char::is_control))
        {
            return Err(error(
                "invalid_notice",
                "An interface change has a name, a kind, `added`, `removed`, `signature_changed`, `renamed` with a different `after_name` of at most 128 bytes, or `definition_added` or `definition_removed` with both counts, and single-line signatures of at most 256 bytes",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_known_workspace(state: &State, workspace_id: &str) -> Result<(), CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Workspace is not registered; register it first",
        ));
    }
    Ok(())
}

/// Workspaces whose sessions belong to the same repository as `workspace_id`:
/// itself, and any the observer placed in the same repository.
pub(crate) fn related_workspace(state: &State, workspace_id: &str, other: &str) -> bool {
    if workspace_id == other {
        return true;
    }
    let repository = |id: &str| state.workspace_git.get(id).map(|git| &git.repository_id);
    repository(workspace_id).is_some() && repository(workspace_id) == repository(other)
}

pub(crate) fn known_versions<'a>(state: &'a State, session_id: &str) -> &'a [KnownVersion] {
    state
        .activity
        .get(session_id)
        .map(|activity| activity.known.as_slice())
        .unwrap_or(&[])
}

/// The other session in `workspace_id` that most recently reported a
/// successful edit of `path` after `after_ms`, with the time it did.
pub(crate) fn last_peer_edit<'a>(
    state: &'a State,
    session_id: &str,
    workspace_id: &str,
    path: &str,
    after_ms: u64,
) -> Option<(&'a str, u64)> {
    state
        .activity
        .values()
        .filter(|activity| {
            activity.session_id != session_id
                && activity.generation == state.generation
                && state
                    .sessions
                    .get(&activity.session_id)
                    .is_some_and(|peer| peer.workspace_id == workspace_id)
        })
        .filter_map(|activity| {
            activity
                .touched
                .iter()
                .find(|touch| touch.path == path && touch.at_ms >= after_ms)
                .map(|touch| (activity.session_id.as_str(), touch.at_ms))
        })
        .max_by_key(|(id, at)| (*at, smallest_id_first(id)))
}

pub(crate) fn normalized_files(files: &[ObservedFile]) -> Result<Vec<ObservedFile>, CoreError> {
    let mut unique = BTreeMap::new();
    for file in files {
        validate_observed_file(&file.path, &file.state)?;
        if unique
            .insert(file.path.clone(), file.state.clone())
            .is_some()
        {
            return Err(error(
                "invalid_path",
                "Each observed file version may be named once",
            ));
        }
        if unique.len() > 64 {
            return Err(error(
                "too_many_paths",
                "A command can name at most 64 file versions",
            ));
        }
    }
    Ok(unique
        .into_iter()
        .map(|(path, state)| ObservedFile { path, state })
        .collect())
}

pub(crate) fn live_lease(state: &State, lease: &Lease, now_ms: u64) -> bool {
    lease.status == LeaseStatus::Active
        && lease.generation == state.generation
        && lease.expires_at_ms > now_ms
}

/// Whether the session wrote a file since its last tool batch or stop.
pub(crate) fn wrote_since_boundary(state: &State, session_id: &str) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.wrote_since_boundary)
}

/// What session `id` works on: the paths its live leases hold, the files it
/// touched within the briefing horizon (with their ages), and those with the
/// paths of its declared intent.
pub(crate) fn work_paths(
    state: &State,
    id: &str,
    now_ms: u64,
) -> (Vec<String>, Vec<RecentPath>, BTreeSet<String>) {
    let editing: Vec<String> = state
        .leases
        .values()
        .filter(|lease| lease.session_id == id && live_lease(state, lease, now_ms))
        .flat_map(|lease| lease.paths.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let recent: Vec<RecentPath> = state
        .activity
        .get(id)
        .filter(|activity| activity.generation == state.generation)
        .map(|activity| {
            activity
                .touched
                .iter()
                .filter(|touch| now_ms.saturating_sub(touch.at_ms) <= BRIEF_HORIZON_MS)
                .map(|touch| RecentPath {
                    path: touch.path.clone(),
                    age_ms: now_ms.saturating_sub(touch.at_ms),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut all: BTreeSet<String> = editing.iter().cloned().collect();
    all.extend(recent.iter().map(|recent| recent.path.clone()));
    if let Some(intent) = state
        .intents
        .get(id)
        .filter(|intent| intent.generation == state.generation)
    {
        all.extend(intent.paths.iter().cloned());
    }
    (editing, recent, all)
}

/// Files session `id` works on or has read: what a changed interface could
/// break for it.
pub(crate) fn concerned_paths(state: &State, id: &str, now_ms: u64) -> BTreeSet<String> {
    let (_, _, mut concerned) = work_paths(state, id, now_ms);
    concerned.extend(
        known_versions(state, id)
            .iter()
            .map(|version| version.path.clone()),
    );
    concerned
}

/// The interface notices of `me`'s workspace, in the order they are held, that touch
/// `concerned` (the files `me` works on or has read), within the briefing
/// horizon and not of `me`'s own making: each notice's ID and what a
/// briefing shows of it. One rule for the briefing and for between-step
/// delivery (issue #214).
pub(crate) fn interface_impacts<'a>(
    state: &'a State,
    me: &'a Session,
    concerned: &'a BTreeSet<String>,
    now_ms: u64,
) -> impl Iterator<Item = (String, InterfaceImpact)> + 'a {
    state
        .interface_notices
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(move |notice| {
            notice.generation == state.generation
                && now_ms.saturating_sub(notice.at_ms) <= BRIEF_HORIZON_MS
                && notice.by_session.as_deref() != Some(me.session_id.as_str())
        })
        .filter_map(move |notice| {
            let affected: Vec<String> = notice
                .dependents
                .iter()
                .filter(|path| concerned.contains(*path))
                .take(BRIEF_PATHS)
                .cloned()
                .collect();
            (!affected.is_empty()).then(|| {
                (
                    notice.notice_id.clone(),
                    InterfaceImpact {
                        path: notice.path.clone(),
                        by_session: notice.by_session.clone(),
                        changes: notice.changes.clone(),
                        affected,
                        age_ms: now_ms.saturating_sub(notice.at_ms),
                    },
                )
            })
        })
}

/// The interface notices told to `session_id` in this generation (issue
/// #214), newest first.
pub(crate) fn told_notices<'a>(
    state: &'a State,
    session_id: &str,
) -> impl Iterator<Item = &'a ToldNotice> {
    state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .into_iter()
        .flat_map(|activity| &activity.notices_told)
}

pub(crate) fn validate_prompt_summary(summary: &str) -> Result<(), CoreError> {
    if summary.is_empty()
        || summary.len() > MAX_PROMPT_SUMMARY_BYTES
        || summary.chars().any(char::is_control)
    {
        return Err(error(
            "invalid_summary",
            &format!(
                "A prompt summary is one line of 1 to {MAX_PROMPT_SUMMARY_BYTES} UTF-8 bytes without control characters"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn activity_mut<'a>(state: &'a mut State, session_id: &str) -> &'a mut SessionActivity {
    let generation = state.generation;
    let activity = state
        .activity
        .entry(session_id.to_owned())
        .or_insert_with(|| SessionActivity {
            session_id: session_id.to_owned(),
            generation,
            ..SessionActivity::default()
        });
    if activity.generation != generation {
        // Times from an earlier generation are not comparable; start again.
        // A version the session read is still what it read: keep it, undated.
        let mut known = take_all(&mut activity.known);
        for version in &mut known {
            version.at_ms = 0;
        }
        *activity = SessionActivity {
            session_id: session_id.to_owned(),
            generation,
            known,
            ..SessionActivity::default()
        };
    }
    // Any new record from the session means it is running again.
    activity.ended = false;
    activity
}

pub(crate) fn take_all<T: Default>(value: &mut T) -> T {
    core::mem::take(value)
}

/// A session's presence setting: its own, or the default.
pub(crate) fn presence_setting(state: &State, session_id: &str) -> PresenceSetting {
    state.presence.get(session_id).copied().unwrap_or_default()
}

/// Record that a session looked at `paths`: newest first, one entry per path.
pub(crate) fn look(
    activity: &mut SessionActivity,
    kind: PresenceKind,
    paths: &[String],
    at_ms: u64,
) {
    for path in paths.iter().rev() {
        activity.looking.retain(|looked| looked.path != *path);
        activity.looking.insert(
            0,
            LookedPath {
                path: path.clone(),
                kind,
                at_ms,
            },
        );
    }
    activity.looking.truncate(MAX_LOOKING);
}
