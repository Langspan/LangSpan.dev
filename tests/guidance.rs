//! `langspan guidance`: the LangSpan block for AGENTS.md and similar files
//! (roadmap I7, ADR 0018). Real Git repositories with the owner's own
//! AGENTS.md, and the real executable.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BEGIN: &str = "<!-- langspan:begin -->";
const END: &str = "<!-- langspan:end -->";

/// A temporary Git repository, removed when dropped.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ls-guidance-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        Self(dir)
    }

    fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }

    fn write(&self, file: &str, bytes: &str) {
        std::fs::write(self.path(file), bytes).unwrap();
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.path(file)).unwrap()
    }

    fn guidance(&self, args: &[&str]) -> Output {
        langspan_in(&self.0, args)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn langspan_in(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("guidance")
        .args(args)
        .args(["--root", root.to_str().unwrap()])
        .env_remove("LANGSPAN_STATE_DIR")
        .output()
        .unwrap()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn failed(output: &Output) -> String {
    assert!(!output.status.success(), "{output:?}");
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// The owner's AGENTS.md, without a final newline to make byte-for-byte
/// restoration harder, and with a CRLF line.
const OWNER: &str = "# Project notes\r\n\nUse tabs.\n\n## Tests\nRun make test.";

fn printed_block() -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("guidance")
        .output()
        .unwrap();
    ok(&output)
}

#[test]
fn guidance_prints_the_block_for_each_format() {
    let agents = printed_block();
    assert!(agents.starts_with(&format!("{BEGIN}\n")), "{agents}");
    assert!(agents.ends_with(&format!("{END}\n")), "{agents}");
    assert!(agents.len() <= 1500, "{} bytes", agents.len());
    for tool in ["langspan_brief", "langspan_admit", "langspan_complete"] {
        assert!(agents.contains(tool), "{tool}: {agents}");
    }
    assert!(agents.contains("advice"), "{agents}");
    let run = |format: &str| {
        ok(&Command::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["guidance", "--format", format])
            .output()
            .unwrap())
    };
    assert_eq!(run("agents"), agents);
    assert_eq!(run("gemini"), agents);
    let rules = run("cursor-rules");
    assert!(rules.starts_with("---\n"), "{rules}");
    assert!(rules.contains("alwaysApply: true"), "{rules}");
    assert!(rules.ends_with(&agents), "{rules}");
    let wrong = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["guidance", "--format", "windsurf"])
        .output()
        .unwrap();
    assert!(failed(&wrong).contains("agents|gemini|cursor-rules"));
}

#[test]
fn the_dry_run_prints_a_diff_and_changes_no_file() {
    let repo = Repo::new("dry");
    repo.write("AGENTS.md", OWNER);
    let before = std::fs::metadata(repo.path("AGENTS.md")).unwrap();
    let shown = ok(&repo.guidance(&["install", "--file", "AGENTS.md"]));
    assert!(shown.contains(&format!("+ {BEGIN}\n")), "{shown}");
    assert!(shown.contains(&format!("+ {END}\n")), "{shown}");
    assert!(
        shown.contains("dry run: add --apply to write AGENTS.md"),
        "{shown}"
    );
    // No line of the owner's is shown as removed.
    assert!(!shown.lines().any(|line| line.starts_with("- ")), "{shown}");
    assert_eq!(repo.read("AGENTS.md"), OWNER);
    let after = std::fs::metadata(repo.path("AGENTS.md")).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    // Nothing else appeared in the repository either.
    let entries: Vec<_> = std::fs::read_dir(&repo.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries.len(), 2, "{entries:?}"); // .git and AGENTS.md
}

#[test]
fn apply_adds_one_block_keeps_the_rest_and_remove_restores_it() {
    let repo = Repo::new("apply");
    repo.write("AGENTS.md", OWNER);
    let block = printed_block();
    let shown = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(shown.contains("wrote"), "{shown}");
    let installed = repo.read("AGENTS.md");
    assert_eq!(installed.matches(BEGIN).count(), 1, "{installed}");
    assert_eq!(installed.matches(END).count(), 1, "{installed}");
    // Exactly the block was added; every other byte is the owner's.
    assert_eq!(installed.replacen(&block, "", 1), format!("\n{OWNER}"));

    // Twice leaves one block, and says nothing changed.
    let again = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(again.contains("already"), "{again}");
    assert_eq!(repo.read("AGENTS.md"), installed);

    // The removal's dry run shows only the block going, and changes nothing.
    let dry = ok(&repo.guidance(&["remove", "--file", "AGENTS.md"]));
    assert!(dry.contains(&format!("- {BEGIN}\n")), "{dry}");
    assert!(!dry.lines().any(|line| line.starts_with("+ ")), "{dry}");
    assert_eq!(repo.read("AGENTS.md"), installed);

    ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert_eq!(repo.read("AGENTS.md"), OWNER);
    let nothing = ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert!(nothing.contains("no LangSpan block"), "{nothing}");
    assert_eq!(repo.read("AGENTS.md"), OWNER);
}

#[test]
fn a_file_with_a_leading_blank_line_or_nothing_in_it_round_trips() {
    for original in ["", "\n", "\n\nText\n", "x"] {
        let repo = Repo::new("edges");
        repo.write("GEMINI.md", original);
        ok(&repo.guidance(&["install", "--file", "GEMINI.md", "--apply"]));
        assert_eq!(repo.read("GEMINI.md").matches(BEGIN).count(), 1);
        ok(&repo.guidance(&["remove", "--file", "GEMINI.md", "--apply"]));
        // An empty file is removed with the block: nothing of it is lost.
        if original.is_empty() {
            assert!(!repo.path("GEMINI.md").exists());
        } else {
            assert_eq!(repo.read("GEMINI.md"), original, "{original:?}");
        }
    }
}

#[test]
fn a_moved_block_is_left_where_the_owner_put_it() {
    let repo = Repo::new("moved");
    let block = printed_block();
    let moved = format!("# Notes\n\n{block}\nMore.\n");
    repo.write("AGENTS.md", &moved);
    let again = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(again.contains("already"), "{again}");
    assert_eq!(repo.read("AGENTS.md"), moved);
    ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert_eq!(repo.read("AGENTS.md"), "# Notes\n\n\nMore.\n");
}

#[test]
fn a_hand_edited_block_or_broken_markers_stop_with_the_line_numbers() {
    let block = printed_block();
    let edited = block.replacen("Other agent", "Some agent", 1);
    assert_ne!(edited, block);
    let block_lines = block.lines().count();
    for (text, lines) in [
        (
            format!("# Notes\n\n{edited}"),
            format!("lines 3-{}", 2 + block_lines),
        ),
        (format!("# Notes\n{BEGIN}\nhalf\n"), "line 2".to_owned()),
        (format!("{END}\n# Notes\n"), "line 1".to_owned()),
        (
            format!("{block}\n{block}"),
            format!("line {}", block_lines + 2),
        ),
        (
            "# Notes\n<!-- langspan:begin v2 -->\n".to_owned(),
            "line 2".to_owned(),
        ),
    ] {
        let repo = Repo::new("edited");
        repo.write("AGENTS.md", &text);
        for action in ["install", "remove"] {
            let error = failed(&repo.guidance(&[action, "--file", "AGENTS.md", "--apply"]));
            assert!(error.contains(&lines), "{action} {text:?}: {error}");
            assert!(error.contains("nothing was changed"), "{error}");
            assert_eq!(repo.read("AGENTS.md"), text);
        }
    }
}

#[test]
fn a_missing_file_is_created_only_with_create() {
    let repo = Repo::new("create");
    let error = failed(&repo.guidance(&["install", "--file", "AGENTS.md", "--apply"]));
    assert!(error.contains("--create"), "{error}");
    assert!(!repo.path("AGENTS.md").exists());
    let shown = ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--create"]));
    assert!(shown.contains("dry run"), "{shown}");
    assert!(!repo.path("AGENTS.md").exists());
    ok(&repo.guidance(&["install", "--file", "AGENTS.md", "--create", "--apply"]));
    assert_eq!(repo.read("AGENTS.md"), printed_block());
    // Removing the block removes the file it created.
    ok(&repo.guidance(&["remove", "--file", "AGENTS.md", "--apply"]));
    assert!(!repo.path("AGENTS.md").exists());
}

#[test]
fn the_file_must_stay_inside_the_root_and_not_be_a_symlink() {
    let repo = Repo::new("bounds");
    repo.write("real.md", OWNER);
    std::os::unix::fs::symlink(repo.path("real.md"), repo.path("AGENTS.md")).unwrap();
    for file in ["../AGENTS.md", "/tmp/AGENTS.md", "", "AGENTS.md"] {
        let error = failed(&repo.guidance(&["install", "--file", file, "--apply"]));
        assert!(!error.is_empty(), "{file}");
    }
    assert_eq!(repo.read("real.md"), OWNER);
    let error = failed(&repo.guidance(&["install", "--apply"]));
    assert!(error.contains("--file"), "{error}");
}

#[test]
fn a_file_in_a_subdirectory_of_the_repository_is_supported() {
    let repo = Repo::new("nested");
    std::fs::create_dir_all(repo.path("pkg")).unwrap();
    repo.write("pkg/AGENTS.md", OWNER);
    ok(&repo.guidance(&["install", "--file", "pkg/AGENTS.md", "--apply"]));
    assert_eq!(repo.read("pkg/AGENTS.md").matches(BEGIN).count(), 1);
    ok(&repo.guidance(&["remove", "--file", "pkg/AGENTS.md", "--apply"]));
    assert_eq!(repo.read("pkg/AGENTS.md"), OWNER);
}
