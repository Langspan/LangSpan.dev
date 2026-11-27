//! Remote awareness: links, publications and the work peers publish: their
//! records, rules, decisions and state changes. `decide` and `apply` in
//! `lib.rs` call the `decide_*` and `apply_*` functions here.

use crate::*;

/// One session's work as its owner's daemon publishes it to teammates.
/// Paths are repository-relative. Everything here crosses a machine boundary
/// and is validated as untrusted input on receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteWork {
    pub host: String,
    /// The first characters of the host session ID: enough to tell two apart.
    pub session: String,
    pub branch_ref: Option<String>,
    pub summary: Option<String>,
    pub editing: Vec<String>,
    pub recent: Vec<String>,
}

/// What one participant publishes for one repository. `sequence` rises by
/// one with each publication and is the only ordering used across machines.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub participant: String,
    pub name: String,
    pub sequence: u64,
    pub work: Vec<RemoteWork>,
    /// The sender stopped publishing but its host refused to delete its ref,
    /// so this final publication says so in its place. It lists no work, and
    /// a receiver treats the sender as gone.
    #[serde(default, skip_serializing_if = "is_false")]
    pub withdrawn: bool,
}

/// The largest publication payload, in bytes, that is pushed or recorded.
pub const PUBLICATION_BYTES: usize = 16 * 1024;

/// One publication that left this machine, as recorded when it was pushed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SentPublication {
    /// Position in the local event log.
    pub seq: u64,
    pub sequence: u64,
    pub published_at_ms: Option<u64>,
    /// The exact bytes pushed, or `None` for a record from before payloads
    /// were kept.
    pub payload: Option<String>,
}

/// The retention windows a compaction accepts, in days, inclusive. The lower bound
/// stays well above any client's retry time, since a retry whose receipt
/// was removed is a new request.
pub const MIN_RETENTION_DAYS: u32 = 7;
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// One compaction of the event log (ADR 0025, J7). Times are wall-clock
/// Unix milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionReport {
    /// False for a dry run, which deleted nothing.
    pub applied: bool,
    pub keep_days: u32,
    /// Records written before this time are older than the window.
    pub cutoff_ms: u64,
    /// The sequence the derived state is snapshotted at: events at or below
    /// it are no longer replayed at start. `None` when none is kept.
    pub snapshot_seq: Option<u64>,
    /// Events removed (or to be removed), and their sequence range.
    pub events: u64,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub receipts: u64,
    /// Bytes of the removed records. SQLite reuses the space for new records
    /// rather than shrinking the file.
    pub bytes: u64,
    /// Why the snapshot stops before the newest event older than the
    /// window: something still open was started there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<String>,
}

/// The owner's standing choice to publish a workspace's work to a Git remote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteLink {
    pub workspace_id: String,
    pub remote: String,
    pub participant: String,
    pub name: String,
    /// Whether task summaries leave the machine. Off unless the owner asks.
    pub share_task: bool,
    /// Sequence and content fingerprint of the last recorded publication.
    /// Kept when publication is disabled, so that enabling it again never
    /// publishes a sequence teammates have already seen.
    pub sequence: u64,
    pub signature: String,
    /// False once the owner disabled publication.
    pub enabled: bool,
    /// True once a disabled workspace's ref is known to be gone from the
    /// remote. Until then the daemon keeps trying to remove it.
    pub withdrawn: bool,
    /// The owner chose to publish under `refs/heads/langspan/peers/`, for
    /// hosts that accept pushes only to branches. Absent in links recorded
    /// before the choice existed, which publish under `refs/langspan/peers/`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub publish_as_branch: bool,
}

/// A teammate's latest publication as this daemon received it. Its age is
/// measured from local receipt, never from the sender's clock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemotePeer {
    pub workspace_id: String,
    pub publication: Publication,
    pub received_ms: u64,
    pub generation: u64,
}

/// A teammate's session on another machine, as shown to the asking session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemotePeerWork {
    pub name: String,
    pub work: RemoteWork,
    pub overlap: Vec<String>,
    /// Time since this daemon last received a new publication from them.
    pub received_age_ms: u64,
}

pub const REMOTE_WORK: usize = 8;
/// A teammate not heard from for this long is treated as disconnected.
pub const REMOTE_HORIZON_MS: u64 = 10 * 60 * 1000;

fn remote_key(workspace_id: &str, participant: &str) -> String {
    format!("{workspace_id}:{participant}")
}

fn remote_link<'a>(state: &'a State, workspace_id: &str) -> Result<&'a RemoteLink, CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    state
        .remote_links
        .get(workspace_id)
        .filter(|link| link.enabled)
        .ok_or_else(|| {
            error(
                "remote_not_enabled",
                "This workspace does not publish; enable it with `langspan remote enable`",
            )
        })
}

fn validate_remote_link(state: &State, link: &RemoteLink) -> Result<(), CoreError> {
    validate_id(&link.workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(&link.workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Register the workspace before enabling publication",
        ));
    }
    validate_id(&link.remote, "Git remote name")?;
    if link.remote.starts_with(['-', '.']) {
        return Err(error(
            "invalid_remote",
            "A Git remote name starts with a letter, digit or underscore",
        ));
    }
    validate_participant(&link.participant, &link.name)?;
    if link.remote.len() > 64 || link.signature.len() > 64 {
        return Err(error(
            "invalid_remote",
            "A Git remote name is at most 64 characters",
        ));
    }
    Ok(())
}

fn validate_participant(participant: &str, name: &str) -> Result<(), CoreError> {
    if !is_hex_digest(participant, 32) || participant.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(error(
            "invalid_participant",
            "A participant ID is 32 lowercase hexadecimal characters",
        ));
    }
    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        return Err(error(
            "invalid_participant",
            "A participant name is 1 to 64 bytes without control characters",
        ));
    }
    Ok(())
}

/// Everything in a publication came from another machine. Reject the whole
/// publication rather than keep part of one.
fn validate_publication(publication: &Publication) -> Result<(), CoreError> {
    validate_participant(&publication.participant, &publication.name)?;
    if publication.withdrawn && !publication.work.is_empty() {
        return Err(error(
            "invalid_publication",
            "A withdrawn publication lists no sessions",
        ));
    }
    if publication.work.len() > REMOTE_WORK {
        return Err(error(
            "invalid_publication",
            "A publication lists at most 8 sessions",
        ));
    }
    for work in &publication.work {
        validate_id(&work.host, "host name")?;
        validate_id(&work.session, "session label")?;
        if work.host.len() > 32
            || work.session.len() > 16
            || work.editing.len() > BRIEF_PATHS
            || work.recent.len() > BRIEF_PATHS
        {
            return Err(error(
                "invalid_publication",
                "A published session exceeds its size bounds",
            ));
        }
        for path in work.editing.iter().chain(&work.recent) {
            validate_relative_path(path)?;
        }
        if let Some(summary) = &work.summary {
            validate_prompt_summary(summary)?;
        }
        if let Some(reference) = &work.branch_ref
            && (reference.len() > 512
                || reference.chars().any(char::is_control)
                || !reference
                    .strip_prefix("refs/heads/")
                    .is_some_and(|name| !name.is_empty()))
        {
            return Err(error(
                "invalid_branch_ref",
                "A published branch is a refs/heads/ name without control characters",
            ));
        }
    }
    Ok(())
}

/// What a workspace's owner publishes now: the current work of every local
/// session in the repository, bounded, with task lines only if the owner
/// chose to share them. Returns it with a fingerprint of its content.
fn publication(state: &State, link: &RemoteLink, now_ms: u64) -> (Publication, String) {
    let mut work = Vec::new();
    for session in state.sessions.values() {
        if session.generation != state.generation
            || !related_workspace(state, &link.workspace_id, &session.workspace_id)
            || state
                .session_liveness
                .get(&session.session_id)
                .is_some_and(|liveness| liveness.status == LivenessStatus::Exited)
        {
            continue;
        }
        let editing: Vec<String> = state
            .leases
            .values()
            .filter(|lease| {
                lease.session_id == session.session_id && live_lease(state, lease, now_ms)
            })
            .flat_map(|lease| lease.paths.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(BRIEF_PATHS)
            .collect();
        let activity = state
            .activity
            .get(&session.session_id)
            .filter(|activity| activity.generation == state.generation);
        let recent: Vec<String> = activity
            .map(|activity| {
                activity
                    .touched
                    .iter()
                    .filter(|touch| now_ms.saturating_sub(touch.at_ms) <= BRIEF_HORIZON_MS)
                    .map(|touch| touch.path.clone())
                    .take(BRIEF_PATHS)
                    .collect()
            })
            .unwrap_or_default();
        let ended = activity.is_some_and(|activity| activity.ended);
        let summary = activity
            .filter(|activity| {
                link.share_task
                    && !activity.ended
                    && now_ms.saturating_sub(activity.last_seen_ms()) <= BRIEF_HORIZON_MS
            })
            .and_then(|activity| activity.summary.clone());
        if editing.is_empty() && recent.is_empty() && (summary.is_none() || ended) {
            continue;
        }
        let (host, id) = session
            .session_id
            .split_once('.')
            .unwrap_or(("agent", &session.session_id));
        work.push(RemoteWork {
            host: host.chars().take(32).collect(),
            session: id.chars().take(8).collect(),
            branch_ref: state
                .workspace_git
                .get(&session.workspace_id)
                .and_then(|git| git.branch_ref.clone()),
            summary,
            editing,
            recent,
        });
        if work.len() == REMOTE_WORK {
            break;
        }
    }
    let mut text = String::new();
    for item in &work {
        text.push_str(&format!(
            "{}|{}|{}|{}|{}|{}\n",
            item.host,
            item.session,
            item.branch_ref.as_deref().unwrap_or(""),
            item.summary.as_deref().unwrap_or(""),
            item.editing.join(","),
            item.recent.join(",")
        ));
    }
    (
        Publication {
            participant: link.participant.clone(),
            name: link.name.clone(),
            sequence: link.sequence + 1,
            work,
            withdrawn: false,
        },
        fingerprint(&text),
    )
}

// The body clones the matched `String`s, as the arm did.
#[allow(clippy::ptr_arg)]
pub(crate) fn decide_remote_enable(
    state: &State,
    workspace_id: &String,
    remote: &String,
    participant: &String,
    name: &String,
    share_task: &bool,
    publish_as_branch: &bool,
) -> Result<Decision, CoreError> {
    let previous = state.remote_links.get(workspace_id);
    // Re-enabling as the same participant continues its sequence, so
    // teammates never see it go backwards.
    let (sequence, signature) = previous
        .filter(|link| &link.participant == participant)
        .map(|link| (link.sequence, link.signature.clone()))
        .unwrap_or_default();
    let link = RemoteLink {
        workspace_id: workspace_id.clone(),
        remote: remote.clone(),
        participant: participant.clone(),
        name: name.clone(),
        share_task: *share_task,
        sequence,
        // An empty fingerprint makes the first pass after enabling
        // publish, whatever was last recorded, and so does a move to
        // the other namespace: the new ref must be written at once.
        signature: if previous
            .is_some_and(|link| link.enabled && link.publish_as_branch == *publish_as_branch)
        {
            signature
        } else {
            String::new()
        },
        enabled: true,
        withdrawn: false,
        publish_as_branch: *publish_as_branch,
    };
    validate_remote_link(state, &link)?;
    // One ref per participant per repository: two worktrees of one
    // repository publishing separately would overwrite each other.
    if let Some(other) = state.remote_links.values().find(|other| {
        other.enabled
            && other.workspace_id != link.workspace_id
            && related_workspace(state, &link.workspace_id, &other.workspace_id)
    }) {
        return Err(error(
            "already_published",
            &format!(
                "This repository already publishes through workspace {:?}, which covers its worktrees",
                other.workspace_id
            ),
        ));
    }
    let events = if previous == Some(&link) {
        Vec::new()
    } else {
        vec![Event::RemoteEnabled { link: link.clone() }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::RemoteLink { link },
    })
}

pub(crate) fn decide_remote_disable(
    state: &State,
    workspace_id: &String,
) -> Result<Decision, CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    let events = if state
        .remote_links
        .get(workspace_id)
        .is_some_and(|link| link.enabled)
    {
        vec![Event::RemoteDisabled {
            workspace_id: workspace_id.clone(),
        }]
    } else {
        Vec::new()
    };
    Ok(Decision {
        events,
        outcome: Outcome::RemoteDisabled {
            workspace_id: workspace_id.clone(),
        },
    })
}

pub(crate) fn decide_remote_outbox(
    state: &State,
    workspace_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let link = remote_link(state, workspace_id)?;
    let (publication, signature) = publication(state, link, now_ms);
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::RemoteOutbox {
            changed: signature != link.signature,
            publication,
            signature,
        },
    })
}

// The body clones the matched `String`s, as the arm did.
#[allow(clippy::ptr_arg)]
pub(crate) fn decide_remote_published(
    state: &State,
    workspace_id: &String,
    sequence: &u64,
    signature: &String,
    payload: &String,
    published_at_ms: &u64,
    as_branch: &Option<bool>,
) -> Result<Decision, CoreError> {
    // A disabled workspace records too: its final "withdrawn"
    // publication, sent where the host refused to delete its ref.
    validate_id(workspace_id, "workspace ID")?;
    let link = state
        .remote_links
        .get(workspace_id)
        .ok_or_else(|| error("remote_not_enabled", "This workspace has never published"))?;
    if *sequence != link.sequence + 1 || signature.len() > 64 {
        return Err(error(
            "invalid_sequence",
            "A publication record must carry the next sequence for its workspace",
        ));
    }
    if payload.len() > PUBLICATION_BYTES {
        return Err(error(
            "invalid_payload",
            "A publication record holds at most 16 KiB of payload",
        ));
    }
    // A push to a destination the owner has since left still uses up
    // its sequence, but does not stand for the current one: the
    // signature stays unset so the next pass publishes there.
    let signature = match as_branch {
        Some(as_branch) if *as_branch != link.publish_as_branch => String::new(),
        _ => signature.clone(),
    };
    let mut link = link.clone();
    link.sequence = *sequence;
    link.signature = signature.clone();
    Ok(Decision {
        events: vec![Event::RemotePublished {
            workspace_id: workspace_id.clone(),
            sequence: *sequence,
            signature,
            payload: (!payload.is_empty()).then(|| payload.clone()),
            published_at_ms: (*published_at_ms != 0).then_some(*published_at_ms),
        }],
        outcome: Outcome::RemoteLink { link },
    })
}

// The body clones the matched `String`s, as the arm did.
#[allow(clippy::ptr_arg)]
pub(crate) fn decide_observe_remote(
    state: &State,
    workspace_id: &String,
    publication: &Publication,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let link = remote_link(state, workspace_id)?;
    validate_publication(publication)?;
    if publication.participant == link.participant {
        return Err(error(
            "own_publication",
            "A workspace does not observe its own publication",
        ));
    }
    let key = remote_key(workspace_id, &publication.participant);
    // A sender's sequence only rises. Within one generation an equal
    // or lower one is a replay or a reordering and changes nothing.
    let newer = state.remote_peers.get(&key).is_none_or(|held| {
        held.generation != state.generation || held.publication.sequence < publication.sequence
    });
    // A teammate who left, whose host kept the ref: shown as gone.
    if publication.withdrawn {
        let held = state.remote_peers.contains_key(&key);
        return Ok(Decision {
            events: if held && newer {
                vec![Event::RemoteForgotten {
                    workspace_id: workspace_id.clone(),
                    participant: publication.participant.clone(),
                }]
            } else {
                Vec::new()
            },
            outcome: Outcome::RemoteObserved {
                participant: publication.participant.clone(),
                accepted: held && newer,
            },
        });
    }
    let events = if newer {
        vec![Event::RemoteObserved {
            peer: RemotePeer {
                workspace_id: workspace_id.clone(),
                publication: publication.clone(),
                received_ms: now_ms,
                generation: state.generation,
            },
        }]
    } else {
        Vec::new()
    };
    Ok(Decision {
        events,
        outcome: Outcome::RemoteObserved {
            participant: publication.participant.clone(),
            accepted: newer,
        },
    })
}

pub(crate) fn decide_remote_withdrawn(
    state: &State,
    workspace_id: &String,
) -> Result<Decision, CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    let link = state
        .remote_links
        .get(workspace_id)
        .ok_or_else(|| error("remote_not_enabled", "This workspace has never published"))?;
    let pending = !link.enabled && !link.withdrawn;
    Ok(Decision {
        events: if pending {
            vec![Event::RemoteWithdrawn {
                workspace_id: workspace_id.clone(),
            }]
        } else {
            Vec::new()
        },
        outcome: Outcome::RemoteDisabled {
            workspace_id: workspace_id.clone(),
        },
    })
}

// The body clones the matched `String`s, as the arm did.
#[allow(clippy::ptr_arg)]
pub(crate) fn decide_forget_remote(
    state: &State,
    workspace_id: &String,
    participant: &String,
) -> Result<Decision, CoreError> {
    remote_link(state, workspace_id)?;
    validate_participant(participant, "-")?;
    let held = state
        .remote_peers
        .contains_key(&remote_key(workspace_id, participant));
    Ok(Decision {
        events: if held {
            vec![Event::RemoteForgotten {
                workspace_id: workspace_id.clone(),
                participant: participant.clone(),
            }]
        } else {
            Vec::new()
        },
        outcome: Outcome::RemoteObserved {
            participant: participant.clone(),
            accepted: held,
        },
    })
}

pub(crate) fn apply_remote_enabled(state: &mut State, link: &RemoteLink) -> Result<(), CoreError> {
    validate_remote_link(state, link)?;
    state
        .remote_links
        .insert(link.workspace_id.clone(), link.clone());
    Ok(())
}

pub(crate) fn apply_remote_disabled(
    state: &mut State,
    workspace_id: &String,
) -> Result<(), CoreError> {
    let Some(link) = state.remote_links.get_mut(workspace_id) else {
        return Err(error(
            "invalid_event",
            "Publication was not enabled for this workspace",
        ));
    };
    link.enabled = false;
    link.withdrawn = false;
    // What teammates published stays theirs, but this workspace no
    // longer takes part, so it stops showing it.
    state
        .remote_peers
        .retain(|_, peer| &peer.workspace_id != workspace_id);
    Ok(())
}

// The body clones the matched `String`, as the arm did.
#[allow(clippy::ptr_arg)]
pub(crate) fn apply_remote_published(
    state: &mut State,
    workspace_id: &String,
    sequence: &u64,
    signature: &String,
) -> Result<(), CoreError> {
    let link = state.remote_links.get_mut(workspace_id).ok_or_else(|| {
        error(
            "invalid_event",
            "A publication record names a workspace that does not publish",
        )
    })?;
    if *sequence != link.sequence + 1 || signature.len() > 64 {
        return Err(error(
            "invalid_event",
            "A publication record must carry the next sequence",
        ));
    }
    link.sequence = *sequence;
    link.signature = signature.clone();
    Ok(())
}

pub(crate) fn apply_remote_observed(state: &mut State, peer: &RemotePeer) -> Result<(), CoreError> {
    validate_publication(&peer.publication)?;
    if peer.generation != state.generation
        || !state
            .remote_links
            .get(&peer.workspace_id)
            .is_some_and(|link| link.enabled)
    {
        return Err(error(
            "invalid_event",
            "A received publication belongs to a publishing workspace in the current generation",
        ));
    }
    state.remote_peers.insert(
        remote_key(&peer.workspace_id, &peer.publication.participant),
        peer.clone(),
    );
    Ok(())
}

pub(crate) fn apply_remote_withdrawn(
    state: &mut State,
    workspace_id: &String,
) -> Result<(), CoreError> {
    if let Some(link) = state.remote_links.get_mut(workspace_id) {
        link.withdrawn = true;
    }
    Ok(())
}

pub(crate) fn apply_remote_forgotten(
    state: &mut State,
    workspace_id: &str,
    participant: &str,
) -> Result<(), CoreError> {
    state
        .remote_peers
        .remove(&remote_key(workspace_id, participant));
    Ok(())
}
