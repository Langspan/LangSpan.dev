#!/usr/bin/env bash
# Holds CI's build cache under its size limit (.github/workflows/check.yml).
# Adds up the size of each path in CACHE_PATHS (one per line, `~` for the
# home; a missing path counts as 0) and writes save=true to $GITHUB_OUTPUT
# when the total is at most LIMIT_MB megabytes, and save=false with a
# warning otherwise. Main runs it after pruning what the cache doesn't keep,
# and saves only on save=true.
set -euo pipefail

: "${CACHE_PATHS:?set CACHE_PATHS to the cached paths, one per line}"
: "${LIMIT_MB:?set LIMIT_MB to the size limit in megabytes}"
: "${GITHUB_OUTPUT:?set GITHUB_OUTPUT to the step output file}"

total=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  case $path in "~/"*) path=$HOME/${path#"~/"} ;; esac
  if [ -e "$path" ]; then
    size=$(du -sm "$path" | cut -f1)
    echo "$size MB  $path"
    total=$((total + size))
  fi
done <<< "$CACHE_PATHS"

if [ "$total" -le "$LIMIT_MB" ]; then
  echo "build cache: $total MB, within the $LIMIT_MB MB limit; saving it"
  echo "save=true" >> "$GITHUB_OUTPUT"
else
  echo "::warning title=Build cache not saved::$total MB is over the $LIMIT_MB MB limit, so this run saves no cache and later runs build from what main last saved. Find what grew in the sizes above, then prune it in check.yml or raise LIMIT_MB."
  echo "save=false" >> "$GITHUB_OUTPUT"
fi
