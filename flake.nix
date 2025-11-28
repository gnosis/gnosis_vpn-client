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
      imports = [
        treefmt-nix.flakeModule
      ];
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
          fs = lib.fileset;
          pkgs = (
            import nixpkgs {
              localSystem = system;
              crossSystem = system;
              overlays = [ (import rust-overlay) ];
            }
          );
          # import docker utilities for building Docker images
          mkDockerImage = args: import ./nix/docker.nix (args // { inherit pkgs; });

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
          src = craneLib.cleanCargoSource ./.;

          # Common build arguments shared across all crane operations
          # These are used for dependency building, clippy, docs, tests, etc.
          commonArgs = {
            inherit src;
            strictDeps = true; # Enforce strict separation of build-time and runtime dependencies

            # Build-time dependencies (available during compilation)
            nativeBuildInputs = [
              pkgs.pkg-config # For finding OpenSSL and other system libraries
            ]
            ++ lib.optionals pkgs.stdenv.isLinux [
              pkgs.mold # Faster linker for Linux builds
            ];

            # Runtime dependencies (linked into the final binary)
            buildInputs = [
              pkgs.pkgsStatic.openssl # Static OpenSSL for standalone binaries
              pkgs.pkgsStatic.sqlite # Static SQLite for standalone binaries
            ]
            ++ lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv # Required for Darwin builds
            ];
          }
          # Add musl-specific configuration for C dependencies (SQLite, mimalloc, etc.)
          // lib.optionalAttrs (lib.hasInfix "musl" targetForSystem) {
            # Disable Nix hardening features that are incompatible with musl
            hardeningDisable = [ "fortify" ];
            # Tell libsqlite3-sys to use pkg-config to find system SQLite instead of building from source
            LIBSQLITE3_SYS_USE_PKG_CONFIG = "1";
          };

          # Build *just* the cargo dependencies (of the entire workspace)
          # This creates a separate derivation containing only compiled dependencies,
          # allowing us to cache and reuse them across all packages (via cachix in CI).
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          # Import the package builder function from nix/mkPackage.nix
          # This function encapsulates all the logic for building gnosis_vpn packages
          # with consistent source files, target configurations, and build settings.
          # See nix/mkPackage.nix for detailed documentation on the builder function.
          mkPackage = import ./nix/mkPackage.nix {
            inherit
              craneLib
              lib
              targetForSystem
              cargoArtifacts
              pkgs
              commonArgs
              ;
            inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
          };

          getSecretEnv =
            name:
            let
              value = builtins.getEnv name;
            in
            if value == "" then null else value;

          # Build the top-level crates of the workspace as individual derivations
          # This modular approach allows consumers to depend on and build only what
          # they need, rather than building the entire workspace as a single derivation.

          # Production build with release profile optimizations
          gvpn = mkPackage {
            pname = "gnosis_vpn";
            profile = "release";
            cargoExtraArgs = "--bin gnosis_vpn --bin gnosis_vpn-ctl";
          };

          # Development build with faster compilation and debug symbols
          gvpn-dev = mkPackage {
            pname = "gnosis_vpn-dev";
            profile = "dev";
            cargoExtraArgs = "--bin gnosis_vpn --bin gnosis_vpn-ctl";
          };

          gvpn-system-tests = mkPackage {
            pname = "gnosis_vpn-system-tests";
            profile = "release";
            cargoExtraArgs = "--bin gnosis_vpn-system-tests";
          };

          rotseeSource = fs.toSource {
            root = ./.;
            fileset = fs.unions [
              ./network-config/rotsee.toml
            ];
          };

          systemTestsSecretValues = {
            vpnId = getSecretEnv "GNOSIS_VPN_ID";
            vpnPass = getSecretEnv "GNOSIS_VPN_PASS";
            vpnSafe = getSecretEnv "GNOSIS_VPN_SAFE";
          };

          mkGnosisVpnSecrets =
            {
              vpnId,
              vpnPass,
              vpnSafe,
            }:
            pkgs.runCommand "gnosisvpn-secret-files"
              {
                GNOSIS_VPN_ID = vpnId;
                GNOSIS_VPN_PASS = vpnPass;
                GNOSIS_VPN_SAFE = vpnSafe;
              }
              ''
                parent_folder=$out/.config/gnosisvpn
                mkdir -p $parent_folder
                printf %s "$GNOSIS_VPN_ID" > $parent_folder/gnosis_vpn-hopr.id
                printf %s "$GNOSIS_VPN_PASS" > $parent_folder/gnosis_vpn-hopr.pass
                printf %s "$GNOSIS_VPN_SAFE" > $parent_folder/gnosis_vpn-hopr.safe
              '';

          mkSystemTestsDockerImage =
            secrets:
            let
              gnosisVpnSecrets = mkGnosisVpnSecrets secrets;
            in
            mkDockerImage {
              name = "gnosis-vpn-system-tests";
              extraContents = [
                gvpn
                gvpn-system-tests
                pkgs.wireguard-tools
                pkgs.which
              ];
              extraFiles = [
                rotseeSource
                gnosisVpnSecrets
              ];
              extraFilesDest = "/";
              env = [
                "RUST_LOG=gnosis_vpn=info"
                "GNOSISVPN_CONFIG_PATH=/network-config/rotsee.toml"
              ];
              Entrypoint = [
                "gnosis_vpn-system-tests"
                "download"
              ];
            };

          systemTestsDockerPackages =
            let
              secretsProvided =
                systemTestsSecretValues.vpnId != null
                && systemTestsSecretValues.vpnPass != null
                && systemTestsSecretValues.vpnSafe != null;
            in
            lib.optionalAttrs secretsProvided {
              gvpn-system-tests-docker = mkSystemTestsDockerImage systemTestsSecretValues;
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

            programs.nixfmt = {
              enable = pkgs.lib.meta.availableOn pkgs.stdenv.buildPlatform pkgs.nixfmt-rfc-style.compiler;
              package = pkgs.nixfmt-rfc-style;
            };
            programs.deno.enable = true;
            settings.formatter.deno.excludes = [
              "*.toml"
              "*.yml"
              "*.yaml"
            ];
            programs.rustfmt.enable = true;
            settings.formatter.rustfmt = {
              command = "${pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default)}/bin/rustfmt";
            };
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
            inherit gvpn-dev;

            # Run clippy (and deny all warnings) on the workspace source,
            # again, reusing the dependency artifacts from above.
            #
            # Note that this is done as a separate derivation so that
            # we can block the CI if there are issues here, but not
            # prevent downstream consumers from building our crate by itself.
            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- --deny warnings";
              }
            );

            docs = craneLib.cargoDoc (
              commonArgs
              // {
                inherit cargoArtifacts;
              }
            );

            # Audit dependencies
            audit = craneLib.cargoAudit {
              inherit src advisory-db;
              # Ignore RSA vulnerability (RUSTSEC-2023-0071) - comes from hopr-lib transitive dependency
              cargoAuditExtraArgs = "--ignore RUSTSEC-2023-0071";
            };

            # Audit licenses
            licenses = craneLib.cargoDeny {
              inherit src;
            };

            # Run tests with cargo-nextest
            # Consider setting `doCheck = false` on other crate derivations
            # if you do not want the tests to run twice
            test = craneLib.cargoNextest (
              commonArgs
              // {
                inherit cargoArtifacts;
                partitions = 1;
                partitionType = "count";
                cargoNextestPartitionsExtraArgs = "--no-tests=pass";
              }
            );

          };

          packages = {
            inherit gvpn;
            inherit gvpn-dev;
            inherit gvpn-system-tests;
            inherit pre-commit-check;
            default = gvpn;
          }
          // systemTestsDockerPackages;

          devShells.default = craneLib.devShell {
            inherit pre-commit-check;
            checks = self.checks.${system};

            packages = [ ];

            VERGEN_GIT_SHA = toString (self.shortRev or self.dirtyShortRev);
          };

          formatter = config.treefmt.build.wrapper;
        };
      flake = { };
    };
}
