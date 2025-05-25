#!/usr/bin/env bash

set -euo pipefail

platform() {
    declare os arch arch_tag
    os="$(uname | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    case "$arch" in
      x86_64|amd64) arch_tag="x86_64";;
      aarch64|arm64) arch_tag="aarch64";;
      armv7l) arch_tag="armv7l";;
      *) echo "Unsupported architecture: $arch"; exit 1;;
    esac

    echo "$arch_tag-$os"
}

download_binary() {
    declare binary latest_tag url
    latest_tag="$1"
    binary="$2"

    url="https://github.com/gnosis/gnosis_vpn-client/releases/download/${latest_tag}/${binary}"

    echo "Downloading ${binary} from ${url}..."
    curl -L --progress-bar "${url}" -o "${binary}"
}

destinations() {
    declare network
    local network="$1"
    if [[ "$network" == "rotsee" ]]; then
        echo "[destinations]

[destinations.12D3KooWDNcj8phBXj9ZJkAxSmjbwNUzEWtSsg6K6BeuKCAyZuCU]
meta = { location = \"USA\", state = \"Iowa\" }
# path = { intermediates = [ \"12D3KooWRT74aKgHF36HwqvvxQiLCL1GVFRSv6eEFQ71wtY2vVvt\" ] }
path = { hops = 0 }

[destinations.12D3KooWRKoZGSHR53rhK83omuomvFjUCV4hL3MwnkurU8C58SGQ]
meta = { location = \"UK\", city = \"London\" }
# path = { intermediates = [ \"12D3KooWC69bPoKYzBYP95GXAumqeMKqxcrtb2vFYLuf4N16R2Lk\" ] }
path = { hops = 0 }
"
    else
        echo "[destinations]

[destinations.12D3KooWMEXkxWMitwu9apsHmjgDZ7imVHgEsjXfcyZfrqYMYjW7]
meta = { location = \"Germany\" }
path = { intermediates = [ \"12D3KooWFUD4BSzjopNzEzhSi9chAkZXRKGtQJzU482rJnyd2ZnP\" ] }

[destinations.12D3KooWBRB3y81TmtqC34JSd61uS8BVeUqWxCSBijD5nLhL6HU5]
meta = { location = \"USA\" }
path = { intermediates = [ \"12D3KooWQLTR4zdLyXToQGx3YKs9LJmeL4MKJ3KMp4rfVibhbqPQ\" ] }

[destinations.12D3KooWGdcnCwJ3645cFgo4drvSN3TKmxQFYEZK7HMPA6wx1bjL]
meta = { location = \"Spain\" }
path = { intermediates = [ \"12D3KooWFnMnefPQp2k3XA3yNViBH4hnUCXcs9LasLUSv6WAgKSr\" ] }
"
    fi
}

main() {
    declare api_endpoint api_token network latest_tag platform_tag dests
    read -r -p "HOPRD API endpoint: " api_endpoint
    read -r -p "HOPRD API access token: " api_token

    network=$(curl -H "Content-Type: application/json" \
        -H "x-auth-token: $api_token" "${api_endpoint}/api/v3/node/info" \
        | grep -Po '(?<="network":\")[^"]*')

    echo "Detected network: $network"

    latest_tag=$(curl -L -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/gnosis/gnosis_vpn-client/releases/latest" \
        | grep -Po '(?<="tag_name": \")[^"]*')

    platform_tag=$(platform)

    echo "Detected platform: $platform_tag"

    mkdir -p ./gnosis_vpn
    pushd ./gnosis_vpn > /dev/null

    download_binary "$latest_tag" "gnosis_vpn-${platform_tag}"
    mv "./gnosis_vpn-${platform_tag}" ./gnosis_vpn
    download_binary "$latest_tag" "gnosis_vpn-ctl-${platform_tag}"
    mv "./gnosis_vpn-ctl-${platform_tag}" ./gnosis_vpn-ctl

    chmod +x ./gnosis_vpn
    chmod +x ./gnosis_vpn-ctl

    dests=$(destinations "$network")
    echo "[hoprd_node]
api_endpoint = \"${api_endpoint}\"
api_token = \"${api_token}\"

$dests
" > ./config.toml

    popd > /dev/null
}

main
