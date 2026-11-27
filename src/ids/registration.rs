//! A workspace registration's lifetime, shared by the daemon and its workers.

/// One lifetime of a workspace registration: its ID, its root and its
/// revision, how many times the ID had been moved or removed (roadmap G6).
/// A `workspace move` and a `workspace remove` each end the lifetime, even
/// when the ID comes back to the same root (switched back, or registered
/// again). What a daemon worker learned during one lifetime (a scan, an
/// index pass, an automatic check) carries it, and the state owner records
/// it only while the workspace still has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub workspace_id: String,
    pub root: std::path::PathBuf,
    pub revision: u32,
}

impl Registration {
    /// The workspace's current registration, if it is registered.
    pub fn of(state: &langspan_core::State, workspace_id: &str) -> Option<Self> {
        let workspace = state.workspaces.get(workspace_id)?;
        Some(Self {
            workspace_id: workspace_id.to_owned(),
            root: workspace.root.clone().into(),
            revision: state
                .workspace_revisions()
                .get(workspace_id)
                .copied()
                .unwrap_or(0),
        })
    }

    /// Whether the workspace is still in this registration.
    pub fn is_current(&self, state: &langspan_core::State) -> bool {
        Self::of(state, &self.workspace_id).as_ref() == Some(self)
    }
}
