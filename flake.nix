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

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };

    # HOPR Nix Library (provides reusable Rust build functions and treefmt config)
    nix-lib = {
      url = "github:hoprnet/nix-lib/v1.1.0";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.rust-overlay.follows = "rust-overlay";
    };

    # Required by nix-lib's treefmt module for project root detection
    flake-root.url = "github:srid/flake-root";
  };

  outputs =
    inputs@{
      self,
      flake-parts,
      nixpkgs,
      rust-overlay,
      crane,
      advisory-db,
      pre-commit,
      nix-lib,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.nix-lib.flakeModules.default
        inputs.flake-root.flakeModule
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

          gnosisvpnPackages = import ./nix/gnosisvpn.nix {
            inherit lib nixLib self craneLib advisory-db;
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
            excludes = [ ];
          };

          treefmt = {
            settings.global.excludes = [
              "LICENSE"
              "LATEST"
              "target/*"
              "modules/*"
            ];

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
        in
        {
          inherit treefmt;

          checks = {
            inherit (gnosisvpnPackages)
              binary-gnosis_vpn-dev
              gnosis_vpn-clippy
              gnosis_vpn-docs
              gnosis_vpn-test
              gnosis_vpn-audit
              gnosis_vpn-licenses
              ;
          };

          packages = {
            binary-gnosis_vpn = gnosisvpnPackages.binary-gnosis_vpn;
            binary-gnosis_vpn-dev = gnosisvpnPackages.binary-gnosis_vpn-dev;
            inherit pre-commit-check;
            default = gnosisvpnPackages.binary-gnosis_vpn;
          };

          devShells.default = craneLib.devShell (
            {
              inherit pre-commit-check;
              checks = self.checks.${system};

              packages = [
                pkgs.cargo-machete
                pkgs.cargo-shear
                pkgs.rust-analyzer
              ];

              VERGEN_GIT_SHA = toString (self.shortRev or self.dirtyShortRev);
            }
            // lib.optionalAttrs pkgs.stdenv.isLinux {
              # Point mnl-sys and nftnl-sys directly to static library dirs,
              # bypassing pkg-config which can fail in cross-compilation contexts
              LIBMNL_LIB_DIR = "${pkgs.pkgsStatic.libmnl}/lib";
              LIBNFTNL_LIB_DIR = "${pkgs.pkgsStatic.libnftnl}/lib";
            }
          );

          formatter = config.treefmt.build.wrapper;
        };
      flake = { };
    };
}
