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
  # Shell snippet appending `--cfg tokio_unstable` to CARGO_BUILD_RUSTFLAGS.
  tokioUnstableHook,
}:

let
  fs = lib.fileset;
  rev = toString (self.shortRev or self.dirtyShortRev);

  # Cargo.lock pins these as bare `?rev=<sha>` git sources (no branch/tag), so
  # crane's default vendoring can't shallow-fetch them — it has to fetch every
  # ref (all branches, tags, and PRs) of the whole repo just to locate the sha,
  # which takes ~10 minutes for the huge hoprnet/hoprnet monorepo. Providing the
  # output hash up front switches crane to a targeted `fetchgit` of just that
  # commit instead, which is also substitutable from the cachix cache.
  # Update these whenever Cargo.lock's rev for the corresponding dependency changes.
  outputHashes = {
    "git+https://github.com/NordSecurity/neptun.git?tag=v3.0.1#a45a31d28780695cb816d5e245a70bd520bd1293" =
      "sha256-QF8BAe2eHrGsVvZIGD+Gdi6XFMQ8zHkOObSRjTRN4MY=";
    "git+https://github.com/hoprnet/edge-client.git?rev=917636aad2eae827542667b1e81793e71d81a324#917636aad2eae827542667b1e81793e71d81a324" =
      "sha256-m5oC/er9iDnpzo/jj0MTI5ktYSa88a+PXTLOucYwipM=";
    "git+https://github.com/hoprnet/hoprnet?branch=release/4.0#08d777e442c251bbd2dc55e36434dbd4999b0165" =
      "sha256-/vIIwu61MXdOQEYQmwkOTA6VoDjZ4afh7Vn4/G3XRno=";
  };

  builders = nixLib.mkRustBuilders {
    rustToolchainFile = ../rust-toolchain.toml;
  };

  sources = {
    main = nixLib.mkSrc {
      inherit fs;
      root = ../.;
      extraFiles = [
        ../gnosisvpn-public-key.asc
      ];
    };
    test = nixLib.mkTestSrc {
      inherit fs;
      root = ../.;
      extraFiles = [
        ../gnosisvpn-public-key.asc
      ];
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

  # Flags that activate Tokio's improved cooperative yielding (the config that
  # produced the validated multi-hop throughput). `--check-cfg` keeps the cfg
  # from tripping `-D warnings`.
  tokioUnstableRustflags = "--cfg tokio_unstable --check-cfg cfg(tokio_unstable)";

  # Appends the tokio_unstable flags to CARGO_BUILD_RUSTFLAGS on BOTH the package
  # and its cargoArtifacts (deps-only cache). nix-lib's rust-builder.nix sets
  # CARGO_BUILD_RUSTFLAGS as a derivation env var (linker flag / +crt-static),
  # which *replaces* `.cargo/config.toml`'s `[build] rustflags` — so the tokio
  # flags must be appended here to reach rustc for nix package builds. Reading
  # `prev.CARGO_BUILD_RUSTFLAGS` preserves nix-lib's linker/static flags and works
  # across local, cross-Linux, and Darwin builders. Applied as the innermost
  # wrapper so deps and final stay consistent regardless of outer wrappers.
  withTokioUnstable =
    drv:
    let
      append = prev: "${prev.CARGO_BUILD_RUSTFLAGS or ""} ${tokioUnstableRustflags}";
    in
    drv.overrideAttrs (
      prev:
      {
        CARGO_BUILD_RUSTFLAGS = append prev;
      }
      // {
        cargoArtifacts =
          if prev.cargoArtifacts != null then
            prev.cargoArtifacts.overrideAttrs (depsPrev: {
              CARGO_BUILD_RUSTFLAGS = append depsPrev;
            })
          else
            null;
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
      # Append any flags already on the final derivation (notably the
      # tokio_unstable cfg stamped by withTokioUnstable) so replacing the base
      # flags here does not drop them.
      CARGO_BUILD_RUSTFLAGS =
        "-C target-feature=+crt-static -C link-arg=-L/usr/lib -C link-arg=-liconv"
        + lib.optionalString (prev ? CARGO_BUILD_RUSTFLAGS) " ${prev.CARGO_BUILD_RUSTFLAGS}";

      postInstall =
        lib.optionalString (prev ? postInstall && prev.postInstall != null) prev.postInstall
        + ''
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
      inherit
        src
        depsSrc
        rev
        outputHashes
        ;
      # prependPackageName=false: skip the automatic `-p gnosis_vpn` that nix-lib
      # derives from [workspace.metadata.crane] name — it has no matching package
      # since the workspace uses a wildcard `members = ["gnosis_vpn*"]`.
      # The --bin flags below are sufficient to select the right binaries.
      prependPackageName = false;
      cargoExtraArgs = "--bin gnosis_vpn-root --bin gnosis_vpn-worker --bin gnosis_vpn-ctl ${extraCargoArgs}";
      cargoToml = ../Cargo.toml;
      extraBuildInputs = linuxExtraBuildInputs;
    };

  # Adds bash/fish/zsh completions by running the built binary in postInstall.
  # Only applied to native (local) builds — cross-compiled binaries can't be executed on the host.
  withShellCompletions =
    drv:
    drv.overrideAttrs (prev: {
      nativeBuildInputs = (prev.nativeBuildInputs or [ ]) ++ [ pkgs.installShellFiles ];
      postInstall =
        lib.optionalString (prev ? postInstall && prev.postInstall != null) prev.postInstall
        + ''
          installShellCompletion --cmd gnosis_vpn-ctl \
            --bash <($out/bin/gnosis_vpn-ctl completions bash) \
            --fish <($out/bin/gnosis_vpn-ctl completions fish) \
            --zsh  <($out/bin/gnosis_vpn-ctl completions zsh)
        '';
    });
in
{
  # Local builds

  # binary-gnosis_vpn (renamed from gnosis_vpn-release)
  binary-gnosis_vpn = withShellCompletions (
    withTokioUnstable (
      builders.local.callPackage nixLib.mkRustPackage (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
    )
  );

  # binary-gnosis_vpn-dev (renamed from gnosis_vpn-dev)
  binary-gnosis_vpn-dev = withShellCompletions (
    withTokioUnstable (
      builders.local.callPackage nixLib.mkRustPackage (
        (mkGnosisvpnBuildArgs {
          src = sources.main;
          depsSrc = sources.deps;
        })
        // {
          CARGO_PROFILE = "dev";
        }
      )
    )
  );

  # Cross-compiled — x86_64 Linux
  binary-gnosis_vpn-x86_64-linux = withX86_64LinuxStaticEnv (
    withTokioUnstable (
      builders.x86_64-linux.callPackage nixLib.mkRustPackage (
        (mkGnosisvpnBuildArgs {
          src = sources.main;
          depsSrc = sources.deps;
        })
        // {
          extraBuildInputs = mkLinuxStaticBuildInputs x86_64LinuxStaticPkgs;
        }
      )
    )
  );

  binary-gnosis_vpn-x86_64-linux-dev = withX86_64LinuxStaticEnv (
    withTokioUnstable (
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
    )
  );

  # Cross-compiled — aarch64 Linux
  binary-gnosis_vpn-aarch64-linux = withAarch64LinuxStaticEnv (
    withTokioUnstable (
      builders.aarch64-linux.callPackage nixLib.mkRustPackage (
        (mkGnosisvpnBuildArgs {
          src = sources.main;
          depsSrc = sources.deps;
        })
        // {
          extraBuildInputs = mkLinuxStaticBuildInputs aarch64LinuxStaticPkgs;
        }
      )
    )
  );

  binary-gnosis_vpn-aarch64-linux-dev = withAarch64LinuxStaticEnv (
    withTokioUnstable (
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
    )
  );

  # System test runner binary only. CI builds this alongside the already-built
  # binary-gnosis_vpn-x86_64-linux and runs system tests against that release
  # artifact directly, instead of recompiling root/worker/ctl a second time
  # here just to have a worker binary to test against.
  binary-gnosis_vpn-system_tests = withTokioUnstable (
    builders.local.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        cargoExtraArgs = "--bin gnosis_vpn-system_tests";
      }
    )
  );

  # Tests / QA
  gnosis_vpn-test = withTokioUnstable (
    builders.local.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.test;
        depsSrc = sources.deps;
      })
      // {
        runTests = true;
      }
    )
  );

  gnosis_vpn-clippy = withTokioUnstable (
    builders.local.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        runClippy = true;
      }
    )
  );

  gnosis_vpn-docs = withTokioUnstable (
    builders.localNightly.callPackage nixLib.mkRustPackage (
      (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
      // {
        buildDocs = true;
      }
    )
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
    withTokioUnstable (
      builders.aarch64-darwin.callPackage nixLib.mkRustPackage (mkGnosisvpnBuildArgs {
        src = sources.main;
        depsSrc = sources.deps;
      })
    )
  );

  binary-gnosis_vpn-aarch64-darwin-dev = withDarwinStaticFlags (
    withTokioUnstable (
      builders.aarch64-darwin.callPackage nixLib.mkRustPackage (
        (mkGnosisvpnBuildArgs {
          src = sources.main;
          depsSrc = sources.deps;
        })
        // {
          CARGO_PROFILE = "dev";
        }
      )
    )
  );
}
