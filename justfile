default:
    just --list

# run root binary locally with a config file and Blokli URL
run-local config_file blokli_url binary="./target/release/gnosis_vpn-root" worker_binary="./target/release/gnosis_vpn-worker" rust_log="info":
    sudo RUST_LOG="{{ rust_log }}" "{{ binary }}" --config-path "{{ config_file }}" --hopr-blokli-url "{{ blokli_url }}" --worker-binary "{{ worker_binary }}"

# build static linux binary (x86_64)
build:
    nix build -L .#binary-gnosis_vpn-x86_64-linux

# build static linux binary (ARM64)
build-arm64:
    nix build -L .#binary-gnosis_vpn-aarch64-linux

# build docker image (x86_64)
docker-build: build
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    cp -f result/bin/gnosis_vpn-root result/bin/gnosis_vpn-worker result/bin/gnosis_vpn-ctl docker/
    docker build --platform linux/x86_64 -t gnosis_vpn-client docker/

# build static linux binary (x86_64) on the `pix-test` PIX deposit pool instead of Curvy; testing only
build-pix-test:
    nix build -L .#binary-gnosis_vpn-pix-test-x86_64-linux -o result-pix-test

# build docker image (x86_64) on the `pix-test` PIX deposit pool; tagged gnosis_vpn-client:pix-test
docker-build-pix-test: build-pix-test
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    cp -f result-pix-test/bin/gnosis_vpn-root result-pix-test/bin/gnosis_vpn-worker result-pix-test/bin/gnosis_vpn-ctl docker/
    docker build --platform linux/x86_64 -t gnosis_vpn-client:pix-test docker/

# build docker image (ARM64)
docker-build-arm64: build-arm64
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    cp -f result/bin/gnosis_vpn-root result/bin/gnosis_vpn-worker result/bin/gnosis_vpn-ctl docker/
    docker build --platform linux/arm64 -t gnosis_vpn-client:arm64 docker/

# run docker container detached; CONFIG_DIR must hold client.toml (+ identity file if used, + the geoip db its geoip_db points at), GNOSISVPN_HOPR_BLOKLI_URL must be set
# see gnosis_vpn-testenv's client-start for a full example against a live cluster
docker-run:
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    log_level=$(if [ "${RUST_LOG:-}" = "" ]; then echo info; else echo "${RUST_LOG}"; fi)
    config_dir="${CONFIG_DIR:?CONFIG_DIR must point at a directory containing client.toml}"
    blokli_url="${GNOSISVPN_HOPR_BLOKLI_URL:?GNOSISVPN_HOPR_BLOKLI_URL must be set; the client ships no default}"

    docker run --detach --rm \
        --env RUST_LOG=${log_level} \
        --env GNOSISVPN_CONFIG_PATH=/config/client.toml \
        --env GNOSISVPN_HOPR_BLOKLI_URL="${blokli_url}" \
        --env GNOSISVPN_HOPR_IDENTITY_FILE=${GNOSISVPN_HOPR_IDENTITY_FILE:-} \
        --env GNOSISVPN_HOPR_IDENTITY_PASS=${GNOSISVPN_HOPR_IDENTITY_PASS:-} \
        --volume "${config_dir}:/config:ro" \
        --cap-add=NET_ADMIN \
        --add-host=host.docker.internal:host-gateway \
        --name gnosis_vpn-client gnosis_vpn-client

# stop docker container
docker-stop:
    docker stop gnosis_vpn-client

# enter docker container interactively
docker-enter:
    docker exec --interactive --tty gnosis_vpn-client sh

system-tests test_binary="gnosis_vpn-system_tests":
    #!/usr/bin/env bash
    set -euo pipefail

    : "${SYSTEM_TEST_HOPRD_ID:?SYSTEM_TEST_HOPRD_ID must be set to run system tests}"
    : "${SYSTEM_TEST_HOPRD_ID_PASSWORD:?SYSTEM_TEST_HOPRD_ID_PASSWORD must be set to run system tests}"
    : "${SYSTEM_TEST_NETWORK:?SYSTEM_TEST_NETWORK must be set to run system tests (jura-dev|piz-palu-dev)}"
    : "${SYSTEM_TEST_WORKER_BINARY:?SYSTEM_TEST_WORKER_BINARY must be set to run system tests}"
    : "${SYSTEM_TEST_ROOT_BINARY:?SYSTEM_TEST_ROOT_BINARY must be set to run system tests}"
    : "${SYSTEM_TEST_CTL_BINARY:?SYSTEM_TEST_CTL_BINARY must be set to run system tests}"

    networks_root="{{ justfile_directory() }}/gnosis_vpn-system_tests/networks"

    # Exact match against the fixture directories, so a traversal value cannot resolve past networks/
    available="$(cd "${networks_root}" && ls -d */ | tr -d /)"
    if ! printf '%s\n' "${available}" | grep -qxF "${SYSTEM_TEST_NETWORK}"; then
        echo "ERROR: unknown network '${SYSTEM_TEST_NETWORK}', available: $(printf '%s\n' "${available}" | tr '\n' ' ')" >&2
        exit 1
    fi
    network_dir="${networks_root}/${SYSTEM_TEST_NETWORK}"

    blokli_url="${SYSTEM_TEST_BLOKLI_URL:-$(cat "${network_dir}/blokli-url")}"

    # Discovery checks run only on networks that list the exits they must find
    expected_exits_args=()
    if [ -f "${network_dir}/expected-exits.toml" ]; then
        expected_exits_args=(--expectedExits "${network_dir}/expected-exits.toml")
    fi

    # Name the resolved target up front so a failed run does not need the secrets UI to explain itself
    echo "=== system test target ==="
    echo "  network:   ${SYSTEM_TEST_NETWORK}"
    echo "  blokli:    ${blokli_url}"
    echo "  expected:  ${expected_exits_args[1]:-none}"
    grep -o '^\[destinations\.[^]]*\]' "${network_dir}/config.toml" | sed 's/^/  destination: /' || true
    # Hashing the encrypted keystore leaks nothing but pins which identity ran; a hash matching the other line means the secret ternary fell through
    echo "  identity:  sha256:$(printf %s "${SYSTEM_TEST_HOPRD_ID}" | sha256sum | cut -c1-12)"
    echo "  root:      ${SYSTEM_TEST_ROOT_BINARY}"
    echo "  worker:    ${SYSTEM_TEST_WORKER_BINARY}"
    echo "=========================="

    # Refresh the sudo credential timestamp to avoid password prompt by expiration during long builds
    sudo -v

    worker_user="gnosisvpn"

    worker_home="/var/lib/${worker_user}"
    # The worker's per-user, per-network identity directory
    worker_network_dir="${worker_home}/.local/share/gnosisvpn/networks/${SYSTEM_TEST_NETWORK}"
    state_dir="/var/lib/${worker_user}"
    config_dir="/etc/${worker_user}"
    runtime_dir="/var/run/${worker_user}"
    worker_binary="${worker_home}/gnosis_vpn-worker"
    ctl_binary="${worker_home}/gnosis_vpn-ctl"

    # Create a system user and add it to a group with its own name, if it doesn't already exist
    if ! getent passwd "${worker_user}" >/dev/null 2>&1; then
        echo "INFO: Creating system user '${worker_user}'..."
        sudo useradd --system \
            --user-group \
            --home "${worker_home}" -m \
            "${worker_user}"
        echo "SUCCESS: User '${worker_user}' created successfully"
    else
        echo "INFO: User '${worker_user}' already exists"
    fi

    # Verify that the worker user's home directory can be resolved
    res_worker_home="$(getent passwd "${worker_user}" | cut -d: -f6)"
    if [ -z "${res_worker_home}" ]; then
        echo "Failed to resolve home for user ${worker_user}" >&2
        exit 1
    else
        echo "Resolved home for user ${worker_user}: ${res_worker_home}"
    fi

    # Create worker home directory
    sudo mkdir -p "${worker_network_dir}" "${config_dir}" "${state_dir}" "${runtime_dir}"

    # The country database the installer ships, at the path the network config names
    geoip_tmp="$(mktemp)"
    curl -fsSL -o "${geoip_tmp}" https://download.gnosisvpn.io/geoip/latest.mmdb
    sudo install -D -m 0644 "${geoip_tmp}" /usr/share/gnosisvpn/geoip.mmdb
    rm -f "${geoip_tmp}"

    # Moves the ID and password into the worker's per-network directory
    printf %s "${SYSTEM_TEST_HOPRD_ID}" | sudo tee "${worker_network_dir}/hopr.id" > /dev/null
    printf %s "${SYSTEM_TEST_HOPRD_ID_PASSWORD}" | sudo tee "${worker_network_dir}/hopr.pass" > /dev/null

    # Non-secret per-network data is checked in, so it is copied rather than piped from a secret
    sudo cp "${network_dir}/config.toml" "${config_dir}/config.toml"

    # The worker user runs both, and may not reach the runner's workspace
    sudo cp "${SYSTEM_TEST_WORKER_BINARY}" "${worker_binary}"
    sudo cp "${SYSTEM_TEST_CTL_BINARY}" "${ctl_binary}"

    # Set ownership and permissions for the binaries and config directory
    sudo chown -R "${worker_user}:${worker_user}" "${worker_home}"
    sudo chmod 0755 "${worker_binary}" "${ctl_binary}"

    # Run the test binary with the appropriate environment variables
    sudo CARGO_BIN_EXE_GNOSIS_VPN_ROOT="${SYSTEM_TEST_ROOT_BINARY}" CARGO_BIN_EXE_GNOSIS_VPN_WORKER="${worker_binary}" CARGO_BIN_EXE_GNOSIS_VPN_CTL="${ctl_binary}" SYSTEM_TEST_WORKER_USER="${worker_user}" GNOSISVPN_WORKER_BINARY="${worker_binary}" GNOSISVPN_FORCE_STATIC_ROUTING="true" RUST_LOG="debug" {{ test_binary }} --blokliUrl "${blokli_url}" --proxy "http://10.128.0.1:3128" "${expected_exits_args[@]}"
