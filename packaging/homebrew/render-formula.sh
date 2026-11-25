#!/bin/sh
# Writes the Homebrew formula for one release to stdout.
# Usage: render-formula.sh VERSION SHA256SUMS
# VERSION has no leading v (0.1.0). SHA256SUMS is the release's checksum file.
# Check the archives first: gh attestation verify FILE -R Langspan/LangSpan.dev
set -eu
version=$1
sums=$2
template="$(dirname "$0")/langspan.rb.template"
script="s/@VERSION@/$version/g"
for target in aarch64-apple-darwin x86_64-apple-darwin \
  x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
  file="langspan-v$version-$target.tar.gz"
  sum=$(awk -v f="$file" '$2 == f { print $1 }' "$sums")
  case $sum in
    [0-9a-f]*) [ ${#sum} -eq 64 ] || { echo "bad checksum for $file" >&2; exit 1; } ;;
    *) echo "no checksum for $file in $sums" >&2; exit 1 ;;
  esac
  script="$script;s/@SHA256_$target@/$sum/g"
done
sed -e "$script" -e '1,3d' "$template"
