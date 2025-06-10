#!/usr/bin/env bash

set -euo pipefail

NON_INTERACTIVE=""
INSTALL_FOLDER="${INSTALL_FOLDER:-./gnosis_vpn}"
HOPRD_API_ENDPOINT="${HOPRD_API_ENDPOINT:-}"
HOPRD_API_TOKEN="${HOPRD_API_TOKEN:-}"
HOPRD_SESSION_PORT="${HOPRD_SESSION_PORT:-1422}"
PLATFORM=""
HOPR_NETWORK=""
VERSION_TAG=""
IS_MACOS=""
WG_PUBLIC_KEY="${WG_PUBLIC_KEY:-}"

# taken from https://stackoverflow.com/a/28938235
BPurple='\033[1;35m'
BCyan='\033[1;36m'
Color_Off='\033[0m'

GLOBAL_NAME="${BPurple}<GnosisVPN installer script>${Color_Off}"

usage() {
    echo -e "Usage: ${GLOBAL_NAME} [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --non-interactive          Run the script in non-interactive mode."
    echo "  -i, --install-folder       Specify the installation folder (default: ./gnosis_vpn)."
    echo "  --api-endpoint             HOPRD API endpoint (default: empty, will prompt)."
    echo "  --api-token                HOPRD API token (default: empty, will prompt)."
    echo "  --session-port             HOPRD session port (default: 1422, will prompt)."
    echo "  --wireguard-public-key     WireGuard public key (required for macOS, optional otherwise)."
    echo "  --version-tag              Specify a specific version tag to install."
    echo "  --help                     Show this help message and exit."
    exit 0
}

check_reqs() {
    required_cmds=(curl grep sed cat uname)
    if [[ -n ${IS_MACOS} ]]; then
        required_cmds+=(xattr)
    fi

    for cmd in "${required_cmds[@]}"; do
        if ! command -v "$cmd" &>/dev/null; then
            echo "Error: $cmd is required to run this script. Please install $cmd and try again."
            exit 1
        fi
    done

}

parse_arguments() {
    while [[ $# -gt 0 ]]; do
        case $1 in
        --help) usage ;;
        --non-interactive) NON_INTERACTIVE="yes" ;;
        -i | --install-folder)
            if [[ -n ${2:-} ]]; then
                INSTALL_FOLDER="$2"
                shift
            else
                echo "Error: --install-folder requires a non-empty argument."
                exit 1
            fi
            ;;
        --api-endpoint)
            if [[ -n ${2:-} ]]; then
                HOPRD_API_ENDPOINT="$2"
                shift
            else
                echo "Error: --api-endpoint requires a non-empty argument."
                exit 1
            fi
            ;;
        --api-token)
            if [[ -n ${2:-} ]]; then
                HOPRD_API_TOKEN="$2"
                shift
            else
                echo "Error: --api-token requires a non-empty argument."
                exit 1
            fi
            ;;
        --session-port)
            if [[ -n ${2:-} ]]; then
                HOPRD_SESSION_PORT="$2"
                shift
            else
                echo "Error: --session-port requires a non-empty argument."
                exit 1
            fi
            ;;
        --wireguard-public-key)
            if [[ -n ${2:-} ]]; then
                WG_PUBLIC_KEY="$2"
                shift
            else
                echo "Error: --wireguard-public-key requires a non-empty argument."
                exit 1
            fi
            ;;
        --version-tag)
            if [[ -n ${2:-} ]]; then
                VERSION_TAG="$2"
                shift
            else
                echo "Error: --version-tag requires a non-empty argument."
                exit 1
            fi
            ;;
        *)
            echo "Unknown parameter passed: $1"
            exit 1
            ;;
        esac
        shift
    done
}

print_intro() {
    echo -e "Welcome to the ${GLOBAL_NAME}!"
    echo "This script will help you install and configure the GnosisVPN client on your system."
    echo ""
    echo "Requirements:"
    echo "  - A running HOPRD node that will act as your entry node."
    echo "  - An additional open port on your node for GnosisVPN to connect to."
    echo ""
    echo "Note:"
    echo "  GnosisVPN uses a port called 'internal_connection_port' for both TCP and UDP connections."
    echo ""
    echo "This installer will:"
    echo "  - Download the GnosisVPN client and control application."
    echo "  - Prompt you for API access to your HOPRD node."
    echo "  - Prompt you for the 'internal_connection_port'."
    echo "  - Generate a configuration file based on your input."
    echo ""

    if [[ -n ${NON_INTERACTIVE} ]]; then
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
    fi
}

prompt_install_dir() {
    declare install_dir
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo "[NON-INTERACTIVE] Using installation directory: ${INSTALL_FOLDER}"
        sleep 1
        return
    fi

    echo ""
    echo "Please specify the installation directory for GnosisVPN."
    echo "Downloaded binaries will be placed in this directory."
    echo "The configuration file will also be created in this directory."
    read -r -p "Enter installation directory [${INSTALL_FOLDER:-<blank>}]: " install_dir
    install_dir="${install_dir:-$INSTALL_FOLDER}"
    # do not rely on realpath as it is unstable on macOS
    if INSTALL_FOLDER=$(realpath "${install_dir:-$INSTALL_FOLDER}" 2>/dev/null); then
        :
    else
        INSTALL_FOLDER=$([[ $install_dir == /* ]] && echo "$install_dir" || echo "$PWD/${install_dir#./}")
    fi
    echo "GnosisVPN will be installed to: \"${INSTALL_FOLDER}\"."
}

# from https://stackoverflow.com/a/37840948
urldecode() {
    : "${*//+/ }"
    echo -e "${_//%/\\x}"
}

prompt_api_access() {
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo "[NON-INTERACTIVE] Using HOPRD API endpoint: ${HOPRD_API_ENDPOINT:-}"
        sleep 1
        echo "[NON-INTERACTIVE] Using HOPRD API token: ${HOPRD_API_TOKEN:-}"
        sleep 1
        return
    fi

    echo ""
    echo "GnosisVPN uses your HOPRD node as entry connection point."
    echo "Therefore, you need to provide the API endpoint and token for your HOPRD node."
    echo -e "If connected to your HOPRD node via ${BCyan}HOPR Admin UI${Color_Off}, paste it's full URL."
    echo "The script will try to parse the required values from the URL."
    declare admin_url
    read -r -p "Enter full HOPRD admin interface URL [or leave blank to provide API_ENDPOINT and API_TOKEN separately]: " admin_url

    declare api_endpoint api_token
    api_endpoint=""
    api_token=""
    if [[ -n ${admin_url} ]]; then
        echo "Parsing admin URL..."
        declare decoded_url
        decoded_url=$(urldecode "$admin_url")
        api_endpoint=$(echo "$decoded_url" | grep -o 'apiEndpoint=[^&]*' | sed 's/apiEndpoint=//' || true)
        api_token=$(echo "$decoded_url" | grep -o 'apiToken=[^&]*' | sed 's/apiToken=//' || true)
    fi
    if [[ -z ${api_endpoint} ]]; then
        if [[ -n ${admin_url} ]]; then
            echo "Warning: Could not parse API endpoint from the provided URL. Please provide it manually."
        fi
        read -r -p "Enter HOPRD API endpoint [${HOPRD_API_ENDPOINT:-<blank>}]: " api_endpoint
    else
        echo "Using parsed API endpoint: ${api_endpoint}"
    fi
    if [[ -z ${api_token} ]]; then
        if [[ -n ${admin_url} ]]; then
            echo "Error: Could not parse API token from the provided URL."
        fi
        read -r -p "Enter HOPRD API token [${HOPRD_API_TOKEN:-<blank>}]: " api_token
    else
        echo "Using parsed API token: ${api_token}"
    fi

    HOPRD_API_ENDPOINT="${api_endpoint:-$HOPRD_API_ENDPOINT}"
    HOPRD_API_TOKEN="${api_token:-$HOPRD_API_TOKEN}"
}

prompt_session_port() {
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo "[NON-INTERACTIVE] Using HOPRD session port: ${HOPRD_SESSION_PORT}"
        sleep 1
        return
    fi

    echo ""
    echo "GnosisVPN requires a port for internal connections."
    echo "This port will be used for both TCP and UDP connections."
    read -r -p "Enter HOPRD session port [${HOPRD_SESSION_PORT:-<blank>}]: " session_port
    HOPRD_SESSION_PORT="${session_port:-$HOPRD_SESSION_PORT}"
}

fetch_network() {
    echo ""
    echo "Accessing HOPRD API to determine network"
    HOPR_NETWORK=$(curl --fail -L --progress-bar \
        -H "Content-Type: application/json" \
        -H "x-auth-token: $HOPRD_API_TOKEN" \
        "${HOPRD_API_ENDPOINT}/api/v3/node/info" |
        grep '"network":' |
        sed -E 's/.*"network": *"([^"]*)".*/\1/')
    echo "Detected network: $HOPR_NETWORK"
}

fetch_version_tag() {
    if [[ -n ${VERSION_TAG} ]]; then
        echo ""
        echo "Verifying provided version tag: ${VERSION_TAG}"
        curl --fail -L --progress-bar \
            -H "Accept: application/vnd.github+json" \
            "https://api.github.com/repos/gnosis/gnosis_vpn-client/releases/tags/${VERSION_TAG}" &>/dev/null ||
            (
                echo "Error: Provided version tag '${VERSION_TAG}' is not valid or does not exist."
                exit 1
            )
        return
    fi

    echo ""
    echo "Fetching the latest GnosisVPN release tag from GitHub..."
    VERSION_TAG=$(curl --fail -L --progress-bar \
        -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/gnosis/gnosis_vpn-client/releases/latest" |
        grep '"tag_name":' |
        sed -E 's/.*"tag_name": *"([^"]*)".*/\1/')
    echo "GnosisVPN version found: ${VERSION_TAG}"
}

check_platform() {
    declare os arch arch_tag
    os="$(uname | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"
    if [[ ${os} == "darwin" ]]; then IS_MACOS="yes"; fi

    case "$arch" in
    x86_64 | amd64) arch_tag="x86_64" ;;
    aarch64 | arm64) arch_tag="aarch64" ;;
    armv7l) arch_tag="armv7l" ;;
    *)
        echo "Unsupported architecture: $arch"
        exit 1
        ;;
    esac

    echo ""
    echo "Detected architecture: $arch_tag-$os"
    PLATFORM="$arch_tag-$os"
}

prompt_wireguard() {
    declare wg_fail
    wg_fail=""
    if ! command -v wg &>/dev/null; then
        echo "Probing for wg command failed."
        wg_fail="yes"
    fi
    if [[ -z $wg_fail ]] && ! command -v wg-quick &>/dev/null; then
        echo "Probing for wg-quick command failed."
        wg_fail="yes"
    fi

    if [[ -n ${NON_INTERACTIVE} ]]; then
        if [[ -n ${WG_PUBLIC_KEY} ]]; then
            echo "[NON-INTERACTIVE] Using provided WireGuard public key: ${WG_PUBLIC_KEY}"
            sleep 1
            return
        fi
        if [[ -n ${wg_fail} ]]; then
            echo "[NON-INTERACTIVE] WireGuard tools are not installed."
            echo "[NON-INTERACTIVE] Cannot continue non interactive installation."
            echo "[NON-INTERACTIVE] Please provide WG_PUBLIC_KEY environment variable or install wireguard-tools."
            exit 1
        fi
    fi

    if [[ -n $wg_fail ]]; then
        echo ""
        echo "WireGuard tools are not installed."
        echo ""
        echo "GnosisVPN works best with WireGuard tools installed."
        echo "However if you know what you are doing you can also continue with manual mode."
        read -r -p "Press [Enter] to continue with manual mode or Ctrl+C to exit."
        declare wg_pub_key
        read -r -p "Enter WireGuard public key [${WG_PUBLIC_KEY:-<blank>}]: " wg_pub_key
        WG_PUBLIC_KEY="${wg_pub_key:-$WG_PUBLIC_KEY}"
    fi
}

enter_install_dir() {
    mkdir -p "${INSTALL_FOLDER}"
    pushd "${INSTALL_FOLDER}" >/dev/null || {
        echo "Failed to create or access installation directory: $INSTALL_FOLDER"
        exit 1
    }
}

exit_install_dir() {
    popd >/dev/null
}

download_binary() {
    declare binary url
    binary="$1"
    url="https://github.com/gnosis/gnosis_vpn-client/releases/download/${VERSION_TAG}/${binary}"

    echo ""
    echo "Downloading ${binary} from ${url}..."
    curl --fail -L --progress-bar "${url}" -o "${binary}"
}

fetch_binaries() {
    download_binary "gnosis_vpn-${PLATFORM}"
    mv "./gnosis_vpn-${PLATFORM}" ./gnosis_vpn
    download_binary "gnosis_vpn-ctl-${PLATFORM}"
    mv "./gnosis_vpn-ctl-${PLATFORM}" ./gnosis_vpn-ctl

    chmod +x ./gnosis_vpn
    chmod +x ./gnosis_vpn-ctl

    if [[ -n ${IS_MACOS} ]]; then
        echo "Detected macOS - GnosisVPN binaries need to be removed from apple quarantine mode."
        echo -e "We will run these two commands with ${BCyan}sudo${Color_Off} to get required privileges:"
        echo "sudo xattr -dr com.apple.quarantine ./gnosis_vpn"
        echo "sudo xattr -dr com.apple.quarantine ./gnosis_vpn-ctl"
        echo "These are the only commands that require sudo privileges."
        sleep 1
        sudo xattr -dr com.apple.quarantine ./gnosis_vpn
        sudo xattr -dr com.apple.quarantine ./gnosis_vpn-ctl
    fi
}

generate_config() {
    declare destinations wg_section
    wg_section=""
    if [[ -n ${WG_PUBLIC_KEY} ]]; then
        wg_section="
[wireguard.manual_mode]
public_key = \"${WG_PUBLIC_KEY}\"
        "
    fi

    if [[ $HOPR_NETWORK == "rotsee" ]]; then
        destinations='[destinations]

[destinations.12D3KooWDNcj8phBXj9ZJkAxSmjbwNUzEWtSsg6K6BeuKCAyZuCU]
meta = { location = "USA", state = "Iowa" }
# path = { intermediates = [ "12D3KooWRT74aKgHF36HwqvvxQiLCL1GVFRSv6eEFQ71wtY2vVvt" ] }
path = { hops = 0 }

[destinations.12D3KooWRKoZGSHR53rhK83omuomvFjUCV4hL3MwnkurU8C58SGQ]
meta = { location = "UK", city = "London" }
# path = { intermediates = [ "12D3KooWC69bPoKYzBYP95GXAumqeMKqxcrtb2vFYLuf4N16R2Lk" ] }
path = { hops = 0 }
'
    else
        destinations='[destinations]

[destinations.12D3KooWMEXkxWMitwu9apsHmjgDZ7imVHgEsjXfcyZfrqYMYjW7]
meta = { location = "Germany" }
path = { intermediates = [ "12D3KooWFUD4BSzjopNzEzhSi9chAkZXRKGtQJzU482rJnyd2ZnP" ] }

[destinations.12D3KooWBRB3y81TmtqC34JSd61uS8BVeUqWxCSBijD5nLhL6HU5]
meta = { location = "USA" }
path = { intermediates = [ "12D3KooWQLTR4zdLyXToQGx3YKs9LJmeL4MKJ3KMp4rfVibhbqPQ" ] }

[destinations.12D3KooWGdcnCwJ3645cFgo4drvSN3TKmxQFYEZK7HMPA6wx1bjL]
meta = { location = "Spain" }
path = { intermediates = [ "12D3KooWFnMnefPQp2k3XA3yNViBH4hnUCXcs9LasLUSv6WAgKSr" ] }
'
    fi

    cat <<EOF >./config.toml
# Generated by GnosisVPN install script

version = 2

[hoprd_node]
endpoint = "${HOPRD_API_ENDPOINT}"
api_token = "${HOPRD_API_TOKEN}"

internal_connection_port = ${HOPRD_SESSION_PORT}

${destinations}
${wg_section}
EOF
    echo "Configuration file generated at ${INSTALL_FOLDER}/config.toml"
}

print_outro() {
    echo ""
    echo "GnosisVPN installation completed successfully!"
    echo ""
    echo "You can now run the GnosisVPN client using the following commands:"
    echo -e "  - Start the client system service: ${BCyan}sudo ${INSTALL_FOLDER}/gnosis_vpn -c ${INSTALL_FOLDER}/config.toml${Color_Off}"
    echo -e "  - Check client status: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl status${Color_Off}"
    echo "You can connect to any of those locations:"
    if [[ $HOPR_NETWORK == "rotsee" ]]; then
        echo -e "  - Connect to London: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 12D3KooWRKoZGSHR53rhK83omuomvFjUCV4hL3MwnkurU8C58SGQ${Color_Off}"
        echo -e "  - Connect to Iowa: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 12D3KooWDNcj8phBXj9ZJkAxSmjbwNUzEWtSsg6K6BeuKCAyZuCU${Color_Off}"
    else
        echo -e "  - Connect to Spain: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 12D3KooWGdcnCwJ3645cFgo4drvSN3TKmxQFYEZK7HMPA6wx1bjL${Color_Off}"
        echo -e "  - Connect to USA: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 12D3KooWBRB3y81TmtqC34JSd61uS8BVeUqWxCSBijD5nLhL6HU5${Color_Off}"
        echo -e "  - Connect to Germany: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 12D3KooWMEXkxWMitwu9apsHmjgDZ7imVHgEsjXfcyZfrqYMYjW7${Color_Off}"
    fi
    echo ""
    echo "Configuration file is located at: ${INSTALL_FOLDER}/config.toml"
    echo "You can edit this file to change settings as needed."
    echo ""
    if [[ -n $WG_PUBLIC_KEY ]]; then
        echo "Your configuration was set up for WireGuard manual mode."
        echo "Check the client's log output after connecting to get a template for your tunnel configuration."
        echo ""
    fi
    echo "After establishing a VPN connection you can browse anonymously by using this HTTP proxy:"
    echo -e "${BCyan}HTTP(s) Proxy: 10.128.0.1:3128${Color_Off}"
    echo ""
}

main() {
    print_intro

    check_platform
    check_reqs

    prompt_install_dir
    prompt_api_access
    prompt_session_port

    fetch_network
    fetch_version_tag
    prompt_wireguard

    enter_install_dir
    fetch_binaries
    generate_config
    exit_install_dir

    print_outro
}

parse_arguments "$@"
main
