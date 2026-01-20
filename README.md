# Gnosis VPN Client

Gnosis VPN is a VPN solution build on top the HOPR mixnet. This repo contains
the system service and a control application. It is part of a larger project
consisting of:

- [Gnosis VPN App](https://github.com/gnosis/gnosis_vpn-app) handling user
  interface
- [Gnosis VPN Server](https://github.com/gnosis/gnosis_vpn-server) handling VPN
  server side

## General concept

The client offers three binaries, `gnosis_vpn-root`, `gnosis_vpn-worker` and
`gnosis_vpn-ctl`. The client system service (`gnosis_vpn-root`) runs with root
privileges and takes care of routing setup. It spawn the worker process
(`gnosis_vpn-worker`) which is responsible for the application logic. The
control application (`gnosis_vpn-ctl`) is used to manage the client and its
connections.

## Development setup

1. Create an extra user on your system (e.g. `gnosisvpn`) with normal
   privileges. This user will be used to run the worker process.

2. Build binaries via: `cargo build` or `nix build .#gnosis_vpn-dev`.

3. Copy worker binary into user's home directory and make it owned by the user:

```bash
> sudo cp target/debug/gnosis_vpn-worker /home/gnosisvpn/
# or from nix build:
# sudo cp result/bin/gnosis_vpn-worker /home/gnosisvpn/
> sudo chown gnosisvpn:gnosisvpn /home/gnosisvpn/gnosis_vpn-worker
```

4. Run the root binary with sudo and provide the path to the worker binary:

```bash
> sudo RUST_LOG="debug" ./target/debug/gnosis_vpn-root -c <config.toml> --hopr-blokli-url <hopr blokli url> --worker-binary /home/gnosisvpn/gnosis_vpn-worker
# or from nix build:
> sudo RUST_LOG="debug" ./result/bin/gnosis_vpn-root -c <config.toml> --hopr-blokli-url <hopr blokli url> --worker-binary /home/gnosisvpn/gnosis_vpn-worker
```

## Installation

Use the latest
[installer](https://github.com/gnosis/gnosis_vpn/releases/latest).

### Check signatures

To validate the signature of the downloaded binary from GitHub, follow these
steps:

1. Import the public key (checkout the repository first):

   ```bash
   gpg --import gpg-publickey.asc
   ```

2. Verify the binary signature:

   ```bash
   gpg --verify gnosis_vpn-root-x86_64-linux.asc gnosis_vpn-root-x86_64-linux
   ```

3. Compare the checksum with the actual checksum:

   ```bash
   diff -u <(cat gnosis_vpn-root-x86_64-linux.sha256) <(shasum -a 256 gnosis_vpn-root-x86_64-linux)
   ```

## General usage

Check available params and env vars via:

`gnosis_vpn-root --help` `gnosis_vpn-ctl --help`

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
-r-xr-xr-x 1 root root  4863367 Jan  1  1970 gnosis_vpn-root
-r-xr-xr-x 1 root root 14863367 Jan  1  1970 gnosis_vpn-worker
-r-xr-xr-x 1 root root  1740050 Jan  1  1970 gnosis_vpn-ctl
```
