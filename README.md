# GnosisVPN Client

The client establishes a VPN connection to a remote endpoint.
It acts as a monitoring/management layer of hoprd session handling and WireGuard setup (Linux only).

## General concept

The client (`gnosis_vpn`) is meant to run as a service binary with privileged access.
We offer a control application (`gnosis_vpn-ctl`) that can be used to manage the client.
The default socket for communicating with the client is `/var/run/gnosisvpn.sock`.
The default configuration file is located in `/etc/gnosisvpn/config.toml`.

The minimal configuration file [config.toml](./config.toml) can be used to ease the setup.
Use [documented-config.toml](./documented-config.toml) as a full reference.

## Installation

Use the [installer](./installer.sh) script to download GnosisVPN and generate an initial config.
Or use this oneliner: `bash -c "$(curl -fsSL https://raw.githubusercontent.com/gnosis/gnosis_vpn-client/HEAD/install.sh)"`.

## General usage

Check available params and env vars via:

`gnosis_vpn --help`

## Usage with automated WireGuard handling

Ensure these requirements are met:

- [WireGuard tools](https://www.wireguard.com/install/) needs to be installed
- an additional TCP/UDP port needs to be accessible on your HOPRD node (this is called the `internal_connection_port` and default to 1422)
- able to run with privileged (sudo) access to handle WireGuard sessions

Prepare the configuration file:

- take the [minimal configuration](./config.toml) file as a starting point
- insert your HOPRD node's API credentials
- set the `internal_connection_port` to the configured port from the requirement

Run the service

- start the client via `sudo ./gnosis_vpn -c ./config.toml`
- see if you spot critical errors or actionable warnings before "enter listening mode"

Instruct the service via the control application `./gnosis_vpn-ctl` from a separate terminal

- check available destinations with `./gnosis_vpn-ctl status`
- connect to a destination of your choice by running `./gnosis_vpn-ctl connect <destination peer id>`

Once a VPN tunnel was created, configure your browsers proxy settings

- Use HTTP proxy at 10.128.0.1:3128 to start browsing with GnosisVPN

## Usage with manual WireGuard handling

Ensure this requirement is met:

- an additional TCP/UDP port needs to be accessible on your HOPRD node (this is called the `internal_connection_port` and default to 1422)

Prepare the configuration file:

- take the [minimal configuration](./config.toml) file as a starting point
- insert your HOPRD node's API credentials
- set the `internal_connection_port` to the configured port from the requirement
- uncomment `[wireguard.manual_mode]` and provide your own WireGuard public key

Run the service and provide a unix communication socket:

- start the client via `./gnosis_vpn -c ./config.toml -s ./gnosis_vpn.sock`
- see if you spot critical errors or actionable warnings before "enter listening mode"

Instruct the service via the control application `./gnosis_vpn-ctl` from a separate terminal

- check available destinations with `./gnosis_vpn-ctl -s ./gnosis_vpn.sock status`
- connect to a destination of your choice by running `./gnosis_vpn-ctl -s ./gnosis_vpn.sock connect <destination peer id>`

Once a HOPRD tunnel was created, configure WireGuard manually

- use config sample instructions printed by the service binary as a starting point
- connect manually to the printed WireGuard endpoint

Once a WireGuard tunnel was created, configure your browsers proxy settings

- Use HTTP proxy at 10.128.0.1:3128 to start browsing with GnosisVPN

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
