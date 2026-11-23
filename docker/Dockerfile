FROM alpine:3.24@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf

# kernel WireGuard via wg/wg-quick — no wireguard-go, no /dev/net/tun needed.
# no iptables: the killswitch talks netlink/nftables directly (statically linked nftnl).
RUN \
    apk add --no-cache --update wireguard-tools && \
    rm -rf /tmp/*

# The worker runs as whoever calls `start-client`, so ctl runs as this user (`docker exec -u gnosisvpn`), which needs a home.
RUN adduser -D -h /var/lib/gnosisvpn gnosisvpn

WORKDIR /app

COPY ./gnosis_vpn-root gnosis_vpn-root
COPY ./gnosis_vpn-worker gnosis_vpn-worker
COPY ./gnosis_vpn-ctl gnosis_vpn-ctl
COPY ./entrypoint.sh entrypoint.sh
RUN chmod 755 gnosis_vpn-root gnosis_vpn-worker gnosis_vpn-ctl entrypoint.sh

# ctl talks to root over /run/gnosisvpn/gnosisvpn.sock — put it on PATH for `docker exec ... gnosis_vpn-ctl`
RUN ln -s /app/gnosis_vpn-ctl /usr/local/bin/gnosis_vpn-ctl

ENTRYPOINT ["./entrypoint.sh"]
