# Install a release

The [latest stable release](https://github.com/Langspan/LangSpan.dev/releases/latest) provides
macOS and Linux binaries. Follow the [platform downloads, verification and
installation instructions](../install.md) before starting the coordinator.
A source build is optional.

The quickest way is the [install script](../install.md#use-the-install-script):

```shell
curl -fsSL https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh | sh
```

It checks the archive against `SHA256SUMS`, and with the GitHub CLI signed
in, against the release workflow's attestation too. It installs `langspan`
in `~/.local/bin` without root and prints each check it made.
