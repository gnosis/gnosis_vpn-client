{
  buildDocs ? false,
  CARGO_PROFILE ? "release",
  cargoExtraArgs ? "",
  cargoToml,
  craneLib,
  depsSrc,
  html-tidy,
  isCross,
  llvmPackages,
  lib,
  libiconv,
  makeSetupHook,
  pandoc,
  pkg-config,
  pkgs,
  pkgsStatic,
  postInstall ? null,
  rev,
  runClippy ? false,
  runTests ? false,
  src,
  stdenv,
}:
let
  # `hostPlatform` is the cross-compilation output platform;
  # `buildPlatform` is the platform we are compiling on
  buildPlatform = stdenv.buildPlatform;
  hostPlatform = stdenv.hostPlatform;

  # when building for Linux amd64 use musl to build static binaries
  useMusl = hostPlatform.config == "x86_64-unknown-linux-gnu";

  pkgsStatOrDyn = if useMusl then pkgsStatic else pkgs;

  targetInterpreter =
    if hostPlatform.isLinux && hostPlatform.isx86_64 then
      "/lib64/ld-linux-x86-64.so.2"
    else if hostPlatform.isLinux && hostPlatform.isAarch64 then
      "/lib64/ld-linux-aarch64.so.1"
    else if hostPlatform.isLinux && hostPlatform.isArmv7 then
      "/lib64/ld-linux-armhf.so.3"
    else
      "";

  # The hook is used when building on darwin for non-darwin, where the flags
  # need to be cleaned up.
  darwinSuffixSalt = builtins.replaceStrings [ "-" "." ] [ "_" "_" ] buildPlatform.config;
  targetSuffixSalt = builtins.replaceStrings [ "-" "." ] [ "_" "_" ] hostPlatform.config;
  setupHookDarwin = makeSetupHook {
    name = "darwin-hopr-gcc-hook";
    substitutions = { inherit darwinSuffixSalt targetSuffixSalt; };
  } ./setup-hook-darwin.sh;

  crateInfo = craneLib.crateNameFromCargoToml { inherit cargoToml; };
  pname = crateInfo.pname;
  pnameSuffix = if CARGO_PROFILE == "release" then "" else "-${CARGO_PROFILE}";
  version = lib.strings.concatStringsSep "." (
    lib.lists.take 3 (builtins.splitVersion crateInfo.version)
  );

  isDarwinForDarwin = buildPlatform.isDarwin && hostPlatform.isDarwin;
  isDarwinForNonDarwin = buildPlatform.isDarwin && !hostPlatform.isDarwin;

  extraBuildInputs =
    if isDarwinForDarwin || isDarwinForNonDarwin then
      [ pkgsStatOrDyn.pkgsBuildHost.apple-sdk_15 ]
    else
      [ ];
  extraNativeBuildInputs =
    if isDarwinForDarwin then
      [ pkgs.lld ]
    else if isDarwinForNonDarwin then
      [
        setupHookDarwin
        pkgs.lld
      ]
    else
      [ pkgs.mold ];

  sharedArgsBase = {
    inherit
      pname
      pnameSuffix
      version
      CARGO_PROFILE
      ;

    # FIXME: some dev dependencies depend on OpenSSL, would be nice to remove
    # this dependency
    nativeBuildInputs =
      [
        llvmPackages.bintools
        pkg-config
        libiconv
      ]
      ++ stdenv.extraNativeBuildInputs
      ++ extraNativeBuildInputs;
    buildInputs = [ pkgsStatOrDyn.openssl ] ++ stdenv.extraBuildInputs ++ extraBuildInputs;

    CARGO_HOME = ".cargo";
    cargoExtraArgs = "-p ${pname} ${cargoExtraArgs}";
    # disable running tests automatically for now
    doCheck = false;
    # set to the revision because during build the Git info is not available
    VERGEN_GIT_SHA = rev;
  };

  sharedArgs =
    if runTests then
      sharedArgsBase
      // {
        # exclude hopr-socks-server because it requires access to the internet
        cargoTestExtraArgs = "--workspace";
        doCheck = true;
      }
    else if runClippy then
      sharedArgsBase // { cargoClippyExtraArgs = "-- -Dwarnings"; }
    else
      sharedArgsBase;

  docsArgs = {
    cargoArtifacts = null;
    cargoExtraArgs = ""; # overwrite the default to build all docs
    cargoDocExtraArgs = "--workspace --no-deps";
    RUSTDOCFLAGS = "--enable-index-page -Z unstable-options";
    CARGO_TARGET_DIR = "target/";
    LD_LIBRARY_PATH = lib.makeLibraryPath [ pkgsStatOrDyn.pkgsBuildHost.openssl ];
    postBuild = ''
      ${pandoc}/bin/pandoc -f markdown+hard_line_breaks -t html README.md > readme.html
      mv target/''${CARGO_BUILD_TARGET}/doc target/
      ${html-tidy}/bin/tidy -q --custom-tags yes -i target/doc/index.html > index.html || :
      sed '/<section id="main-content" class="content">/ r readme.html' index.html > target/doc/index.html
      cp index.html target/doc/index-old.html
      rm readme.html index.html
    '';
  };

  defaultArgs = {
    cargoArtifacts = craneLib.buildDepsOnly (sharedArgs // { src = depsSrc; });
  };

  args = if buildDocs then sharedArgs // docsArgs else sharedArgs // defaultArgs;

  builder =
    if runTests then
      craneLib.cargoTest
    else if runClippy then
      craneLib.cargoClippy
    else if buildDocs then
      craneLib.cargoDoc
    else
      craneLib.buildPackage;
in
builder (
  args
  // {
    inherit src postInstall;

    preConfigure = ''
      # respect the amount of available cores for building
      export CARGO_BUILD_JOBS=$NIX_BUILD_CORES
    '';

    preFixup = lib.optionalString (isCross && targetInterpreter != "") ''
      for f in `find $out/bin/ -type f`; do
        echo "patching interpreter for $f to ${targetInterpreter}"
        patchelf --set-interpreter ${targetInterpreter} --output $f.patched $f
        mv $f.patched $f
      done
    '';
  }
)
