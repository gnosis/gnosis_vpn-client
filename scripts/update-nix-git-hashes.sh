#!/usr/bin/env bash
#
# update-nix-git-hashes.sh - sync nix/gnosisvpn.nix's outputHashes with Cargo.lock.
#
# Cargo.lock pins each git dependency as `git+<url>[?rev=|tag=]<ref>#<commit>`.
# nix/gnosisvpn.nix's `outputHashes` attrset needs a matching FOD sha256 for
# every one of those entries, keyed on that exact string. Whenever Renovate
# (or `cargo update`) moves a git dependency, this script re-prefetches every
# git source in Cargo.lock and rewrites the outputHashes block to match.
#
# Required commands: nix-prefetch-git, jq, nix (for `nix hash`).
#
# Linux/CI-oriented: relies on GNU grep (`-oP`) and bash `mapfile`; BSD userland
# (stock macOS) lacks both. Run it via `nix shell` / in the workflow, not raw.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cargo_lock="$repo_root/Cargo.lock"
nix_file="$repo_root/nix/gnosisvpn.nix"

for cmd in nix-prefetch-git jq nix; do
    if ! command -v "$cmd" >/dev/null; then
        echo "error: required command '$cmd' not found" >&2
        exit 1
    fi
done

# LC_ALL=C keeps the emitted order byte-stable across runner locales, matching
# the entry order committed in nix/gnosisvpn.nix so re-runs produce clean diffs.
mapfile -t sources < <(grep -oP '^source = "\Kgit\+[^"]+' "$cargo_lock" | LC_ALL=C sort -u)

if [ "${#sources[@]}" -eq 0 ]; then
    echo "error: no git dependencies found in $cargo_lock" >&2
    exit 1
fi

to_sri() {
    # nix >=2.19 renamed `hash to-sri` to `hash convert`; support both.
    nix hash to-sri --type sha256 "$1" 2>/dev/null ||
        nix hash convert --hash-algo sha256 --to sri "$1"
}

# crane percent-decodes each source string before its outputHashes lookup, so the key must match decoded.
percent_decode() {
    local s="${1//\\/\\\\}"
    s=$(printf '%s' "$s" | sed -E 's/%([0-9A-Fa-f]{2})/\\x\1/g')
    printf '%b' "$s"
}

lines=()
for source in "${sources[@]}"; do
    url="${source#git+}"
    url="${url%%[?#]*}"
    rev="${source##*#}"

    echo "prefetching $url @ $rev" >&2
    base32_hash="$(nix-prefetch-git --url "$url" --rev "$rev" --quiet | jq -r '.sha256')"
    sri_hash="$(to_sri "$base32_hash")"

    lines+=("    \"$(percent_decode "$source")\" =")
    lines+=("      \"${sri_hash}\";")
done
# Command substitution strips trailing newlines, so add one back: awk's block
# print must end in \n or the closing "};" merges onto the last hash line.
block="$(printf '%s\n' "${lines[@]}")"$'\n'

awk -v block="$block" '
  /^  outputHashes = \{/ { print; printf "%s", block; skipping=1; next }
  skipping && /^  \};/   { print; skipping=0; next }
  skipping               { next }
  { print }
' "$nix_file" >"$nix_file.tmp"
mv "$nix_file.tmp" "$nix_file"

echo "updated $nix_file"
