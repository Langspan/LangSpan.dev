#![cfg(unix)]
//! `doctor` compares the version of the running coordinator with the version
//! recorded in the binary each installed hook names (roadmap I5, ADR 0025).
//! Two binaries of different versions and a real daemon: the hook binary is
//! a copy of this build whose version marker is rewritten in place. Doctor
//! never runs the binary it inspects, so the rewritten copy, whose code
//! signature no longer matches on macOS, does not need to run.

mod support;

use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    time::{Duration, Instant},
};

const PREFIX: &[u8] = b"langspan-binary-version=";
const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("ls-ver-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("home")).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let base = fs::canonicalize(base).unwrap();
        Self { base, daemon: None }
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn repo(&self) -> PathBuf {
        let root = self.base.join("app");
        fs::create_dir_all(&root).unwrap();
        assert!(
            Process::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        root
    }
    /// A copy of this build at `keg/bin/langspan`, as a package manager
    /// would lay it out.
    fn keg(&self) -> PathBuf {
        let bin = self.base.join("keg/bin");
        fs::create_dir_all(&bin).unwrap();
        let copy = bin.join("langspan");
        fs::copy(env!("CARGO_BIN_EXE_langspan"), &copy).unwrap();
        copy
    }
    fn start(&mut self) {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut command, &self.base.join("home"));
        self.daemon = Some(
            command
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.state_dir().join("daemon.sock").exists()
            || langspan::client::status_from(&self.state_dir(), &[]).is_err()
        {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// `doctor --json` for `root`: its exit code and findings.
    fn doctor(&self, root: &Path) -> (Option<i32>, Vec<(bool, String)>) {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut command, &self.base.join("home"));
        let output = command
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["doctor", "--json", "--root"])
            .arg(root)
            .arg("--home")
            .arg(self.base.join("home"))
            .arg("--state-dir")
            .arg(self.state_dir())
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("doctor --json: {output:?}"));
        let findings = report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| {
                (
                    finding["ok"].as_bool().unwrap(),
                    finding["detail"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        (output.status.code(), findings)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Rewrite the version in `binary`'s marker from `from` to `to`, the same
/// length.
fn rewrite_version(binary: &Path, from: &str, to: &str) {
    assert_eq!(to.len(), from.len());
    let mut bytes = fs::read(binary).unwrap();
    let mut needle = PREFIX.to_vec();
    needle.extend_from_slice(from.as_bytes());
    needle.push(0);
    let at: Vec<usize> = bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle.as_slice())
        .map(|(at, _)| at)
        .collect();
    assert_eq!(at.len(), 1, "the binary records version {from} once");
    let start = at[0] + PREFIX.len();
    bytes[start..start + to.len()].copy_from_slice(to.as_bytes());
    // A new file, as an upgrade writes one: never the inode a process maps.
    let temporary = binary.with_extension("new");
    fs::write(&temporary, bytes).unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(temporary, binary).unwrap();
}

/// A version of the same length that orders after this one.
fn newer() -> String {
    let (major, rest) = VERSION.split_once('.').unwrap();
    let major: u32 = major.parse().unwrap();
    let newer = format!("{}.{rest}", major + 1);
    assert_eq!(newer.len(), VERSION.len(), "pick another rewrite");
    newer
}

/// The same length, every digit zero: older unless this is 0.0.0.
fn older() -> String {
    let older: String = VERSION
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    assert_ne!(older, VERSION);
    older
}

fn version_line<'a>(findings: &'a [(bool, String)], binary: &Path) -> &'a (bool, String) {
    let named = format!("{}, version", binary.display());
    findings
        .iter()
        .find(|(_, text)| text.starts_with("Hooks (claude-code)") && text.contains(&named))
        .unwrap_or_else(|| panic!("no version finding for {}: {findings:#?}", binary.display()))
}

#[test]
fn doctor_names_the_coordinator_and_hook_versions_and_fails_when_they_differ() {
    let mut fixture = Fixture::new("d");
    let root = fixture.repo();
    let keg = fixture.keg();
    let installed = support::output_once_not_busy(|| {
        let mut command = Process::new(&keg);
        support::in_home(&mut command, &fixture.base.join("home"));
        command
            .args([
                "hooks",
                "install",
                "claude-code",
                "--workspace",
                "app",
                "--root",
            ])
            .arg(&root)
            .arg("--apply")
            .arg("--state-dir")
            .arg(fixture.state_dir());
        command
    });
    assert!(installed.status.success(), "{installed:?}");
    fixture.start();

    // The same version: reported, nothing to fix about it.
    let (_, findings) = fixture.doctor(&root);
    assert!(
        findings.iter().any(|(ok, text)| *ok
            && text.starts_with(&format!("Coordinator version {VERSION}, process "))),
        "{findings:#?}"
    );
    let (ok, text) = version_line(&findings, &keg);
    assert!(ok, "{text}");
    assert!(text.contains(&format!("version {VERSION}")), "{text}");

    // The hooks' binary is upgraded; the coordinator still runs the old one.
    let newer = newer();
    rewrite_version(&keg, VERSION, &newer);
    let (code, findings) = fixture.doctor(&root);
    assert_eq!(code, Some(1), "{findings:#?}");
    let (ok, text) = version_line(&findings, &keg);
    assert!(!ok, "{text}");
    assert!(text.contains(&format!("version {newer}")), "{text}");
    assert!(
        text.contains(&format!(
            "the coordinator, version {VERSION}, is older than this binary; restart it"
        )),
        "{text}"
    );

    // The reverse: the hooks name a binary older than the coordinator.
    let older = older();
    rewrite_version(&keg, &newer, &older);
    let (code, findings) = fixture.doctor(&root);
    assert_eq!(code, Some(1), "{findings:#?}");
    let (ok, text) = version_line(&findings, &keg);
    assert!(!ok, "{text}");
    assert!(
        text.contains(&format!(
            "version {older}; the coordinator, version {VERSION}, is newer than this binary"
        )),
        "{text}"
    );
}

#[test]
fn doctor_says_when_a_hook_binary_records_no_version() {
    let mut fixture = Fixture::new("u");
    let root = fixture.repo();
    let keg = fixture.keg();
    let installed = support::output_once_not_busy(|| {
        let mut command = Process::new(&keg);
        support::in_home(&mut command, &fixture.base.join("home"));
        command
            .args([
                "hooks",
                "install",
                "claude-code",
                "--workspace",
                "app",
                "--root",
            ])
            .arg(&root)
            .arg("--apply")
            .arg("--state-dir")
            .arg(fixture.state_dir());
        command
    });
    assert!(installed.status.success(), "{installed:?}");
    fixture.start();
    // A build from before the marker: the same file without it.
    let mut bytes = fs::read(&keg).unwrap();
    let at = bytes
        .windows(PREFIX.len())
        .position(|window| window == PREFIX)
        .unwrap();
    bytes[at] = b'L';
    fs::write(&keg, bytes).unwrap();
    let (code, findings) = fixture.doctor(&root);
    assert_eq!(code, Some(1), "{findings:#?}");
    let text = findings
        .iter()
        .find(|(_, text)| text.starts_with("Hooks (claude-code)"))
        .unwrap_or_else(|| panic!("{findings:#?}"));
    assert!(!text.0, "{}", text.1);
    assert!(text.1.contains("does not record its version"), "{}", text.1);
}
