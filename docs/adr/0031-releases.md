# ADR 0031: Releases: versions, archives and what proves where they came from

**Status:** Implementation decision, 24 November 2026. Written in the private
repository; it takes effect only in the public repository
`Langspan/LangSpan.dev`. No release has been made yet.

## Context

LangSpan has been a source build. Publishing binaries makes three things
public contracts that are costly to change later: the version scheme, the
archive names (a Homebrew formula and `cargo binstall` metadata are built
from them), and how a user checks that an archive is the one this
repository's workflow built.

## Decision

- **Versions.** SemVer, starting at 0.1.0. release-please
  (`release-please-config.json`, `release-type: rust`) reads Conventional
  Commit titles on `main`: `feat:` raises the minor version, `fix:`, `perf:`,
  `revert:` and `build:` the patch version; before 1.0 a breaking change
  raises the minor version. It bumps the literal `version` in all three
  `Cargo.toml` files and `Cargo.lock`. The versions stay literal because its
  Cargo updater rewrites `package.version` and has no case for
  `version.workspace = true` (`src/updaters/rust/cargo-toml.ts` at v17.11.2).
- **Tags and Releases.** Merging the release pull request creates the tag
  `vX.Y.Z` at once and a draft Release (`draft`, `force-tag-creation`). The
  same workflow run builds and attaches the assets, then publishes the
  draft, so no one sees a release without its assets. A release is
  immutable only with the repository setting for release immutability
  turned on, which was off for 0.3.0. Nothing runs on the tag: a tag made
  with `GITHUB_TOKEN` starts no workflow.
- **Testing the shipped binary.** Each build job runs the two-developer
  demonstration on the binary it packages, on that target's runner, before
  publishing. After publishing, `verify-published` downloads every archive
  on its own runner, checks `SHA256SUMS` and the attestations, installs it
  with the published `install.sh` and runs the demonstration again. If that
  fails, `withdraw` marks the release a prerelease, restores the previous
  latest release and opens an issue
  ([runbook](../development-process.md#withdraw-a-release)).
- **Archives.** `langspan-vX.Y.Z-TARGET.tar.gz` for `aarch64-apple-darwin`,
  `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu`, each built natively on its own runner with the
  pinned toolchain and `--locked`. Each holds a directory of the same name
  with `langspan`, `LICENSE`, `NOTICE`, `README.md`, `CHANGELOG.md` and
  `THIRD-PARTY-LICENSES.html`. Linux builds run on Ubuntu 22.04, so they need
  glibc 2.35 or later. musl and Windows are not built.
- **Evidence.** `SHA256SUMS` lists every archive and SBOM. A SLSA build
  provenance attestation covers every file in it, and its Sigstore bundle is
  a release asset (`langspan-vX.Y.Z.sigstore.json`). Each archive also has
  an SBOM attestation for its CycloneDX SBOM (`.cdx.json`). Users check with
  `gh attestation verify`. Builds are not promised to be bit-for-bit
  reproducible: the C code and the macOS linker vary between builds.
- **Signing on macOS.** Archives are not signed with a Developer ID or
  notarised. That needs the owner's Apple Developer account.

## Alternatives considered

**release-plz** (v0.4.3) is the Rust-native choice. It reads
`version.workspace = true`, writes the changelog with git-cliff, runs
cargo-semver-checks, and publishes to crates.io with trusted publishing built
in. It was not chosen now because:

- It versions, tags and releases each package. This workspace ships one
  binary from three unpublished crates; release-plz would need `git_only`,
  a `version_group` and release settings on the two library crates to make
  one tag `vX.Y.Z` and one Release, where release-please's `rust` strategy
  already treats the root package as the release and bumps the members.
- Its main strength, crates.io publishing, is not decided; the crates stay
  `publish = false`.
- It has the same `GITHUB_TOKEN` limits: its release pull request starts no
  checks and its tag starts no workflow, so the same `workflow_dispatch`
  step and same-workflow build jobs would be needed.
- The owner already runs release-please on another project, so one process
  covers both.

Revisit if LangSpan is published to crates.io: release-plz's publishing and
semver checks would then outweigh the extra configuration.

**cargo-dist** builds and hosts artifacts on a pushed tag; a tag created
with `GITHUB_TOKEN` would not start it, and it generates and owns its own
workflow. **cargo-release** runs on a maintainer's machine with their
credentials and makes no reviewable release pull request.

## Consequences

- No long-lived secret: every job uses `GITHUB_TOKEN` with the permissions
  that job names, and attestations use the run's OIDC identity. Attestation
  jobs compile nothing, so no dependency's build script runs where the
  signing token can be requested.
- Artifact attestations need a public repository on GitHub Free. The
  workflow is guarded to `Langspan/LangSpan.dev` and skipped elsewhere.
- Homebrew installs come from a tap, `Langspan/homebrew-tap`, which does not
  exist yet; `packaging/homebrew/` holds the formula template.
- Renaming an archive or changing a target breaks the formula and any
  script that downloads releases; that is a breaking change for packagers.
