{
  description = "Gnosis VPN client service";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
    };
    crane = {
      url = "github:ipetkov/crane";
    };

    pre-commit.url = "github:cachix/git-hooks.nix";
    pre-commit.inputs.nixpkgs.follows = "nixpkgs";

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs =
    inputs@{
      self,
      flake-parts,
      nixpkgs,
      rust-overlay,
      crane,
      advisory-db,
      treefmt-nix,
      pre-commit,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ treefmt-nix.flakeModule ];
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      perSystem =
        {
          config,
          self',
          inputs',
          lib,
          system,
          ...
        }:
        let
          pkgs = (
            import nixpkgs {
              localSystem = system;
              overlays = [ (import rust-overlay) ];
            }
          );

          # use for statically linked musl libraries on linux
          staticPkgs = pkgs.pkgsStatic;

          # Map Nix system names to Rust target triples
          # Linux targets use musl for static linking, Darwin uses standard targets
          systemTargets = {
            "x86_64-linux" = "x86_64-unknown-linux-musl";
            "aarch64-linux" = "aarch64-unknown-linux-musl";
            "x86_64-darwin" = "x86_64-apple-darwin";
            "aarch64-darwin" = "aarch64-apple-darwin";
          };

          # Map current system to its corresponding Rust target triple
          # This ensures we build for the correct architecture and platform
          targetForSystem = builtins.getAttr system systemTargets;

          # Target-specific build arguments
          # Each target triple has its own compiler flags and build configuration.
          # - Linux targets use musl for static linking and mold for faster linking
          # - Darwin targets use different profiles based on architecture (intelmac for x86_64)
          # - All targets enable crt-static for standalone binaries
          targetCrateArgs = {
            "x86_64-unknown-linux-musl" = {
              CARGO_PROFILE = "release";
              CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
              CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
              # Add musl-specific configuration for C dependencies (SQLite, etc.)
              # Disable Nix hardening features that are incompatible with musl
              hardeningDisable = [ "fortify" ];
              # Tell libsqlite3-sys to use pkg-config to find system SQLite instead of building from source
              LIBSQLITE3_SYS_USE_PKG_CONFIG = "1";

              # Use the musl-gcc linker from the staticPkgs overlay
              CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${staticPkgs.stdenv.cc}/bin/x86_64-unknown-linux-musl-gcc";

              # Variables for cc-rs (C compilations)
              CC_x86_64_unknown_linux_musl = "${staticPkgs.stdenv.cc}/bin/x86_64-unknown-linux-musl-gcc";
              CXX_x86_64_unknown_linux_musl = "${staticPkgs.stdenv.cc}/bin/x86_64-unknown-linux-musl-g++";

              # poing pkg-config to static libraries
              PKG_CONFIG_PATH = "${staticPkgs.openssl.dev}/lib/pkgconfig:${staticPkgs.sqlite.dev}/lib/pkgconfig:${staticPkgs.libmnl}/lib/pkgconfig:${staticPkgs.libnftnl}/lib/pkgconfig";
            };
            "aarch64-unknown-linux-musl" = {
              CARGO_PROFILE = "release";
              CARGO_BUILD_TARGET = "aarch64-unknown-linux-musl";
              CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
              # Add musl-specific configuration for C dependencies (SQLite, etc.)
              # Disable Nix hardening features that are incompatible with musl
              hardeningDisable = [ "fortify" ];
              # Tell libsqlite3-sys to use pkg-config to find system SQLite instead of building from source
              LIBSQLITE3_SYS_USE_PKG_CONFIG = "1";

              # Use the musl-gcc linker from the staticPkgs overlay
              CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "${staticPkgs.stdenv.cc}/bin/aarch64-unknown-linux-musl-gcc";

              # Variables for cc-rs (C compilations)
              CC_aarch64_unknown_linux_musl = "${staticPkgs.stdenv.cc}/bin/aarch64-unknown-linux-musl-gcc";
              CXX_aarch64_unknown_linux_musl = "${staticPkgs.stdenv.cc}/bin/aarch64-unknown-linux-musl-g++";

              # poing pkg-config to static libraries
              PKG_CONFIG_PATH = "${staticPkgs.openssl.dev}/lib/pkgconfig:${staticPkgs.sqlite.dev}/lib/pkgconfig:${staticPkgs.libmnl}/lib/pkgconfig:${staticPkgs.libnftnl}/lib/pkgconfig";
            };
            "x86_64-apple-darwin" = {
              CARGO_PROFILE = "intelmac";
              # force libiconv from macos lib folder
              CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-L/usr/lib -C link-arg=-liconv";
            };
            "aarch64-apple-darwin" = {
              CARGO_PROFILE = "release";
              # force libiconv from macos lib folder
              CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-L/usr/lib -C link-arg=-liconv";
            };
          };

          crateArgsForTarget = builtins.getAttr targetForSystem targetCrateArgs;

          # Configure crane with custom Rust toolchain
          # We don't overlay the custom toolchain for the *entire* pkgs (which
          # would require rebuilding anything else that uses rust). Instead, we
          # just update the scope that crane will use by appending our specific
          # toolchain there.
          craneLib = (crane.mkLib pkgs).overrideToolchain (
            p:
            (p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
              targets = [ targetForSystem ];
            }
          );

          # Clean cargo source to exclude build artifacts and unnecessary files
          srcFiles = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./.cargo/config.toml
              ./Cargo.toml
              ./Cargo.lock
              ./deny.toml
              (craneLib.fileset.commonCargoSources ./gnosis_vpn-lib)
              (craneLib.fileset.commonCargoSources ./gnosis_vpn-ctl)
              (craneLib.fileset.commonCargoSources ./gnosis_vpn-root)
              (craneLib.fileset.commonCargoSources ./gnosis_vpn-worker)
              ./rustfmt.toml
              ./rust-toolchain.toml
              ./taplo.toml
            ];
          };

          # Common build arguments shared across all crane operations
          # These are used for dependency building, clippy, docs, tests, etc.
          # CARGO_PROFILE is part of the commonArgs to make depsonly build match the target derivations
          commonArgsRelease = {
            src = srcFiles;
            strictDeps = true; # Enforce strict separation of build-time and runtime dependencies

            # Build-time dependencies (available during compilation)
            nativeBuildInputs = [
              pkgs.pkg-config # For finding OpenSSL and other system libraries
              pkgs.cmake
            ]
            ++ lib.optionals pkgs.stdenv.isLinux [
              staticPkgs.stdenv.cc
            ]
            ++ lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
            ];

            # Runtime dependencies (linked into the final binary)
            buildInputs =
              if pkgs.stdenv.isLinux then
                [
                  staticPkgs.openssl
                  staticPkgs.sqlite
                  staticPkgs.libmnl
                  staticPkgs.libnftnl
                ]
              else
                [
                  pkgs.openssl
                  pkgs.sqlite
                ];
          }
          // crateArgsForTarget;

          commonArgsDev = commonArgsRelease // {
            CARGO_PROFILE = "dev";
          };

          # Build *just* the cargo dependencies (of the entire workspace)
          # This creates a separate derivation containing only compiled dependencies,
          # allowing us to cache and reuse them across all packages (via cachix in CI).
          cargoArtifacts-release = craneLib.buildDepsOnly (
            commonArgsRelease
            // {
              doCheck = false; # Disable tests for deps-only build
            }
          );
          cargoArtifacts-dev = craneLib.buildDepsOnly (
            commonArgsDev
            // {
              doCheck = false; # Disable tests for deps-only build
            }
          );

          # Import the package builder function from nix/mkPackage.nix
          # This function encapsulates all the logic for building gnosis_vpn packages
          # with consistent source files, target configurations, and build settings.
          # See nix/mkPackage.nix for detailed documentation on the builder function.
          # Production build with release profile optimizations
          gnosis_vpn-release = import ./nix/mkPackage.nix {
            inherit
              craneLib
              lib
              pkgs
              ;
            inherit (craneLib.crateNameFromCargoToml { src = srcFiles; }) version;
            pname = "gnosis_vpn";
            commonArgs = commonArgsRelease;
            cargoArtifacts = cargoArtifacts-release;
          };

          # Development build with faster compilation and debug symbols
          # Override release profile set in commonArgs with "dev" profile
          gnosis_vpn-dev = import ./nix/mkPackage.nix {
            inherit
              craneLib
              lib
              pkgs
              ;
            inherit (craneLib.crateNameFromCargoToml { src = srcFiles; }) version;
            pname = "gnosis_vpn-dev";
            commonArgs = commonArgsDev;
            cargoArtifacts = cargoArtifacts-dev;
          };

          pre-commit-check = pre-commit.lib.${system}.run {
            src = ./.;
            hooks = {
              # https://github.com/cachix/git-hooks.nix
              treefmt.enable = false;
              treefmt.package = config.treefmt.build.wrapper;
              check-executables-have-shebangs.enable = true;
              check-shebang-scripts-are-executable.enable = true;
              check-case-conflicts.enable = true;
              check-symlinks.enable = true;
              check-merge-conflicts.enable = true;
              check-added-large-files.enable = true;
              commitizen.enable = true;
            };
            tools = pkgs;
            excludes = [
            ];
          };

          treefmt = {
            projectRootFile = "LICENSE";

            settings.global.excludes = [
              "LICENSE"
              "LATEST"
              "target/*"
              "modules/*"
            ];

            programs.nixfmt.enable = pkgs.lib.meta.availableOn pkgs.stdenv.buildPlatform pkgs.nixfmt.compiler;
            programs.deno.enable = true;
            settings.formatter.deno.excludes = [
              "*.toml"
              "*.yml"
              "*.yaml"
            ];
            programs.rustfmt.enable = true;
            programs.shellcheck.enable = true;
            programs.shfmt = {
              enable = true;
              indent_size = 4;
            };
            programs.taplo.enable = true; # TOML formatter
            programs.yamlfmt.enable = true;
            # trying setting from https://github.com/google/yamlfmt/blob/main/docs/config-file.md
            settings.formatter.yamlfmt.settings = {
              formatter.type = "basic";
              formatter.max_line_length = 120;
              formatter.trim_trailing_whitespace = true;
              formatter.include_document_start = true;
            };
          };
          generate-lockfile = {
            type = "app";
            program = toString (
              pkgs.writeShellScript "generate-lockfile" ''
                export PATH="${craneLib.rustc}/bin:$PATH"
                exec cargo generate-lockfile "$@"
              ''
            );
            meta.description = "Generate Cargo.lock with minimal dependencies (Rust toolchain only)";
          };
        in
        {
          inherit treefmt;

          checks = {
            # Build the dev crates as part of `nix flake check` for convenience
            inherit gnosis_vpn-dev;

            # Run clippy (and deny all warnings) on the workspace source,
            # again, reusing the dependency artifacts from above.
            #
            # Note that this is done as a separate derivation so that
            # we can block the CI if there are issues here, but not
            # prevent downstream consumers from building our crate by itself.
            clippy = craneLib.cargoClippy (
              commonArgsDev
              // {
                cargoClippyExtraArgs = "--all-targets -- --deny warnings";
                cargoArtifacts = cargoArtifacts-dev;
              }
            );

            docs = craneLib.cargoDoc (
              commonArgsDev
              // {
                cargoArtifacts = cargoArtifacts-dev;
              }
            );

            # Audit dependencies
            audit = craneLib.cargoAudit {
              src = srcFiles;
              inherit advisory-db;
              # Ignore RSA vulnerability (RUSTSEC-2023-0071) - comes from hopr-lib transitive dependency
              cargoAuditExtraArgs = "--ignore RUSTSEC-2023-0071";
            };

            # Audit licenses
            licenses = craneLib.cargoDeny {
              src = srcFiles;
            };

            # Run tests with cargo-nextest
            # Consider setting `doCheck = false` on other crate derivations
            # if you do not want the tests to run twice
            test = craneLib.cargoNextest (
              commonArgsDev
              // {
                cargoArtifacts = cargoArtifacts-dev;
                partitions = 1;
                partitionType = "count";
                cargoNextestPartitionsExtraArgs = "--no-tests=pass";
              }
            );

          };

          packages = {
            gnosis_vpn = gnosis_vpn-release;
            inherit gnosis_vpn-dev;
            inherit pre-commit-check;
            default = gnosis_vpn-release;
          };

          apps = {
            inherit generate-lockfile;
          };

          devShells.default = craneLib.devShell {
            inherit pre-commit-check;
            checks = self.checks.${system};

            packages = [
              pkgs.cargo-machete
              pkgs.cargo-shear
              pkgs.rust-analyzer
            ];

            VERGEN_GIT_SHA = toString (self.shortRev or self.dirtyShortRev);
          };

          formatter = config.treefmt.build.wrapper;
        };
      flake = { };
    };
}
