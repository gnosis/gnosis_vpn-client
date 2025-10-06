# Package builder function for gnosis_vpn Rust workspace
#
# This function creates a standardized Nix derivation for building gnosis_vpn packages
# with consistent source files, target configurations, and build parameters.
#
# Usage example:
#   gvpn = mkPackage {
#     pname = "gnosis_vpn";
#     profile = "release";
#   };
{
  craneLib, # Crane library instance with custom Rust toolchain
  lib, # nixpkgs library utilities
  targetForSystem, # Target triple for the current system (e.g., "x86_64-unknown-linux-musl")
  cargoArtifacts, # Pre-built cargo dependencies for caching
  version, # Package version extracted from Cargo.toml
}:
{
  pname, # Package name (e.g., "gnosis_vpn" or "gnosis_vpn-dev")
  profile ? "release", # Cargo build profile (default: "release", can be "dev", "intelmac", etc.)
  cargoExtraArgs ? "--all", # Additional cargo build arguments (default: "--all")
  ... # Any additional arguments are passed through to craneLib.buildPackage
}@args:
let
  # Source files configuration
  # Uses filesets to include only necessary files for the build, excluding
  # target directories, documentation, and other non-essential files.
  # This improves build reproducibility and reduces closure size.
  srcFiles = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      (craneLib.fileset.commonCargoSources ../gnosis_vpn-lib)
      (craneLib.fileset.commonCargoSources ../gnosis_vpn-ctl)
      (craneLib.fileset.commonCargoSources ../gnosis_vpn)
    ];
  };

  # Target-specific build arguments
  # Each target triple has its own compiler flags and build configuration.
  # - Linux targets use musl for static linking and mold for faster linking
  # - Darwin targets use different profiles based on architecture (intelmac for x86_64)
  # - All targets enable crt-static for standalone binaries
  targetCrateArgs = {
    "x86_64-unknown-linux-musl" = {
      CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
      CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-fuse-ld=mold";
    };
    "aarch64-unknown-linux-musl" = {
      CARGO_BUILD_TARGET = "aarch64-unknown-linux-musl";
      CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-fuse-ld=mold";
    };
    "x86_64-apple-darwin" = {
      CARGO_PROFILE = "intelmac";
      CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
    };
    "aarch64-apple-darwin" = {
      CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
    };
  };

  # Individual crate build arguments shared across all packages
  # These are common settings that apply to every package built with this function.
  individualCrateArgs = {
    inherit cargoArtifacts; # Reuse pre-built dependencies for faster builds
    inherit version; # Package version from Cargo.toml
    # Disable tests in package builds since they run separately via cargo-nextest check
    doCheck = false;
  };

  # Final package arguments
  # Merges all configuration layers in order:
  # 1. Base crate arguments (version, artifacts, doCheck)
  # 2. Package-specific settings (pname, profile, source)
  # 3. Target-specific flags (RUSTFLAGS, target triple)
  # 4. Additional user-provided arguments (after filtering internal ones)
  packageArgs =
    individualCrateArgs
    // {
      inherit pname;
      inherit cargoExtraArgs;
      src = srcFiles;
      CARGO_PROFILE = profile;
    }
    // (builtins.getAttr targetForSystem targetCrateArgs)
    // (builtins.removeAttrs args [
      "pname"
      "profile"
      "cargoExtraArgs"
    ]);
in
# Build the package using crane's buildPackage with all merged arguments
craneLib.buildPackage packageArgs
