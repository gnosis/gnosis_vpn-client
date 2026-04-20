# gnosisvpn.nix - GnosisVPN package definitions
#
# Package definitions using HOPR nix-lib build tools.
# Uses nixLib.mkRustPackage for consistent, reproducible builds across platforms.
# This file replaces nix/mkPackage.nix which used crane directly.
#
# Structure:
# - Local builds: binary-gnosis_vpn (release), binary-gnosis_vpn-dev (dev)
# - Cross-compiled: binary-gnosis_vpn-{arch}-{os} for each target platform
# - QA: gnosis_vpn-test, gnosis_vpn-clippy, gnosis_vpn-docs, gnosis_vpn-audit, gnosis_vpn-licenses
{
  lib,
  nixLib,
  self,
  pkgs,
  craneLib,
  advisory-db,
}:

let
  fs = lib.fileset;
  rev = toString (self.shortRev or self.dirtyShortRev);

  builders = nixLib.mkRustBuilders {
    rustToolchainFile = ../rust-toolchain.toml;
  };

  sources = {
    main = nixLib.mkSrc {
      inherit fs;
      root = ../.;
    };
    test = nixLib.mkTestSrc {
      inherit fs;
      root = ../.;
    };
    deps = nixLib.mkDepsSrc {
      inherit fs;
      root = ../.;
    };
    # Includes audit and license config files needed by crane-based checks
    checks = nixLib.mkSrc {
      inherit fs;
      root = ../.;
      extraFiles = [
        ../.cargo/audit.toml
        ../deny.toml
      ];
    };
  };

  mkGnosisvpnBuildArgs =
    {
      pkgs,
      stdenv,
      src,
      depsSrc,
      extraCargoArgs ? "",
    }:
    let
      # Linux static builds require libmnl, libnftnl, and sqlite in addition to
      # the openssl+cacert that nix-lib provides by default.
      linuxExtraBuildInputs = lib.optionals stdenv.hostPlatform.isLinux (
        with pkgs.pkgsStatic;
        [
          libmnl
          libnftnl
          sqlite
        ]
      );
      linuxRustflagsArg = lib.optionalString stdenv.hostPlatform.isLinux
        "--config ${lib.escapeShellArg "build.rustflags=[\"-C\",\"target-feature=+crt-static\"]"}";
    in
    {
      inherit src depsSrc rev;
      # prependPackageName=false: skip the automatic `-p gnosis_vpn` that nix-lib
      # derives from [workspace.metadata.crane] name — it has no matching package
      # since the workspace uses a wildcard `members = ["gnosis_vpn*"]`.
      # The --bin flags below are sufficient to select the right binaries.
      prependPackageName = false;
      cargoExtraArgs =
        "--bin gnosis_vpn-root --bin gnosis_vpn-worker --bin gnosis_vpn-ctl ${extraCargoArgs}"
        + lib.optionalString stdenv.hostPlatform.isLinux " ${linuxRustflagsArg}";
      cargoToml = ../Cargo.toml;
      extraBuildInputs = linuxExtraBuildInputs;
    };

  mkGnosisvpnBuildArgsFor = builder: args: builder.callPackage mkGnosisvpnBuildArgs args;
in
{
  # Local builds

  # binary-gnosis_vpn (renamed from gnosis_vpn-release)
  binary-gnosis_vpn = builders.local.callPackage nixLib.mkRustPackage (mkGnosisvpnBuildArgsFor builders.local {
    src = sources.main;
    depsSrc = sources.deps;
  });

  # binary-gnosis_vpn-dev (renamed from gnosis_vpn-dev)
  binary-gnosis_vpn-dev = builders.local.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.local {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      CARGO_PROFILE = "dev";
    }
  );

  # Cross-compiled — x86_64 Linux
  binary-gnosis_vpn-x86_64-linux =
    builders.x86_64-linux.callPackage nixLib.mkRustPackage
      (mkGnosisvpnBuildArgsFor builders.x86_64-linux {
        src = sources.main;
        depsSrc = sources.deps;
      });

  binary-gnosis_vpn-x86_64-linux-dev = builders.x86_64-linux.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.x86_64-linux {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      CARGO_PROFILE = "dev";
    }
  );

  # Cross-compiled — aarch64 Linux
  binary-gnosis_vpn-aarch64-linux =
    builders.aarch64-linux.callPackage nixLib.mkRustPackage
      (mkGnosisvpnBuildArgsFor builders.aarch64-linux {
        src = sources.main;
        depsSrc = sources.deps;
      });

  binary-gnosis_vpn-aarch64-linux-dev = builders.aarch64-linux.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.aarch64-linux {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      CARGO_PROFILE = "dev";
    }
  );

  # System test package: all service binaries + the system test runner in one derivation.
  # Used by CI to run the system test against a live network in a single nix build command.
  binary-gnosis_vpn-system_tests = builders.local.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.local {
      src = sources.main;
      depsSrc = sources.deps;
      extraCargoArgs = "--bin gnosis_vpn-system_tests";
    })
    // {
      CARGO_PROFILE = "dev";
    }
  );

  # Tests / QA
  gnosis_vpn-test = builders.local.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.local {
      src = sources.test;
      depsSrc = sources.deps;
    })
    // {
      runTests = true;
    }
  );

  gnosis_vpn-clippy = builders.local.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.local {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      runClippy = true;
    }
  );

  gnosis_vpn-docs = builders.localNightly.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.localNightly {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      buildDocs = true;
    }
  );

  # Audit dependencies
  # Vulnerabilities are exempted because they are either:
  # - From transitive dependencies we cannot control
  # - Unmaintained crates with no viable alternatives
  # - Lack a fixed version
  gnosis_vpn-audit = craneLib.cargoAudit {
    src = sources.checks;
    inherit advisory-db;
  };

  # Audit licenses
  gnosis_vpn-licenses = craneLib.cargoDeny {
    src = sources.checks;
  };
}
// lib.optionalAttrs pkgs.stdenv.isDarwin {
  # macOS — aarch64 (only available on Darwin hosts; cctools is Darwin-only)
  binary-gnosis_vpn-aarch64-darwin =
    builders.aarch64-darwin.callPackage nixLib.mkRustPackage
      (mkGnosisvpnBuildArgsFor builders.aarch64-darwin {
        src = sources.main;
        depsSrc = sources.deps;
      });

  binary-gnosis_vpn-aarch64-darwin-dev = builders.aarch64-darwin.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgsFor builders.aarch64-darwin {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      CARGO_PROFILE = "dev";
    }
  );
}
