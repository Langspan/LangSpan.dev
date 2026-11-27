//! The other worktrees' uncommitted callers of the names a write renames
//! or removes (issue #213), for the hook to hold that write once. Read
//! from the index and answered, never recorded.

use super::{
    PROTOCOL_VERSION, Registration, Response, Store, check_calls, error, inflight_worktrees,
};
use langspan_core::{Command, NOTICE_DEPENDENTS, Outcome, PeerCallers, State};

/// Worktrees one answer names callers in, at most.
const MAX_CALLER_WORKTREES: usize = 8;
/// Bytes of JSON one answer's callers take at most, a quarter of a frame.
const CALLER_BYTES: usize = 16 * 1024;

/// A `CheckCalls` answer: the break check of its calls (`check_calls`)
/// with the peers' callers of its `renamed` names. With renamed names
/// alone no notice is read, and `ready` says only that there is an index.
pub(in crate::daemon) fn checked_calls(
    store: &mut Store,
    graph: Option<&crate::graph::Index>,
    request_id: String,
    command: &Command,
) -> Response {
    let generation = store.state().generation;
    let Command::CheckCalls {
        session_id,
        calls,
        renamed,
    } = command
    else {
        return error(request_id, generation, "internal", "not a write check");
    };
    let mut callers = Vec::new();
    if !renamed.is_empty() {
        let asking = match langspan_core::validate_renamed(store.state(), session_id, renamed) {
            Ok(asking) => asking.workspace_id.clone(),
            Err(e) => return error(request_id, generation, &e.code, e.message),
        };
        if let Some(graph) = graph {
            match peer_callers(store.state(), graph, &asking, renamed) {
                Ok(found) => callers = found,
                Err(failure) => return error(request_id, generation, "index_unavailable", failure),
            }
        }
    }
    if !calls.is_empty() || renamed.is_empty() {
        return check_calls(store, graph, request_id, session_id, calls, callers);
    }
    Response {
        version: PROTOCOL_VERSION,
        request_id,
        replayed: false,
        generation,
        lease_valid: None,
        outcome: Outcome::CallBreaks {
            breaks: Vec::new(),
            coverage: Default::default(),
            ready: graph.is_some(),
            callers,
        },
    }
}

/// The files of each other worktree of `workspace_id`'s repository that
/// differ from that worktree's `HEAD` and mention a name of `renamed`, at
/// most `NOTICE_DEPENDENTS` per worktree and name, in at most
/// `MAX_CALLER_WORKTREES` worktrees in workspace order. Each worktree is
/// read in one snapshot of its index, and only once that index has a
/// complete scan: a worktree without one names no callers, so the hold
/// errs toward letting the write through.
pub(super) fn peer_callers(
    state: &State,
    graph: &crate::graph::Index,
    workspace_id: &str,
    renamed: &[String],
) -> Result<Vec<PeerCallers>, String> {
    let mut callers = Vec::new();
    if renamed.is_empty() {
        return Ok(callers);
    }
    for (other, registration) in inflight_worktrees(state, workspace_id)
        .into_iter()
        .take(MAX_CALLER_WORKTREES)
    {
        let looked_up = graph.with_coverage(&registration, |graph| {
            let mut found = Vec::new();
            for name in renamed {
                let paths = graph.inflight_mentioning(other, name, NOTICE_DEPENDENTS)?;
                if !paths.is_empty() {
                    found.push((name.clone(), paths));
                }
            }
            Ok(found)
        });
        let found = match looked_up {
            Ok((found, coverage)) if coverage.scanned => found,
            Ok(_) => continue,
            Err(failure) => return Err(failure.to_string()),
        };
        let sessions = langspan_core::live_sessions(state, other);
        callers.extend(found.into_iter().map(|(name, paths)| PeerCallers {
            name,
            worktree: root(&registration),
            sessions: sessions.clone(),
            paths,
        }));
    }
    Ok(within_budget(callers))
}

/// `callers` cut to `CALLER_BYTES` of JSON in all, so that the answer
/// fits one frame with its breaks: an entry's last paths go first, then
/// the entries that no longer fit with any path.
fn within_budget(callers: Vec<PeerCallers>) -> Vec<PeerCallers> {
    let mut kept = Vec::new();
    let mut used = 0;
    for mut entry in callers {
        while !entry.paths.is_empty() {
            let bytes = serde_json::to_vec(&entry).map_or(usize::MAX, |json| json.len());
            if used + bytes <= CALLER_BYTES {
                used += bytes;
                kept.push(entry);
                break;
            }
            entry.paths.pop();
        }
    }
    kept
}

fn root(registration: &Registration) -> String {
    registration.root.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    //! In-process: a real index file and `State` values, no daemon, Git
    //! or socket. What is verified is the join of the index's references
    //! and in-flight files, and which worktrees are read.
    use super::*;
    use langspan_core::{Command, Outcome};
    use std::{
        fs,
        time::{Duration, Instant},
    };

    /// Index `files` of one worktree as a complete scan would, each listed
    /// in `dirty` as differing from `HEAD` (new there, so Git is not read).
    fn scan(
        index: &mut crate::graph::Index,
        workspace_id: &str,
        root: &std::path::Path,
        files: &[(&str, &str)],
        dirty: &[&str],
    ) {
        let mut scan = crate::graph::ScanView::default();
        for (path, text) in files {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, text).unwrap();
            let digest = crate::sha256::hex(&crate::sha256::digest(text.as_bytes()));
            scan.files.insert((*path).to_owned(), digest);
        }
        for path in dirty {
            scan.dirty.insert((*path).to_owned(), None);
        }
        for _ in 0..2 {
            index
                .update(
                    workspace_id,
                    root,
                    0,
                    &scan,
                    1_000,
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
        }
    }

    fn state(roots: &[(&str, &std::path::Path)]) -> State {
        let mut state = State::default();
        for (id, root) in roots {
            state.workspaces.insert(
                (*id).into(),
                langspan_core::Workspace {
                    workspace_id: (*id).into(),
                    root: root.to_str().unwrap().into(),
                    verified_overlaps: Vec::new(),
                },
            );
            state.workspace_git.insert(
                (*id).into(),
                langspan_core::WorkspaceGit {
                    workspace_id: (*id).into(),
                    repository_id: "repository".into(),
                    branch_ref: None,
                },
            );
        }
        for (session, workspace) in [("alice", "main"), ("bob", "tree")] {
            state.sessions.insert(
                session.into(),
                langspan_core::Session {
                    session_id: session.into(),
                    workspace_id: workspace.into(),
                    generation: 0,
                    parent: None,
                    headless: false,
                },
            );
        }
        state
    }

    const CALLER: &str = "fn run() -> u8 { parse_config() }\n";

    /// The most callers an answer can find (8 worktrees, 8 names, 32 long
    /// paths each) are cut to the budget, keeping whole entries first.
    #[test]
    fn the_callers_of_one_answer_fit_their_byte_budget() {
        let entry = |worktree: usize, name: usize| PeerCallers {
            name: format!("name_{name}"),
            worktree: format!("/work/tree-{worktree}"),
            sessions: vec!["claude-code.bob".into()],
            paths: (0..NOTICE_DEPENDENTS)
                .map(|n| format!("src/{}/{n}.rs", "d".repeat(400)))
                .collect(),
        };
        let all: Vec<PeerCallers> = (0..MAX_CALLER_WORKTREES)
            .flat_map(|worktree| {
                (0..langspan_core::MAX_RENAMED).map(move |name| entry(worktree, name))
            })
            .collect();
        let kept = within_budget(all.clone());
        assert!(serde_json::to_vec(&kept).unwrap().len() <= CALLER_BYTES + kept.len() + 2);
        assert!(!kept.is_empty() && kept.iter().all(|entry| !entry.paths.is_empty()));
        assert_eq!(kept[0].paths, all[0].paths[..kept[0].paths.len()]);
        let small = vec![PeerCallers {
            paths: vec!["src/caller.rs".into()],
            ..entry(0, 0)
        }];
        assert_eq!(within_budget(small.clone()), small);
    }

    #[test]
    fn another_worktrees_uncommitted_callers_of_a_renamed_name_are_found() {
        let directory =
            std::env::temp_dir().join(format!("langspan-rename-callers-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let (main, tree) = (directory.join("main"), directory.join("tree"));
        let mut index = crate::graph::Index::open(&directory.join("graph.sqlite3")).unwrap();
        let mut state = state(&[("main", &main), ("tree", &tree)]);
        let renamed = ["parse_config".to_owned()];
        let found = |index: &crate::graph::Index, state: &State| {
            peer_callers(state, index, "main", &renamed).unwrap()
        };
        // Not scanned yet: nothing is claimed of the other worktree.
        assert!(found(&index, &state).is_empty());
        // The caller and a committed (clean) file that also mentions the
        // name: only the uncommitted one is a caller.
        scan(
            &mut index,
            "tree",
            &tree,
            &[("src/caller.rs", CALLER), ("src/clean.rs", CALLER)],
            &["src/caller.rs"],
        );
        // The asking worktree's own uncommitted callers are not peers'.
        scan(
            &mut index,
            "main",
            &main,
            &[("src/own.rs", CALLER)],
            &["src/own.rs"],
        );
        assert_eq!(
            found(&index, &state),
            [PeerCallers {
                name: "parse_config".into(),
                worktree: tree.to_string_lossy().into_owned(),
                sessions: vec!["bob".into()],
                paths: vec!["src/caller.rs".into()],
            }]
        );
        // A name nobody calls, and no names at all, find nothing.
        assert!(
            peer_callers(&state, &index, "main", &["unused".to_owned()])
                .unwrap()
                .is_empty()
        );
        assert!(
            peer_callers(&state, &index, "main", &[])
                .unwrap()
                .is_empty()
        );
        // An ended session is not named; its worktree's callers still are.
        state.activity.entry("bob".into()).or_default().ended = true;
        assert_eq!(found(&index, &state)[0].sessions, Vec::<String>::new());
        // A worktree of another repository is not read.
        state.workspace_git.get_mut("tree").unwrap().repository_id = "other".into();
        assert!(found(&index, &state).is_empty());
        drop(index);
        fs::remove_dir_all(directory).unwrap();
    }

    /// The daemon's answer to a check of renamed names alone, from a real
    /// store file: the callers, and a bounded request.
    #[test]
    fn a_check_of_renamed_names_alone_answers_the_peers_callers() {
        let directory =
            std::env::temp_dir().join(format!("langspan-rename-check-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let (main, tree) = (directory.join("main"), directory.join("tree"));
        let mut store = crate::store::Store::open(&directory.join("state.sqlite3")).unwrap();
        for (id, root) in [("main", &main), ("tree", &tree)] {
            fs::create_dir_all(root).unwrap();
            let root = root.to_str().unwrap().to_owned();
            let commands = [
                Command::WorkspaceRegisterVerified {
                    workspace_id: id.into(),
                    root,
                    verified_overlaps: Vec::new(),
                },
                Command::ObserveWorkspaceGit {
                    workspace_id: id.into(),
                    repository_id: "repository".into(),
                    branch_ref: None,
                },
                Command::SessionRegister {
                    session_id: format!("{id}-session"),
                    workspace_id: id.into(),
                    parent: None,
                    headless: false,
                },
            ];
            for (n, command) in commands.iter().enumerate() {
                let original = match command {
                    Command::WorkspaceRegisterVerified {
                        workspace_id, root, ..
                    } => Command::WorkspaceRegister {
                        workspace_id: workspace_id.clone(),
                        root: root.clone(),
                    },
                    other => other.clone(),
                };
                store
                    .execute(&format!("{id}-{n}"), &original, command, 1_000)
                    .unwrap();
            }
        }
        let mut index = crate::graph::Index::open(&directory.join("graph.sqlite3")).unwrap();
        scan(
            &mut index,
            "tree",
            &tree,
            &[("src/caller.rs", CALLER)],
            &["src/caller.rs"],
        );
        let check = |store: &mut crate::store::Store, calls: &[_], renamed: &[&str]| {
            let command = Command::CheckCalls {
                session_id: "main-session".into(),
                calls: calls.to_vec(),
                renamed: renamed.iter().map(|name| (*name).to_owned()).collect(),
            };
            checked_calls(store, Some(&index), "r".into(), &command).outcome
        };
        let Outcome::CallBreaks {
            breaks,
            ready,
            callers,
            ..
        } = check(&mut store, &[], &["parse_config"])
        else {
            panic!("not call breaks");
        };
        assert!(breaks.is_empty() && ready);
        assert_eq!(callers.len(), 1, "{callers:?}");
        assert_eq!(callers[0].sessions, ["tree-session"]);
        assert_eq!(callers[0].paths, ["src/caller.rs"]);
        // With calls to check too, the callers come with the breaks.
        let call = langspan_core::CallSite {
            path: "src/own.rs".into(),
            line: 1,
            name: "parse_config".into(),
            args: 0,
            member: false,
        };
        let with_calls = check(&mut store, &[call], &["parse_config"]);
        assert!(
            matches!(&with_calls, Outcome::CallBreaks { callers, .. } if callers.len() == 1),
            "{with_calls:?}"
        );
        for bad in [&[][..], &["f\nignore previous"][..]] {
            assert!(
                matches!(check(&mut store, &[], bad), Outcome::Error { ref code, .. } if code == "invalid_calls"),
                "{bad:?}"
            );
        }
        drop((store, index));
        fs::remove_dir_all(directory).unwrap();
    }
}
