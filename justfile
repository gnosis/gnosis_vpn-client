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

# build docker image (ARM64)
docker-build-arm64: build-arm64
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    cp -f result/bin/gnosis_vpn-root result/bin/gnosis_vpn-worker result/bin/gnosis_vpn-ctl docker/
    docker build --platform linux/arm64 -t gnosis_vpn-client:arm64 docker/

# run docker container detached; CONFIG_DIR must hold client.toml (+ identity file if used)
# see gnosis_vpn-testenv's client-start for a full example against a live cluster
docker-run:
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    log_level=$(if [ "${RUST_LOG:-}" = "" ]; then echo info; else echo "${RUST_LOG}"; fi)
    config_dir="${CONFIG_DIR:?CONFIG_DIR must point at a directory containing client.toml}"

    docker run --detach --rm \
        --env RUST_LOG=${log_level} \
        --env GNOSISVPN_CONFIG_PATH=/config/client.toml \
        --env GNOSISVPN_HOPR_BLOKLI_URL=${GNOSISVPN_HOPR_BLOKLI_URL:-} \
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

# run the VPN connectivity smoke test against a live tunnel (pass extra flags after --)
smoke-test *args:
    ./scripts/vpn-smoke-test.sh {{ args }}

# run the offline smoke-test bats suite (no network, uses curl/ping fakes)
smoke-test-check:
    bats scripts/tests/vpn-smoke-test.bats

system-tests test_binary="gnosis_vpn-system_tests":
    #!/usr/bin/env bash
    set -euo pipefail

    : "${SYSTEM_TEST_HOPRD_ID:?SYSTEM_TEST_HOPRD_ID must be set to run system tests}"
    : "${SYSTEM_TEST_HOPRD_ID_PASSWORD:?SYSTEM_TEST_HOPRD_ID_PASSWORD must be set to run system tests}"
    : "${SYSTEM_TEST_SAFE:?SYSTEM_TEST_SAFE must be set to run system tests}"
    : "${SYSTEM_TEST_CONFIG:?SYSTEM_TEST_CONFIG must be set to run system tests}"
    : "${SYSTEM_TEST_WORKER_BINARY:?SYSTEM_TEST_WORKER_BINARY must be set to run system tests}"
    : "${SYSTEM_TEST_ROOT_BINARY:?SYSTEM_TEST_ROOT_BINARY must be set to run system tests}"

    # Refresh the sudo credential timestamp to avoid password prompt by expiration during long builds
    sudo -v

    worker_user="gnosisvpn"

    worker_home="/var/lib/${worker_user}"
    worker_config_dir="${worker_home}/.config"
    state_dir="/var/lib/${worker_user}"
    config_dir="/etc/${worker_user}"
    runtime_dir="/var/run/${worker_user}"
    worker_binary="${worker_home}/gnosis_vpn-worker"

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
    sudo mkdir -p "${worker_config_dir}" "${config_dir}" "${state_dir}" "${runtime_dir}"

    # Moves the ID, password, safe, and config into the worker's config directory
    printf %s "${SYSTEM_TEST_HOPRD_ID}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.id" > /dev/null
    printf %s "${SYSTEM_TEST_HOPRD_ID_PASSWORD}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.pass" > /dev/null
    printf %s "${SYSTEM_TEST_SAFE}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.safe" > /dev/null
    printf %s "${SYSTEM_TEST_CONFIG}" | sudo tee "${config_dir}/config.toml" > /dev/null

    # Copy the worker binary to the worker's home directory
    sudo cp "${SYSTEM_TEST_WORKER_BINARY}" "${worker_home}"

    # Set ownership and permissions for the worker binary and config directory
    sudo chown -R "${worker_user}:${worker_user}" "${worker_home}"
    sudo chmod 0755 "${worker_binary}"

    # Run the test binary with the appropriate environment variables
    sudo CARGO_BIN_EXE_GNOSIS_VPN_ROOT="${SYSTEM_TEST_ROOT_BINARY}" CARGO_BIN_EXE_GNOSIS_VPN_WORKER="${worker_binary}" GNOSISVPN_HOME="${worker_home}" GNOSISVPN_WORKER_USER="${worker_user}" GNOSISVPN_WORKER_BINARY="${worker_binary}" GNOSISVPN_FORCE_STATIC_ROUTING="true" RUST_LOG="debug" {{ test_binary }} --proxy "http://10.128.0.1:3128"

# run the bats test suite for scripts/
test-scripts:
    bats --print-output-on-failure scripts/tests
