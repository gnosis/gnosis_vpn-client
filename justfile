# build static linux binary
build:
    nix build .#packages.x86_64-linux.gvpn

# build docker image
docker-build: build
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    cp result/bin/* docker/
    chmod 775 docker/gnosis_vpn docker/gnosis_vpn-ctl
    docker build --platform linux/x86_64 -t gnosis_vpn-client docker/

# run docker container detached
docker-run:
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    log_level=$(if [ "${RUST_LOG:-}" = "" ]; then echo info; else echo "${RUST_LOG}"; fi)

    docker run --detach --rm \
        --env DESTINATION_ADDRESS_1=${DESTINATION_ADDRESS_1} \
        --env DESTINATION_ADDRESS_2=${DESTINATION_ADDRESS_2} \
        --env API_PORT=${API_PORT} \
        --env API_TOKEN=${API_TOKEN} \
        --env RUST_LOG=${log_level} \
        --publish 51822:51820/udp \
        --cap-add=NET_ADMIN \
        --add-host=host.docker.internal:host-gateway \
        --sysctl net.ipv4.conf.all.src_valid_mark=1 \
        --name gnosis_vpn-client gnosis_vpn-client

# stop docker container
docker-stop:
    docker stop gnosis_vpn-client

# enter docker container interactively
docker-enter:
    docker exec --interactive --tty gnosis_vpn-client bash

system-tests test_binary="gnosis_vpn-system_tests":
    #!/usr/bin/env bash
    set -euo pipefail

    : "${SYSTEM_TEST_HOPRD_ID:?SYSTEM_TEST_HOPRD_ID must be set to run system tests}"
    : "${SYSTEM_TEST_HOPRD_ID_PASSWORD:?SYSTEM_TEST_HOPRD_ID_PASSWORD must be set to run system tests}"
    : "${SYSTEM_TEST_SAFE:?SYSTEM_TEST_SAFE must be set to run system tests}"
    : "${SYSTEM_TEST_CONFIG:?SYSTEM_TEST_CONFIG must be set to run system tests}"
    : "${SYSTEM_TEST_WORKER_BINARY:?SYSTEM_TEST_WORKER_BINARY must be set to run system tests}"

    worker_user="gnosisvpn"

    worker_home="/var/lib/${worker_user}"
    worker_binary="${worker_home}/gnosis_vpn-worker"
    worker_config_dir="/etc/${worker_user}"
    state_dir="/var/lib/${worker_user}"
    runtime_dir="/var/run/${worker_user}"

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
    sudo mkdir -p "${worker_config_dir}" "${state_dir}" "${runtime_dir}"
    
    # Moves the ID, password, safe, and config into the worker's config directory 
    printf %s "${SYSTEM_TEST_HOPRD_ID}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.id" > /dev/null
    printf %s "${SYSTEM_TEST_HOPRD_ID_PASSWORD}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.pass" > /dev/null
    printf %s "${SYSTEM_TEST_SAFE}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.safe" > /dev/null
    printf %s "${SYSTEM_TEST_CONFIG}" | sudo tee "${worker_config_dir}/config.toml" > /dev/null

    # Copy the worker binary to the worker's home directory
    sudo cp "${SYSTEM_TEST_WORKER_BINARY}" "${worker_home}"

    # Set ownership and permissions for the worker binary and config directory
    sudo chown -R "${worker_user}:${worker_user}" "${worker_home}"
    sudo chmod 0755 "${worker_binary}"

    echo "worker binary permissions: $(sudo ls -l "${worker_binary}")"

    # Run the test binary with the appropriate environment variables
    sudo CARGO_BIN_EXE_GNOSIS_VPN_WORKER="${worker_binary}" GNOSISVPN_HOME="${worker_home}" GNOSISVPN_WORKER_USER="${worker_user}" GNOSISVPN_WORKER_BINARY="${worker_binary}" RUST_LOG="debug" {{ test_binary }} download