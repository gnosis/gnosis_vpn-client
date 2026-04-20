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

  # Target-specific package sets for cross-compiled Linux builds.
  # pkgsCross.*.pkgsStatic gives static libraries for the target arch regardless of host,
  # preventing host/target package mixing (e.g. aarch64 host building x86_64 target).
  x86_64LinuxStaticPkgs = pkgs.pkgsCross.musl64.pkgsStatic;
  aarch64LinuxStaticPkgs = pkgs.pkgsCross.aarch64-multiplatform-musl.pkgsStatic;

  # Linux static builds require libmnl, libnftnl, and sqlite in addition to
  # the openssl+cacert that nix-lib provides by default.
  # Takes staticPkgs so native and cross builds each pull from the correct arch.
  mkLinuxStaticBuildInputs =
    staticPkgs: with staticPkgs; [
      libmnl
      libnftnl
      sqlite
    ];

  # Native Linux builds: host == target, so host's pkgsStatic is correct.
  linuxExtraBuildInputs = lib.optionals pkgs.stdenv.isLinux (
    mkLinuxStaticBuildInputs pkgs.pkgsStatic
  );

  # Parameters required for musl static builds that nix-lib does not cover.
  # nix-lib handles: CARGO_BUILD_TARGET, CARGO_TARGET_*_LINKER, +crt-static, openssl paths.
  # These must be applied via overrideAttrs since rust-package.nix drops unknown attrs
  # before they reach mkDerivation.
  # Takes staticPkgs so each target arch supplies its own correct library paths
  # instead of pulling from the host's pkgs.pkgsStatic.
  mkLinuxStaticEnv = staticPkgs: {
    # musl is incompatible with the fortify hardening flag
    hardeningDisable = [ "fortify" ];
    # tell libsqlite3-sys to locate sqlite via pkg-config
    LIBSQLITE3_SYS_USE_PKG_CONFIG = "1";
    # give mnl-sys / nftnl-sys direct lib dirs; pkg-config can fail in cross contexts
    LIBMNL_LIB_DIR = "${staticPkgs.libmnl}/lib";
    LIBNFTNL_LIB_DIR = "${staticPkgs.libnftnl}/lib";
    PKG_CONFIG_PATH = lib.concatStringsSep ":" [
      "${staticPkgs.openssl.dev}/lib/pkgconfig"
      "${staticPkgs.sqlite.dev}/lib/pkgconfig"
      "${staticPkgs.libmnl}/lib/pkgconfig"
      "${staticPkgs.libnftnl}/lib/pkgconfig"
    ];
  };

  # Stamps env onto both the package and its internal cargoArtifacts so the
  # deps-only cache and the final build share the same environment.
  mkWithStaticEnv =
    env: drv:
    drv.overrideAttrs (
      prev:
      env
      // {
        cargoArtifacts =
          if prev.cargoArtifacts != null then prev.cargoArtifacts.overrideAttrs (_: env) else null;
      }
    );

  # CC/CXX are arch-specific: cc-rs uses them to compile C code in build.rs scripts.
  withX86_64LinuxStaticEnv = mkWithStaticEnv (
    mkLinuxStaticEnv x86_64LinuxStaticPkgs
    // {
      CC_x86_64_unknown_linux_musl = "${x86_64LinuxStaticPkgs.stdenv.cc}/bin/x86_64-unknown-linux-musl-gcc";
      CXX_x86_64_unknown_linux_musl = "${x86_64LinuxStaticPkgs.stdenv.cc}/bin/x86_64-unknown-linux-musl-g++";
    }
  );

  withAarch64LinuxStaticEnv = mkWithStaticEnv (
    mkLinuxStaticEnv aarch64LinuxStaticPkgs
    // {
      CC_aarch64_unknown_linux_musl = "${aarch64LinuxStaticPkgs.stdenv.cc}/bin/aarch64-unknown-linux-musl-gcc";
      CXX_aarch64_unknown_linux_musl = "${aarch64LinuxStaticPkgs.stdenv.cc}/bin/aarch64-unknown-linux-musl-g++";
    }
  );

  # Darwin: set CARGO_BUILD_RUSTFLAGS with +crt-static and system libiconv flags,
  # overriding any value previously set by nix-lib, then rewrite any Nix store
  # libiconv references to /usr/lib so the binary works outside of Nix.
  withDarwinStaticFlags =
    drv:
    drv.overrideAttrs (prev: {
      CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-L/usr/lib -C link-arg=-liconv";

      postInstall =
        lib.optionalString (prev ? postInstall && prev.postInstall != null) prev.postInstall
        + lib.optionalString pkgs.stdenv.isDarwin ''
          for bin in $(find "$out/bin" -type f); do
            linked_iconv=$(otool -L "$bin" | grep "/nix/store/.*libiconv.*dylib" | awk '{print $1}')

            if [ -n "$linked_iconv" ]; then
              echo "Rewriting $bin - found nix libiconv reference: $linked_iconv"
              install_name_tool -change "$linked_iconv" "/usr/lib/libiconv.2.dylib" "$bin"
              echo "Fixed libiconv path"
            else
              echo "Not rewriting $bin - no nix libiconv reference found"
            fi
          done
        '';
    });

  mkGnosisvpnBuildArgs =
    {
      src,
      depsSrc,
      extraCargoArgs ? "",
    }:
    {
      inherit src depsSrc rev;
      # prependPackageName=false: skip the automatic `-p gnosis_vpn` that nix-lib
      # derives from [workspace.metadata.crane] name — it has no matching package
      # since the workspace uses a wildcard `members = ["gnosis_vpn*"]`.
      # The --bin flags below are sufficient to select the right binaries.
      prependPackageName = false;
      cargoExtraArgs = "--bin gnosis_vpn-root --bin gnosis_vpn-worker --bin gnosis_vpn-ctl ${extraCargoArgs}";
      cargoToml = ../Cargo.toml;
      extraBuildInputs = linuxExtraBuildInputs;
    };
in
{
  # Local builds

  # binary-gnosis_vpn (renamed from gnosis_vpn-release)
  binary-gnosis_vpn = builders.local.callPackage nixLib.mkRustPackage (mkGnosisvpnBuildArgs {
    src = sources.main;
    depsSrc = sources.deps;
  });

  # binary-gnosis_vpn-dev (renamed from gnosis_vpn-dev)
  binary-gnosis_vpn-dev = builders.local.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgs {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      CARGO_PROFILE = "dev";
    }
  );

  # Cross-compiled — x86_64 Linux
  binary-gnosis_vpn-x86_64-linux = withX86_64LinuxStaticEnv (
    builders.x86_64-linux.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        extraBuildInputs = mkLinuxStaticBuildInputs x86_64LinuxStaticPkgs;
      }
    )
  );

  binary-gnosis_vpn-x86_64-linux-dev = withX86_64LinuxStaticEnv (
    builders.x86_64-linux.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        CARGO_PROFILE = "dev";
        extraBuildInputs = mkLinuxStaticBuildInputs x86_64LinuxStaticPkgs;
      }
    )
  );

  # Cross-compiled — aarch64 Linux
  binary-gnosis_vpn-aarch64-linux = withAarch64LinuxStaticEnv (
    builders.aarch64-linux.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        extraBuildInputs = mkLinuxStaticBuildInputs aarch64LinuxStaticPkgs;
      }
    )
  );

  binary-gnosis_vpn-aarch64-linux-dev = withAarch64LinuxStaticEnv (
    builders.aarch64-linux.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        CARGO_PROFILE = "dev";
        extraBuildInputs = mkLinuxStaticBuildInputs aarch64LinuxStaticPkgs;
      }
    )
  );

  # System test package: all service binaries + the system test runner in one derivation.
  # Used by CI to run the system test against a live network in a single nix build command.
  binary-gnosis_vpn-system_tests = builders.local.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgs {
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
    (mkGnosisvpnBuildArgs {
      src = sources.test;
      depsSrc = sources.deps;
    })
    // {
      runTests = true;
    }
  );

  gnosis_vpn-clippy = builders.local.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgs {
      src = sources.main;
      depsSrc = sources.deps;
    })
    // {
      runClippy = true;
    }
  );

  gnosis_vpn-docs = builders.localNightly.callPackage nixLib.mkRustPackage (
    (mkGnosisvpnBuildArgs {
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
  binary-gnosis_vpn-aarch64-darwin = withDarwinStaticFlags (
    builders.aarch64-darwin.callPackage nixLib.mkRustPackage (mkGnosisvpnBuildArgs {
      src = sources.main;
      depsSrc = sources.deps;
    })
  );

  binary-gnosis_vpn-aarch64-darwin-dev = withDarwinStaticFlags (
    builders.aarch64-darwin.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        CARGO_PROFILE = "dev";
      }
    )
  );
}
