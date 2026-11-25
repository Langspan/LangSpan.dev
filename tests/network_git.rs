#![cfg(unix)]
//! Git commands that talk to a remote: bounded by their deadline together
//! with everything they start, and never waiting for a person to answer.
//! The remote here is a loopback HTTP listener or Git's `ext::` transport;
//! nothing leaves the machine.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

mod support;
use support::{alive, asks_for_credentials};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Repository(PathBuf);

impl Repository {
    fn new(name: &str) -> Self {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-net-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        Self(root)
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_hanging_credential_helper_is_killed_with_its_process_group_at_the_deadline() {
    let repository = Repository::new("hang");
    let (url, _) = asks_for_credentials();
    let pid_file = repository.0.join("helper.pid");
    let helper = repository.script(
        "helper",
        &format!("echo $$ > '{}'\nexec sleep 60", pid_file.display()),
    );
    // Only this repository's own config: the empty value clears any helper
    // from the system or global config first.
    git(&repository.0, &["config", "credential.helper", ""]);
    git(
        &repository.0,
        &[
            "config",
            "--add",
            "credential.helper",
            &format!("!{}", helper.display()),
        ],
    );
    git(&repository.0, &["remote", "add", "origin", &url]);
    let started = Instant::now();
    let result = langspan::git::run_network(
        &repository.0,
        &["fetch", "--quiet", "origin"],
        started + Duration::from_secs(3),
    );
    let elapsed = started.elapsed();
    let error = result.expect_err("the fetch must fail at its deadline");
    assert!(error.contains("deadline"), "{error}");
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");
    let pid: i32 = fs::read_to_string(&pid_file)
        .expect("the helper ran")
        .trim()
        .parse()
        .unwrap();
    // Killed processes are reaped by init; give that a bounded moment.
    let gone_by = Instant::now() + Duration::from_secs(10);
    while alive(pid) && Instant::now() < gone_by {
        std::thread::sleep(Duration::from_millis(20));
    }
    let survived = alive(pid);
    if survived {
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::Signal::KILL,
        );
    }
    assert!(!survived, "credential helper {pid} outlived the deadline");
}

#[test]
fn a_working_credential_helper_still_runs_and_nothing_prompts() {
    let repository = Repository::new("helper");
    let (url, seen) = asks_for_credentials();
    let called = repository.0.join("called");
    let helper = repository.script(
        "helper",
        &format!(
            "echo \"$1\" >> '{}'\n[ \"$1\" = get ] && printf 'username=u\\npassword=p\\n'\nexit 0",
            called.display()
        ),
    );
    git(&repository.0, &["config", "credential.helper", ""]);
    git(
        &repository.0,
        &[
            "config",
            "--add",
            "credential.helper",
            &format!("!{}", helper.display()),
        ],
    );
    git(&repository.0, &["remote", "add", "origin", &url]);
    let status = langspan::git::run_network(
        &repository.0,
        &["fetch", "--quiet", "origin"],
        Instant::now() + Duration::from_secs(20),
    )
    .expect("Git finishes on its own");
    assert!(!status.success(), "the server refuses every request");
    let calls = fs::read_to_string(&called).expect("the helper was consulted");
    assert!(calls.lines().any(|line| line == "get"), "{calls}");
    assert!(
        seen.try_iter().any(|authorised| authorised),
        "the helper's answer was used for a request"
    );
}

/// The environment Git gives a transport command, observed through Git's
/// `ext::` transport, which runs a local command instead of connecting.
fn transport_environment(repository: &Repository) -> String {
    let out = repository.0.join("env.txt");
    let remote = format!("ext::sh -c env% >{}", out.display());
    let _ = langspan::git::run_network(
        &repository.0,
        &[
            "-c",
            "protocol.ext.allow=always",
            "fetch",
            "--quiet",
            &remote,
        ],
        Instant::now() + Duration::from_secs(20),
    );
    fs::read_to_string(out).expect("the transport command ran")
}

#[test]
fn network_git_never_prompts_and_ssh_runs_in_batch_mode() {
    let repository = Repository::new("env");
    let environment = transport_environment(&repository);
    assert!(
        environment
            .lines()
            .any(|line| line == "GIT_TERMINAL_PROMPT=0"),
        "{environment}"
    );
    assert!(
        environment
            .lines()
            .any(|line| line == "GIT_ASKPASS=/usr/bin/false"),
        "{environment}"
    );
    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        assert!(
            environment
                .lines()
                .any(|line| line == "GIT_SSH_COMMAND=ssh -o BatchMode=yes"),
            "{environment}"
        );
    }
}

#[test]
fn the_owners_own_ssh_command_is_respected() {
    let repository = Repository::new("own-ssh");
    git(
        &repository.0,
        &["config", "core.sshCommand", "my-ssh -i key"],
    );
    let environment = transport_environment(&repository);
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        assert!(
            !environment
                .lines()
                .any(|line| line.starts_with("GIT_SSH_COMMAND=")),
            "core.sshCommand must stay in charge: {environment}"
        );
    }
}
