#!/usr/bin/env bash

set -euo pipefail

NON_INTERACTIVE=""
INSTALL_FOLDER="${INSTALL_FOLDER:-./gnosis_vpn}"
HOPRD_API_ENDPOINT="${HOPRD_API_ENDPOINT:-}"
HOPRD_API_TOKEN="${HOPRD_API_TOKEN:-}"
HOPRD_SESSION_PORT="${HOPRD_SESSION_PORT:-1422}"
FUN_RETURN_VALUE=""
IS_MACOS=""
WG_PUBLIC_KEY="${WG_PUBLIC_KEY:-}"

parse_arguments() {
  while [[ $# -gt 0 ]]; do
    case $1 in
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
    *)
      echo "Unknown parameter passed: $1"
      exit 1
      ;;
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
  echo '  GnosisVPN uses a port called `internal_connection_port` for both TCP and UDP connections.'
  echo ""
  echo "This installer will:"
  echo "  - Download the GnosisVPN client and control application."
  echo "  - Prompt you for API access to your HOPRD node."
  echo '  - Prompt you for the `internal_connection_port`.'
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

install_folder() {
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
  read -r -p "Installation directory [${INSTALL_FOLDER}]: " install_dir
  INSTALL_FOLDER="${install_dir:-$INSTALL_FOLDER}"
}

api_access() {
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
  echo "For convenience you can just paste your admin interface URL here."
  declare admin_url
  read -r -p "HOPRD admin interface URL [if empty will prompt for API_ENDPOINT and API_TOKEN separately]: " admin_url

  declare api_endpoint api_token
  api_endpoint=""
  api_token=""
  if [[ -n ${admin_url} ]]; then
    echo "Parsing admin URL..."
    api_endpoint=$(echo "$admin_url" | grep -oP '(?<=apiEndpoint=)[^&]+' || true)
    api_token=$(echo "$admin_url" | grep -oP '(?<=apiToken=)[^&]+' || true)
  fi
  if [[ -z ${api_endpoint} ]]; then
    if [[ -n ${admin_url} ]]; then
      echo "Error: Could not parse API endpoint from the provided URL."
    fi
    read -r -p "HOPRD API endpoint [${HOPRD_API_ENDPOINT}]: " api_endpoint
  else
    echo "Using parsed API endpoint: ${api_endpoint}"
  fi
  if [[ -z ${api_token} ]]; then
    if [[ -n ${admin_url} ]]; then
      echo "Error: Could not parse API token from the provided URL."
    fi
    read -r -p "HOPRD API token [${HOPRD_API_TOKEN}]: " api_token
  else
    echo "Using parsed API token: ${api_token}"
  fi

  HOPRD_API_ENDPOINT="${api_endpoint:-$HOPRD_API_ENDPOINT}"
  HOPRD_API_TOKEN="${api_token:-$HOPRD_API_TOKEN}"
}

session_port() {
  if [[ -n ${NON_INTERACTIVE} ]]; then
    echo "[NON-INTERACTIVE] Using HOPRD session port: ${HOPRD_SESSION_PORT}"
    sleep 1
    return
  fi

  echo ""
  echo "GnosisVPN requires a port for internal connections."
  echo "This port will be used for both TCP and UDP connections."
  read -r -p "HOPRD session port [${HOPRD_SESSION_PORT}]: " session_port
  HOPRD_SESSION_PORT="${session_port:-$HOPRD_SESSION_PORT}"
}

fetch_network() {
  echo ""
  echo "Accessing HOPRD API to determine network"
  declare network
  network=$(curl -L --progress-bar \
    -H "Content-Type: application/json" \
    -H "x-auth-token: $HOPRD_API_TOKEN" \
    "${HOPRD_API_ENDPOINT}/api/v3/node/info" |
    grep -Po '(?<="network":\")[^"]*')
  echo "Detected network: $network"
  FUN_RETURN_VALUE="$network"
}

fetch_latest_tag() {
  echo ""
  echo "Fetching the latest GnosisVPN release tag from GitHub..."
  latest_tag=$(curl -L --progress-bar \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/gnosis/gnosis_vpn-client/releases/latest" |
    grep -Po '(?<="tag_name": \")[^"]*')
  echo "GnosisVPN version found: $latest_tag"
  FUN_RETURN_VALUE="$latest_tag"
}

run_platform() {
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
  FUN_RETURN_VALUE="$arch_tag-$os"
}

prompt_wg_public_key() {
  echo "GnosisVPN will only be able to run in manual mode, where you need to manage your WireGuard tunnel manually."
  echo "However GnosisVPN will try to help you with that."
  echo "In order to provide the underlying connection, GnosisVPN needs your WireGuard public key."
  echo "Go ahead and create an empty tunnel in your favorite WireGuard application and copy the public key."
  declare wg_pub_key
  read -r -p "WireGuard public key [${WG_PUBLIC_KEY}]: " wg_pub_key
  WG_PUBLIC_KEY="${wg_pub_key:-$WG_PUBLIC_KEY}"
}

check_wireguard() {
  if [[ -n ${NON_INTERACTIVE} ]]; then
    if [[ -n ${WG_PUBLIC_KEY} ]]; then
      echo "[NON-INTERACTIVE] Using provided WireGuard public key: ${WG_PUBLIC_KEY}"
      sleep 1
      return
    fi
    if [[ -n ${IS_MACOS} ]]; then
      echo "[NON-INTERACTIVE] WireGuard public key is required for macOS. Cannot continue non interactive installation."
      echo "[NON-INTERACTIVE] Please provide WG_PUBLIC_KEY environment variable."
      exit 1
    fi
  fi

  if [[ -n ${IS_MACOS} ]]; then
    echo ""
    echo "MacOS detected - GnosisVPN cannot manage your WireGuard tunnel automatically yet."
    prompt_wg_public_key
    return
  fi

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

  if [[ -n $wg_fail ]]; then

    if [[ -n ${NON_INTERACTIVE} ]]; then
      echo "[NON-INTERACTIVE] WireGuard tools are not installed. Cannot continue non interactive installation."
      echo "[NON-INTERACTIVE] Please provide WG_PUBLIC_KEY environment variable."
      exit 1
    fi

    echo ""
    echo "WireGuard tools are not installed."
    prompt_wg_public_key
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
  declare binary latest_tag url
  latest_tag="$1"
  binary="$2"

  url="https://github.com/gnosis/gnosis_vpn-client/releases/download/${latest_tag}/${binary}"

  echo ""
  echo "Downloading ${binary} from ${url}..."
  curl -L --progress-bar "${url}" -o "${binary}"
}

fetch_binaries() {
  declare latest_tag platform_tag
  latest_tag="$1"
  platform_tag="$2"
  download_binary "$latest_tag" "gnosis_vpn-${platform_tag}"
  mv "./gnosis_vpn-${platform_tag}" ./gnosis_vpn
  download_binary "$latest_tag" "gnosis_vpn-ctl-${platform_tag}"
  mv "./gnosis_vpn-ctl-${platform_tag}" ./gnosis_vpn-ctl

  chmod +x ./gnosis_vpn
  chmod +x ./gnosis_vpn-ctl

  if [[ -n ${IS_MACOS} ]]; then
    echo "Detected macOS - GnosisVPN binaries need to be removed from apple quarantine mode."
    echo "We will run these two commands with sudo to get required privileges:"
    echo "sudo xattr -dr com.apple.quarantine ./gnosis_vpn"
    echo "sudo xattr -dr com.apple.quarantine ./gnosis_vpn-ctl"
    echo "These are the only commands that require sudo privileges."
    sleep 1
    sudo xattr -dr com.apple.quarantine ./gnosis_vpn
    sudo xattr -dr com.apple.quarantine ./gnosis_vpn-ctl
  fi
}

destinations() {
  declare network
  local network="$1"
  if [[ $network == "rotsee" ]]; then
    echo '[destinations]

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
    echo '[destinations]

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
}

generate_config() {
  declare wg_section
  wg_section=""
  if [[ -n ${WG_PUBLIC_KEY} ]]; then
    wg_section="
[wireguard.manual_mode]
public_key = \"${WG_PUBLIC_KEY}\"
        "
  fi
  echo "# Generated by GnosisVPN install script

version = 2

[hoprd_node]
endpoint = \"${HOPRD_API_ENDPOINT}\"
api_token = \"${HOPRD_API_TOKEN}\"

internal_connection_port = ${HOPRD_SESSION_PORT}

$dests
$wg_section
" >./config.toml
  echo "Configuration file generated at ${INSTALL_FOLDER}/config.toml"
}

print_outro() {
  echo ""
  echo "GnosisVPN installation completed successfully!"
  echo ""
  echo "You can now run the GnosisVPN client using the following commands:"
  echo "  - Start the client: sudo ${INSTALL_FOLDER}/gnosis_vpn -c ${INSTALL_FOLDER}/config.toml"
  echo "  - Instruct the client: ${INSTALL_FOLDER}/gnosis_vpn-ctl status"
  echo "  - Check available commands: ${INSTALL_FOLDER}/gnosis_vpn-ctl --help"
  echo ""
  echo "Configuration file is located at: ${INSTALL_FOLDER}/config.toml"
  echo "You can edit this file to change settings as needed."
}

main() {
  print_intro
  install_folder
  api_access
  session_port

  declare network latest_tag platform_tag wg_pub_key
  fetch_network
  network=${FUN_RETURN_VALUE}
  fetch_latest_tag
  latest_tag=${FUN_RETURN_VALUE}
  run_platform
  platform_tag=${FUN_RETURN_VALUE}

  check_wireguard "${platform_tag}"
  dests=$(destinations "$network")

  enter_install_dir
  fetch_binaries "$latest_tag" "$platform_tag"
  generate_config "$dests"
  exit_install_dir

  print_outro
}

parse_arguments "$@"
main
