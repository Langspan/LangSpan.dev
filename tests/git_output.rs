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

/// A temporary directory for one test in this process, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("langspan-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// A repository holding `a.rs` and `src/b.rs` in one commit.
    fn repo(test: &str) -> Self {
        let scratch = Self::new(test);
        setup_git(&scratch.0, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(scratch.0.join("a.rs"), "fn a() {}\n").unwrap();
        fs::create_dir(scratch.0.join("src")).unwrap();
        fs::write(scratch.0.join("src/b.rs"), "fn b() {}\n").unwrap();
        commit(&scratch.0);
        scratch
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn commit(root: &Path) {
    setup_git(root, &["add", "--all"]);
    setup_git(
        root,
        &[
            "-c",
            "user.name=LangSpan Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "--message=fixture",
        ],
    );
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

// Git verified: how `git config` lists filter keys, and how it fails on a
// configuration file it cannot parse.
#[test]
fn filter_overrides_refuses_a_configuration_it_cannot_read_whole() {
    use langspan::git::{GitError, filter_overrides};
    let scratch = Scratch::repo("filter-overrides");
    let root = &scratch.0;
    setup_git(
        root,
        &["config", "filter.private.clean", "secret-cleaner %f"],
    );
    let arguments = filter_overrides(root, soon()).unwrap();
    for setting in ["clean=", "smudge=", "process=", "required=false"] {
        let wanted = format!("filter.private.{setting}");
        assert!(arguments.contains(&wanted), "{wanted} in {arguments:?}");
    }
    assert!(!arguments.iter().any(|a| a.contains("secret-cleaner")));

    // A key with no driver name between `filter.` and the setting.
    setup_git(root, &["config", "filter.nodriver", "1"]);
    let error = filter_overrides(root, soon()).unwrap_err();
    assert!(matches!(error, GitError::Corrupt(_)), "{error:?}");
    assert!(error.to_string().contains("malformed"), "{error}");
    setup_git(root, &["config", "--unset", "filter.nodriver"]);

    let mut config = fs::read_to_string(root.join(".git/config")).unwrap();
    for n in 0..64 {
        config.push_str(&format!("[filter \"n{n}\"]\n\tclean = cat\n"));
    }
    fs::write(root.join(".git/config"), &config).unwrap();
    let error = filter_overrides(root, soon()).unwrap_err();
    assert!(matches!(error, GitError::Corrupt(_)), "{error:?}");
    assert!(error.to_string().contains("more than 64"), "{error}");

    config.push_str("[unterminated\n");
    fs::write(root.join(".git/config"), &config).unwrap();
    let error = filter_overrides(root, soon()).unwrap_err();
    assert!(matches!(error, GitError::Failed(_)), "{error:?}");
    assert!(error.to_string().contains("could not list"), "{error}");
}

// Git verified: what `rev-parse HEAD:path`, `hash-object` and `merge-base`
// answer for each kind of worktree path, and the file system's answer for a
// path under a regular file.
#[test]
fn standing_names_each_path_it_cannot_judge() {
    use langspan::git::{GitError, standing};
    use langspan_core::Standing;
    let scratch = Scratch::repo("standing-errors");
    let root = &scratch.0;
    let missing = root.join("not-a-directory");

    let error = standing(root, "-a.rs", root, soon()).unwrap_err();
    assert!(matches!(error, GitError::Invalid(_)), "{error:?}");
    assert!(error.to_string().contains("worktree-relative"), "{error}");

    let error = standing(&missing, "a.rs", root, soon()).unwrap_err();
    assert!(matches!(error, GitError::Failed(_)), "{error:?}");
    assert!(error.to_string().contains("HEAD"), "{error}");

    let error = standing(root, "src", root, soon()).unwrap_err();
    assert!(matches!(error, GitError::Invalid(_)), "{error:?}");
    assert!(error.to_string().contains("not a regular file"), "{error}");

    let error = standing(root, "a.rs/x", root, soon()).unwrap_err();
    assert!(matches!(error, GitError::Io(_)), "{error:?}");
    assert!(error.to_string().contains("cannot read"), "{error}");

    let error = standing(root, "a.rs", &missing, soon()).unwrap_err();
    assert!(matches!(error, GitError::Failed(_)), "{error:?}");
    assert!(error.to_string().contains("compare the commit"), "{error}");

    assert_eq!(standing(root, "a.rs", root, soon()), Ok(Standing::Merged));
    fs::write(root.join("new.rs"), "fn new() {}\n").unwrap();
    assert_eq!(
        standing(root, "new.rs", root, soon()),
        Ok(Standing::Uncommitted)
    );
    fs::remove_file(root.join("a.rs")).unwrap();
    assert_eq!(
        standing(root, "a.rs", root, soon()),
        Ok(Standing::Uncommitted)
    );
}

// Git verified: `core.autocrlf` stores LF while the file on disk keeps CRLF.
#[test]
fn standing_treats_a_crlf_checkout_of_the_committed_file_as_committed() {
    use langspan::git::standing;
    use langspan_core::Standing;
    let scratch = Scratch::repo("standing-crlf");
    let root = &scratch.0;
    setup_git(root, &["config", "core.autocrlf", "true"]);
    fs::write(root.join("c.rs"), "fn c() {}\r\nfn d() {}\r\n").unwrap();
    commit(root);
    let blob = langspan::git::output(root, &["cat-file", "blob", "HEAD:c.rs"], soon(), 4096);
    assert_eq!(blob.unwrap(), b"fn c() {}\nfn d() {}\n", "Git stores LF");
    assert_eq!(standing(root, "c.rs", root, soon()), Ok(Standing::Merged));

    // Same length once normalized, different content.
    fs::write(root.join("c.rs"), "fn c() {}\r\nfn e() {}\r\n").unwrap();
    assert_eq!(
        standing(root, "c.rs", root, soon()),
        Ok(Standing::Uncommitted)
    );
}

// Git verified: a command whose standard error runs far past 8 KiB.
#[test]
fn run_with_errors_keeps_only_the_end_of_a_long_standard_error() {
    let scratch = Scratch::repo("stderr-tail");
    let paths: Vec<String> = (0..1000).map(|n| format!("missing-{n:04}.rs")).collect();
    let mut args = vec!["ls-files", "--error-unmatch", "--"];
    args.extend(paths.iter().map(String::as_str));
    let (status, _, errors) =
        langspan::git::run_with_errors(&scratch.0, &args, &[], soon(), 4096).unwrap();
    assert!(!status.success());
    assert_eq!(errors.len(), 8192);
    let errors = String::from_utf8_lossy(&errors);
    assert!(!errors.contains("missing-0000.rs"), "the start is dropped");
    assert!(errors.contains("missing-0999.rs"), "the end is kept");
}

#[test]
fn said_cuts_a_long_line_on_a_character_boundary() {
    // 3-byte characters, so the first byte kept falls inside one.
    let said = langspan::git::said("€".repeat(1000).as_bytes());
    assert_eq!(said, format!("...{}", "€".repeat(341)));
}

#[test]
fn remote_refusals_and_participant_ids_say_what_failed() {
    use langspan::remote::{Refusal, participant_id, refused_withdrawal};
    for refusal in [
        Refusal::from("denied"),
        Refusal::from(String::from("denied")),
    ] {
        assert_eq!(refusal.detail, "denied");
        assert!(!refusal.permanent);
    }
    let refusal = Refusal::from(langspan::git::GitError::TimedOut);
    assert_eq!(refusal.detail, "Git operation deadline elapsed");
    assert!(!refusal.permanent);

    let scratch = Scratch::new("participant-id");
    fs::write(scratch.0.join("participant"), "not-hexadecimal\n").unwrap();
    let error = participant_id(&scratch.0).unwrap_err().to_string();
    assert!(error.contains("32-character hexadecimal"), "{error}");

    let link = langspan_core::RemoteLink {
        workspace_id: "w".into(),
        remote: "origin".into(),
        participant: "p".into(),
        name: "n".into(),
        share_task: false,
        sequence: 1,
        signature: String::new(),
        enabled: false,
        withdrawn: false,
        publish_as_branch: false,
        share_messages: false,
    };
    let message = refused_withdrawal(&link, "refs/langspan/peers/p", "", false);
    assert!(
        message.contains("teammates still see your last one"),
        "{message}"
    );
    assert!(!message.contains("Git said"), "{message}");
}
