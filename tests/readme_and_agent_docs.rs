//! Holds the README and the always-loaded agent file to the budgets in
//! `docs/writing-style.md` ("The README" and "Docs for agents"). A README
//! that grows past its budget, or pushes the quickstart down, loses the
//! person deciding whether to try LangSpan; every line of `AGENTS.md` costs
//! context on every agent turn.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The README's upper bound in words of rendered text.
const README_MAX_WORDS: usize = 1_500;
/// `langspan setup` appears within this many words of the README's start.
const QUICKSTART_WITHIN_WORDS: usize = 200;
/// The always-loaded agent file's upper bound in words.
const AGENTS_MAX_WORDS: usize = 600;

fn read(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)).unwrap()
}

/// The words a reader sees: HTML tags, link targets and badge lines removed.
fn rendered_words(markdown: &str) -> Vec<String> {
    let mut text = String::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("[![") {
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                for skipped in chars.by_ref() {
                    if skipped == '>' {
                        break;
                    }
                }
                out.push(' ');
            }
            ']' if chars.peek() == Some(&'(') => {
                for skipped in chars.by_ref() {
                    if skipped == ')' {
                        break;
                    }
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect()
}

#[test]
fn rendered_words_skip_markup_and_count_text() {
    let words = rendered_words(
        "[![badge](https://x)](https://y)\n<img alt=\"a b\" src=\"c\">\n# Two words\n\
         See [the guide](docs/usage.md) now. `langspan setup` ---\n",
    );
    assert_eq!(
        words,
        [
            "Two",
            "words",
            "See",
            "[the",
            "guide",
            "now.",
            "`langspan",
            "setup`"
        ]
    );
}

#[test]
fn the_readme_stays_within_its_word_budget() {
    let words = rendered_words(&read("README.md"));
    assert!(
        words.len() <= README_MAX_WORDS,
        "README.md has {} words; the budget is {README_MAX_WORDS}. Move detail \
         to a linked doc (docs/writing-style.md, \"The README\").",
        words.len()
    );
}

#[test]
fn the_readme_reaches_setup_within_its_first_words() {
    let words = rendered_words(&read("README.md"));
    let position = words
        .windows(2)
        .position(|pair| pair[0].ends_with("langspan") && pair[1].starts_with("setup"));
    assert!(
        position.is_some_and(|at| at <= QUICKSTART_WITHIN_WORDS),
        "`langspan setup` is at word {position:?} of README.md; the quickstart \
         belongs within the first {QUICKSTART_WITHIN_WORDS} words."
    );
}

/// The first fenced `sh` block after `heading` in `markdown`, without fences.
fn sh_block_after(markdown: &str, heading: &str) -> String {
    let rest = &markdown[markdown
        .find(heading)
        .unwrap_or_else(|| panic!("no {heading:?} heading"))..];
    let start = rest.find("```sh\n").expect("no sh block after the heading") + "```sh\n".len();
    let len = rest[start..].find("```").expect("unclosed sh block");
    rest[start..start + len].to_owned()
}

/// The release targets named in the first table after `heading`.
fn target_rows(markdown: &str, heading: &str) -> Vec<String> {
    let rest = &markdown[markdown.find(heading).unwrap()..];
    rest.lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .filter_map(|line| {
            let cell = line.split('|').nth(2)?.trim();
            let target = cell.strip_prefix('`')?.split('`').next()?;
            target.contains("-").then(|| target.to_owned())
        })
        .collect()
}

const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
];

#[test]
fn the_quickstart_names_one_target_per_platform() {
    for (doc, heading) in [
        ("README.md", "## Quickstart"),
        ("docs/install.md", "# Install"),
    ] {
        assert_eq!(
            target_rows(&read(doc), heading),
            TARGETS,
            "{doc} needs a four-row table of release targets under {heading:?}"
        );
    }
}

#[test]
fn the_quickstart_runs_top_to_bottom_without_gh() {
    let block = sh_block_after(&read("README.md"), "## Quickstart");
    // Each step a stranger needs, in order: download, verify, extract,
    // install, keep PATH for later shells, set up, check.
    let steps = [
        "target=",
        "curl -fsSLO \"$url/SHA256SUMS\"",
        "curl -fsSLO \"$url/$archive\"",
        "shasum -a 256 -c",
        "tar -xzf \"$archive\"",
        "cd \"${archive%.tar.gz}\"",
        "install -m 0755 langspan \"$HOME/.local/bin/langspan\"",
        ">> \"$HOME/.",
        "langspan setup",
        "langspan doctor",
    ];
    let mut from = 0;
    for step in steps {
        let at = block[from..]
            .find(step)
            .unwrap_or_else(|| panic!("README quickstart lacks {step:?} after byte {from}"));
        from += at + step.len();
    }
    assert!(
        !block.contains("gh "),
        "the quickstart must not need the GitHub CLI"
    );
    let parsed = std::process::Command::new("sh")
        .args(["-n", "-c", &block])
        .status()
        .unwrap();
    assert!(parsed.success(), "the README quickstart is not valid sh");
}

#[test]
fn the_install_guide_repeats_the_readme_quickstart() {
    assert_eq!(
        sh_block_after(&read("docs/install.md"), "## Install"),
        sh_block_after(&read("README.md"), "## Quickstart"),
        "docs/install.md and the README must give the same install commands"
    );
}

#[test]
fn the_quickstart_names_shells_that_paste_it_whole() {
    // A shell without bracketed paste runs pasted lines one at a time, and
    // `langspan setup` would read the next pasted line as its answer.
    for doc in ["README.md", "docs/install.md"] {
        let text = read(doc);
        assert!(
            text.contains("paste the block into zsh, or bash 5.1 or later:"),
            "{doc} must name the shells that take a pasted block whole"
        );
        assert!(
            text.contains("macOS's `/bin/bash`"),
            "{doc} must say why macOS's own bash is not one of them"
        );
    }
}

#[test]
fn the_readme_states_each_host_status_once() {
    let readme = read("README.md");
    assert!(!readme.contains("pending"), "README.md calls a run pending");
    let antigravity_status = readme
        .lines()
        .filter(|line| line.contains("Antigravity") && line.to_lowercase().contains("verified"))
        .count();
    assert_eq!(
        antigravity_status, 1,
        "state Antigravity's status in one README line"
    );
}

#[test]
fn the_agent_file_stays_within_its_word_budget() {
    let words = rendered_words(&read("AGENTS.md"));
    assert!(
        words.len() <= AGENTS_MAX_WORDS,
        "AGENTS.md has {} words; the budget is {AGENTS_MAX_WORDS}. Keep only \
         what an agent can't infer from the code, and link the rest \
         (docs/writing-style.md, \"Docs for agents\").",
        words.len()
    );
}

/// The folder and archive stem of the stand-in release.
const RELEASE: &str = "langspan-v9.9.9-aarch64-apple-darwin";
/// The file in the home that the `langspan` stand-ins append their arguments to.
const RAN: &str = "ran";

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// A `langspan` stand-in that records `label` and its arguments.
fn write_stand_in(path: &Path, label: &str) {
    let script = format!("#!/bin/sh\necho \"{label} $*\" >> \"$HOME/{RAN}\"\n");
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A release served from a local folder, an empty folder to paste the
/// quickstart in, a home with an earlier install, and a `PATH` holding only
/// the tools the block names. Real `curl`, `awk`, `tar` and checksum tool.
struct Quickstart {
    base: PathBuf,
    /// The checksum command the block runs: its own, or the Linux variant.
    checker: &'static str,
}

impl Quickstart {
    fn new(name: &str, checker: &'static str) -> Self {
        let program = checker.split(' ').next().unwrap();
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "langspan-quickstart-{name}-{program}-{}",
                std::process::id()
            ));
        let _ = fs::remove_dir_all(&base);
        let quickstart = Self { base, checker };
        let stage = quickstart.base.join("stage").join(RELEASE);
        let earlier = quickstart.base.join("home/.local/bin");
        for dir in [
            &stage,
            &earlier,
            &quickstart.base.join("release"),
            &quickstart.base.join("work"),
            &quickstart.base.join("bin"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        for tool in [
            "curl", "awk", "tar", "gzip", "mkdir", "install", "basename", program,
        ] {
            match on_path(tool) {
                Some(found) => symlink(found, quickstart.base.join("bin").join(tool)).unwrap(),
                // GNU tar runs gzip; a tar with gzip built in needs none.
                None if tool == "gzip" => {}
                None => panic!("the quickstart test needs {tool} on PATH"),
            }
        }
        write_stand_in(&stage.join("langspan"), "new");
        write_stand_in(&earlier.join("langspan"), "earlier");
        let packed = Command::new("tar")
            .arg("-czf")
            .arg(quickstart.archive())
            .arg("-C")
            .arg(quickstart.base.join("stage"))
            .arg(RELEASE)
            .status()
            .unwrap();
        assert!(packed.success());
        quickstart.list(&quickstart.digest());
        quickstart
    }

    fn archive(&self) -> PathBuf {
        self.base.join("release").join(format!("{RELEASE}.tar.gz"))
    }

    fn program(&self) -> &'static str {
        self.checker.split(' ').next().unwrap()
    }

    /// The archive's SHA-256, from the same tool the block checks it with.
    fn digest(&self) -> String {
        let mut words = self.checker.split(' ').filter(|word| *word != "-c");
        let output = Command::new(words.next().unwrap())
            .args(words)
            .arg(self.archive())
            .output()
            .unwrap();
        assert!(output.status.success());
        let line = String::from_utf8(output.stdout).unwrap();
        line.split_whitespace().next().unwrap().to_owned()
    }

    /// Publishes `SHA256SUMS` as the release workflow lays it out: the
    /// archive under `digest`, and the target's SBOM.
    fn list(&self, digest: &str) {
        let sbom = "1".repeat(64);
        fs::write(
            self.base.join("release/SHA256SUMS"),
            format!("{digest}  {RELEASE}.tar.gz\n{sbom}  {RELEASE}.cdx.json\n"),
        )
        .unwrap();
    }

    /// The README's block, pointed at the local release.
    fn block(&self) -> String {
        let block = sh_block_after(&read("README.md"), "## Quickstart");
        assert!(block.contains("shasum -a 256 -c"));
        let release = format!("url=file://{}", self.base.join("release").display());
        block
            .replace("shasum -a 256 -c", self.checker)
            .lines()
            .map(|line| {
                if line.starts_with("url=") {
                    release.as_str()
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Runs `block` as a shell runs a pasted block: a failed line does not
    /// end the shell. Returns the block's standard error if it failed.
    fn paste(&self, block: &str) -> Result<(), String> {
        let home = self.base.join("home");
        let path = std::env::join_paths([home.join(".local/bin"), self.base.join("bin")]).unwrap();
        let output = Command::new("/bin/sh")
            .args(["-c", block])
            .current_dir(self.base.join("work"))
            .env_clear()
            .env("HOME", &home)
            .env("SHELL", "/bin/zsh")
            .env("PATH", path)
            .output()
            .unwrap();
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }

    fn home_file(&self, relative: &str) -> Option<String> {
        fs::read_to_string(self.base.join("home").join(relative)).ok()
    }
}

impl Drop for Quickstart {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The checksum commands the docs name that this machine has. The docs give
/// `sha256sum -c` for Linux only: macOS's `sha256sum -c` takes no list on
/// standard input.
fn checkers() -> Vec<&'static str> {
    let found: Vec<_> = ["shasum -a 256 -c", "sha256sum -c"]
        .into_iter()
        .filter(|checker| cfg!(target_os = "linux") || checker.starts_with("shasum"))
        .filter(|checker| on_path(checker.split(' ').next().unwrap()).is_some())
        .collect();
    assert!(!found.is_empty(), "no SHA-256 tool on PATH");
    found
}

#[test]
fn the_quickstart_installs_and_sets_up_an_intact_download() {
    for checker in checkers() {
        let quickstart = Quickstart::new("intact", checker);
        assert_eq!(quickstart.paste(&quickstart.block()), Ok(()), "{checker}");
        assert_eq!(
            quickstart.home_file(RAN).as_deref(),
            Some("new setup\nnew doctor\n"),
            "{checker}: the downloaded binary runs setup, then doctor"
        );
        assert!(
            quickstart
                .home_file(".zshrc")
                .is_some_and(|profile| profile.contains(".local/bin")),
            "{checker}: PATH is kept for later shells"
        );
    }
}

#[test]
fn the_quickstart_stops_at_the_first_failed_step() {
    type Break = fn(&Quickstart) -> String;
    let cases: [(&str, Break); 6] = [
        ("checksum-mismatch", |quickstart| {
            quickstart.list(&"0".repeat(64));
            quickstart.block()
        }),
        ("no-checksum-tool", |quickstart| {
            fs::remove_file(quickstart.base.join("bin").join(quickstart.program())).unwrap();
            quickstart.block()
        }),
        ("unknown-target", |quickstart| {
            let block = quickstart.block();
            assert!(block.contains("target=aarch64-apple-darwin\n"));
            block.replace("target=aarch64-apple-darwin\n", "target=riscv64-none\n")
        }),
        ("no-checksum-file", |quickstart| {
            fs::remove_file(quickstart.base.join("release/SHA256SUMS")).unwrap();
            quickstart.block()
        }),
        ("no-archive", |quickstart| {
            fs::remove_file(quickstart.archive()).unwrap();
            quickstart.block()
        }),
        // The checksum matches, but the archive doesn't extract, and the
        // folder already holds a file named `langspan`.
        ("not-an-archive", |quickstart| {
            fs::write(quickstart.archive(), "not a tarball").unwrap();
            quickstart.list(&quickstart.digest());
            write_stand_in(&quickstart.base.join("work/langspan"), "stray");
            quickstart.block()
        }),
    ];
    for checker in checkers() {
        for (name, break_it) in cases {
            let quickstart = Quickstart::new(name, checker);
            let earlier = quickstart.home_file(".local/bin/langspan");
            let block = break_it(&quickstart);
            assert!(
                quickstart.paste(&block).is_err(),
                "{name}, {checker}: the block must report the failure"
            );
            assert_eq!(
                quickstart.home_file(RAN),
                None,
                "{name}, {checker}: no langspan may run after a failed step"
            );
            assert_eq!(
                quickstart.home_file(".local/bin/langspan"),
                earlier,
                "{name}, {checker}: nothing may be installed after a failed step"
            );
            assert_eq!(
                quickstart.home_file(".zshrc"),
                None,
                "{name}, {checker}: the shell startup file must stay untouched"
            );
            assert!(
                !quickstart.base.join("work").join(RELEASE).exists(),
                "{name}, {checker}: nothing may be extracted after a failed step"
            );
        }
    }
}

#[test]
fn the_quickstart_gates_every_step_on_the_one_before() {
    // A line added without `&&` would run after a failed check.
    let block = sh_block_after(&read("README.md"), "## Quickstart");
    let lines: Vec<&str> = block.lines().collect();
    let first_step = lines
        .iter()
        .position(|line| line.starts_with("curl "))
        .expect("the quickstart downloads with curl");
    for line in &lines[..first_step] {
        assert!(
            line.starts_with("target=") || line.starts_with("url="),
            "only the two settings may precede the first download: {line:?}"
        );
    }
    let (last, gated) = lines[first_step..].split_last().unwrap();
    for line in gated {
        assert!(
            line.ends_with(" &&"),
            "{line:?} must end with `&&` so the next step needs it to succeed"
        );
    }
    assert_eq!(*last, "langspan doctor");
    assert!(!block.contains("||"), "no step may excuse a failure");
}
