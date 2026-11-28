//! The table of `tests/golden/cli/options.txt`: each command form in the
//! help, every option the parser knows, how one command line's result is
//! written, and the table made of those results. Test code, shared by the
//! binary's in-process check (`run/option_matrix.rs`) and
//! `tests/cli_golden.rs`, which runs a sample of it through the binary
//! (issue #458).

use std::path::Path;

/// `text` with `dir` (as given and as resolved) written `$TMP`.
pub(crate) fn normalized(text: &str, dir: &Path) -> String {
    let mut text = text.to_owned();
    for form in [std::fs::canonicalize(dir).unwrap(), dir.to_path_buf()] {
        text = text.replace(form.to_str().unwrap(), "$TMP");
    }
    git_reason_hidden(&text)
}

/// `text` with the reason Git gives after "not a git repository" written
/// `{reason}`. Git names the parent directories, or, when the temporary
/// directory is on its own filesystem, the mount point it stopped at, so the
/// reason depends on the machine. It runs to the end of the JSON string or
/// line it is in.
fn git_reason_hidden(text: &str) -> String {
    const SAID: &str = "not a git repository";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(SAID) {
        let (before, after) = rest.split_at(at + SAID.len());
        out.push_str(before);
        let end = after.find(['"', '\n']).unwrap_or(after.len());
        out.push_str(" {reason}");
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Each command form in the help, as a complete command line: a mutation
/// carries its `--request-id`. `ROOT`, `HOME2` and `PID` are filled in per
/// run.
pub(crate) const COMMANDS: &[&str] = &[
    "setup",
    "demo",
    "daemon",
    "mcp",
    // `--json` stops `dashboard` after its options are checked, so that
    // nothing is served.
    "dashboard --json",
    "workspace add w ROOT --request-id r1",
    "workspace list",
    "workspace move w ROOT --request-id r1",
    "workspace remove w",
    "session start s1 w --request-id r1",
    "session attach-pid s1 PID --request-id r1",
    "intent set s1 a.rs --request-id r1",
    "intent end s1 --request-id r1",
    "admit s1 a.rs --request-id r1",
    "complete s1 l1 --request-id r1",
    "brief s1 --request-id r1",
    "remote enable w --name n --request-id r1",
    "remote disable w --request-id r1",
    "remote outbox w",
    "remote sent w",
    "check run w --name n -- true",
    "check status w",
    "check auto w",
    "check flaky w",
    "integrate check w --onto main --name n b -- true",
    "land w --onto main --name n b",
    "land-queue w --onto main",
    "land-cancel w --onto main --request-id r1",
    "graph symbol w name",
    "graph references w name",
    "graph impact w a.rs",
    "graph path w from to",
    "graph map w",
    "graph blind-spots w",
    "graph inflight w",
    "graph export w --out ROOT/index.jsonl",
    "graph import w --in ROOT/index.jsonl",
    "graph history w name",
    "check-write s1 a.rs",
    "recap w",
    "trail w",
    "today",
    "report w",
    "count",
    "policy w",
    "policy set w conflict refuse --request-id r1",
    "remember w text --request-id r1",
    "forget w n1 --request-id r1",
    "recall w words",
    "commit --mine -m text",
    "adopt s2",
    "message w text --request-id r1",
    "handoff s1 text --request-id r1",
    "ack s1 m1 --request-id r1",
    "messages show w m1",
    "messages status w last",
    "messages retract w last --request-id r1",
    "doctor",
    "hosts",
    "guidance",
    "guidance install --root ROOT --file AGENTS.md",
    "guidance remove --root ROOT --file AGENTS.md",
    "status",
    "status --mine",
    "status --all",
    "notify status",
    "statusline",
    "events",
    "compact",
    "changes w",
    "request REQUEST",
    "hook claude-code",
    "hooks install claude-code --workspace w --root ROOT",
    "hooks remove claude-code --root ROOT",
    "install",
    "uninstall",
];

/// Every option the parser knows, with a value valid for the commands that
/// take it.
pub(crate) const OPTIONS: &[(&str, Option<&str>)] = &[
    ("--json", None),
    ("--apply", None),
    ("--help", None),
    ("-h", None),
    ("--symbol", Some("name")),
    ("--tag", Some("t")),
    ("--since-away", None),
    ("--since-seq", Some("3")),
    ("--open", None),
    ("--allow-network", None),
    ("--demo", None),
    ("--check-again", None),
    ("--rebase", None),
    ("--adapt-renames", None),
    ("--symbols", None),
    ("--publish-as-branch", None),
    ("--mine", None),
    ("--detail", None),
    ("--rank", None),
    ("--renamed", None),
    ("--recent", None),
    ("--all", None),
    ("--skill", None),
    ("--purge", None),
    ("--create", None),
    ("--yes", None),
    ("--verbose", None),
    ("--switch", None),
    ("--off", None),
    ("--state-dir", Some("STATE")),
    ("--request-id", Some("r2")),
    ("--summary", Some("text")),
    ("--ttl-ms", Some("1000")),
    ("--after", Some("0")),
    ("--limit", Some("5")),
    ("--workspace", Some("w")),
    ("--root", Some("ROOT")),
    ("--budget-ms", Some("1000")),
    ("--event", Some("PreToolUse")),
    ("--settings", Some("ROOT/settings.json")),
    ("--prompt-summary", Some("off")),
    ("--code-briefing", Some("off")),
    ("--presence", Some("off")),
    ("--presence-minutes", Some("5")),
    ("--max-peers", Some("4")),
    ("--remote", Some("origin")),
    ("--name", Some("n")),
    ("--share-task", Some("on")),
    ("--onto", Some("main")),
    ("--timeout-s", Some("5")),
    ("--wait-s", Some("0")),
    ("--session", Some("s1")),
    ("--to", Some("s2")),
    ("--from", Some("s1")),
    ("--home", Some("HOME2")),
    ("--hosts", Some("claude-code")),
    ("--listen", Some("127.0.0.1:0")),
    ("-m", Some("text")),
    ("--message", Some("text")),
    ("--statusline", Some("on")),
    ("--deadline-ms", Some("200")),
    ("--forget", Some("t1")),
    ("--since", Some("2026-01-01T00:00:00Z")),
    ("--sound", Some("on")),
    ("--depth", Some("1")),
    ("--path", Some("a.rs")),
    ("--out", Some("ROOT/out.jsonl")),
    ("--in", Some("ROOT/in.jsonl")),
    ("--before", Some("5")),
    ("--keep-days", Some("90")),
    ("--month", Some("2026-01")),
    ("--format", Some("agents")),
    ("--file", Some("AGENTS.md")),
    ("--tools", Some("all")),
    ("--host", Some("claude-code")),
    ("--mcp-tools", Some("all")),
    ("--wait-ms", Some("100")),
    ("--max-hops", Some("2")),
    ("--mode", Some("between-steps")),
    ("--outline-reads", Some("on")),
];

/// The command line for `form` with `option` added before any `--`, its
/// placeholders filled in for `dir`.
pub(crate) fn command_line(
    form: &str,
    option: Option<(&str, Option<&str>)>,
    dir: &Path,
) -> Vec<String> {
    let fill = |word: &str| -> String {
        let root = dir.join("root");
        match word {
            "ROOT" => root.display().to_string(),
            "HOME2" => dir.join("home2").display().to_string(),
            "STATE" => dir.join("state").display().to_string(),
            "PID" => std::process::id().to_string(),
            "REQUEST" => format!(
                r#"{{"version":{},"request_id":"read","command":{{"op":"status"}}}}"#,
                langspan::protocol::PROTOCOL_VERSION
            ),
            _ => word.replace("ROOT/", &format!("{}/", root.display())),
        }
    };
    let mut words: Vec<String> = form.split_whitespace().map(fill).collect();
    if let Some((name, value)) = option {
        let at = words.iter().position(|w| w == "--").unwrap_or(words.len());
        let mut added = vec![name.to_owned()];
        added.extend(value.map(fill));
        words.splice(at..at, added);
    }
    words
}

const HELP_START: &str = "LangSpan — local cooperative coordination";
const NOT_VALID: &str = "option {option} is not valid for this command";

/// Makes `dir` ready for `form`: the root, two homes and the state
/// directory each command line expects. `dir/home` is the home it runs in.
pub(crate) fn prepare(form: &str, dir: &Path) {
    std::fs::create_dir_all(dir.join("root")).unwrap();
    // A home with a host configuration, so that `install` and `setup`
    // check their options before they find nothing to install.
    std::fs::create_dir_all(dir.join("home/.claude")).unwrap();
    std::fs::create_dir_all(dir.join("home2/.claude")).unwrap();
    // An empty owner-only directory: no coordinator answers there. For
    // `daemon` and `setup`, which would start one or a login service, a
    // regular file instead, where they stop.
    if form == "daemon" || form == "setup" {
        std::fs::write(dir.join("state"), "").unwrap();
    } else {
        std::fs::create_dir(dir.join("state")).unwrap();
        std::fs::set_permissions(
            dir.join("state"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
    }
}

/// What one command line did, as the table shows it: the exit code and
/// stderr, which is where every refusal of an option goes, or that it
/// printed the help.
pub(crate) fn result_of(
    option: Option<(&str, Option<&str>)>,
    code: i32,
    stdout: &str,
    stderr: &str,
    dir: &Path,
) -> String {
    if stdout.starts_with(HELP_START) {
        return format!("exit {code}: the help");
    }
    let stderr = normalized(stderr.trim_end(), dir);
    let stderr = match option {
        // The parser reads `-m` as `--message`, and names it so.
        Some(("-m", _)) => stderr.replace("option --message ", "option {option} "),
        Some((name, _)) => stderr.replace(&format!("option {name} "), "option {option} "),
        None => stderr,
    };
    if stderr.is_empty() {
        return format!("exit {code}");
    }
    format!("exit {code}: {}", stderr.replace('\n', "\n    "))
}

/// The table: `results` holds, for each command in order, its result
/// alone and then with each option.
pub(crate) fn table(results: &[String]) -> String {
    let mut text = String::from(
        "# Each command line from the help alone, then with each option added.\n\
         # `same` lists the options with which it exits and reports as it\n\
         # does alone: accepted. The others are grouped by what they print,\n\
         # the option named as {option}. `every other option` of the\n\
         # table's list is not valid for the command. No coordinator is running, so a\n\
         # command that gets past its options stops where it asks one. A\n\
         # command that asks before it checks its options (check run,\n\
         # check status) lists them all as same; the transcript tests\n\
         # cover those. `daemon` and `setup` get a state directory that is\n\
         # a regular file, so that neither starts anything.\n",
    );
    for (command, form) in COMMANDS.iter().enumerate() {
        let row = |option: Option<usize>| {
            let index = command * (OPTIONS.len() + 1) + option.map_or(0, |o| o + 1);
            &results[index]
        };
        let alone = row(None);
        let mut groups: Vec<(&String, Vec<&str>)> = Vec::new();
        let mut same = Vec::new();
        for (option, (name, _)) in OPTIONS.iter().enumerate() {
            let result = row(Some(option));
            let named = if *name == "-m" { "--message" } else { name };
            if result.replace("{option}", named) == *alone {
                same.push(*name);
            } else if let Some(group) = groups.iter_mut().find(|(r, _)| *r == result) {
                group.1.push(name);
            } else {
                groups.push((result, vec![name]));
            }
        }
        // Those "not valid for this command", last and unlisted.
        let rest = groups
            .iter()
            .position(|(result, _)| result.contains(NOT_VALID))
            .map(|group| groups.remove(group));
        text.push_str(&format!("\n## langspan {form}\nalone: {alone}\n"));
        text.push_str(&format!("same: {}\n", same.join(" ")));
        for (result, names) in groups {
            text.push_str(&format!("{}\n  {result}\n", names.join(" ")));
        }
        if let Some((result, _)) = rest {
            text.push_str(&format!("every other option\n  {result}\n"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn git_reason_is_the_same_on_every_filesystem() {
        let shown = r#"Git said: fatal: not a git repository {reason}"}"#;
        for reason in [
            " (or any of the parent directories): .git",
            " (or any parent up to mount point /) | Stopping at filesystem boundary \
             (GIT_DISCOVERY_ACROSS_FILESYSTEM not set).",
        ] {
            let text = format!(r#"Git said: fatal: not a git repository{reason}"}}"#);
            assert_eq!(super::git_reason_hidden(&text), shown);
        }
        assert_eq!(super::git_reason_hidden("no git here\n"), "no git here\n");
    }
}
