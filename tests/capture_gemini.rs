#![cfg(unix)]
//! `scripts/capture-gemini.sh` refuses to run, and writes nothing, unless
//! `GEMINI_CLI_HOME` names an existing directory and Gemini CLI answers a
//! one-word prompt there (issue A6). Gemini CLI has no sign-in status
//! command. The `gemini` on `PATH` here is a stand-in for that one prompt
//! only: the real one needs an account, and the script stops before any
//! other command.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/capture-gemini.sh");

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A new directory holding a `gemini` that, asked a `-p` prompt in the
/// default approval mode, prints `output` and exits with `code`, and whose
/// every other command fails.
fn scratch(output: &str, code: i32) -> Scratch {
    let base = std::env::temp_dir().join(format!(
        "ls-capture-gemini-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(base.join("bin")).unwrap();
    let gemini = base.join("bin/gemini");
    fs::write(
        &gemini,
        format!(
            "#!/bin/sh\n[ \"$1 $2 $3\" = '--approval-mode default -p' ] || exit 99\necho '{output}'\nexit {code}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&gemini, fs::Permissions::from_mode(0o700)).unwrap();
    Scratch(base)
}

/// Run the script with `gemini_home` as `GEMINI_CLI_HOME`, or with it unset.
fn capture(base: &Path, gemini_home: Option<&Path>) -> Output {
    let path = format!(
        "{}:{}",
        base.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new("bash");
    command
        .arg(SCRIPT)
        .arg(base.join("out"))
        .env("PATH", path)
        .env("LANGSPAN", env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_STATE_DIR", base.join("state"))
        .env("LANGSPAN_AUTOSTART", "off")
        .current_dir(base);
    match gemini_home {
        Some(home) => command.env("GEMINI_CLI_HOME", home),
        None => command.env_remove("GEMINI_CLI_HOME"),
    };
    command.output().unwrap()
}

#[test]
fn the_gemini_capture_stops_and_writes_nothing_when_gemini_is_not_signed_in() {
    for (output, code) in [
        ("Please set an Auth method", 41),
        ("", 1),
        ("an answer without the word", 0),
    ] {
        let base = scratch(output, code);
        let home = base.0.join("gemini-home");
        fs::create_dir(&home).unwrap();
        let run = capture(&base.0, Some(&home));
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert_ne!(run.status.code(), Some(0), "{output:?} {code}: {stderr}");
        assert!(
            stderr.contains("gemini is not signed in"),
            "{output:?} {code}: {stderr}"
        );
        // Gemini's own answer is not echoed.
        if !output.is_empty() {
            assert!(!stderr.contains(output), "{output:?} {code}: {stderr}");
        }
        for written in ["out", "state"] {
            assert!(
                !base.0.join(written).exists(),
                "{output:?} {code}: wrote {written}"
            );
        }
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "{output:?} {code}");
    }
}

#[test]
fn the_gemini_capture_refuses_to_run_without_a_separate_gemini_home() {
    let base = scratch("ready", 0);
    let missing = base.0.join("no-such-home");
    for home in [None, Some(missing.as_path())] {
        let run = capture(&base.0, home);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert_ne!(run.status.code(), Some(0), "{home:?}: {stderr}");
        assert!(stderr.contains("set GEMINI_CLI_HOME"), "{home:?}: {stderr}");
        assert!(!base.0.join("out").exists(), "{home:?}");
        assert!(!base.0.join("state").exists(), "{home:?}");
        assert!(!missing.exists(), "{home:?}");
    }
}

/// Every Gemini CLI command the script runs is in the default approval mode,
/// with no option that approves a tool, allows one by policy or skips trust:
/// the person approves each edit by hand.
#[test]
fn the_gemini_capture_never_approves_automatically() {
    let script = fs::read_to_string(SCRIPT).unwrap();
    let code: Vec<&str> = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    for banned in [
        "auto_edit",
        "yolo",
        "--policy",
        "--allowed-tools",
        "--skip-trust",
        " -y ",
    ] {
        for line in &code {
            assert!(!line.contains(banned), "{banned:?} in: {line}");
        }
    }
    let runs: Vec<&&str> = code
        .iter()
        .filter(|line| line.contains("gemini --") || line.contains("gemini -p"))
        .collect();
    assert!(runs.len() >= 3, "{runs:?}");
    for line in runs {
        assert!(line.contains("gemini --approval-mode default"), "{line}");
    }
}
