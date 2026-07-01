# AGENTS.md

## This repo uses Nix

The project is built and developed with **Nix flakes** (see
[flake.nix](flake.nix) and [flake.lock](flake.lock)). The Rust toolchain,
formatters, and dev tooling are all provided through Nix — they are **not**
assumed to be on the system `PATH`.

### Entering the dev environment

A `direnv` config ([.envrc](.envrc)) runs `use flake`, so the dev shell loads
automatically on `cd` into the repo if `direnv` is installed and allowed
(`direnv allow`).

Without `direnv`, enter the shell manually before running `cargo`, `rustc`,
`clippy`, `just`, etc.:

```bash
nix develop
```

> Note: `cargo`/`rustc` are only available inside the Nix dev shell. Running
> them outside it (or in an environment without Nix) will fail with "command not
> found".

The dev shell provides: the pinned Rust toolchain (from `rust-toolchain.toml`),
`rust-analyzer`, `just`, `cargo-machete`, and `cargo-shear`.

### Building

```bash
nix build -L .#binary-gnosis_vpn-x86_64-linux    # static x86_64 linux binary
nix build -L .#binary-gnosis_vpn-aarch64-linux   # static ARM64 linux binary
just build                                        # shortcut for the x86_64 build
nix flake show                                    # list all packages/checks
```

### Checks & formatting

```bash
nix flake check          # runs clippy, tests, docs, cargo-audit, license checks
nix fmt                  # format the tree via treefmt (rustfmt, nixfmt, taplo, prettier, …)
```

Pre-commit hooks (managed via `git-hooks.nix`) run on commit; `commitizen`
enforces conventional commit messages.

Supported systems: `x86_64-linux`, `aarch64-linux`, `aarch64-darwin`.
