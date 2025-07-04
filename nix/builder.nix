{ nixpkgs, localSystem, crossSystem, rust-overlay, crane, lib, ... }:
let
  pkgs = (import nixpkgs {
    localSystem = localSystem;
    crossSystem = crossSystem;
    overlays = [ (import rust-overlay) ];
  });

  systemTargets = {
    "x86_64-linux" = "x86_64-unknown-linux-musl";
    "aarch64-linux" = "aarch64-unknown-linux-musl";
    "x86_64-darwin" = "x86_64-apple-darwin";
    "aarch64-darwin" = "aarch64-apple-darwin";
  };

  targetForSystem = builtins.getAttr crossSystem systemTargets;

  # NB: we don't need to overlay our custom toolchain for the *entire*
  # pkgs (which would require rebuidling anything else which uses rust).
  # Instead, we just want to update the scope that crane will use by appending
  # our specific toolchain there.
  # cross = pkgs.pkgsCross.musl64;
  craneLib = (crane.mkLib pkgs).overrideToolchain (
    p:
    (p.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml).override {
      targets = [ targetForSystem ];
    }
  );

  src = craneLib.cleanCargoSource ../.;

  # Common arguments can be set here to avoid repeating them later
  commonArgs = {
    inherit src;
    strictDeps = true;

    nativeBuildInputs = [ pkgs.pkg-config ] ++ lib.optionals pkgs.stdenv.isLinux [
      pkgs.mold
    ];
    buildInputs =
      [
        pkgs.pkgsStatic.openssl
      ]
      ++ lib.optionals pkgs.stdenv.isDarwin [
        # Additional darwin specific inputs can be set here
        pkgs.libiconv
      ];

    # Additional environment variables can be set directly
    # MY_CUSTOM_VAR = "some value";
  };

  # Build *just* the cargo dependencies (of the entire workspace),
  # so we can reuse all of that work (e.g. via cachix) when running in CI
  # It is *highly* recommended to use something like cargo-hakari to avoid
  # cache misses when building individual top-level-crates
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  individualCrateArgs = commonArgs // {
    inherit cargoArtifacts;
    inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
    # NB: we disable tests since we'll run them all via cargo-nextest
    doCheck = false;
  };

  srcFiles =
    lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        (craneLib.fileset.commonCargoSources ../gnosis_vpn-lib)
        (craneLib.fileset.commonCargoSources ../gnosis_vpn-ctl)
        (craneLib.fileset.commonCargoSources ../gnosis_vpn)
      ];
    };

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

  # Build the top-level crates of the workspace as individual derivations.
  # This allows consumers to only depend on (and build) only what they need.
  # Though it is possible to build the entire workspace as a single derivation,
  # so this is left up to you on how to organize things
  #
  # Note that the cargo workspace must define `workspace.members` using wildcards,
  # otherwise, omitting a crate (like we do below) will result in errors since
  # cargo won't be able to find the sources for all members.

in
craneLib.buildPackage (
  individualCrateArgs //
  (builtins.getAttr targetForSystem targetCrateArgs) // {
    pname = "gnosis_vpn";
    cargoExtraArgs = "--all";
    src = srcFiles;
  }
)
