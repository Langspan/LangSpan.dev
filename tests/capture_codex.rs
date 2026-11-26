#![cfg(unix)]
//! `scripts/capture-codex.sh` refuses to run, and writes nothing, unless
//! `codex login status` reports a signed-in account (issue A3). The `codex`
//! on `PATH` here is a stand-in for the real CLI's status command only: the
//! real one needs an account, and the script stops before any other command.

use std::{
    fs,
    io::Write as _,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A new directory holding a `codex` whose `login status` prints `output`
/// and exits with `code`, and whose every other command fails.
fn scratch(output: &str, code: i32) -> Scratch {
    let base = std::env::temp_dir().join(format!(
        "ls-capture-codex-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(base.join("bin")).unwrap();
    let codex = base.join("bin/codex");
    fs::write(
        &codex,
        format!(
            "#!/bin/sh\n[ \"$1 $2\" = 'login status' ] || exit 99\necho '{output}'\nexit {code}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    Scratch(base)
}

/// Run the script with `codex_home` as `CODEX_HOME`, or with it unset.
fn capture(base: &Path, codex_home: Option<&Path>) -> Output {
    let path = format!(
        "{}:{}",
        base.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new("bash");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/capture-codex.sh"
        ))
        .arg(base.join("out"))
        .env("PATH", path)
        .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_STATE_DIR", base.join("state"))
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(base);
    match codex_home {
        Some(home) => command.env("CODEX_HOME", home),
        None => command.env_remove("CODEX_HOME"),
    };
    command.output().unwrap()
}

#[test]
fn the_codex_capture_stops_and_writes_nothing_when_codex_is_not_signed_in() {
    for (output, code) in [("Not logged in", 1), ("Not logged in", 0), ("", 2)] {
        let base = scratch(output, code);
        let home = base.0.join("codex-home");
        let run = capture(&base.0, Some(&home));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert_ne!(run.status.code(), Some(0), "{output:?} {code}: {stderr}");
        assert!(
            stderr.contains("codex is not signed in"),
            "{output:?} {code}: {stderr}"
        );
        for written in ["out", "state", "codex-home"] {
            assert!(
                !base.0.join(written).exists(),
                "{output:?} {code}: wrote {written}"
            );
        }
    }
}

#[test]
fn the_codex_capture_refuses_to_run_without_a_separate_codex_home() {
    let base = scratch("Logged in using ChatGPT", 0);
    let run = capture(&base.0, None);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_ne!(run.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("set CODEX_HOME"), "{stderr}");
    assert!(!base.0.join("out").exists());
}

/// A `codex` that reports a signed-in account and whose every `exec` turn
/// records `per_turn` events through `langspan session start`, so the run's
/// own events span several pages of `langspan events`.
fn signed_in(per_turn: u32) -> Scratch {
    let base = scratch("Logged in using ChatGPT", 0);
    let script = r#"#!/bin/sh
case "$1 $2" in
  'login status') echo 'Logged in using ChatGPT'; exit 0 ;;
  '--version ') echo 'codex-cli 0.0.0-standin'; exit 0 ;;
esac
[ "$1" = exec ] || { echo "stand-in codex: unexpected $*" >&2; exit 99; }
turn=$$-$(date +%s)
i=0
while [ $i -lt PER_TURN ]; do
  i=$((i + 1))
  "$LANGSPAN" session start "standin.t$turn-$i" standin --request-id "standin-$turn-$i" >&2 || exit 98
done
"#;
    fs::write(
        base.0.join("bin/codex"),
        script.replace("PER_TURN", &per_turn.to_string()),
    )
    .unwrap();
    base
}

fn langspan(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(args)
        .env("LANGSPAN_STATE_DIR", state)
        .env("LANGSPAN_AUTOSTART", "off")
        .output()
        .unwrap()
}

/// Every event in the coordinator, in order, as `(seq, line)`.
fn all_events(state: &Path) -> Vec<(u64, String)> {
    let mut events = Vec::new();
    let mut after = 0;
    loop {
        let run = langspan(
            state,
            &["events", "--limit", "1000", "--after", &after.to_string()],
        );
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let page = String::from_utf8(run.stdout).unwrap();
        if page.is_empty() {
            return events;
        }
        for line in page.lines() {
            let seq = line.split_once(' ').unwrap().0.parse().unwrap();
            events.push((seq, line.to_owned()));
            after = seq;
        }
    }
}

struct Daemon(std::process::Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon(state: &Path) -> Daemon {
    fs::create_dir_all(state).unwrap();
    fs::set_permissions(state, fs::Permissions::from_mode(0o700)).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["daemon", "--state-dir"])
        .arg(state)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(state.join("daemon.sock")).is_err() {
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    daemon
}

/// The capture saves exactly its own events, all of them, whether the
/// coordinator starts empty or already holds more than one 1,000-event page,
/// and while the run's own events fill several transport pages.
#[test]
fn the_codex_capture_saves_every_event_of_its_own_run_and_no_earlier_one() {
    for seeded in [0, 1001] {
        let base = signed_in(150);
        let state = base.0.join("state");
        let _daemon = daemon(&state);
        let seed = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "for w in seed standin; do mkdir \"$w\" && \"$LANGSPAN\" --request-id add-$w workspace add $w \"$PWD/$w\" >/dev/null || exit 1; done; i=0; while [ $i -lt {seeded} ]; do i=$((i + 1)); \"$LANGSPAN\" session start seed.s$i seed --request-id seed-$i >/dev/null || exit 1; done"
            ))
            .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_STATE_DIR", &state)
            .env("LANGSPAN_AUTOSTART", "off")
            .current_dir(&base.0)
            .output()
            .unwrap();
        assert!(
            seed.status.success(),
            "{}{}",
            String::from_utf8_lossy(&seed.stdout),
            String::from_utf8_lossy(&seed.stderr)
        );
        let before = all_events(&state).last().map_or(0, |event| event.0);

        let mut child = Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/capture-codex.sh"
            ))
            .arg(base.0.join("out"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    base.0.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_STATE_DIR", &state)
            .env("LANGSPAN_AUTOSTART", "off")
            .env("CODEX_HOME", base.0.join("codex-home"))
            .current_dir(&base.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // The owner's Enter after trusting the project in Codex.
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        let run = child.wait_with_output().unwrap();
        let said = format!(
            "seeded {seeded}: {}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(run.status.success(), "{said}");

        // The fixtures are named for the Codex version that sent the events.
        assert_eq!(
            fs::read_to_string(base.0.join("out/codex-version.txt")).expect(&said),
            "codex-cli 0.0.0-standin\n",
            "{said}"
        );
        let saved = fs::read_to_string(base.0.join("out/events.txt")).expect(&said);
        let saved: Vec<&str> = saved.lines().collect();
        let first: u64 = saved
            .first()
            .and_then(|line| line.split_once(' '))
            .map(|(seq, _)| seq.parse().unwrap())
            .expect(&said);
        assert!(first > before, "{said}");
        // The daemon records events of its own (a workspace scan) at any
        // time, so the run's start is somewhere after `before`: everything
        // from the first saved event on is saved, and nothing this run wrote
        // comes before it.
        let everything = all_events(&state);
        let expected: Vec<&str> = everything
            .iter()
            .filter(|event| event.0 >= first)
            .map(|event| event.1.as_str())
            .collect();
        assert_eq!(saved, expected, "{said}");
        for (_, line) in everything.iter().filter(|e| e.0 > before && e.0 < first) {
            assert!(
                !line.contains("capture-") && !line.contains("\"standin.t"),
                "an event of this run is missing: {line}"
            );
        }
        assert!(!saved.iter().any(|line| line.contains("seed.s")), "{said}");
        assert_eq!(
            saved
                .iter()
                .filter(
                    |line| line.contains("\"session_registered\"") && line.contains("\"standin.t")
                )
                .count(),
            450,
            "{said}"
        );
        for own in ["capture-peer-", ".holder"] {
            assert!(saved.iter().any(|line| line.contains(own)), "{own}: {said}");
        }
    }
}
