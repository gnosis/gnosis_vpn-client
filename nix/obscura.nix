# obscura, the headless browser the network-baseline harness drives over CDP.
# Not packaged in nixpkgs, so it comes from the upstream release, pinned per platform.
{ lib, pkgs }:
let
  version = "0.2.1";

  assets = {
    aarch64-darwin = {
      file = "obscura-aarch64-macos.tar.gz";
      hash = "sha256-UjPaZCbsFmZ9fkN0uCQYnG37OzJeXPP7XwTHvEi1Kg8=";
    };
    x86_64-linux = {
      file = "obscura-x86_64-linux.tar.gz";
      hash = "sha256-ahpms/GrEY+n0xMwiUqGhheupowG11Q22FE1bDnfHtM=";
    };
    aarch64-linux = {
      file = "obscura-aarch64-linux.tar.gz";
      hash = "sha256-ApfCbVg/WY8BJqcnHMQHUFmKmpy9htHW95srmQl9UkQ=";
    };
  };

  inherit (pkgs.stdenv.hostPlatform) system;
  asset =
    assets.${system}
      or (throw "obscura: no upstream prebuilt for ${system} (have ${lib.concatStringsSep ", " (lib.attrNames assets)})");
in
pkgs.stdenv.mkDerivation {
  pname = "obscura";
  inherit version;

  src = pkgs.fetchurl {
    url = "https://github.com/h4ckf0r0day/obscura/releases/download/v${version}/${asset.file}";
    inherit (asset) hash;
  };

  # The tarball is two flat binaries, no top-level directory.
  sourceRoot = ".";

  nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
  buildInputs = lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc.cc.lib ];

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    install -m755 obscura $out/bin/obscura
    if [ -f obscura-worker ]; then
      install -m755 obscura-worker $out/bin/obscura-worker
    fi
    runHook postInstall
  '';

  meta = {
    description = "Headless browser driven over CDP by the network-baseline harness";
    homepage = "https://github.com/h4ckf0r0day/obscura";
    platforms = lib.attrNames assets;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
