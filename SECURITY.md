# Security Policy

## Linux Binary Verification

All GnosisVPN client binaries include SHA256 checksums for integrity verification. Additionally:

- **Linux binaries** are signed with GPG
- **macOS binaries** use Apple's code signing mechanism and are signed with an Apple Developer certificate

We strongly recommend verifying binaries before installation.

### GPG Public Key

**Key ID:** `84F73FEA46D10972`

**Fingerprint:** `9A30 8031 FD3B FE8E DBF5  076D 84F7 3FEA 46D1 0972`

**Email:** tech@hoprnet.org

### Importing the Public Key

You can import the GnosisVPN public key using any of these methods:

**From keyserver:**

```bash
gpg --keyserver keyserver.ubuntu.com --recv-keys 9A308031FD3BFE8EDBF5076D84F73FEA46D10972
echo "9A308031FD3BFE8EDBF5076D84F73FEA46D10972:6:" | gpg --import-ownertrust
```

**From this repository:**

```bash
curl -s -O https://raw.githubusercontent.com/gnosis/gnosis_vpn-client/main/gnosisvpn-public-key.asc
gpg --import gnosisvpn-public-key.asc
```

**From release assets:**

Download `gnosisvpn-public-key.asc` from any release and import:

```bash
gpg --import gnosisvpn-public-key.asc
```

### Verifying Binary Signatures

Each Linux release includes three files per binary (`gnosis_vpn-ctl`, `gnosis_vpn-worker`, `gnosis_vpn-root`):

1. **Binary file** (e.g., `gnosis_vpn-ctl-x86_64-linux`)
2. **SHA256 checksum** (e.g., `gnosis_vpn-ctl-x86_64-linux.sha256`)
3. **GPG signature** (e.g., `gnosis_vpn-ctl-x86_64-linux.asc`)

#### Verify SHA256 Checksum

```bash
sha256sum -c gnosis_vpn-ctl-x86_64-linux.sha256
```

Expected output:

```
gnosis_vpn-ctl-x86_64-linux: OK
```

#### Verify GPG Signature

```bash
gpg --verify gnosis_vpn-ctl-x86_64-linux.asc gnosis_vpn-ctl-x86_64-linux
```

Expected output:

```
gpg: Signature made Mon May  4 12:25:22 2026 CEST
gpg:                using EDDSA key 9A308031FD3BFE8EDBF5076D84F73FEA46D10972
gpg: checking the trustdb
gpg: marginals needed: 3  completes needed: 1  trust model: pgp
gpg: depth: 0  valid:   1  signed:   0  trust: 0-, 0q, 0n, 0m, 0f, 1u
gpg: next trustdb check due at 2075-11-23
gpg: Good signature from "GnosisVPN (Gnosis VPN) <tech@hoprnet.org>" [ultimate]
```

## macOS Binary Verification

macOS binaries are signed with an Apple Developer certificate and notarized by Apple. The system verifies signatures
automatically during installation.

### Verify SHA256 Checksum (macOS)

Each macOS release includes a SHA256 checksum file for manual verification:

Download the binary and checksum from the release page https://github.com/gnosis/gnosis_vpn-client/releases


```bash
# Verify checksum
shasum -a 256 -c gnosis_vpn-ctl-aarch64-darwin.sha256
```

Expected output:

```
gnosis_vpn-ctl-aarch64-darwin: OK
```

## Reporting Security Vulnerabilities

If you discover a security vulnerability in GnosisVPN, please report it privately to:

**Email:** tech@hoprnet.org

Please include:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)
