# Contributing

This covers how a change gets into LangSpan, whether a person or an agent
writes it. The rules for building it are in
[docs/engineering.md](docs/engineering.md); read that first. Product context
is in [AGENTS.md](AGENTS.md).

## Set up

You need:

- Rust through rustup. `rust-toolchain.toml` pins the version, and
  `rustup toolchain install` in the repository installs it.
- A C compiler, for the bundled SQLite and the Tree-sitter grammars.
- Git, and `bash` for the demonstration script.
- `cargo-deny` and `cargo-machete` for the dependency checks:
  `cargo install --locked cargo-deny --version 0.22.1` and
  `cargo install --locked cargo-machete --version 0.10.1`.
- For changes under `.github/`, `zizmor` 1.33.1 to audit the workflows
  (`zizmor .`); CI runs it too.

The integration tests bind Unix sockets and start child processes. A sandbox
that forbids either cannot run the whole suite; say so in the pull request
rather than skipping tests.

## Make a change

1. Open an issue first for anything larger than a small fix, so the approach
   can be agreed before the work.
2. Branch from `main`. One concern per branch and per pull request.
3. Write the failing test first and watch it fail for the right reason, then
   make it pass.
4. Run the full check until it passes:

   ```sh
   sh scripts/check.sh
   ```

   It checks formatting, runs clippy with warnings as errors, runs every test,
   builds the release binary, runs `cargo deny` and `cargo machete`, and ends with the
   two-developer demonstration. CI runs the same script on Linux and macOS.
5. Open a pull request and fill in the template.

## Pull request titles

Pull requests are squash-merged, so the title becomes the commit on `main`,
and release notes and version numbers are made from those commits. Use a
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) title:

| Type | Use for | Release effect |
| --- | --- | --- |
| `feat:` | something a user can now do | next minor version |
| `fix:` | a bug fix | next patch version |
| `perf:` | a speed or size improvement | next patch version |
| `build:` | dependency updates (Dependabot uses `build(deps):`) | next patch version |
| `revert:` | undoing an earlier commit | next patch version |
| `docs:`, `test:`, `refactor:`, `ci:`, `chore:` | everything else | no release by itself |

A breaking change adds `!` after the type, as in `feat!:`. Before 1.0 it
raises the minor version.

## What a pull request needs

- The commands you ran and their results. A failing test is quoted as it
  failed, never summarised as passing.
- A row in [docs/dependencies.md](docs/dependencies.md) for any new
  dependency, before it is added.
- An update to [docs/security/threat-model.md](docs/security/threat-model.md)
  for any change that adds or widens a trust boundary.
- Updated docs where behaviour changed.

Copied code needs a licence that allows it, and is listed in [NOTICE](NOTICE).
Never copy from source whose licence forbids it.

## Licence

LangSpan is licensed under the [Apache License 2.0](LICENSE). Under its
section 5, a contribution you submit for inclusion is licensed under the same
terms, unless you say otherwise.

There is no contributor licence agreement. Each commit carries a Developer
Certificate of Origin sign-off ([developercertificate.org](https://developercertificate.org/)):
`git commit -s` adds the `Signed-off-by:` line that says you have the right to
submit it under the project's licence.

Contributions written with a coding agent are welcome. Say so in the commit
(a `Co-Authored-By:` line naming the agent) and in the pull request. They pass
the same checks and the same human review as any other change, and the person
who opens the pull request signs off for it and answers for it.

## Conduct and security

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md). Report
a suspected vulnerability privately, as [SECURITY.md](SECURITY.md) describes,
never in a public issue.
