#!/usr/bin/env bash
# Installs the prebuilt cargo-deny and cargo-machete that CI runs, from their
# upstream GitHub releases, each checked against a pinned SHA-256 before it
# is unpacked. Compiling them with `cargo install` took four to six minutes
# per job; a download takes seconds. Then cargo-audit and cargo-nextest,
# pinned by version (see below).
#
# Usage: bash scripts/install-ci-tools.sh DIR
# Puts the binaries in DIR (created if missing); add DIR to PATH after.
#
# The digests were taken from the downloaded assets and agree with the
# digest GitHub records for each asset and with the .sha256 file published
# beside it. To bump a version: change it here and in docs/engineering.md,
# then replace each digest the same way.
set -euo pipefail

dir=${1:?usage: install-ci-tools.sh DIR}
deny=0.22.1
machete=0.10.1
audit=0.23.0
nextest=0.9.152

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    target=x86_64-unknown-linux-musl
    deny_sha=13f7e825e9cb811c2f8de63f29b0f45851dfb53004029f0f2501af524728364d
    machete_sha=785d381985140ffa172246d82089adeb7f2eb9c774d961c48f77a4494023afb5
    check() { sha256sum -c -; }
    ;;
  Darwin-arm64)
    target=aarch64-apple-darwin
    deny_sha=2afa2d672d2b2ee012835528d0c2acb73693ee973a8dc377db0e09d7d8dee9a2
    machete_sha=0659860970e26179d730d1a2e99fc6513e41fea122ad5451d7ae43234fc1ebab
    check() { shasum -a 256 -c -; }
    ;;
  *)
    echo "no pinned binaries for $(uname -s)-$(uname -m); install with" >&2
    echo "  cargo install --locked cargo-deny --version $deny" >&2
    echo "  cargo install --locked cargo-machete --version $machete" >&2
    exit 1
    ;;
esac

mkdir -p "$dir"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# fetch NAME VERSION_DIR_URL ASSET SHA256
fetch() {
  local name=$1 url=$2 asset=$3 sha=$4
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$asset" "$url/$asset"
  # A digest mismatch stops here, before anything is unpacked.
  (cd "$work" && printf '%s  %s\n' "$sha" "$asset" | check)
  tar -xzf "$work/$asset" -C "$work"
  install -m 0755 "$work/${asset%.tar.gz}/$name" "$dir/$name"
}

fetch cargo-deny "https://github.com/EmbarkStudios/cargo-deny/releases/download/$deny" \
  "cargo-deny-$deny-$target.tar.gz" "$deny_sha"
fetch cargo-machete "https://github.com/bnjbvr/cargo-machete/releases/download/v$machete" \
  "cargo-machete-v$machete-$target.tar.gz" "$machete_sha"

"$dir/cargo-deny" --version
"$dir/cargo-machete" --version

# cargo-audit, the second advisory check, and cargo-nextest, the test
# runner, are built from their crates.io releases with `cargo install
# --locked`: cargo checks each downloaded crate against the checksum in the
# registry index and builds the dependency versions the release's own
# lockfile pins. Compiling takes minutes, so a copy of the same version
# already in DIR (restored from the job's cache) is kept.
#
# build NAME VERSION: NAME is the crate and binary, `cargo-X`, and
# `cargo-X X --version` prints a line ending in VERSION, or holding it
# before a space.
build() {
  local name=$1 version=$2
  case "$("$dir/$name" "${name#cargo-}" --version 2>/dev/null | head -n 1 || true)" in
    *" $version" | *" $version "*)
      echo "$name $version is already in $dir"
      ;;
    *)
      cargo install --locked --root "$work/$name" --version "$version" "$name"
      install -m 0755 "$work/$name/bin/$name" "$dir/$name"
      ;;
  esac
  "$dir/$name" "${name#cargo-}" --version
}

build cargo-audit "$audit"
build cargo-nextest "$nextest"
