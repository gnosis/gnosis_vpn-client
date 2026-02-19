# Package builder function for gnosis_vpn Rust workspace
#
# This function creates a standardized Nix derivation for building gnosis_vpn packages
# with consistent source files, target configurations, and build parameters.
#
# Usage example:
#   gvpn = mkPackage {
#     pname = "gnosis_vpn";
#   };
{
  # Crane library instance with custom Rust toolchain
  craneLib,
  # nixpkgs library utilities
  lib,
  # package repo
  pkgs,
  # Pre-built cargo dependencies for caching
  cargoArtifacts,
  # Package version extracted from Cargo.toml
  version,
  # Common build arguments including buildInputs and nativeBuildInputs
  commonArgs,
  # Package name (e.g., "gnosis_vpn-root", "gnosis_vpn-worker" or "gnosis_vpn-dev")
  pname,
}:
let
  # Source files configuration
  # Uses filesets to include only necessary files for the build, excluding
  # target directories, documentation, and other non-essential files.
  # This improves build reproducibility and reduces closure size.
  srcFiles = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../.cargo/config.toml
      ../Cargo.toml
      ../Cargo.lock
      (craneLib.fileset.commonCargoSources ../gnosis_vpn-lib)
      (craneLib.fileset.commonCargoSources ../gnosis_vpn-ctl)
      (craneLib.fileset.commonCargoSources ../gnosis_vpn-root)
      (craneLib.fileset.commonCargoSources ../gnosis_vpn-worker)
      ./rustfmt.toml
      ./rust-toolchain.toml
    ];
  };

  # Individual crate build arguments shared across all packages
  # These are common settings that apply to every package built with this function.
  individualCrateArgs = {
    inherit cargoArtifacts; # Reuse pre-built dependencies for faster builds
    inherit version; # Package version from Cargo.toml
    # Disable tests in package builds since they run separately via cargo-nextest check
    doCheck = false;
  };

  # libiconv fixup is only needed/supported on Darwin
  postInstall = lib.optionalString pkgs.stdenv.isDarwin ''
    for bin in $(find "$out/bin" -type f); do
      local linked_iconv=$(otool -L "$bin" | grep "/nix/store/.*libiconv.*dylib" | awk '{print $1}')

      if [ -n "$linked_iconv" ]; then
        echo "Rewriting $bin - found nix libiconv reference: $linked_iconv"

        # macOS usually ships libiconv.2.dylib in /usr/lib
        install_name_tool -change "$linked_iconv" "/usr/lib/libiconv.2.dylib" "$bin"

        echo "Fixed libiconv path"
      else
        echo "Not rewriting $bin - no nix libiconv reference found"
      fi
    done
  '';

  # Final package arguments
  # Merges all configuration layers in order:
  # 1. Common arguments (buildInputs, nativeBuildInputs, src, etc.)
  # 2. Base crate arguments (version, artifacts, doCheck)
  # 3. Package-specific settings (pname, source)
  packageArgs =
    commonArgs
    // individualCrateArgs
    // {
      inherit pname;
      inherit postInstall;
      cargoExtraArgs = "--bin gnosis_vpn-root --bin gnosis_vpn-worker --bin gnosis_vpn-ctl";
      src = srcFiles;
    };
in
# Build the package using crane's buildPackage with all merged arguments
craneLib.buildPackage packageArgs
