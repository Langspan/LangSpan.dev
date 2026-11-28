//! Publication of a workspace's work summary to teammates through the
//! repository's own Git remote, and receipt of theirs.
//!
//! Each participant owns exactly one ref on the remote,
//! `refs/langspan/peers/<participant>`, or, when the owner chose
//! `--publish-as-branch` for a host that accepts pushes only to branches,
//! `refs/heads/langspan/peers/<participant>`. Its tip commit holds one small
//! `state.json`. Readers look in both. Git's own transport authenticates the
//! exchange and the remote's push permission scopes who may take part;
//! LangSpan never sees a credential. Nothing is published unless the owner
//! enabled it for the workspace. See ADR 0006.

use crate::git;
use langspan_core::{Publication, RemoteLink};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

/// The largest `state.json` written or read.
pub const PAYLOAD_LIMIT: usize = langspan_core::PUBLICATION_BYTES;
/// The most teammates read from one remote in one pass.
pub const PEER_LIMIT: usize = 64;
const PAYLOAD_VERSION: u32 = 1;

/// What this build writes.
#[derive(Serialize)]
struct Payload<'a> {
    v: u32,
    publication: &'a Publication,
}

/// What this build reads from a teammate: the same fields, with any others
/// ignored, so a peer running a newer build that adds a field is still
/// heard ([ADR 0015](../docs/adr/0015-version-tolerance.md)). The read is
/// bounded to `PAYLOAD_LIMIT` bytes before parsing, and the core validates
/// every value it keeps (`ObserveRemote`). A change that removes or redefines
/// a field bumps `v`, and a different `v` is not read.
#[derive(Deserialize)]
struct PeerPayload {
    v: u32,
    publication: PeerPublication,
}

#[derive(Deserialize)]
struct PeerPublication {
    participant: String,
    name: String,
    sequence: u64,
    work: Vec<PeerWork>,
    /// The participant has left; absent from older builds' publications.
    #[serde(default)]
    withdrawn: bool,
}

#[derive(Deserialize)]
struct PeerWork {
    host: String,
    session: String,
    branch_ref: Option<String>,
    summary: Option<String>,
    editing: Vec<String>,
    recent: Vec<String>,
}

impl From<PeerPublication> for Publication {
    fn from(peer: PeerPublication) -> Self {
        Publication {
            participant: peer.participant,
            name: peer.name,
            sequence: peer.sequence,
            work: peer
                .work
                .into_iter()
                .map(|work| langspan_core::RemoteWork {
                    host: work.host,
                    session: work.session,
                    branch_ref: work.branch_ref,
                    summary: work.summary,
                    editing: work.editing,
                    recent: work.recent,
                })
                .collect(),
            withdrawn: peer.withdrawn,
        }
    }
}

/// Where participants publish by default: outside the branch list.
const DEFAULT_PEERS: &str = "refs/langspan/peers/";
/// Where a participant publishes when the owner chose `--publish-as-branch`,
/// for hosts that accept pushes only to branches. Every reader looks in both.
const BRANCH_PEERS: &str = "refs/heads/langspan/peers/";

/// Both namespaces a reader fetches, each with the local prefix its refs are
/// tracked under. The branch namespace's tracking refs are LangSpan's own,
/// never the owner's remote-tracking branches.
fn namespaces(link: &RemoteLink) -> [(&'static str, String); 2] {
    [
        (
            DEFAULT_PEERS,
            format!("refs/langspan/remote/{}/", link.remote),
        ),
        (
            BRANCH_PEERS,
            format!("refs/langspan/remote-branch/{}/", link.remote),
        ),
    ]
}

/// The ref this link publishes to on the remote.
pub fn peer_ref(link: &RemoteLink) -> String {
    let namespace = if link.publish_as_branch {
        BRANCH_PEERS
    } else {
        DEFAULT_PEERS
    };
    format!("{namespace}{}", link.participant)
}

/// The local tracking ref for `link`'s own publication in its namespace.
fn own_tracking(link: &RemoteLink) -> String {
    let [default, branch] = namespaces(link);
    let (_, prefix) = if link.publish_as_branch {
        branch
    } else {
        default
    };
    format!("{prefix}{}", link.participant)
}

/// A refusal by the remote, and whether it is one that repeating the same
/// push will not change (permission, authentication, a host rule).
#[derive(Debug)]
pub struct Refusal {
    pub detail: String,
    pub permanent: bool,
}

impl From<&str> for Refusal {
    fn from(detail: &str) -> Self {
        detail.to_owned().into()
    }
}

impl From<String> for Refusal {
    fn from(detail: String) -> Self {
        Self {
            detail,
            permanent: false,
        }
    }
}

impl From<crate::git::GitError> for Refusal {
    fn from(error: crate::git::GitError) -> Self {
        error.to_string().into()
    }
}

/// True when Git's text says the host decided, rather than that it could not
/// be reached: HTTP 401 or 403, or a refusal, denial or protection rule.
pub fn permanent(said: &str) -> bool {
    let lower = said.to_ascii_lowercase();
    [
        "error: 401",
        "error: 403",
        "http 401",
        "http 403",
        "refusing",
        "denied",
        "protected",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Git's words, labelled as Git's, for an error message.
fn quoted(said: &str) -> String {
    if said.is_empty() {
        String::new()
    } else {
        format!(" Git said: \"{said}\".")
    }
}

/// The command that publishes this link as a branch instead, restating the
/// owner's other choices so that nothing else changes.
fn as_branch_command(link: &RemoteLink) -> String {
    let mut command = format!(
        "langspan remote enable {} --name {:?}",
        link.workspace_id, link.name
    );
    if link.remote != "origin" {
        command.push_str(&format!(" --remote {}", link.remote));
    }
    if link.share_task {
        command.push_str(" --share-task on");
    }
    command.push_str(" --publish-as-branch");
    command
}

/// The error for a publication the remote would not take.
fn refused_publication(link: &RemoteLink, how: &str, said: &str) -> Refusal {
    let mut detail = format!(
        "Git remote {:?} refused the publication to {}{how}.{}",
        link.remote,
        peer_ref(link),
        quoted(said)
    );
    if link.publish_as_branch {
        detail.push_str(" Check that you may push branches to it.");
    } else if said.contains("403") {
        detail.push_str(&format!(
            " The host answered HTTP 403 for a ref outside refs/heads/, as hosts that accept pushes only to branches do. To publish as a branch instead, visible to everyone in the repository's branch list, run `{}`.",
            as_branch_command(link)
        ));
    } else {
        detail.push_str(&format!(
            " Check that you may push to it. If this host accepts pushes only to branches, `{}` publishes as a branch instead, visible to everyone in the repository's branch list.",
            as_branch_command(link)
        ));
    }
    Refusal {
        permanent: permanent(said),
        detail,
    }
}

fn line(bytes: Vec<u8>) -> Result<Option<String>, String> {
    let text = String::from_utf8(bytes).map_err(|_| "Git output is not UTF-8")?;
    let text = text.trim_end_matches('\n');
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

fn object(text: Option<String>) -> Result<String, String> {
    text.filter(|oid| {
        matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
    .ok_or_else(|| "Git did not return an object ID".into())
}

/// A stable random-looking identity for this state directory, created once.
/// It identifies a participant to teammates; it is not a secret.
pub fn participant_id(state_dir: &Path) -> Result<String, crate::Error> {
    let path = state_dir.join("participant");
    if let Ok(existing) = fs::read_to_string(&path) {
        let existing = existing.trim();
        if existing.len() == 32 && existing.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(existing.to_ascii_lowercase());
        }
        return Err("participant file is not a 32-character hexadecimal ID".into());
    }
    let seed = format!(
        "{}|{}|{:?}|{}",
        std::process::id(),
        state_dir.display(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::env::var("HOSTNAME").unwrap_or_default()
    );
    let id = crate::sha256::hex(&crate::sha256::digest(seed.as_bytes()))[..32].to_owned();
    let mut file = crate::local::private_file(&path, true)?;
    file.write_all(id.as_bytes())?;
    Ok(id)
}

pub struct Received {
    pub publications: Vec<Publication>,
    /// Participants that have a ref on the remote now, so a caller can tell
    /// a withdrawal from an unreadable publication.
    pub present: Vec<String>,
    /// False when more participants exist than are read in one pass. An
    /// absent participant then proves nothing.
    pub complete: bool,
}

/// Bring teammates' refs up to date and return what each currently says. A
/// publication that cannot be read or parsed is skipped; one bad teammate
/// does not hide the others. Both namespaces are read, so a teammate who
/// publishes as a branch and one who does not see each other; a participant
/// found in both is taken at the higher sequence.
pub fn fetch(root: &Path, link: &RemoteLink, deadline: Instant) -> Result<Received, String> {
    let spaces = namespaces(link);
    let refspecs: Vec<String> = spaces
        .iter()
        .map(|(remote, local)| format!("+{remote}*:{local}*"))
        .collect();
    // `fetch.pruneTags` off: pruning here must only ever touch LangSpan's refs.
    let mut args = vec![
        "-c",
        "fetch.pruneTags=false",
        "fetch",
        "--quiet",
        "--no-tags",
        "--prune",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        // An empty refmap: without it Git would also update the owner's
        // remote-tracking branches for refs that match their own fetch
        // refspec, which every branch publication does.
        "--refmap=",
        &link.remote,
    ];
    args.extend(refspecs.iter().map(String::as_str));
    let (status, said) = git::network(root, &args, deadline)?;
    if !status.success() {
        return Err(format!(
            "could not fetch from Git remote {:?}; check that it is reachable and that you are signed in to it.{}",
            link.remote,
            quoted(&said)
        ));
    }
    let mut listing_args = vec!["for-each-ref", "--format=%(refname) %(objectname)"];
    listing_args.extend(spaces.iter().map(|(_, local)| local.as_str()));
    let listing = git::output(root, &listing_args, deadline, 1024 * 1024)?;
    let listing = String::from_utf8(listing).map_err(|_| "Git ref listing is not UTF-8")?;
    let mut held = std::collections::BTreeMap::<String, Publication>::new();
    let mut present = Vec::<String>::new();
    let mut complete = true;
    for entry in listing.lines() {
        let Some((name, oid)) = entry.split_once(' ') else {
            continue;
        };
        let Some(participant) = spaces
            .iter()
            .find_map(|(_, local)| name.strip_prefix(local.as_str()))
        else {
            continue;
        };
        if participant.len() != 32
            || !participant
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            continue;
        }
        // Refs that are not participant refs were skipped above, so junk
        // on the remote cannot use up the limit.
        if !present.iter().any(|known| known == participant) {
            if present.len() == PEER_LIMIT {
                complete = false;
                break;
            }
            present.push(participant.to_owned());
        }
        if participant == link.participant {
            continue;
        }
        let Some(publication) = read(root, oid, participant, deadline) else {
            continue;
        };
        match held.get(participant) {
            Some(other) if other.sequence >= publication.sequence => {}
            _ => {
                held.insert(participant.to_owned(), publication);
            }
        }
    }
    Ok(Received {
        publications: held.into_values().collect(),
        present,
        complete,
    })
}

/// The publication a tracking ref's commit holds, if it is a valid one from
/// `participant`.
fn read(root: &Path, oid: &str, participant: &str, deadline: Instant) -> Option<Publication> {
    let oid = object(Some(oid.to_owned())).ok()?;
    let bytes = git::output(
        root,
        &["cat-file", "blob", &format!("{oid}:state.json")],
        deadline,
        PAYLOAD_LIMIT,
    )
    .ok()?;
    // The ref name is the identity the remote vouches for; a payload that
    // names someone else is not believed.
    match serde_json::from_slice::<PeerPayload>(&bytes) {
        Ok(payload)
            if payload.v == PAYLOAD_VERSION && payload.publication.participant == participant =>
        {
            Some(payload.publication.into())
        }
        _ => None,
    }
}

/// This participant's own refs on the remote, in either namespace, as the
/// last fetch saw them: (remote ref, tracking ref).
fn own_refs(
    root: &Path,
    link: &RemoteLink,
    deadline: Instant,
) -> Result<Vec<(String, String)>, String> {
    let mut found = Vec::new();
    for (remote, local) in namespaces(link) {
        let tracking = format!("{local}{}", link.participant);
        let listed = git::output(
            root,
            &["for-each-ref", "--format=%(refname)", &tracking],
            deadline,
            1024,
        )?;
        if !listed.is_empty() {
            found.push((format!("{remote}{}", link.participant), tracking));
        }
    }
    Ok(found)
}

/// Write the publication as the next commit on this participant's ref and
/// push it. Call after `fetch`, so the parent is what the remote holds.
/// Returns the exact `state.json` that was pushed.
pub fn publish(
    root: &Path,
    state_dir: &Path,
    link: &RemoteLink,
    publication: &Publication,
    deadline: Instant,
) -> Result<String, Refusal> {
    let payload = serde_json::to_vec(&Payload {
        v: PAYLOAD_VERSION,
        publication,
    })
    .map_err(|e| e.to_string())?;
    if payload.len() > PAYLOAD_LIMIT {
        return Err(String::from("publication exceeds 16 KiB; nothing was published").into());
    }
    let scratch = state_dir.join(format!("publish-{}.json", link.workspace_id));
    let index = state_dir.join(format!("publish-{}.index", link.workspace_id));
    let result = (|| -> Result<(), Refusal> {
        let _ = fs::remove_file(&scratch);
        let mut file = crate::local::private_file(&scratch, true).map_err(|e| e.to_string())?;
        file.write_all(&payload).map_err(|e| e.to_string())?;
        drop(file);
        let scratch_text = scratch.to_str().ok_or("state directory is not UTF-8")?;
        let blob = object(line(git::output(
            root,
            &["hash-object", "-w", "--no-filters", scratch_text],
            deadline,
            256,
        )?)?)?;
        // A private index keeps the owner's index and working tree untouched.
        let _ = fs::remove_file(&index);
        let env: [(&str, &OsStr); 1] = [("GIT_INDEX_FILE", index.as_os_str())];
        git::output_with_env(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},state.json"),
            ],
            &env,
            deadline,
            256,
        )?;
        let tree = object(line(git::output_with_env(
            root,
            &["write-tree"],
            &env,
            deadline,
            256,
        )?)?)?;
        // No parent: the ref holds the current state and no history. What
        // was published earlier, including a task line the owner has since
        // stopped sharing, is not kept reachable from it.
        let message = format!("langspan publication {}", publication.sequence);
        let args = [
            "-c",
            "user.name=LangSpan",
            "-c",
            "user.email=langspan@localhost.invalid",
            "commit-tree",
            &tree,
            "-m",
            &message,
        ];
        let commit = object(line(git::output(root, &args, deadline, 256)?)?)?;
        // Replacing this participant's own single-writer ref. The ref
        // carries no source, and the owner's pre-push hooks are written for
        // their branches: do not run them for it.
        let target = format!("+{commit}:{}", peer_ref(link));
        let (status, said) = push(root, link, &target, deadline)?;
        if status.success() {
            return Ok(());
        }
        // Some remotes refuse every forced update, even of a ref nobody else
        // writes (a "block force pushes" rule, or a hosting proxy). Publish
        // the same tree as an ordinary fast-forward on top of what the remote
        // holds. Earlier publications then stay reachable in the ref's
        // history: ADR 0006 records the cost.
        let parent = git::output(
            root,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                &own_tracking(link),
            ],
            deadline,
            256,
        )
        .ok()
        .and_then(|bytes| line(bytes).ok().flatten());
        let Some(parent) = parent else {
            return Err(refused_publication(link, "", &said));
        };
        let child = object(line(git::output(
            root,
            &[
                "-c",
                "user.name=LangSpan",
                "-c",
                "user.email=langspan@localhost.invalid",
                "commit-tree",
                &tree,
                "-p",
                &parent,
                "-m",
                &message,
            ],
            deadline,
            256,
        )?)?)?;
        let target = format!("{child}:{}", peer_ref(link));
        let (status, said) = push(root, link, &target, deadline)?;
        if !status.success() {
            return Err(refused_publication(
                link,
                ", forced and as a fast-forward",
                &said,
            ));
        }
        Ok(())
    })();
    let _ = fs::remove_file(&scratch);
    let _ = fs::remove_file(&index);
    result?;
    Ok(String::from_utf8(payload).map_err(|_| "publication payload is not UTF-8")?)
}

/// Push one publication. A push to a branch also makes Git write the
/// owner's remote-tracking branch for it (`refs/remotes/<remote>/...`); when
/// that ref did not exist before, it is removed again, so the owner's refs
/// stay as they were.
fn push(
    root: &Path,
    link: &RemoteLink,
    target: &str,
    deadline: Instant,
) -> Result<(std::process::ExitStatus, String), String> {
    let tracking = peer_ref(link)
        .strip_prefix("refs/heads/")
        .map(|branch| format!("refs/remotes/{}/{branch}", link.remote));
    let listed = |name: &str| {
        git::output(
            root,
            &["for-each-ref", "--format=%(objectname)", name],
            deadline,
            256,
        )
        .map(|bytes| line(bytes).ok().flatten())
    };
    let existed = match &tracking {
        Some(name) => listed(name)?.is_some(),
        None => true,
    };
    let result = git::network(
        root,
        &["push", "--quiet", "--no-verify", &link.remote, target],
        deadline,
    )?;
    if let Some(name) = tracking.filter(|_| !existed)
        && let Some(written) = listed(&name)?
    {
        git::output(root, &["update-ref", "-d", &name, &written], deadline, 256)?;
    }
    Ok(result)
}

/// What `withdraw` achieved when the remote still holds a ref.
pub enum Withdrawal {
    /// Every ref of this participant is gone from the remote.
    Gone,
    /// The host refused to delete `remote_ref`. If `told` is true, the ref
    /// now holds a final "withdrawn" publication, so teammates stop showing
    /// this participant's work; the ref itself is still there.
    Refused {
        remote_ref: String,
        said: String,
        told: bool,
    },
}

/// Remove this participant's refs from the remote, in whichever namespace
/// they are: the owner stopped publishing, and teammates should stop seeing
/// the last state. Call after `fetch`, so the tracking refs show what the
/// remote holds.
pub fn withdraw(root: &Path, link: &RemoteLink, deadline: Instant) -> Result<Withdrawal, String> {
    // Every ref is tried: a refused one that already says "withdrawn" must
    // not hide another, still active, in the other namespace. The first ref
    // that still shows work is reported, so it gets its own withdrawal.
    let mut refused_told = None;
    for (remote_ref, tracking) in own_refs(root, link, deadline)? {
        let (status, said) = git::network(
            root,
            &[
                "push",
                "--quiet",
                "--no-verify",
                &link.remote,
                &format!(":{remote_ref}"),
            ],
            deadline,
        )?;
        if !status.success() {
            if !holds_withdrawal(root, &tracking, link, deadline) {
                return Ok(Withdrawal::Refused {
                    remote_ref,
                    said,
                    told: false,
                });
            }
            refused_told.get_or_insert((remote_ref, said));
        }
    }
    Ok(match refused_told {
        Some((remote_ref, said)) => Withdrawal::Refused {
            remote_ref,
            said,
            told: true,
        },
        None => Withdrawal::Gone,
    })
}

/// Whether the remote's copy of this participant's ref already says it
/// withdrew.
fn holds_withdrawal(root: &Path, tracking: &str, link: &RemoteLink, deadline: Instant) -> bool {
    git::output(
        root,
        &["for-each-ref", "--format=%(objectname)", tracking],
        deadline,
        256,
    )
    .ok()
    .and_then(|bytes| line(bytes).ok().flatten())
    .and_then(|oid| read(root, &oid, &link.participant, deadline))
    .is_some_and(|publication| publication.withdrawn)
}

/// The final publication sent in place of a deletion the host refused.
pub fn withdrawal(link: &RemoteLink, remote_ref: &str) -> (Publication, RemoteLink) {
    let mut target = link.clone();
    // Publish to the ref that is still there, in its own namespace.
    target.publish_as_branch = remote_ref.starts_with(BRANCH_PEERS);
    (
        Publication {
            participant: link.participant.clone(),
            name: link.name.clone(),
            sequence: link.sequence + 1,
            work: Vec::new(),
            withdrawn: true,
        },
        target,
    )
}

/// The owner's message for a deletion the host refused.
pub fn refused_withdrawal(link: &RemoteLink, remote_ref: &str, said: &str, told: bool) -> String {
    format!(
        "Git remote {:?} refused to delete {remote_ref}.{} LangSpan has stopped publishing{}. The ref itself is still on the remote: delete it by hand where the host allows, for example in the hosting site's branch list or with `git push {} --delete {remote_ref}` from a machine that may delete refs there. LangSpan checks again on later passes.",
        link.remote,
        quoted(said),
        if told {
            ", and the ref now holds a final publication saying so, so teammates no longer show your work"
        } else {
            "; until the ref holds a final publication saying so, teammates still see your last one"
        },
        link.remote,
    )
}

/// After a move to the other namespace, remove this participant's ref from
/// the namespace it no longer publishes in, once the remote holds the ref in
/// the new one (so teammates never see a gap). Call after `fetch`, on every
/// pass: it pushes only when there is something to remove.
pub fn retire_other(root: &Path, link: &RemoteLink, deadline: Instant) -> Result<(), Refusal> {
    let current = peer_ref(link);
    let own = own_refs(root, link, deadline)?;
    if !own.iter().any(|(remote_ref, _)| remote_ref == &current) {
        return Ok(());
    }
    for (remote_ref, _) in own {
        if remote_ref == current {
            continue;
        }
        let (status, said) = git::network(
            root,
            &[
                "push",
                "--quiet",
                "--no-verify",
                &link.remote,
                &format!(":{remote_ref}"),
            ],
            deadline,
        )?;
        if !status.success() {
            return Err(Refusal {
                permanent: permanent(&said),
                detail: format!(
                    "Git remote {:?} refused to delete your earlier publication at {remote_ref}, left from before you moved to {current}.{} Teammates take the newer of the two, so they see the current one; delete the old ref by hand where the host allows.",
                    link.remote,
                    quoted(&said)
                ),
            });
        }
    }
    Ok(())
}

/// How long to wait after the second, third, fourth and later consecutive
/// failure of the same kind. A first failure is retried at the next pass.
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

/// Spacing of attempts against a remote that keeps failing the same way,
/// such as refused authentication or an unreachable host, so a broken remote
/// costs a pass every ten minutes rather than every few seconds. The clock is
/// the caller's.
#[derive(Debug, Default)]
pub struct Backoff {
    class: Option<&'static str>,
    failures: usize,
    retry_at: Option<Instant>,
}

impl Backoff {
    /// Whether an attempt may run at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|at| now >= at)
    }

    /// Record a failure of `class` at `now` and return the wait before the
    /// next attempt. A failure of another class starts the count again.
    pub fn failed(&mut self, class: &'static str, now: Instant) -> Duration {
        if self.class != Some(class) {
            self.class = Some(class);
            self.failures = 0;
        }
        self.failures += 1;
        let wait = match self.failures {
            1 => Duration::ZERO,
            n => BACKOFF[(n - 2).min(BACKOFF.len() - 1)],
        };
        self.retry_at = Some(now + wait);
        wait
    }

    /// Record a refusal the host will repeat (permission, authentication, a
    /// host rule) at `now`: wait the longest interval at once.
    pub fn refused(&mut self, class: &'static str, now: Instant) -> Duration {
        let longest = BACKOFF[BACKOFF.len() - 1];
        self.class = Some(class);
        self.failures = BACKOFF.len() + 1;
        self.retry_at = Some(now + longest);
        longest
    }

    /// A pass that reached the remote: the next one runs on schedule.
    pub fn succeeded(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_failures_of_one_kind_back_off_to_ten_minutes_and_success_resets() {
        let start = Instant::now();
        let mut backoff = Backoff::default();
        let mut now = start;
        let mut waits = Vec::new();
        for _ in 0..7 {
            assert!(backoff.due(now));
            let wait = backoff.failed("fetch", now);
            waits.push(wait.as_secs());
            if !wait.is_zero() {
                assert!(!backoff.due(now + wait - Duration::from_millis(1)));
            }
            now += wait;
        }
        assert_eq!(waits, [0, 5, 30, 120, 600, 600, 600]);

        // Another kind of failure is counted from the start.
        assert_eq!(backoff.failed("publish", now), Duration::ZERO);
        assert_eq!(backoff.failed("publish", now), Duration::from_secs(5));

        backoff.succeeded();
        assert!(backoff.due(now));
        assert_eq!(backoff.failed("publish", now), Duration::ZERO);
    }

    #[test]
    fn a_refusal_for_good_waits_the_longest_at_once_and_stays_there() {
        let now = Instant::now();
        let mut backoff = Backoff::default();
        assert_eq!(backoff.refused("publish", now), Duration::from_secs(600));
        assert!(!backoff.due(now + Duration::from_secs(599)));
        assert!(backoff.due(now + Duration::from_secs(600)));
        // Refused again after the wait: still the longest, not the start.
        let later = now + Duration::from_secs(600);
        assert_eq!(backoff.failed("publish", later), Duration::from_secs(600));
        backoff.succeeded();
        assert!(backoff.due(later));
    }

    #[test]
    fn a_refusal_is_permanent_only_when_git_says_the_host_decided() {
        for said in [
            "error: RPC failed; HTTP 403 curl 22 The requested URL returned error: 403",
            "fatal: unable to access: The requested URL returned error: 401",
            "remote: error: refusing refs/langspan/peers/x",
            "git@example.com: Permission denied (publickey).",
            "remote: error: GH006: Protected branch update failed",
        ] {
            assert!(permanent(said), "{said}");
        }
        for said in [
            "fatal: unable to access: Could not resolve host: example.com",
            "ssh: connect to host example.com port 22: Connection timed out",
            "",
        ] {
            assert!(!permanent(said), "{said}");
        }
    }
}
