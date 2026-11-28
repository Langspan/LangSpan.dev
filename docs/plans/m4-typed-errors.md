# Plan: typed errors at the client and Git edges (issue #368, first slice)

Issue #368 asks for an error enum in each of `store`, `client` and `git`, and a
ratchet on the count of `Result<_, String>` signatures. All three in one pull
request would be well over the ~300-line size limit, so this pull request lands
the first independent slice and says "Refs #368".

- **In this slice:** the `client` and `git` enums, the one caller that decides
  from client error text (`src/hook.rs`), and the ratchet.
- **Left for the next slice:** `store`. `StoreResult` is `Box<dyn Error>` over
  rusqlite, serde and core errors in `src/store.rs` and seven submodules.

## What changes where

1. `src/client.rs`: add `pub enum ClientError { Unavailable(String), TimedOut(String), Refused(String), Corrupt(String) }`.
   - `Display` prints the message, which is the same text as today, and the type implements `std::error::Error`.
   - `call`, `call_until`, `exchange`, `read_all` (and its `ask`), `full_state`, `status_with` (and its `ask`), `status_from` and the non-Unix stubs return it.
   - A socket or connect failure becomes `Unavailable`; an `io::ErrorKind::TimedOut` there, or a passed deadline, becomes `TimedOut`.
   - A daemon error outcome, an older daemon that can't carry the command, or a failed core `apply` becomes `Refused`. An unreadable or mismatched answer becomes `Corrupt`.
2. `src/hook.rs:161`: `Session::call_by` matches `ClientError::TimedOut` for the `timeout` coverage instead of `text.contains("deadline" | "timed out")`.
3. `src/git.rs`: add `pub enum GitError { Unavailable(String), TimedOut, Failed(String), Corrupt(String), Io(String), Invalid(String) }`.
   - `Display` keeps every current message. `impl Error` and `From<GitError> for String` mean callers that use `?` into `String` don't change.
   - Every public function, plus `owner_ssh`, `crlf_committed`, `run`, `drain` and `drain_tail`, returns it.
4. The callers that the compile experiment showed don't build after the change. Each fix is the smallest one that compiles; none changes behaviour.
   - `src/auto_check.rs` (its `ask` closure builds `ClientError`)
   - `src/landing.rs` (`?` conversion)
   - `src/hook/workspace.rs` (`.map_err(|e| unsupported(e.into()))`)
   - `src/observer.rs` and `src/rewrite.rs` (`.map_err(String::from)`)
   - `src/remote.rs` (one `From<GitError> for Refusal`)
   - `src/daemon/index_queries.rs` (`detail.to_string()` inside `json!`)
   - `tests/git_output.rs` (match the variant)
5. The ratchet, in `scripts/ratchets.py`:
   - Count production `-> Result<…, String>` return types, in fn and closure signatures, per crate.
   - Compare the count with `scripts/ratchets/string-errors.tsv` (rows `CRATE\tN`). It fails if the count grew, and reports an improvement, with the row to lower, if it shrank.
   - It is one file with a per-crate total, not a row in every per-file baseline, so this slice doesn't touch about 60 baseline files that other tasks are editing.
   - Documented in the script's docstring and in `docs/development-process.md#ratchets`.
6. Baselines:
   - `--record` `src/git.rs` and `src/client.rs`, whose `pub` count grows by the enums.
   - Write the new `string-errors.tsv` at the lowered count. At `cf6579b6` the approximate grep gives 192 before and 173 after; the scanner's exact production count goes in the PR.

## Test-first order

1. `scripts/ratchets/test_ratchets.py`: a fixture with string-typed signatures, a closure, a test-only fn and a nested generic. The count is exact, it fails when it grows, and it reports an improvement when it shrinks. Watch the tests fail, then implement.
2. `src/client.rs` unit tests (in-process; a socket only where the existing tests already use one): a past deadline gives `ClientError::TimedOut`, and a missing socket gives `Unavailable`, each with today's message text. Watch them fail to compile or fail, then implement.
3. `src/git.rs` unit test: a past deadline gives `GitError::TimedOut` with the text "Git operation deadline elapsed". This is a process test, because what it checks is the run of the Git child process.
4. `src/hook.rs`: the coverage class comes from the variant. A small pure function `coverage(&ClientError)` is tested in-process.
5. The CLI transcript goldens (`tests/cli_transcript.rs`), the contract and property tests, and the whole nextest suite pass unchanged.

## Shared contracts touched

- No wire format, protocol version or golden changes. Messages users see are byte-for-byte the same.
- Library surface: `langspan::client::*` and `langspan::git::*` error types change from `Box<dyn Error>` and `String` to the enums. Both implement `std::error::Error` and `Display`, so the CLI edge's `Box<dyn Error>` still takes them with `?`.
- The ratchet script gains a check, and the ratchets doc section gains a paragraph.
