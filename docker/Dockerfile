FROM alpine:3.23@sha256:51183f2cfa6320055da30872f211093f9ff1d3cf06f39a0bdb212314c5dc7375

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
