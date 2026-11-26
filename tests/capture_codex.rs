#![cfg(unix)]
//! `scripts/capture-codex.sh` refuses to run, and writes nothing, unless
//! `codex login status` reports a signed-in account (issue A3). The `codex`
//! on `PATH` here is a stand-in for the real CLI's status command only: the
//! real one needs an account, and the script stops before any other command.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
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
