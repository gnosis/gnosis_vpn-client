# Gnosis VPN Client

Gnosis VPN is a VPN solution build on top the HOPR mixnet.
It manages VPN connections to remote targets.

## General concept

The client offers two binaries, `gnosis_vpn` and `gnosis_vpn-ctl`.
The client (`gnosis_vpn`) runs as a system service with privileged access.
The control application (`gnosis_vpn-ctl`) is used to manage the client and its connections.

## Installation

Follow the [onboarding instructions](./ONBOARDING.md) to set up your HOPR node and install the Gnosis VPN client.

## General usage

Check available params and env vars via:

`gnosis_vpn --help`
`gnosis_vpn-ctl --help`

## Deployment

Show potential deployment targets:

`nix flake show`

Build for a target, e.g. `x86_64-linux`:

`nix build .#gnosisvpn-x86_64-linux`

The resulting binaries are in `result/bin/`:

```
$ ls -l result*/bin/
result/bin/:
total 4752
-r-xr-xr-x 1 root root 4863368 Jan  1  1970 gnosis_vpn
-r-xr-xr-x 1 root root 1740050 Jan  1  1970 gnosis_vpn-ctl
```
