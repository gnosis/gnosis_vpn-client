#!/usr/bin/env bash

set -euo pipefail

platform() {
    declare os,arch,arch_tag
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
    declare binary,latest_tag,url
    latest_tag="$1"
    binary="$2"

    url="https://github.com/gnosis/gnosis_vpn-client/releases/download/${latest_tag}/${binary}"

    curl -L --fail --show-error --retry 3 "${url}" -o "${url}"
}

foo() {
    declare latest_tag

    latest_tag=$(curl -L -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/gnosis/gnosis_vpn-client/releases/latest" \
        | grep -Po '(?<="tag_name": \")[^"]*')
}

main() {

}


# Prompt user for API configuration
read -p "Enter your API endpoint (e.g., https://api.node.local:8080): " API_ENDPOINT
read -p "Enter your API access token: " -s API_TOKEN
echo

# Verify API access
echo "Verifying API access..."
HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer ${API_TOKEN}" \
  "${API_ENDPOINT}/info")

if [[ "$HTTP_STATUS" -ne 200 ]]; then
  echo "Failed to verify API access (HTTP $HTTP_STATUS). Please check your endpoint and token."
  exit 1
fi

API_INFO=$(curl -s \
  -H "Authorization: Bearer ${API_TOKEN}" \
  "${API_ENDPOINT}/info")

# Generate config file
CONFIG_FILE="config.json"
cat > "$CONFIG_FILE" <<EOF
{
  "api_endpoint": "${API_ENDPOINT}",
  "api_token": "${API_TOKEN}",
  "node_info": $API_INFO
}
EOF

echo "Configuration written to $INSTALL_DIR/$CONFIG_FILE"

echo "Installation complete. Binaries and config are in $INSTALL_DIR."

main
