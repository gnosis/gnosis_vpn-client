FROM alpine:3.24@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b

# kernel WireGuard via wg/wg-quick — no wireguard-go, no /dev/net/tun needed.
# no iptables: the killswitch talks netlink/nftables directly (statically linked nftnl).
RUN \
    apk add --no-cache --update wireguard-tools && \
    rm -rf /tmp/*

# gnosis_vpn-root execs the worker under this uid/gid after an NSS lookup
# (see gnosis_vpn-lib/src/worker.rs) — it must be a real user, not just a config value.
RUN adduser -D -h /var/lib/gnosisvpn gnosisvpn

WORKDIR /app

COPY ./gnosis_vpn-root gnosis_vpn-root
COPY ./gnosis_vpn-worker gnosis_vpn-worker
COPY ./entrypoint.sh entrypoint.sh
RUN chmod 755 gnosis_vpn-root gnosis_vpn-worker entrypoint.sh

ENTRYPOINT ["./entrypoint.sh"]
