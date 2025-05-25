#!/usr/bin/env bash

set -euo pipefail

NON_INTERACTIVE=""
INSTALL_FOLDER="${INSTALL_FOLDER:-./gnosis_vpn}"
HOPRD_API_ENDPOINT="${HOPRD_API_ENDPOINT:-}"
HOPRD_API_TOKEN="${HOPRD_API_TOKEN:-}"
HOPRD_SESSION_PORT="${HOPRD_SESSION_PORT:-}"

parse_arguments() {
    while [[ "$#" -gt 0 ]]; do
        case $1 in
            --non-interactive) NON_INTERACTIVE="yes";;
            -i|--install-folder)
                if [[ -n "${2:-}" ]]; then
                    INSTALL_FOLDER="$2"
                    shift
                else
                    echo "Error: --install-folder requires a non-empty argument."
                    exit 1
                fi ;;
            --api-endpoint)
                if [[ -n "${2:-}" ]]; then
                    HOPRD_API_ENDPOINT="$2"
                    shift
                else
                    echo "Error: --api-endpoint requires a non-empty argument."
                    exit 1
                fi ;;
            --api-token)
                if [[ -n "${2:-}" ]]; then
                    HOPRD_API_TOKEN="$2"
                    shift
                else
                    echo "Error: --api-token requires a non-empty argument."
                    exit 1
                fi ;;
            --session-port)
                if [[ -n "${2:-}" ]]; then
                    HOPRD_SESSION_PORT="$2"
                    shift
                else
                    echo "Error: --session-port requires a non-empty argument."
                    exit 1
                fi ;;
            *) echo "Unknown parameter passed: $1"; exit 1 ;;
        esac
        shift
    done
}

print_intro() {
    echo "Welcome to the GnosisVPN installation script!"
    echo "This script will help you install and configure the GnosisVPN client on your system."
    echo ""
    echo "Requirements:"
    echo "  - A running HOPRD node that will act as your entry node."
    echo "  - An additional open port on your node for GnosisVPN to connect to."
    echo ""
    echo "Note:"
    echo "  GnosisVPN uses a port called \`internal_connection_port\` for both TCP and UDP connections."
    echo ""
    echo "This installer will:"
    echo "  - Download the GnosisVPN client and control application."
    echo "  - Prompt you for API access to your HOPRD node."
    echo "  - Prompt you for the \`internal_connection_port\`."
    echo "  - Generate a configuration file based on your input."
    echo ""

    if [[ -n "${NON_INTERACTIVE}" ]]; then
        echo "Running in non-interactive mode."
        for i in {5..2}; do
            printf "\rProceeding in %d seconds..." "$i"
            sleep 1
        done
        printf "\rProceeding in 1 second..."
        sleep 1
        echo ""
    else
        read -r -n 1 -s -p "Press any key to continue or Ctrl+C to exit..."
        echo ""
    fi
}

install_folder() {
    declare install_dir
    if [[ -z "${NON_INTERACTIVE}" ]]; then
        echo "[NON-INTERACTIVE] Using installation directory: ${INSTALL_FOLDER}"
        sleep 1
    else
        read -r -p "Installation directory [${INSTALL_FOLDER}]: " install_dir
    fi
    INSTALL_FOLDER="${install_dir:-$INSTALL_FOLDER}"
}

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
    print_intro
    install_folder
    determine_api_access
    declare network latest_tag platform_tag dests

    session_port="${HOPRD_SESSION_PORT:-}"
    if [[ -z "${session_port}" ]]; then
        read -r -p "HOPRD session port (default 1422): " session_port
        session_port="${session_port:-1422}"
    fi

    network=$(curl -L -H "Content-Type: application/json" \
        -H "x-auth-token: $api_token" "${api_endpoint}/api/v3/node/info" \
        | grep -Po '(?<="network":\")[^"]*')

    echo "Detected network: $network"

    latest_tag=$(curl -L -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/gnosis/gnosis_vpn-client/releases/latest" \
        | grep -Po '(?<="tag_name": \")[^"]*')

    platform_tag=$(platform)

    echo "Detected platform: $platform_tag"

    mkdir -p "$install_dir"
    pushd "$install_dir" > /dev/null || {
        echo "Failed to create or access installation directory: $install_dir"
        exit 1
    }

    download_binary "$latest_tag" "gnosis_vpn-${platform_tag}"
    mv "./gnosis_vpn-${platform_tag}" ./gnosis_vpn
    download_binary "$latest_tag" "gnosis_vpn-ctl-${platform_tag}"
    mv "./gnosis_vpn-ctl-${platform_tag}" ./gnosis_vpn-ctl

    chmod +x ./gnosis_vpn
    chmod +x ./gnosis_vpn-ctl

    dests=$(destinations "$network")
    echo "# Generated by GnosisVPN install script

version = 2

[hoprd_node]
endpoint = \"${api_endpoint}\"
api_token = \"${api_token}\"

internal_connection_port = ${session_port}

$dests
" > ./config.toml

    popd > /dev/null
}

parse_arguments "$@"
main
