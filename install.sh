#!/usr/bin/env bash

set -euo pipefail

# inputs
NON_INTERACTIVE=""
INSTALL_FOLDER="./gnosis_vpn"
HOPRD_API_ENDPOINT=""
HOPRD_API_TOKEN=""
HOPRD_SESSION_PORT=1422
WG_PUBLIC_KEY=""
EXPERT_CHANNELS=""
VERSION_TAG=""

# internals
PLATFORM=""
HOPR_NETWORK=""
IS_MACOS=""

# taken from https://stackoverflow.com/a/28938235
BPurple='\033[1;35m'
BCyan='\033[1;36m'
BRed='\033[1;31m'
Color_Off='\033[0m'

GLOBAL_NAME="${BPurple}<Gnosis VPN installer script>${Color_Off}"

usage() {
    echo -e "Usage: ${GLOBAL_NAME} [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --non-interactive              run the script in non-interactive mode"
    echo "  -i, --install-folder <string>  installation folder (default: ./gnosis_vpn)"
    echo "  --api-endpoint <string>        hoprd API endpoint (default: empty, will prompt)"
    echo "  --api-token <string>           hoprd API token (default: empty, will prompt)"
    echo "  --session-port <integer>       hoprd session port (default: 1422, will prompt)"
    echo "  --expert-wg <string>           use Gnosis VPN in manual WireGuard mode (not recommended) - expects WireGuard public key"
    echo "  --expert-channels              ignore Gnosis VPN relayer channels (not recommended)"
    echo "  --version-tag <string>         specific version tag to install"
    echo "  --help                         show this help message and exit"
    exit 0
}

trim() {
    declare str="$*"
    # strip leading
    str="${str#"${str%%[![:space:]]*}"}"
    # strip trailing
    str="${str%"${str##*[![:space:]]}"}"
    printf '%s' "$str"
}

parse_arguments() {
    while [[ $# -gt 0 ]]; do
        case $1 in
        --help) usage ;;
        --non-interactive) NON_INTERACTIVE="yes" ;;
        --expert-channels) EXPERT_CHANNELS="yes" ;;
        -i | --install-folder)
            if [[ -n ${2:-} ]]; then
                INSTALL_FOLDER="$2"
                shift
            else
                echo "${BRed}Error:${Color_Off} --install-folder requires a non-empty argument."
                exit 1
            fi
            ;;
        --api-endpoint)
            if [[ -n ${2:-} ]]; then
                HOPRD_API_ENDPOINT="$2"
                shift
            else
                echo "${BRed}Error:${Color_Off} --api-endpoint requires a non-empty argument."
                exit 1
            fi
            ;;
        --api-token)
            if [[ -n ${2:-} ]]; then
                HOPRD_API_TOKEN="$2"
                shift
            else
                echo "${BRed}Error:${Color_Off} --api-token requires a non-empty argument."
                exit 1
            fi
            ;;
        --session-port)
            if [[ -n ${2:-} ]]; then
                HOPRD_SESSION_PORT="$2"
                shift
            else
                echo "${BRed}Error:${Color_Off} --session-port requires a non-empty argument."
                exit 1
            fi
            ;;
        --expert-wg)
            if [[ -n ${2:-} ]]; then
                WG_PUBLIC_KEY="$2"
                shift
            else
                echo "${BRed}Error:${Color_Off} --expect-wg requires a non-empty argument."
                exit 1
            fi
            ;;
        --version-tag)
            if [[ -n ${2:-} ]]; then
                VERSION_TAG="$2"
                shift
            else
                echo "${BRed}Error:${Color_Off} --version-tag requires a non-empty argument."
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
    echo ""
    echo -e "Welcome to the ${GLOBAL_NAME}!"
    echo "This script will help you install and configure the Gnosis VPN client on your system."
    echo ""
    echo "Requirements:"
    echo "  - A running hoprd node that will act as your entry node."
    echo "  - Open channels to the Gnosis VPN relayer nodes."
    echo "  - WireGuard tools installed on your system."
    echo "  - An additional open port on your node for Gnosis VPN to connect to."
    echo ""
    echo "Note:"
    echo "  Gnosis VPN uses a port called 'internal_connection_port' for both TCP and UDP connections."
    echo ""
    echo "This installer will:"
    echo "  - Download the Gnosis VPN client and control application."
    echo "  - Prompt you for API access to your hoprd node."
    echo "  - Check your hoprd node for open channels to Gnosis VPN relayer nodes."
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
    else
        read -r -n 1 -s -p "Press any key to continue or Ctrl+C to exit..."
    fi
}

prompt_install_dir() {
    echo ""
    declare install_dir
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo "[NON-INTERACTIVE] Using installation directory: ${INSTALL_FOLDER}"
        sleep 1
        return
    fi

    echo "Please specify the installation directory for Gnosis VPN."
    echo "Downloaded binaries will be placed in this directory."
    echo "The configuration file will also be created in this directory."
    read -r -p "Enter installation directory [${INSTALL_FOLDER:-<blank>}]: " install_dir
    install_dir=$(trim "${install_dir:-$INSTALL_FOLDER}")
    # do not rely on realpath as it is unstable on macOS
    if INSTALL_FOLDER=$(realpath "${install_dir:-$INSTALL_FOLDER}" 2>/dev/null); then
        :
    else
        INSTALL_FOLDER=$([[ $install_dir == /* ]] && echo "$install_dir" || echo "$PWD/${install_dir#./}")
    fi
    echo ""
    echo "Gnosis VPN will be installed to: \"${INSTALL_FOLDER}\"."
}

# from https://stackoverflow.com/a/37840948
urldecode() {
    : "${*//+/ }"
    echo -e "${_//%/\\x}"
}

prompt_api_access() {
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo ""
        echo "[NON-INTERACTIVE] Using hoprd API endpoint: ${HOPRD_API_ENDPOINT:-}"
        sleep 1
        echo "[NON-INTERACTIVE] Using hoprd API token: ${HOPRD_API_TOKEN:-}"
        sleep 1
        return
    fi

    echo ""
    echo "Gnosis VPN uses your hoprd node as entry connection point."
    echo "Therefore, you need to provide the API endpoint and token for your hoprd node."
    echo -e "If you are connected to your hoprd node via ${BCyan}HOPR Admin UI${Color_Off}, paste its full URL."
    echo "The script will try to parse the required values from the URL."
    declare admin_url
    read -r -p "Enter full hoprd admin interface URL [or leave blank to provide API_ENDPOINT and API_TOKEN separately]: " admin_url
    admin_url=$(trim "${admin_url}")

    declare api_endpoint api_token
    api_endpoint=""
    api_token=""
    if [[ -n ${admin_url} ]]; then
        echo ""
        echo "Parsing admin URL..."
        declare decoded_url
        decoded_url=$(urldecode "$admin_url")
        api_endpoint=$(echo "$decoded_url" | grep -o 'apiEndpoint=[^&]*' | sed 's/apiEndpoint=//' || true)
        api_token=$(echo "$decoded_url" | grep -o 'apiToken=[^&]*' | sed 's/apiToken=//' || true)
    fi

    echo ""
    if [[ -z ${api_endpoint} ]]; then
        if [[ -n ${admin_url} ]]; then
            echo "Warning: Could not parse API endpoint from the provided URL. Please provide it manually."
        fi
        read -r -p "Enter hoprd API endpoint [${HOPRD_API_ENDPOINT:-<blank>}]: " api_endpoint
        api_endpoint=$(trim "${api_endpoint}")
    else
        echo "Using parsed API endpoint: ${api_endpoint}"
    fi
    if [[ -z ${api_token} ]]; then
        if [[ -n ${admin_url} ]]; then
            echo "Warning: Could not parse API token from the provided URL. Please provide it manually."
        fi
        read -r -p "Enter hoprd API token [${HOPRD_API_TOKEN:-<blank>}]: " api_token
        api_token=$(trim "${api_token}")
    else
        echo "Using parsed API token: ${api_token}"
    fi

    HOPRD_API_ENDPOINT="${api_endpoint:-$HOPRD_API_ENDPOINT}"
    HOPRD_API_TOKEN="${api_token:-$HOPRD_API_TOKEN}"
}

prompt_session_port() {
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo ""
        echo "[NON-INTERACTIVE] Using hoprd session port: ${HOPRD_SESSION_PORT}"
        sleep 1
        return
    fi

    echo ""
    echo "Gnosis VPN requires a port for internal connections."
    echo "This port will be used for both TCP and UDP connections."
    read -r -p "Enter hoprd session port [${HOPRD_SESSION_PORT:-<blank>}]: " session_port
    HOPRD_SESSION_PORT=$(trim "${session_port:-$HOPRD_SESSION_PORT}")
}

fetch_network() {
    echo ""
    echo "Accessing hoprd API to determine network"
    HOPR_NETWORK=$(curl --fail -L --progress-bar \
        -H "Content-Type: application/json" \
        -H "x-auth-token: $HOPRD_API_TOKEN" \
        "${HOPRD_API_ENDPOINT}/api/v3/node/info" |
        grep '"network":' |
        sed -E 's/.*"network": *"([^"]*)".*/\1/')
    echo ""
    echo "Detected network: $HOPR_NETWORK"
}

check_channel() {
    declare channels channel name
    channels="$1"
    channel="$2"
    name="$3"

    #  \"peerAddress\":\"<peer>\" followed by any non-}  then \"status\":\"(capture)\"
    local re='\"peerAddress\":\"'"$channel"'\"[^}]*\"status\":\"([^\"]+)\"'

    if [[ $channels =~ $re ]]; then
        # BASH_REMATCH[1] now holds the status
        [[ ${BASH_REMATCH[1]} == Open ]]
    else
        echo -e ""
        echo -e "${BRed}Error:${Color_Off} Missing channel to ${name} relayer"
        echo -e "Please open a channel to ${name} relayer node ${BCyan}${channel}${Color_Off} before proceeding."
        return 1
    fi
}

check_channels() {
    if [[ -n ${NON_INTERACTIVE} ]]; then
        if [[ -n ${EXPERT_CHANNELS} ]]; then
            echo ""
            echo "[NON-INTERACTIVE] Skipping Gnosis VPN relayer channels check."
            sleep 1
            return
        fi
    fi

    if [[ -n ${EXPERT_CHANNELS} ]]; then
        echo ""
        echo "Skipping Gnosis VPN relayer channels check."
        return
    fi

    echo ""
    echo "Checking for open channels to Gnosis VPN relayer nodes..."
    declare channels
    channels=$(curl --fail -L --progress-bar \
        -H "Content-Type: application/json" \
        -H "x-auth-token: $HOPRD_API_TOKEN" \
        "${HOPRD_API_ENDPOINT}/api/v3/channels?includingClosed=false&fullTopology=false")

    declare missing_channel=""
    if [[ $HOPR_NETWORK == "rotsee" ]]; then
        check_channel "${channels}" "0xc00B7d90463394eC29a080393fF09A2ED82a0F86" "Stockholm" || missing_channel="yes"
        check_channel "${channels}" "0xFE3AF421afB84EED445c2B8f1892E3984D3e41eA" "Columbus" || missing_channel="yes"
    else
        check_channel "${channels}" "0x25865191AdDe377fd85E91566241178070F4797A" "USA" || missing_channel="yes"
        check_channel "${channels}" "0x652cDe234ec643De0E70Fb3a4415345D42bAc7B2" "India" || missing_channel="yes"
        check_channel "${channels}" "0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA" "Germany" || missing_channel="yes"
        check_channel "${channels}" "0x2Cf9E5951C9e60e01b579f654dF447087468fc04" "Spain" || missing_channel="yes"
    fi

    if [[ -n ${NON_INTERACTIVE} ]]; then
        if [[ -n ${missing_channel} ]]; then
            echo ""
            echo "[NON-INTERACTIVE] Missing open channels to Gnosis VPN relayer nodes."
            echo "[NON-INTERACTIVE] Stopping non interactive installation."
            exit 1
        fi
        return
    fi

    if [[ -n ${missing_channel} ]]; then
        for i in {30..2}; do
            printf "\rRechecking open channels to Gnosis VPN relayer nodes in %d seconds... " "$i"
            sleep 1
        done
        printf "\rRechecking open channels to Gnosis VPN relayer nodes in 1 second...  "
        sleep 1
        check_channels
        return
    fi

    echo ""
    echo "Found open channels to Gnosis VPN relayer nodes"
}

fetch_version_tag() {
    if [[ -n ${VERSION_TAG} ]]; then
        echo ""
        echo "Verifying provided version tag: ${VERSION_TAG}"
        curl --fail --head -L --progress-bar \
            "https://codeload.github.com/gnosis/gnosis_vpn-client/tar.gz/${VERSION_TAG}" &>/dev/null ||
            (
                echo ""
                echo -e "${BRed}Error:${Color_Off} Provided version tag \"${VERSION_TAG}\" is not valid or does not exist."
                exit 1
            )
        return
    fi

    echo ""
    echo "Fetching the latest Gnosis VPN release tag from GitHub..."

    VERSION_TAG=$(curl --fail -L --progress-bar https://raw.githubusercontent.com/gnosis/gnosis_vpn-client/main/LATEST)
    echo ""
    echo "Downloadable Gnosis VPN version found: ${VERSION_TAG}"
}

check_platform() {
    declare os arch arch_tag
    os="$(uname | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"
    if [[ ${os} == "darwin" ]]; then IS_MACOS="yes"; fi

    echo ""
    case "$arch" in
    x86_64 | amd64) arch_tag="x86_64" ;;
    aarch64 | arm64) arch_tag="aarch64" ;;
    armv7l) arch_tag="armv7l" ;;
    *)
        echo -e "${BRed}Unsupported architecture:${Color_Off} $arch"
        exit 1
        ;;
    esac

    echo "Detected architecture: $arch_tag-$os"
    PLATFORM="$arch_tag-$os"
}

check_reqs() {
    required_cmds=(curl grep sed cat uname)
    if [[ -n ${IS_MACOS} ]]; then
        required_cmds+=(xattr)
    fi

    for cmd in "${required_cmds[@]}"; do
        if ! command -v "$cmd" &>/dev/null; then
            echo ""
            echo "${BRed}Error:${Color_Off} $cmd is required to run this script. Please install $cmd and try again."
            exit 1
        fi
    done

}

check_wireguard() {
    declare wg_fail
    wg_fail=""
    if ! command -v wg &>/dev/null; then
        echo ""
        echo "Warning: Probing for wg command failed."
        wg_fail="yes"
    fi
    if [[ -z $wg_fail ]] && ! command -v wg-quick &>/dev/null; then
        echo ""
        echo "Warning: Probing for wg-quick command failed."
        wg_fail="yes"
    fi

    if [[ -n ${NON_INTERACTIVE} ]]; then
        if [[ -n ${WG_PUBLIC_KEY} ]]; then
            echo ""
            echo "[NON-INTERACTIVE] Using provided WireGuard public key: ${WG_PUBLIC_KEY}"
            sleep 1
            return
        fi
        if [[ -n ${wg_fail} ]]; then
            echo ""
            echo "[NON-INTERACTIVE] WireGuard tools are not installed."
            echo "[NON-INTERACTIVE] Cannot continue non interactive installation."
            echo "[NON-INTERACTIVE] Please provide WG_PUBLIC_KEY environment variable or install WireGuard tools."
            exit 1
        fi
    fi

    if [[ -n ${WG_PUBLIC_KEY} ]]; then
        echo ""
        echo "Using provided WireGuard public key: ${WG_PUBLIC_KEY}"
        return
    fi

    if [[ -n $wg_fail ]]; then
        echo ""
        echo -e "${BRed}Error:${Color_Off}: WireGuard tools are not installed."
        echo ""
        if [[ -n ${IS_MACOS} ]]; then
            echo "You can install WireGuard tools using Homebrew (https://brew.sh):"
            echo -e "${BCyan}brew install wireguard-tools${Color_Off}"
        else
            echo "You can install WireGuard tools using your package manager."
            echo "For example, on Debian/Ubuntu you can run:"
            echo -e "${BCyan}sudo apt install wireguard-tools${Color_Off}"
            echo "See https://www.wireguard.com/install/ for more information."
        fi
        echo ""
        read -r -p "Please install WireGuard tools - once installed press [Enter] to proceed."
        check_wireguard
    fi

    echo ""
    echo "Successfully detected WireGuard tools."
}

enter_install_dir() {
    mkdir -p "${INSTALL_FOLDER}"
    pushd "${INSTALL_FOLDER}" >/dev/null || {
        echo ""
        echo -e "${BRed}Failed to create or access installation directory:${Color_Off} $INSTALL_FOLDER"
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
        echo ""
        echo "Detected macOS - Gnosis VPN binaries need to be removed from apple quarantine mode."
        echo -e "We will run these two commands with ${BCyan}sudo${Color_Off} to get required privileges:"
        echo "sudo xattr -dr com.apple.quarantine ./gnosis_vpn"
        echo "sudo xattr -dr com.apple.quarantine ./gnosis_vpn-ctl"
        echo "These are the only commands that require sudo privileges."
        sleep 1
        sudo xattr -dr com.apple.quarantine ./gnosis_vpn
        sudo xattr -dr com.apple.quarantine ./gnosis_vpn-ctl
    fi
}

backup_config() {
    if [[ -f "./config.toml" ]]; then
        declare timestamp backup_name
        timestamp=$(date +%Y%m%d-%H%M%S)
        backup_name="config-${timestamp}.toml"
        cp "./config.toml" "./${backup_name}" || {
            echo ""
            echo -e "${BRed}Failed to back up config file:${Color_Off} ./config.toml to ./${backup_name}"
            exit 1
        }
        echo ""
        echo "Backed up existing config to ${backup_name}"
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
path = { intermediates = [ "12D3KooWRT74aKgHF36HwqvvxQiLCL1GVFRSv6eEFQ71wtY2vVvt" ] }

[destinations.12D3KooWRKoZGSHR53rhK83omuomvFjUCV4hL3MwnkurU8C58SGQ]
meta = { location = "UK", city = "London" }
path = { intermediates = [ "12D3KooWC69bPoKYzBYP95GXAumqeMKqxcrtb2vFYLuf4N16R2Lk" ] }
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

[destinations.12D3KooWJVhifJNJQPDSYz5aC8hWEyFdgB3xdJyKYQoPYLn4Svv8]
meta = { location = "India" }
path = { intermediates = [ "12D3KooWFcTznqz9wEvPFPsTTXDVtWXtPy8jo4AAUXHUqTW8fP2h" ] }
'
    fi

    cat <<EOF >./config.toml
# Generated by Gnosis VPN install script

version = 2

[hoprd_node]
endpoint = "${HOPRD_API_ENDPOINT}"
api_token = "${HOPRD_API_TOKEN}"

internal_connection_port = ${HOPRD_SESSION_PORT}

${destinations}
${wg_section}
EOF
    echo ""
    echo "Configuration file generated at ${INSTALL_FOLDER}/config.toml"
}

print_outro() {
    echo ""
    echo "Gnosis VPN installation completed successfully!"
    echo ""
    echo "You can now run the Gnosis VPN client using the following commands:"
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
    check_wireguard
    fetch_version_tag

    prompt_api_access
    fetch_network
    check_channels

    prompt_session_port
    prompt_install_dir

    enter_install_dir
    fetch_binaries
    backup_config
    generate_config
    exit_install_dir

    print_outro
}

parse_arguments "$@"
main
