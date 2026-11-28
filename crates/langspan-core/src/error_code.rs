//! The wire error codes: the one place a code's string is written.
//! `CoreError::code` and `Outcome::Error::code` stay strings on the wire,
//! so a client still reads a code a newer daemon sends; this list is how
//! LangSpan writes one. `tests/golden/error_codes.txt` holds it, and the
//! usage guide's "Error codes" table says what each means.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! error_codes {
    ($($variant:ident = $name:literal,)+) => {
        /// A wire error code. Serialized as its `as_str` name.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum ErrorCode {
            $(#[serde(rename = $name)] $variant,)+
        }

        impl ErrorCode {
            /// Every code, sorted by name.
            pub const ALL: &[ErrorCode] = &[$(ErrorCode::$variant,)+];

            /// The code as it appears on the wire.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(ErrorCode::$variant => $name,)+
                }
            }
        }
    };
}

error_codes! {
    AdoptionChain = "adoption_chain",
    AdoptionChanged = "adoption_changed",
    AlreadyAdopted = "already_adopted",
    AlreadyPublished = "already_published",
    AlreadyRetracted = "already_retracted",
    CoordinationUnavailable = "coordination_unavailable",
    DifferentRepository = "different_repository",
    IndexExportFailed = "index_export_failed",
    IndexImportRefused = "index_import_refused",
    IndexUnavailable = "index_unavailable",
    Internal = "internal",
    InvalidAdoption = "invalid_adoption",
    InvalidAlert = "invalid_alert",
    InvalidAway = "invalid_away",
    InvalidBase = "invalid_base",
    InvalidBatch = "invalid_batch",
    InvalidBranchOids = "invalid_branch_oids",
    InvalidBranchRef = "invalid_branch_ref",
    InvalidCalls = "invalid_calls",
    InvalidCheck = "invalid_check",
    InvalidCheckReport = "invalid_check_report",
    InvalidCheckout = "invalid_checkout",
    InvalidCommand = "invalid_command",
    InvalidContentDigest = "invalid_content_digest",
    InvalidCoverage = "invalid_coverage",
    InvalidCursor = "invalid_cursor",
    InvalidDeadline = "invalid_deadline",
    InvalidDigest = "invalid_digest",
    InvalidEvent = "invalid_event",
    InvalidFailure = "invalid_failure",
    InvalidFrame = "invalid_frame",
    InvalidGitOid = "invalid_git_oid",
    InvalidGraphContext = "invalid_graph_context",
    InvalidHosts = "invalid_hosts",
    InvalidId = "invalid_id",
    InvalidInputDigest = "invalid_input_digest",
    InvalidKey = "invalid_key",
    InvalidLanding = "invalid_landing",
    InvalidLeft = "invalid_left",
    InvalidLimit = "invalid_limit",
    InvalidLineDigests = "invalid_line_digests",
    InvalidLines = "invalid_lines",
    InvalidMessage = "invalid_message",
    InvalidMove = "invalid_move",
    InvalidName = "invalid_name",
    InvalidNote = "invalid_note",
    InvalidNotice = "invalid_notice",
    InvalidNumber = "invalid_number",
    InvalidOverwrites = "invalid_overwrites",
    InvalidParticipant = "invalid_participant",
    InvalidPath = "invalid_path",
    InvalidPaths = "invalid_paths",
    InvalidPayload = "invalid_payload",
    InvalidPolicy = "invalid_policy",
    InvalidPresence = "invalid_presence",
    InvalidProcessIdentity = "invalid_process_identity",
    InvalidPublication = "invalid_publication",
    InvalidQuery = "invalid_query",
    InvalidReason = "invalid_reason",
    InvalidRefusal = "invalid_refusal",
    InvalidRegions = "invalid_regions",
    InvalidRemote = "invalid_remote",
    InvalidRequest = "invalid_request",
    InvalidRequestId = "invalid_request_id",
    InvalidRetention = "invalid_retention",
    InvalidRoot = "invalid_root",
    InvalidSequence = "invalid_sequence",
    InvalidSession = "invalid_session",
    InvalidSummary = "invalid_summary",
    InvalidSymbol = "invalid_symbol",
    InvalidSymbols = "invalid_symbols",
    InvalidTag = "invalid_tag",
    InvalidTestName = "invalid_test_name",
    InvalidTestResults = "invalid_test_results",
    InvalidTold = "invalid_told",
    InvalidTtl = "invalid_ttl",
    InvalidTurn = "invalid_turn",
    InvalidWait = "invalid_wait",
    InvalidWindow = "invalid_window",
    InvalidWorkspaceVerification = "invalid_workspace_verification",
    LeaseExpired = "lease_expired",
    LeaseIdReused = "lease_id_reused",
    LeaseNotActive = "lease_not_active",
    LeaseNotFound = "lease_not_found",
    MessageNotKept = "message_not_kept",
    NoCurrentEvidence = "no_current_evidence",
    NotCheckoutTop = "not_checkout_top",
    NotHolder = "not_holder",
    NotQueued = "not_queued",
    NotSender = "not_sender",
    OldRootExists = "old_root_exists",
    OldRootMissing = "old_root_missing",
    OtherWorkspace = "other_workspace",
    OwnPublication = "own_publication",
    OwnerGrantOff = "owner_grant_off",
    PolicyFixed = "policy_fixed",
    PresenceOff = "presence_off",
    ProcessIdentityMismatch = "process_identity_mismatch",
    ReadTimeout = "read_timeout",
    ReceiptUnreadable = "receipt_unreadable",
    RecipientNotFound = "recipient_not_found",
    RemoteNotEnabled = "remote_not_enabled",
    RemoteSharingOn = "remote_sharing_on",
    ReportBusy = "report_busy",
    ReportUnavailable = "report_unavailable",
    RequestIdConflict = "request_id_conflict",
    ResponseTooLarge = "response_too_large",
    RootDerivedId = "root_derived_id",
    SecretInCommand = "secret_in_command",
    SecretInTestName = "secret_in_test_name",
    SenderNotInWorkspace = "sender_not_in_workspace",
    SessionConflict = "session_conflict",
    SessionEnded = "session_ended",
    SessionLive = "session_live",
    SessionNotFound = "session_not_found",
    SnapshotMismatch = "snapshot_mismatch",
    StaleLease = "stale_lease",
    StaleLivenessObservation = "stale_liveness_observation",
    StaleSession = "stale_session",
    StatusPart = "status_part",
    StorageError = "storage_error",
    StorageOperation = "storage_operation",
    SymbolAmbiguous = "symbol_ambiguous",
    SymbolNotFound = "symbol_not_found",
    TooManyAttempts = "too_many_attempts",
    TooManyInputs = "too_many_inputs",
    TooManyKeys = "too_many_keys",
    TooManyPaths = "too_many_paths",
    TooManyRelatedOperations = "too_many_related_operations",
    TooManyVerifiedOverlaps = "too_many_verified_overlaps",
    TurnLost = "turn_lost",
    UnknownMessage = "unknown_message",
    UnknownNotice = "unknown_notice",
    UnknownSession = "unknown_session",
    UnknownWorkspace = "unknown_workspace",
    UnsupportedVersion = "unsupported_version",
    WorkspaceBusy = "workspace_busy",
    WorkspaceChanged = "workspace_changed",
    WorkspaceConflict = "workspace_conflict",
    WorkspaceInUse = "workspace_in_use",
    WorkspaceNotFound = "workspace_not_found",
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ErrorCode> for String {
    fn from(code: ErrorCode) -> String {
        code.as_str().to_owned()
    }
}
