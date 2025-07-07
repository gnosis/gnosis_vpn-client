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
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        # To import a flake module
        # 1. Add foo to inputs
        # 2. Add foo as a parameter to the outputs function
        # 3. Add here: foo.flakeModule

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

          systemTargets = {
            "x86_64-linux" = "x86_64-unknown-linux-musl";
            "aarch64-linux" = "aarch64-unknown-linux-musl";
            "x86_64-darwin" = "x86_64-apple-darwin";
            "aarch64-darwin" = "aarch64-apple-darwin";
          };

          targetForSystem = builtins.getAttr system systemTargets;

          # NB: we don't need to overlay our custom toolchain for the *entire*
          # pkgs (which would require rebuidling anything else which uses rust).
          # Instead, we just want to update the scope that crane will use by appending
          # our specific toolchain there.
          # cross = pkgs.pkgsCross.musl64;
          craneLib = (crane.mkLib pkgs).overrideToolchain (
            p:
            (p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
              targets = [ targetForSystem ];
            }
          );

          src = craneLib.cleanCargoSource ./.;

          # Common arguments can be set here to avoid repeating them later
          commonArgs = {
            inherit src;
            strictDeps = true;

            nativeBuildInputs =
              [ pkgs.pkg-config ]
              ++ lib.optionals pkgs.stdenv.isLinux [
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

          srcFiles = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              (craneLib.fileset.commonCargoSources ./gnosis_vpn-lib)
              (craneLib.fileset.commonCargoSources ./gnosis_vpn-ctl)
              (craneLib.fileset.commonCargoSources ./gnosis_vpn)
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

          gvpn = craneLib.buildPackage (
            individualCrateArgs
            // (builtins.getAttr targetForSystem targetCrateArgs)
            // {
              pname = "gnosis_vpn";
              cargoExtraArgs = "--all";
              src = srcFiles;
            }
          );

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
            programs.prettier.enable = true;
            settings.formatter.prettier.excludes = [
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
          # Per-system attributes can be defined here. The self' and inputs'
          # module parameters provide easy access to attributes of the same
          # system.

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

          };

          # Equivalent to  inputs'.nixpkgs.legacyPackages.hello;
          packages = {
            inherit gvpn;
            default = gvpn;
          };

          devShells.default = craneLib.devShell {
            # Inherit inputs from checks.
            checks = self.checks.${system};
            # Additional dev-shell environment variables can be set directly
            # MY_CUSTOM_DEVELOPMENT_VAR = "something else";

            # Extra inputs can be added here; cargo and rustc are provided by default.
            packages = [ ];
          };

          treefmt = treefmt;
          formatter = config.treefmt.build.wrapper;
        };
      flake = {
        # The usual flake attributes can be defined here, including system-
        # agnostic ones like nixosModule and system-enumerating ones, although
        # those are more easily expressed in perSystem.

      };
    };
}
