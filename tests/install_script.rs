//! Runs `scripts/install.sh` against a local release. A stand-in `curl`
//! serves the release folder for the script's `https://github.com/...` URLs
//! and refuses any call without `--proto =https`; a stand-in `uname` reports
//! the platform under test. Real `sh`, `awk`, `tar` and checksum tool.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RELEASES: &str = "https://github.com/Langspan/LangSpan.dev/releases";
/// The one-line install the README and the install guide give.
const ONE_LINE: &str =
    "curl -fsSL https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh | sh";

const PLATFORMS: [(&str, &str, &str); 4] = [
    ("Darwin", "arm64", "aarch64-apple-darwin"),
    ("Darwin", "x86_64", "x86_64-apple-darwin"),
    ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
    ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
];

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The machine's SHA-256 tool, as the script would pick it.
fn sha256(path: &Path) -> String {
    let output = if Command::new("sha256sum").arg("--version").output().is_ok() {
        Command::new("sha256sum").arg(path).output().unwrap()
    } else {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
            .unwrap()
    };
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()[..64].to_owned()
}

/// A release folder per version (`latest` and `v9.9.8`), stand-in tools,
/// a home holding an earlier install, and a temporary folder for the script.
struct Fixture {
    base: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("langspan-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let fixture = Self { base };
        for dir in ["stubs", "home/.local/bin", "tmp", "stage"] {
            fs::create_dir_all(fixture.base.join(dir)).unwrap();
        }
        fixture.publish("latest", "v9.9.9");
        fixture.publish("v9.9.8", "v9.9.8");
        write_executable(
            &fixture.base.join("home/.local/bin/langspan"),
            "#!/bin/sh\necho earlier\n",
        );
        let served = fixture.base.join("release");
        write_executable(
            &fixture.base.join("stubs/curl"),
            &format!(
                r#"#!/bin/sh
proto= out= url=
while [ "$#" -gt 0 ]; do
	case "$1" in
	--proto) proto=$2; shift ;;
	-o) out=$2; shift ;;
	--connect-timeout | --max-time | --max-filesize) shift ;;
	-*) ;;
	*) url=$1 ;;
	esac
	shift
done
echo "$url" >> "$HOME/../curl.log"
[ "$proto" = "=https" ] || {{ echo "stub curl: no --proto =https" >&2; exit 2; }}
case "$url" in
{RELEASES}/latest/download/*) file="{served}/latest/${{url#{RELEASES}/latest/download/}}" ;;
{RELEASES}/download/*) file="{served}/${{url#{RELEASES}/download/}}" ;;
*) echo "stub curl: unexpected URL $url" >&2; exit 2 ;;
esac
[ -f "$file" ] || {{ echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; }}
cat "$file" > "$out"
"#,
                served = served.display()
            ),
        );
        // A stand-in GitHub CLI, signed out unless the fixture writes a
        // `gh-mode` of `verifies` or `rejects`, or `other-expired`: signed in
        // to github.com, with another account's sign-in expired, so only a
        // status check scoped to the active github.com account passes, as
        // with the real gh. It needs the archive to exist.
        write_executable(
            &fixture.base.join("stubs/gh"),
            r#"#!/bin/sh
echo "$*" >> "$HOME/../gh.log"
mode=$(cat "$HOME/../gh-mode" 2>/dev/null || echo signed-out)
case "$1 $mode" in
"auth signed-out") echo "You are not logged into any GitHub hosts." >&2; exit 1 ;;
"auth other-expired")
	[ "$*" = "auth status --active --hostname github.com" ] && exit 0
	echo "X Failed to log in to github.example.com account old-login" >&2; exit 1 ;;
"auth "*) exit 0 ;;
"attestation verifies" | "attestation other-expired") [ -f "$3" ] && echo "Verification succeeded!" ;;
"attestation "*) echo "stub gh: no attestation matched" >&2; exit 1 ;;
*) exit 2 ;;
esac
"#,
        );
        write_executable(
            &fixture.base.join("stubs/uname"),
            "#!/bin/sh\ncase \"$1\" in -s) echo \"$FAKE_OS\" ;; -m) echo \"$FAKE_ARCH\" ;; esac\n",
        );
        fixture
    }

    /// An archive per target, laid out as the release workflow packs it,
    /// and `SHA256SUMS` listing them with an SBOM each.
    fn publish(&self, folder: &str, version: &str) {
        let release = self.base.join("release").join(folder);
        fs::create_dir_all(&release).unwrap();
        let mut sums = String::new();
        for (_, _, target) in PLATFORMS {
            let stem = format!("langspan-{version}-{target}");
            let dir = self.base.join("stage").join(&stem);
            fs::create_dir_all(&dir).unwrap();
            write_executable(
                &dir.join("langspan"),
                &format!("#!/bin/sh\necho \"$*\" >> \"$HOME/ran\"\necho {stem}\n"),
            );
            fs::write(dir.join("LICENSE"), "licence\n").unwrap();
            let archive = release.join(format!("{stem}.tar.gz"));
            let packed = Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(self.base.join("stage"))
                .arg(&stem)
                .status()
                .unwrap();
            assert!(packed.success());
            sums.push_str(&format!("{}  {stem}.tar.gz\n", sha256(&archive)));
            sums.push_str(&format!("{}  {stem}.cdx.json\n", "1".repeat(64)));
        }
        fs::write(release.join("SHA256SUMS"), sums).unwrap();
    }

    fn release(&self, relative: &str) -> PathBuf {
        self.base.join("release").join(relative)
    }

    fn run(&self, os: &str, arch: &str, args: &[&str], path_has_bin: bool) -> Output {
        let stubs = self.base.join("stubs").display().to_string();
        let path = if path_has_bin {
            format!(
                "{}:{stubs}:/usr/bin:/bin",
                self.base.join("home/.local/bin").display()
            )
        } else {
            format!("{stubs}:/usr/bin:/bin")
        };
        self.run_args_path(os, arch, args, &path)
    }

    fn run_with_path(&self, os: &str, arch: &str, path: &str) -> Output {
        self.run_args_path(os, arch, &[], path)
    }

    fn run_args_path(&self, os: &str, arch: &str, args: &[&str], path: &str) -> Output {
        Command::new("/bin/sh")
            .arg(repo("scripts/install.sh"))
            .args(args)
            .env_clear()
            .env("HOME", self.base.join("home"))
            .env("SHELL", "/bin/zsh")
            .env("PATH", path)
            .env("TMPDIR", self.base.join("tmp"))
            .env("FAKE_OS", os)
            .env("FAKE_ARCH", arch)
            .output()
            .unwrap()
    }

    fn gh_calls(&self) -> Vec<String> {
        fs::read_to_string(self.base.join("gh.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn installed(&self) -> String {
        fs::read_to_string(self.base.join("home/.local/bin/langspan")).unwrap()
    }

    fn downloads(&self) -> Vec<String> {
        fs::read_to_string(self.base.join("curl.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Nothing ran, the earlier install is intact and no temporary file is left.
    fn assert_nothing_changed(&self, output: &Output, says: &str) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "the script must fail: {stderr}");
        assert!(stderr.contains(says), "stderr lacks {says:?}: {stderr}");
        assert!(output.stdout.is_empty(), "nothing printed as a success");
        assert_eq!(self.installed(), "#!/bin/sh\necho earlier\n");
        self.assert_clean();
    }

    fn assert_clean(&self) {
        assert!(
            !self.base.join("home/ran").exists(),
            "langspan must not run"
        );
        let left: Vec<_> = fs::read_dir(self.base.join("tmp")).unwrap().collect();
        assert!(left.is_empty(), "temporary files left: {left:?}");
        let stray = fs::read_dir(self.base.join("home/.local/bin"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != "langspan")
            .count();
        assert_eq!(stray, 0, "a partial copy is left in ~/.local/bin");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn installs_the_latest_release_for_each_platform_without_running_it() {
    for (os, arch, target) in PLATFORMS {
        let fixture = Fixture::new(&format!("latest-{target}"));
        let output = fixture.run(os, arch, &[], false);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{target}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stem = format!("langspan-v9.9.9-{target}");
        assert!(fixture.installed().contains(&stem), "{target}");
        let mode = fs::metadata(fixture.base.join("home/.local/bin/langspan"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "{target}");
        assert_eq!(
            fixture.downloads(),
            [
                format!("{RELEASES}/latest/download/SHA256SUMS"),
                format!("{RELEASES}/latest/download/{stem}.tar.gz"),
            ]
        );
        for next in [
            "export PATH=\"$HOME/.local/bin:$PATH\"",
            ">> ~/.zshrc",
            "langspan setup",
            "langspan demo",
        ] {
            assert!(
                stdout.contains(next),
                "{target}: stdout lacks {next:?}: {stdout}"
            );
        }
        fixture.assert_clean();
        assert!(
            !fixture.base.join("home/.zshrc").exists(),
            "no startup file is changed"
        );
    }
}

#[test]
fn installs_a_pinned_version_with_or_without_its_v() {
    for version in ["v9.9.8", "9.9.8"] {
        let fixture = Fixture::new(&format!("pinned-{version}"));
        let output = fixture.run("Linux", "x86_64", &[version], true);
        assert!(output.status.success(), "{version}");
        assert!(
            fixture
                .installed()
                .contains("langspan-v9.9.8-x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            fixture.downloads()[0],
            format!("{RELEASES}/download/v9.9.8/SHA256SUMS")
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("export PATH"),
            "no PATH step when ~/.local/bin is on PATH: {stdout}"
        );
        assert!(stdout.contains("1. Close Claude Code, then connect your agents: langspan setup"));
    }
}

#[test]
fn a_checksum_mismatch_installs_nothing() {
    let fixture = Fixture::new("mismatch");
    let sums = fixture.release("latest/SHA256SUMS");
    let text = fs::read_to_string(&sums).unwrap();
    let digest = sha256(&fixture.release("latest/langspan-v9.9.9-x86_64-unknown-linux-gnu.tar.gz"));
    fs::write(&sums, text.replace(&digest, &"0".repeat(64))).unwrap();
    let output = fixture.run("Linux", "x86_64", &[], false);
    fixture.assert_nothing_changed(&output, "does not match SHA256SUMS; nothing was installed");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Run the installer again"));
}

const SIGNER: &str = "Langspan/LangSpan.dev/.github/workflows/release.yml";

#[test]
fn a_signed_in_gh_verifies_the_archive_before_it_is_installed() {
    let fixture = Fixture::new("gh-verifies");
    fs::write(fixture.base.join("gh-mode"), "verifies").unwrap();
    let output = fixture.run("Linux", "x86_64", &[], false);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.gh_calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0], "auth status --active --hostname github.com");
    // The downloaded copy in the script's temporary folder (macOS's mktemp
    // ignores TMPDIR, so only its absolute path and name are checked).
    let verify = &calls[1];
    assert!(verify.starts_with("attestation verify /"), "{verify}");
    assert!(
        verify.ends_with(&format!(
            "/langspan-v9.9.9-x86_64-unknown-linux-gnu.tar.gz -R Langspan/LangSpan.dev --signer-workflow {SIGNER}"
        )),
        "{verify}"
    );
    assert!(
        stdout.contains(&format!(
            "Provenance verified by gh attestation verify: built by {SIGNER}"
        )),
        "{stdout}"
    );
    let digest = sha256(&fixture.release("latest/langspan-v9.9.9-x86_64-unknown-linux-gnu.tar.gz"));
    assert!(
        stdout.contains(&format!("SHA-256 matches SHA256SUMS: {digest}")),
        "{stdout}"
    );
    fixture.assert_clean();
}

#[test]
fn another_accounts_expired_sign_in_does_not_skip_the_check() {
    let fixture = Fixture::new("gh-other-expired");
    fs::write(fixture.base.join("gh-mode"), "other-expired").unwrap();
    let output = fixture.run("Linux", "x86_64", &[], false);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.gh_calls();
    assert_eq!(calls.len(), 2, "the attestation is checked: {calls:?}");
    assert!(calls[1].starts_with("attestation verify /"), "{calls:?}");
    assert!(
        stdout.contains(&format!(
            "Provenance verified by gh attestation verify: built by {SIGNER}"
        )),
        "{stdout}"
    );
    fixture.assert_clean();
}

#[test]
fn an_attestation_gh_rejects_installs_nothing() {
    let fixture = Fixture::new("gh-rejects");
    fs::write(fixture.base.join("gh-mode"), "rejects").unwrap();
    let output = fixture.run("Linux", "x86_64", &[], false);
    fixture.assert_nothing_changed(
        &output,
        &format!("gh attestation verify did not confirm that {SIGNER} built langspan-v9.9.9-x86_64-unknown-linux-gnu.tar.gz; nothing was installed."),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stub gh: no attestation matched"),
        "gh's reason is shown: {stderr}"
    );
    assert!(stderr.contains("update it"), "{stderr}");
}

#[test]
fn without_a_signed_in_gh_the_install_says_provenance_was_not_checked() {
    let fixture = Fixture::new("gh-signed-out");
    let output = fixture.run("Linux", "x86_64", &[], false);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert_eq!(
        fixture.gh_calls(),
        ["auth status --active --hostname github.com"],
        "no verify when signed out"
    );
    assert!(
        stdout.contains("Provenance not checked: gh is not signed in to github.com, or is older than 2.49 (run gh auth login, or update gh)"),
        "{stdout}"
    );

    // No gh on PATH at all: a folder of links to the tools the script and
    // the stand-ins use, so a gh in /usr/bin isn't found either.
    let fixture = Fixture::new("gh-absent");
    let tools = fixture.base.join("no-gh");
    fs::create_dir_all(&tools).unwrap();
    for stub in ["curl", "uname"] {
        std::os::unix::fs::symlink(fixture.base.join("stubs").join(stub), tools.join(stub))
            .unwrap();
    }
    for tool in [
        "awk",
        "tar",
        "gzip",
        "mktemp",
        "dirname",
        "basename",
        "sha256sum",
        "shasum",
        "rm",
        "mkdir",
        "cp",
        "chmod",
        "mv",
        "cat",
        "head",
        "sleep",
    ] {
        if let Some(found) = ["/usr/bin", "/bin"]
            .iter()
            .map(|dir| Path::new(dir).join(tool))
            .find(|path| path.exists())
        {
            std::os::unix::fs::symlink(found, tools.join(tool)).unwrap();
        }
    }
    let output = fixture.run_with_path("Linux", "x86_64", &tools.display().to_string());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.gh_calls().is_empty());
    assert!(
        stdout.contains("Provenance not checked: the GitHub CLI (gh) is not installed"),
        "{stdout}"
    );
    fixture.assert_clean();
}

#[test]
fn an_unknown_platform_downloads_nothing() {
    for (os, arch) in [
        ("Linux", "riscv64"),
        ("FreeBSD", "amd64"),
        ("Darwin", "ppc"),
    ] {
        let fixture = Fixture::new(&format!("unknown-{os}-{arch}"));
        let output = fixture.run(os, arch, &[], false);
        fixture.assert_nothing_changed(
            &output,
            &format!("no LangSpan release for this platform ({os} {arch})"),
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("build-from-source"));
        assert!(fixture.downloads().is_empty(), "{os} {arch}");
    }
}

#[test]
fn a_failed_download_installs_nothing() {
    type Break = fn(&Fixture);
    let cases: [(&str, &[&str], Break, &str); 5] = [
        (
            "no-sums",
            &[],
            |f| fs::remove_file(f.release("latest/SHA256SUMS")).unwrap(),
            "could not download",
        ),
        (
            "no-archive",
            &[],
            |f| {
                fs::remove_file(f.release("latest/langspan-v9.9.9-x86_64-unknown-linux-gnu.tar.gz"))
                    .unwrap()
            },
            "could not download",
        ),
        ("no-version", &["v1.0.0"], |_| {}, "could not download"),
        (
            "not-listed",
            &[],
            |f| fs::write(f.release("latest/SHA256SUMS"), "garbage\n").unwrap(),
            "lists no single archive for x86_64-unknown-linux-gnu",
        ),
        (
            "not-an-archive",
            &[],
            |f| {
                let archive = f.release("latest/langspan-v9.9.9-x86_64-unknown-linux-gnu.tar.gz");
                fs::write(&archive, "not a tarball").unwrap();
                fs::write(
                    f.release("latest/SHA256SUMS"),
                    format!(
                        "{}  langspan-v9.9.9-x86_64-unknown-linux-gnu.tar.gz\n",
                        sha256(&archive)
                    ),
                )
                .unwrap();
            },
            "could not extract langspan",
        ),
    ];
    for (name, args, break_it, says) in cases {
        let fixture = Fixture::new(name);
        break_it(&fixture);
        let output = fixture.run("Linux", "x86_64", args, false);
        fixture.assert_nothing_changed(&output, says);
    }
}

#[test]
fn a_listed_name_cannot_leave_the_release() {
    let fixture = Fixture::new("traversal");
    fs::write(
        fixture.release("latest/SHA256SUMS"),
        format!(
            "{}  ../../x-x86_64-unknown-linux-gnu.tar.gz\n",
            "0".repeat(64)
        ),
    )
    .unwrap();
    let output = fixture.run("Linux", "x86_64", &[], false);
    fixture.assert_nothing_changed(&output, "unexpected archive");
    assert_eq!(fixture.downloads().len(), 1, "only SHA256SUMS is fetched");
}

#[test]
fn a_bad_argument_downloads_nothing() {
    for args in [
        &["../evil"][..],
        &["v1.2.3/../x"],
        &["v1", "v2"],
        &["$(id)"],
    ] {
        let fixture = Fixture::new("bad-argument");
        let output = fixture.run("Linux", "x86_64", args, false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains("v0.4.0"), "{args:?}: {stderr}");
        assert!(fixture.downloads().is_empty(), "{args:?}");
    }
}

#[test]
fn the_script_never_evaluates_input_or_asks_for_root() {
    let script = fs::read_to_string(repo("scripts/install.sh")).unwrap();
    let code: Vec<&str> = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    for banned in ["eval", "sudo", "http://", "| sh", "source "] {
        assert!(
            !code.iter().any(|line| line.contains(banned)),
            "install.sh must not use {banned:?}"
        );
    }
    assert!(script.contains("\nset -eu\n"));
}

#[test]
fn the_docs_lead_with_the_one_line_install_and_the_release_attaches_it() {
    for (doc, heading) in [
        ("README.md", "## Quickstart"),
        ("docs/install.md", "# Install"),
    ] {
        let text = fs::read_to_string(repo(doc)).unwrap();
        let section = &text[text.find(heading).unwrap()..];
        let one_line = section
            .find(ONE_LINE)
            .unwrap_or_else(|| panic!("{doc} lacks {ONE_LINE:?}"));
        let manual = section.find("curl -fsSLO").expect("the manual steps stay");
        assert!(
            one_line < manual,
            "{doc} must give the one-line install first"
        );
    }
    let workflow = fs::read_to_string(repo(".github/workflows/release.yml")).unwrap();
    assert!(
        workflow.contains("dist/install.sh"),
        "the release workflow must attach install.sh"
    );
}

/// An earlier `langspan` in its own folder, for PATH to find first.
fn shadow(fixture: &Fixture) -> PathBuf {
    let earlier = fixture.base.join("earlier-bin");
    fs::create_dir_all(&earlier).unwrap();
    write_executable(&earlier.join("langspan"), "#!/bin/sh\necho older\n");
    earlier
}

#[test]
fn an_earlier_langspan_first_on_path_is_named_and_put_behind() {
    // ~/.local/bin after the other folder, absent, or after it with a
    // trailing slash: each time the printed steps must reach this install.
    for (name, order) in [
        ("shadow-after", ["earlier", "bin"].as_slice()),
        ("shadow-absent", &["earlier"]),
        ("shadow-slash", &["earlier", "bin/"]),
    ] {
        let fixture = Fixture::new(name);
        let earlier = shadow(&fixture);
        let bin = fixture.base.join("home/.local/bin");
        let mut parts: Vec<String> = order
            .iter()
            .map(|part| match *part {
                "earlier" => earlier.display().to_string(),
                "bin" => bin.display().to_string(),
                _ => format!("{}/", bin.display()),
            })
            .collect();
        parts.push(format!(
            "{}:/usr/bin:/bin",
            fixture.base.join("stubs").display()
        ));
        let output = fixture.run_with_path("Linux", "x86_64", &parts.join(":"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{name}");
        assert!(
            stdout.contains(&format!("{}/langspan runs first", earlier.display())),
            "{name}: the earlier binary is named: {stdout}"
        );
        assert!(
            stdout.contains("export PATH=\"$HOME/.local/bin:$PATH\""),
            "{name}: {stdout}"
        );
        assert!(stdout.contains("2. Close Claude Code"), "{name}: {stdout}");
    }
}

#[test]
fn this_install_first_on_path_needs_no_path_step() {
    for name in ["first-plain", "first-slash", "first-link"] {
        let fixture = Fixture::new(name);
        let earlier = shadow(&fixture);
        let bin = fixture.base.join("home/.local/bin").display().to_string();
        let entry = match name {
            "first-plain" => bin,
            "first-slash" => format!("{bin}/"),
            _ => {
                let link = fixture.base.join("linked-bin");
                std::os::unix::fs::symlink(&bin, &link).unwrap();
                link.display().to_string()
            }
        };
        let path = format!(
            "{entry}:{}:{}:/usr/bin:/bin",
            earlier.display(),
            fixture.base.join("stubs").display()
        );
        let output = fixture.run_with_path("Linux", "x86_64", &path);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{name}");
        assert!(!stdout.contains("export PATH"), "{name}: {stdout}");
        assert!(!stdout.contains("runs first"), "{name}: {stdout}");
        assert!(stdout.contains("1. Close Claude Code"), "{name}: {stdout}");
    }
}

#[test]
fn the_threat_model_covers_the_install_script() {
    let text = fs::read_to_string(repo("docs/security/threat-model.md")).unwrap();
    let start = text
        .find("\n## The install script\n")
        .expect("the threat model has an install script section");
    let rest = &text[start + 1..];
    let section = &rest[..rest[3..].find("\n## ").map_or(rest.len(), |end| end + 3)];
    for needed in [
        "scripts/install.sh",
        "SHA256SUMS",
        "gh attestation verify",
        "--proto",
        "rename",
        "PATH",
    ] {
        assert!(section.contains(needed), "the section lacks {needed:?}");
    }
}

/// One section of the threat model, from its `## ` heading to the next.
fn threat_model_section(heading: &str) -> String {
    let text = fs::read_to_string(repo("docs/security/threat-model.md")).unwrap();
    let start = text
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("the threat model has no {heading:?} section"));
    let rest = &text[start + 1..];
    rest[..rest[3..].find("\n## ").map_or(rest.len(), |end| end + 3)].to_string()
}

/// Each job of `release.yml` with the scopes it may write: its
/// `permissions:` entries and the `permission-*` inputs of a minted token.
fn release_jobs() -> Vec<(String, Vec<String>)> {
    let workflow = fs::read_to_string(repo(".github/workflows/release.yml")).unwrap();
    let jobs = &workflow[workflow.find("\njobs:\n").expect("release.yml has jobs")..];
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    for line in jobs.lines().skip(2) {
        let is_job = line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.starts_with("  #")
            && line.ends_with(':');
        if is_job {
            found.push((line.trim().trim_end_matches(':').to_string(), Vec::new()));
        } else if let Some((_, scopes)) = found.last_mut() {
            let entry = line.split('#').next().unwrap().trim();
            if let Some(scope) = entry.strip_suffix(": write") {
                scopes.push(scope.trim_start_matches("permission-").to_string());
            }
        }
    }
    found
}

#[test]
fn the_threat_model_names_every_release_job_and_each_scope_it_writes() {
    let jobs = release_jobs();
    for id in ["verify-published", "withdraw"] {
        assert!(
            jobs.iter().any(|(job, _)| job == id),
            "release.yml lacks {id}"
        );
    }
    let section = threat_model_section("Release pipeline");
    for (job, scopes) in jobs {
        let named = format!("`{job}`");
        let rows: Vec<&str> = section.lines().filter(|l| l.contains(&named)).collect();
        assert!(
            !rows.is_empty(),
            "the release pipeline section lacks {named}"
        );
        for scope in scopes {
            let scope = format!("`{scope}: write`");
            assert!(
                rows.iter().any(|row| row.contains(&scope)),
                "no line of the release pipeline section names both {named} and {scope}"
            );
        }
    }
}
