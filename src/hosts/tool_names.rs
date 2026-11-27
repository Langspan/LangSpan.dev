//! The MCP server's tool list and the hosts whose hooks can stand in for
//! some of it, kept apart from the server so readers need not depend on it.

/// Every tool the server lists, in alphabetical order, one per line. This is
/// the only place the set of tools is written down: adding a tool means adding
/// its name here, and the tests compare the server's tools with this list by
/// name. Documents do not state how many tools there are.
pub const TOOL_NAMES: &[&str] = &[
    "langspan_admit",
    "langspan_adopt",
    "langspan_blind_spots",
    "langspan_brief",
    "langspan_changes",
    "langspan_check_write",
    "langspan_commit_mine",
    "langspan_complete",
    "langspan_events",
    "langspan_flaky_tests",
    "langspan_forget",
    "langspan_handoff",
    "langspan_history",
    "langspan_impact",
    "langspan_integrate_check",
    "langspan_intent_end",
    "langspan_intent_set",
    "langspan_land",
    "langspan_land_cancel",
    "langspan_land_queue",
    "langspan_map",
    "langspan_message_status",
    "langspan_my_changes",
    "langspan_path",
    "langspan_policy",
    "langspan_recall",
    "langspan_recap",
    "langspan_references",
    "langspan_remember",
    "langspan_report",
    "langspan_retract_message",
    "langspan_send_message",
    "langspan_session_register",
    "langspan_show_message",
    "langspan_status",
    "langspan_symbol",
    "langspan_today",
    "langspan_workspace_register",
];

/// Hosts whose hooks report reliably enough for `--tools hooked`. Cursor
/// follows once its adapter is verified on a real host, and Antigravity
/// once `--tools hooked` is checked there.
pub const HOOKED_HOSTS: &[&str] = &["claude-code", "codex"];
