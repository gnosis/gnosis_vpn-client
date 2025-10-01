#!/usr/bin/env bash

set -euo pipefail

# inputs
NON_INTERACTIVE=""
INSTALL_FOLDER="./gnosis_vpn"
WG_PRIVATE_KEY=""
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
    echo "  --expert-wg <string>           use Gnosis VPN without WireGuard key rotation (not recommended) - expects WireGuard private key"
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
        --expert-wg)
            if [[ -n ${2:-} ]]; then
                WG_PRIVATE_KEY="$2"
                shift
            else
                echo "${BRed}Error:${Color_Off} --expert-wg requires a non-empty argument."
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
    echo "  - Open channels to the Gnosis VPN relayer nodes."
    echo "  - WireGuard tools installed on your system."
    echo ""
    echo ""
    echo "This installer will:"
    echo "  - Download the Gnosis VPN client and control application."
    echo "  - Generate a configuration file based on your input."

    echo ""
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo "Running in non-interactive mode."
        for i in {5..2}; do
            printf "\rProceeding in %d seconds..." "$i"
            sleep 1
        done
        printf "\rProceeding in 1 second...\n"
        sleep 1
    else
        read -r -n 1 -s -p "Press any key to continue or Ctrl+C to exit..."
    fi
}

prompt_install_dir() {
    echo ""
    if [[ -n ${NON_INTERACTIVE} ]]; then
        echo "[NON-INTERACTIVE] Using installation directory: ${INSTALL_FOLDER}"
        sleep 1
        return
    fi

    echo "Please specify the installation directory for Gnosis VPN."
    echo "Downloaded binaries will be placed in this directory."
    echo "The configuration file will also be created in this directory."
    declare install_dir
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
    declare os
    os="$(uname | tr '[:upper:]' '[:lower:]')"
    declare arch
    arch="$(uname -m)"
    if [[ ${os} == "darwin" ]]; then IS_MACOS="yes"; fi

    echo ""
    declare arch_tag
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
    declare required_cmds=(curl grep sed cat uname)
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

check_wg_commands() {
    declare required_cmds=(wg wg-quick)
    for cmd in "${required_cmds[@]}"; do
        if ! command -v "$cmd" &>/dev/null; then
            echo ""
            echo "Warning: $cmd command is not available."
            return 1
        fi
    done
}

check_wireguard() {
    if [[ -n ${NON_INTERACTIVE} ]]; then
        if ! check_wg_commands; then
            echo ""
            echo "[NON-INTERACTIVE] WireGuard tools are not installed."
            echo "[NON-INTERACTIVE] Cannot continue non-interactive installation."
            exit 1
        fi
        if [[ -n ${WG_PRIVATE_KEY} ]]; then
            echo ""
            echo "[NON-INTERACTIVE] Using provided WireGuard private key: ${WG_PRIVATE_KEY}"
            sleep 1
            return
        fi
    fi

    if ! check_wg_commands; then
        echo ""
        echo -e "${BRed}Error:${Color_Off}: WireGuard tools are not installed."

        echo ""
        if [[ -n ${IS_MACOS} ]]; then
            if command -v brew &>/dev/null; then
                echo "Executing 'brew install wireguard-tools' to install WireGuard tools."
                read -r -p "Press [Enter] to proceed."
                brew install wireguard-tools || {
                    echo ""
                    echo -e "${BRed}Failed to install WireGuard tools.${Color_Off}"
                    exit 1
                }
            else
                echo -e "${BRed}Error:${Color_Off} Homebrew (https://brew.sh) not found."
                echo "Unable to install WireGuard tools on macOS."
                echo -e "Please install Homebrew from ${BCyan}(https://brew.sh)${Color_Off}."
                echo "Then restart this installer. Exiting."
                exit 1
            fi
        else
            echo "You can install WireGuard tools using your package manager."
            echo "For example, on Debian/Ubuntu you can run:"
            echo -e "${BCyan}sudo apt install wireguard-tools${Color_Off}"
            echo "See https://www.wireguard.com/install/ for more information."
            echo ""
            read -r -p "Please install WireGuard tools - once installed press [Enter] to proceed."
        fi
        check_wireguard
        return
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
    declare binary="$1"
    declare url="https://github.com/gnosis/gnosis_vpn-client/releases/download/${VERSION_TAG}/${binary}"

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
        declare timestamp
        timestamp=$(date +%Y%m%d-%H%M%S)
        declare backup_name="config-${timestamp}.toml"
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
    declare wg_section=""
    if [[ -n ${WG_PRIVATE_KEY} ]]; then
        wg_section="
[wireguard]
force_private_key = \"${WG_PRIVATE_KEY}\"
        "
    fi

    declare destinations
    if [[ $HOPR_NETWORK == "rotsee" ]]; then
        destinations='
[destinations]

[destinations.0x7220CfE91F369bfE79F883c2891e97407D7a4D48]
meta = { location = "USA", state = "Iowa" }
path = { intermediates = [ "0xFE3AF421afB84EED445c2B8f1892E3984D3e41eA" ] }

[destinations.0xcD9D0E23cD999dFC0D1400D837F8e612DbbbDFAA]
meta = { location = "UK", city = "London" }
path = { intermediates = [ "0xc00B7d90463394eC29a080393fF09A2ED82a0F86" ] }
'
    else
        destinations='
[destinations]

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
meta = { location = "Germany" }
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA"] }

[destinations.0xa5Ca174Ef94403d6162a969341a61baeA48F57F8]
meta = { location = "USA" }
path = { intermediates = ["0x25865191AdDe377fd85E91566241178070F4797A"] }

[destinations.0x8a6E6200C9dE8d8F8D9b4c08F86500a2E3Fbf254]
meta = { location = "Spain" }
path = { intermediates = ["0x2Cf9E5951C9e60e01b579f654dF447087468fc04"] }

[destinations.0x9454fc1F54DC7682124BA2d153345f4F6b404A79]
meta = { location = "India" }
path = { intermediates = [ "0x652cDe234ec643De0E70Fb3a4415345D42bAc7B2" ] }
'
    fi

    cat <<EOF >./config.toml
# Generated by Gnosis VPN install script

version = 4

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
        echo -e "  - Connect to London: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 0xcD9D0E23cD999dFC0D1400D837F8e612DbbbDFAA${Color_Off}"
        echo -e "  - Connect to Iowa: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 0x7220CfE91F369bfE79F883c2891e97407D7a4D48${Color_Off}"
    else
        echo -e "  - Connect to Spain: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 0x8a6E6200C9dE8d8F8D9b4c08F86500a2E3Fbf254${Color_Off}"
        echo -e "  - Connect to USA: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 0xa5Ca174Ef94403d6162a969341a61baeA48F57F8${Color_Off}"
        echo -e "  - Connect to Germany: ${BCyan}${INSTALL_FOLDER}/gnosis_vpn-ctl connect 0xD9c11f07BfBC1914877d7395459223aFF9Dc2739${Color_Off}"
    fi
    echo ""
    echo "Configuration file is located at: ${INSTALL_FOLDER}/config.toml"
    echo "You can edit this file to change settings as needed."
    echo ""
    echo "After establishing a VPN connection you can browse anonymously by using this HTTPS proxy:"
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
