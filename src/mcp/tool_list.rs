//! Which tools the server lists, and whether the host's hooks report.

use super::{HOOKED_HOSTS, TOOL_NAMES};

/// The tools `--tools hooked` leaves out of the list while the host's hooks
/// report: the hooks call them for the agent (roadmap I11, ADR 0018). Each
/// still answers when called by name.
pub const HIDDEN_WHEN_HOOKED: &[&str] = &[
    "langspan_admit",
    "langspan_complete",
    "langspan_session_register",
    "langspan_workspace_register",
];

/// How long without a hook event from this host before the whole list is
/// shown again.
pub const HOOK_QUIET_MS: u64 = 10 * 60 * 1000;

/// Which tools `langspan mcp` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolList {
    /// Every tool, always. The default.
    All,
    /// Every tool except [`HIDDEN_WHEN_HOOKED`] while `host`'s hooks report
    /// for the workspace the server was started in.
    Hooked { host: String },
}

impl ToolList {
    /// The list `--tools` and `--host` ask for, or why they cannot be used.
    pub fn parse(tools: Option<&str>, host: Option<&str>) -> Result<Self, String> {
        match (tools.unwrap_or("all"), host) {
            ("all", None) => Ok(Self::All),
            ("all", Some(_)) => Err("--host is valid only with --tools hooked".into()),
            ("hooked", Some(host)) if HOOKED_HOSTS.contains(&host) => Ok(Self::Hooked {
                host: host.to_owned(),
            }),
            ("hooked", Some(host)) => Err(format!(
                "--tools hooked is for claude-code and codex, whose hooks report reliably; not {host:?}"
            )),
            ("hooked", None) => Err(
                "--tools hooked needs --host claude-code or --host codex: the list is shortened only while that host's hooks report"
                    .into(),
            ),
            (other, _) => Err(format!("--tools takes all or hooked, not {other:?}")),
        }
    }
}

/// Whether `host`'s hooks have reported, within `quiet_ms` of `now_ms`, for
/// the workspace that holds `cwd`, in the daemon's current generation. The
/// workspace is the registered one whose root is the deepest ancestor of (or
/// equal to) `cwd`. Any hook run counts: an edit observed after a tool call,
/// a prompt, or a recorded hook run (`SessionSeen`) from a session that only
/// reads or runs commands. Sessions are told apart by
/// host through the prefix of their ID (`claude-code.`, `codex.`). Anything
/// not known answers `false`, so every tool is listed.
pub fn hooks_reporting(
    state: &langspan_core::State,
    host: &str,
    cwd: &std::path::Path,
    now_ms: u64,
    quiet_ms: u64,
) -> bool {
    let deepest = state
        .workspaces
        .values()
        .map(|workspace| std::path::Path::new(&workspace.root))
        .filter(|root| cwd.starts_with(root))
        .map(|root| root.components().count())
        .max();
    let Some(depth) = deepest else {
        return false;
    };
    let here: Vec<&str> = state
        .workspaces
        .values()
        .filter(|workspace| {
            let root = std::path::Path::new(&workspace.root);
            cwd.starts_with(root) && root.components().count() == depth
        })
        .map(|workspace| workspace.workspace_id.as_str())
        .collect();
    state.sessions.values().any(|session| {
        session.generation == state.generation
            && here.contains(&session.workspace_id.as_str())
            && session
                .session_id
                .split_once('.')
                .is_some_and(|(prefix, _)| prefix == host)
            && state
                .activity
                .get(&session.session_id)
                .is_some_and(|activity| {
                    activity.generation == state.generation
                        && activity.last_seen_ms() > 0
                        && now_ms.saturating_sub(activity.last_seen_ms()) < quiet_ms
                })
    })
}

/// How the tools a server lists differ from [`TOOL_NAMES`], naming each tool:
/// `Ok` when they are the same set, otherwise an error saying which tools are
/// missing from the server, which are not in the list, and whether the list
/// itself is out of order or repeats a name.
pub fn compare_tool_names<'a>(served: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    use std::collections::BTreeSet;
    let mut problems = Vec::new();
    if let Some(pair) = TOOL_NAMES.windows(2).find(|pair| pair[0] >= pair[1]) {
        problems.push(format!(
            "TOOL_NAMES is not in alphabetical order or repeats a name at {:?} then {:?}",
            pair[0], pair[1]
        ));
    }
    let expected: BTreeSet<&str> = TOOL_NAMES.iter().copied().collect();
    let mut served_set = BTreeSet::new();
    let mut repeated = BTreeSet::new();
    for name in served {
        if !served_set.insert(name) {
            repeated.insert(name);
        }
    }
    let missing: Vec<_> = expected.difference(&served_set).collect();
    let unexpected: Vec<_> = served_set.difference(&expected).collect();
    if !missing.is_empty() {
        problems.push(format!("listed in TOOL_NAMES but not served: {missing:?}"));
    }
    if !unexpected.is_empty() {
        problems.push(format!(
            "served but not listed in TOOL_NAMES (src/mcp.rs): {unexpected:?}"
        ));
    }
    if !repeated.is_empty() {
        problems.push(format!("served more than once: {repeated:?}"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}
