#!/bin/bash
#
# Unified Logging Library for Gnosis VPN Installer
#
# This library provides consistent logging functions across all installer scripts.
# Source this file in your scripts to use the logging functions.
#
# Usage:
#   source "$(dirname "$0")/logging.sh"
#   setup_logging "script-name"
#   log_info "This is an info message"
#   log_success "This is a success message"
#   log_warn "This is a warning"
#   log_error "This is an error"
#

# Default log directory and file
INSTALLER_LOG_DIR="${INSTALLER_LOG_DIR:-/Library/Logs/GnosisVPNInstaller}"
INSTALLER_LOG_FILE="${INSTALLER_LOG_FILE:-${INSTALLER_LOG_DIR}/installer.log}"

# Setup logging for a script
setup_logging() {
    local script_name="${1:-unknown}"

    # Create log directory if it doesn't exist
    mkdir -p "$INSTALLER_LOG_DIR"

    # Set script-specific log file if not already set
    if [[ -z "${SCRIPT_LOG_FILE:-}" ]]; then
        SCRIPT_LOG_FILE="${INSTALLER_LOG_DIR}/${script_name}.log"
    fi

    # Redirect output to both console and log files
    exec > >(tee -a "$INSTALLER_LOG_FILE" "$SCRIPT_LOG_FILE") 2>&1

    # Log script start
    echo ""
    echo "=========================================="
    echo "Script: $script_name"
    echo "Started: $(date '+%Y-%m-%d %H:%M:%S')"
    echo "=========================================="
    echo ""
}

# Get timestamp for log messages
log_timestamp() {
    date '+%Y-%m-%d %H:%M:%S'
}

# Log info message
log_info() {
    echo "[$(log_timestamp)] [INFO] $*"
}

# Log success message
log_success() {
    echo "[$(log_timestamp)] [SUCCESS] $*"
}

# Log warning message
log_warn() {
    echo "[$(log_timestamp)] [WARN] $*"
}

# Log error message
log_error() {
    echo "[$(log_timestamp)] [ERROR] $*"
}

# Log debug message (only if DEBUG=true)
log_debug() {
    if [[ "${DEBUG:-false}" == "true" ]]; then
        echo "[$(log_timestamp)] [DEBUG] $*"
    fi
}

# Log section separator
log_section() {
    local title="$1"
    echo ""
    echo "=========================================="
    echo "$title"
    echo "=========================================="
    echo ""
}

# Log script completion
log_script_end() {
    local status="${1:-success}"
    echo ""
    echo "=========================================="
    echo "Script completed: $status"
    echo "Ended: $(date '+%Y-%m-%d %H:%M:%S')"
    echo "=========================================="
    echo ""
}

# Export functions for use in subshells
export -f log_timestamp
export -f log_info
export -f log_success
export -f log_warn
export -f log_error
export -f log_debug
export -f log_section
export -f log_script_end
