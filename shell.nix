{ pkgs ? import <nixpkgs> { }, gdal ? null, rustToolchain ? null }:

let
  package = import ./default.nix {
    inherit pkgs gdal rustToolchain;
  };
in

pkgs.mkShell {
  inputsFrom = [ package ];

  packages =
    (pkgs.lib.optionals (rustToolchain != null) [ rustToolchain ])
    ++ (with pkgs; [
      cargo-llvm-cov

      (python312.withPackages (ps: with ps; [
        pip
        pytest
        pytest-cov
        virtualenv
        xarray
        h5netcdf
        numpy
        matplotlib
      ]))

      proj
      gdal
      netcdf
      maturin
    ]);

  shellHook = ''
    alias ridal="$(pwd)/target/release/ridal"
  '';
}
