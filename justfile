# build static linux binary
build:
    nix build .#gnosisvpn-x86_64-linux

# build docker image
docker-build: build
    cp result/bin/* docker/
    chmod 775 docker/gnosis_vpn docker/gnosis_vpn-ctl
    docker build --platform linux/x86_64 -t gnosis_vpn-client docker/

# run docker container detached
docker-run:
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    PRIVATE_KEY=$(if [ "{{ PRIVATE_KEY }}" = "" ]; then wg genkey; else echo "{{ PRIVATE_KEY }}"; fi)
    SERVER_PUBLIC_KEY=$(if ["{{ SERVER_PUBLIC_KEY }}" = ""]; then wg genkey | wg pubkey; else echo "{{ SERVER_PUBLIC_KEY }}"; fi)

    docker run --rm --detach \
        --env ADDRESS={{ ADDRESS }} \
        --env PRIVATE_KEY=$PRIVATE_KEY \
        --env SERVER_PUBLIC_KEY=$SERVER_PUBLIC_KEY \
        --env DESTINATION_PEER_ID={{ DESTINATION_PEER_ID }} \
        --env API_PORT={{ API_PORT }} \
        --env API_TOKEN={{ API_TOKEN }} \
        --publish 51822:51820/udp \
        --cap-add=NET_ADMIN \
        --add-host=host.docker.internal:host-gateway \
        --name gnosis_vpn-client gnosis_vpn-client

# stop docker container
docker-stop:
    docker stop gnosis_vpn-client

# enter docker container interactively
docker-enter:
    docker exec --interactive --tty gnosis_vpn-client bash
