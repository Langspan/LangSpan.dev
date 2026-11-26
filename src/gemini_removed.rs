//! `langspan hook gemini-cli` after Gemini CLI support was removed (issue
//! #246, [ADR 0033](../docs/adr/0033-gemini-cli-adapter.md)). An install
//! from an earlier release still runs this command on every Gemini CLI
//! event, so for one release it stays and does nothing: it answers `{}` (no
//! decision, so Gemini CLI's own approval decides), starts no coordinator,
//! reads or writes no state and exits 0. At a session's start it prints one
//! line on stderr saying how to remove the hooks.

/// The answer to every event: no decision.
pub const ANSWER: &str = "{}";

/// The line printed once per session, at its `SessionStart`. `root` is the
/// project a project-level hook names with `--root`.
pub fn notice(root: Option<&str>) -> String {
    let remove = match root {
        Some(root) => format!(
            "`langspan hooks remove gemini-cli --root {} --apply`",
            crate::install::quote(root)
        ),
        None => "`langspan hooks remove gemini-cli --root DIR --apply` for a project".into(),
    };
    format!(
        "LangSpan: Gemini CLI support was removed, so this hook does nothing. Remove it with `langspan uninstall --hosts gemini-cli --apply` for the user-level install, or {remove}."
    )
}

/// The notice for `event`, if it is the start of a session. Anything that
/// is not a JSON object naming `SessionStart` gets none.
pub fn notice_for(event: &[u8], root: Option<&str>) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Event {
        hook_event_name: String,
    }
    let event: Event = serde_json::from_slice(event).ok()?;
    (event.hook_event_name == "SessionStart").then(|| notice(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_session_start_gets_the_notice() {
        let start = br#"{"session_id":"s","hook_event_name":"SessionStart","source":"resume"}"#;
        assert_eq!(notice_for(start, None), Some(notice(None)));
        for other in [
            &br#"{"hook_event_name":"BeforeTool"}"#[..],
            br#"{"hook_event_name":"SessionEnd"}"#,
            b"{broken",
            b"[]",
            b"",
        ] {
            assert_eq!(notice_for(other, None), None);
        }
        let project = notice(Some("/work/it's"));
        assert!(
            project.contains(r"--root '/work/it'\''s' --apply"),
            "{project}"
        );
        assert_eq!(project.lines().count(), 1);
    }
}
