# Plan: shared repository, coordinator and hook helpers in tests/support (#374)

## Starting point (main at 8f4467df)

- 41 files under `tests/` define their own `fn git(` and 37 their own
  `fn hook(`. `tests/support/mod.rs` has process and wait helpers, and no
  repository, coordinator or hook-call helper.
- The largest copies (assessment F9): `World::new`/`start` in `tests/recall.rs`
  and `tests/today.rs` (48 lines), the coordinator start in
  `tests/break_alerts.rs` and `tests/code_briefing.rs` (35), and the hook call
  in `anchored_notes`, `break_alerts`, `formatter_notice` and
  `shell_concurrent_edits` (29 to 32).

## Changes

1. `tests/support/mod.rs` gains three helpers, each `#[allow(dead_code)]` like
   the rest:
   - `git(root, args) -> String`: `git -C root -c user.name=t -c
     user.email=t@example.invalid ARGS` with `GIT_CONFIG_NOSYSTEM=1` and
     `GIT_CONFIG_GLOBAL=/dev/null`; fails the test with git's output unless it
     succeeds; returns stdout untrimmed.
   - `coordinator(state) -> Owned`: `langspan daemon --state-dir STATE`
     with `LANGSPAN_AUTOSTART=off` and stdout and stderr discarded, returned
     once it answers a status request (`daemon_ready`); killed on drop.
   - `hook_call(state, vendor, workspace, root, extra, event) -> Output`: one
     `langspan hook VENDOR --state-dir STATE --workspace W --root ROOT EXTRA`
     with `LANGSPAN_AUTOSTART=off`, the event's JSON on stdin, stdout and
     stderr captured.
2. These 26 files drop their own `fn git(` for `support::git` (callers
   unchanged): adopt, anchored_notes, antigravity, auto_checks, between_steps,
   break_alerts, briefing, briefing_delta, code_briefing, coordination_policy,
   cursor, dashboard, formatter_notice, graph, graph_golden, line_ownership,
   messages, outline_reads, recall, remote, rename_holds,
   shell_concurrent_edits, shell_writes, spool, today, wider_checks.
   `recall` and `today` have a `World::git` method; its callers become
   `git(&world.work(), ...)`. 41 files become 15.
   Kept local, because their copy differs on purpose: fixed commit dates
   (`cli_transcript`, `hook_replies`), `--no-optional-locks` or a byte or
   `(bool, String)` result (`git_observer`, `store_scan_integration`,
   `repository_shapes`), trimmed output (`landing`, `remote_backoff`), no
   isolation for a credential-helper test (`network_git`), and the rest.
3. The coordinator start in `recall`, `today`, `break_alerts` and
   `code_briefing` becomes `support::coordinator` (`daemon: Option<Owned>`).
   The hook-call body in `today`, `recall` (`session_start`), `break_alerts`,
   `code_briefing` and `anchored_notes` becomes `support::hook_call`; each
   file's own parsing of the reply stays where it is.
   Kept local, found while moving: the hook calls in `formatter_notice` and
   `shell_concurrent_edits` run the hook without system or global Git
   configuration, which `hook_call` doesn't set, and `anchored_notes`
   starts its coordinator with a test scan interval.
4. Ratchet: `no_ratchet_grows_past_its_baseline` in `tests/ratchets.rs`
   also counts the files under `tests/` (outside `tests/support/`) that
   define `fn git(` or `fn hook(` and fails when the count is above
   `OWN_GIT_OR_HOOK_FILES`, naming the files; when it falls it says to
   lower the constant. It is part of the existing test, so the test count
   is unchanged, and in Rust rather than `scripts/ratchets.py`, which
   measures production code only. One paragraph in
   `docs/development-process.md#ratchets` describes it.

## Behaviour

No assertion changes and no test added or removed among the moved files. Test
setup changes in two small ways, said in the PR: every moved `git` call now
runs with the same identity and without system or global Git configuration
(a test can no longer read the machine's Git configuration), and a
coordinator is ready when it answers a status request rather than when
`langspan status` exits 0 (the same request). The total test count is
unchanged.

## Order (test first)

1. Add the ratchet check; see it fail on main's count (51 files with either).
2. Add the helpers, move the files, building as they go.
3. Set the ceiling to the new count (39) and see the ratchet pass; run the
   whole suite, `cargo fmt`, clippy, and the assessment's `dupes.py`
   measure (tests, 10-line windows) for the new duplication figure.

## Shared contracts touched

None of the protocol, CLI, daemon or golden files. `tests/support/mod.rs` is
shared by every test binary that includes it; the additions only add items.
