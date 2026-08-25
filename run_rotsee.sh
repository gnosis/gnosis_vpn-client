 sudo rm -f /Users/gnosisvpn-dev/gnosis_vpn-worker;
 sudo cp ./target/release/gnosis_vpn-worker /Users/gnosisvpn-dev/;
 sudo chown gnosisvpn-dev:gnosisvpn-dev /Users/gnosisvpn-dev/gnosis_vpn-worker;

 sudo rm /tmp/rotsee.log

 sudo RUST_LOG="debug,hopr_strategy=debug,gnosis_vpn_root=debug,gnosis_vpn_lib=debug,hopr_network_graph=debug,hopr_transport_probe=debug,hopr_transport::path=trace" HOPR_SESSION_MAX_BUFFERED_SEGMENTS=2 GNOSISVPN_HOME=/Users/gnosisvpn-dev  RUST_BACKTRACE=full SOCKET_PATH=/var/run/gnosisvpn.sock ./target/release/gnosis_vpn-root -c /Users/gnosisvpn-dev/rotsee.toml --hopr-blokli-url https://blokli.rotsee.hoprnet.link --worker-binary /Users/gnosisvpn-dev/gnosis_vpn-worker --worker-user gnosisvpn-dev --allow-insecure --client-autostart 30min --log-file /tmp/rotsee.log --hopr-identity-file /Users/gnosisvpn-dev/.config/gnosisvpn-hopr.id --hopr-identity-pass sarPJdAdvKd2a9G3Jzgp1UkKBN1Td2MlBZ2KHK2oUeAakpsw
