# Gnosis VPN macOS PKG Installer

This directory contains the macOS PKG installer implementation for Gnosis VPN Client. The installer provides a user-friendly graphical interface for installing and configuring the Gnosis VPN client on macOS systems.

## Features

- **Custom UI**: Professional welcome, readme, and completion screens with branding
- **System Requirements Check**: Validates macOS version, architecture, and disk space
- **Automatic Downloads**: Fetches the latest binaries from GitHub releases
- **WireGuard Integration**: Automatically detects and installs WireGuard tools if needed
- **Network Selection**: Choose between Production (Gnosis Chain) or Rotsee testnet
- **Configuration Generation**: Creates `config.toml` with selected network destinations
- **macOS Integration**: Removes quarantine attributes and sets proper permissions

## Directory Structure

```
installer/
├── build/                      # Build output directory (generated)
├── resources/                  # Installer resources
│   ├── welcome.html           # Welcome screen
│   ├── readme.html            # Requirements and info screen
│   ├── conclusion.html        # Completion screen with instructions
│   └── scripts/
│       ├── installationCheck.js  # Pre-flight system checks
│       ├── preinstall            # Downloads binaries and verifies WireGuard
│       └── postinstall           # Generates configuration
├── Distribution.xml           # Installer flow and UI configuration
├── build-pkg.sh              # Build script
├── sign-pkg.sh               # Signing and notarization script
└── README.md                 # This file
```

## Building the Installer

### Prerequisites

- macOS 11.0 or later
- Xcode Command Line Tools installed:
  ```bash
  xcode-select --install
  ```
- (Optional) Apple Developer ID certificate for signing

### Build Steps

1. **Build the unsigned installer:**
   ```bash
   cd installer
   ./build-pkg.sh latest
   ```

   This downloads the latest binaries from GitHub, creates universal binaries (x86_64 + arm64), and packages them into `build/GnosisVPN-Installer-<version>.pkg`
   
   You can also specify a specific version:
   ```bash
   ./build-pkg.sh v0.12.0
   ```

2. **Test the installer:**
   ```bash
   open build/GnosisVPN-Installer-1.0.0.pkg
   ```

3. **(Optional) Sign the installer for distribution:**
   ```bash
   export SIGNING_IDENTITY="Developer ID Installer: Your Name (TEAM_ID)"
   ./sign-pkg.sh build/GnosisVPN-Installer-1.0.0.pkg
   ```

4. **(Optional) Notarize for Gatekeeper:**
   ```bash
   export APPLE_ID="your@email.com"
   export TEAM_ID="ABC123XYZ"
   export KEYCHAIN_PROFILE="AC_PASSWORD"
   ./sign-pkg.sh build/GnosisVPN-Installer-1.0.0-signed.pkg --notarize
   ```

## What the Installer Does

### Build-Time Phase (NEW)

**The build script now downloads and packages binaries at build time, not during installation:**

1. **Binary Download & Packaging** (build-pkg.sh):
   - Fetches latest version tag from GitHub (or uses specified version)
   - Downloads both x86_64 and aarch64 binaries for `gnosis_vpn` and `gnosis_vpn-ctl`
   - Creates universal binaries using `lipo` (supports both Intel and Apple Silicon)
   - Packages binaries into the PKG payload

**Benefits:**
- ✅ Installation progress is visible in macOS Installer UI
- ✅ Faster installations (no network downloads during install)
- ✅ Works offline
- ✅ More reliable (no network failure points during install)

### Pre-Installation Phase

1. **System Checks** (installationCheck.js):
   - Validates macOS version (requires 11.0+)
   - Checks system architecture (Intel or Apple Silicon)
   - Verifies available disk space (minimum 50MB)

2. **Pre-Install Script** (preinstall):
   - Minimal checks only
   - Warns if WireGuard tools are not installed (non-blocking)

### Installation Phase

- Installs universal binaries to `/usr/local/bin/`
- Creates `/etc/gnosisvpn/` directory

### Post-Installation Phase

1. **Post-Install Script** (postinstall):
   - Backs up existing `config.toml` if present
   - Generates new configuration based on network selection (rotsee or dufour)
   - Sets proper file permissions
   - Verifies installation integrity

## Configuration

### Network Selection

During installation, users can choose between:

- **Rotsee Network** - Default (Recommended)
  - USA (Iowa)
  - UK (London)

- **Dufour Network**
  - Germany
  - USA
  - Spain
  - India

### Environment Variables

The installer scripts support these environment variables:

- `INSTALLER_CHOICE_NETWORK`: Network selection ("rotsee" or "dufour", default: "rotsee")

### Installation Locations

After installation, files are located at:

- **Binaries**: `/usr/local/bin/`
  - `gnosis_vpn` - Main VPN daemon
  - `gnosis_vpn-ctl` - Control utility
- **Application**: `/Applications/GnosisVPN.app`
- **Configuration**: `/etc/gnosisvpn/config.toml`

## Customization

### Modifying UI Content

Edit the HTML files in `resources/`:
- `welcome.html` - Introduction and requirements
- `readme.html` - Detailed information and checks
- `conclusion.html` - Post-installation instructions

### Changing Installation Logic

- **System checks**: Edit `resources/scripts/installationCheck.js`
- **Download/setup**: Edit `resources/scripts/preinstall`
- **Configuration**: Edit `resources/scripts/postinstall`

### Modifying Installer Flow

Edit `Distribution.xml` to change:
- Installation choices
- UI panels
- Package metadata
- Localization

## Distribution

### For Testing

Share the unsigned `.pkg` file directly. Users may need to right-click and select "Open" to bypass Gatekeeper warnings.

### For Production

1. **Code Sign** the package with Developer ID certificate
2. **Notarize** with Apple to avoid Gatekeeper warnings
3. **Staple** the notarization ticket for offline verification
4. Upload to GitHub releases with SHA256 checksum

Example GitHub release:

```markdown
## Installation

Download: [GnosisVPN-Installer-1.0.0-signed.pkg](...)

SHA256: `abc123...`

Verify checksum:
\`\`\`bash
shasum -a 256 GnosisVPN-Installer-1.0.0-signed.pkg
\`\`\`
```

## Troubleshooting

### Build Issues

**Error: "productsign: command not found"**
- Install Xcode Command Line Tools: `xcode-select --install`

**Error: "Distribution.xml not found"**
- Ensure you're running the script from the `installer/` directory

### Signing Issues

**Error: "No Developer ID Installer certificate found"**
- Download and install your certificate from https://developer.apple.com
- Double-click the `.cer` file to add it to Keychain Access

**Error: "productsign failed"**
- Verify your signing identity name:
  ```bash
  security find-identity -v -p basic | grep "Developer ID Installer"
  ```
- Set the exact identity name:
  ```bash
  export SIGNING_IDENTITY="Developer ID Installer: Your Name (TEAM123)"
  ```

### Notarization Issues

**Error: "APPLE_ID environment variable not set"**
- Set required environment variables:
  ```bash
  export APPLE_ID="your@email.com"
  export TEAM_ID="ABC123XYZ"
  ```

**Error: "No keychain profile found"**
- Create an app-specific password at https://appleid.apple.com
- Store it in keychain:
  ```bash
  xcrun notarytool store-credentials AC_PASSWORD \
    --apple-id "your@email.com" \
    --team-id "TEAM123"
  ```

### Installation Issues

**"WireGuard installation failed"**
- Install Homebrew: https://brew.sh
- Or install WireGuard manually: `brew install wireguard-tools`

**"Failed to download binaries during build"**
- Check internet connection during PKG build
- Verify GitHub releases are accessible
- Try specifying a specific version: `./build-pkg.sh v1.0.0`

**"GnosisVPN.app not found"**
- The app may not be available for all platforms
- The installer will still complete successfully with just the command-line tools

## Logs

Installation logs are written to:
- Pre-install: `/tmp/gnosis-vpn-preinstall.log`
- Post-install: `/tmp/gnosis-vpn-postinstall.log`

View logs:
```bash
cat /tmp/gnosis-vpn-preinstall.log
cat /tmp/gnosis-vpn-postinstall.log
```

## Development

### Testing Changes

After modifying any files:

1. Rebuild the installer:
   ```bash
   ./build-pkg.sh 1.0.0-dev
   ```

2. Test in a clean environment or VM
3. Check log files for errors
4. Verify installed files:
   - Binaries in `/usr/local/bin/`
   - App in `/Applications/GnosisVPN.app`
   - Config in `/etc/gnosisvpn/config.toml`

### Debugging

Enable verbose output:
```bash
set -x  # Add to script for bash tracing
```

Test scripts independently:
```bash
# Test preinstall (downloads binaries and app)
sudo ./resources/scripts/preinstall "" "/" "/" "/"

# Test postinstall (generates config)
export INSTALLER_CHOICE_NETWORK="rotsee"
sudo ./resources/scripts/postinstall "" "/" "/" "/"
```

## Version Management

The build script fetches the latest version from:
```
https://raw.githubusercontent.com/gnosis/gnosis_vpn-client/main/LATEST
```

To build an installer for a specific version:
```bash
./build-pkg.sh v1.2.3
```

## Security

- Scripts run with root privileges during installation
- Binaries are downloaded over HTTPS from GitHub at build time
- Universal binaries are packaged directly into the PKG
- No personal information is collected or transmitted

## License

This installer is part of the Gnosis VPN Client project. See the main repository for license information.

## Support

- Documentation: https://github.com/gnosis/gnosis_vpn-client
- Issues: https://github.com/gnosis/gnosis_vpn-client/issues
- Releases: https://github.com/gnosis/gnosis_vpn-client/releases
