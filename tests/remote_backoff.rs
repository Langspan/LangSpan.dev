#![cfg(unix)]
//! How often a publishing coordinator goes back to a remote that fails, with
//! a real daemon, real Git and a loopback HTTP server or a local remote,
//! through LangSpan's remote passes (ADR 0006).
//!
//! Every failure to reach the remote backs off, deliberately, not only
//! refused credentials. A missing remote, an unreachable host and a refused credential all wait 5 s, then
//! 30 s, up to 10 minutes. What never backs off is a pass that reached the
//! remote and failed on what it read, such as a teammate's publication that
//! is not valid.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[allow(dead_code)] // Only the credential server is used here.
mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn git(root: &Path, args: &[&str]) -> String {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-backoff-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        let hub = base.join("hub.git");
        git(
            &base,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                hub.to_str().unwrap(),
            ],
        );
        let work = base.join("work");
        git(
            &base,
            &[
                "clone",
                "--quiet",
                hub.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        fs::write(work.join("a.txt"), "a\n").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "init"]);
        git(&work, &["push", "--quiet", "origin", "HEAD:main"]);
        // Only this repository's config decides credentials: no helper.
        git(&work, &["config", "credential.helper", ""]);
        // Every fetch from a local remote runs this first, which counts it,
        // whether or not the remote is there.
        let counter = base.join("upload-pack");
        fs::write(
            &counter,
            format!(
                "#!/bin/sh\necho run >> '{}'\nexec git-upload-pack \"$@\"\n",
                base.join("fetches").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&counter, fs::Permissions::from_mode(0o700)).unwrap();
        git(
            &work,
            &[
                "config",
                "remote.origin.uploadpack",
                counter.to_str().unwrap(),
            ],
        );
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let config = state.join("config.json");
        fs::write(&config, r#"{"remote_pass_ms":200}"#).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let mut world = Self { base, daemon: None };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(&state)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while world.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }
    fn enable(&self) {
        let root = self.work().to_str().unwrap().to_owned();
        let (code, text) = self.cli(&[
            "remote",
            "enable",
            "app",
            "--name",
            "Alice",
            "--root",
            &root,
            "--request-id",
            "en",
        ]);
        assert_eq!(code, 0, "{text}");
    }
    /// Fetches tried against a local remote so far.
    fn fetches(&self) -> usize {
        fs::read_to_string(self.base.join("fetches"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Passes run every 200 ms here. A remote that refuses credentials is asked
/// twice (the second failure starts the 5 s wait), then not again for 5 s,
/// and never with a credential: nothing prompts and no helper answers.
#[test]
fn refused_credentials_back_off_and_a_failure_on_what_the_remote_sent_does_not() {
    let w = World::new();
    let (url, seen) = support::asks_for_credentials();
    git(&w.work(), &["remote", "set-url", "origin", &url]);
    w.enable();
    std::thread::sleep(Duration::from_secs(3));
    let requests: Vec<bool> = seen.try_iter().collect();
    assert!(!requests.is_empty(), "the remote was asked");
    assert!(
        requests.len() <= 6,
        "two passes of at most a few requests each, not one every 200 ms: {}",
        requests.len()
    );
    assert!(
        requests.iter().all(|authorised| !authorised),
        "no credential was sent"
    );

    // A remote that answers, holding a teammate's publication that is not
    // valid (a 33-byte host name): every pass fails on it, and none waits.
    let w = World::new();
    let participant = "e".repeat(32);
    let state_json = format!(
        r#"{{"v":1,"publication":{{"participant":"{participant}","name":"Erin","sequence":1,"work":[{{"host":"{}","session":"c0ffee00","branch_ref":null,"summary":null,"editing":[],"recent":[]}}]}}}}"#,
        "h".repeat(33)
    );
    let hub = w.base.join("hub.git");
    let blob_file = w.base.join("state.json");
    fs::write(&blob_file, state_json).unwrap();
    let blob = git(&hub, &["hash-object", "-w", blob_file.to_str().unwrap()]);
    let tree = {
        let mut child = Process::new("git")
            .arg("-C")
            .arg(&hub)
            .arg("mktree")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("100644 blob {blob}\tstate.json\n").as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned()
    };
    let commit = git(&hub, &["commit-tree", &tree, "-m", "langspan"]);
    git(
        &hub,
        &[
            "update-ref",
            &format!("refs/langspan/peers/{participant}"),
            &commit,
        ],
    );
    w.enable();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        // Backing off would allow two in 3 s; load can slow passes, so the
        // bound is far from the 15 an idle machine runs.
        w.fetches() >= 3,
        "a pass that reached the remote does not wait: {} fetches in 3 s",
        w.fetches()
    );
}

/// The remote fails until it is fixed: the coordinator waits as it backs
/// off, and the first pass that reaches the remote clears the wait, so the
/// passes after it run on schedule again.
#[test]
fn a_background_remote_pass_never_prompts_and_a_successful_one_resets_the_back_off() {
    let w = World::new();
    let hub = w.base.join("hub.git");
    let missing = w.base.join("moved.git");
    git(
        &w.work(),
        &["remote", "set-url", "origin", missing.to_str().unwrap()],
    );
    w.enable();
    // Two failures, then a 5 s wait.
    std::thread::sleep(Duration::from_millis(1_500));
    let failed = w.fetches();
    assert!(
        (1..=3).contains(&failed),
        "two tries, then waiting: {failed}"
    );
    fs::rename(&hub, &missing).unwrap();
    let fixed = Instant::now();
    let deadline = fixed + Duration::from_secs(15);
    while w.fetches() == failed {
        assert!(
            Instant::now() < deadline,
            "the remote was never tried again"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let first = fixed.elapsed();
    assert!(
        first >= Duration::from_secs(2),
        "the fixed remote was reached only after the back-off: {first:?}"
    );
    let reached = w.fetches();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        w.fetches() >= reached + 2,
        "after a pass that worked, passes run every 200 ms again: {}",
        w.fetches() - reached
    );
}
