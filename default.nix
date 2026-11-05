{ pkgs ? import <nixpkgs> { }
, gdal ? null
, rustToolchain ? null
  # The commit being built, reported by the server's health check. The flake
  # passes its own revision; a build that does not know it leaves it out.
, rev ? null
}:

let
  # The rust gdal system is a bit hard to compile without precompiled bindings.
  # Sometimes the unstable version of GDAL is updated faster than the precompiled
  # bindings are in gdal-sys. Therefore, it may be necessary to provide an older
  # GDAL as an argument.
  my-gdal = if gdal != null then gdal else pkgs.gdal;

  manifest = (pkgs.lib.importTOML ./Cargo.toml).package;

  rustPlatform =
    if rustToolchain != null then
      pkgs.makeRustPlatform
        {
          cargo = rustToolchain;
          rustc = rustToolchain;
        }
    else
      pkgs.rustPlatform;

in

rustPlatform.buildRustPackage ({
  pname = manifest.name;
  version = manifest.version;

  src = pkgs.lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  buildNoDefaultFeatures = true;
  buildFeatures = [ "cli" "server" ];

  nativeBuildInputs = with pkgs; [
    pkg-config
    cmake
    gnumake
    clang
  ];

  buildInputs = with pkgs; [
    proj
    my-gdal
    zlib
  ];

  # libz-sys fails when compiling, so testing is disabled for now
  doCheck = false;
} // pkgs.lib.optionalAttrs (rev != null) {
  RIDAL_GIT_COMMIT = rev;
})
