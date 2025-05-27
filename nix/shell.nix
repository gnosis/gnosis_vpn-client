{ config
, pkgs
, crane
, extraPackages ? [ ]
, useRustNightly ? false
}:
let
  cargoTarget = pkgs.stdenv.buildPlatform.config;
  rustToolchain =
    if useRustNightly
    then pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default)
    else
      (pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile
        ../rust-toolchain.toml).override { targets = [ cargoTarget ]; };
  craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
in
craneLib.devShell {
  packages = with pkgs; [
    openssl
    pkg-config
    patchelf

    # testing utilities
    bash
    curl
    # gnu awk
    gawk
    gnumake
    jq
    which
    yq-go

    # development helper
    cargo-outdated

    # github integration
    gh

    # test Github automation
    act

    ## formatting
    config.treefmt.build.wrapper
  ] ++
  (lib.attrValues config.treefmt.build.programs) ++
  lib.optionals stdenv.isLinux [ autoPatchelfHook ] ++ extraPackages;
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.pkgsBuildHost.openssl ];
  RUST_MIN_STACK = "16777216"; # 16MB required to run the tests and compilation
}
