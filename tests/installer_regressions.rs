use langspan::install::{self, Registration};
use serde_json::{Value, json};

fn registration(executable: &str, root: &str) -> Registration {
    Registration {
        executable: executable.into(),
        state_dir: "/state".into(),
        workspace_id: "demo".into(),
        root: root.into(),
        share_prompt: true,
        code_briefing: true,
    }
}

fn serialized(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn versioned_executable_install_upgrade_and_remove_are_idempotent() {
    let original = json!({"model": "x"});
    let first = registration("/opt/langspan-0.1.0", "/root");
    let installed = install::plan(Some(original.clone()), Some(&first)).unwrap();
    for event in install::EVENTS {
        assert!(install::is_managed(
            &installed["hooks"][event][0]["hooks"][0]
        ));
    }
    let repeated = install::plan(Some(installed.clone()), Some(&first)).unwrap();
    assert_eq!(serialized(&repeated), serialized(&installed));

    let moved = registration("/new/location/langspan-0.2.0", "/other root");
    let upgraded = install::plan(Some(installed), Some(&moved)).unwrap();
    for event in install::EVENTS {
        assert_eq!(upgraded["hooks"][event].as_array().unwrap().len(), 1);
        assert_eq!(
            upgraded["hooks"][event][0]["hooks"][0]["command"],
            moved.command()
        );
    }
    assert_eq!(install::plan(Some(upgraded), None).unwrap(), original);
}

#[test]
fn legacy_generated_commands_migrate_without_claiming_arbitrary_commands() {
    let legacy = "'/opt/bin/langspan' hook claude-code --state-dir '/state' --workspace 'demo' --root '/root'";
    let old_hook = json!({"type": "command", "command": legacy, "timeout": 5});
    assert!(install::is_managed(&old_hook));
    for command in [
        "'/opt/bin/langspan' hook claude-code && '/other/guard'",
        "'/opt/bin/langspan-0.1.0' hook claude-code --state-dir '/state' --workspace 'demo' --root '/root'",
        "'/other/tool' hook claude-code --state-dir '/state' --workspace 'demo' --root '/root'",
    ] {
        assert!(
            !install::is_managed(&json!({"type": "command", "command": command})),
            "unmarked command must remain user-owned: {command}"
        );
    }
    assert!(!install::is_managed(
        &json!({"type": "prompt", "command": legacy})
    ));

    let old = json!({"hooks": {"PreToolUse": [{"matcher": "Edit|Write", "hooks": [old_hook]}]}});
    let updated = install::plan(
        Some(old),
        Some(&registration("/opt/langspan-0.2.0", "/root")),
    )
    .unwrap();
    assert_eq!(updated["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert!(install::is_managed(
        &updated["hooks"]["PreToolUse"][0]["hooks"][0]
    ));
}

#[test]
fn removal_preserves_unrelated_empty_groups_and_event_keys() {
    for original in [
        json!({"hooks": {}}),
        json!({"before": 1, "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": []}],
            "PostToolUse": [], "Stop": []
        }, "after": 2}),
    ] {
        let removed = install::plan(Some(original.clone()), None).unwrap();
        assert_eq!(serialized(&removed), serialized(&original));
    }
}

#[test]
fn removing_managed_entries_preserves_remaining_key_order() {
    let managed =
        json!({"type": "command", "command": registration("/bin/langspan", "/root").command()});
    let mut original: Value = serde_json::from_str(
        r#"{"first":1,"hooks":{"PreToolUse":[{"hooks":[]}],"Stop":[],"SessionStart":[],"UserPromptSubmit":[]},"middle":2,"last":3}"#,
    )
    .unwrap();
    original["hooks"]["PreToolUse"][0]["hooks"] = json!([managed.clone()]);
    let removed = install::plan(Some(original), None).unwrap();
    let event_keys: Vec<_> = removed["hooks"].as_object().unwrap().keys().collect();
    assert_eq!(event_keys, ["Stop", "SessionStart", "UserPromptSubmit"]);

    let mut only_ours: Value = serde_json::from_str(
        r#"{"first":1,"hooks":{"PreToolUse":[{"hooks":[]}]},"middle":2,"last":3}"#,
    )
    .unwrap();
    only_ours["hooks"]["PreToolUse"][0]["hooks"] = json!([managed]);
    let removed = install::plan(Some(only_ours), None).unwrap();
    let keys: Vec<_> = removed.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["first", "middle", "last"]);
}

#[test]
fn replacing_a_registration_keeps_existing_settings_and_event_positions() {
    let original: Value = serde_json::from_str(
        r#"{"first":1,"hooks":{"PreToolUse":[],"Stop":[],"PostToolUse":[],"SessionStart":[],"PostToolUseFailure":[]},"middle":2,"last":3}"#,
    )
    .unwrap();
    let installed = install::plan(
        Some(original.clone()),
        Some(&registration("/bin/langspan", "/root")),
    )
    .unwrap();
    let before: Vec<_> = original["hooks"].as_object().unwrap().keys().collect();
    let after: Vec<_> = installed["hooks"].as_object().unwrap().keys().collect();
    // Existing events keep their positions; events new to the file follow them.
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(
        after[before.len()..],
        [
            "UserPromptSubmit",
            "SessionEnd",
            "StopFailure",
            "SubagentStop",
            "PostToolBatch"
        ]
    );
    let upgraded = install::plan(
        Some(installed.clone()),
        Some(&registration("/bin/langspan", "/new root")),
    )
    .unwrap();
    let before: Vec<_> = installed.as_object().unwrap().keys().collect();
    let after: Vec<_> = upgraded.as_object().unwrap().keys().collect();
    assert_eq!(after, before);
    let before: Vec<_> = installed["hooks"].as_object().unwrap().keys().collect();
    let after: Vec<_> = upgraded["hooks"].as_object().unwrap().keys().collect();
    assert_eq!(after, before);
}

#[test]
fn codex_registration_covers_patch_shell_and_session_events_only() {
    let registration = registration("/opt/langspan-0.2.0", "/installed project");
    let planned = install::plan_for(install::Vendor::Codex, None, Some(&registration)).unwrap();
    let hooks = planned["hooks"].as_object().unwrap();
    let events: Vec<_> = hooks.keys().collect();
    assert_eq!(
        events,
        [
            "PreToolUse",
            "PostToolUse",
            "SessionStart",
            "UserPromptSubmit",
            "Stop",
            "SessionEnd"
        ]
    );
    for (event, groups) in hooks {
        assert_eq!(groups.as_array().unwrap().len(), 1);
        if event.ends_with("ToolUse") {
            assert_eq!(groups[0]["matcher"], "^(apply_patch|Bash)$");
        } else {
            assert!(groups[0].get("matcher").is_none(), "{event}");
        }
        let hook = &groups[0]["hooks"][0];
        assert_eq!(hook["type"], "command");
        // Codex allows SessionEnd at most 3 seconds.
        assert_eq!(hook["timeout"], if event == "SessionEnd" { 3 } else { 5 });
        assert!(hook.get("onFailure").is_none());
        assert_eq!(
            hook["command"],
            registration.command_for(install::Vendor::Codex)
        );
        assert!(install::is_managed_for(install::Vendor::Codex, hook));
        assert!(!install::is_managed(hook));
        assert!(hook["command"].as_str().unwrap().contains(
            "hook codex --state-dir '/state' --workspace 'demo' --root '/installed project'"
        ));
    }
}

#[test]
fn codex_upgrade_and_remove_preserve_claude_and_other_hooks() {
    let original: Value = serde_json::from_str(
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/other/setup"}]}],"PreToolUse":[{"matcher":"Bash","hooks":[]}]},"other":true}"#,
    )
    .unwrap();
    let claude = install::plan(
        Some(original),
        Some(&registration("/opt/langspan-claude", "/root")),
    )
    .unwrap();
    let codex_registration = registration("/opt/langspan-codex-v1", "/root");
    let installed = install::plan_for(
        install::Vendor::Codex,
        Some(claude.clone()),
        Some(&codex_registration),
    )
    .unwrap();
    let repeated = install::plan_for(
        install::Vendor::Codex,
        Some(installed.clone()),
        Some(&codex_registration),
    )
    .unwrap();
    assert_eq!(serialized(&repeated), serialized(&installed));
    let upgraded = install::plan_for(
        install::Vendor::Codex,
        Some(installed),
        Some(&registration("/new/bin/langspan-codex-v2", "/new root")),
    )
    .unwrap();
    let removed = install::plan_for(install::Vendor::Codex, Some(upgraded), None).unwrap();
    assert_eq!(serialized(&removed), serialized(&claude));
}

#[test]
fn unmarked_codex_commands_remain_user_owned() {
    let hook = json!({
        "type": "command",
        "command": "'/bin/langspan' hook codex --state-dir '/state' --workspace 'demo' --root '/root'"
    });
    assert!(!install::is_managed_for(install::Vendor::Codex, &hook));
    let original =
        json!({"hooks": {"PreToolUse": [{"matcher": "^apply_patch$", "hooks": [hook]}]}});
    let removed = install::plan_for(install::Vendor::Codex, Some(original.clone()), None).unwrap();
    assert_eq!(serialized(&removed), serialized(&original));
}

#[test]
fn gemini_registration_names_its_hooks_and_times_them_in_milliseconds() {
    let gemini = install::Vendor::Gemini;
    let registration = registration("/opt/langspan", "/installed project");
    let planned = install::plan_for(gemini, None, Some(&registration)).unwrap();
    let hooks = planned["hooks"].as_object().unwrap();
    let events: Vec<_> = hooks.keys().collect();
    assert_eq!(
        events,
        [
            "BeforeTool",
            "AfterTool",
            "SessionStart",
            "BeforeAgent",
            "SessionEnd"
        ]
    );
    for (event, groups) in hooks {
        assert_eq!(groups.as_array().unwrap().len(), 1);
        match event.as_str() {
            "BeforeTool" => assert_eq!(
                groups[0]["matcher"],
                "^(write_file|replace|run_shell_command)$"
            ),
            "AfterTool" => assert_eq!(
                groups[0]["matcher"],
                "^(write_file|replace|run_shell_command|read_file)$"
            ),
            _ => assert!(groups[0].get("matcher").is_none(), "{event}"),
        }
        let hook = &groups[0]["hooks"][0];
        assert_eq!(hook["name"], "langspan");
        assert_eq!(hook["type"], "command");
        assert_eq!(hook["timeout"], 5000, "Gemini CLI reads milliseconds");
        assert!(install::is_managed_for(gemini, hook));
        assert!(!install::is_managed_for(install::Vendor::ClaudeCode, hook));
        assert!(hook["command"].as_str().unwrap().contains(
            "hook gemini-cli --state-dir '/state' --workspace 'demo' --root '/installed project'"
        ));
    }
}

#[test]
fn gemini_install_is_idempotent_and_remove_restores_the_owners_settings() {
    let gemini = install::Vendor::Gemini;
    let original: Value = serde_json::from_str(
        r#"{"general":{"vimMode":true},"hooks":{"BeforeTool":[{"matcher":"write_file","hooks":[{"name":"guard","type":"command","command":"/x/guard.sh","timeout":2000}]}]},"mcpServers":{"docs":{"command":"npx","args":["docs"],"env":{"TOKEN":"$DOCS_TOKEN"}}}}"#,
    )
    .unwrap();
    let first = registration("/opt/langspan", "/root");
    let installed = install::plan_for(gemini, Some(original.clone()), Some(&first)).unwrap();
    assert_eq!(
        installed["hooks"]["BeforeTool"][0], original["hooks"]["BeforeTool"][0],
        "the owner's hook stays first and unchanged"
    );
    assert_eq!(installed["mcpServers"], original["mcpServers"]);
    let repeated = install::plan_for(gemini, Some(installed.clone()), Some(&first)).unwrap();
    assert_eq!(serialized(&repeated), serialized(&installed));
    // Another host's install in the same file is not Gemini's to remove.
    let with_claude = install::plan(Some(installed), Some(&first)).unwrap();
    let removed = install::plan_for(gemini, Some(with_claude), None).unwrap();
    assert_eq!(
        serialized(&install::plan(Some(removed), None).unwrap()),
        serialized(&original)
    );
}

#[test]
fn a_gemini_install_path_with_a_dollar_sign_is_refused() {
    // Gemini CLI expands `$NAME` in every settings string.
    for (executable, root) in [("/opt/$HOME/langspan", "/root"), ("/opt/langspan", "/r$x")] {
        let error = install::plan_for(
            install::Vendor::Gemini,
            None,
            Some(&registration(executable, root)),
        )
        .unwrap_err();
        assert!(error.contains("would expand the `$`"), "{error}");
    }
    // The other hosts run the quoted command as it is.
    assert!(
        install::plan_for(
            install::Vendor::Codex,
            None,
            Some(&registration("/opt/$HOME/langspan", "/root"))
        )
        .is_ok()
    );
}

#[cfg(unix)]
mod filesystem {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "langspan-installer-regression-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn settings_symlinks_are_refused_without_modifying_link_or_target() {
        let scratch = Scratch::new();
        let target = scratch.0.join("shared.json");
        let settings = scratch.0.join("settings.local.json");
        let original = b"{\"model\":\"x\"}\n";
        fs::write(&target, original).unwrap();
        symlink(&target, &settings).unwrap();

        let read = install::read(&settings);
        let write = install::write_planned(&settings, None, &|_| {
            install::json_bytes(&json!({"changed": true}))
        });
        assert!(read.is_err(), "reading a settings symlink must be refused");
        assert!(write.is_err(), "writing a settings symlink must be refused");
        assert!(fs::symlink_metadata(&settings).unwrap().is_symlink());
        assert_eq!(fs::read(&target).unwrap(), original);
    }

    #[test]
    fn dangling_settings_symlinks_are_not_treated_as_missing_files() {
        let scratch = Scratch::new();
        let target = scratch.0.join("missing.json");
        let settings = scratch.0.join("settings.local.json");
        symlink(&target, &settings).unwrap();

        let read = install::read(&settings);
        let write = install::write_planned(&settings, None, &|_| {
            install::json_bytes(&json!({"changed": true}))
        });
        assert!(read.is_err(), "dangling settings symlink must be refused");
        assert!(write.is_err(), "dangling settings symlink must be refused");
        assert!(fs::symlink_metadata(&settings).unwrap().is_symlink());
        assert!(!target.exists());
    }

    #[test]
    fn failed_exclusive_creation_keeps_a_preexisting_temporary_file() {
        let scratch = Scratch::new();
        let settings = scratch.0.join("settings.local.json");
        let temporary = scratch.0.join(format!(
            ".settings.local.json.langspan-{}",
            std::process::id()
        ));
        fs::write(&settings, b"{\"original\":true}\n").unwrap();
        fs::write(&temporary, b"not owned by this invocation").unwrap();

        assert!(
            install::write_planned(&settings, None, &|_| {
                install::json_bytes(&json!({"changed": true}))
            })
            .is_err()
        );
        assert_eq!(
            fs::read(&temporary).unwrap(),
            b"not owned by this invocation"
        );
        assert_eq!(fs::read(&settings).unwrap(), b"{\"original\":true}\n");
    }

    #[test]
    fn generated_command_passes_only_expected_arguments_to_the_shell() {
        let scratch = Scratch::new();
        let executable = scratch.0.join("langspan-versioned");
        fs::write(&executable, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let registration = registration(executable.to_str().unwrap(), "/it's here");
        for (vendor, name) in [
            (install::Vendor::ClaudeCode, "claude-code"),
            (install::Vendor::Codex, "codex"),
        ] {
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", &registration.command_for(vendor)])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "hook\n{name}\n--state-dir\n/state\n--workspace\ndemo\n--root\n/it's here\n"
                )
            );
        }
    }
}

#[test]
fn the_prompt_summary_opt_out_is_installed_recognised_replaced_and_removed() {
    let mut private = registration("/bin/langspan", "/root");
    private.share_prompt = false;
    for vendor in [install::Vendor::ClaudeCode, install::Vendor::Codex] {
        let installed = install::plan_for(vendor, None, Some(&private)).unwrap();
        let hook = &installed["hooks"]["UserPromptSubmit"][0]["hooks"][0];
        assert!(
            hook["command"]
                .as_str()
                .unwrap()
                .contains("--prompt-summary off #")
        );
        assert!(install::is_managed_for(vendor, hook));
        // Reinstalling with sharing on replaces the entry instead of adding one.
        let shared = install::plan_for(
            vendor,
            Some(installed.clone()),
            Some(&registration("/bin/langspan", "/root")),
        )
        .unwrap();
        assert_eq!(
            shared["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(!shared.to_string().contains("--prompt-summary"));
        let removed = install::plan_for(vendor, Some(installed), None).unwrap();
        assert_eq!(removed, serde_json::json!({}));
    }
    // Any other trailing option is not ours to manage.
    let foreign = serde_json::json!({"type":"command","command":
        "'/bin/langspan' hook codex --state-dir '/s' --workspace 'w' --root '/r' --ttl-ms 5 # langspan-managed:codex:v1"});
    assert!(!install::is_managed_for(install::Vendor::Codex, &foreign));
}

#[test]
fn the_code_briefing_opt_out_alone_or_with_the_prompt_opt_out_is_recognised_and_removed() {
    for (share_prompt, expected) in [
        (true, "--root '/root' --code-briefing off #"),
        (false, "--prompt-summary off --code-briefing off #"),
    ] {
        let mut quiet = registration("/bin/langspan", "/root");
        quiet.code_briefing = false;
        quiet.share_prompt = share_prompt;
        for vendor in [install::Vendor::ClaudeCode, install::Vendor::Codex] {
            let installed = install::plan_for(vendor, None, Some(&quiet)).unwrap();
            let hook = &installed["hooks"]["UserPromptSubmit"][0]["hooks"][0];
            assert!(
                hook["command"].as_str().unwrap().contains(expected),
                "{hook}"
            );
            assert!(install::is_managed_for(vendor, hook));
            // Reinstalling with defaults replaces the entry.
            let default = install::plan_for(
                vendor,
                Some(installed.clone()),
                Some(&registration("/bin/langspan", "/root")),
            )
            .unwrap();
            assert_eq!(
                default["hooks"]["UserPromptSubmit"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert!(!default.to_string().contains("--code-briefing"));
            let removed = install::plan_for(vendor, Some(installed), None).unwrap();
            assert_eq!(removed, json!({}));
        }
    }
    // Another order, a repeat, or a value other than off is not ours.
    for tail in [
        "--code-briefing off --prompt-summary off",
        "--code-briefing off --code-briefing off",
        "--code-briefing on",
        "--code-briefing",
    ] {
        let hook = json!({"type":"command","command": format!(
            "'/bin/langspan' hook codex --state-dir '/state' --workspace 'demo' --root '/root' {tail} # langspan-managed:codex:v1")});
        assert!(
            !install::is_managed_for(install::Vendor::Codex, &hook),
            "{tail}"
        );
    }
}
