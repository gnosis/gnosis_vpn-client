# docker.nix - Docker image builder utility
#
# Creates layered Docker images with optimized caching and minimal size.
# Provides a consistent base for all container images.
#
# This builder automatically selects between buildImage (for macOS) and
# buildLayeredImage (for Linux) to work around fakeroot issues on recent macOS.

{
  pkgs, # Nixpkgs package set (should be Linux packages for Docker images)
  name, # Name of the Docker image
  Entrypoint, # Container entrypoint script or binary
  Cmd ? [ ], # Default command to run in container
  env ? [ ], # Additional environment variables for the container
  extraContents ? [ ], # Additional packages to include in image
  extraFiles ? [ ], # Additional files to include in image
  extraFilesDest ? "/extra", # Destination inside the image for extra files
  basePackages ? null, # Override base packages (default: bash, cacert, coreutils, etc.)
  tag ? "latest", # Docker image tag
}:
let
  # Library path for essential system libraries
  libPath = pkgs.lib.makeLibraryPath [ pkgs.openssl ];

  # Default base packages included in all Docker images
  # These provide essential runtime dependencies
  defaultBasePackages = with pkgs; [
    bash
    cacert
    coreutils
    dnsutils
    findutils
    iana-etc
    nettools
    util-linux
  ];

  # Use provided base packages or default
  actualBasePackages = if basePackages != null then basePackages else defaultBasePackages;

  # Base packages included in all Docker images
  # These provide essential runtime dependencies
  baseRoot = pkgs.buildEnv {
    name = "image-root";
    paths = actualBasePackages ++ extraContents;
    pathsToLink = [ "/bin" ];
  };

  extraFilesRoot =
    if extraFiles == [ ] then
      null
    else
      pkgs.buildEnv {
        name = "image-extra-files";
        paths = extraFiles;
        pathsToLink = [ "/" ];
      };

  copyToRoot =
    if extraFilesRoot == null then
      baseRoot
    else
      pkgs.runCommand "image-root-with-extra" { } ''
        mkdir -p $out
        cp -R ${baseRoot}/. $out/
        mkdir -p "$out${extraFilesDest}"
        cp -R ${extraFilesRoot}/. "$out${extraFilesDest}/"
      '';


  # Default environment variables
  defaultEnv = [
    "NO_COLOR=true" # suppress colored log output
    "RUST_BACKTRACE=full"
    "LD_LIBRARY_PATH=${libPath}"
  ];

  Env = defaultEnv ++ env;

  # Use buildImage on macOS to avoid fakeroot issues
  # buildLayeredImage requires fakeroot which doesn't work on recent macOS
  buildImageArgs = {
    inherit name tag copyToRoot;
    created = "now";
    config = { inherit Cmd Entrypoint Env; };
  };

  buildLayeredImageArgs = {
    inherit name tag;
    created = "now";
    contents = [ copyToRoot ];
    config = { inherit Cmd Entrypoint Env; };
  };
in
if pkgs.stdenv.isDarwin then
  pkgs.dockerTools.buildImage buildImageArgs
else
  pkgs.dockerTools.buildLayeredImage buildLayeredImageArgs
