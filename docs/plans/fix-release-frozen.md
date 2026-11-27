# Plan: release PRs pass the frozen event-type test (#518)

## Problem

`every_published_release_has_a_frozen_list` (`tests/protocol.rs`) treats
every `## [x.y.z]` heading in `CHANGELOG.md` as published. A release-please
PR adds the heading for the version it creates (and bumps `Cargo.toml`), so
it can never hold `fixtures/event-types/<new>.txt`, and every release PR
fails (#504 for 0.9.0).

## Fix chosen: published versions come from the tags

Of the issue's two options, take the first. A changelog heading counts as
published when it is not the version this build carries
(`CARGO_PKG_VERSION`), or when a `v<version>` tag exists. Reasons:

- release-please adds a heading only for the version a release PR creates,
  so every other heading is a release that has been tagged.
- The version being released is published only once its tag exists. On the
  release PR, and on main between the merge and the tag, there is no tag.
- Generating the list in the release PR (option two) needs a workflow step
  that pushes to release-please's branch with a token; more moving parts for
  the same protection.

Tags are read with `git --no-optional-locks tag --list v<version>` in the
repository root. A failing `git` fails the test with what failed and what to
do; it never counts as "not tagged".

Where the protection still holds:

- Locally, clones have the tags, so a tagged release without a list fails.
- In the merge queue, checkout fetches all history and tags
  (`fetch-depth: 0`), so once `v0.9.0` exists every queued PR fails until
  its list lands.
- The next release PR bumps the version, so the previous one is an older
  heading and is checked with no tag at all.
- Pull request runs check out two commits without tags, so there the newest
  release is checked only from the next release onward. Stated in the test's
  comment.

## Changes

1. `tests/protocol.rs`
   - New pure function `published_releases(changelog, current, tagged)`:
     the changelog headings, minus `current` unless `tagged(current)`.
   - Test first, in-process (no git, no files): a simulated release PR
     (changelog with `## [0.9.0]`, current `0.9.0`, no tag) leaves 0.9.0
     out and keeps 0.8.0; the same with the tag present includes 0.9.0;
     an older heading is published without its tag. Watch it fail against
     the current rule (which returns 0.9.0 untagged) before switching.
   - `every_published_release_has_a_frozen_list` uses the function, with
     `env!("CARGO_PKG_VERSION")` and a `git tag --list` lookup. Its
     assertions on missing lists and rows stay as they are, so a tagged
     release without a list still fails.
   - Break the rule once (drop the tag check) and confirm the tagged-case
     test catches it.
2. `docs/development-process.md` (Releases, the paragraph naming the test):
   say the list lands in the first PR after the tag, not the release PR,
   and that the merge queue fails until it does.
3. This plan.

No protocol, wire format, daemon or CLI change; no shared contract touched.
`tests/` has no ratchet baselines, so none to record.

## Checks

- `cargo nextest run --test protocol` (new tests and the existing one).
- Simulate #504 locally in a throwaway worktree: bump `Cargo.toml` to 0.9.0
  and add a `## [0.9.0]` heading; the test passes. Create a local `v0.9.0`
  tag there; it fails naming 0.9.0. Delete the tag.
- `cargo fmt`, clippy `-D warnings`, the full `cargo nextest run`.
- #504 going green needs this merged and #504 rebased; reported as such.
