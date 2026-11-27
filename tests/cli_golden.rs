//! The CLI's help text, its usage errors and which options each command
//! accepts, pinned byte for byte in `tests/golden/cli/` (issue #323). To
//! change one on purpose, see `tests/support/golden.rs`.

#[path = "support/golden.rs"]
mod golden;
mod support;

use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

/// A directory of the test's own, removed when it is dropped. Under `/tmp`,
/// not the platform's temporary directory: macOS's is long enough that a
/// state directory under it does not fit a Unix socket path, and the
/// output would differ from Linux.
struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(format!("/tmp/lsg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the binary in `dir` with `dir/home` as its home, no coordinator
/// autostart, `state` as its state directory and stdin closed; kills it
/// after 30 seconds.
fn run(dir: &Path, state: &Path, args: &[String]) -> Output {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_langspan"));
    support::in_home(&mut command, &home)
        .env("LANGSPAN_AUTOSTART", "off")
        .env("LANGSPAN_STATE_DIR", state)
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = support::Owned::spawn(&mut command);
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.child().try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "`langspan {args:?}` ran past 30 s"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output()
}

/// `text` with `dir` (as given and as resolved) written `$TMP`.
fn normalized(text: &str, dir: &Path) -> String {
    let mut text = text.to_owned();
    for form in [std::fs::canonicalize(dir).unwrap(), dir.to_path_buf()] {
        text = text.replace(form.to_str().unwrap(), "$TMP");
    }
    text
}

fn shown(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.len() > 80 {
                format!("<{} bytes>", arg.len())
            } else if arg.is_empty() || arg.contains([' ', '"', '{']) {
                format!("'{arg}'")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn help() -> String {
    let scratch = Scratch::new("help");
    let output = run(&scratch.0, &scratch.0.join("state"), &["--help".into()]);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn help_text_matches_its_golden_copy() {
    let text = help();
    golden::check("cli/help.txt", &text);
    // Every way of asking prints the same bytes.
    let scratch = Scratch::new("help-forms");
    for args in [&[][..], &["-h"], &["status", "--help"]] {
        let args: Vec<String> = args.iter().map(|arg| (*arg).into()).collect();
        let output = run(&scratch.0, &scratch.0.join("state"), &args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), text, "{args:?}");
    }
}

/// One command line per usage error the CLI reports before it asks the
/// coordinator anything.
const USAGE_ERRORS: &[&[&str]] = &[
    &["frobnicate"],
    &["admit"],
    &["adopt"],
    &["--version", "extra"],
    &["status", "--bogus"],
    &["status", "--limit"],
    &["status", "--symbol"],
    &["status", "--tag"],
    &["events", "--limit", "1", "--limit", "2"],
    &["status", "--all", "--all"],
    &["status", "--symbol", "name"],
    &["remember", "w", "text", "--symbol", "a", "--symbol", "b"],
    &["status", "--tag", "t"],
    &["recap", "w", "--tag", "a", "--tag", "b"],
    &["status", "--home", "elsewhere"],
    &["status", "--apply"],
    &["status", "--limit", "5"],
    &["setup", "--apply"],
    &["setup", "--json"],
    &["setup", "--skill"],
    &["demo", "--json"],
    &["demo", "--state-dir", "elsewhere"],
    &["demo", "--limit", "5"],
    &["dashboard", "--listen", "nowhere"],
    &["dashboard", "--json"],
    &["mcp", "--tools", "some"],
    &["mcp", "--tools", "hooked"],
    &["mcp", "--host", "elsewhere"],
    &["admit", "s1", "a.rs"],
    &["workspace", "remove", "w", "--apply"],
    &[
        "admit",
        "s1",
        "a.rs",
        "--ttl-ms",
        "soon",
        "--request-id",
        "r1",
    ],
    &["session", "attach-pid", "s1", "one", "--request-id", "r1"],
    &["brief", "s1", "--max-peers", "many"],
    &["brief", "s1", "--max-peers", "99999999999"],
    &["events", "--after", "start"],
    &["events", "--limit", "all"],
    &["changes", "w", "--after", "start"],
    &["graph", "history", "w", "name", "--before", "then"],
    &["graph", "impact", "w", "a.rs", "--depth", "99999999999"],
    &["graph", "export", "w"],
    &["graph", "import", "w"],
    &["remote", "enable", "w", "--request-id", "r1"],
    &[
        "remote",
        "enable",
        "w",
        "--name",
        "n",
        "--share-task",
        "maybe",
        "--request-id",
        "r1",
    ],
    &[
        "policy",
        "set",
        "w",
        "nothing",
        "refuse",
        "--request-id",
        "r1",
    ],
    &[
        "policy",
        "set",
        "w",
        "conflict",
        "maybe",
        "--request-id",
        "r1",
    ],
    &[
        "policy",
        "set",
        "w",
        "conflict",
        "wait",
        "--wait-ms",
        "long",
        "--request-id",
        "r1",
    ],
    &["today", "--since", "yesterday"],
    &["report", "w", "--since", "yesterday"],
    &["count", "--month", "May"],
    &["compact", "--keep-days", "99999999999"],
    &["notify", "maybe"],
    &["notify", "on", "--sound", "loud"],
    &["statusline", "--deadline-ms", "1"],
    &["check", "auto", "w", "--off", "--name", "n"],
    &["check", "flaky", "w", "--forget", "t1"],
    &["integrate", "check", "w", "--name", "n", "b", "--", "true"],
    &[
        "land", "w", "--onto", "main", "--name", "n", "b", "--wait-s", "9999",
    ],
    &["land-cancel", "w", "--onto", "main"],
    &["commit", "--mine"],
    &["commit"],
    &["install", "--hosts", "nothing"],
    &["install", "--hosts", "claude-code", "--statusline", "maybe"],
    &["install", "--mcp-tools", "some"],
    &["install", "--presence-minutes", "99"],
    &["guidance", "--format", "html"],
    &["guidance", "install"],
    &["guidance", "install", "--root", "."],
    &[
        "hooks",
        "install",
        "nothing",
        "--workspace",
        "w",
        "--root",
        ".",
    ],
    &["hooks", "install", "claude-code", "--root", "."],
    &["hook", "nothing"],
    &["hook", "antigravity"],
    &["hook", "antigravity", "--event", "Nothing"],
    &["hook", "claude-code", "--event", "PreToolUse"],
    &["hook", "claude-code", "--workspace", "w"],
    &["hook", "codex", "--mode", "between-steps"],
    &["hook", "claude-code", "--ttl-ms", "0"],
    &["hook", "claude-code", "--presence", "maybe"],
    &["hook", "claude-code", "--bogus"],
    &["hook", "claude-code", "--limit", "5"],
    &["request", "not json"],
];

#[test]
fn usage_errors_match_their_golden_copy() {
    let scratch = Scratch::new("usage");
    let state = scratch.0.join("state");
    // `remote enable` reads this machine's participant ID there first.
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let mut cases: Vec<Vec<String>> = USAGE_ERRORS
        .iter()
        .map(|args| args.iter().map(|arg| (*arg).into()).collect())
        .collect();
    cases.push(vec![
        "request".into(),
        "x".repeat(langspan::protocol::MAX_FRAME_BYTES),
    ]);
    let mut text = String::new();
    for args in &cases {
        let output = run(&scratch.0, &state, args);
        assert!(
            !output.status.success(),
            "`langspan {}` succeeded: {output:?}",
            shown(args)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        // A usage error is found before the coordinator is asked anything.
        assert!(
            !stderr.contains("coordinat"),
            "`langspan {}` reached the coordinator: {stderr}",
            shown(args)
        );
        text.push_str(&format!(
            "$ langspan {}\nexit {}\n",
            shown(args),
            output.status.code().unwrap()
        ));
        for (name, stream) in [("stdout", stdout), ("stderr", stderr)] {
            for line in normalized(&stream, &scratch.0).lines() {
                text.push_str(&format!("{name}: {line}\n"));
            }
        }
        text.push('\n');
    }
    golden::check("cli/usage-errors.txt", &text);
}

/// Each command form in the help, as a complete command line: a mutation
/// carries its `--request-id`. `ROOT`, `HOME2` and `PID` are filled in per
/// run.
const COMMANDS: &[&str] = &[
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
const OPTIONS: &[(&str, Option<&str>)] = &[
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
fn command_line(form: &str, option: Option<(&str, Option<&str>)>, dir: &Path) -> Vec<String> {
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

/// What one command line did, as the table shows it: the exit code and
/// stderr, which is where every refusal of an option goes, or that it
/// printed the help.
fn result_of(form: &str, option: Option<(&str, Option<&str>)>, dir: &Path) -> String {
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
    let args = command_line(form, option, dir);
    let output = run(dir, &dir.join("state"), &args);
    if output.stdout.starts_with(HELP_START.as_bytes()) {
        return format!("exit {}: the help", output.status.code().unwrap_or(-1));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = normalized(stderr.trim_end(), dir);
    let stderr = match option {
        // The parser reads `-m` as `--message`, and names it so.
        Some(("-m", _)) => stderr.replace("option --message ", "option {option} "),
        Some((name, _)) => stderr.replace(&format!("option {name} "), "option {option} "),
        None => stderr,
    };
    let code = output.status.code().unwrap_or(-1);
    if stderr.is_empty() {
        return format!("exit {code}");
    }
    format!("exit {code}: {}", stderr.replace('\n', "\n    "))
}

#[test]
fn every_option_against_every_command_matches_the_table() {
    let scratch = Scratch::new("options");
    // Each (command, option) pair, `None` the command alone.
    let pairs: Vec<(usize, Option<usize>)> = (0..COMMANDS.len())
        .flat_map(|command| {
            std::iter::once((command, None))
                .chain((0..OPTIONS.len()).map(move |option| (command, Some(option))))
        })
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    let mut results: Vec<(usize, String)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&(command, option)) = pairs.get(index) else {
                            return done;
                        };
                        let dir = scratch.0.join(index.to_string());
                        let result = result_of(COMMANDS[command], option.map(|o| OPTIONS[o]), &dir);
                        let _ = std::fs::remove_dir_all(&dir);
                        done.push((index, result));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    });
    results.sort();
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
            &results[index].1
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
    golden::check("cli/options.txt", &text);
}

/// The table covers every option and command the help names, so a new one
/// needs a row before its tests pass.
#[test]
fn the_option_table_covers_every_option_and_command_in_the_help() {
    let help = help();
    for word in help.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        let option = word.trim_end_matches('-');
        // `--on-interface-change` comes after `--`, in the words `check
        // auto` reads itself, not from the option parser.
        if option.starts_with("--")
            && option.len() > 2
            && !["--version", "--on-interface-change"].contains(&option)
        {
            assert!(
                OPTIONS.iter().any(|(name, _)| *name == option),
                "the help names {option}, which the option table does not test"
            );
        }
    }
    for line in help.lines() {
        let Some(usage) = line.strip_prefix("  ") else {
            continue;
        };
        let words: Vec<&str> = usage
            .split_whitespace()
            .take_while(|word| word.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
            .collect();
        if words.is_empty() || usage.starts_with(' ') {
            continue;
        }
        let name = words.join(" ");
        assert!(
            COMMANDS.iter().any(|form| form.starts_with(&name)),
            "the help names `langspan {name}`, which the option table does not test"
        );
    }
}
