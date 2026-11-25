use std::process::Command;

#[test]
fn help_explains_the_usable_coordination_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for command in [
        "workspace",
        "session",
        "intent",
        "admit",
        "complete",
        "events",
        "mcp",
        "dashboard",
        "install",
        "uninstall",
        "--request-id",
    ] {
        assert!(text.contains(command), "help missing {command}");
    }
}

/// Every `langspan` command line the skill names is in the help: its
/// command words begin a usage line, and each option it gives is the
/// command's own, from that usage entry.
#[test]
fn every_command_the_skill_names_is_in_the_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    let commands = langspan::skill::commands();
    assert!(commands.len() >= 4, "{commands:?}");
    for command in commands {
        let words: Vec<&str> = command.split_whitespace().skip(1).collect();
        let name: Vec<&str> = words
            .iter()
            .take_while(|word| word.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
            .copied()
            .collect();
        assert!(!name.is_empty(), "{command}");
        let usage = format!("  {}", name.join(" "));
        // The usage entry: its first line and the indented lines after it,
        // up to the next command.
        let entry: String = help
            .lines()
            .skip_while(|line| {
                !(line.starts_with(&usage) && line[usage.len()..].starts_with([' ', '\n'])
                    || *line == usage)
            })
            .enumerate()
            .take_while(|(index, line)| *index == 0 || line.starts_with("    "))
            .map(|(_, line)| format!("{line}\n"))
            .collect();
        assert!(
            !entry.is_empty(),
            "the skill names `{command}`, not in the help"
        );
        for option in words
            .iter()
            .filter(|word| word.starts_with('-') && **word != "--")
        {
            assert!(
                entry.contains(option),
                "the skill gives `{command}` the option {option}, not in its usage:\n{entry}"
            );
        }
    }
}

#[test]
fn mutation_requires_an_explicit_request_identity_before_connecting() {
    let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["admit", "alice", "src/api.rs"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--request-id"));
}

#[test]
fn hook_install_is_a_dry_run_unless_applied_and_remove_restores_settings() {
    let base = std::env::temp_dir().join(format!("ls-install-{}", std::process::id()));
    let settings = base.join("root/.claude/settings.local.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    // An event list LangSpan never registers is the owner's and survives
    // the round trip. (An empty list for an event LangSpan does register,
    // such as Stop, is removed with LangSpan's entry: see docs/usage.md.)
    let original = "{\"model\":\"x\",\"hooks\":{\"Notification\":[]}}\n";
    std::fs::write(&settings, original).unwrap();
    let run = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_langspan"))
            .args(args)
            .args(["--root", base.join("root").to_str().unwrap()])
            .args(["--state-dir", base.join("state").to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let install = ["hooks", "install", "claude-code", "--workspace", "demo"];
    assert!(run(&install).contains("Dry run; nothing was written"));
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), original);
    run(&[&install[..], &["--apply"]].concat());
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(
        written["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" hook claude-code ")
    );
    assert_eq!(written["model"], "x");
    run(&["hooks", "remove", "claude-code", "--apply"]);
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        restored,
        serde_json::from_str::<serde_json::Value>(original).unwrap()
    );
    std::fs::remove_dir_all(base).unwrap();
}
