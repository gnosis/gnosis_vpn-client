#!/bin/bash
#
# Gnosis VPN Uninstaller for macOS
#
# This script removes all files installed by the Gnosis VPN installer.
#
# Usage:
#   sudo ./uninstall.sh
#

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
PKG_ID="org.gnosis.vpn.client"
BIN_DIR="/usr/local/bin"
CONFIG_DIR="/etc/gnosisvpn"
LOG_DIR="/Library/Logs/GnosisVPNInstaller"

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

# Check if running as root
check_root() {
    if [[ $EUID -ne 0 ]]; then
        log_error "This script must be run as root"
        echo "Please run: sudo $0"
        exit 1
    fi
}

# Print banner
print_banner() {
    echo "=========================================="
    echo "  Gnosis VPN Uninstaller"
    echo "=========================================="
    echo ""
}

# Confirm uninstallation
confirm_uninstall() {
    echo "This will remove:"
    echo "  - Binaries: $BIN_DIR/gnosis_vpn, $BIN_DIR/gnosis_vpn-ctl"
    echo "  - Configuration: $CONFIG_DIR/"
    echo "  - Installation logs: $LOG_DIR/"
    echo "  - Package receipt: $PKG_ID"
    echo ""
    read -p "Are you sure you want to uninstall Gnosis VPN? [y/N] " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        log_info "Uninstallation cancelled"
        exit 0
    fi
    echo ""
}

# Backup configuration
backup_config() {
    if [[ -d "$CONFIG_DIR" ]]; then
        local timestamp
        timestamp=$(date +%Y%m%d-%H%M%S)
        local backup_dir="${HOME}/gnosis-vpn-config-backup-${timestamp}"
        
        log_info "Backing up configuration to: $backup_dir"
        if cp -R "$CONFIG_DIR" "$backup_dir"; then
            log_success "Configuration backed up to $backup_dir"
        else
            log_warn "Failed to backup configuration"
        fi
        echo ""
    fi
}

# Remove binaries
remove_binaries() {
    log_info "Removing binaries..."
    
    local removed=0
    
    if [[ -f "$BIN_DIR/gnosis_vpn" ]]; then
        rm -f "$BIN_DIR/gnosis_vpn"
        log_success "Removed $BIN_DIR/gnosis_vpn"
        removed=$((removed + 1))
    fi
    
    if [[ -f "$BIN_DIR/gnosis_vpn-ctl" ]]; then
        rm -f "$BIN_DIR/gnosis_vpn-ctl"
        log_success "Removed $BIN_DIR/gnosis_vpn-ctl"
        removed=$((removed + 1))
    fi
    
    if [[ $removed -eq 0 ]]; then
        log_warn "No binaries found to remove"
    fi
    
    echo ""
}

# Remove configuration
remove_config() {
    log_info "Removing configuration..."
    
    if [[ -d "$CONFIG_DIR" ]]; then
        rm -rf "$CONFIG_DIR"
        log_success "Removed $CONFIG_DIR"
    else
        log_warn "Configuration directory not found"
    fi
    
    echo ""
}

# Remove logs
remove_logs() {
    log_info "Removing installation logs..."
    
    if [[ -d "$LOG_DIR" ]]; then
        rm -rf "$LOG_DIR"
        log_success "Removed $LOG_DIR"
    else
        log_warn "Log directory not found"
    fi
    
    echo ""
}

# Forget package receipt
forget_package() {
    log_info "Removing package receipt..."
    
    if pkgutil --pkgs | grep -q "^${PKG_ID}$"; then
        pkgutil --forget "$PKG_ID"
        log_success "Forgot package: $PKG_ID"
    else
        log_warn "Package receipt not found: $PKG_ID"
    fi
    
    echo ""
}

# Verify uninstallation
verify_uninstall() {
    log_info "Verifying uninstallation..."
    
    local errors=0
    
    if [[ -f "$BIN_DIR/gnosis_vpn" ]]; then
        log_error "Binary still exists: $BIN_DIR/gnosis_vpn"
        errors=$((errors + 1))
    fi
    
    if [[ -f "$BIN_DIR/gnosis_vpn-ctl" ]]; then
        log_error "Binary still exists: $BIN_DIR/gnosis_vpn-ctl"
        errors=$((errors + 1))
    fi
    
    if [[ -d "$CONFIG_DIR" ]]; then
        log_error "Configuration directory still exists: $CONFIG_DIR"
        errors=$((errors + 1))
    fi
    
    if pkgutil --pkgs | grep -q "^${PKG_ID}$"; then
        log_error "Package receipt still exists: $PKG_ID"
        errors=$((errors + 1))
    fi
    
    if [[ $errors -eq 0 ]]; then
        log_success "Uninstallation verified successfully"
    else
        log_warn "Uninstallation completed with $errors warning(s)"
    fi
    
    echo ""
}

# Print summary
print_summary() {
    echo "=========================================="
    echo "  Uninstallation Summary"
    echo "=========================================="
    echo ""
    echo "Gnosis VPN has been uninstalled from your system."
    echo ""
    echo "What was removed:"
    echo "  ✓ Binaries"
    echo "  ✓ Configuration (backed up to ~/gnosis-vpn-config-backup-*)"
    echo "  ✓ Installation logs"
    echo "  ✓ Package receipt"
    echo ""
    echo "To reinstall, download and run the installer again."
    echo "=========================================="
}

# Main uninstallation process
main() {
    print_banner
    check_root
    confirm_uninstall
    backup_config
    remove_binaries
    remove_config
    remove_logs
    forget_package
    verify_uninstall
    print_summary
}

# Execute main
main

exit 0
