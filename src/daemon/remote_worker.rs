//! The host worker, which checks the agent hosts for a LangSpan install, and
//! the remote worker, which receives and publishes over the network with a
//! backoff, both off the state owner's thread.

use super::unix::{Job, ask, now_ms};
use langspan_core::{Command, Outcome};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

const REMOTE_DEADLINE: Duration = Duration::from_secs(20);
/// Unchanged work is republished this often, so teammates can tell a
/// quiet participant from a disconnected one.
const REMOTE_HEARTBEAT: Duration = Duration::from_secs(120);

/// Check the agent hosts in the daemon's home at start and every
/// `every` after (roadmap A7). Reads directory entries and LangSpan's
/// own entries in host files, never writes them; what it finds reaches
/// the state owner as one request naming the uncovered hosts.
pub(super) fn host_worker(sender: mpsc::SyncSender<Job>, directory: PathBuf, every: Duration) {
    let home = match crate::hosts::UserHome::new(None) {
        Ok(home) => home,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"warn","event":"host_check_unavailable","detail":error.to_string()})
            );
            return;
        }
    };
    let mut checks: u64 = 0;
    let mut last_failure: Option<String> = None;
    loop {
        let known = match ask(&sender, "read".into(), Command::Status) {
            Some(Outcome::Status { state, .. }) => Some((
                state
                    .workspaces
                    .values()
                    .map(|workspace| PathBuf::from(&workspace.root))
                    .collect::<Vec<_>>(),
                state.uninstalled_hosts.keys().cloned().collect::<Vec<_>>(),
            )),
            _ => None,
        };
        let failure = match known {
            // The owner is busy or stopping: the next check counts.
            None => Some("the state owner did not answer".to_owned()),
            Some((roots, held)) => {
                let reports = crate::hosts::report(&home, &directory);
                // A host that could not be inspected keeps the notice
                // it has, and the reason is logged.
                let observation = crate::hosts::observe(&reports, &roots, &directory, &held);
                checks += 1;
                let answer = match ask(
                    &sender,
                    format!("hosts-{}-{checks}", now_ms()),
                    Command::ObserveHosts {
                        not_installed: observation.not_installed,
                    },
                ) {
                    Some(Outcome::HostsObserved { .. }) => None,
                    Some(Outcome::Error { code, message }) => Some(format!("{code}: {message}")),
                    _ => Some("the state owner did not answer".to_owned()),
                };
                let mut problems = observation.errors;
                problems.extend(answer);
                (!problems.is_empty()).then(|| problems.join("; "))
            }
        };
        if failure != last_failure {
            if let Some(detail) = &failure {
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"warn","event":"host_check_failed","detail":detail})
                );
            }
            last_failure = failure;
        }
        std::thread::sleep(every);
    }
}

/// Why a remote pass failed. `class` names the network operation for a
/// failure that waiting can help (an unreachable remote, refused
/// authentication, a deadline); it is `None` for one that the remote
/// answered, which never delays the next pass.
struct RemoteFailure {
    class: Option<&'static str>,
    detail: String,
}

/// What the remote worker remembers about one publishing workspace.
#[derive(Default)]
struct Linked {
    seen: std::collections::BTreeSet<String>,
    last_publish: Option<Instant>,
    /// The failure last logged and the wait it caused.
    failure: Option<(String, Duration)>,
    backoff: crate::remote::Backoff,
    refused: Refused,
    retire_refused: Refused,
}

/// A push the host refused for good (HTTP 401/403, refusing, denied,
/// protected): `what` is not pushed again until its own backoff, set
/// straight to the longest wait, is due. A different publication is
/// tried at once. Kept apart from the pass's backoff so that receiving
/// teammates' publications goes on meanwhile.
#[derive(Default)]
struct Refused {
    what: String,
    detail: String,
    backoff: crate::remote::Backoff,
}

impl Refused {
    fn held(&self, what: &str) -> Option<String> {
        (self.what == what && !self.backoff.due(Instant::now())).then(|| self.detail.clone())
    }
    fn hold(what: &str, class: &'static str, detail: &str) -> Self {
        let mut backoff = crate::remote::Backoff::default();
        backoff.refused(class, Instant::now());
        Self {
            what: what.to_owned(),
            detail: detail.to_owned(),
            backoff,
        }
    }
}

fn network(class: &'static str) -> impl FnOnce(String) -> RemoteFailure {
    move |detail| RemoteFailure {
        class: Some(class),
        detail,
    }
}

impl From<String> for RemoteFailure {
    fn from(detail: String) -> Self {
        Self {
            class: None,
            detail,
        }
    }
}

/// One pass for one publishing workspace: receive, then publish if due.
fn remote_pass(
    sender: &mpsc::SyncSender<Job>,
    directory: &Path,
    generation: u64,
    root: &Path,
    link: &langspan_core::RemoteLink,
    linked: &mut Linked,
) -> Result<(), RemoteFailure> {
    let Linked {
        seen,
        last_publish,
        refused,
        retire_refused,
        ..
    } = linked;
    // Ordering and replay are decided by the core from each sender's
    // sequence, so every attempt is its own request.
    static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let generation = generation.to_string();
    let id = |kind: &str, subject: &str| {
        let attempt = ATTEMPT
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string();
        crate::hook::wire_id(kind, &[&link.workspace_id, subject, &generation, &attempt])
    };
    let deadline = Instant::now() + REMOTE_DEADLINE;
    let received = crate::remote::fetch(root, link, deadline).map_err(network("fetch"))?;
    let mut rejected = Vec::new();
    for publication in received.publications {
        let request_id = id("remote-observe", &publication.participant);
        if let Some(Outcome::Error { code, message }) = ask(
            sender,
            request_id,
            Command::ObserveRemote {
                workspace_id: link.workspace_id.clone(),
                publication,
            },
        ) {
            rejected.push(format!("{code}: {message}"));
        }
    }
    // A ref that was there and is gone is a withdrawal, but only when
    // every participant on the remote was listed.
    if received.complete {
        for participant in seen
            .iter()
            .filter(|participant| !received.present.contains(participant))
        {
            ask(
                sender,
                id("remote-forget", participant),
                Command::ForgetRemote {
                    workspace_id: link.workspace_id.clone(),
                    participant: participant.clone(),
                },
            );
        }
        *seen = received
            .present
            .into_iter()
            .filter(|participant| participant != &link.participant)
            .collect();
    }
    // A move to the other namespace leaves the old ref behind; it is
    // removed once the new one is on the remote, checked every pass.
    // A refusal here never stops publication: it is reported, and a
    // refusal for good is asked again only after the longest wait.
    let retire = match retire_refused.held("retire") {
        Some(detail) => Some(detail),
        None => match crate::remote::retire_other(root, link, deadline) {
            Ok(()) => {
                *retire_refused = Refused::default();
                None
            }
            Err(refusal) => {
                if refusal.permanent {
                    *retire_refused = Refused::hold("retire", "retire", &refusal.detail);
                }
                Some(refusal.detail)
            }
        },
    };
    // Reported through the same once-per-change path as any failure.
    let rejections = |rejected: &[String]| match rejected.first() {
        _ if retire.is_some() => Err(retire.clone().unwrap_or_default()),
        None => Ok(()),
        Some(first) => Err(format!(
            "{} publication(s) from teammates were rejected: {first}",
            rejected.len()
        )),
    };
    let Some(Outcome::RemoteOutbox {
        publication,
        signature,
        changed,
    }) = ask(
        sender,
        "read".into(),
        Command::RemoteOutbox {
            workspace_id: link.workspace_id.clone(),
        },
    )
    else {
        return Err(String::from("the daemon did not return this workspace's outbox").into());
    };
    let heartbeat = !publication.work.is_empty()
        && last_publish.is_none_or(|at| at.elapsed() >= REMOTE_HEARTBEAT);
    if !changed && !heartbeat {
        return rejections(&rejected).map_err(RemoteFailure::from);
    }
    // A refusal is the destination's answer about this content: a move
    // to another remote or namespace is asked afresh.
    let refusal_key = format!(
        "{}\n{}\n{signature}",
        link.remote,
        crate::remote::peer_ref(link)
    );
    if let Some(detail) = refused.held(&refusal_key) {
        return Err(detail.into());
    }
    let payload = match crate::remote::publish(root, directory, link, &publication, deadline) {
        Ok(payload) => payload,
        // The host answered: the pass's own backoff is not touched, so
        // receipt goes on; only this push waits.
        Err(refusal) if refusal.permanent => {
            *refused = Refused::hold(&refusal_key, "publish", &refusal.detail);
            return Err(refusal.detail.into());
        }
        Err(refusal) => return Err(network("publish")(refusal.detail)),
    };
    *refused = Refused::default();
    let published_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    *last_publish = Some(Instant::now());
    // Recorded after the push: the log holds what left, not what was
    // tried. If recording fails, the same sequence is pushed again next
    // pass, which replaces the ref with the same content.
    match ask(
        sender,
        id("remote-published", &publication.sequence.to_string()),
        Command::RemotePublished {
            workspace_id: link.workspace_id.clone(),
            sequence: publication.sequence,
            signature,
            payload,
            published_at_ms,
            // Where this push went: the owner may have moved meanwhile.
            as_branch: Some(link.publish_as_branch),
        },
    ) {
        Some(Outcome::Error { code, message }) => Err(format!("{code}: {message}").into()),
        Some(_) => rejections(&rejected).map_err(RemoteFailure::from),
        None => Err(String::from("publication was pushed but could not be recorded").into()),
    }
}

/// Remove a disabled workspace's ref. Repeated every pass, across
/// restarts, until the remote confirms it is gone. Where the host refuses
/// the deletion, the ref is replaced once with a final "withdrawn"
/// publication, so teammates stop showing this machine's work, and the
/// owner is told the ref is still there.
fn remote_withdraw(
    sender: &mpsc::SyncSender<Job>,
    directory: &Path,
    generation: u64,
    root: &Path,
    link: &langspan_core::RemoteLink,
    refused: &mut Refused,
) -> Result<(), RemoteFailure> {
    const WHAT: &str = "withdraw";
    if let Some(detail) = refused.held(WHAT) {
        return Err(detail.into());
    }
    let deadline = Instant::now() + REMOTE_DEADLINE;
    crate::remote::fetch(root, link, deadline).map_err(network("fetch"))?;
    let crate::remote::Withdrawal::Refused {
        remote_ref,
        said,
        mut told,
    } = crate::remote::withdraw(root, link, deadline).map_err(network("withdraw"))?
    else {
        ask(
            sender,
            crate::hook::wire_id(
                "remote-withdrawn",
                &[
                    &link.workspace_id,
                    &link.sequence.to_string(),
                    &generation.to_string(),
                ],
            ),
            Command::RemoteWithdrawn {
                workspace_id: link.workspace_id.clone(),
            },
        );
        *refused = Refused::default();
        return Ok(());
    };
    let mut detail = None;
    if !told {
        let (publication, target) = crate::remote::withdrawal(link, &remote_ref);
        match crate::remote::publish(root, directory, &target, &publication, deadline) {
            Ok(payload) => {
                told = true;
                let published_at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
                // Recorded like any publication, so `remote sent` shows
                // it and enabling again never reuses the number.
                if let Some(Outcome::Error { code, message }) = ask(
                    sender,
                    crate::hook::wire_id(
                        "remote-published",
                        &[
                            &link.workspace_id,
                            &publication.sequence.to_string(),
                            &generation.to_string(),
                        ],
                    ),
                    Command::RemotePublished {
                        workspace_id: link.workspace_id.clone(),
                        sequence: publication.sequence,
                        signature: "withdrawn".into(),
                        payload,
                        published_at_ms,
                        as_branch: None,
                    },
                ) {
                    detail = Some(format!(
                        "the final withdrawn publication was pushed but could not be recorded: {code}: {message}"
                    ));
                }
            }
            Err(refusal) => {
                detail = Some(format!(
                    "the final withdrawn publication was refused too: {}",
                    refusal.detail
                ))
            }
        }
    }
    let mut message = crate::remote::refused_withdrawal(link, &remote_ref, &said, told);
    if let Some(detail) = detail {
        message.push_str(&format!(" ({detail})"));
    }
    if !crate::remote::permanent(&said) {
        return Err(network("withdraw")(message));
    }
    // The host will refuse again: ask it again only after the longest
    // wait, in case the owner deleted the ref by hand meanwhile.
    if told {
        *refused = Refused::hold(WHAT, "withdraw", &message);
    }
    Err(message.into())
}

/// Network work never runs on the state owner's thread. A failure here
/// leaves local coordination exactly as it was.
pub(super) fn remote_worker(
    sender: mpsc::SyncSender<Job>,
    directory: PathBuf,
    generation: u64,
    pass: Duration,
) {
    let mut linked = std::collections::BTreeMap::<String, Linked>::new();
    loop {
        std::thread::sleep(pass);
        let Some(Outcome::Status { state, .. }) = ask(&sender, "read".into(), Command::Status)
        else {
            continue;
        };
        linked.retain(|workspace_id, _| state.remote_links.contains_key(workspace_id));
        for (workspace_id, link) in &state.remote_links {
            let Some(workspace) = state.workspaces.get(workspace_id) else {
                continue;
            };
            let root = Path::new(&workspace.root);
            let entry = linked.entry(workspace_id.clone()).or_default();
            if !entry.backoff.due(Instant::now()) {
                continue;
            }
            let result = if link.enabled {
                remote_pass(&sender, &directory, generation, root, link, entry)
            } else if link.withdrawn {
                continue;
            } else {
                entry.seen.clear();
                entry.last_publish = None;
                remote_withdraw(
                    &sender,
                    &directory,
                    generation,
                    root,
                    link,
                    &mut entry.refused,
                )
            };
            // Local coordination never waits for this: only this
            // workspace's next remote pass is delayed.
            let failure = match result {
                Ok(()) => {
                    entry.backoff.succeeded();
                    None
                }
                Err(RemoteFailure { class, detail }) => {
                    let wait = match class {
                        Some(class) => entry.backoff.failed(class, Instant::now()),
                        None => {
                            entry.backoff.succeeded();
                            Duration::ZERO
                        }
                    };
                    Some((detail, wait))
                }
            };
            // Logged once per change of failure or of the wait it causes.
            match (&failure, &entry.failure) {
                (None, Some(_)) => eprintln!(
                    "{}",
                    serde_json::json!({"level":"info","event":"remote_recovered","workspace_id":workspace_id})
                ),
                (Some((detail, wait)), previous) if previous.as_ref() != failure.as_ref() => {
                    eprintln!(
                        "{}",
                        serde_json::json!({"level":"warn","event":"remote_unavailable","workspace_id":workspace_id,"detail":detail,"retry_in_ms":wait.as_millis() as u64})
                    )
                }
                _ => {}
            }
            entry.failure = failure;
        }
    }
}
