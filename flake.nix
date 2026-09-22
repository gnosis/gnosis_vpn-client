{
  description = "Gnosis VPN client service";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
    };
    # crane's repo history is huge (500k+ objects); the plain github: fetcher
    # falls back to a full unshallow git clone of it on every job. Fetching
    # it as a shallow git input instead keeps this to a few MiB.
    crane = {
      url = "git+https://github.com/ipetkov/crane.git?shallow=1";
    };

    pre-commit.url = "github:cachix/git-hooks.nix";
    pre-commit.inputs.nixpkgs.follows = "nixpkgs";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    nix-lib = {
      url = "github:hoprnet/nix-lib/1409f8caa2666afcf575dd5e05d5a8c521f5c1d6";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.rust-overlay.follows = "rust-overlay";
    };
  };

  outputs =
    inputs@{
      self,
      flake-parts,
      nixpkgs,
      rust-overlay,
      crane,
      pre-commit,
      nix-lib,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.nix-lib.flakeModules.default
      ];
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
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
          pkgs = import nixpkgs {
            localSystem = system;
            overlays = [ (import rust-overlay) ];
          };

          nixLib = nix-lib.lib.${system};

          craneLib = (crane.mkLib pkgs).overrideToolchain (
            p:
            (p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
              targets = [ ];
            }
          );

          # Build with `--cfg tokio_unstable` so Tokio's improved cooperative
          # yielding is active (the config that produced the validated throughput).
          # nix-lib's shells/builds set CARGO_BUILD_RUSTFLAGS (linker flag), which
          # *replaces* `.cargo/config.toml`'s `[build]` table, so append the tokio
          # flags there to keep them alongside the linker flag. `--check-cfg` keeps
          # the flag from tripping `-D warnings`.
          tokioUnstableHook = ''
            export CARGO_BUILD_RUSTFLAGS="''${CARGO_BUILD_RUSTFLAGS:-} --cfg tokio_unstable --check-cfg cfg(tokio_unstable)"
          '';

          gnosisvpnPackages = import ./nix/gnosisvpn.nix {
            inherit
              lib
              nixLib
              self
              pkgs
              craneLib
              tokioUnstableHook
              ;
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
              # commitizen 4.13.9 fails to build against Python 3.14 in this nixpkgs revision.
              commitizen.enable = false;
            };
            tools = pkgs;
            excludes = [ ];
          };

        in
        {
          # nix-lib's flake module sets up treefmt and formatter automatically.
          # Use nix-lib.treefmt to extend it with project-specific settings.
          # nix-lib already covers: rustfmt, nixfmt, taplo, yamlfmt, shfmt, prettier, ruff-format.
          nix-lib.treefmt = {
            projectRootFile = "LICENSE";
            globalExcludes = [
              "modules/*"
            ];
            extraFormatters = {
              programs.deno.enable = true;
              settings.formatter.deno.excludes = [
                "*.md"
                "*.toml"
                "*.yml"
                "*.yaml"
              ];
              programs.shellcheck.enable = true;
              programs.shfmt.indent_size = 4;
              programs.nixfmt.enable = true;
            };
          };

          checks = {
            inherit (gnosisvpnPackages)
              gnosis_vpn-clippy
              gnosis_vpn-docs
              gnosis_vpn-test
              gnosis_vpn-licenses
              ;
          };

          apps.audit = {
            type = "app";
            program = "${
              pkgs.writeShellApplication {
                name = "audit";
                runtimeInputs = [
                  pkgs.git
                  ((pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
                    targets = [ ];
                  })
                  pkgs.cargo-audit
                ];
                text = ''
                  repo_root="${1:-$PWD}"
                  if repo_git_root="$(git -C "$repo_root" rev-parse --show-toplevel 2>/dev/null)"; then
                    repo_root="$repo_git_root"
                  fi
                  if [ ! -f "$repo_root/Cargo.toml" ] || [ ! -f "$repo_root/.cargo/audit.toml" ]; then
                    echo "run from the repo root, any repo subdir, or pass the repo root" >&2
                    exit 1
                  fi

                  cd "$repo_root"
                  db_dir="${CARGO_HOME:-$HOME/.cargo}/advisory-db"
                  cargo audit fetch --db "$db_dir"
                  cargo audit -n --db "$db_dir" --config .cargo/audit.toml --file "$repo_root/Cargo.lock"
                '';
              }
            }/bin/audit";
          };

          packages = {
            inherit (gnosisvpnPackages)
              binary-gnosis_vpn
              binary-gnosis_vpn-dev
              binary-gnosis_vpn-x86_64-linux
              binary-gnosis_vpn-x86_64-linux-dev
              binary-gnosis_vpn-aarch64-linux
              binary-gnosis_vpn-aarch64-linux-dev
              binary-gnosis_vpn-system_tests
              ;
            inherit pre-commit-check;
            default = gnosisvpnPackages.binary-gnosis_vpn;
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
            inherit (gnosisvpnPackages)
              binary-gnosis_vpn-aarch64-darwin
              binary-gnosis_vpn-aarch64-darwin-dev
              ;
          };

          devShells.default = craneLib.devShell (
            {
              inherit pre-commit-check;
              checks = self.checks.${system};

              # Keep `--cfg tokio_unstable` on interactive `cargo` invocations too.
              shellHook = tokioUnstableHook;

              packages = [
                pkgs.cargo-machete
                pkgs.cargo-shear
                pkgs.jq
                pkgs.just
                pkgs.nix-prefetch-git
                pkgs.rust-analyzer
              ]
              ++ lib.attrValues config.treefmt.build.programs;

              VERGEN_GIT_SHA = toString (self.shortRev or self.dirtyShortRev);
            }
            // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              # Point mnl-sys and nftnl-sys directly to static library dirs,
              # bypassing pkg-config which can fail in cross-compilation contexts
              LIBMNL_LIB_DIR = "${pkgs.pkgsStatic.libmnl}/lib";
              LIBNFTNL_LIB_DIR = "${pkgs.pkgsStatic.libnftnl}/lib";
            }
          );

        };
      flake = { };
    };
}
