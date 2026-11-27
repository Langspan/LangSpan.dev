//! Observation: disk, branch, Git, scans, checkouts and uncoordinated
//! changes. Their record types and rules, and the `decide_checked` and
//! `apply` arms of their commands and events, which call these functions.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::*;

/// One disk fact. `before: None` means no prior baseline, not a created file.
/// Related operation IDs are unverified correlations, never causal attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiskObservation {
    pub observation_id: String,
    pub workspace_id: String,
    pub path: String,
    pub before: Option<FileState>,
    pub after: FileState,
    pub related_operation_ids: Vec<String>,
    pub observed_at_ms: u64,
    pub generation: u64,
}

/// A local ref fact. Both OIDs are absent for an unborn HEAD or missing branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchObservation {
    pub observation_id: String,
    pub workspace_id: String,
    pub branch_ref: String,
    pub head_oid: Option<String>,
    pub tree_oid: Option<String>,
    pub observed_at_ms: u64,
    pub generation: u64,
}

/// Bounded metadata for the store's most recent completed workspace scan. A
/// missing summary means no baseline, distinct from a scan with zero files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanSummary {
    pub workspace_id: String,
    pub observed_at_ms: u64,
    pub generation: u64,
    pub file_count: u64,
    pub branch_count: u64,
    pub head_oid: Option<String>,
    pub branch_ref: Option<String>,
    /// What the scan did not version. Absent when it versioned every file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<ScanCoverage>,
}

/// Paths listed in a workspace that its last completed scan did not
/// version, by reason. A scan is partial when `omitted` or `too_large` is
/// above zero: some working files of the checkout itself were not read.
/// Submodules and skip-worktree entries are outside the working tree the
/// scan reads, by design; they are counted, never read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ScanCoverage {
    /// Left out because the file-count, byte or time bound was reached.
    /// Files Git reports as changed are versioned before any clean file.
    pub omitted: u64,
    /// Over the per-file bound: listed, not versioned.
    pub too_large: u64,
    /// The first `COVERAGE_PATHS` of them, in path order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub too_large_paths: Vec<String>,
    /// Gitlinks: recorded as present, never read into.
    pub submodules: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub submodule_paths: Vec<String>,
    /// Entries outside a sparse checkout: not in the working tree, and not
    /// reported as deleted.
    pub skip_worktree: u64,
}

/// Paths a coverage record names per reason, at most.
pub const COVERAGE_PATHS: usize = 8;

impl ScanCoverage {
    /// True when some working files of the checkout were not versioned.
    pub fn is_partial(&self) -> bool {
        self.omitted > 0 || self.too_large > 0
    }
}

/// The repository and branch a physical workspace was last observed on. The
/// repository identity groups linked worktrees; it never merges their
/// admission domains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGit {
    pub workspace_id: String,
    pub repository_id: String,
    pub branch_ref: Option<String>,
}

/// The observer could not complete a scan of a workspace. Until one
/// completes, disk facts, check staleness marks and the index are as of the
/// last complete scan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScanFailure {
    pub workspace_id: String,
    pub detail: String,
    pub at_ms: u64,
    pub generation: u64,
}

/// The longest line an adapter renders for an uncoordinated change, before
/// its ellipsis. A briefing that carries one takes this line, plus its
/// ellipsis and newline, out of `MESSAGE_BRIEF_BYTES`: both are delivered
/// once, so every message and withdrawal selected beside it must still be
/// shown.
pub const UNCOORDINATED_LINE_LIMIT: usize = 700;

/// Checkout changes kept per workspace.
pub const MAX_CHECKOUTS: usize = 4;

/// Host names one host check may report.
pub const MAX_HOSTS: usize = 16;

/// An agent host whose configuration directory exists on this machine but
/// which no LangSpan install covers: no user-level install, and no project
/// install in a registered workspace. Only its name is kept; what the owner
/// can run is derived from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UninstalledHost {
    pub host: String,
    /// When the daemon first saw it so, by its own clock.
    pub at_ms: u64,
    pub generation: u64,
}

/// The checkout of one physical workspace changed under the sessions in it:
/// its branch changed, its HEAD moved and files changed with it, or several
/// changed files went back to their committed version at once (`git
/// checkout`, `switch`, `reset --hard`, `stash`, `pull`). Built from the
/// scan's branch observation. Who did it is not known, unless a session's
/// shell command explains it (`CheckoutRun`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutChange {
    pub change_id: String,
    pub workspace_id: String,
    pub from_branch: Option<String>,
    pub to_branch: Option<String>,
    pub from_head: Option<String>,
    pub to_head: Option<String>,
    /// Files whose content changed in the same scan.
    pub files: u32,
    pub at_ms: u64,
    pub generation: u64,
}

/// Uncoordinated changes kept per workspace.
pub const MAX_UNCOORDINATED: usize = 4;
/// Changed files one uncoordinated change keeps, at most.
pub const MAX_UNCOORDINATED_FILES: usize = 8;
/// Codex sessions one uncoordinated change names, at most.
pub const MAX_UNCOORDINATED_SESSIONS: usize = 2;
/// Changed files one scan reports for comparison, at most.
pub const MAX_UNCOORDINATED_OBSERVED: usize = 256;
/// The host name that starts a Codex session's ID (`codex.<host session>`).
pub const CODEX_HOST: &str = "codex";
/// The host names that start the other hooked hosts' session IDs.
pub const CLAUDE_CODE_HOST: &str = "claude-code";
pub const CURSOR_HOST: &str = "cursor";
pub const ANTIGRAVITY_HOST: &str = "antigravity";
/// The tool name of a shell call on every host, as the hooks record it.
pub const SHELL_TOOL: &str = "Bash";

/// Files a scan saw change in one physical workspace that no session of it
/// reported, while Codex sessions are registered there (issue #129). Codex
/// trusts project hooks by the exact path of their file, so a session resumed
/// under another path for the same directory runs none of them. The sessions
/// named are the likely cause, not a proven author: a hand edit looks the same.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UncoordinatedChange {
    pub change_id: String,
    pub workspace_id: String,
    /// The Codex sessions registered here, most recently active first.
    pub sessions: Vec<String>,
    /// The versions the scan saw, sorted by path.
    pub files: Vec<ObservedFile>,
    pub at_ms: u64,
    pub generation: u64,
    /// Sessions this change was delivered to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered: Vec<String>,
}

/// An uncoordinated change as one session's briefing carries it: the files
/// still at a version no session has reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UncoordinatedNotice {
    pub sessions: Vec<String>,
    pub paths: Vec<String>,
    pub age_ms: u64,
}

/// A checkout change as one session's briefing carries it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckoutNotice {
    pub from_branch: Option<String>,
    pub to_branch: Option<String>,
    pub from_head: Option<String>,
    pub to_head: Option<String>,
    pub files: u32,
    pub age_ms: u64,
    /// The other session whose shell command explains the change, when one
    /// does (`CheckoutRun`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_session: Option<String>,
    /// That command's line, redacted and capped by its hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Shell runs that moved a checkout, kept per workspace.
pub const MAX_CHECKOUT_RUNS: usize = 8;
/// A checkout change first observed up to this long after a run finished,
/// at the state the run left, is the run's: the observer scans every few
/// seconds and may see the change only after the run's hook reported it.
pub const CHECKOUT_RUN_GRACE_MS: u64 = 60_000;
/// The longest a checkout run is taken to have run: a shell call's
/// supported lifetime, two hours.
pub const MAX_CHECKOUT_RUN_MS: u64 = 2 * 60 * 60 * 1000;
/// The command text a checkout run keeps, at most, in bytes.
pub const MAX_CHECKOUT_COMMAND_BYTES: usize = 256;

/// One session's shell command ran a Git operation that changes the
/// checkout (`git merge`, `pull`, `rebase`, `reset`, `checkout`, `switch`,
/// `stash` and the like) and the checkout changed while it ran: reported by
/// that session's post-tool hook. A checkout change it explains is not news
/// to that session, and other sessions are told whose command it was.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutRun {
    pub run_id: String,
    pub workspace_id: String,
    pub session_id: String,
    /// The command line, redacted and capped by the hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The checkout the command left.
    pub to_branch: Option<String>,
    pub to_head: Option<String>,
    /// When the command started and finished on the daemon's clock, placed
    /// back from the report's arrival by the durations its hook measured
    /// (never after the arrival), whenever it is reported.
    pub started_ms: u64,
    pub finished_ms: u64,
    pub generation: u64,
    /// The command changed no file's content: it moved HEAD or the branch
    /// only, as a commit does. Absent from an earlier record.
    #[serde(default, skip_serializing_if = "is_false")]
    pub files_unchanged: bool,
}

impl CheckoutRun {
    /// Whether this run explains `change`: observed while the command ran,
    /// or the first change observed after it finished, within
    /// `CHECKOUT_RUN_GRACE_MS`, at the checkout the command left, unless a
    /// change while it ran already showed that checkout or the command
    /// changed no file and the change left branch and HEAD in place.
    /// `changes` are the workspace's changes, to find the first.
    pub fn explains(&self, change: &CheckoutChange, changes: &[CheckoutChange]) -> bool {
        if change.workspace_id != self.workspace_id
            || change.generation != self.generation
            || change.at_ms < self.started_ms
        {
            return false;
        }
        if change.at_ms <= self.finished_ms {
            return true;
        }
        let left = |other: &CheckoutChange| {
            other.to_head == self.to_head && other.to_branch == self.to_branch
        };
        // A run that changed no file (a commit) explains no later change
        // with branch and HEAD in place (a restore): the scan records no
        // change for such a run, so that one came after it.
        let moved = change.from_head != change.to_head || change.from_branch != change.to_branch;
        change.at_ms <= self.finished_ms.saturating_add(CHECKOUT_RUN_GRACE_MS)
            && left(change)
            && (moved || !self.files_unchanged)
            && !changes.iter().any(|other| {
                other.change_id != change.change_id
                    && other.workspace_id == self.workspace_id
                    && other.generation == self.generation
                    && other.at_ms < change.at_ms
                    // Another change after the run came first; one while it
                    // ran at the state it left already showed its result.
                    && (other.at_ms > self.finished_ms
                        || (other.at_ms >= self.started_ms && left(other)))
            })
    }
}

/// Whether an active, unexpired lease of this workspace holds `path`.
fn leased(state: &State, workspace_id: &str, path: &str, now_ms: u64) -> bool {
    state.leases.values().any(|lease| {
        lease.workspace_id == workspace_id
            && lease.status == LeaseStatus::Active
            && lease.generation == state.generation
            && lease.expires_at_ms > now_ms
            && lease.paths.iter().any(|held| held == path)
    })
}

/// Whether a session of this workspace reported reading or writing the file
/// at this version.
fn explained(state: &State, workspace_id: &str, file: &ObservedFile) -> bool {
    state
        .sessions
        .values()
        .filter(|session| session.workspace_id == workspace_id)
        .filter_map(|session| state.activity.get(&session.session_id))
        .any(|activity| {
            activity
                .known
                .iter()
                .any(|known| known.path == file.path && known.state == file.state)
        })
}

/// Whether a session of this workspace reported reading or writing the file
/// at or after `from_ms`, at any version.
fn reported_since(state: &State, workspace_id: &str, path: &str, from_ms: u64) -> bool {
    state
        .sessions
        .values()
        .filter(|session| session.workspace_id == workspace_id)
        .filter_map(|session| state.activity.get(&session.session_id))
        .any(|activity| {
            activity
                .known
                .iter()
                .any(|known| known.path == path && known.at_ms >= from_ms)
        })
}

/// The Codex sessions registered in this workspace in this generation, most
/// recently active first. An ended or exited one is kept: Codex reports no
/// session end, and a resumed session is a new process.
fn codex_sessions(state: &State, workspace_id: &str) -> Vec<String> {
    let mut sessions: Vec<(u64, &str)> = state
        .sessions
        .values()
        .filter(|session| {
            session.workspace_id == workspace_id
                && session.generation == state.generation
                && session
                    .session_id
                    .split_once('.')
                    .is_some_and(|(host, _)| host == CODEX_HOST)
        })
        .map(|session| {
            let last = state
                .activity
                .get(&session.session_id)
                .filter(|activity| activity.generation == state.generation)
                .map_or(0, |activity| activity.last_ms);
            (last, session.session_id.as_str())
        })
        .collect();
    sessions.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    sessions.into_iter().map(|(_, id)| id.to_owned()).collect()
}

/// The files of an uncoordinated change that are still at a version no
/// session of its workspace reported: a report arriving after the scan
/// explains the rest.
fn still_unexplained(state: &State, change: &UncoordinatedChange) -> Vec<String> {
    change
        .files
        .iter()
        .filter(|file| !explained(state, &change.workspace_id, file))
        .map(|file| file.path.clone())
        .collect()
}

/// This workspace's uncoordinated changes of this generation, within the
/// briefing horizon, with a file still unexplained. Newest first.
fn held_uncoordinated<'a>(
    state: &'a State,
    workspace_id: &str,
    now_ms: u64,
) -> impl Iterator<Item = &'a UncoordinatedChange> {
    state
        .uncoordinated
        .get(workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(move |change| {
            change.generation == state.generation
                && now_ms.saturating_sub(change.at_ms) <= BRIEF_HORIZON_MS
                && !still_unexplained(state, change).is_empty()
        })
}

/// The uncoordinated change owed to this session's next briefing, the
/// newest, with the notice it carries: one always fits the rendered text, and
/// each is delivered once, so the rest follow at the next briefings.
pub(crate) fn uncoordinated_owed<'a>(
    state: &'a State,
    session_id: &'a str,
    now_ms: u64,
) -> impl Iterator<Item = (&'a UncoordinatedChange, UncoordinatedNotice)> {
    let workspace = state
        .sessions
        .get(session_id)
        .map_or("", |session| session.workspace_id.as_str());
    held_uncoordinated(state, workspace, now_ms)
        .filter(move |change| !change.delivered.iter().any(|held| held == session_id))
        .take(1)
        .map(move |change| {
            (
                change,
                UncoordinatedNotice {
                    sessions: change.sessions.clone(),
                    paths: still_unexplained(state, change),
                    age_ms: now_ms.saturating_sub(change.at_ms),
                },
            )
        })
}

fn validate_uncoordinated(state: &State, change: &UncoordinatedChange) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &change.change_id,
        &change.workspace_id,
        change.generation,
    )?;
    if change.files.is_empty()
        || change.files.len() > MAX_UNCOORDINATED_FILES
        || normalized_files(&change.files)? != change.files
        || change.sessions.is_empty()
        || change.sessions.len() > MAX_UNCOORDINATED_SESSIONS
        || change.sessions.iter().any(|session| {
            state
                .sessions
                .get(session)
                .is_none_or(|known| known.workspace_id != change.workspace_id)
        })
    {
        return Err(error(
            "invalid_event",
            &format!(
                "An uncoordinated change names 1 to {MAX_UNCOORDINATED_FILES} sorted, unique files and 1 to {MAX_UNCOORDINATED_SESSIONS} sessions of its workspace"
            ),
        ));
    }
    Ok(())
}

/// A write was recorded in the workspace: its write generation moves on, so
/// no check result is compared with one from before it to learn which tests
/// flip.
fn wrote_in(state: &mut State, workspace_id: &str, session_id: Option<&str>) {
    classes::wrote(state, workspace_id, session_id);
}

fn validate_workspace_git(state: &State, git: &WorkspaceGit) -> Result<(), CoreError> {
    validate_id(&git.workspace_id, "workspace ID")?;
    validate_id(&git.repository_id, "repository ID")?;
    if !state.workspaces.contains_key(&git.workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Register the workspace before recording its repository",
        ));
    }
    if let Some(reference) = &git.branch_ref
        && (reference.len() > 512
            || reference.chars().any(char::is_control)
            || !reference
                .strip_prefix("refs/heads/")
                .is_some_and(|name| !name.is_empty()))
    {
        return Err(error(
            "invalid_branch_ref",
            "A workspace branch is a nonempty refs/heads/ name, at most 512 bytes without control characters",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::ptr_arg)]
pub(crate) fn decide_observe_disk(
    state: &State,
    workspace_id: &String,
    path: &String,
    before: &Option<FileState>,
    after: &FileState,
    related_operation_ids: &Vec<String>,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let observation = DiskObservation {
        observation_id: request_id.into(),
        workspace_id: workspace_id.clone(),
        path: path.clone(),
        before: before.clone(),
        after: after.clone(),
        related_operation_ids: normalized_related_ids(related_operation_ids)?,
        observed_at_ms: now_ms,
        generation: state.generation,
    };
    validate_disk_observation(state, &observation)?;
    Ok(Decision {
        events: vec![Event::DiskObserved {
            observation: observation.clone(),
        }],
        outcome: Outcome::DiskObserved { observation },
    })
}

#[allow(clippy::ptr_arg)]
pub(crate) fn decide_observe_branch(
    state: &State,
    workspace_id: &String,
    branch_ref: &String,
    head_oid: &Option<String>,
    tree_oid: &Option<String>,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let observation = BranchObservation {
        observation_id: request_id.into(),
        workspace_id: workspace_id.clone(),
        branch_ref: branch_ref.clone(),
        head_oid: head_oid.clone(),
        tree_oid: tree_oid.clone(),
        observed_at_ms: now_ms,
        generation: state.generation,
    };
    validate_branch_observation(state, &observation)?;
    Ok(Decision {
        events: vec![Event::BranchObserved {
            observation: observation.clone(),
        }],
        outcome: Outcome::BranchObserved { observation },
    })
}

#[allow(clippy::ptr_arg)]
pub(crate) fn decide_observe_workspace_git(
    state: &State,
    workspace_id: &String,
    repository_id: &String,
    branch_ref: &Option<String>,
) -> Result<Decision, CoreError> {
    let git = WorkspaceGit {
        workspace_id: workspace_id.clone(),
        repository_id: repository_id.clone(),
        branch_ref: branch_ref.clone(),
    };
    validate_workspace_git(state, &git)?;
    let events = if state.workspace_git.get(workspace_id) == Some(&git) {
        Vec::new()
    } else {
        vec![Event::WorkspaceGitObserved { git: git.clone() }]
    };
    Ok(Decision {
        events,
        outcome: Outcome::WorkspaceGit { git },
    })
}

pub(crate) fn decide_observe_scan(
    state: &State,
    workspace_id: &String,
    failure: &Option<String>,
    coverage: &Option<ScanCoverage>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    let held = state.scan_failures.get(workspace_id);
    let mut events = match failure {
        Some(detail) => {
            let detail: String = detail
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(200)
                .collect();
            // The same failure repeated is one fact, not many.
            if held.is_some_and(|held| held.detail == detail && held.generation == state.generation)
            {
                Vec::new()
            } else {
                vec![Event::ScanFailed {
                    failure: ScanFailure {
                        workspace_id: workspace_id.clone(),
                        detail,
                        at_ms: now_ms,
                        generation: state.generation,
                    },
                }]
            }
        }
        None if held.is_some() => vec![Event::ScanRecovered {
            workspace_id: workspace_id.clone(),
        }],
        None => Vec::new(),
    };
    // Coverage is of the last completed scan: a failed attempt
    // leaves it as it was, and an unchanged one is not a new fact.
    if failure.is_none() {
        let coverage = coverage
            .clone()
            .filter(|coverage| coverage != &ScanCoverage::default());
        if let Some(coverage) = &coverage {
            validate_coverage(coverage)?;
        }
        if state.scan_coverage.get(workspace_id) != coverage.as_ref() {
            events.push(Event::ScanCoverageObserved {
                workspace_id: workspace_id.clone(),
                coverage,
            });
        }
    }
    Ok(Decision {
        events,
        outcome: Outcome::ScanObserved {
            workspace_id: workspace_id.clone(),
            failed: failure.is_some(),
        },
    })
}

#[allow(clippy::ptr_arg)]
pub(crate) fn decide_observe_hosts(
    state: &State,
    not_installed: &Vec<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    if not_installed.len() > MAX_HOSTS {
        return Err(error(
            "invalid_hosts",
            &format!("a host check names at most {MAX_HOSTS} hosts"),
        ));
    }
    let now_missing: BTreeSet<&str> = not_installed.iter().map(String::as_str).collect();
    if now_missing.len() != not_installed.len() {
        return Err(error("invalid_hosts", "a host is named more than once"));
    }
    for host in &now_missing {
        validate_host_name(host)?;
    }
    // Each change of a host's state is one fact; an unchanged
    // host is not recorded again.
    let mut events: Vec<Event> = state
        .uninstalled_hosts
        .keys()
        .filter(|host| !now_missing.contains(host.as_str()))
        .map(|host| Event::HostCleared { host: host.clone() })
        .collect();
    events.extend(
        now_missing
            .iter()
            .filter(|host| !state.uninstalled_hosts.contains_key(**host))
            .map(|host| Event::HostNotInstalled {
                host: UninstalledHost {
                    host: (*host).into(),
                    at_ms: now_ms,
                    generation: state.generation,
                },
            }),
    );
    Ok(Decision {
        events,
        outcome: Outcome::HostsObserved {
            not_installed: now_missing.iter().map(|host| (*host).into()).collect(),
        },
    })
}

#[allow(clippy::too_many_arguments, clippy::ptr_arg)]
pub(crate) fn decide_observe_checkout(
    state: &State,
    workspace_id: &String,
    from_branch: &Option<String>,
    to_branch: &Option<String>,
    from_head: &Option<String>,
    to_head: &Option<String>,
    files: &u32,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let change = CheckoutChange {
        change_id: request_id.into(),
        workspace_id: workspace_id.clone(),
        from_branch: from_branch.clone(),
        to_branch: to_branch.clone(),
        from_head: from_head.clone(),
        to_head: to_head.clone(),
        files: *files,
        at_ms: now_ms,
        generation: state.generation,
    };
    validate_checkout(state, &change)?;
    Ok(Decision {
        events: vec![Event::CheckoutChanged {
            change: change.clone(),
        }],
        outcome: Outcome::CheckoutObserved { change },
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_observe_checkout_run(
    state: &State,
    session_id: &String,
    command: &Option<String>,
    to_branch: &Option<String>,
    to_head: &Option<String>,
    ran_ms: &u64,
    finished_ago_ms: &u64,
    files_unchanged: &bool,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let Some(session) = state.sessions.get(session_id) else {
        return Err(error(
            "unknown_session",
            "A checkout run names a registered session; register it first",
        ));
    };
    // When the command ran, not when its report arrived: a
    // redelivered report credits no later change. Placed back from
    // the daemon's own now by durations, so never after it; a run
    // two hours at most, a shell call's lifetime.
    let finished_ms = now_ms.saturating_sub(*finished_ago_ms);
    let run = CheckoutRun {
        run_id: request_id.into(),
        workspace_id: session.workspace_id.clone(),
        session_id: session_id.clone(),
        command: command.clone(),
        to_branch: to_branch.clone(),
        to_head: to_head.clone(),
        started_ms: finished_ms.saturating_sub((*ran_ms).min(MAX_CHECKOUT_RUN_MS)),
        finished_ms,
        generation: state.generation,
        files_unchanged: *files_unchanged,
    };
    validate_checkout_run(state, &run)?;
    Ok(Decision {
        events: vec![Event::CheckoutRunObserved { run: run.clone() }],
        outcome: Outcome::CheckoutRunObserved { run },
    })
}

#[allow(clippy::ptr_arg)]
pub(crate) fn decide_observe_uncoordinated(
    state: &State,
    workspace_id: &String,
    files: &Vec<ObservedFile>,
    scanned_from_ms: &Option<u64>,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if files.is_empty() || files.len() > MAX_UNCOORDINATED_OBSERVED {
        return Err(error(
            "invalid_path",
            &format!(
                "An uncoordinated change names 1 to {MAX_UNCOORDINATED_OBSERVED} changed files"
            ),
        ));
    }
    let files = normalized_files(files)?;
    // A file under an admitted edit is that edit's, whose report has
    // not arrived yet; a version a session reported is explained; and
    // a file a session reported after the scan began reading is newer
    // than what the scan saw: the version before that session's edit.
    let unexplained: Vec<ObservedFile> = files
        .into_iter()
        .filter(|file| {
            !leased(state, workspace_id, &file.path, now_ms)
                && !explained(state, workspace_id, file)
                && !scanned_from_ms
                    .is_some_and(|from| reported_since(state, workspace_id, &file.path, from))
        })
        .take(MAX_UNCOORDINATED_FILES)
        .collect();
    // A Codex session already named in a notice still held is not
    // named again: one notice per session until it ages out.
    let named: BTreeSet<&str> = held_uncoordinated(state, workspace_id, now_ms)
        .flat_map(|change| change.sessions.iter().map(String::as_str))
        .collect();
    let sessions = if unexplained.is_empty() {
        Vec::new()
    } else {
        codex_sessions(state, workspace_id)
            .into_iter()
            .filter(|session| !named.contains(session.as_str()))
            .take(MAX_UNCOORDINATED_SESSIONS)
            .collect()
    };
    if sessions.is_empty() {
        return Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::UncoordinatedObserved {
                sessions: Vec::new(),
                paths: Vec::new(),
            },
        });
    }
    let change = UncoordinatedChange {
        change_id: request_id.into(),
        workspace_id: workspace_id.clone(),
        sessions: sessions.clone(),
        files: unexplained,
        at_ms: now_ms,
        generation: state.generation,
        delivered: Vec::new(),
    };
    validate_uncoordinated(state, &change)?;
    Ok(Decision {
        outcome: Outcome::UncoordinatedObserved {
            sessions,
            paths: change.files.iter().map(|file| file.path.clone()).collect(),
        },
        events: vec![Event::UncoordinatedChanged { change }],
    })
}

pub(crate) fn apply_operation_observed(
    state: &mut State,
    observation: &Observation,
) -> Result<(), CoreError> {
    validate_request_id(&observation.observation_id)?;
    let session = known_session(state, &observation.session_id)?;
    for (id, label) in [
        (&observation.operation_id, "operation ID"),
        (&observation.source, "observation source"),
        (&observation.tool, "tool name"),
    ] {
        validate_id(id, label)?;
    }
    validate_canonical_paths(&observation.paths)?;
    if observation.workspace_id != session.workspace_id
        || observation.generation != state.generation
    {
        return Err(error(
            "invalid_event",
            "Observation must match its session's workspace and the current generation",
        ));
    }
    if let Some(lease) = observation
        .lease_id
        .as_ref()
        .and_then(|id| state.leases.get(id))
        && lease.session_id != observation.session_id
    {
        return Err(error(
            "invalid_event",
            "Observation names another session's admission",
        ));
    }
    // Observations grant nothing. Older versions classified scope and
    // same-session holders differently; preserve those historical
    // decisions rather than rejecting an existing event log.
    if observation.result == ToolResult::Succeeded {
        if !observation.paths.is_empty() {
            wrote_in(
                state,
                &observation.workspace_id,
                Some(&observation.session_id),
            );
        }
        let activity = activity_mut(state, &observation.session_id);
        activity.wrote_since_boundary |= !observation.paths.is_empty();
        // The file is no longer the version the session knew. Until
        // the adapter reports what it became, its base is unknown,
        // never the old version.
        activity
            .known
            .retain(|version| !observation.paths.contains(&version.path));
        activity.last_ms = activity.last_ms.max(observation.observed_at_ms);
        for path in &observation.paths {
            activity.touched.retain(|touch| &touch.path != path);
            activity.touched.insert(
                0,
                TouchedPath {
                    path: path.clone(),
                    at_ms: observation.observed_at_ms,
                },
            );
        }
        activity.touched.truncate(MAX_TOUCHED);
    }
    Ok(())
}

pub(crate) fn apply_workspace_git_observed(
    state: &mut State,
    git: &WorkspaceGit,
) -> Result<(), CoreError> {
    validate_workspace_git(state, git)?;
    state
        .workspace_git
        .insert(git.workspace_id.clone(), git.clone());
    Ok(())
}

pub(crate) fn apply_scan_failed(state: &mut State, failure: &ScanFailure) -> Result<(), CoreError> {
    validate_known_workspace(state, &failure.workspace_id)?;
    state
        .scan_failures
        .insert(failure.workspace_id.clone(), failure.clone());
    Ok(())
}

pub(crate) fn apply_scan_recovered(
    state: &mut State,
    workspace_id: &String,
) -> Result<(), CoreError> {
    state.scan_failures.remove(workspace_id);
    Ok(())
}

pub(crate) fn apply_scan_coverage_observed(
    state: &mut State,
    workspace_id: &String,
    coverage: &Option<ScanCoverage>,
) -> Result<(), CoreError> {
    validate_known_workspace(state, workspace_id)?;
    match coverage {
        Some(coverage) => {
            validate_coverage(coverage)?;
            state
                .scan_coverage
                .insert(workspace_id.clone(), coverage.clone());
        }
        None => {
            state.scan_coverage.remove(workspace_id);
        }
    }
    Ok(())
}

pub(crate) fn apply_host_not_installed(
    state: &mut State,
    host: &UninstalledHost,
) -> Result<(), CoreError> {
    validate_host_name(&host.host)?;
    if !state.uninstalled_hosts.contains_key(&host.host)
        && state.uninstalled_hosts.len() >= MAX_HOSTS
    {
        return Err(error(
            "invalid_hosts",
            &format!("at most {MAX_HOSTS} hosts are held as not installed"),
        ));
    }
    state
        .uninstalled_hosts
        .insert(host.host.clone(), host.clone());
    Ok(())
}

pub(crate) fn apply_host_cleared(state: &mut State, host: &String) -> Result<(), CoreError> {
    state.uninstalled_hosts.remove(host);
    Ok(())
}

pub(crate) fn apply_checkout_changed(
    state: &mut State,
    change: &CheckoutChange,
) -> Result<(), CoreError> {
    validate_checkout(state, change)?;
    let changes = state
        .checkouts
        .entry(change.workspace_id.clone())
        .or_default();
    changes.insert(0, change.clone());
    changes.truncate(MAX_CHECKOUTS);
    Ok(())
}

pub(crate) fn apply_checkout_run_observed(
    state: &mut State,
    run: &CheckoutRun,
) -> Result<(), CoreError> {
    validate_checkout_run(state, run)?;
    let runs = state
        .checkout_runs
        .entry(run.workspace_id.clone())
        .or_default();
    runs.insert(0, run.clone());
    // Commits add runs and no change, so the two lists move apart.
    // An older run stays while it explains a change still held, so
    // that change keeps its attribution.
    let changes = state
        .checkouts
        .get(&run.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let explaining: BTreeSet<usize> = changes
        .iter()
        .filter_map(|change| runs.iter().position(|run| run.explains(change, changes)))
        .collect();
    let mut index = 0;
    runs.retain(|_| {
        index += 1;
        index <= MAX_CHECKOUT_RUNS || explaining.contains(&(index - 1))
    });
    Ok(())
}

pub(crate) fn apply_uncoordinated_changed(
    state: &mut State,
    change: &UncoordinatedChange,
) -> Result<(), CoreError> {
    validate_uncoordinated(state, change)?;
    if !change.delivered.is_empty() {
        return Err(error(
            "invalid_event",
            "A new uncoordinated change has not been delivered to any session",
        ));
    }
    let changes = state
        .uncoordinated
        .entry(change.workspace_id.clone())
        .or_default();
    changes.insert(0, change.clone());
    changes.truncate(MAX_UNCOORDINATED);
    Ok(())
}

pub(crate) fn apply_uncoordinated_delivered(
    state: &mut State,
    workspace_id: &String,
    change_id: &String,
    session_id: &String,
) -> Result<(), CoreError> {
    if known_session(state, session_id)?.workspace_id != *workspace_id {
        return Err(error(
            "invalid_event",
            "An uncoordinated change is delivered to a session of its workspace",
        ));
    }
    // A change already truncated away has nothing left to mark.
    if let Some(change) = state
        .uncoordinated
        .get_mut(workspace_id)
        .and_then(|changes| changes.iter_mut().find(|c| c.change_id == *change_id))
        && !change.delivered.contains(session_id)
    {
        change.delivered.push(session_id.clone());
    }
    Ok(())
}

pub(crate) fn apply_disk_observed(
    state: &mut State,
    observation: &DiskObservation,
) -> Result<(), CoreError> {
    validate_disk_observation(state, observation)?;
    // A first baseline is not a change. Anything else means the
    // working tree a check ran on is no longer the working tree.
    let changed = observation
        .before
        .as_ref()
        .is_some_and(|before| before != &observation.after);
    if changed {
        wrote_in(state, &observation.workspace_id, None);
    }
    if changed && let Some(records) = state.checks.get_mut(&observation.workspace_id) {
        for record in records.iter_mut().filter(|record| record.inputs.is_empty()) {
            record.changed_since = true;
        }
    }
    Ok(())
}

pub(crate) fn apply_branch_observed(
    state: &mut State,
    observation: &BranchObservation,
) -> Result<(), CoreError> {
    validate_branch_observation(state, observation)?;
    Ok(())
}
