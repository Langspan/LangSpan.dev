//! The CLI's help text, its usage errors and which options each command
//! accepts, pinned byte for byte in `tests/golden/cli/` (issue #323). To
//! change one on purpose, see `tests/support/golden.rs`. The binary's own
//! unit test `every_option_against_every_command_matches_the_table` checks
//! the whole option table in-process; here a sample of it runs through the
//! binary.

#[path = "support/golden.rs"]
mod golden;
#[allow(dead_code)] // Its `table` is for the in-process check.
#[path = "../src/cli/run/option_table.rs"]
mod option_table;
mod support;

use option_table::{COMMANDS, OPTIONS, normalized};

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

/// What `form` with `option` did through the binary, as the table shows it.
fn result_of(form: &str, option: Option<(&str, Option<&str>)>, dir: &Path) -> String {
    option_table::prepare(form, dir);
    let args = option_table::command_line(form, option, dir);
    let output = run(dir, &dir.join("state"), &args);
    option_table::result_of(
        option,
        output.status.code().unwrap_or(-1),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        dir,
    )
}

/// One command form for each driver `main` and `run` dispatch to.
const FAMILIES: &[&str] = &[
    "setup",
    "demo",
    "daemon",
    "mcp",
    "dashboard --json",
    "workspace add w ROOT --request-id r1",
    "check run w --name n -- true",
    "check auto w",
    "check flaky w",
    "integrate check w --onto main --name n b -- true",
    "land-cancel w --onto main --request-id r1",
    "commit --mine -m text",
    "adopt s2",
    "status --mine",
    "doctor",
    "hosts",
    "guidance install --root ROOT --file AGENTS.md",
    "notify status",
    "statusline",
    "count",
    "request REQUEST",
    "hook claude-code",
    "hooks install claude-code --workspace w --root ROOT",
    "install",
    "uninstall",
];

/// What the golden table says `form` with `option` prints.
fn in_the_table(table: &str, form: &str, option: Option<&str>) -> String {
    let (_, section) = table
        .split_once(&format!("\n## langspan {form}\n"))
        .unwrap_or_else(|| panic!("the table has no `langspan {form}`"));
    let lines: Vec<&str> = section.lines().take_while(|l| !l.is_empty()).collect();
    let alone = lines[0].strip_prefix("alone: ").unwrap().to_owned();
    let Some(option) = option else { return alone };
    let same = lines[1].strip_prefix("same: ").unwrap();
    if same.split(' ').any(|name| name == option) {
        let named = if option == "-m" { "--message" } else { option };
        return alone.replace(&format!("option {named} "), "option {option} ");
    }
    let group = lines[2..].chunks(2).find(|group| {
        group[0] == "every other option" || group[0].split(' ').any(|name| name == option)
    });
    group.unwrap()[1].trim_start().to_owned()
}

/// The binary reaches the dispatch the in-process table checks: for each
/// driver, its command line alone, with an option it accepts and with one
/// it refuses, started as a process because `main`'s wiring, exit code and
/// stderr are the subject.
#[test]
fn the_binary_prints_what_the_table_says_for_each_driver() {
    let table = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cli/options.txt"),
    )
    .unwrap();
    let scratch = Scratch::new("families");
    std::thread::scope(|scope| {
        for (index, form) in FAMILIES.iter().enumerate() {
            let (table, dir) = (&table, scratch.0.join(index.to_string()));
            scope.spawn(move || {
                assert!(COMMANDS.contains(form), "{form} is not in the table");
                let same = in_the_table(table, form, None);
                let accepted = OPTIONS
                    .iter()
                    .find(|(name, _)| in_the_table(table, form, Some(name)) == same);
                let refused = OPTIONS.iter().find(|(name, _)| {
                    !["--help", "-h"].contains(name)
                        && in_the_table(table, form, Some(name)) != same
                });
                for (line, option) in [None, accepted, refused].into_iter().enumerate() {
                    if line > 0 && option.is_none() {
                        continue;
                    }
                    let dir = dir.join(line.to_string());
                    let expected = in_the_table(table, form, option.map(|(name, _)| *name));
                    assert_eq!(
                        result_of(form, option.copied(), &dir),
                        expected,
                        "{form} {option:?}"
                    );
                }
            });
        }
    });
}
