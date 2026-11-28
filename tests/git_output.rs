#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn successful_git_with_warnings_does_not_become_a_complete_observation() {
    let root = std::env::temp_dir().join(format!("langspan-git-warning-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .arg(&root)
        .status()
        .unwrap();
    assert!(status.success());
    // Git warns and omits this corrupt local ref while for-each-ref exits zero.
    fs::write(
        root.join(".git/refs/heads/private-warning-marker"),
        "not-an-object-id\n",
    )
    .unwrap();
    let result = langspan::git::output(
        &root,
        &["for-each-ref", "--format=%(refname)", "refs/heads/"],
        Instant::now() + Duration::from_secs(3),
        4096,
    );
    fs::remove_dir_all(&root).unwrap();
    let error = result.expect_err("Git warning must invalidate the complete snapshot");
    assert!(
        matches!(error, langspan::git::GitError::Failed(_)),
        "{error:?}"
    );
    let error = error.to_string();
    assert!(error.contains("warning"), "{error}");
    assert!(
        !error.contains("private-warning-marker"),
        "repository stderr must not be relayed"
    );
}

struct PromisorFixture(PathBuf);

impl Drop for PromisorFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn setup_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[test]
fn missing_promisor_objects_fail_without_launching_a_remote_helper() {
    let fixture = PromisorFixture(
        std::env::temp_dir().join(format!("langspan-promisor-fixture-{}", std::process::id())),
    );
    fs::create_dir(&fixture.0).unwrap();
    let root = fixture.0.join("repo");
    let bin = fixture.0.join("bin");
    let marker = fixture.0.join("remote-helper-called");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&bin).unwrap();
    setup_git(
        &root,
        &[
            "init",
            "--quiet",
            "--initial-branch=main",
            "--object-format=sha1",
        ],
    );
    for (key, value) in [
        ("core.repositoryformatversion", "1"),
        ("extensions.partialClone", "origin"),
        ("remote.origin.url", "langspanfixture::local-only"),
        ("remote.origin.promisor", "true"),
        ("remote.origin.partialclonefilter", "blob:none"),
        ("protocol.langspanfixture.allow", "always"),
    ] {
        setup_git(&root, &["config", key, value]);
    }
    let helper = bin.join("git-remote-langspanfixture");
    // This owned helper has no transport implementation and cannot access a
    // network. Its only side effect proves that Git tried a lazy fetch.
    fs::write(
        &helper,
        b"#!/bin/sh\nprintf invoked > \"$LANGSPAN_REMOTE_HELPER_MARKER\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "promisor_object_child"])
        .env("PATH", path)
        .env("LANGSPAN_PROMISOR_TEST_ROOT", &root)
        .env("LANGSPAN_REMOTE_HELPER_MARKER", &marker)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_NO_LAZY_FETCH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("local promisor fixture exceeded its subprocess deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[ignore = "subprocess fixture for isolated Git environment"]
fn promisor_object_child() {
    let Some(root) = std::env::var_os("LANGSPAN_PROMISOR_TEST_ROOT") else {
        return;
    };
    let marker = PathBuf::from(std::env::var_os("LANGSPAN_REMOTE_HELPER_MARKER").unwrap());
    let missing = "a".repeat(40);
    let raw = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(&root)
        .args(["cat-file", "blob", &missing])
        .env_remove("GIT_NO_LAZY_FETCH")
        .output()
        .unwrap();
    assert!(
        !raw.status.success(),
        "owned helper intentionally cannot fetch an object"
    );
    assert!(
        marker.exists(),
        "fixture must prove raw Git attempts a lazy fetch"
    );
    fs::remove_file(&marker).unwrap();

    let result = langspan::git::output(
        Path::new(&root),
        &["cat-file", "blob", &missing],
        Instant::now() + Duration::from_secs(1),
        4096,
    );
    assert!(
        result.is_err(),
        "missing local object must remain an explicit failure"
    );
    assert!(
        !marker.exists(),
        "read-only Git helper launched a promisor remote helper"
    );
}

#[test]
fn a_refusal_keeps_gits_last_lines_bounded_and_without_credentials() {
    let token = format!("ghp_{}", "A1b2C3d4E5".repeat(3));
    let errors = format!(
        "{}\nremote: error: refusing refs/langspan/peers/x\nerror: RPC failed; HTTP 403 curl 22 The requested URL returned error: 403\nfatal: unable to access 'https://alice:hunter2@github.com/o/r.git/': denied\n\x1b[31mtoken {token}\x1b[0m\n",
        "progress line\n".repeat(400)
    );
    let said = langspan::git::said(errors.as_bytes());
    assert!(
        said.len() <= langspan::git::SAID_LIMIT + 3,
        "{}",
        said.len()
    );
    assert!(said.contains("HTTP 403"), "{said}");
    assert!(said.contains("refusing refs/langspan/peers/x"), "{said}");
    assert!(
        said.contains("https://[redacted]@github.com/o/r.git/"),
        "{said}"
    );
    for secret in ["alice", "hunter2", &token, "\x1b"] {
        assert!(!said.contains(secret), "{secret} in {said}");
    }
    assert_eq!(langspan::git::said(b""), "");
    // A huge single line is cut from the front, on a character boundary.
    let long = "é".repeat(2000);
    let said = langspan::git::said(long.as_bytes());
    assert!(said.starts_with("...") && said.len() <= langspan::git::SAID_LIMIT + 3);
}
