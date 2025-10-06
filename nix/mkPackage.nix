{
  craneLib,
  lib,
  targetForSystem,
  cargoArtifacts,
  version,
}:
{
  pname,
  profile ? "release",
  cargoExtraArgs ? "--all",
  ...
}@args:
let
  # Source files configuration
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

  # Individual crate arguments
  individualCrateArgs = {
    inherit cargoArtifacts;
    inherit version;
    # NB: we disable tests since we'll run them all via cargo-nextest
    doCheck = false;
  };

  # Package-specific arguments
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
craneLib.buildPackage packageArgs
