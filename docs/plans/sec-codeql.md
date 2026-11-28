# Plan: triage of the open CodeQL alerts (sec-codeql)

Input: `target/codeql/alerts.json` (70 open alerts) and `target/codeql/rust.sarif`
(the Rust analysis with each result's code flow), from commit `075ee3a5`.

Every one of the 70 alerts is `rust/cleartext-logging` (CWE-312/359/532). The
query treats `println!`, `eprintln!`, `assert!` messages and the like as log
sinks, and takes a value as sensitive by its name: anything called `secret`,
`*_secret(...)` or `session_id`. I traced each result's code flow from source
to sink in the code.

**Decision: all 70 are false positives. No code change is planned.** The
remedy is to dismiss each alert on GitHub as "false positive" with the reason
from its group below. That is an outward-facing action on the repository's
code-scanning state, so it is left to the owner (or the orchestrator with the
owner's approval); this task does not call the API.

A configuration change can't suppress them in the repository either: the
analysis is GitHub's default CodeQL setup (`dynamic/github-code-scanning/codeql`),
which reads no query filter or `paths-ignore` file from the repository.
Switching to an advanced-setup workflow only to exclude this query would add a
workflow and a trust surface. I don't recommend it without the owner's
decision.

## Group A: session IDs printed as CLI output (37 alerts)

Source: a `session_id` field (or `holder.session_id`, `waiter.session_id`,
`AdoptReport::session_id` through `report.text()`). Sink: `println!` of the
human output of a CLI command, on the owner's own terminal.

Why it is not sensitive: a LangSpan session ID is a cooperative label, not a
credential. The threat model says so directly ("The push turn's holder
(`session_id`) is a cooperative label, as for every session ID"; "A briefing
names sessions by the host-supplied session ID ... LangSpan does not
authenticate agents to each other"; `docs/security/threat-model.md`). Session
IDs already appear by design in `status`, `events`, briefings, desktop notices
and the published presence ref (prefixes). Knowing one grants nothing that
reaching the owner-only socket doesn't already grant. The output is the
command's answer to the person who ran it, not a log file.

| Alert | Location | Value |
| --- | --- | --- |
| 77 | `src/cli/land.rs:194` | landing holder's `session_id` (flow from line 178) |
| 78 | `src/cli/land.rs:467` | waiting landing's `session_id` (flow from line 470) |
| 80 | `src/cli/local.rs:174` | `AdoptReport.session_id` via `report.text()` (`src/adopt.rs:56`, `:60`) |
| 50 | `src/cli/render.rs:120` | `session.session_id` |
| 51 | `src/cli/render.rs:124` | `liveness.session_id` |
| 52 | `src/cli/render.rs:134` | `session_id` |
| 53 | `src/cli/render.rs:138` | `session_id` |
| 54 | `src/cli/render.rs:142` | `session_id` |
| 55 | `src/cli/render.rs:145` | `intent.session_id` |
| 56 | `src/cli/render.rs:150` | `session_id` |
| 57 | `src/cli/render.rs:152` | `lease.session_id` |
| 58 | `src/cli/render.rs:164` | `holder.session_id` |
| 59 | `src/cli/render.rs:224` | `session_id` |
| 60 | `src/cli/render.rs:227` | `session_id` |
| 61 | `src/cli/render.rs:231` | `session_id` |
| 62 | `src/cli/render.rs:237` | `session_id` |
| 63 | `src/cli/render.rs:246` | `session_id` |
| 64 | `src/cli/render.rs:249` | `adoption.session_id` |
| 65 | `src/cli/render.rs:257` | `session_id` |
| 66 | `src/cli/render.rs:263` | `session_id` |
| 67 | `src/cli/render.rs:312` | `entry.session_id` |
| 68 | `src/cli/render.rs:329` | `session_id` |
| 69 | `src/cli/render.rs:527` | `run.session_id` |
| 70 | `src/cli/render.rs:574` | `session_id` |
| 71 | `src/cli/render.rs:596` | `session_id` |
| 72 | `src/cli/render.rs:714` | `session_id` |
| 73 | `src/cli/render.rs:735` | `session_id` |
| 40 | `src/cli/render/messages.rs:7` | `session_id` |
| 41 | `src/cli/render/messages.rs:31` | `session_id` |
| 42 | `src/cli/render/messages.rs:78` | `session_id` |
| 43 | `src/cli/render/messages.rs:112` | `recipient.session_id` |
| 44 | `src/cli/render/recaps.rs:28` | `session.session_id` |
| 45 | `src/cli/render/recaps.rs:114` | `session_id` |
| 46 | `src/cli/render/state.rs:17` | `session.session_id` |
| 47 | `src/cli/render/state.rs:53` | `adoption.session_id` |
| 48 | `src/cli/render/state.rs:59` | `intent.session_id` |
| 49 | `src/cli/render/state.rs:72` | `lease.session_id` |

Alert 80 has two flows (`src/adopt.rs:56` and `:60`), both
the same session ID.

## Group B: the outcome of `refuse_secret` (26 alerts)

Source (every flow): `crate::hook::refuse_secret(original)` at
`src/store.rs:1021`, carried as `Execution.outcome` out of `Store::execute`.
Sink: an `assert!`/`assert_eq!` failure message (`{outcome:?}`,
`{:?}`) in a test.

Why it is not sensitive: `refuse_secret` (`src/text/redact.rs:57`) returns
only a constant `Outcome::Error` with code `secret_in_test_name` and fixed
message text. It never carries the secret-looking input; it exists precisely
so that input is refused before a receipt or event holds it. The query flags
the value because the function's name contains "secret". The sinks are test
assertion messages, printed only when a test fails.

| Alert | Location |
| --- | --- |
| 7 | `src/daemon.rs:1685` (unit test) |
| 8 | `src/store.rs:2527` (unit test) |
| 12 | `tests/retention.rs:490` |
| 13 | `tests/retention.rs:542` |
| 14 | `tests/retention.rs:561` |
| 15 | `tests/retention.rs:621` |
| 16 | `tests/retention.rs:672` |
| 17 | `tests/retention.rs:687` |
| 18 | `tests/retention.rs:728` |
| 20 | `tests/retention.rs:1204` |
| 21 | `tests/retention.rs:1278` |
| 22 | `tests/retention.rs:1322` |
| 23 | `tests/retention.rs:1471` |
| 24 | `tests/retention.rs:1738` |
| 25 | `tests/retention.rs:1760` |
| 26 | `tests/retention.rs:1830` |
| 27 | `tests/status_history.rs:69` |
| 28 | `tests/status_history.rs:388` |
| 29 | `tests/store.rs:166` |
| 30 | `tests/store.rs:238` |
| 32 | `tests/store.rs:595` |
| 33 | `tests/store.rs:801` |
| 34 | `tests/store.rs:1088` |
| 31 | `tests/store_scan_integration.rs:120` |
| 35 | `tests/store_scan_integration.rs:577` |
| 36 | `tests/store_scan_integration.rs:646` |

With Group A's 37 and Group C's 7, that makes 70.

## Group C: synthetic secrets in test fixtures (7 alerts)

Source: a test-local variable named `secret`, holding a made-up value the test
feeds in to prove redaction removes it. Sink: that test's assertion message,
printed only on failure, to name which fixture leaked. None is a real
credential; the shapes are built from parts or are placeholders, so no real
token is in the repository.

| Alert | Location | Value |
| --- | --- | --- |
| 79 | `src/text/redact.rs:560` | provider-key shapes from `secret_shape(..)` in `provider_key_shapes_are_redacted` |
| 9 | `tests/daemon.rs:496` | the literals `"=a"`, `"=b"`, `"--token c"`, `"--secret d"` |
| 10 | `tests/git_output.rs:199` | `"alice"`, `"hunter2"`, a generated `token`, `"\x1b"` |
| 38 | `tests/landing.rs:1600` | the daemon's JSON reply to a check whose path is a generated `ghp_` + `"aB3"` × 12 |
| 39 | `tests/landing.rs:1601` | the same reply |
| 11 | `tests/landing.rs:1602` | the same reply |
| 19 | `tests/shell_concurrent_edits.rs:323` | a command holding the placeholder `ghp_0123456789abcdefghijABCDEFGHIJ012345` (flow from line 789) |

## Why a sanitizer model would not help

The task asked for a CodeQL model that marks the redaction functions as
sanitizers. None of the 70 flows passes through a redaction function, so such
a model would remove none of them:

- Group A and C sources are values named `session_id` or `secret`. Their flows
  go straight to the print or assertion and never reach `redact`,
  `redact_command` or `redact_check`.
- In Group B the query treats `refuse_secret` itself as the source, because
  of its name. A model marking it as a barrier would not change that.

That argument holds whatever model formats the default setup accepts. I did
not check whether the default setup loads Rust model packs from the
repository, and no model file is added.

## Outcome

The owner decided on #481 to dismiss the alerts as false positives (approval
recorded there on 2026-11-27). The owner re-triaged against `main` at
`f4fd976`, where there were 72 alerts. Alerts 81 and 82 are new, and both are
Group B. All 72 are dismissed: 37 in Group A, 28 in Group B and 7 in Group C.
None is left open, and #481 is closed.

## Changes

No source, test or configuration file changes. The only file this task
writes is this plan, which records the triage.
