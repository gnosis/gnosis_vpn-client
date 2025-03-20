FROM alpine:3.21

# enable wireguard
RUN \
    apk add --no-cache --update wireguard-tools && \
    rm -rf /tmp/*

WORKDIR /app

# add local files
COPY ./gnosis_vpn gnosis_vpn
COPY ./gnosis_vpn-ctl gnosis_vpn-ctl
COPY ./wgclient.conf wgclient.conf
COPY ./monitor.sh monitor.sh

# port handling
EXPOSE 51820/udp

WORKDIR /app

CMD ["./monitor.sh"]
