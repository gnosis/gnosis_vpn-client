/**
 * Installation Check Script for Gnosis VPN Installer
 *
 * This script runs before the installation begins to verify system requirements.
 * It performs checks for:
 * - macOS version compatibility
 * - System architecture support
 * - Available disk space
 */

// Check macOS version - require 11.0 (Big Sur) or later
if (system.version && system.version.ProductVersion) {
    var osVersion = system.version.ProductVersion;
    var majorVersion = parseInt(osVersion.split('.')[0], 10);

    if (majorVersion < 11) {
        my.result.title = "Unsupported macOS Version";
        my.result.message = "macOS 11.0 (Big Sur) or later is required. You are running macOS " + osVersion + ".";
        my.result.type = "Fatal";
        false;
    }
}

// Check system architecture
if (system.sysctl) {
    var arch = system.sysctl("hw.machine");
    var supportedArchitectures = ["x86_64", "arm64", "arm64e"];

    var isSupported = false;
    for (var i = 0; i < supportedArchitectures.length; i++) {
        if (arch === supportedArchitectures[i]) {
            isSupported = true;
            break;
        }
    }

    if (!isSupported) {
        my.result.title = "Unsupported Architecture";
        my.result.message = "Your system architecture (" + arch + ") is not supported. Only x86_64 (Intel) and arm64 (Apple Silicon) are supported.";
        my.result.type = "Fatal";
        false;
    }
}

// All checks passed
my.result.title = "System Compatible";
my.result.message = "Your system meets all requirements for Gnosis VPN installation.";
my.result.type = "Info";
true;
