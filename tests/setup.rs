#![cfg(unix)]
//! `langspan setup`, the coordinator's login service and the on-demand
//! start (issue #159), against a temporary home, real hook processes and a
//! real daemon.
//!
//! The systemd test puts a stand-in `systemctl` first on `PATH`: the
//! machine's own user manager is never touched. The stand-in records each
//! call and, on `restart`, runs the unit's `ExecStart` line, so the daemon
//! the unit names is the one that runs. `systemd-analyze verify`, where
//! installed, checks the unit as systemd parses it.

mod support;

use langspan::client;
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command as Process, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Base(PathBuf);

impl Base {
    fn new() -> Self {
        // Not `temp_dir()`: on macOS it is a long path under `/var/folders`,
        // and the home's `.local/state/langspan/daemon.sock` would not fit
        // in a Unix socket address.
        let base = Path::new("/tmp").join(format!(
            "ls-setup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let base = Base(fs::canonicalize(base).unwrap());
        fs::create_dir_all(base.home().join(".claude")).unwrap();
        // Where Claude Code keeps it with `--home`, and with
        // `CLAUDE_CONFIG_DIR` set (as `support::in_home` sets it).
        fs::write(base.home().join(".claude.json"), "{}\n").unwrap();
        fs::write(base.home().join(".claude/.claude.json"), "{}\n").unwrap();
        base
    }
    fn home(&self) -> PathBuf {
        self.0.join("home")
    }
    #[cfg(target_os = "linux")]
    fn state_dir(&self) -> PathBuf {
        self.home().join(".local/state/langspan")
    }
    /// `langspan ARGS` in this home (`HOME`, not `--home`), with `PATH`
    /// starting at `bin` when given, and stdin not a terminal.
    fn run(&self, args: &[&str], bin: Option<&Path>) -> Output {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut command, &self.home())
            .args(args)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove(langspan::autostart::ENV)
            .stdin(Stdio::null());
        if let Some(bin) = bin {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![bin.to_path_buf()];
            paths.extend(std::env::split_paths(&path));
            command.env("PATH", std::env::join_paths(paths).unwrap());
        }
        command.output().unwrap()
    }
    /// Every file under the home, with its content.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
            entries.sort_by_key(|entry| entry.path());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    out.push((path.clone(), Vec::new()));
                    walk(&path, out);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap_or_default()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.home(), &mut out);
        out
    }
}

impl Drop for Base {
    fn drop(&mut self) {
        for pid in daemons_for(&self.0) {
            let _ = rustix::process::kill_process(
                rustix::process::Pid::from_raw(pid).unwrap(),
                rustix::process::Signal::KILL,
            );
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The `langspan daemon` processes whose command line names a path under
/// `base`, read from `/proc` (Linux) or `ps` (elsewhere).
fn daemons_for(base: &Path) -> Vec<i32> {
    let base = base.to_string_lossy().into_owned();
    let listed = Process::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| line.contains(" daemon --state-dir ") && line.contains(&base))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn setup_without_a_terminal_or_yes_prints_the_plan_and_changes_nothing() {
    let base = Base::new();
    let before = base.snapshot();
    let output = base.run(&["setup", "--home", &base.home().to_string_lossy()], None);
    let shown = text(&output);
    assert!(output.status.success(), "{shown}");
    assert!(shown.contains("State directory: "), "{shown}");
    assert!(
        shown.contains(".claude/settings.json would become:"),
        "{shown}"
    );
    assert!(shown.contains("skills/langspan/SKILL.md"), "{shown}");
    assert!(shown.contains("Coordinator:"), "{shown}");
    assert!(
        shown.contains("Not run from a terminal and no --yes, so nothing was changed."),
        "{shown}"
    );
    assert!(
        !shown.contains("Run the same command with --apply"),
        "{shown}"
    );
    assert_eq!(base.snapshot(), before, "setup changed the home: {shown}");
    assert!(daemons_for(&base.0).is_empty());
}

#[test]
fn setup_yes_without_a_service_starts_a_coordinator_and_a_second_run_changes_nothing() {
    let base = Base::new();
    let home = base.home().to_string_lossy().into_owned();
    let output = base.run(&["setup", "--home", &home, "--yes"], None);
    let shown = text(&output);
    assert!(shown.contains("Wrote "), "{shown}");
    assert!(shown.contains("No login service: --home names"), "{shown}");
    assert!(
        shown.contains("Started a coordinator in the background"),
        "{shown}"
    );
    assert!(shown.contains("langspan doctor:"), "{shown}");
    assert!(shown.contains("ok Coordinator reachable"), "{shown}");
    assert!(
        base.home()
            .join(".claude/skills/langspan/SKILL.md")
            .is_file()
    );
    assert_eq!(daemons_for(&base.0).len(), 1, "{shown}");
    let before = base.snapshot();
    let again = base.run(&["setup", "--home", &home, "--yes"], None);
    let shown = text(&again);
    assert!(again.status.success(), "{shown}");
    assert!(
        shown.contains("Already set up; nothing to change."),
        "{shown}"
    );
    assert!(!shown.contains("Wrote "), "{shown}");
    assert_eq!(base.snapshot(), before, "{shown}");
    assert_eq!(daemons_for(&base.0).len(), 1);
}

#[test]
fn setup_refuses_a_state_directory_too_long_for_the_socket_and_changes_nothing() {
    let base = Base::new();
    let home = base.home().to_string_lossy().into_owned();
    let long = base.0.join("s".repeat(120));
    let before = base.snapshot();
    let output = base.run(
        &[
            "setup",
            "--home",
            &home,
            "--state-dir",
            &long.to_string_lossy(),
            "--yes",
        ],
        None,
    );
    let shown = text(&output);
    assert!(!output.status.success(), "{shown}");
    assert!(shown.contains("too long for a Unix socket"), "{shown}");
    assert!(!shown.contains("Wrote "), "{shown}");
    assert!(!shown.contains("Started a coordinator"), "{shown}");
    assert_eq!(base.snapshot(), before, "{shown}");
    assert!(!long.exists());
    assert!(daemons_for(&base.0).is_empty());
}

#[test]
fn setup_refuses_apply_and_json() {
    let base = Base::new();
    let home = base.home().to_string_lossy().into_owned();
    for extra in ["--apply", "--json", "--skill"] {
        let output = base.run(&["setup", "--home", &home, extra], None);
        assert!(!output.status.success(), "{extra}: {}", text(&output));
    }
}

/// A stand-in `systemctl` in `bin` that records its arguments in `calls`
/// and keeps `active`/`enabled` marks next to it. `restart` runs the unit's
/// `ExecStart` in the background, as the user manager would.
#[cfg(target_os = "linux")]
fn stand_in_systemctl(bin: &Path, unit: &Path) {
    fs::create_dir_all(bin).unwrap();
    let script = format!(
        r#"#!/bin/sh
dir='{dir}'
echo "$*" >> "$dir/calls"
case "$*" in
  "--user show --property=Version") echo Version=255; exit 0 ;;
  "--user daemon-reload") exit 0 ;;
  "--user enable langspan.service") touch "$dir/enabled"; exit 0 ;;
  "--user restart langspan.service")
    line=$(grep '^ExecStart=' '{unit}' | cut -d= -f2-)
    eval "set -- $line"
    "$@" >/dev/null 2>&1 </dev/null &
    touch "$dir/active"; exit 0 ;;
  "--user is-active --quiet langspan.service") test -e "$dir/active"; exit $? ;;
  "--user is-enabled --quiet langspan.service") test -e "$dir/enabled"; exit $? ;;
  "--user disable --now langspan.service") rm -f "$dir/active" "$dir/enabled"; exit 0 ;;
esac
echo "unexpected: $*" >&2
exit 1
"#,
        dir = bin.display(),
        unit = unit.display()
    );
    let path = bin.join("systemctl");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn setup_installs_a_systemd_user_unit_that_uninstall_removes() {
    let base = Base::new();
    let bin = base.0.join("bin");
    let unit = base.home().join(".config/systemd/user/langspan.service");
    stand_in_systemctl(&bin, &unit);
    let output = base.run(&["setup", "--yes"], Some(&bin));
    let shown = text(&output);
    assert!(output.status.success(), "{shown}");
    assert!(
        shown.contains("so the coordinator runs at every login"),
        "{shown}"
    );
    let written = fs::read_to_string(&unit).unwrap();
    let state_dir = base.state_dir();
    assert!(
        written.contains(&format!(
            "ExecStart=\"{}\" daemon --state-dir \"{}\"\n",
            fs::canonicalize(env!("CARGO_BIN_EXE_langspan"))
                .unwrap()
                .display(),
            state_dir.display()
        )),
        "{written}"
    );
    assert_eq!(
        fs::metadata(&unit).unwrap().permissions().mode() & 0o777,
        0o600
    );
    if let Ok(verified) = Process::new("systemd-analyze")
        .args(["verify", "--user"])
        .arg(&unit)
        .output()
    {
        let said = String::from_utf8_lossy(&verified.stderr);
        // Only what concerns this unit: the machine's other units may warn.
        let ours: Vec<&str> = said
            .lines()
            .filter(|line| line.contains("langspan.service"))
            .collect();
        assert!(ours.is_empty(), "systemd-analyze: {said}");
    }
    let calls = fs::read_to_string(bin.join("calls")).unwrap();
    assert!(
        calls.contains(
            "--user daemon-reload\n--user enable langspan.service\n--user restart langspan.service\n"
        ),
        "{calls}"
    );
    assert!(shown.contains("ok Coordinator reachable"), "{shown}");
    assert_eq!(daemons_for(&base.0).len(), 1, "{shown}");
    let log = state_dir.join("daemon.log");
    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // A second run: the file matches and the service runs.
    let again = base.run(&["setup", "--yes"], Some(&bin));
    let shown = text(&again);
    assert!(
        shown.contains("Already set up; nothing to change."),
        "{shown}"
    );
    assert_eq!(fs::read_to_string(&unit).unwrap(), written);

    // A hook finds the service installed for its state directory and does
    // not start a second coordinator, even with the first one gone.
    for pid in daemons_for(&base.0) {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::Signal::KILL,
        )
        .unwrap();
    }
    wait_until_gone(&base.0);
    let (_, stderr) = run_hook(&base.0, &state_dir, 1500, &bin);
    assert!(
        stderr.contains("\"reason\":\"service_installed\""),
        "{stderr}"
    );
    assert!(daemons_for(&base.0).is_empty());

    let dry = base.run(&["uninstall"], Some(&bin));
    let shown = text(&dry);
    assert!(
        shown.contains("login service") && shown.contains("would be stopped and removed"),
        "{shown}"
    );
    assert!(unit.is_file());
    let removed = base.run(&["uninstall", "--apply"], Some(&bin));
    let shown = text(&removed);
    assert!(removed.status.success(), "{shown}");
    assert!(
        shown.contains("Stopped and removed the coordinator's login service"),
        "{shown}"
    );
    assert!(!unit.exists());
    let calls = fs::read_to_string(bin.join("calls")).unwrap();
    assert!(
        calls.ends_with("--user disable --now langspan.service\n--user daemon-reload\n"),
        "{calls}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn setup_refuses_a_unit_langspan_did_not_write() {
    let base = Base::new();
    let bin = base.0.join("bin");
    let unit = base.home().join(".config/systemd/user/langspan.service");
    stand_in_systemctl(&bin, &unit);
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "[Service]\nExecStart=/bin/true\n").unwrap();
    let before = base.snapshot();
    let output = base.run(&["setup", "--yes"], Some(&bin));
    assert!(!output.status.success());
    assert!(
        text(&output).contains("LangSpan did not write it"),
        "{}",
        text(&output)
    );
    assert_eq!(base.snapshot(), before);
    let output = base.run(&["uninstall", "--apply"], Some(&bin));
    assert!(
        text(&output).contains("was not written by LangSpan"),
        "{}",
        text(&output)
    );
    assert!(unit.is_file());
}

#[cfg(target_os = "linux")]
fn wait_until_gone(base: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !daemons_for(base).is_empty() {
        assert!(Instant::now() < deadline, "daemon did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One Claude Code `PreToolUse` hook for an edit under the home, as the host
/// runs it: user-level, with `budget_ms`. Returns its run time and stderr.
fn run_hook(base: &Path, state_dir: &Path, budget_ms: u64, bin: &Path) -> (Duration, String) {
    let root = base.join("repo");
    fs::create_dir_all(&root).unwrap();
    let event = json!({
        "session_id": "00000000-0000-4000-8000-0000000000d1",
        "transcript_path": "/dev/null",
        "cwd": root,
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {"file_path": root.join("a.txt"), "old_string": "a", "new_string": "b"},
        "tool_use_id": format!("toolu_{}", NEXT.fetch_add(1, Ordering::Relaxed)),
    });
    let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
    support::in_home(&mut command, &base.join("home"))
        .args(["hook", "claude-code", "--state-dir"])
        .arg(state_dir)
        .args(["--budget-ms", &budget_ms.to_string()])
        .env_remove("XDG_CONFIG_HOME")
        .env_remove(langspan::autostart::ENV)
        .env(
            "PATH",
            std::env::join_paths(
                std::iter::once(bin.to_path_buf()).chain(std::env::split_paths(
                    &std::env::var_os("PATH").unwrap_or_default(),
                )),
            )
            .unwrap(),
        )
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let took = started.elapsed();
    assert!(output.status.success(), "{}", text(&output));
    (took, String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn two_hooks_with_no_coordinator_start_exactly_one_within_their_budget() {
    let base = Base::new();
    let state_dir = base.0.join("state");
    let bin = base.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let budget_ms = 1500;
    let results: Vec<(Duration, String)> = std::thread::scope(|scope| {
        let hooks: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| run_hook(&base.0, &state_dir, budget_ms, &bin)))
            .collect();
        hooks.into_iter().map(|hook| hook.join().unwrap()).collect()
    });
    for (took, stderr) in &results {
        // The budget plus a process start: far below a hook that waited for
        // a coordinator without a bound.
        assert!(
            *took < Duration::from_millis(budget_ms + 1500),
            "took {took:?}: {stderr}"
        );
    }
    let started = results
        .iter()
        .filter(|(_, stderr)| stderr.contains("\"event\":\"coordinator_started\""))
        .count();
    assert_eq!(started, 1, "{results:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !client::accepts(&state_dir, Instant::now() + Duration::from_millis(200)) {
        assert!(
            Instant::now() < deadline,
            "the started coordinator never accepted"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(daemons_for(&base.0).len(), 1);
    let log = fs::read_to_string(state_dir.join("daemon.log")).unwrap();
    assert_eq!(
        log.matches("coordinator_started_on_demand").count(),
        1,
        "{log}"
    );
    // A third hook finds it running and starts nothing.
    let (_, stderr) = run_hook(&base.0, &state_dir, budget_ms, &bin);
    assert!(!stderr.contains("coordinator_"), "{stderr}");
    assert_eq!(daemons_for(&base.0).len(), 1);
}

#[test]
fn the_on_demand_start_can_be_turned_off() {
    let base = Base::new();
    let state_dir = base.0.join("state");
    let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
    support::in_home(&mut command, &base.home())
        .args(["hook", "claude-code", "--state-dir"])
        .arg(&state_dir)
        .env(langspan::autostart::ENV, "off")
        .stdin(Stdio::null());
    let output = command.output().unwrap();
    assert!(output.status.success(), "{}", text(&output));
    assert!(!state_dir.exists());
    assert!(daemons_for(&base.0).is_empty());
}
