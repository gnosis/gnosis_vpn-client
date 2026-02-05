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
    worker_group="gnosisvpn"

    if ! getent group "${worker_group}" >/dev/null 2>&1; then
        echo "INFO: Creating group '${worker_group}'..."
        sudo groupadd --system "${worker_group}"
        echo "SUCCESS: Group '${worker_group}' created successfully"
    else
        echo "INFO: Group '${worker_group}' already exists"
    fi

    if ! getent passwd "${worker_user}" >/dev/null 2>&1; then
        echo "INFO: Creating system user '${worker_user}'..."
        sudo useradd --system \
            --gid "${worker_group}" \
            --home-dir "/var/lib/${worker_user}" \
            --shell /usr/sbin/nologin \
            --comment "Gnosis VPN Service User" \
            "${worker_user}"
        echo "SUCCESS: User '${worker_user}' created successfully"
    else
        echo "INFO: User '${worker_user}' already exists"
    fi


    worker_home="$(getent passwd "${worker_user}" | cut -d: -f6)"
    if [ -z "${worker_home}" ]; then
        echo "Failed to resolve home for user ${worker_user}" >&2
        exit 1
    else
        echo "Resolved home for user ${worker_user}: ${worker_home}"
    fi

    worker_home="${worker_home:-/var/lib/${worker_user}}"
    worker_dst="${worker_home}/gnosis_vpn-worker"
    worker_config_dir="${worker_home}/.config"
    worker_cache_dir="${worker_home}/.cache"

    sudo mkdir -p "${worker_home}" "${worker_config_dir}" "${worker_cache_dir}"
    sudo mkdir -p "/etc/${worker_user}"
    sudo chown -R "${worker_user}:${worker_group}" "${worker_home}"
    
    printf %s "${SYSTEM_TEST_HOPRD_ID}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.id" > /dev/null
    printf %s "${SYSTEM_TEST_HOPRD_ID_PASSWORD}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.pass" > /dev/null
    printf %s "${SYSTEM_TEST_SAFE}" | sudo tee "${worker_config_dir}/gnosisvpn-hopr.safe" > /dev/null
    printf %s "${SYSTEM_TEST_CONFIG}" | sudo tee "/etc/${worker_user}/config.toml" > /dev/null

    sudo cp "${SYSTEM_TEST_WORKER_BINARY}" "${worker_dst}"
    sudo chown "${worker_user}:${worker_group}" "${worker_dst}"
    sudo chmod 0755 "${worker_dst}"

    echo "worker binary permissions: $(ls -l "${worker_dst}")"

    sudo CARGO_BIN_EXE_GNOSIS_VPN_WORKER="${worker_dst}" RUST_LOG="debug" {{ test_binary }} download
