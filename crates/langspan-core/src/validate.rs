//! Input validation: the checks a command or an event passes before it
//! changes state. Pure, and shared by every domain.

use std::collections::BTreeSet;

use crate::{
    BRIEF_HORIZON_MS, BranchObservation, COVERAGE_PATHS, CheckoutChange, CheckoutRun, CoreError,
    DiskObservation, FileState, LeaseStatus, MAX_CHECKOUT_COMMAND_BYTES, MAX_INTENT_SUMMARY_BYTES,
    MAX_PROMPT_TARGETS, MIN_PRESENCE_WINDOW_MS, PRESENCE_PATHS, PresenceKind, PresenceSetting,
    ScanCoverage, Session, SessionLiveness, State, ToolIdentity, Workspace, validate_oid,
};

pub fn validate_request_id(id: &str) -> Result<(), CoreError> {
    validate_id(id, "request ID").map_err(|mut error| {
        error.code = "invalid_request_id".into();
        error
    })
}

/// The most operations one tool-batch record names.
pub const MAX_BATCH_OPERATIONS: usize = 64;

/// A host's error type, such as Claude Code's `rate_limit`: 1 to 40
/// lower-case ASCII letters and underscores. Never free text.
pub(crate) fn validate_failure(failure: &str) -> Result<(), CoreError> {
    if failure.is_empty()
        || failure.len() > 40
        || !failure
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(error(
            "invalid_failure",
            "A stop failure is the host's error type: 1 to 40 lower-case letters or underscores",
        ));
    }
    Ok(())
}

pub(crate) fn validate_batch(operation_ids: &[String], calls: u32) -> Result<(), CoreError> {
    let distinct: BTreeSet<&String> = operation_ids.iter().collect();
    if operation_ids.is_empty()
        || operation_ids.len() > MAX_BATCH_OPERATIONS
        || distinct.len() != operation_ids.len()
        || (calls as usize) < operation_ids.len()
        || operation_ids
            .iter()
            .any(|id| validate_id(id, "operation ID").is_err())
    {
        return Err(error(
            "invalid_batch",
            "A tool batch names 1 to 64 distinct operation IDs and at least as many calls",
        ));
    }
    Ok(())
}

pub(crate) fn validate_id(id: &str, label: &str) -> Result<(), CoreError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(error(
            "invalid_id",
            &format!(
                "{label} must contain 1 to 128 ASCII letters, digits, dots, underscores or hyphens"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn validate_tool_identity(operation: &ToolIdentity) -> Result<(), CoreError> {
    validate_id(&operation.operation_id, "operation ID")?;
    validate_id(&operation.source, "observation source")?;
    validate_id(&operation.tool, "tool name")?;
    if !is_hex_digest(&operation.input_digest, 64) {
        return Err(error(
            "invalid_input_digest",
            "Tool input digest must contain exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

pub(crate) fn is_hex_digest(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn validate_relative_path(path: &str) -> Result<(), CoreError> {
    if path.is_empty()
        || path.len() > 512
        || path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(error(
            "invalid_path",
            "Each file key must be a relative path of at most 512 bytes without empty, dot or parent components, backslashes or NUL",
        ));
    }
    Ok(())
}

pub(crate) fn normalized_paths(paths: &[String]) -> Result<Vec<String>, CoreError> {
    if paths.is_empty() {
        return Err(error(
            "invalid_path",
            "Specify at least one normalized relative file path",
        ));
    }
    let mut unique = BTreeSet::new();
    for path in paths {
        validate_relative_path(path)?;
        unique.insert(path.clone());
        if unique.len() > 64 {
            return Err(error(
                "too_many_paths",
                "A command can name at most 64 unique file paths; split independent work into separate operations",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

/// Prompt target paths, sorted and without repeats: 1 to
/// `MAX_PROMPT_TARGETS` normalized relative paths.
pub(crate) fn prompt_target_paths(paths: &[String]) -> Result<Vec<String>, CoreError> {
    let paths = normalized_paths(paths)?;
    if paths.len() > MAX_PROMPT_TARGETS {
        return Err(error(
            "too_many_paths",
            &format!("A prompt names at most {MAX_PROMPT_TARGETS} target paths"),
        ));
    }
    Ok(paths)
}

/// Presence paths, sorted and without repeats: 1 to `PRESENCE_PATHS`
/// normalized relative paths, for a search or a shell command.
pub(crate) fn presence_paths(
    kind: PresenceKind,
    paths: &[String],
) -> Result<Vec<String>, CoreError> {
    if kind == PresenceKind::Read {
        return Err(error(
            "invalid_presence",
            "A read is recorded as an observed source version, not as presence",
        ));
    }
    let paths = normalized_paths(paths)?;
    if paths.len() > PRESENCE_PATHS {
        return Err(error(
            "too_many_paths",
            &format!("A presence record names at most {PRESENCE_PATHS} paths"),
        ));
    }
    Ok(paths)
}

pub(crate) fn validate_presence_setting(setting: &PresenceSetting) -> Result<(), CoreError> {
    if !(MIN_PRESENCE_WINDOW_MS..=BRIEF_HORIZON_MS).contains(&setting.window_ms) {
        return Err(error(
            "invalid_limit",
            "A presence window is between 1 and 30 minutes",
        ));
    }
    Ok(())
}

pub(crate) fn normalized_related_ids(ids: &[String]) -> Result<Vec<String>, CoreError> {
    let mut unique = BTreeSet::new();
    for id in ids {
        validate_id(id, "related operation ID")?;
        unique.insert(id.clone());
        if unique.len() > 64 {
            return Err(error(
                "too_many_related_operations",
                "A disk observation may correlate at most 64 distinct operations",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

pub(crate) fn validate_fact_header(
    state: &State,
    observation_id: &str,
    workspace_id: &str,
    generation: u64,
) -> Result<(), CoreError> {
    validate_request_id(observation_id)?;
    validate_id(workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Observation workspace is not registered; register it before recording facts",
        ));
    }
    if generation != state.generation {
        return Err(error(
            "invalid_event",
            "Observation must belong to the current daemon generation",
        ));
    }
    Ok(())
}

fn validate_file_state(state: &FileState) -> Result<(), CoreError> {
    if let FileState::Present { digest, .. } = state
        && !is_hex_digest(digest, 64)
    {
        return Err(error(
            "invalid_content_digest",
            "Observed file content digest must contain exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

/// Validate a prepared workspace-relative file key and its observed version.
/// Full snapshot projections use the same rules even for clean or unchanged
/// files, so an accepted baseline cannot become impossible to update later.
pub fn validate_observed_file(path: &str, version: &FileState) -> Result<(), CoreError> {
    validate_relative_path(path)?;
    validate_file_state(version)
}

pub(crate) fn validate_disk_observation(
    state: &State,
    observation: &DiskObservation,
) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &observation.observation_id,
        &observation.workspace_id,
        observation.generation,
    )?;
    validate_observed_file(&observation.path, &observation.after)?;
    if let Some(before) = &observation.before {
        validate_file_state(before)?;
    }
    if normalized_related_ids(&observation.related_operation_ids)?
        != observation.related_operation_ids
    {
        return Err(error(
            "invalid_event",
            "Related operation IDs in an event must already be sorted and unique",
        ));
    }
    Ok(())
}

pub(crate) fn validate_branch_observation(
    state: &State,
    observation: &BranchObservation,
) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &observation.observation_id,
        &observation.workspace_id,
        observation.generation,
    )?;
    let reference = &observation.branch_ref;
    if reference.len() > 512
        || reference.chars().any(char::is_control)
        || (reference != "HEAD"
            && !reference
                .strip_prefix("refs/heads/")
                .is_some_and(|name| !name.is_empty()))
    {
        return Err(error(
            "invalid_branch_ref",
            "Branch reference must be HEAD or a nonempty refs/heads/ name, at most 512 bytes without control characters",
        ));
    }
    if observation.head_oid.is_some() != observation.tree_oid.is_some() {
        return Err(error(
            "invalid_branch_oids",
            "Supply head and tree together; omit both only for an absent or unborn reference",
        ));
    }
    for oid in [&observation.head_oid, &observation.tree_oid]
        .into_iter()
        .flatten()
    {
        if !is_hex_digest(oid, 40) && !is_hex_digest(oid, 64) {
            return Err(error(
                "invalid_git_oid",
                "Observed Git object IDs must contain 40 or 64 hexadecimal characters",
            ));
        }
    }
    Ok(())
}

/// A host's command-line name: lower-case letters and hyphens, as the
/// adapters name them (`claude-code`, `antigravity`).
pub(crate) fn validate_host_name(host: &str) -> Result<(), CoreError> {
    if host.is_empty()
        || host.len() > 32
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
    {
        return Err(error(
            "invalid_hosts",
            "a host name is 1 to 32 lower-case letters and hyphens",
        ));
    }
    Ok(())
}

pub(crate) fn validate_coverage(coverage: &ScanCoverage) -> Result<(), CoreError> {
    for (paths, count) in [
        (&coverage.too_large_paths, coverage.too_large),
        (&coverage.submodule_paths, coverage.submodules),
    ] {
        if paths.len() > COVERAGE_PATHS || paths.len() as u64 > count {
            return Err(error(
                "invalid_coverage",
                "A coverage record names at most 8 paths per reason, and no more than it counts",
            ));
        }
        for path in paths {
            validate_relative_path(path)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_checkout(state: &State, change: &CheckoutChange) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &change.change_id,
        &change.workspace_id,
        change.generation,
    )?;
    validate_checkout_state(
        [&change.from_branch, &change.to_branch],
        [&change.from_head, &change.to_head],
    )?;
    if change.from_branch == change.to_branch
        && change.from_head == change.to_head
        && change.files == 0
    {
        return Err(error(
            "invalid_checkout",
            "A checkout change needs a different branch or HEAD, or changed files",
        ));
    }
    Ok(())
}

pub(crate) fn validate_checkout_run(state: &State, run: &CheckoutRun) -> Result<(), CoreError> {
    validate_fact_header(state, &run.run_id, &run.workspace_id, run.generation)?;
    if state
        .sessions
        .get(&run.session_id)
        .is_none_or(|session| session.workspace_id != run.workspace_id)
    {
        return Err(error(
            "unknown_session",
            "A checkout run names a session registered in its workspace",
        ));
    }
    validate_checkout_state([&run.to_branch], [&run.to_head])?;
    if run.command.as_ref().is_some_and(|command| {
        command.len() > MAX_CHECKOUT_COMMAND_BYTES || command.chars().any(char::is_control)
    }) {
        return Err(error(
            "invalid_command",
            "A checkout run's command is at most 256 bytes without control characters",
        ));
    }
    if run.started_ms > run.finished_ms {
        return Err(error(
            "invalid_event",
            "A checkout run cannot start after it finished",
        ));
    }
    Ok(())
}

/// Branches are nonempty `refs/heads/` names; heads are Git object IDs.
fn validate_checkout_state<const B: usize, const H: usize>(
    branches: [&Option<String>; B],
    heads: [&Option<String>; H],
) -> Result<(), CoreError> {
    for reference in branches.into_iter().flatten() {
        if reference.len() > 512
            || reference.chars().any(char::is_control)
            || !reference
                .strip_prefix("refs/heads/")
                .is_some_and(|name| !name.is_empty())
        {
            return Err(error(
                "invalid_branch_ref",
                "A checkout branch is a nonempty refs/heads/ name, at most 512 bytes without control characters",
            ));
        }
    }
    for oid in heads.into_iter().flatten() {
        validate_oid(oid)?;
    }
    Ok(())
}

pub(crate) fn validate_canonical_paths(paths: &[String]) -> Result<(), CoreError> {
    if normalized_paths(paths)? != paths {
        return Err(error(
            "invalid_event",
            "Event file keys must already be sorted and unique",
        ));
    }
    Ok(())
}

pub(crate) fn validate_summary(summary: &str) -> Result<(), CoreError> {
    if summary.len() > MAX_INTENT_SUMMARY_BYTES {
        return Err(error(
            "invalid_summary",
            &format!(
                "Intent summary exceeds {MAX_INTENT_SUMMARY_BYTES} UTF-8 bytes; shorten the summary"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn validate_workspace(state: &State, workspace: &Workspace) -> Result<(), CoreError> {
    validate_id(&workspace.workspace_id, "workspace ID")?;
    if workspace.root.is_empty() || workspace.root.contains('\0') {
        return Err(error(
            "invalid_root",
            "Workspace root must be a nonempty canonical path without NUL",
        ));
    }
    if normalized_workspace_links(&workspace.verified_overlaps)? != workspace.verified_overlaps {
        return Err(error(
            "invalid_event",
            "Verified workspace overlaps must already be sorted and unique",
        ));
    }
    validate_workspace_link_targets(
        state,
        &workspace.workspace_id,
        &workspace.root,
        &workspace.verified_overlaps,
    )?;
    if let Some(existing) = state.workspaces.get(&workspace.workspace_id)
        && (existing.root != workspace.root
            || existing.verified_overlaps != workspace.verified_overlaps)
    {
        return Err(error(
            "workspace_conflict",
            "Workspace ID is already bound to a different canonical root or verification record; use its existing registration",
        ));
    }
    for existing in state.workspaces.values() {
        if existing.workspace_id != workspace.workspace_id
            && roots_overlap(&existing.root, &workspace.root)
        {
            let verified = workspace
                .verified_overlaps
                .binary_search(&existing.workspace_id)
                .is_ok()
                || existing
                    .verified_overlaps
                    .binary_search(&workspace.workspace_id)
                    .is_ok();
            if existing.root == workspace.root || !verified {
                return Err(error(
                    "workspace_conflict",
                    "This root overlaps another workspace without a verified distinct worktree boundary; reuse its registered ID",
                ));
            }
        }
    }
    if !state.workspaces.contains_key(&workspace.workspace_id) {
        validate_no_parent_claims(state, workspace)?;
    }
    Ok(())
}

fn validate_no_parent_claims(state: &State, workspace: &Workspace) -> Result<(), CoreError> {
    for lease in state.leases.values() {
        if lease.status != LeaseStatus::Active || lease.generation != state.generation {
            continue;
        }
        let Some(parent) = state.workspaces.get(&lease.workspace_id) else {
            continue;
        };
        // A verified overlap whose roots do not overlap as text, such as a
        // registered root that is now a link: the core cannot tell which of
        // the lease's paths lie under this root, so any current admission
        // there makes the registration wait.
        if !roots_overlap(&parent.root, &workspace.root)
            && workspace
                .verified_overlaps
                .binary_search(&parent.workspace_id)
                .is_ok()
        {
            return Err(error(
                "workspace_in_use",
                "A current admission in a workspace this root overlaps through a link may cover it; complete it or wait for expiry, then start a new registration attempt with a new request ID",
            ));
        }
        let prefix = if parent.root.ends_with('/') {
            parent.root.clone()
        } else {
            format!("{}/", parent.root)
        };
        let Some(child_key) = workspace.root.strip_prefix(&prefix) else {
            continue;
        };
        let child_key = child_key.trim_end_matches('/');
        if !child_key.is_empty()
            && lease.paths.iter().any(|path| {
                path == child_key
                    || path
                        .strip_prefix(child_key)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
        {
            return Err(error(
                "workspace_in_use",
                "A current parent-workspace admission covers this subtree; complete it or wait for expiry, then start a new registration attempt with a new request ID",
            ));
        }
    }
    Ok(())
}

pub(crate) fn normalized_workspace_links(ids: &[String]) -> Result<Vec<String>, CoreError> {
    let mut unique = BTreeSet::new();
    for id in ids {
        validate_id(id, "verified workspace ID")?;
        unique.insert(id.clone());
        if unique.len() > 64 {
            return Err(error(
                "too_many_verified_overlaps",
                "A workspace registration can verify at most 64 distinct overlapping workspaces",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

pub(crate) fn validate_workspace_link_targets(
    state: &State,
    workspace_id: &str,
    root: &str,
    ids: &[String],
) -> Result<(), CoreError> {
    for id in ids {
        let other = state.workspaces.get(id).ok_or_else(|| {
            error(
                "invalid_workspace_verification",
                "A verified overlap must name an already registered workspace",
            )
        })?;
        // The roots need not overlap as text: a registered root kept as
        // given can be a link to a directory around or inside `root`, and
        // only the adapter can resolve it.
        if id == workspace_id || other.root == root {
            return Err(error(
                "invalid_workspace_verification",
                "Verified overlaps must name distinct physical roots; identical roots must share one workspace ID",
            ));
        }
    }
    Ok(())
}

// The adapter supplies physical canonical roots. This lexical slash-component
// check is an additional guard; platform-specific alias checks belong there.
fn roots_overlap(a: &str, b: &str) -> bool {
    fn contains(parent: &str, child: &str) -> bool {
        parent == child
            || child
                .strip_prefix(parent)
                .is_some_and(|suffix| parent.ends_with('/') || suffix.starts_with('/'))
    }
    contains(a, b) || contains(b, a)
}

pub(crate) fn validate_session_registration(
    state: &State,
    session: &Session,
) -> Result<(), CoreError> {
    validate_id(&session.session_id, "session ID")?;
    validate_id(&session.workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(&session.workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Workspace is not registered; register it before attaching a session",
        ));
    }
    if session.generation != state.generation {
        return Err(error(
            "invalid_event",
            "Session registration must use the current daemon generation",
        ));
    }
    if let Some(parent) = &session.parent {
        validate_id(parent, "parent session ID")?;
        if *parent == session.session_id {
            return Err(error(
                "invalid_session",
                "A session cannot be its own parent; leave the parent out for a top-level session",
            ));
        }
    }
    if let Some(existing) = state.sessions.get(&session.session_id)
        && existing.workspace_id != session.workspace_id
    {
        return Err(error(
            "session_conflict",
            "Session ID is already bound to a different workspace; use a new session ID",
        ));
    }
    Ok(())
}

pub(crate) fn validate_session_liveness(
    state: &State,
    liveness: &SessionLiveness,
) -> Result<(), CoreError> {
    known_session(state, &liveness.session_id)?;
    if liveness.process.pid == 0 {
        return Err(error(
            "invalid_process_identity",
            "Process identity requires a positive PID and an OS boot/start token",
        ));
    }
    validate_id(&liveness.process.start_token, "process start token")?;
    if liveness.generation != state.generation {
        return Err(error(
            "invalid_event",
            "Liveness observations must use the current daemon generation",
        ));
    }
    if let Some(current) = state.session_liveness.get(&liveness.session_id)
        && current.generation == state.generation
    {
        if current.process != liveness.process {
            return Err(error(
                "process_identity_mismatch",
                "This session already names a different process incarnation in this generation; do not reuse its process identity",
            ));
        }
        if liveness.observed_at_ms < current.observed_at_ms {
            return Err(error(
                "stale_liveness_observation",
                "A liveness observation cannot replace newer evidence in the same generation",
            ));
        }
    }
    Ok(())
}

pub(crate) fn known_session<'a>(
    state: &'a State,
    session_id: &str,
) -> Result<&'a Session, CoreError> {
    validate_id(session_id, "session ID")?;
    state.sessions.get(session_id).ok_or_else(|| {
        error(
            "session_not_found",
            "Session is not registered; attach it to a workspace first",
        )
    })
}

pub(crate) fn current_session<'a>(
    state: &'a State,
    session_id: &str,
) -> Result<&'a Session, CoreError> {
    validate_id(session_id, "session ID")?;
    let session = state.sessions.get(session_id).ok_or_else(|| {
        error(
            "session_not_found",
            "Session is not registered; attach it to a workspace first",
        )
    })?;
    if session.generation != state.generation {
        return Err(error(
            "stale_session",
            "Session belongs to a previous daemon generation; register it again to reattach",
        ));
    }
    Ok(session)
}

pub(crate) fn error(code: &str, message: &str) -> CoreError {
    CoreError {
        code: code.into(),
        message: message.into(),
    }
}
