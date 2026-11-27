//! Briefing: what a session is told and what it has already been told.
//! Its record types and rules, `brief` (moved whole), and the
//! `decide_checked` and `apply` arms of its commands and events, which
//! call these functions. Ranking, recent changes and trails are in
//! `rank.rs`, `recent.rs` and `trail.rs`.

// The arm functions take what the arm's pattern bound, as bound (`&String`,
// `&Vec<_>`), so that each body moved out of the match unchanged.
#![allow(clippy::ptr_arg)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    AnchorState, BRIEF_INTERFACE_NOTICES, BRIEF_MESSAGES, CheckNotice, CheckoutChange,
    CheckoutNotice, Command, CoreError, Decision, Delivery, Event, INFLIGHT_BYTES,
    INFLIGHT_CANDIDATES, INFLIGHT_LINES, INFLIGHT_MENTIONS, InflightNotice, InflightSymbol,
    InterfaceImpact, LeftWork, LivenessStatus, MAX_TOLD, MAX_TRAILS, MESSAGE_BRIEF_BYTES, Message,
    Note, NoticeVia, Outcome, PRESENCE_PATHS, PresencePath, REMOTE_HORIZON_MS, RecentChanges,
    Recipient, RemotePeerWork, Reservation, RetractedMessage, Session, SessionActivity, StaleRead,
    State, TRAIL_BYTES, Trail, TrailView, UNCOORDINATED_LINE_LIMIT, UncoordinatedNotice,
    activity_mut, anchors, concerned_paths, current_session, error, fingerprint, fingerprint_u64,
    inflight_cost, inflight_key, inflight_notice, inflight_told, interface_impacts, is_false,
    is_zero, known_session, known_versions, last_peer_edit, left_work, message_cost, owed_to,
    presence_setting, recent, related_workspace, reservations, retracted_cost, takes_handoff,
    told_notices, trail, uncoordinated_owed, validate_known_workspace, work_paths,
};

/// The signature of a briefing with nothing in it.
const EMPTY_SIGNATURE: &str = "cbf29ce484222325";
/// A briefing outcome stays well inside one 64 KiB transport frame.
const BRIEF_BYTES: usize = 40 * 1024;

pub const MAX_TOUCHED: usize = 32;
pub const MAX_KNOWN: usize = 128;
pub const MAX_BRIEF_PEERS: u32 = 16;
pub const BRIEF_PATHS: usize = 8;
/// Activity older than this, within one generation, is left out of briefings.
pub const BRIEF_HORIZON_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentPath {
    pub path: String,
    pub age_ms: u64,
}

/// One other session's relevant work, as shown to the asking session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerWork {
    pub session_id: String,
    pub workspace_id: String,
    pub workspace_root: String,
    /// False for a linked worktree of the same repository: overlap there is
    /// logical, and no admission in one blocks the other.
    pub same_workspace: bool,
    pub branch_ref: Option<String>,
    pub liveness: Option<LivenessStatus>,
    /// The host reported this session over; `recent` is what it left behind.
    pub ended: bool,
    pub summary: Option<String>,
    pub intent: Option<String>,
    pub editing: Vec<String>,
    pub recent: Vec<RecentPath>,
    /// Paths this peer shares with the asking session's own work. The
    /// asking session's prompt targets count as its work here.
    pub overlap: Vec<String>,
    /// Paths this peer's last prompt points at (its prompt targets) that the
    /// asking session works on or its own prompt points at, and that are not
    /// already in `overlap`. Advisory: it never holds a write (roadmap C8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_overlap: Vec<String>,
    /// How this peer is related to the asking session through a host
    /// subagent; absent for an unrelated session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<PeerRelation>,
    /// Where this peer was recently looking (presence, roadmap A13): at most
    /// `PRESENCE_PATHS`, newest first, none it is editing or edited, within
    /// the asking session's presence window. Advisory: it never holds,
    /// refuses or delays anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub looking: Vec<PresencePath>,
}

/// A peer that belongs to the same host conversation as the asking session.
/// A subagent's own parent is never a peer in its briefing (ADR 0004).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeerRelation {
    /// The session that started the asking subagent. No new briefing names
    /// it; it stays so briefing receipts stored before that change still read.
    Parent,
    /// A subagent the asking session started.
    Subagent,
    /// Another subagent of the asking subagent's parent.
    Sibling,
}

/// How `peer` is related to `me` through a host subagent, if at all.
fn relation(me: &Session, peer: &Session) -> Option<PeerRelation> {
    if peer.parent.as_deref() == Some(me.session_id.as_str()) {
        Some(PeerRelation::Subagent)
    } else if me.parent.is_some() && me.parent == peer.parent {
        Some(PeerRelation::Sibling)
    } else {
        None
    }
}

/// What a briefing of `session_id` compares in-flight work with: the
/// session's workspace, and the files it works on or has read. `None` for
/// a session that is not current.
pub fn briefing_concern(
    state: &State,
    session_id: &str,
    now_ms: u64,
) -> Option<(String, BTreeSet<String>)> {
    let me = current_session(state, session_id).ok()?;
    Some((
        me.workspace_id.clone(),
        concerned_paths(state, session_id, now_ms),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Briefing {
    pub session_id: String,
    /// False when the same peers and paths were already delivered.
    pub changed: bool,
    pub peers: Vec<PeerWork>,
    pub omitted_peers: u32,
    /// Items left out because this session was already told them and they
    /// are unchanged: only in a briefing asked for with `changes_only`
    /// (issue #222). Zero means the briefing is the full view.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unchanged: u32,
    /// Files this session read that a peer in the same checkout has since
    /// reported editing. Detection after the fact, from reported edits only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_reads: Vec<StaleRead>,
    /// Teammates' sessions on other machines, from their publications.
    /// Always advisory: nothing here holds or releases a file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote: Vec<RemotePeerWork>,
    /// Exported symbols a peer changed that this session's files mention by
    /// name. A name match, not a resolved dependency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_changes: Vec<InterfaceImpact>,
    /// The notice ID of each of `interface_changes`, in the same order. A
    /// hook reports the ones its rendered text showed (`InterfaceShown`,
    /// issue #214).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_notice_ids: Vec<String>,
    /// Exported symbols another worktree of the repository has added or
    /// changed and not committed, that this session's files mention by name
    /// (roadmap C7). Each signature is told once, at most `INFLIGHT_LINES`
    /// within `INFLIGHT_BYTES`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inflight: Vec<InflightNotice>,
    /// In-flight symbols owed to this session but left for a later
    /// briefing.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_inflight: u32,
    /// The source index has not finished a first scan of this session's
    /// workspace or of another worktree of its repository, so `inflight`
    /// may leave out work in flight there (issue #262). Not part of the
    /// signature: it is said beside news, and is not news by itself.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inflight_partial: bool,
    /// Remembered notes about the workspace or about files this session
    /// works on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// Messages addressed to this session or to everyone, and a pending
    /// handoff, that it has not yet acknowledged. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// Messages this session received that were since withdrawn: each is
    /// delivered once, so the session disregards it. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retracted: Vec<RetractedMessage>,
    /// The checkout this session works in changed under it since it last
    /// read or edited a file there, other than by its own shell command.
    /// Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkout_changes: Vec<CheckoutNotice>,
    /// Changes on disk in this checkout that no session reported, with the
    /// Codex sessions that may have run without LangSpan's hooks. Each is
    /// delivered once. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncoordinated: Vec<UncoordinatedNotice>,
    /// Results of the automatic check run after this session's writes, not
    /// yet delivered. Each is delivered once. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckNotice>,
    /// Ended sessions that left uncommitted work in this checkout, from the
    /// sessions the request named (`Brief::left`). At most
    /// `BRIEF_LEFT_WORK`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_work: Vec<LeftWork>,
    /// The trail an ended session of this checkout left, at a start or a
    /// prompt, taken once (roadmap C9). Shown first, after that session's
    /// own handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<TrailView>,
    /// What changed recently in the repository, only in a briefing asked
    /// for at session start (roadmap C3). Never part of the signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent: Option<RecentChanges>,
    /// Numbers other running sessions of this repository reserved (ADR
    /// 0040), newest first, at most `BRIEF_RESERVATIONS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserved: Vec<Reservation>,
}

// Ties go to the lexically smallest session, so the result is deterministic.
pub(crate) fn smallest_id_first(id: &str) -> core::cmp::Reverse<&str> {
    core::cmp::Reverse(id)
}

/// Select the other sessions whose current work is relevant to `session_id`:
/// same physical workspace or a worktree of the same repository, registered in
/// this generation, not known to have exited, and active within the horizon.
/// Returns the briefing and a signature that ignores ages, so an unchanged
/// situation is not delivered twice.
/// The signature leaves out messages: a delivered message is acknowledged,
/// so its absence afterwards is not a change. Also returns the keys of the
/// in-flight signatures it carries, to be remembered once delivered.
fn brief(
    state: &State,
    session_id: &str,
    max_peers: u32,
    handoffs: bool,
    now_ms: u64,
    (inflight, mentioned): (&[InflightSymbol], &BTreeSet<String>),
) -> Result<(Briefing, String, Vec<String>), CoreError> {
    let me = current_session(state, session_id)?;
    let repository = state
        .workspace_git
        .get(&me.workspace_id)
        .map(|git| git.repository_id.as_str());
    let work_paths = |id: &str| work_paths(state, id, now_ms);
    // Where a running session's last prompt pointed, while it is recent.
    let prompt_targets = |id: &str| -> BTreeSet<String> {
        state
            .activity
            .get(id)
            .filter(|activity| activity.generation == state.generation && !activity.ended)
            .and_then(|activity| activity.prompt_targets.as_ref())
            .filter(|targets| now_ms.saturating_sub(targets.at_ms) <= BRIEF_HORIZON_MS)
            .map(|targets| targets.paths.iter().cloned().collect())
            .unwrap_or_default()
    };
    // Where a running peer that shows its presence was recently looking,
    // within this session's window: newest first, none it edits or edited.
    let window_ms = presence_setting(state, session_id).window_ms;
    let looking = |id: &str, edited: &BTreeSet<String>| -> Vec<PresencePath> {
        if !presence_setting(state, id).shown {
            return Vec::new();
        }
        state
            .activity
            .get(id)
            .filter(|activity| activity.generation == state.generation && !activity.ended)
            .map(|activity| {
                activity
                    .looking
                    .iter()
                    .filter(|looked| {
                        now_ms.saturating_sub(looked.at_ms) <= window_ms
                            && !edited.contains(&looked.path)
                    })
                    .take(PRESENCE_PATHS)
                    .map(|looked| PresencePath {
                        path: looked.path.clone(),
                        kind: looked.kind,
                        age_s: now_ms.saturating_sub(looked.at_ms) / 1000,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let (_, _, mine) = work_paths(session_id);
    // What a peer's work or task is compared with: this session's work and
    // the files its own task points at.
    let mut mine_or_targeted = mine.clone();
    mine_or_targeted.extend(prompt_targets(session_id));
    let mut peers = Vec::new();
    for peer in state.sessions.values() {
        if peer.session_id == session_id || peer.generation != state.generation {
            continue;
        }
        // A subagent's parent started it and knows its own task and files;
        // told back to the subagent, they read as another session's. The
        // parent's held files are still refused to it at admission, and its
        // edits of files the subagent read are still stale reads.
        if me.parent.as_deref() == Some(peer.session_id.as_str()) {
            continue;
        }
        let same_workspace = peer.workspace_id == me.workspace_id;
        let peer_git = state.workspace_git.get(&peer.workspace_id);
        if !same_workspace
            && !(repository.is_some()
                && peer_git.map(|git| git.repository_id.as_str()) == repository)
        {
            continue;
        }
        let liveness = state
            .session_liveness
            .get(&peer.session_id)
            .map(|liveness| liveness.status);
        if liveness == Some(LivenessStatus::Exited) {
            continue;
        }
        let (mut editing, mut recent, all) = work_paths(&peer.session_id);
        let activity = state
            .activity
            .get(&peer.session_id)
            .filter(|activity| activity.generation == state.generation);
        // A session that only received briefings has shown no work of its own.
        let fresh = activity.is_some_and(|activity| {
            (activity.summary.is_some() || !activity.touched.is_empty())
                && now_ms.saturating_sub(activity.last_seen_ms()) <= BRIEF_HORIZON_MS
        });
        let intent = state
            .intents
            .get(&peer.session_id)
            .filter(|intent| intent.generation == state.generation);
        let targets = prompt_targets(&peer.session_id);
        // Every file the peer holds or edited within the horizon, not only
        // those listed: such a file is shown as edited, never as looked at.
        let mut edited: BTreeSet<String> = editing.iter().cloned().collect();
        edited.extend(activity.into_iter().flat_map(|activity| {
            activity
                .touched
                .iter()
                .filter(|touch| now_ms.saturating_sub(touch.at_ms) <= BRIEF_HORIZON_MS)
                .map(|touch| touch.path.clone())
        }));
        let looking = looking(&peer.session_id, &edited);
        if editing.is_empty()
            && intent.is_none()
            && !fresh
            && targets.is_empty()
            && looking.is_empty()
        {
            continue;
        }
        // A finished session matters only for the edits it left behind.
        if activity.is_some_and(|activity| activity.ended)
            && editing.is_empty()
            && recent.is_empty()
        {
            continue;
        }
        let Some(workspace) = state.workspaces.get(&peer.workspace_id) else {
            continue;
        };
        let overlap: Vec<String> = all
            .intersection(&mine_or_targeted)
            .take(BRIEF_PATHS)
            .cloned()
            .collect();
        editing.truncate(BRIEF_PATHS);
        recent.truncate(BRIEF_PATHS);
        let last_ms = activity.map(SessionActivity::last_seen_ms).unwrap_or(0);
        let relation = relation(me, peer);
        // A subagent's task is the asking parent's own request: its files
        // are news to the parent, its task line and declared intent are not.
        let task_known = relation == Some(PeerRelation::Subagent);
        if task_known && editing.is_empty() && recent.is_empty() {
            continue;
        }
        let target_overlap: Vec<String> = if task_known {
            Vec::new()
        } else {
            targets
                .intersection(&mine_or_targeted)
                .filter(|path| !overlap.contains(*path))
                .take(BRIEF_PATHS)
                .cloned()
                .collect()
        };
        // A peer without admissions, edits, intent or targets (one known by
        // its task line or by where it looked) ranks after every peer with
        // them, so presence never takes such a peer's place.
        let no_work =
            editing.is_empty() && recent.is_empty() && intent.is_none() && targets.is_empty();
        peers.push((
            (
                overlap.is_empty() && target_overlap.is_empty(),
                editing.is_empty(),
                no_work,
                u64::MAX - last_ms,
            ),
            PeerWork {
                session_id: peer.session_id.clone(),
                workspace_id: peer.workspace_id.clone(),
                workspace_root: workspace.root.clone(),
                same_workspace,
                branch_ref: peer_git.and_then(|git| git.branch_ref.clone()),
                liveness,
                ended: activity.is_some_and(|activity| activity.ended),
                // A declared intent is the session's own account of its
                // task: it replaces the line taken from the prompt.
                summary: activity
                    .filter(|activity| fresh && !activity.ended && !task_known)
                    .filter(|_| intent.is_none_or(|intent| intent.summary.is_empty()))
                    .and_then(|activity| activity.summary.clone()),
                intent: intent
                    .filter(|_| !task_known)
                    .map(|intent| intent.summary.clone()),
                editing,
                recent,
                overlap,
                target_overlap,
                relation,
                looking,
            },
        ));
    }
    // A peer known only by where it looked ranks after every other, so it
    // never takes a slot from a peer with work or a task.
    let rank = |(key, peer): &((bool, bool, bool, u64), PeerWork)| {
        (key.0, key.1, key.2, presence_only(peer), key.3)
    };
    peers.sort_by(|a, b| (rank(a), &a.1.session_id).cmp(&(rank(b), &b.1.session_id)));
    let total = peers.len();
    // Peers are chosen by their work within the byte budget first; where
    // they look is added after, in the space left, first peers first, so
    // presence never takes the place of another peer's work. A trail this
    // briefing may tell takes its encoding out of that budget, so the
    // reply holds both within its frame.
    let peer_bytes = BRIEF_BYTES
        - trail::owed(state, me, handoffs).map_or(0, |trail| {
            trail::json_cost(trail).min(trail::TRAIL_JSON_BYTES)
        });
    let mut bytes = 0;
    let mut peers: Vec<PeerWork> = peers
        .into_iter()
        .map(|(_, peer)| peer)
        .take(max_peers as usize)
        .take_while(|peer| {
            // Counted as encoded, escapes included, so quoted or control
            // characters cannot push the reply past its frame.
            let cost = 256
                + recent::json_len(&peer.workspace_root)
                + [&peer.summary, &peer.intent, &peer.branch_ref]
                    .into_iter()
                    .flatten()
                    .map(|text| recent::json_len(text))
                    .sum::<usize>()
                + peer
                    .editing
                    .iter()
                    .chain(&peer.overlap)
                    .chain(&peer.target_overlap)
                    .chain(peer.recent.iter().map(|recent| &recent.path))
                    .map(|path| recent::json_len(path) + 32)
                    .sum::<usize>();
            bytes += cost;
            if bytes > peer_bytes {
                bytes -= cost;
                return false;
            }
            true
        })
        .collect();
    // A peer there only for where it looked, without room for that, is left
    // out and counted.
    peers.retain_mut(|peer| {
        let presence: usize = peer
            .looking
            .iter()
            .map(|looked| recent::json_len(&looked.path) + 32)
            .sum();
        if bytes + presence <= peer_bytes {
            bytes += presence;
            return true;
        }
        let only = presence_only(peer);
        peer.looking.clear();
        !only
    });
    let mut stale_reads: Vec<StaleRead> = known_versions(state, session_id)
        .iter()
        .filter_map(|version| {
            let (peer, at_ms) = last_peer_edit(
                state,
                session_id,
                &me.workspace_id,
                &version.path,
                version.at_ms,
            )?;
            Some(StaleRead {
                path: version.path.clone(),
                session_id: peer.to_owned(),
                age_ms: now_ms.saturating_sub(at_ms),
            })
        })
        .collect();
    stale_reads.sort_by(|a, b| (a.age_ms, &a.path).cmp(&(b.age_ms, &b.path)));
    stale_reads.truncate(BRIEF_PATHS);
    let mut remote: Vec<RemotePeerWork> = state
        .remote_peers
        .values()
        .filter(|peer| {
            peer.generation == state.generation
                && now_ms.saturating_sub(peer.received_ms) <= REMOTE_HORIZON_MS
                && related_workspace(state, &me.workspace_id, &peer.workspace_id)
        })
        .flat_map(|peer| {
            let mine = &mine;
            peer.publication
                .work
                .iter()
                .map(move |work| RemotePeerWork {
                    name: peer.publication.name.clone(),
                    overlap: work
                        .editing
                        .iter()
                        .chain(&work.recent)
                        .filter(|path| mine.contains(*path))
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    work: work.clone(),
                    received_age_ms: now_ms.saturating_sub(peer.received_ms),
                })
        })
        .collect();
    remote.sort_by(|a, b| {
        (
            a.overlap.is_empty(),
            a.work.editing.is_empty(),
            &a.name,
            &a.work.session,
        )
            .cmp(&(
                b.overlap.is_empty(),
                b.work.editing.is_empty(),
                &b.name,
                &b.work.session,
            ))
    });
    remote.truncate(BRIEF_PATHS);
    let concerned = concerned_paths(state, session_id, now_ms);
    // A notice already told between steps is not told again (issue #214).
    let (notice_ids, interface_changes): (Vec<String>, Vec<InterfaceImpact>) =
        interface_impacts(state, me, &concerned, now_ms)
            .filter(|(id, _)| {
                !told_notices(state, session_id)
                    .any(|told| told.notice_id == *id && told.via == NoticeVia::BetweenSteps)
            })
            .take(BRIEF_INTERFACE_NOTICES)
            .unzip();
    // Notes about the whole workspace, or about a file this session works
    // on; an anchored note, about its definition's file or a file that
    // mentions its name (roadmap E1). What still holds is chosen first,
    // each kind newest first.
    let mut notes: Vec<&Note> = state
        .notes
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|note| {
            note.author.as_deref() != Some(session_id)
                && anchors::note_reaches(note, &concerned, mentioned)
        })
        .collect();
    notes.sort_by_key(|note| AnchorState::rank(note.anchor_state));
    let notes: Vec<Note> = notes.into_iter().take(4).cloned().collect();
    // Withdrawals first, then messages owed to this session, each newest
    // first, as many as the text of one briefing is sure to hold beside an
    // owed uncoordinated change, whose line is reserved; the rest wait for
    // the next one. The first always fits.
    let uncoordinated: Vec<UncoordinatedNotice> = uncoordinated_owed(state, session_id, now_ms)
        .map(|(_, notice)| notice)
        .collect();
    let message_bytes = if uncoordinated.is_empty() {
        MESSAGE_BRIEF_BYTES
    } else {
        MESSAGE_BRIEF_BYTES - (UNCOORDINATED_LINE_LIMIT + 4)
    };
    let mut bytes = 0;
    let retracted: Vec<RetractedMessage> = state
        .retracted_notices
        .get(session_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(BRIEF_MESSAGES)
        .take_while(|retracted| {
            bytes += retracted_cost(retracted);
            bytes <= message_bytes
        })
        .cloned()
        .collect();
    let messages: Vec<Message> = state
        .messages
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|message| {
            owed_to(
                message,
                session_id,
                handoffs && takes_handoff(state, me, message),
            )
        })
        .take(BRIEF_MESSAGES)
        .take_while(|message| {
            bytes += message_cost(message);
            bytes <= message_bytes
        })
        .map(|message| Message {
            acknowledged: Vec::new(),
            ..message.clone()
        })
        .collect();
    // An ended session's trail (roadmap C9) shares the messages' room and
    // follows that session's own handoff: while the handoff is owed here
    // and waits for room, so does the trail.
    let used = retracted.iter().map(retracted_cost).sum::<usize>()
        + messages.iter().map(message_cost).sum::<usize>();
    let trail = trail::owed(state, me, handoffs)
        .filter(|_| used + TRAIL_BYTES < message_bytes)
        .filter(|trail| {
            state
                .messages
                .get(&me.workspace_id)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|message| {
                    message.from.as_deref() == Some(trail.session_id.as_str())
                        && matches!(message.to, Recipient::NextSession {})
                        && owed_to(message, session_id, takes_handoff(state, me, message))
                })
                .all(|handoff| {
                    messages
                        .iter()
                        .any(|shown| shown.message_id == handoff.message_id)
                })
        })
        .map(|trail| trail::view(state, trail, now_ms));
    // A checkout change matters to a session that read or edited files
    // here before it: what it knows may no longer be on disk.
    // A change a session's own shell command explains is not news to it;
    // others are told whose command it was.
    let mut checkout_ids = Vec::new();
    let changes = state
        .checkouts
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let runs = state
        .checkout_runs
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let explained_by =
        |change: &CheckoutChange| runs.iter().find(|run| run.explains(change, changes));
    let checkout_changes: Vec<CheckoutNotice> = changes
        .iter()
        .filter(|change| explained_by(change).is_none_or(|run| run.session_id != session_id))
        .filter(|change| {
            change.generation == state.generation
                && now_ms.saturating_sub(change.at_ms) <= BRIEF_HORIZON_MS
                && state
                    .activity
                    .get(session_id)
                    .filter(|activity| activity.generation == state.generation)
                    .is_some_and(|activity| {
                        activity
                            .known
                            .iter()
                            .any(|known| known.at_ms <= change.at_ms)
                            || activity
                                .touched
                                .iter()
                                .any(|touch| touch.at_ms <= change.at_ms)
                    })
        })
        .take(2)
        .map(|change| {
            checkout_ids.push(change.change_id.clone());
            let run = explained_by(change);
            CheckoutNotice {
                by_session: run.map(|run| run.session_id.clone()),
                command: run.and_then(|run| run.command.clone()),
                from_branch: change.from_branch.clone(),
                to_branch: change.to_branch.clone(),
                from_head: change.from_head.clone(),
                to_head: change.to_head.clone(),
                files: change.files,
                age_ms: now_ms.saturating_sub(change.at_ms),
            }
        })
        .collect();
    // In-flight signatures from other worktrees of the repository (roadmap
    // C7), each told once. Each line is delivered once, so it is chosen
    // only for a briefing with nothing else used up on delivery, whose
    // text is sure to hold it; otherwise every one is counted and waits.
    let quiet = messages.is_empty()
        && trail.is_none()
        && retracted.is_empty()
        && uncoordinated.is_empty()
        && !state.check_notices.contains_key(session_id);
    let owed: Vec<(String, InflightNotice)> = inflight
        .iter()
        .filter(|item| {
            item.workspace_id != me.workspace_id
                && related_workspace(state, &me.workspace_id, &item.workspace_id)
                && !inflight_told(state, session_id, item)
        })
        .filter_map(|item| {
            let key = inflight_key(item);
            let mut notice = inflight_notice(state, item)?;
            notice.mentioned_in.retain(|path| concerned.contains(path));
            notice.mentioned_in.truncate(INFLIGHT_MENTIONS);
            (!notice.mentioned_in.is_empty()).then_some((key, notice))
        })
        .collect();
    let mut inflight_keys = Vec::new();
    let mut inflight_notices = Vec::new();
    let mut bytes = 0;
    for (key, notice) in &owed {
        bytes += inflight_cost(notice);
        if !quiet || inflight_notices.len() == INFLIGHT_LINES || bytes > INFLIGHT_BYTES {
            break;
        }
        inflight_keys.push(key.clone());
        inflight_notices.push(notice.clone());
    }
    let mut signature = String::new();
    for key in &inflight_keys {
        signature.push_str(&format!("inflight|{key}\n"));
    }
    for id in &checkout_ids {
        signature.push_str(&format!("checkout|{id}\n"));
    }
    // A note whose definition changed state is news again.
    for note in &notes {
        match note.anchor_state {
            Some(held) => {
                signature.push_str(&format!("note|{}|{}\n", note.note_id, held.as_str()));
            }
            None => signature.push_str(&format!("note|{}\n", note.note_id)),
        }
    }
    for impact in &interface_changes {
        signature.push_str(&format!("interface|{}|", impact.path));
        for change in &impact.changes {
            signature.push_str(&format!(
                "{}:{}:{}|",
                change.name,
                change.change,
                change.after.as_deref().unwrap_or("")
            ));
        }
        signature.push_str(&impact.affected.join(","));
        signature.push('\n');
    }
    for item in &remote {
        signature.push_str(&format!(
            "remote|{}|{}|{}|{}|{}|{}\n",
            item.name,
            item.work.session,
            item.work.branch_ref.as_deref().unwrap_or(""),
            item.work.summary.as_deref().unwrap_or(""),
            item.work.editing.join(","),
            item.work.recent.join(",")
        ));
    }
    for stale in &stale_reads {
        signature.push_str(&format!("stale|{}|{}\n", stale.path, stale.session_id));
    }
    let reserved = reservations::peer_claims(state, session_id, &me.workspace_id);
    for claim in &reserved {
        signature.push_str(&format!(
            "reserved|{}|{}|{}\n",
            claim.key, claim.number, claim.session_id
        ));
    }
    // A peer listed only for where it looked is presence, not work.
    for peer in peers.iter().filter(|peer| !presence_only(peer)) {
        signature.push_str(&peer.session_id);
        for part in [&peer.editing, &peer.overlap] {
            signature.push('|');
            signature.push_str(&part.join(","));
        }
        signature.push('|');
        for recent in &peer.recent {
            signature.push_str(&recent.path);
            signature.push(',');
        }
        // A peer in this checkout is on this session's own branch, learned
        // by the Git observer on its own thread, perhaps only after a
        // briefing: not news. A change of that branch is a checkout change.
        let branch = (!peer.same_workspace).then_some(&peer.branch_ref);
        for text in [Some(&peer.summary), Some(&peer.intent), branch] {
            signature.push('|');
            signature.push_str(text.and_then(Option::as_deref).unwrap_or(""));
        }
        // Only when present, so a briefing without one keeps the signature
        // it had before prompt targets existed and is not delivered again.
        if !peer.target_overlap.is_empty() {
            signature.push_str("|targets:");
            signature.push_str(&peer.target_overlap.join(","));
        }
        signature.push_str(if peer.ended { "|ended\n" } else { "\n" });
    }
    // Presence has a signature part of its own: alone it makes a briefing
    // new only at a session start or prompt, never mid-turn, so a peer's
    // reads do not interrupt a turn. The part is a set of bits, one pair per
    // peer and path (`presence_bits`), so that a briefing is news only for
    // a path not shown before: a path that ages out, or is read again, or
    // the same files read in another order, is not.
    let presence = presence_bits(&peers);
    // Without presence the signature is as it was before presence existed,
    // so a briefing stored then is not delivered again.
    let mut signature = fingerprint(&signature);
    if let Some(bits) = presence {
        signature.push('+');
        for byte in bits {
            signature.push_str(&format!("{byte:02x}"));
        }
    }
    Ok((
        Briefing {
            session_id: session_id.into(),
            changed: false,
            omitted_peers: u32::try_from(total - peers.len()).unwrap_or(u32::MAX),
            // Counted by `keep_changes` for a briefing of changes only.
            unchanged: 0,
            peers,
            stale_reads,
            remote,
            interface_changes,
            interface_notice_ids: notice_ids,
            omitted_inflight: u32::try_from(owed.len() - inflight_notices.len())
                .unwrap_or(u32::MAX),
            inflight: inflight_notices,
            // Set by the request that carries the index's answer.
            inflight_partial: false,
            notes,
            messages,
            retracted,
            checkout_changes,
            uncoordinated,
            // Filled in by the request that names the sessions (`Brief::left`).
            left_work: Vec::new(),
            trail,
            // One result per briefing, the newest: it always fits the
            // rendered text, and each delivered one is removed, so the rest
            // follow at the next briefings.
            checks: state
                .check_notices
                .get(session_id)
                .and_then(|notices| notices.first())
                .cloned()
                .into_iter()
                .collect(),
            recent: None,
            reserved,
        },
        signature,
        inflight_keys,
    ))
}

/// The key of one briefing item: what it says, without ages, so that the
/// same item told again has the same key (issue #222).
fn item_key(parts: &[&str]) -> u64 {
    let mut text = String::new();
    for part in parts {
        text.push_str(part);
        text.push('\u{1f}');
    }
    fingerprint_u64(&text)
}

/// A peer's own line without its files: who it is, where, its task and
/// whether it is still running. A peer in this checkout is on this
/// session's own branch, which the Git observer may learn only after a
/// briefing (as in `brief`'s signature): not part of its key.
fn peer_key(peer: &PeerWork) -> u64 {
    let branch = (!peer.same_workspace)
        .then_some(peer.branch_ref.as_deref())
        .flatten();
    let relation = peer.relation.map(|relation| format!("{relation:?}"));
    item_key(&[
        "peer",
        &peer.session_id,
        &peer.workspace_root,
        branch.unwrap_or(""),
        peer.summary.as_deref().unwrap_or(""),
        peer.intent.as_deref().unwrap_or(""),
        if peer.ended { "ended" } else { "" },
        if peer.liveness == Some(LivenessStatus::Unknown) {
            "unknown"
        } else {
            ""
        },
        relation.as_deref().unwrap_or(""),
    ])
}

fn peer_path_key(peer: &PeerWork, kind: &str, path: &str) -> u64 {
    item_key(&["peer", &peer.session_id, kind, path])
}

fn stale_key(stale: &StaleRead) -> u64 {
    item_key(&["stale", &stale.path, &stale.session_id])
}

fn remote_work_key(remote: &RemotePeerWork) -> u64 {
    let work = &remote.work;
    item_key(&[
        "remote",
        &remote.name,
        &work.session,
        work.branch_ref.as_deref().unwrap_or(""),
        work.summary.as_deref().unwrap_or(""),
        &work.editing.join(","),
        &work.recent.join(","),
        &remote.overlap.join(","),
    ])
}

fn interface_key(impact: &InterfaceImpact) -> u64 {
    let changes: Vec<String> = impact
        .changes
        .iter()
        .map(|change| {
            // Everything that says what changed: a removal's signature and
            // a count change's counts too (issue #270).
            format!(
                "{:?}",
                (
                    &change.name,
                    &change.kind,
                    &change.change,
                    &change.before,
                    &change.after,
                    &change.after_name,
                    change.before_count,
                    change.after_count,
                )
            )
        })
        .collect();
    item_key(&[
        "interface",
        &impact.path,
        &changes.join("|"),
        &impact.affected.join(","),
    ])
}

/// A note and its anchor's state: an anchored note whose state changes is
/// news again (roadmap E1).
fn note_key(note: &Note) -> u64 {
    item_key(&[
        "note",
        &note.note_id,
        note.anchor_state.map_or("", AnchorState::as_str),
    ])
}

fn reserved_key(claim: &Reservation) -> u64 {
    item_key(&[
        "reserved",
        &claim.key,
        &claim.number.to_string(),
        &claim.session_id,
    ])
}

fn checkout_key(change: &CheckoutNotice) -> u64 {
    item_key(&[
        "checkout",
        change.from_branch.as_deref().unwrap_or(""),
        change.to_branch.as_deref().unwrap_or(""),
        change.from_head.as_deref().unwrap_or(""),
        change.to_head.as_deref().unwrap_or(""),
        change.by_session.as_deref().unwrap_or(""),
        &change.files.to_string(),
    ])
}

/// The kinds of a peer's listed paths, each with its own key.
const PEER_LISTS: [&str; 5] = ["overlap", "target", "editing", "recent", "looking"];

fn peer_paths<'a>(peer: &'a PeerWork, kind: &str) -> Vec<&'a str> {
    match kind {
        "overlap" => peer.overlap.iter().map(String::as_str).collect(),
        "target" => peer.target_overlap.iter().map(String::as_str).collect(),
        "editing" => peer.editing.iter().map(String::as_str).collect(),
        "recent" => peer
            .recent
            .iter()
            .map(|recent| recent.path.as_str())
            .collect(),
        _ => peer
            .looking
            .iter()
            .map(|looked| looked.path.as_str())
            .collect(),
    }
}

/// Every item key of `briefing`, in the order a reader meets them. Messages,
/// withdrawals, check results, in-flight signatures, uncoordinated changes
/// and left work are not keyed: each is delivered once already. A hook
/// names the keys of the items it could not place to `Command::ForgetTold`.
pub fn briefing_keys(briefing: &Briefing) -> Vec<u64> {
    let mut keys = Vec::new();
    keys.extend(briefing.stale_reads.iter().map(stale_key));
    keys.extend(briefing.checkout_changes.iter().map(checkout_key));
    keys.extend(briefing.notes.iter().map(note_key));
    keys.extend(briefing.interface_changes.iter().map(interface_key));
    for peer in &briefing.peers {
        keys.push(peer_key(peer));
        for kind in PEER_LISTS {
            keys.extend(
                peer_paths(peer, kind)
                    .into_iter()
                    .map(|path| peer_path_key(peer, kind, path)),
            );
        }
    }
    keys.extend(briefing.remote.iter().map(remote_work_key));
    keys.extend(briefing.reserved.iter().map(reserved_key));
    keys
}

/// Leave in `briefing` only what `told` does not hold, count the rest in
/// `unchanged`, and return the keys left in. A safety item stays in when
/// `acted` says the session read or edited one of its files since it was
/// last told: a peer's hold on a file this session works on, an interface
/// change to names its files use, a file it read that a peer has since
/// edited. Mid-turn, where a peer is looking is not news by itself
/// (`once`: something delivered once is in the briefing); it waits, untold,
/// for a briefing that has other news.
fn keep_changes(
    briefing: &mut Briefing,
    told: &BTreeSet<u64>,
    acted: &dyn Fn(&str) -> bool,
    stale_again: &dyn Fn(&StaleRead) -> bool,
    mid_turn: bool,
    once: bool,
) -> BTreeSet<u64> {
    let mut shown = BTreeSet::new();
    let mut unchanged = 0_usize;
    let mut keep = |key: u64, safety: bool| {
        let fresh = !told.contains(&key) || safety;
        if fresh {
            shown.insert(key);
        } else {
            unchanged += 1;
        }
        fresh
    };
    briefing
        .stale_reads
        .retain(|stale| keep(stale_key(stale), acted(&stale.path) || stale_again(stale)));
    briefing
        .checkout_changes
        .retain(|change| keep(checkout_key(change), false));
    briefing.notes.retain(|note| keep(note_key(note), false));
    // Each notice's ID goes with its change, so the hook's report of what it
    // showed names the right one (issue #269).
    let mut ids = briefing.interface_notice_ids.clone().into_iter();
    let mut kept_ids = Vec::new();
    briefing.interface_changes.retain(|impact| {
        let id = ids.next();
        let fresh = keep(
            interface_key(impact),
            impact.affected.iter().any(|path| acted(path)),
        );
        if fresh {
            kept_ids.extend(id);
        }
        fresh
    });
    briefing.interface_notice_ids = kept_ids;
    // Presence counted apart: alone, mid-turn, it is held back.
    let mut presence = Vec::new();
    let mut work_news = false;
    briefing.peers.retain_mut(|peer| {
        let only_presence = presence_only(peer);
        // A file this peer holds in this checkout that this session works
        // on: refused to this session while held.
        let held: BTreeSet<String> = peer
            .overlap
            .iter()
            .filter(|path| peer.same_workspace && peer.editing.contains(path))
            .cloned()
            .collect();
        let header = peer_key(peer);
        let header_fresh = keep(header, false);
        if header_fresh {
            if only_presence {
                presence.push(header);
            } else {
                work_news = true;
            }
        }
        let mut any = false;
        let id = peer.session_id.clone();
        let mut list = |kind: &str, path: &str| {
            let key = item_key(&["peer", &id, kind, path]);
            let safety =
                matches!(kind, "overlap" | "editing") && held.contains(path) && acted(path);
            let fresh = keep(key, safety);
            if fresh {
                any = true;
                if kind == "looking" {
                    presence.push(key);
                } else {
                    work_news = true;
                }
            }
            fresh
        };
        peer.overlap.retain(|path| list("overlap", path));
        peer.target_overlap.retain(|path| list("target", path));
        peer.editing.retain(|path| list("editing", path));
        peer.recent.retain(|recent| list("recent", &recent.path));
        peer.looking.retain(|looked| list("looking", &looked.path));
        if !header_fresh {
            // Its task was told: the line names the peer and what is new.
            peer.summary = None;
            peer.intent = None;
        }
        header_fresh || any
    });
    briefing
        .remote
        .retain(|remote| keep(remote_work_key(remote), false));
    briefing
        .reserved
        .retain(|claim| keep(reserved_key(claim), false));
    let other_news = work_news
        || !briefing.reserved.is_empty()
        || once
        || !briefing.stale_reads.is_empty()
        || !briefing.checkout_changes.is_empty()
        || !briefing.notes.is_empty()
        || !briefing.interface_changes.is_empty()
        || !briefing.remote.is_empty()
        || !briefing.inflight.is_empty();
    if mid_turn && !other_news && !presence.is_empty() {
        for key in &presence {
            shown.remove(key);
        }
        // A peer is left in only for its own line, told here for the first
        // time (and not only for where it looked), or for a file still in.
        // A told line had its task cleared, so its key no longer matches.
        briefing.peers.retain_mut(|peer| {
            peer.looking.clear();
            shown.contains(&peer_key(peer))
                || !(peer.overlap.is_empty()
                    && peer.target_overlap.is_empty()
                    && peer.editing.is_empty()
                    && peer.recent.is_empty())
        });
    }
    briefing.unchanged = u32::try_from(unchanged).unwrap_or(u32::MAX);
    shown
}

/// Whether a `changes_only` briefing still has an item to place in context.
fn shown_anything(briefing: &Briefing) -> bool {
    !briefing.peers.is_empty()
        || !briefing.stale_reads.is_empty()
        || !briefing.remote.is_empty()
        || !briefing.interface_changes.is_empty()
        || !briefing.inflight.is_empty()
        || !briefing.notes.is_empty()
        || !briefing.checkout_changes.is_empty()
        || !briefing.reserved.is_empty()
}

/// Bytes in the presence part of a briefing signature: with the work
/// fingerprint and its `+`, the signature stays within the 64 bytes an
/// event holds.
const PRESENCE_SIGNATURE_BYTES: usize = 23;

/// The presence part of a briefing signature: for each peer and path it
/// shows as presence, two bits set from that pair's fingerprint. None when
/// it shows none. A path whose bits another pair already set is not news on
/// its own; it is shown with the next briefing that is.
fn presence_bits(peers: &[PeerWork]) -> Option<[u8; PRESENCE_SIGNATURE_BYTES]> {
    const BITS: u64 = (PRESENCE_SIGNATURE_BYTES * 8) as u64;
    let mut bits = [0u8; PRESENCE_SIGNATURE_BYTES];
    let mut any = false;
    for peer in peers {
        for looked in &peer.looking {
            let hash = fingerprint_u64(&format!("{}|{}", peer.session_id, looked.path));
            for bit in [hash % BITS, (hash >> 32) % BITS] {
                bits[(bit / 8) as usize] |= 1 << (bit % 8);
            }
            any = true;
        }
    }
    any.then_some(bits)
}

/// Whether a briefing signature shows presence that `previous` did not:
/// a bit set in its presence part that is clear in the previous one. A
/// previous signature without a presence part, or from before this form,
/// has shown none.
fn new_presence(signature: &str, previous: &str) -> bool {
    let bits = |signature: &str| -> Option<Vec<u8>> {
        let (_, hex) = signature.split_once('+')?;
        if hex.len() != PRESENCE_SIGNATURE_BYTES * 2 {
            return None;
        }
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
            .collect()
    };
    let Some(now) = bits(signature) else {
        return false;
    };
    match bits(previous) {
        Some(before) => now
            .iter()
            .zip(&before)
            .any(|(now, before)| now & !before != 0),
        None => true,
    }
}

/// A briefing signature without its presence part.
fn without_presence(signature: &str) -> &str {
    signature
        .split_once('+')
        .map_or(signature, |(work, _)| work)
}

/// A peer in a briefing only for where it looked.
fn presence_only(peer: &PeerWork) -> bool {
    !peer.looking.is_empty()
        && peer.editing.is_empty()
        && peer.recent.is_empty()
        && peer.overlap.is_empty()
        && peer.target_overlap.is_empty()
        && peer.summary.is_none()
        && peer.intent.is_none()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_brief(
    state: &State,
    command: &Command,
    session_id: &String,
    max_peers: &u32,
    deliver: &bool,
    mid_turn: &bool,
    left: &Vec<String>,
    recent: &bool,
    recent_window_ms: &Option<u64>,
    mcp: &bool,
    changes_only: &bool,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    if !(1..=MAX_BRIEF_PEERS).contains(max_peers) {
        return Err(error(
            "invalid_limit",
            "A briefing names between 1 and 16 peers",
        ));
    }
    if *recent && *mid_turn {
        return Err(error(
            "invalid_request",
            "What changed recently is told at a session's start, never mid-turn; leave out recent",
        ));
    }
    let window = recent::window(*recent_window_ms)?;
    let none = BTreeSet::new();
    let (inflight, inflight_partial, mentioned) = match command {
        Command::BriefPrepared {
            inflight,
            inflight_partial,
            mentioned,
            ..
        } => {
            if inflight.len() > INFLIGHT_CANDIDATES {
                return Err(error(
                    "invalid_limit",
                    "A briefing is offered at most 32 in-flight symbols",
                ));
            }
            (inflight.as_slice(), *inflight_partial, mentioned)
        }
        _ => (&[][..], false, &none),
    };
    let (mut briefing, signature, inflight_keys) = brief(
        state,
        session_id,
        *max_peers,
        !*mid_turn,
        now_ms,
        (inflight, mentioned),
    )?;
    briefing.left_work = left_work(state, session_id, left)?;
    // Not part of the signature (see `Briefing::inflight_partial`).
    briefing.inflight_partial = inflight_partial;
    // Not part of the signature: a prompt's briefing never carries
    // it, so it must not make the next one look new.
    if *recent {
        let me = current_session(state, session_id)?;
        briefing.recent = Some(recent::recent(state, me, window, now_ms));
    }
    let previous = state
        .activity
        .get(session_id)
        .and_then(|activity| activity.briefed.as_deref())
        .unwrap_or(EMPTY_SIGNATURE);
    // Delivered messages are acknowledged below and so are absent
    // from the next briefing: the remembered signature leaves them
    // out, and any message at all makes this briefing new.
    // Presence is news only at a session start or prompt, and only
    // for a path not shown before. Mid-turn, or when some or all of
    // it merely aged out, it is not (see `brief`).
    let differs = without_presence(&signature) != without_presence(previous)
        || (!*mid_turn && new_presence(&signature, previous));
    let once = !briefing.messages.is_empty()
        || !briefing.uncoordinated.is_empty()
        || !briefing.retracted.is_empty()
        || !briefing.checks.is_empty()
        || !briefing.left_work.is_empty()
        || briefing.trail.is_some();
    // What the session was told, and when: from this generation
    // only, since times from an earlier one do not compare.
    let activity = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation);
    let told: BTreeSet<u64> = activity
        .map(|activity| activity.told.iter().copied().collect())
        .unwrap_or_default();
    let keys = briefing_keys(&briefing);
    let (told_next, shown) = if *changes_only {
        let since = activity.map_or(0, |activity| activity.told_at_ms);
        // The session read or edited `path` after it was last told:
        // a safety item about the file is news again. A read in the
        // same millisecond as that briefing counts as told with it:
        // the clock cannot order the two, and usually the read is
        // the hook run that carried the briefing.
        let acted = |path: &str| {
            activity.is_some_and(|activity| {
                activity
                    .touched
                    .iter()
                    .any(|touch| touch.path == path && touch.at_ms > since)
                    || activity
                        .known
                        .iter()
                        .any(|known| known.path == path && known.at_ms > since)
            })
        };
        // That read may still have come after the briefing, from a
        // concurrent hook: a peer's edit after that millisecond makes
        // the file stale again (issue #259).
        let stale_again = |stale: &StaleRead| {
            now_ms.saturating_sub(stale.age_ms) > since
                && activity.is_some_and(|activity| {
                    activity
                        .known
                        .iter()
                        .any(|known| known.path == stale.path && known.at_ms >= since)
                })
        };
        let shown_keys = keep_changes(&mut briefing, &told, &acted, &stale_again, *mid_turn, once);
        briefing.changed = once || shown_anything(&briefing);
        let next: Vec<u64> = keys
            .iter()
            .copied()
            .filter(|key| told.contains(key) || shown_keys.contains(key))
            .take(MAX_TOLD)
            .collect();
        (next, briefing.changed)
    } else {
        // The whole view: once delivered, every item in it is told.
        briefing.changed = differs || once;
        (keys.into_iter().take(MAX_TOLD).collect(), true)
    };
    let told_before: Vec<u64> = activity
        .map(|activity| activity.told.clone())
        .unwrap_or_default();
    let mut events = Vec::new();
    if *deliver {
        let records = if *changes_only {
            shown || told_next != told_before
        } else {
            // Mid-turn, a full view not delivered as news (where a
            // peer looks, alone) is not recorded: it stays news for
            // the next prompt.
            (signature != previous && briefing.changed)
                || (told_next != told_before && (briefing.changed || !*mid_turn))
        };
        if records {
            events.push(Event::BriefingDelivered {
                session_id: session_id.clone(),
                signature,
                told: told_next,
                told_at_ms: shown.then_some(now_ms),
            });
        }
        events.extend(
            briefing
                .messages
                .iter()
                .map(|message| Event::MessageAcknowledged {
                    workspace_id: message.workspace_id.clone(),
                    message_id: message.message_id.clone(),
                    session_id: session_id.clone(),
                    via: Some(if *mid_turn {
                        Delivery::MidTurn
                    } else if *mcp {
                        Delivery::McpBrief
                    } else {
                        Delivery::Briefing
                    }),
                    at_ms: Some(now_ms),
                    generation: Some(state.generation),
                }),
        );
        events.extend(
            briefing
                .retracted
                .iter()
                .map(|retracted| Event::RetractionDelivered {
                    session_id: session_id.clone(),
                    message_id: retracted.message_id.clone(),
                }),
        );
        events.extend(briefing.trail.iter().map(|view| Event::TrailTaken {
            workspace_id: view.trail.workspace_id.clone(),
            from: view.trail.session_id.clone(),
            session_id: session_id.clone(),
        }));
        events.extend(briefing.checks.iter().map(|notice| Event::CheckDelivered {
            session_id: session_id.clone(),
            check_id: notice.check_id.clone(),
        }));
        if !inflight_keys.is_empty() {
            events.push(Event::InflightDelivered {
                session_id: session_id.clone(),
                keys: inflight_keys,
            });
        }
        // The MCP tool returns every notice, so asking there tells
        // them all. A hook renders within a byte cap that can leave
        // some out, so it reports the ones it showed itself
        // (`InterfaceShown`). A notice told before is not recorded
        // twice.
        let newly_told: Vec<String> = briefing
            .interface_notice_ids
            .iter()
            .filter(|id| !told_notices(state, session_id).any(|told| told.notice_id == **id))
            .cloned()
            .collect();
        if *mcp && !newly_told.is_empty() {
            events.push(Event::InterfaceNoticesTold {
                session_id: session_id.clone(),
                notice_ids: newly_told,
                via: NoticeVia::Briefing,
            });
        }
        events.extend(
            uncoordinated_owed(state, session_id, now_ms).map(|(change, _)| {
                Event::UncoordinatedDelivered {
                    workspace_id: change.workspace_id.clone(),
                    change_id: change.change_id.clone(),
                    session_id: session_id.clone(),
                }
            }),
        );
    }
    Ok(Decision {
        events,
        outcome: Outcome::Briefing { briefing },
    })
}

pub(crate) fn decide_trails(
    state: &State,
    workspace_id: &String,
    limit: &Option<u32>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    let limit = limit.unwrap_or(4);
    if !(1..=MAX_TRAILS as u32).contains(&limit) {
        return Err(error(
            "invalid_limit",
            "A trail listing names between 1 and 16 trails",
        ));
    }
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::Trails {
            workspace_id: workspace_id.clone(),
            trails: trail::listed(state, workspace_id, limit as usize, now_ms),
            omitted: 0,
        },
    })
}

pub(crate) fn apply_trail_recorded(state: &mut State, trail: &Trail) -> Result<(), CoreError> {
    trail::validate_trail(state, trail)?;
    let trails = state.trails.entry(trail.workspace_id.clone()).or_default();
    trails.retain(|kept| kept.session_id != trail.session_id);
    trails.insert(0, trail.clone());
    trails.truncate(MAX_TRAILS);
    Ok(())
}

pub(crate) fn apply_trail_taken(
    state: &mut State,
    workspace_id: &String,
    from: &String,
    session_id: &String,
) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    // A trail the cap already dropped has nothing left to mark.
    if let Some(trail) = state
        .trails
        .get_mut(workspace_id)
        .and_then(|trails| trails.iter_mut().find(|trail| &trail.session_id == from))
        && trail.taken_by.is_none()
    {
        trail.taken_by = Some(session_id.clone());
    }
    Ok(())
}

pub(crate) fn apply_briefing_delivered(
    state: &mut State,
    session_id: &String,
    signature: &String,
    told: &Vec<u64>,
    told_at_ms: &Option<u64>,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if signature.len() > 64 || told.len() > MAX_TOLD {
        return Err(error(
            "invalid_event",
            "A briefing signature is at most 64 bytes, and a delivery names at most 256 item keys",
        ));
    }
    let activity = activity_mut(state, session_id);
    activity.briefed = Some(signature.clone());
    activity.told = told.clone();
    if let Some(at_ms) = told_at_ms {
        activity.told_at_ms = *at_ms;
    }
    Ok(())
}
