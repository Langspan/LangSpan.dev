//! The host catalog: the hosts LangSpan supports, their session namespaces,
//! the tools each can stand in for, and the host settings shared by install
//! and the hook adapters.
pub(crate) mod port;
pub(crate) mod tool_names;
pub(crate) mod vendor;

use std::time::Duration;

pub const CLAUDE_CODE_SOURCE: &str = langspan_core::CLAUDE_CODE_HOST;
/// The core names a Codex session by this prefix of its ID (issue #129).
pub const CODEX_SOURCE: &str = langspan_core::CODEX_HOST;
pub const CURSOR_SOURCE: &str = langspan_core::CURSOR_HOST;
/// The session namespace: a conversation is `antigravity.<conversationId>`.
pub const ANTIGRAVITY_SOURCE: &str = langspan_core::ANTIGRAVITY_HOST;

/// The host directory overrides a service keeps from setup's environment,
/// the ones `UserHome::files` reads. Directory names only, never a secret.
/// A coordinator started on demand makes the same ones absolute.
pub const HOST_DIRS: [&str; 3] = ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "GEMINI_CLI_HOME"];

/// How long one watcher runs at most. A longer turn falls back to the next
/// hook's briefing for what changes after this.
pub const WATCH_LIMIT: Duration = Duration::from_secs(20 * 60);
/// The hook entry's `timeout`, in seconds: past `WATCH_LIMIT`, so the host
/// never stops a watcher that is about to stop itself.
pub const WATCHER_TIMEOUT_SECONDS: u64 = WATCH_LIMIT.as_secs() + 60;
