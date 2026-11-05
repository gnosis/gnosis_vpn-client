#!/usr/bin/env bash
#
# fetch-curvy-zk.sh <dir> - download the Curvy pix-aggregation proving files into <dir>, as the installer ships them.
#

set -euo pipefail

RS_SDK_VERSION="0.2.0"
RELEASE_URL="https://github.com/0xCurvy/rs-sdk/releases/download/v${RS_SDK_VERSION}"

dest="${1:?usage: fetch-curvy-zk.sh <dir>}"

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
fetch pix-aggregation-bound-2-9-30.signet.zst 334964e70daf187c66ee02af6c3061049458994de162ce6981c7a8eec7dc60f6
fetch verifyPixAggregationBound_2_9_30-final.zkey a7269ddecf820217aa103d9267e8884f3c7f37a8211f235c9f6c814f5786ed97
echo "Curvy proving files for rs-sdk ${RS_SDK_VERSION} in ${dest}"
