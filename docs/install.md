# Install and verify LangSpan

Download the [latest stable release](https://github.com/Langspan/LangSpan.dev/releases/latest) or use the [platform downloads](https://langspan.dev/#start). Choose the archive for your operating system and processor; run `uname -m` if needed.

| Platform | Archive suffix |
| --- | --- |
| macOS Apple silicon | `aarch64-apple-darwin.tar.gz` |
| macOS Intel | `x86_64-apple-darwin.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu.tar.gz` |
| Linux x86-64 | `x86_64-unknown-linux-gnu.tar.gz` |

Linux requires glibc 2.35 or later. macOS archives are not Apple Developer ID-signed; verify them before allowing execution in System Settings. Add the installation directory to your shell startup file if needed. The Homebrew tap and install script are not published.

<a id="verify-a-download"></a>

### Verify the binary

Each release includes `SHA256SUMS`, a CycloneDX SBOM per target and a
provenance bundle. Download the archive and checksum file from the same release.
For example, in an empty directory on macOS Apple silicon, use the
[GitHub CLI](https://cli.github.com/) to resolve the latest stable tag once
and download its matching archive and checksums:

```sh
tag=$(gh release view -R Langspan/LangSpan.dev --json tagName --jq .tagName)
archive="langspan-$tag-aarch64-apple-darwin.tar.gz"
gh release download "$tag" -R Langspan/LangSpan.dev \
  --pattern "$archive" --pattern SHA256SUMS
awk -v name="$archive" '$2 == name' SHA256SUMS | shasum -a 256 -c
gh attestation verify "$archive" \
  -R Langspan/LangSpan.dev \
  --signer-workflow Langspan/LangSpan.dev/.github/workflows/release.yml
```

Substitute the archive suffix for your platform (on Linux, use `sha256sum -c`).
The attestation command checks that the archive was built by this repository's
release workflow. Add `--predicate-type https://cyclonedx.org/bom` to check
its SBOM attestation instead. Builds are not bit-for-bit reproducible; the
C code and the macOS linker vary between builds.
[ADR 0031](adr/0031-releases.md) records how releases are made.

### Build from source

```sh
git clone https://github.com/Langspan/LangSpan.dev.git
cd LangSpan.dev
cargo build --workspace --release --locked
export PATH="$PWD/target/release:$PATH"
langspan setup
```

Requires Rust and a C compiler. Restart a running coordinator after upgrading. See the [engineering guide](engineering.md).

### Connect a native agent

For [user-level setup](usage.md#all-repositories-for-this-user), use `langspan setup`. For a single checkout, see [project-level installation](reference-overview.md#one-project). To remove installed hooks, MCP configuration and the service, run `langspan uninstall --apply`.

## Install and connect

After verifying and extracting the archive, close Claude Code if running and execute from the extracted directory:

```sh
mkdir -p "$HOME/.local/bin"
install -m 0755 langspan "$HOME/.local/bin/langspan"
export PATH="$HOME/.local/bin:$PATH"
langspan setup
```

Review and confirm the setup plan. In Codex, use `/hooks` to review and trust the hooks. Open your agents in a Git repository; run `langspan dashboard --open` to inspect their activity.

See the [usage guide](usage.md#set-up-in-one-command) and [host evidence](IMPLEMENTATION_STATUS.md).
