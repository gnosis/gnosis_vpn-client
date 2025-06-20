# Gnosis VPN Client

Gnosis VPN is a VPN solution build on top the HOPR mixnet.
It manages VPN connections to remote targets.

## General concept

The client offers two binaries, `gnosis_vpn` and `gnosis_vpn-ctl`.
The client (`gnosis_vpn`) runs as a system service with privileged access.
The control application (`gnosis_vpn-ctl`) is used to manage the client and its connections.

## Installation

Follow the [onboarding instructions](./ONBOARDING.md) to set up your HOPR node and install the Gnosis VPN client.

### Check signatures

To validate the signature of the downloaded binary from GitHub, follow these steps:

1. Import the public key:

   ```bash
   gpg --import gpg-publickey.asc
   ```

2. Verify the binary signature:

   ```bash
   gpg --verify gnosis_vpn-aarch64-darwin.sig gnosis_vpn-aarch64-darwin
   ```

3. Verify the SHA256 checksum signature:

   ```bash
   gpg --verify gnosis_vpn-aarch64-darwin.sha256.asc
   ```

4. Compare the decrypted checksum with the actual checksum:

   ```bash
   diff -u <(gpg --decrypt gnosis_vpn-aarch64-darwin.sha256.asc) <(shasum -a 256 gnosis_vpn-aarch64-darwin)
   ```

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
