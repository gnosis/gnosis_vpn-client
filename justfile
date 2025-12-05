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

system-tests test_binary="gnosis_vpn-system-tests" network="rotsee":
    #!/usr/bin/env bash
    set -euo pipefail

    : "${SYSTEM_TEST_HOPRD_ID:?SYSTEM_TEST_HOPRD_ID must be set to run system tests}"
    : "${SYSTEM_TEST_HOPRD_ID_PASSWORD:?SYSTEM_TEST_HOPRD_ID_PASSWORD must be set to run system tests}"
    : "${SYSTEM_TEST_SAFE:?SYSTEM_TEST_SAFE must be set to run system tests}"
    : "${SYSTEM_TEST_CONFIG:?SYSTEM_TEST_CONFIG must be set to run system tests}"
    
    config_dir="${CONFIG_DIR:-/etc/gnosisvpn}"
    cache_dir="${XDG_CONFIG_HOME}/gnosisvpn"

    sudo mkdir -p "${config_dir}"
    sudo mkdir -p "${cache_dir}"

    printf %s "${SYSTEM_TEST_HOPRD_ID}" | sudo tee "${cache_dir}/gnosisvpn-hopr.id" > /dev/null
    printf %s "${SYSTEM_TEST_HOPRD_ID_PASSWORD}" | sudo tee "${cache_dir}/gnosisvpn-hopr.pass" > /dev/null
    printf %s "${SYSTEM_TEST_SAFE}" | sudo tee "${cache_dir}/gnosisvpn-hopr.safe" > /dev/null
    printf %s "${SYSTEM_TEST_CONFIG}" | sudo tee "${config_dir}/config.toml" > /dev/null

    set RUST_LOG=none,{{replace(test_binary, "-", "_")}}=info && sudo {{ test_binary }} download
