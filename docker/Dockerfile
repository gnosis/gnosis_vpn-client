FROM alpine:3.23@sha256:865b95f46d98cf867a156fe4a135ad3fe50d2056aa3f25ed31662dff6da4eb62

# enable wireguard
RUN \
    apk add --no-cache --update \
        curl \
        iptables \
        wireguard-tools && \
    # copied from https://github.com/linuxserver/docker-wireguard/blob/master/Dockerfile#L41C3-L41C246
    sed -i 's|\[\[ $proto == -4 \]\] && cmd sysctl -q net\.ipv4\.conf\.all\.src_valid_mark=1|[[ $proto == -4 ]] \&\& [[ $(sysctl -n net.ipv4.conf.all.src_valid_mark) != 1 ]] \&\& cmd sysctl -q net.ipv4.conf.all.src_valid_mark=1|' /usr/bin/wg-quick && \
    rm -rf /tmp/*

WORKDIR /app

# add local files
COPY ./config.toml config.toml
COPY ./gnosis_vpn gnosis_vpn
COPY ./gnosis_vpn-ctl gnosis_vpn-ctl
COPY ./wrapper.sh wrapper.sh

# port handling
EXPOSE 51822/udp

CMD ["./wrapper.sh"]
