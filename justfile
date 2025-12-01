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

system-tests:
    ./scripts/run_system_tests.sh
