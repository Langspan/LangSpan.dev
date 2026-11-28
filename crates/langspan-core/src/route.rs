//! Where each command is answered (issue #371).
//!
//! The core decides most commands; the daemon answers the rest from its
//! stores, its source index or itself. The core's refusal, the store's
//! receipts and the daemon's dispatch all follow this one table. The match
//! below names every variant and has no wildcard, so a new `Command` does not
//! compile until it is given a route here.

use crate::{Command, CoreError, error};

/// Which part of the coordinator answers a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Decided by the core; its events and receipt are recorded.
    Decide,
    /// Decided by the core from current state; nothing is recorded.
    Read,
    /// Answered by the daemon's store from the event log; the core refuses it.
    Store,
    /// Answered by the daemon with its source index; the core refuses it.
    Index,
    /// Answered by the coordinator about itself; the core refuses it.
    Internal,
}

/// The core's refusal of a command routed to the daemon's stores or itself.
pub(crate) fn answered_elsewhere() -> CoreError {
    error(
        "storage_operation",
        "Event history, status pages, change projections, sent messages and index queries are answered by the daemon's stores",
    )
}

impl Command {
    /// The route of this command.
    pub fn route(&self) -> Route {
        match self {
            Command::Version => Route::Internal,
            Command::GraphSymbol { .. }
            | Command::GraphReferences { .. }
            | Command::GraphImpact { .. }
            | Command::GraphBlindSpots { .. }
            | Command::GraphPath { .. }
            | Command::GraphMap { .. }
            | Command::GraphContext { .. }
            | Command::GraphInflight { .. }
            | Command::GraphExport { .. }
            | Command::GraphImport { .. }
            | Command::CheckWrite { .. }
            | Command::CheckCalls { .. } => Route::Index,
            Command::Events { .. }
            | Command::Today { .. }
            | Command::Report { .. }
            | Command::Count { .. }
            | Command::History { .. }
            | Command::StatusRecords { .. }
            | Command::Changes { .. }
            | Command::RemoteSent { .. }
            | Command::Compact { .. }
            | Command::ShowMessage { .. }
            | Command::MessageStatus { .. } => Route::Store,
            // Without `apply`, a dry run that says what would end.
            Command::WorkspaceRemove { apply: false, .. } => Route::Read,
            Command::WorkspaceRemove { apply: true, .. } => Route::Decide,
            Command::Status
            | Command::Dashboard
            | Command::StatusLine { .. }
            | Command::RemoteOutbox { .. }
            | Command::CheckStatus { .. }
            | Command::FlakyTests { .. }
            | Command::LandingReadiness { .. }
            | Command::TurnQueue { .. }
            | Command::WorkspaceList { .. }
            | Command::Recap { .. }
            | Command::Trails { .. }
            | Command::Policy { .. }
            | Command::Holders { .. }
            | Command::Recall { .. } => Route::Read,
            Command::WorkspaceRegister { .. }
            | Command::WorkspaceRegisterVerified { .. }
            | Command::WorkspaceMove { .. }
            | Command::WorkspaceMoveVerified { .. }
            | Command::SessionRegister { .. }
            | Command::ObserveSessionLiveness { .. }
            | Command::SessionSeen { .. }
            | Command::RecordContext { .. }
            | Command::ForgetTold { .. }
            | Command::ConfirmTold { .. }
            | Command::IntentSet { .. }
            | Command::IntentEnd { .. }
            | Command::Admit { .. }
            | Command::AdmitTool { .. }
            | Command::ObserveSource { .. }
            | Command::RemoteEnable { .. }
            | Command::RemoteDisable { .. }
            | Command::RemotePublished { .. }
            | Command::ObserveRemote { .. }
            | Command::ForgetRemote { .. }
            | Command::RemoteWithdrawn { .. }
            | Command::RecordCheck { .. }
            | Command::ForgetFlakyTest { .. }
            | Command::AutoCheckEnable { .. }
            | Command::AutoCheckDisable { .. }
            | Command::RecordLanding { .. }
            | Command::TakeTurn { .. }
            | Command::ReleaseTurn { .. }
            | Command::LeaveTurnQueue { .. }
            | Command::ObserveInterface { .. }
            | Command::ObserveInterfaceDependents { .. }
            | Command::Remember { .. }
            | Command::ObserveNoteAnchors { .. }
            | Command::Forget { .. }
            | Command::SendMessage { .. }
            | Command::AckMessage { .. }
            | Command::Handoff { .. }
            | Command::RetractMessage { .. }
            | Command::ObserveScan { .. }
            | Command::ObserveCheckout { .. }
            | Command::ObserveCheckoutRun { .. }
            | Command::ObserveUncoordinated { .. }
            | Command::ObserveHosts { .. }
            | Command::RecordRefusal { .. }
            | Command::SetPolicy { .. }
            | Command::RecordWait { .. }
            | Command::AcknowledgeInterface { .. }
            | Command::BetweenSteps { .. }
            | Command::InterfaceShown { .. }
            | Command::OutlineRead { .. }
            | Command::Complete { .. }
            | Command::Observe { .. }
            | Command::ObserveTool { .. }
            | Command::ObserveDisk { .. }
            | Command::ObserveBranch { .. }
            | Command::ObserveWorkspaceGit { .. }
            | Command::SessionPrompt { .. }
            | Command::SetPromptTargets { .. }
            | Command::ObservePresence { .. }
            | Command::SetPresence { .. }
            | Command::SessionEnd { .. }
            | Command::SessionStop { .. }
            | Command::ObserveToolBatch { .. }
            | Command::Brief { .. }
            | Command::Adopt { .. }
            | Command::BriefPrepared { .. }
            | Command::RememberPrepared { .. }
            | Command::NoteAnchorsPrepared { .. } => Route::Decide,
        }
    }
}
