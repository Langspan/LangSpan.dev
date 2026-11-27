# Install and verify LangSpan

LangSpan ships as one binary for macOS and Linux. Each [release](https://github.com/Langspan/LangSpan.dev/releases/latest) has an archive per target, a `SHA256SUMS` file, an SBOM per target and a provenance bundle. You need only the archive for your target and `SHA256SUMS`. To check your platform, run `uname -sm`.

| Platform | Target |
| --- | --- |
| macOS, Apple silicon (`Darwin arm64`) | `aarch64-apple-darwin` |
| macOS, Intel (`Darwin x86_64`) | `x86_64-apple-darwin` |
| Linux, ARM64 (`Linux aarch64`) | `aarch64-unknown-linux-gnu` |
| Linux, x86-64 (`Linux x86_64`) | `x86_64-unknown-linux-gnu` |

Linux requires glibc 2.35 or later. The macOS binary isn't signed with an Apple Developer ID. The Homebrew tap is not published.

## Use the install script

Close Claude Code. To install the latest release, run:

```shell
curl -fsSL https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh | sh
```

To install a particular release, give its version: `curl -fsSL https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh | sh -s -- v0.4.0`.

The script does the following:

1. Finds your target from `uname -sm`. On another platform it stops and points to [Build from source](#build-from-source).
2. Downloads `SHA256SUMS` and your target's archive over HTTPS.
3. Checks the archive's SHA-256 against `SHA256SUMS` before it extracts anything.
4. Copies `langspan` to `~/.local/bin`. It doesn't use root or change your shell startup files.
5. Prints the next steps: add `~/.local/bin` to `PATH` if it isn't there, or put it first if an earlier `langspan` elsewhere on `PATH` would run instead (the script names that file), run `langspan setup`, then run `langspan demo`.

The script doesn't run `langspan setup` itself. A failed download, a checksum that doesn't match or an unknown platform stops it with a message that says what failed and what to do; nothing is installed. Each release attaches the script as `install.sh`, and `SHA256SUMS` lists it.

To read the script before you run it, download it, read it, then run the copy you read:

```shell
curl -fsSLO https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh
less install.sh
sh install.sh
```

To also check that the release workflow built it, run `gh attestation verify install.sh -R Langspan/LangSpan.dev` ([verify where the archive was built](#verify-a-download)). The source is [`scripts/install.sh`](../scripts/install.sh).

<a id="a-browser-download-on-macos"></a>

### A browser download on macOS

macOS marks a file that a browser downloads, and Gatekeeper rejects the marked `langspan` binary because it has no Apple Developer ID signature. `curl` doesn't set that mark, so the script and the steps below install a binary that runs. If you downloaded the archive in a browser, remove the mark from the installed binary:

```shell
xattr -d com.apple.quarantine "$HOME/.local/bin/langspan"
```

<a id="install-and-connect"></a>

## Install by hand

To install without the script, close Claude Code. In an empty folder, set your target on the first line and paste the block into zsh, or bash 5.1 or later:

```sh
target=aarch64-apple-darwin
url=https://github.com/Langspan/LangSpan.dev/releases/latest/download
curl -fsSLO "$url/SHA256SUMS" &&
archive=$(awk -v t="-$target.tar.gz" 'index($2, t) { print $2 }' SHA256SUMS) &&
curl -fsSLO "$url/$archive" &&
awk -v a="$archive" '$2 == a' SHA256SUMS | shasum -a 256 -c &&
tar -xzf "$archive" &&
cd "${archive%.tar.gz}" &&
mkdir -p "$HOME/.local/bin" &&
install -m 0755 langspan "$HOME/.local/bin/langspan" &&
echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.$(basename "$SHELL")rc" &&
export PATH="$HOME/.local/bin:$PATH" &&
langspan setup &&
langspan doctor
```

Each `&&` runs the next step only if the one before succeeds. A failed download, a checksum that doesn't match, or a missing checksum tool stops the block there: nothing is extracted, installed or run. Fix the reported error and paste the block again.

The block does the following:

1. Downloads `SHA256SUMS` from the latest release and finds your target's archive name in it.
2. Downloads the archive and checks it against `SHA256SUMS`. The check prints `OK`; on Linux without `shasum`, use `sha256sum -c` instead.
3. Extracts the archive into a folder of the same name and enters it.
4. Installs `langspan` in `~/.local/bin`, and adds that folder to `PATH` in your shell's startup file (`~/.zshrc` or `~/.bashrc`) for new shells.
5. Runs `langspan setup`, which shows one plan, asks once, connects the agent hosts it finds, starts the coordinator and runs `langspan doctor`.

zsh and bash 5.1 or later take a pasted block as a whole, so setup's question waits for you. An older bash, such as macOS's `/bin/bash` 3.2, runs pasted lines one at a time, and this block isn't tested there; use zsh.

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
