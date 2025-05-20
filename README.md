# GnosisVPN Client

The client establishes a VPN connection to a remote endpoint.
It acts as a monitoring/management layer of hoprd session handling and WireGuard setup (Linux only).

## General concept

The client (`gnosis_vpn`) is meant to run as a service binary with privileged access.
We offer a control application (`gnosis_vpn-ctl`) that can be used to manage the client.
The default socket for communicating with the client is `/var/run/gnosisvpn.sock`.
The default configuration file is located in `/etc/gnosisvpn/config.toml`.

A minimal configuration file is [config.toml](./config.toml).
Use [documented-config.toml](./documented-config.toml) as a full reference.

### Env vars

`GNOSISVPN_CONFIG_PATH` - path to the configuration file
`GNOSISVPN_SOCKET_PATH` - path to the control socket

## Usage

Take the [minimal configuration](./config.toml) file as a starting point.
At the minimum add your HOPD node's API credentials.
Ensure you have a forwardable TCP/UDP port configured that matches `internal_connection_port`.

If you run on MacOS, uncomment `[wireguard.manual_mode]` and provide your own WireGuard public key.

- start the client via `./gnosis_vpn -c ./config.toml` on a separate terminal or run it as a service
- you can instruct the client via the control application `./gnosis_vpn-ctl`, run `./gnosis_vpn-ctl --help` for a list of commands
- check available destinations with `./gnosis_vpn-ctl status`
- connect to a destination of your choice by running `./gnosis_vpn-ctl connect <destination peer id>`
- in manual mode, follow the instructions to connect your WireGuard session, otherwise wait for the connection to establish
- configure your browsers proxy to use an HTTP proxy at 10.128.0.1:3128

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
-r-xr-xr-x 1 root root 1740048 Jan  1  1970 gnosis_vpn-ctl
```
