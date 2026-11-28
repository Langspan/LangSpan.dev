//! Every command in the help alone and with every option, checked against
//! `tests/golden/cli/options.txt` in-process (issue #458): the same
//! dispatch, parser and drivers as the binary, without starting it 6,156
//! times. The commands read `HOME`, the state directory, the working
//! directory and stdin, which a test cannot set within its own process,
//! so this test starts itself once per command with those set, and that
//! process runs each of the command's lines in turn. `tests/cli_golden.rs`
//! runs a sample of the table through the binary, which checks that `main`
//! wires this dispatch.

#[path = "../../../tests/support/golden.rs"]
mod golden;

use super::option_table as table;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};
use table::{COMMANDS, OPTIONS};

/// Names the command whose lines `one_command_in_process` runs.
const COMMAND_ENV: &str = "LANGSPAN_TEST_OPTION_ROW";
/// Opens and closes each command line's output on stdout and stderr.
const MARK: &str = "\u{1}langspan-option-row";

/// One command line, as `main` runs it, and its exit code.
fn dispatch(input: Vec<String>) -> u8 {
    if input.first().map(String::as_str) == Some("hook") {
        let code = crate::hook::hook(input);
        return (0..=u8::MAX).find(|n| ExitCode::from(*n) == code).unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(super::run(input.clone()));
    // As at the binary's exit, nothing left running is waited for.
    runtime.shutdown_background();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{}", crate::error_line(&error.to_string(), &input));
            1
        }
    }
}

/// Run by `every_option_against_every_command_matches_the_table` in a
/// process of its own, whose working directory is the command's scratch
/// directory: each line of one command, its output between marks.
#[test]
#[ignore = "started by every_option_against_every_command_matches_the_table"]
fn one_command_in_process() {
    let Some(command) = std::env::var_os(COMMAND_ENV) else {
        return;
    };
    let form = COMMANDS[command.to_str().unwrap().parse::<usize>().unwrap()];
    let dir = std::env::current_dir().unwrap();
    for option in std::iter::once(None).chain(OPTIONS.iter().copied().map(Some)) {
        // Each line starts from a fresh directory, as if alone.
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        }
        table::prepare(form, &dir);
        let args = table::command_line(form, option, &dir);
        println!("{MARK}");
        eprintln!("{MARK}");
        let code = dispatch(args);
        println!("{MARK}");
        eprintln!("{MARK} {code}");
    }
}

/// Each command line's (exit code, stdout, stderr) from one process's
/// marked output.
fn lines_of(stdout: &str, stderr: &str) -> Vec<(i32, String, String)> {
    let mark = format!("{MARK}\n");
    let outs = stdout.split(&mark).skip(1).step_by(2);
    let errs = stderr.split(&mark).skip(1);
    outs.zip(errs)
        .map(|(out, err)| {
            let (err, code) = err.rsplit_once(&format!("{MARK} ")).unwrap();
            let code = code.lines().next().unwrap().parse().unwrap();
            (code, out.to_owned(), err.to_owned())
        })
        .collect()
}

/// The results of one command alone and with each option, from a process
/// of this test's own executable run in `dir`; killed after 60 seconds.
fn row(command: usize, dir: &Path) -> Vec<String> {
    std::fs::create_dir_all(dir).unwrap();
    let home = dir.join("owner");
    let name = concat!(module_path!(), "::one_command_in_process");
    let name = name.split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--ignored", "--nocapture"])
        // As `tests/support::in_home` sets them for the binary.
        .env("HOME", &home)
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env("CODEX_HOME", home.join(".codex"))
        .env("GEMINI_CLI_HOME", &home)
        .env_remove("XDG_STATE_HOME")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("LANGSPAN_AUTOSTART", "off")
        .env("LANGSPAN_STATE_DIR", dir.join("state"))
        .env(COMMAND_ENV, command.to_string())
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Read while it runs: the help it prints more than fills a pipe.
    let read = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut text = Vec::new();
            pipe.read_to_end(&mut text).unwrap();
            String::from_utf8_lossy(&text).into_owned()
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the lines of `langspan {}` ran past 60 s",
                COMMANDS[command]
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let (stdout, stderr) = (stdout.join().unwrap(), stderr.join().unwrap());
    assert!(status.success(), "{stdout}\n{stderr}");
    let lines = lines_of(&stdout, &stderr);
    assert_eq!(lines.len(), OPTIONS.len() + 1, "{stdout}\n{stderr}");
    let options = std::iter::once(None).chain(OPTIONS.iter().copied().map(Some));
    lines
        .iter()
        .zip(options)
        .map(|((code, out, err), option)| table::result_of(option, *code, out, err, dir))
        .collect()
}

/// A directory of the test's own under `/tmp`, removed when it is dropped:
/// macOS's temporary directory is too long for a state directory's socket
/// path, and the output would differ from Linux.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn every_option_against_every_command_matches_the_table() {
    let scratch = Scratch(PathBuf::from(format!(
        "/tmp/lsg-options-{}",
        std::process::id()
    )));
    let _ = std::fs::remove_dir_all(&scratch.0);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    let mut rows: Vec<(usize, Vec<String>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let command = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if command >= COMMANDS.len() {
                            return done;
                        }
                        done.push((command, row(command, &scratch.0.join(command.to_string()))));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    });
    rows.sort();
    for (command, row) in &mut rows {
        if COMMANDS[*command] == "demo" {
            demo_as_the_binary(row);
        }
    }
    let results: Vec<String> = rows.into_iter().flat_map(|(_, row)| row).collect();
    golden::check("cli/options.txt", &table::table(&results));
}

/// Past its options `demo` starts the running executable as its
/// coordinator, which here is this test's, not `langspan`. Each line that
/// gets that far takes the golden's `alone` line instead, which
/// `tests/cli_golden.rs` checks against the binary.
fn demo_as_the_binary(row: &mut [String]) {
    let alone = row[0].clone();
    assert!(
        alone.contains("the demo's coordinator exited before it accepted connections"),
        "`langspan demo` stopped before it started its coordinator: {alone}"
    );
    let golden = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cli/options.txt"),
    )
    .unwrap();
    let (_, section) = golden.split_once("\n## langspan demo\nalone: ").unwrap();
    let binary = section.lines().next().unwrap();
    for result in row.iter_mut().filter(|result| **result == alone) {
        *result = binary.to_owned();
    }
}

/// The table covers every option and command the help names, so a new one
/// needs a row before its tests pass.
#[test]
fn the_option_table_covers_every_option_and_command_in_the_help() {
    let help = crate::help::HELP;
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
