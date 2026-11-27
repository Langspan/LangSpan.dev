# Install and verify LangSpan

LangSpan ships as one binary for macOS and Linux. Each [release](https://github.com/Langspan/LangSpan.dev/releases/latest) has an archive per target, a `SHA256SUMS` file, an SBOM per target and a provenance bundle. You need only the archive for your target and `SHA256SUMS`. To check your platform, run `uname -sm`.

| Platform | Target |
| --- | --- |
| macOS, Apple silicon (`Darwin arm64`) | `aarch64-apple-darwin` |
| macOS, Intel (`Darwin x86_64`) | `x86_64-apple-darwin` |
| Linux, ARM64 (`Linux aarch64`) | `aarch64-unknown-linux-gnu` |
| Linux, x86-64 (`Linux x86_64`) | `x86_64-unknown-linux-gnu` |

Linux requires glibc 2.35 or later. The macOS binary isn't signed with an Apple Developer ID. The Homebrew tap and install script are not published.

<a id="install-and-connect"></a>

## Install and connect

Close Claude Code. In an empty folder, set your target on the first line and paste the block into zsh, or bash 5.1 or later:

```sh
target=aarch64-apple-darwin
url=https://github.com/Langspan/LangSpan.dev/releases/latest/download
curl -fsSLO "$url/SHA256SUMS"
archive=$(awk -v t="-$target.tar.gz" 'index($2, t) { print $2 }' SHA256SUMS)
curl -fsSLO "$url/$archive"
awk -v a="$archive" '$2 == a' SHA256SUMS | shasum -a 256 -c
tar -xzf "$archive"
cd "${archive%.tar.gz}"
mkdir -p "$HOME/.local/bin"
install -m 0755 langspan "$HOME/.local/bin/langspan"
echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.$(basename "$SHELL")rc"
export PATH="$HOME/.local/bin:$PATH"
langspan setup
langspan doctor
```

The block does the following:

1. Downloads `SHA256SUMS` from the latest release and finds your target's archive name in it.
2. Downloads the archive and checks it against `SHA256SUMS`. The check prints `OK`; on Linux without `shasum`, use `sha256sum -c` instead.
3. Extracts the archive into a folder of the same name and enters it.
4. Installs `langspan` in `~/.local/bin`, and adds that folder to `PATH` in your shell's startup file (`~/.zshrc` or `~/.bashrc`) for new shells.
5. Runs `langspan setup`, which shows one plan, asks once, connects the agent hosts it finds, starts the coordinator and runs `langspan doctor`.

zsh and bash 5.1 or later take a pasted block as a whole, so setup's question waits for you. An older bash, such as macOS's `/bin/bash` 3.2, runs pasted lines one at a time, and setup would read the next pasted line as its answer and change nothing; use zsh there, or run the last two lines one at a time.

On a new install, `doctor` reports that no workspace is registered yet; your first agent session registers one. In Codex, use `/hooks` to review and trust the hooks. Open your agents in a Git repository, and run `langspan dashboard --open` to watch their activity. The [usage guide](usage.md#set-up-in-one-command) covers setup in detail.

<a id="verify-a-download"></a>

### Verify where the archive was built

The checksum shows that the download is intact. To also check that this repository's release workflow built the archive, use the [GitHub CLI](https://cli.github.com/), signed in, in the folder where you downloaded the archive:

```sh
gh attestation verify "$archive" \
  -R Langspan/LangSpan.dev \
  --signer-workflow Langspan/LangSpan.dev/.github/workflows/release.yml
```

Add `--predicate-type https://cyclonedx.org/bom` to check the SBOM attestation instead. Builds are not bit-for-bit reproducible; the C code and the macOS linker vary between builds. [ADR 0031](adr/0031-releases.md) records how releases are made.

### Build from source

```sh
git clone https://github.com/Langspan/LangSpan.dev.git
cd LangSpan.dev
cargo build --workspace --release --locked
export PATH="$PWD/target/release:$PATH"
langspan setup
```

Requires Rust and a C compiler. Restart a running coordinator after upgrading; `langspan doctor` reports one that runs an older build ([mixed versions](usage.md#mixed-versions-after-an-upgrade)). See the [engineering guide](engineering.md).

### Connect a native agent

For [user-level setup](usage.md#all-repositories-for-this-user), use `langspan setup`. For a single checkout, see [project-level installation](reference-overview.md#one-project). To remove installed hooks, MCP configuration and the service, run `langspan uninstall --apply`.

See the [host evidence](IMPLEMENTATION_STATUS.md).
