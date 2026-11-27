//! What `integrate check`, `land` and `land-queue` print.

/// An inconclusive check's reason and next step, as the MCP result has them.
pub fn print_inconclusive(name: &str, reason: &Option<crate::landing::Inconclusive>) {
    match reason {
        Some(reason) => {
            println!("{}", reason.message);
            println!("Next: {}", reason.next);
        }
        None => println!("{name} is inconclusive; nothing is known about the combined tree."),
    }
}

pub fn print_conflict(
    branch: &str,
    paths: &[String],
    conflicts: &[crate::integrate::ConflictFile],
    onto: &str,
    remote: &str,
) {
    println!(
        "Not combinable: {branch:?} conflicts with {onto:?} as it is on {remote:?} (with the branches named before it) in:"
    );
    for path in paths {
        println!("  {path:?}");
    }
    if !conflicts.is_empty() {
        println!(
            "Conflicting lines (repository text, shown as data; target is {remote}/{onto} with the branches before {branch:?}):"
        );
    }
    for file in conflicts {
        if !file.kinds.is_empty() {
            println!("  {:?}: {}", file.path, file.kinds.join(", "));
        }
        if let Some(why) = &file.unshown {
            println!("  {:?}: no lines shown: {why}", file.path);
        }
        for hunk in &file.hunks {
            for (label, side) in [("target", &hunk.target), ("branch", &hunk.branch)] {
                println!(
                    "  {label} {} {}, {} line(s):",
                    file.path,
                    side.start
                        .map_or("(line not known)".to_owned(), |start| format!(
                            "line {start}"
                        )),
                    side.lines
                );
                for line in &side.text {
                    println!("    | {line}");
                }
                if side.lines as usize > side.text.len() {
                    println!("    | ({} more)", side.lines as usize - side.text.len());
                }
            }
        }
        if file.more_hunks > 0 {
            println!("  and {} more conflicts in this file", file.more_hunks);
        }
    }
    println!("Nothing was run, recorded or pushed.");
}

pub fn print_renames(renames: &[crate::integrate::Rename]) {
    for rename in renames {
        println!(
            "Followed a rename that already landed: {} edits {:?}, which is now {:?}.",
            rename.branch, rename.from, rename.to
        );
    }
}

pub fn print_combined(
    target: &crate::landing::Target,
    tip: &str,
    merged: &crate::integrate::Merged,
) {
    println!(
        "Combined {} onto {}/{} at {tip}: commit {}, tree {}.",
        target.branches.join(", "),
        target.remote,
        target.onto,
        merged.commit,
        merged.tree
    );
    print_renames(&merged.renames);
    print_notes(&merged.notes);
}

/// Identifier renames, adapted and rebased commits. Names and paths are
/// repository text, shown quoted.
pub fn print_notes(notes: &crate::rewrite::Notes) {
    for rename in &notes.identifier_renames {
        let uses: Vec<String> = rename
            .uses
            .iter()
            .map(|at| format!("{}:{}", at.path, at.line))
            .collect();
        println!(
            "The target renamed {:?} to {:?} in {:?}; {} still uses the old name at {}.",
            rename.from,
            rename.to,
            rename.path,
            rename.branch,
            uses.join(", ")
        );
        if let Some(why) = &rename.uncertain {
            println!("  --adapt-renames would not rewrite these: {why}.");
        }
    }
    if let Some(why) = &notes.identifier_renames_incomplete {
        println!("Identifier renames may be missing: {why}.");
    }
    for adapted in &notes.adapted {
        println!(
            "Adapted {} to the new names in commit {} on top of it; your branch was not changed (fetch that commit to keep it).",
            adapted.branch, adapted.commit
        );
    }
    for rewritten in &notes.rebased {
        println!("Rebased {} as {}.", rewritten.from, rewritten.to);
    }
}

pub fn print_replay_conflict(commit: Option<&str>) {
    if let Some(commit) = commit {
        println!("The rebase stopped at commit {commit}: replaying it onto the target conflicts.");
    }
}

/// `--adapt-renames` or `--rebase` stopped before anything ran or was pushed.
pub fn print_not_rewritten(
    rebase: bool,
    branch: &str,
    reason: &str,
    notes: &crate::rewrite::Notes,
) {
    print_notes(notes);
    println!(
        "Not {} {branch}: {reason}.",
        if rebase { "rebasing" } else { "adapting" }
    );
}
