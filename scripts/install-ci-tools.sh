#!/usr/bin/env bash
# Installs the prebuilt cargo-deny, cargo-machete and cargo-nextest that CI
# runs, from their upstream GitHub releases, each checked against a pinned
# SHA-256 before it is unpacked. Compiling them with `cargo install` took
# minutes per job; a download takes seconds. Then cargo-audit and, for the
# coverage job, cargo-llvm-cov, pinned by version (see below).
#
# Usage: bash scripts/install-ci-tools.sh DIR [TOOL...]
# Puts the binaries in DIR (created if missing); add DIR to PATH after.
# With no TOOL, installs what scripts/check.sh runs: cargo-deny,
# cargo-machete, cargo-audit and cargo-nextest. cargo-llvm-cov is for
# scripts/coverage.sh.
#
# The digests were taken from the downloaded assets and agree with the
# digest GitHub records for each asset and with the .sha256 file published
# beside it. To bump a version: change it here and in docs/engineering.md,
# then replace each digest the same way.
set -euo pipefail

dir=${1:?usage: install-ci-tools.sh DIR [TOOL...]}
shift
deny=0.22.1
machete=0.10.1
audit=0.23.0
nextest=0.9.152
llvm_cov=0.9.2

tools=" ${*:-cargo-deny cargo-machete cargo-audit cargo-nextest} "
for tool in $tools; do
  case $tool in
    cargo-deny | cargo-machete | cargo-audit | cargo-nextest | cargo-llvm-cov) ;;
    *)
      echo "install-ci-tools.sh: no tool $tool; choose from cargo-deny cargo-machete cargo-audit cargo-nextest cargo-llvm-cov" >&2
      exit 2
      ;;
  esac
done
wanted() { [[ $tools == *" $1 "* ]]; }

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    target=x86_64-unknown-linux-musl
    deny_sha=13f7e825e9cb811c2f8de63f29b0f45851dfb53004029f0f2501af524728364d
    machete_sha=785d381985140ffa172246d82089adeb7f2eb9c774d961c48f77a4494023afb5
    nextest_target=x86_64-unknown-linux-gnu
    nextest_sha=d039b43c3e0958441b6ad0a39c8884d018b3d2e8fb9928f209074b1100b5e17a
    check() { sha256sum -c -; }
    ;;
  Darwin-arm64)
    target=aarch64-apple-darwin
    deny_sha=2afa2d672d2b2ee012835528d0c2acb73693ee973a8dc377db0e09d7d8dee9a2
    machete_sha=0659860970e26179d730d1a2e99fc6513e41fea122ad5451d7ae43234fc1ebab
    # nextest publishes one universal (x86_64 and arm64) macOS binary.
    nextest_target=universal-apple-darwin
    nextest_sha=2fadf57df4243e691f088ee3bff4f4ab9af16da0c95a1db84b5cb1685e098a76
    check() { shasum -a 256 -c -; }
    ;;
  *)
    echo "no pinned binaries for $(uname -s)-$(uname -m); install with" >&2
    echo "  cargo install --locked cargo-deny --version $deny" >&2
    echo "  cargo install --locked cargo-machete --version $machete" >&2
    echo "  cargo install --locked cargo-nextest --version $nextest" >&2
    exit 1
    ;;
esac

mkdir -p "$dir"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# fetch NAME VERSION_DIR_URL ASSET SHA256 [PATH]: PATH is the binary's path
# in the archive, by default NAME in a folder named after ASSET.
fetch() {
  local name=$1 url=$2 asset=$3 sha=$4 path=${5:-${3%.tar.gz}/$1}
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$asset" "$url/$asset"
  # A digest mismatch stops here, before anything is unpacked.
  (cd "$work" && printf '%s  %s\n' "$sha" "$asset" | check)
  tar -xzf "$work/$asset" -C "$work"
  install -m 0755 "$work/$path" "$dir/$name"
}

if wanted cargo-deny; then
  fetch cargo-deny "https://github.com/EmbarkStudios/cargo-deny/releases/download/$deny" \
    "cargo-deny-$deny-$target.tar.gz" "$deny_sha"
  "$dir/cargo-deny" --version
fi
if wanted cargo-machete; then
  fetch cargo-machete "https://github.com/bnjbvr/cargo-machete/releases/download/v$machete" \
    "cargo-machete-v$machete-$target.tar.gz" "$machete_sha"
  "$dir/cargo-machete" --version
fi
# Downloaded every run, so a copy restored from the job's cache is replaced
# by the verified one, not trusted. Its archive holds the bare binary.
if wanted cargo-nextest; then
  fetch cargo-nextest "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$nextest" \
    "cargo-nextest-$nextest-$nextest_target.tar.gz" "$nextest_sha" cargo-nextest
  "$dir/cargo-nextest" nextest --version
fi

# cargo-audit, the second advisory check, and cargo-llvm-cov are built from
# their crates.io releases with `cargo install
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

if wanted cargo-audit; then build cargo-audit "$audit"; fi
if wanted cargo-llvm-cov; then build cargo-llvm-cov "$llvm_cov"; fi
