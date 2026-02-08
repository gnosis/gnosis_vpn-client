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
> sudo RUST_LOG="debug" GNOSISVPN_HOME=/home/gnosisvpn ./target/debug/gnosis_vpn-root -c <config.toml> \
    --hopr-blokli-url <hopr blokli url> --worker-binary /home/gnosisvpn/gnosis_vpn-worker
# or from nix build:
> sudo RUST_LOG="debug" GNOSISVPN_HOME=/home/gnosisvpn ./result/bin/gnosis_vpn-root -c <config.toml> \
    --hopr-blokli-url <hopr blokli url> --worker-binary /home/gnosisvpn/gnosis_vpn-worker
```

### Worker user configuration

There are three environment variables that control the worker process setup:

- `GNOSISVPN_HOME`: The home directory for the service. This is where state and
  caching data will be stored. Defaults to `/var/lib/gnosisvpn` on Linux and
  `/Libary/Application Support/gnosisvpn` on macOS.

- `GNOSISVPN_WORKER_USER`: The user with limited privileges that will run the
  worker process. This user needs to have read and execute permissions for the
  worker binary and write permissions for the `GNOSISVPN_HOME` directory.
  Defaults to `gnosisvpn`.

- `GNOSISVPN_WORKER_BINARY`: The path to the worker binary. The worker process
  will be spawned with this binary.

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

2. Verify the binary signature (examples for x86_64 and ARM64):

   ```bash
   # For x86_64 (AMD64)
   gpg --verify gnosis_vpn-root-x86_64-linux.asc gnosis_vpn-root-x86_64-linux
   
   # For ARM64
   gpg --verify gnosis_vpn-root-aarch64-linux.asc gnosis_vpn-root-aarch64-linux
   ```

3. Compare the checksum with the actual checksum:

   ```bash
   # For x86_64 (AMD64)
   diff -u <(cat gnosis_vpn-root-x86_64-linux.sha256) <(shasum -a 256 gnosis_vpn-root-x86_64-linux)
   
   # For ARM64
   diff -u <(cat gnosis_vpn-root-aarch64-linux.sha256) <(shasum -a 256 gnosis_vpn-root-aarch64-linux)
   ```

## General usage

Check available params and env vars via:

`gnosis_vpn-root --help` `gnosis_vpn-ctl --help`

## Deployment

Show potential deployment targets:

`nix flake show`

Build for a target, e.g. `x86_64-linux` or `aarch64-linux`:

```bash
# For x86_64 (AMD64)
nix build .#packages.x86_64-linux.gnosis_vpn

# For ARM64
nix build .#packages.aarch64-linux.gnosis_vpn
```

The resulting binaries are in `result/bin/`:

```
$ ls -l result*/bin/
result/bin/:
total 4752
-r-xr-xr-x 1 root root  4863367 Jan  1  1970 gnosis_vpn-root
-r-xr-xr-x 1 root root 14863367 Jan  1  1970 gnosis_vpn-worker
-r-xr-xr-x 1 root root  1740052 Jan  1  1970 gnosis_vpn-ctl
```
