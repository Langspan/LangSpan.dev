//! Admission: leases, regions, stale source, conflicts and refusals. Their
//! record types and rules, and the `decide_checked` and `apply` arms of
//! their commands and events, which call these functions. Coordination
//! policies are in `policy.rs`.

// The arm functions take what the arm's pattern bound, as bound (`&String`,
// `&Vec<_>`), so that each body moved out of the match unchanged.
#![allow(clippy::ptr_arg)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    Command, CoreError, Decision, Event, MAX_KNOWN, Outcome, PresenceKind, REFUSAL_PATHS,
    RefusalKind, Session, State, ToolIdentity, activity_mut, current_session, error, is_hex_digest,
    known_session, known_versions, last_peer_edit, lines, look, normalized_files, normalized_paths,
    policy, presence_setting, recent, status_line, validate_batch, validate_canonical_paths,
    validate_id, validate_known_workspace, validate_tool_identity, wrote_since_boundary,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub lease_id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub paths: Vec<String>,
    pub generation: u64,
    pub expires_at_ms: u64,
    pub status: LeaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ToolIdentity>,
    /// Admitted paths whose source version could not be compared with what
    /// the session last read, because no read of them was recorded. The
    /// admission is not evidence that the session's view of them is current.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_unknown: Vec<String>,
    /// Where in each file the edit was located. A path with no entry is held
    /// whole, which is also how a lease recorded before regions replays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<PathRegions>,
}

/// Part of one file an edit was located in, by the hook, from the file as it
/// was on disk. Lines are one-based and inclusive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Region {
    /// A definition found by the source parser. `key` is its name and the
    /// names of the definitions enclosing it, outermost first. `digest` is
    /// the SHA-256 of the definition's text with line endings and trailing
    /// whitespace normalized: the symbol's own version. The lines say where it
    /// was, for people; admission compares keys, not lines.
    Symbol {
        key: Vec<String>,
        digest: String,
        start_line: u32,
        end_line: u32,
    },
    /// A line range, for a file whose language is not parsed. Comparable only
    /// with ranges located in the same version of the file.
    Lines { start_line: u32, end_line: u32 },
}

/// The regions of one admitted path and the version of the file (`digest`,
/// SHA-256 of its bytes) they were located in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathRegions {
    pub path: String,
    pub digest: String,
    pub regions: Vec<Region>,
}

/// One-based, inclusive.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    pub start_line: u32,
    pub end_line: u32,
}

/// Lines of `path` that session `session_id` wrote `age_ms` ago and that an
/// edit would change, numbered in the file as it is now; `lines` is empty
/// when they could not be placed in it. The hook finds them in the line
/// journal ([ADR 0028](../../../docs/adr/0028-line-ownership.md)).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WrittenLines {
    pub path: String,
    pub session_id: String,
    pub lines: Vec<LineRange>,
    pub age_ms: u64,
}

/// At most this many written-lines entries in one admission.
pub const MAX_WRITTEN: usize = 64;
/// Line ranges per entry, at most.
pub const MAX_WRITTEN_RANGES: usize = 32;
/// Another session's lines count this long after it wrote them.
pub const LINE_HORIZON_MS: u64 = 30 * 60 * 1000;

/// At most this many regions in one admission, over all its paths.
pub const MAX_REGIONS: usize = 64;
/// No region line beyond this: far past the line count of a 1 MiB file.
pub const MAX_REGION_LINE: u32 = 2_000_000;
/// A symbol key names at most this many nested definitions.
pub const MAX_KEY_DEPTH: usize = 8;
const MAX_KEY_NAME_BYTES: usize = 128;

/// A file version as an adapter saw it on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservedFile {
    pub path: String,
    pub state: FileState,
}

/// How a session came to know a file version.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceOrigin {
    /// The host reported that the session read the file.
    Read,
    /// The host reported that the session's own write succeeded.
    Write,
    /// The session was refused an edit and told the file had changed. It has
    /// been informed of this version; it has not necessarily read it.
    Refused,
    /// The session was about to edit a file it had no recorded version of,
    /// on a host that reports no reads (Codex). This is the file as it was on
    /// disk at that moment, so later edits are compared with it. The session
    /// has not necessarily read it ([ADR 0027](../../../docs/adr/0027-turn-and-batch-events.md)).
    Snapshot,
}

/// The version of a file a session last read or wrote. `at_ms` is zero for a
/// version carried over from an earlier daemon generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnownVersion {
    pub path: String,
    pub state: FileState,
    pub at_ms: u64,
}

/// A file whose version on disk differs from the one the session last read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleFile {
    pub path: String,
    pub read: FileState,
    pub current: FileState,
    /// The other session in this workspace that most recently reported
    /// editing the path, when there is one. A correlation, not proof.
    pub changed_by: Option<String>,
}

/// A file the asking session read that a peer has since reported editing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleRead {
    pub path: String,
    pub session_id: String,
    pub age_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    Active,
    Completed,
    Expired,
    Invalidated,
}

/// What the host reported about a tool call after it ran.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResult {
    Succeeded,
    Failed,
    Interrupted,
}

/// How an observation relates to the admission it names, judged when the
/// observation is recorded. `Expired` and `Invalidated` are late observations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseRelation {
    Current,
    Completed,
    Expired,
    Invalidated,
    NotAdmitted,
    /// The reported paths include a resource that the named admission did not
    /// cover. This classification takes precedence over the lease's age.
    ScopeMismatch,
    /// The reported tool identity does not match the named admission. A legacy
    /// observation cannot bypass the input check on a typed admission.
    InputMismatch,
}

/// Evidence of a reported tool result. It is never authority: recording it does
/// not grant, renew, complete or reactivate any admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub observation_id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub operation_id: String,
    pub source: String,
    pub tool: String,
    pub paths: Vec<String>,
    pub result: ToolResult,
    pub lease_id: Option<String>,
    pub lease_relation: LeaseRelation,
    pub concurrent_holders: Vec<Conflict>,
    pub observed_at_ms: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    #[default]
    Regular,
    Symlink,
}

impl FileKind {
    fn is_regular(&self) -> bool {
        *self == Self::Regular
    }
}

/// A successfully observed file state. Read failures are not `Missing`; adapters
/// report them as incomplete scans without inventing a deletion observation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileState {
    Missing,
    Present {
        digest: String,
        #[serde(default, skip_serializing_if = "FileKind::is_regular")]
        kind: FileKind,
    },
}

// As with Command::Status, use an empty struct for strict unit-variant decoding.
#[derive(Deserialize)]
#[serde(
    remote = "FileState",
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum FileStateWire {
    Missing {},
    Present {
        digest: String,
        #[serde(default)]
        kind: FileKind,
    },
}

impl<'de> Deserialize<'de> for FileState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        FileStateWire::deserialize(deserializer)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    pub lease_id: String,
    pub session_id: String,
    pub paths: Vec<String>,
    /// The holder's regions in the conflicting paths it holds by region. A
    /// conflicting path missing here is held whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<PathRegions>,
}

/// Supplied files whose version differs from the one the session last read
/// or wrote. A file the session has no recorded version of is not stale: it
/// is unknown, and reported as such on the admission.
fn stale_files(state: &State, session: &Session, base: &[ObservedFile]) -> Vec<StaleFile> {
    let known = known_versions(state, &session.session_id);
    base.iter()
        .filter_map(|file| {
            let version = known.iter().find(|version| version.path == file.path)?;
            (version.state != file.state).then(|| StaleFile {
                path: file.path.clone(),
                read: version.state.clone(),
                current: file.state.clone(),
                // A peer whose own last version of the file is not what is
                // on disk now did not make it so: after a checkout, reset or
                // any other unreported change, it is not named.
                changed_by: last_peer_edit(
                    state,
                    &session.session_id,
                    &session.workspace_id,
                    &file.path,
                    version.at_ms,
                )
                .filter(|(peer, _)| {
                    known_versions(state, peer)
                        .iter()
                        .find(|known| known.path == file.path)
                        .is_none_or(|known| known.state == file.state)
                })
                .map(|(id, _)| id.to_owned()),
            })
        })
        .collect()
}

pub(crate) struct ObservationInput<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) operation_id: &'a str,
    pub(crate) source: &'a str,
    pub(crate) tool: &'a str,
    pub(crate) paths: &'a [String],
    pub(crate) result: ToolResult,
    pub(crate) lease_id: Option<&'a str>,
    pub(crate) operation: Option<&'a ToolIdentity>,
}

pub(crate) fn decide_observation(
    state: &State,
    input: ObservationInput<'_>,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    // A stale session may still report what happened before a restart.
    let session = known_session(state, input.session_id)?;
    if let Some(operation) = input.operation {
        validate_tool_identity(operation)?;
    } else {
        validate_id(input.operation_id, "operation ID")?;
        validate_id(input.source, "observation source")?;
        validate_id(input.tool, "tool name")?;
    }
    let paths = normalized_paths(input.paths)?;
    let mut scope: BTreeSet<String> = paths.iter().cloned().collect();
    let lease_relation = match input.lease_id {
        None => LeaseRelation::NotAdmitted,
        Some(id) => {
            validate_id(id, "lease ID")?;
            match state.leases.get(id) {
                None => LeaseRelation::NotAdmitted,
                Some(lease) => {
                    if lease.session_id != input.session_id {
                        return Err(error(
                            "not_holder",
                            "An observation can name only its own session's admission",
                        ));
                    }
                    scope.extend(lease.paths.iter().cloned());
                    if lease.operation.as_ref() != input.operation {
                        LeaseRelation::InputMismatch
                    } else if paths
                        .iter()
                        .any(|path| lease.paths.binary_search(path).is_err())
                    {
                        LeaseRelation::ScopeMismatch
                    } else {
                        lease_relation(state, lease, now_ms)
                    }
                }
            }
        }
    };
    let scope: Vec<String> = scope.into_iter().collect();
    // Judged as the named admission was: by its regions, if it had any.
    let regions = input
        .lease_id
        .and_then(|id| state.leases.get(id))
        .map(|lease| lease.regions.as_slice())
        .unwrap_or_default();
    let concurrent_holders = conflicts(state, &session.workspace_id, &scope, regions, now_ms)
        .into_iter()
        .filter(|holder| input.lease_id != Some(holder.lease_id.as_str()))
        .collect();
    let observation = Observation {
        observation_id: request_id.into(),
        session_id: input.session_id.into(),
        workspace_id: session.workspace_id.clone(),
        operation_id: input.operation_id.into(),
        source: input.source.into(),
        tool: input.tool.into(),
        paths,
        result: input.result,
        lease_id: input.lease_id.map(str::to_owned),
        lease_relation,
        concurrent_holders,
        observed_at_ms: now_ms,
        generation: state.generation,
    };
    Ok(Decision {
        events: vec![Event::OperationObserved {
            observation: observation.clone(),
        }],
        outcome: Outcome::Observed { observation },
    })
}

/// The regions of `regions` that name the version `base` gives for their
/// path. Regions are trusted only for the version the hook saw on disk when
/// it asked; any other path is held whole.
fn trusted_regions<'a>(
    regions: &[PathRegions],
    paths: &[String],
    base: impl Iterator<Item = &'a ObservedFile> + Clone,
) -> Result<Vec<PathRegions>, CoreError> {
    Ok(normalized_regions(regions, paths)?
        .into_iter()
        .filter(|located| {
            base.clone().any(|file| {
                file.path == located.path
                    && matches!(&file.state, FileState::Present { digest, .. } if *digest == located.digest)
            })
        })
        .collect())
}

/// The current admissions an admission of `paths` by `session_id` would
/// meet. A host's tool call is never held back by an earlier tool call of
/// the same session.
fn holders_of(
    state: &State,
    session_id: &str,
    workspace_id: &str,
    paths: &[String],
    regions: &[PathRegions],
    tool_call: bool,
    now_ms: u64,
) -> Vec<Conflict> {
    conflicts(state, workspace_id, paths, regions, now_ms)
        .into_iter()
        .filter(|holder| {
            !(tool_call
                && holder.session_id == session_id
                && state
                    .leases
                    .get(&holder.lease_id)
                    .is_some_and(|lease| lease.operation.is_some()))
        })
        .collect()
}

fn conflicts(
    state: &State,
    workspace_id: &str,
    paths: &[String],
    regions: &[PathRegions],
    now_ms: u64,
) -> Vec<Conflict> {
    let located = |list: &'_ [PathRegions], path: &str| -> Option<PathRegions> {
        list.iter().find(|entry| entry.path == path).cloned()
    };
    state
        .leases
        .values()
        .filter_map(|lease| {
            if lease.status != LeaseStatus::Active
                || lease.generation != state.generation
                || lease.expires_at_ms <= now_ms
                || lease.workspace_id != workspace_id
            {
                return None;
            }
            let intersection: Vec<_> = paths
                .iter()
                .filter(|path| lease.paths.binary_search(path).is_ok())
                .filter(|path| {
                    !separate(
                        located(regions, path).as_ref(),
                        located(&lease.regions, path).as_ref(),
                    )
                })
                .cloned()
                .collect();
            if intersection.is_empty() {
                None
            } else {
                Some(Conflict {
                    lease_id: lease.lease_id.clone(),
                    session_id: lease.session_id.clone(),
                    regions: intersection
                        .iter()
                        .filter_map(|path| located(&lease.regions, path))
                        .collect(),
                    paths: intersection,
                })
            }
        })
        .collect()
}

/// True only when two edits of one path cannot touch the same code: both were
/// located, and either they name different symbols, neither inside the other,
/// or they name disjoint line ranges of the same version of the file. Every
/// other case, including a whole-file edit on either side and a mix of
/// symbols and lines, is a conflict as before regions existed.
fn separate(ours: Option<&PathRegions>, theirs: Option<&PathRegions>) -> bool {
    let (Some(ours), Some(theirs)) = (ours, theirs) else {
        return false;
    };
    ours.regions.iter().all(|a| {
        theirs.regions.iter().all(|b| match (a, b) {
            (Region::Symbol { key: a, .. }, Region::Symbol { key: b, .. }) => {
                !(a.starts_with(b) || b.starts_with(a))
            }
            (
                Region::Lines {
                    start_line: a_start,
                    end_line: a_end,
                },
                Region::Lines {
                    start_line: b_start,
                    end_line: b_end,
                },
            ) => ours.digest == theirs.digest && (a_end < b_start || b_end < a_start),
            _ => false,
        })
    })
}

/// Validate regions against the admitted (normalized) paths and return them
/// with each path's regions sorted and deduplicated.
fn normalized_regions(
    regions: &[PathRegions],
    paths: &[String],
) -> Result<Vec<PathRegions>, CoreError> {
    let invalid = |message: &str| Err(error("invalid_regions", message));
    let mut seen = BTreeSet::new();
    let mut total = 0;
    let mut normalized = Vec::with_capacity(regions.len());
    for located in regions {
        if paths.binary_search(&located.path).is_err() {
            return invalid("Regions name a path this admission does not request");
        }
        if !seen.insert(located.path.as_str()) {
            return invalid("Each path's regions may be given once");
        }
        if !is_hex_digest(&located.digest, 64) {
            return invalid("A file version digest must contain exactly 64 hexadecimal characters");
        }
        if located.regions.is_empty() {
            return invalid("Give at least one region, or leave the path out to hold it whole");
        }
        let symbols = located
            .regions
            .iter()
            .filter(|region| matches!(region, Region::Symbol { .. }))
            .count();
        if symbols != 0 && symbols != located.regions.len() {
            return invalid("One path's regions are all symbols or all line ranges");
        }
        for region in &located.regions {
            let (start_line, end_line) = match region {
                Region::Symbol {
                    key,
                    digest,
                    start_line,
                    end_line,
                } => {
                    if key.is_empty()
                        || key.len() > MAX_KEY_DEPTH
                        || key.iter().any(|name| {
                            name.is_empty()
                                || name.len() > MAX_KEY_NAME_BYTES
                                || name.chars().any(char::is_control)
                        })
                    {
                        return invalid(
                            "A symbol key is 1 to 8 names of 1 to 128 bytes without control characters",
                        );
                    }
                    if !is_hex_digest(digest, 64) {
                        return invalid(
                            "A symbol digest must contain exactly 64 hexadecimal characters",
                        );
                    }
                    (*start_line, *end_line)
                }
                Region::Lines {
                    start_line,
                    end_line,
                } => (*start_line, *end_line),
            };
            if start_line == 0 || start_line > end_line || end_line > MAX_REGION_LINE {
                return invalid("Region lines are one-based, ordered and at most 2000000");
            }
        }
        let mut sorted = located.regions.clone();
        sorted.sort();
        sorted.dedup();
        total += sorted.len();
        if total > MAX_REGIONS {
            return invalid(
                "An admission can name at most 64 distinct regions; leave paths out to hold them whole",
            );
        }
        normalized.push(PathRegions {
            path: located.path.clone(),
            digest: located.digest.clone(),
            regions: sorted,
        });
    }
    normalized.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(normalized)
}

pub(crate) fn lease_relation(state: &State, lease: &Lease, now_ms: u64) -> LeaseRelation {
    match lease.status {
        LeaseStatus::Completed => LeaseRelation::Completed,
        LeaseStatus::Invalidated => LeaseRelation::Invalidated,
        _ if lease.generation != state.generation => LeaseRelation::Invalidated,
        LeaseStatus::Expired => LeaseRelation::Expired,
        LeaseStatus::Active if lease.expires_at_ms <= now_ms => LeaseRelation::Expired,
        LeaseStatus::Active => LeaseRelation::Current,
    }
}

pub(crate) fn decide_admit(
    state: &State,
    command: &Command,
    session_id: &String,
    paths: &Vec<String>,
    ttl_ms: &u64,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let (operation, base, regions, overwrites) = if let Command::AdmitTool {
        operation,
        base,
        regions,
        overwrites,
        ..
    } = command
    {
        validate_tool_identity(operation)?;
        (
            Some(operation.clone()),
            Some(normalized_files(base)?),
            regions.as_slice(),
            overwrites.as_slice(),
        )
    } else {
        (None, None, &[][..], &[][..])
    };
    let session = current_session(state, session_id)?;
    let paths = normalized_paths(paths)?;
    lines::validate_overwrites(overwrites, &paths)?;
    let regions = trusted_regions(regions, &paths, base.iter().flatten())?;
    if !(1..=60_000).contains(ttl_ms) {
        return Err(error(
            "invalid_ttl",
            "Lease TTL must be between 1 and 60000 milliseconds",
        ));
    }
    let expires_at_ms = now_ms.checked_add(*ttl_ms).ok_or_else(|| {
        error(
            "invalid_deadline",
            "Lease deadline overflows the supplied clock; use a valid monotonic timestamp",
        )
    })?;
    if state.leases.contains_key(request_id) {
        return Err(error(
            "lease_id_reused",
            "This lease ID already exists; replay the original request through storage or use a new request ID",
        ));
    }
    // A host's tool call is never held back by an earlier tool call
    // of the same session. That earlier call may still sit at a
    // permission prompt the owner refused, which sends no post-tool
    // event; denying the session its own file until the admission
    // expires helps nobody. A manual admission stays exclusive, as
    // ADR 0001 defines.
    let tool_call = operation.is_some();
    let holders = holders_of(
        state,
        session_id,
        &session.workspace_id,
        &paths,
        &regions,
        tool_call,
        now_ms,
    );
    // Each refusal is one event, stored with the refusal's receipt:
    // the same request again is answered from it and adds none.
    let refused = |kind, peer: Option<&String>| Event::WriteRefused {
        session_id: session_id.clone(),
        workspace_id: session.workspace_id.clone(),
        kind,
        paths: paths.iter().take(REFUSAL_PATHS).cloned().collect(),
        peer_session_id: peer.cloned(),
        input_digest: operation
            .as_ref()
            .map(|operation| operation.input_digest.clone()),
        at_ms: now_ms,
        generation: state.generation,
    };
    if !holders.is_empty() {
        // A hook's tool call; a manual `admit` refused is not a write.
        // Where the workspace waits for a holder, the hook records
        // the refusal when its wait ends without an admission.
        let waits = policy::conflict_wait_ms(state, &session.workspace_id) > 0;
        return Ok(Decision {
            events: (tool_call && !waits)
                .then(|| {
                    refused(
                        RefusalKind::Conflict,
                        holders.first().map(|holder| &holder.session_id),
                    )
                })
                .into_iter()
                .collect(),
            outcome: Outcome::Conflict { holders },
        });
    }
    let mut base_unknown = Vec::new();
    if let Some(base) = &base {
        if let Some(extra) = base
            .iter()
            .find(|file| paths.binary_search(&file.path).is_err())
        {
            return Err(error(
                "invalid_base",
                &format!(
                    "Base version names {:?}, which this admission does not request",
                    extra.path
                ),
            ));
        }
        let stale = stale_files(state, session, base);
        if !stale.is_empty() {
            // One refusal per change. The session is told; what it was
            // told about becomes its known version, so a host with no
            // reported read, or a file that no longer exists to be
            // read, cannot be refused for the same change forever.
            return Ok(Decision {
                events: vec![
                    Event::SourceObserved {
                        session_id: session_id.clone(),
                        files: stale
                            .iter()
                            .map(|file| ObservedFile {
                                path: file.path.clone(),
                                state: file.current.clone(),
                            })
                            .collect(),
                        origin: SourceOrigin::Refused,
                        at_ms: now_ms,
                        generation: state.generation,
                    },
                    refused(
                        RefusalKind::StaleBase,
                        stale.iter().find_map(|file| file.changed_by.as_ref()),
                    ),
                ],
                outcome: Outcome::StaleBase { stale },
            });
        }
        let unread = lines::unread_lines(state, session, base, overwrites);
        if !unread.is_empty() {
            // Refused once, like a stale base: what the session was
            // told about becomes its known version, so the retry, or
            // any edit after it reads the file, is admitted.
            let told: Vec<ObservedFile> = base
                .iter()
                .filter(|file| unread.iter().any(|lines| lines.path == file.path))
                .cloned()
                .collect();
            return Ok(Decision {
                events: vec![
                    Event::SourceObserved {
                        session_id: session_id.clone(),
                        files: told,
                        origin: SourceOrigin::Refused,
                        at_ms: now_ms,
                        generation: state.generation,
                    },
                    refused(
                        RefusalKind::UnreadLines,
                        unread.first().map(|lines| &lines.session_id),
                    ),
                ],
                outcome: Outcome::UnreadLines { lines: unread },
            });
        }
        let known = known_versions(state, session_id);
        base_unknown = paths
            .iter()
            .filter(|path| {
                let supplied = base.iter().find(|file| &file.path == *path);
                // A file that does not exist yet has no earlier version to miss.
                !matches!(supplied.map(|file| &file.state), Some(FileState::Missing))
                    && !(supplied.is_some() && known.iter().any(|version| &version.path == *path))
            })
            .cloned()
            .collect();
    }
    let lease = Lease {
        lease_id: request_id.into(),
        session_id: session_id.clone(),
        workspace_id: session.workspace_id.clone(),
        paths,
        generation: state.generation,
        expires_at_ms,
        status: LeaseStatus::Active,
        operation,
        base_unknown,
        regions,
    };
    Ok(Decision {
        events: vec![Event::LeaseGranted {
            lease: lease.clone(),
        }],
        outcome: Outcome::Admitted { lease },
    })
}

pub(crate) fn decide_complete(
    state: &State,
    session_id: &String,
    lease_id: &String,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    validate_id(lease_id, "lease ID")?;
    let lease = state.leases.get(lease_id).ok_or_else(|| {
        error(
            "lease_not_found",
            "Lease does not exist; inspect status for the active lease ID",
        )
    })?;
    if lease.session_id != *session_id {
        return Err(error(
            "not_holder",
            "Only the session holding this lease can complete it",
        ));
    }
    if lease.generation != state.generation || lease.status == LeaseStatus::Invalidated {
        return Err(error(
            "stale_lease",
            "The lease belongs to a previous daemon generation; request a new admission",
        ));
    }
    if lease.status == LeaseStatus::Expired
        || (lease.status == LeaseStatus::Active && lease.expires_at_ms <= now_ms)
    {
        return Err(error(
            "lease_expired",
            "The lease has expired; request a new admission before continuing work",
        ));
    }
    if lease.status != LeaseStatus::Active {
        return Err(error(
            "lease_not_active",
            "The lease is no longer active; replay completion through storage or request a new admission",
        ));
    }
    Ok(Decision {
        events: vec![Event::LeaseCompleted {
            lease_id: lease_id.clone(),
        }],
        outcome: Outcome::Completed {
            lease_id: lease_id.clone(),
        },
    })
}

pub(crate) fn decide_observe_tool_batch(
    state: &State,
    session_id: &String,
    operation_ids: &Vec<String>,
    calls: &u32,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    validate_batch(operation_ids, *calls)?;
    Ok(Decision {
        events: vec![Event::ToolBatchObserved {
            session_id: session_id.clone(),
            operation_ids: operation_ids.clone(),
            calls: *calls,
            at_ms: now_ms,
            generation: state.generation,
        }],
        outcome: Outcome::ToolBatchObserved {
            session_id: session_id.clone(),
            calls: *calls,
            wrote: wrote_since_boundary(state, session_id),
        },
    })
}

pub(crate) fn decide_observe_source(
    state: &State,
    session_id: &String,
    files: &Vec<ObservedFile>,
    origin: &SourceOrigin,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    current_session(state, session_id)?;
    let files = normalized_files(files)?;
    if files.is_empty() {
        return Err(error(
            "invalid_path",
            "Name at least one observed file version",
        ));
    }
    let count = u32::try_from(files.len()).unwrap_or(u32::MAX);
    Ok(Decision {
        events: vec![Event::SourceObserved {
            session_id: session_id.clone(),
            files,
            origin: *origin,
            at_ms: now_ms,
            generation: state.generation,
        }],
        outcome: Outcome::SourceObserved {
            session_id: session_id.clone(),
            files: count,
        },
    })
}

pub(crate) fn decide_policy(state: &State, workspace_id: &String) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::Policy {
            workspace_id: workspace_id.clone(),
            rows: policy::table(state, workspace_id),
        },
    })
}

pub(crate) fn decide_holders(
    state: &State,
    session_id: &String,
    paths: &Vec<String>,
    base: &Vec<ObservedFile>,
    regions: &Vec<PathRegions>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let session = current_session(state, session_id)?;
    let paths = normalized_paths(paths)?;
    let base = normalized_files(base)?;
    let regions = trusted_regions(regions, &paths, base.iter())?;
    Ok(Decision {
        events: Vec::new(),
        outcome: Outcome::Holders {
            holders: holders_of(
                state,
                session_id,
                &session.workspace_id,
                &paths,
                &regions,
                true,
                now_ms,
            ),
        },
    })
}

pub(crate) fn decide_record_refusal(
    state: &State,
    session_id: &String,
    kind: &RefusalKind,
    paths: &Vec<String>,
    peer_session_id: &Option<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    if *kind != RefusalKind::CommitHeld {
        return Err(error(
            "invalid_refusal",
            "Only a held commit is reported; the coordinator records the other refusals itself",
        ));
    }
    let session = known_session(state, session_id)?;
    let paths = normalized_paths(paths)?;
    if let Some(peer) = peer_session_id {
        known_session(state, peer)?;
    }
    Ok(Decision {
        events: vec![Event::WriteRefused {
            session_id: session_id.clone(),
            workspace_id: session.workspace_id.clone(),
            kind: *kind,
            paths: paths.into_iter().take(REFUSAL_PATHS).collect(),
            peer_session_id: peer_session_id.clone(),
            input_digest: None,
            at_ms: now_ms,
            generation: state.generation,
        }],
        outcome: Outcome::RefusalRecorded {
            session_id: session_id.clone(),
        },
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_write_refused(
    state: &mut State,
    session_id: &String,
    workspace_id: &String,
    kind: &RefusalKind,
    paths: &Vec<String>,
    peer_session_id: &Option<String>,
    input_digest: &Option<String>,
    at_ms: &u64,
    generation: &u64,
) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    validate_known_workspace(state, workspace_id)?;
    normalized_paths(paths)?;
    if paths.len() > REFUSAL_PATHS
        || peer_session_id
            .as_ref()
            .is_some_and(|peer| peer.len() > 200)
        || input_digest
            .as_ref()
            .is_some_and(|digest| !is_hex_digest(digest, 64))
    {
        return Err(error(
            "invalid_event",
            "A refusal keeps at most 8 paths, one peer session ID and a SHA-256 input digest",
        ));
    }
    status_line::remember(state, workspace_id, *at_ms, *generation);
    recent::remember_refusal(state, workspace_id, paths, *at_ms, *generation);
    policy::remember_refusal(state, session_id, *kind, paths, *at_ms, *generation);
    Ok(())
}

pub(crate) fn apply_lease_granted(state: &mut State, lease: &Lease) -> Result<(), CoreError> {
    validate_id(&lease.lease_id, "lease ID")?;
    if let Some(operation) = &lease.operation {
        validate_tool_identity(operation)?;
    }
    let session = current_session(state, &lease.session_id)?;
    if lease.workspace_id != session.workspace_id
        || lease.generation != state.generation
        || lease.status != LeaseStatus::Active
        || lease.expires_at_ms == 0
    {
        return Err(error(
            "invalid_event",
            "Granted lease must be active in its session's current workspace and generation with a positive deadline",
        ));
    }
    validate_canonical_paths(&lease.paths)?;
    if normalized_regions(&lease.regions, &lease.paths)? != lease.regions {
        return Err(error(
            "invalid_event",
            "Granted lease regions must be in their normalized order",
        ));
    }
    if state.leases.contains_key(&lease.lease_id) {
        return Err(error(
            "invalid_event",
            "A lease-granted event cannot reuse an existing lease ID",
        ));
    }
    state.leases.insert(lease.lease_id.clone(), lease.clone());
    Ok(())
}

pub(crate) fn apply_tool_batch_observed(
    state: &mut State,
    session_id: &String,
    operation_ids: &Vec<String>,
    calls: &u32,
    generation: &u64,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    validate_batch(operation_ids, *calls)?;
    if *generation != state.generation {
        return Err(error(
            "invalid_event",
            "A tool batch record must belong to the current daemon generation",
        ));
    }
    // Only the mark: a boundary does not bring an ended session back
    // or start an activity record.
    if let Some(activity) = state.activity.get_mut(session_id) {
        activity.wrote_since_boundary = false;
    }
    Ok(())
}

pub(crate) fn apply_source_observed(
    state: &mut State,
    session_id: &String,
    files: &Vec<ObservedFile>,
    origin: &SourceOrigin,
    at_ms: &u64,
    generation: &u64,
) -> Result<(), CoreError> {
    current_session(state, session_id)?;
    if *generation != state.generation || files.is_empty() || normalized_files(files)? != *files {
        return Err(error(
            "invalid_event",
            "A source observation names sorted, unique files in the current generation",
        ));
    }
    let activity = activity_mut(state, session_id);
    for file in files.iter().rev() {
        activity.known.retain(|version| version.path != file.path);
        activity.known.insert(
            0,
            KnownVersion {
                path: file.path.clone(),
                state: file.state.clone(),
                at_ms: *at_ms,
            },
        );
    }
    activity.known.truncate(MAX_KNOWN);
    // A reported read is also where the session is looking
    // (roadmap A13), unless its presence is off.
    if *origin == SourceOrigin::Read && presence_setting(state, session_id).shown {
        let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
        look(
            activity_mut(state, session_id),
            PresenceKind::Read,
            &paths,
            *at_ms,
        );
    }
    Ok(())
}

pub(crate) fn apply_lease_ended(
    state: &mut State,
    event: &Event,
    lease_id: &String,
) -> Result<(), CoreError> {
    validate_id(lease_id, "lease ID")?;
    let lease = state
        .leases
        .get_mut(lease_id)
        .ok_or_else(|| error("invalid_event", "Lease event references an unknown lease"))?;
    if lease.status != LeaseStatus::Active || lease.generation != state.generation {
        return Err(error(
            "invalid_event",
            "Only an active lease in the current generation can end",
        ));
    }
    lease.status = if matches!(event, Event::LeaseCompleted { .. }) {
        LeaseStatus::Completed
    } else {
        LeaseStatus::Expired
    };
    Ok(())
}
