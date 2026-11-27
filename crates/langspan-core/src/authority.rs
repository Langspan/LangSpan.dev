//! The authority each command needs (issue #334,
//! [ADR 0039](../../../docs/adr/0039-caller-authority.md)).
//!
//! A coordination command records or reads coordination state and any
//! caller of the socket may send it. An owner grant makes the coordinator act
//! with the owner's own authority beyond that state: it runs a program, reads
//! or writes a file outside the state directory, sends data off the machine,
//! deletes history, or changes the admission policy every session meets.
//!
//! The match below names every variant and has no wildcard, so a new
//! `Command` does not compile until it is given a class here.

use crate::Command;

/// What a caller must hold to have a command carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// Any client of the owner-only socket.
    Coordination,
    /// The owner's own action, which a confined agent does not have.
    OwnerGrant,
}

/// The class of `command`.
pub fn authority(command: &Command) -> Authority {
    match command {
        // Runs the owner's program after a session's writes.
        Command::AutoCheckEnable { .. } => Authority::OwnerGrant,
        // Pushes this machine's work to a remote.
        Command::RemoteEnable { .. } => Authority::OwnerGrant,
        // Write and read a file at any absolute path.
        Command::GraphExport { .. } | Command::GraphImport { .. } => Authority::OwnerGrant,
        // Changes the policy every session's admission meets.
        Command::SetPolicy { .. } => Authority::OwnerGrant,
        // End a workspace's records, and delete events and receipts.
        Command::WorkspaceRemove { .. } | Command::Compact { .. } => Authority::OwnerGrant,
        Command::WorkspaceRegister { .. }
        | Command::WorkspaceRegisterVerified { .. }
        | Command::WorkspaceList { .. }
        | Command::WorkspaceMove { .. }
        | Command::WorkspaceMoveVerified { .. }
        | Command::SessionRegister { .. }
        | Command::ObserveSessionLiveness { .. }
        | Command::SessionSeen { .. }
        | Command::RecordContext { .. }
        | Command::ForgetTold { .. }
        | Command::IntentSet { .. }
        | Command::IntentEnd { .. }
        | Command::Admit { .. }
        | Command::AdmitTool { .. }
        | Command::ObserveSource { .. }
        | Command::RemoteDisable { .. }
        | Command::RemoteOutbox { .. }
        | Command::RemotePublished { .. }
        | Command::RemoteSent { .. }
        | Command::ObserveRemote { .. }
        | Command::ForgetRemote { .. }
        | Command::RemoteWithdrawn { .. }
        | Command::RecordCheck { .. }
        | Command::FlakyTests { .. }
        | Command::ForgetFlakyTest { .. }
        | Command::AutoCheckDisable { .. }
        | Command::CheckStatus { .. }
        | Command::LandingReadiness { .. }
        | Command::RecordLanding { .. }
        | Command::TakeTurn { .. }
        | Command::ReleaseTurn { .. }
        | Command::LeaveTurnQueue { .. }
        | Command::TurnQueue { .. }
        | Command::ObserveInterface { .. }
        | Command::ObserveInterfaceDependents { .. }
        | Command::Remember { .. }
        | Command::ObserveNoteAnchors { .. }
        | Command::Forget { .. }
        | Command::SendMessage { .. }
        | Command::AckMessage { .. }
        | Command::ShowMessage { .. }
        | Command::MessageStatus { .. }
        | Command::Handoff { .. }
        | Command::RetractMessage { .. }
        | Command::ObserveScan { .. }
        | Command::ObserveCheckout { .. }
        | Command::ObserveCheckoutRun { .. }
        | Command::ObserveUncoordinated { .. }
        | Command::ObserveHosts { .. }
        | Command::Recap { .. }
        | Command::Trails { .. }
        | Command::Recall { .. }
        | Command::History { .. }
        | Command::Today { .. }
        | Command::Report { .. }
        | Command::Count { .. }
        | Command::StatusLine { .. }
        | Command::RecordRefusal { .. }
        | Command::Policy { .. }
        | Command::Holders { .. }
        | Command::RecordWait { .. }
        | Command::GraphSymbol { .. }
        | Command::GraphReferences { .. }
        | Command::GraphImpact { .. }
        | Command::GraphBlindSpots { .. }
        | Command::GraphPath { .. }
        | Command::GraphMap { .. }
        | Command::GraphContext { .. }
        | Command::GraphInflight { .. }
        | Command::CheckWrite { .. }
        | Command::CheckCalls { .. }
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
        | Command::NoteAnchorsPrepared { .. }
        | Command::Status
        | Command::StatusRecords { .. }
        | Command::Dashboard
        | Command::Version
        | Command::Events { .. }
        | Command::Changes { .. } => Authority::Coordination,
    }
}

#[cfg(test)]
mod tests {
    use crate::{Authority, Command, authority};

    #[test]
    fn owner_grants_are_the_commands_that_act_beyond_coordination_state() {
        let compact = Command::Compact {
            keep_days: 30,
            apply: false,
        };
        let export = Command::GraphExport {
            workspace_id: "w".into(),
            path: "/tmp/x".into(),
        };
        assert_eq!(authority(&compact), Authority::OwnerGrant);
        assert_eq!(authority(&export), Authority::OwnerGrant);
        assert_eq!(authority(&Command::Status), Authority::Coordination);
        let disable = Command::AutoCheckDisable {
            workspace_id: "w".into(),
        };
        assert_eq!(authority(&disable), Authority::Coordination);
    }
}
