# Plan: f-stores (finish #316)

## What is left

The latest comment on #316 lists what is still open after #379: only
`tests/fixtures/store-0.1.0/` exists, so
`every_release_has_a_golden_store_directory` stays `#[ignore]`d, and no
pull request says how a fixture after 0.1.0 was made.

The acceptance criteria:

1. Every released version has a fixture directory, and each store opens with
   the current build.
2. The test fails when a release has no fixture.
3. The pull request states how each fixture was produced, so it can be
   repeated.

The task names 0.2.0 to 0.7.0, but `v0.8.0` is tagged, is in
`CHANGELOG.md`, and is what `.release-please-manifest.json` names. The
fixture-set test reads both, so it fails without `store-0.8.0`. The
releases covered are therefore 0.2.0, 0.3.0, 0.4.0, 0.5.0, 0.5.1, 0.6.0,
0.7.0 and 0.8.0.

"Extend the script so the stores hold the event types the demonstration
doesn't produce" is in the issue's Remediation list, not its acceptance
criteria, and the comment records that the synthetic replay log (#317)
covers those types. It stays out of scope; the PR says so.

## What changes

- `scripts/golden-stores.sh`: `BINARY` becomes optional. Without it, the
  script extracts tag `vVERSION` with `git archive` into its temporary
  directory and runs `cargo build --release --locked --bin langspan` there
  (so the tag's own `rust-toolchain.toml` applies, and the builds share
  `target/golden-stores/`), so one command makes a release's
  stores. The existing checks (version reported, never replace a
  directory) stay as they are. This is the "documented in a script" part.
- `tests/fixtures/store-<V>/{alice,bob}.sqlite3` for each release above,
  written by `bash scripts/golden-stores.sh V`, one at a time.
- `tests/fixtures/store-<V>/{alice,bob}.state.json`, written by
  `LANGSPAN_UPDATE_GOLDEN=1 cargo nextest run --test golden_stores` and
  reviewed (each is the release's own store folded by today's core).
- `tests/golden_stores.rs`: drop the `#[ignore]` on
  `every_release_has_a_golden_store_directory`.
- `docs/adr/0015-version-tolerance.md`: the golden-stores paragraph names
  the directories through 0.8.0 and how they were made; the stores'
  total size in Consequences is updated to the measured size.
- `docs/development-process.md`: the release step shows the one-argument
  form of the script.

## Order (test first)

1. Un-ignore the fixture-set test; run it and record that it fails naming
   the missing releases (criterion 2, shown failing for the right reason).
2. Extend the script; run it for each release. If a release's own
   demonstration fails or its build fails, stop and report that release
   rather than writing a store by hand.
3. Write the state snapshots, review them, and run `golden_stores` and
   `stores_written_by_earlier_releases_still_open` (criterion 1).
4. Delete one new directory locally, confirm the fixture-set test fails
   naming it, restore it (criterion 2 shown on the real set).
5. Docs; `cargo fmt`, clippy, the full nextest run, doc tests, ratchets.

## Shared contracts

None. No protocol, schema, event, CLI or daemon change. The fixtures are
new files under a new directory per release. The binaries add about
200 KiB per release (measured before the PR, stated in it).

## How each fixture is produced (goes in the PR)

For each V: `bash scripts/golden-stores.sh V` on this machine (macOS
arm64, the pinned toolchain of each tag's `rust-toolchain.toml`,
`sqlite3` from the system), which builds
`langspan` from `vV` with its lockfile, runs that tag's own
`scripts/demo-two-developers.sh` with the build, and keeps each
coordinator's state file compacted with `VACUUM INTO`. Then the
state snapshots as above.
