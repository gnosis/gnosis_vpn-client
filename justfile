# build static linux binary
build:
    nix build .#gnosisvpn-x86_64-linux

# build docker image
docker-build: build
    cp result/bin/* docker/
    chmod 775 docker/gnosis_vpn docker/gnosis_vpn-ctl
    docker build --platform linux/x86_64 -t gnosis_vpn-client docker/

# run docker container detached
docker-run ip_address='' private_key='' server_public_key='':
    #!/usr/bin/env bash
    set -o errexit -o nounset -o pipefail

    ADDRESS=$(if ["{{ ip_address }}" = ""]; then echo "need ip param"; exit 1; else echo "{{ ip_address }}"; fi)
    PRIVATE_KEY=$(if [ "{{ private_key }}" = "" ]; then wg genkey; else echo "{{ private_key }}"; fi)
    SERVER_PUBLIC_KEY=$(if ["{{ server_public_key }}" = ""]; then wg genkey | wg pubkey; else echo "{{ server_public_key }}"; fi)

    docker run --rm --detach \
        --env ADDRESS=$ADDRESS \
        --env PRIVATE_KEY=$PRIVATE_KEY \
        --env SERVER_PUBLIC_KEY=$SERVER_PUBLIC_KEY
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
