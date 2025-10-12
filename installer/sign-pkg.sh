#!/bin/bash
#
# Sign and notarize script for Gnosis VPN macOS PKG installer
#
# This script signs the PKG installer with a Developer ID certificate
# and optionally submits it for notarization with Apple.
#
# Prerequisites:
#   - Apple Developer account with Developer ID Installer certificate
#   - Certificate installed in Keychain
#   - App-specific password for notarization (stored in keychain)
#
# Usage:
#   ./sign-pkg.sh <path-to-pkg> [--notarize]
#
# Example:
#   ./sign-pkg.sh build/GnosisVPN-Installer-1.0.0.pkg
#   ./sign-pkg.sh build/GnosisVPN-Installer-1.0.0.pkg --notarize
#

set -euo pipefail

# Configuration
PKG_FILE="${1:-}"
NOTARIZE="${2:-}"
SIGNING_IDENTITY="${SIGNING_IDENTITY:-Developer ID Installer}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

# Print usage
usage() {
    cat <<EOF
Usage: $0 <path-to-pkg> [--notarize]

Sign and optionally notarize a macOS PKG installer.

Arguments:
    path-to-pkg     Path to the unsigned PKG file

Options:
    --notarize      Submit for notarization after signing

Environment variables:
    SIGNING_IDENTITY    Developer ID certificate name (default: "Developer ID Installer")
    APPLE_ID            Apple ID for notarization
    TEAM_ID             Apple Developer Team ID
    KEYCHAIN_PROFILE    Keychain profile name for notarization credentials

Example:
    export SIGNING_IDENTITY="Developer ID Installer: Your Name (TEAM123)"
    $0 build/GnosisVPN-Installer-1.0.0.pkg --notarize

EOF
    exit 1
}

# Validate input
validate_input() {
    if [[ -z "$PKG_FILE" ]]; then
        log_error "No PKG file specified"
        usage
    fi

    if [[ ! -f "$PKG_FILE" ]]; then
        log_error "PKG file not found: $PKG_FILE"
        exit 1
    fi

    log_info "Package to sign: $PKG_FILE"
}

# Check for signing certificate
check_certificate() {
    log_info "Checking for signing certificate..."

    # List available installer certificates
    local certs
    if certs=$(security find-identity -v -p basic | grep "Developer ID Installer"); then
        log_success "Found signing certificates:"
        echo "$certs"
        echo ""
    else
        log_error "No Developer ID Installer certificate found in keychain"
        log_info "To install a certificate:"
        log_info "  1. Download your certificate from https://developer.apple.com"
        log_info "  2. Double-click the .cer file to install it in Keychain Access"
        exit 1
    fi
}

# Sign the package
sign_package() {
    log_info "Signing package with identity: $SIGNING_IDENTITY"

    local signed_pkg="${PKG_FILE%.pkg}-signed.pkg"

    if productsign \
        --sign "$SIGNING_IDENTITY" \
        "$PKG_FILE" \
        "$signed_pkg"; then

        log_success "Package signed successfully: $signed_pkg"
        echo ""

        # Update PKG_FILE to point to signed version
        PKG_FILE="$signed_pkg"
    else
        log_error "Failed to sign package"
        log_info "Make sure the signing identity is correct:"
        log_info "  export SIGNING_IDENTITY='Developer ID Installer: Your Name (TEAM_ID)'"
        exit 1
    fi
}

# Verify signature
verify_signature() {
    log_info "Verifying package signature..."

    if pkgutil --check-signature "$PKG_FILE"; then
        log_success "Signature verification passed"
        echo ""
    else
        log_error "Signature verification failed"
        exit 1
    fi
}

# Submit for notarization
notarize_package() {
    log_info "Submitting package for notarization..."

    # Check for required environment variables
    if [[ -z "${APPLE_ID:-}" ]]; then
        log_error "APPLE_ID environment variable not set"
        log_info "Set your Apple ID: export APPLE_ID='your@email.com'"
        exit 1
    fi

    if [[ -z "${TEAM_ID:-}" ]]; then
        log_error "TEAM_ID environment variable not set"
        log_info "Set your Team ID: export TEAM_ID='ABC123XYZ'"
        exit 1
    fi

    local keychain_profile="${KEYCHAIN_PROFILE:-AC_PASSWORD}"

    log_info "Using Apple ID: $APPLE_ID"
    log_info "Using Team ID: $TEAM_ID"
    log_info "Using keychain profile: $keychain_profile"
    echo ""

    # Submit for notarization
    log_info "Submitting to Apple (this may take several minutes)..."

    if xcrun notarytool submit "$PKG_FILE" \
        --apple-id "$APPLE_ID" \
        --team-id "$TEAM_ID" \
        --keychain-profile "$keychain_profile" \
        --wait; then

        log_success "Notarization successful"
        echo ""
    else
        log_error "Notarization failed"
        log_info "Check the notarization log for details:"
        log_info "  xcrun notarytool log <submission-id> --keychain-profile $keychain_profile"
        exit 1
    fi
}

# Staple notarization ticket
staple_ticket() {
    log_info "Stapling notarization ticket to package..."

    if xcrun stapler staple "$PKG_FILE"; then
        log_success "Notarization ticket stapled successfully"
        echo ""
    else
        log_warn "Failed to staple ticket (package is still valid, but requires internet for verification)"
        echo ""
    fi
}

# Print summary
print_summary() {
    echo "=========================================="
    echo "  Signing Summary"
    echo "=========================================="
    echo "Signed package: $PKG_FILE"
    echo ""

    if [[ -f "$PKG_FILE" ]]; then
        local size
        size=$(du -h "$PKG_FILE" | cut -f1)
        echo "Size:           $size"

        local sha256
        sha256=$(shasum -a 256 "$PKG_FILE" | cut -d' ' -f1)
        echo "SHA256:         $sha256"
    fi

    echo ""
    echo "The package is ready for distribution!"
    echo ""
    echo "To distribute:"
    echo "  1. Upload to GitHub releases"
    echo "  2. Share the SHA256 checksum"
    echo "  3. Users can verify with: shasum -a 256 <pkg-file>"
    echo ""
    echo "=========================================="
}

# Main process
main() {
    echo "=========================================="
    echo "  Gnosis VPN PKG Signing Tool"
    echo "=========================================="
    echo ""

    validate_input
    check_certificate
    sign_package
    verify_signature

    # Notarize if requested
    if [[ "$NOTARIZE" == "--notarize" ]]; then
        notarize_package
        staple_ticket
    else
        log_warn "Skipping notarization (use --notarize flag to notarize)"
        log_info "For distribution outside of known developers, notarization is required for macOS 10.15+"
        echo ""
    fi

    print_summary
}

# Execute main
main

exit 0
