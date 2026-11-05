#!/usr/bin/env bash
#
# fetch-curvy-zk.sh <dir> - download the Curvy pix-aggregation proving files into <dir>, as the installer ships them.
#
# The file names and digests belong to one curvy-witnesscalc version; the client rejects files from any other.
#

set -euo pipefail

RS_SDK_VERSION="0.1.1"
RELEASE_URL="https://github.com/0xCurvy/rs-sdk/releases/download/v${RS_SDK_VERSION}"
CARGO_LOCK="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/Cargo.lock"

dest="${1:?usage: fetch-curvy-zk.sh <dir>}"

locked_version="$(grep -A1 '^name = "curvy-witnesscalc"$' "${CARGO_LOCK}" | sed -n 's/^version = "\(.*\)"$/\1/p')"
if [[ ${locked_version} != "${RS_SDK_VERSION}" ]]; then
    echo "ERROR: Cargo.lock has curvy-witnesscalc ${locked_version}, this script fetches ${RS_SDK_VERSION}; update its names and digests" >&2
    exit 1
fi

fetch() {
    local file="$1" expected="$2"
    curl -fsSL --retry 3 -o "${dest}/${file}" "${RELEASE_URL}/${file}"
    local actual
    actual="$(sha256sum "${dest}/${file}" | cut -d' ' -f1)"
    if [[ ${actual} != "${expected}" ]]; then
        echo "ERROR: ${file}: SHA-256 ${actual}, expected ${expected}" >&2
        exit 1
    fi
}

mkdir -p "${dest}"
fetch pix-aggregation-2-9-30.signet.zst b974028ba40afdc067524819d61bdd9172a5e56369cfc05a75ba5d469c379c3a
fetch verifyPixAggregation_2_9_30-final.zkey 7611d3ade361c02b70b27620aca12aa8c15f7c7aba352d3e7add6409974fb13d
echo "Curvy proving files for rs-sdk ${RS_SDK_VERSION} in ${dest}"
