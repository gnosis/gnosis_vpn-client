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
          pkgs = (
            import nixpkgs {
              localSystem = system;
              crossSystem = system;
              overlays = [ (import rust-overlay) ];
            }
          );

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
            ]
            ++ lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv # Required for Darwin builds
            ];
          }
          # Add musl-specific CFLAGS for C dependencies (SQLite, mimalloc, etc.)
          # The cc crate used by build scripts also checks TARGET_CFLAGS
          // lib.optionalAttrs (lib.hasInfix "musl" targetForSystem) {
            # Disable Nix hardening features that are incompatible with musl
            hardeningDisable = [ "fortify" "stackprotector" ];

            CFLAGS = "-O3 -ffunction-sections -fdata-sections -fPIC -Dfcntl64=fcntl";
            CPPFLAGS = "-O3 -ffunction-sections -fdata-sections -fPIC -Dfcntl64=fcntl";
            TARGET_CFLAGS = "-O3 -ffunction-sections -fdata-sections -fPIC -Dfcntl64=fcntl";
            "CFLAGS_${lib.replaceStrings ["-"] ["_"] targetForSystem}" = "-O3 -ffunction-sections -fdata-sections -fPIC -Dfcntl64=fcntl";
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

          # Build the top-level crates of the workspace as individual derivations
          # This modular approach allows consumers to depend on and build only what
          # they need, rather than building the entire workspace as a single derivation.

          # Production build with release profile optimizations
          gvpn = mkPackage {
            pname = "gnosis_vpn";
            profile = "release";
          };

          # Development build with faster compilation and debug symbols
          gvpn-dev = mkPackage {
            pname = "gnosis_vpn-dev";
            profile = "dev";
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

        in
        {
          inherit treefmt;

          checks = {
            # Build the crates as part of `nix flake check` for convenience
            inherit gvpn;

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
            inherit pre-commit-check;
            default = gvpn;
          };

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
