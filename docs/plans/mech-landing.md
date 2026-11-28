# Plan: route an agent's push to the default branch through `langspan land` (#536)

Issue #536 has four criteria. This pull request is the first slice and says
"Refs #536":

| Criterion | This slice |
| --- | --- |
| A recorded real-host session shows a push landing in one step after main moved | Not done: the owner records it (tests/live or a retained session), never the fast suite. |
| A failing check blocks the push with a clear reason | Done: the hook denies a direct push to the default branch, and `land` refuses to push on a failed check (exit 6, existing tests); the denial says so. |
| The benchmark records push retries per arm | Not done: a later slice (the driver tells agents not to push today, so retries need a scenario change). |
| The safety rules are in the landing ADR | Done: ADR 0007 gains a "Routing an agent's push" section. |

## Readings of the decision, and the one chosen

"Never pushes the default branch unless the repository configures that" can
mean (a) LangSpan's own `land` must not push the default branch without
configuration, or (b) an agent's direct `git push` to the default branch is
routed to `land` unless the repository allows direct pushes. Reading (a) would
break `land --onto main`, which is shipped and documented. This plan takes (b):
the only way an agent's commits reach the default branch is `land`, which never
forces and never pushes on a failed check; the repository opts out with
`git config langspan.defaultBranchPush direct`.

## Behaviour

At `PreToolUse` for a shell call, before the commit guard:

> Changed in review: steps 1 to 3 read the destination from the command, and
> review kept finding pushes they misread. The hook now refuses a push combined
> with another command. It asks Git with a dry run of a lone push, and refuses
> one whose dry run fails. See ADR 0007, "Routing an agent's push".

1. `crate::shell::git_pushes(command)` (new, pure, `src/shell/git_push.rs`)
   reads each `git push` in the line with the existing lenient lexer
   (`Lexer`, `command_words`): its directory (`cd`, `-C`), remote, refspecs,
   and whether it forces (`-f`, `--force`, `--force-with-lease`,
   `--force-if-includes`, a `+` refspec), deletes (`-d`, `--delete`, `:dst`)
   or pushes everything (`--all`, `--mirror`). Tags-only pushes and
   `--dry-run` are not pushes of a branch.
2. `push_route::route` (new, `src/hook/push_route.rs`) resolves, with
   `git --no-optional-locks` and a bound taken from the hook deadline:
   the remote's default branch (`symbolic-ref refs/remotes/REMOTE/HEAD`),
   the current branch, and for a push without refspecs its `@{push}`
   destination. A push whose destination is the default branch (or `--all`,
   `--mirror`) is denied with `coverage: "denied_push"` and a reason that
   names the exact command:
   `langspan --state-dir DIR land WORKSPACE --onto MAIN --name NAME BRANCH --apply --rebase --check-again -- COMMAND`,
   with CHECK and TEST_COMMAND as placeholders the agent fills in (built:
   reading the workspace's automatic check over the socket was left out to
   keep the hook free of a daemon call before admission). A forced push to
   the default branch is denied with the same reason and says landing never
   forces.
3. Anything it cannot read (an expansion where a remote or refspec is needed,
   `--git-dir`, Git failing, no `REMOTE/HEAD`) passes with a detail, like the
   commit guard: routing is cooperative (AGENTS.md), and an unreadable line
   must not hold every unusual push. Pushes to other branches pass untouched.
4. `git config --get langspan.defaultBranchPush` = `direct` passes every push
   (the repository's opt-in). The denial does not mention this setting.

Each denial is repeated for every attempt (deterministic), unlike the
commit guard's hold-once.

## Test-first order

1. `src/shell/git_push.rs` unit tests (pure, no Git): plain push, remote only,
   `HEAD:main`, `feat:refs/heads/main`, `+main`, `--force`, `-C dir`, `cd`,
   `--all`, `--tags`, `--dry-run`, expansion is an error, push inside `&&`,
   wrappers. Write, watch fail (no function), implement.
2. `src/hook/push_route.rs` tests with a real temporary repository and a
   local file remote (Git's `symbolic-ref`/`@{push}` resolution is what is
   verified, so a Git test is justified and says so): push to main denied
   with the land command; push to a feature branch passes; bare `git push`
   on main denied; forced push to main denied and says never forced;
   `langspan.defaultBranchPush direct` passes; no `origin/HEAD` passes with a
   detail. Break the default-branch comparison once and confirm the tests
   catch it.
3. Wire it into `shell_pre` (`src/hook/shell_hooks.rs`): a denied push returns
   before the snapshot, so no run is recorded.

## Shared contracts touched

None on the wire: no protocol field, no `Command`/`Outcome` variant, no golden
CLI output. The hook reply is an ordinary `PreToolUse` deny already used by
the commit guard and admission. Shared files edited: `src/hook/shell_hooks.rs`
(one call), `src/hook.rs` and `src/shell.rs` (a `mod` line and a re-export),
ADR 0007, `docs/usage/checks-and-landing.md`, `docs/IMPLEMENTATION_STATUS.md`.
No new trust boundary: the hook already runs `git --no-optional-locks` in the
checkout (commit guard), and reads the daemon's status over its socket; the
threat model is unchanged.

Ratchets: record baselines for the two new files; tighten any that shrank.
