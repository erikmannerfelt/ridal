{
  description = "Simple (-ish) Ground Penetrating Radar software";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, fenix }:
    let
      # `rev` is set when the tree is clean, as it always is when building
      # `github:erikmannerfelt/ridal/<commit>`; a local build of a modified
      # tree has `dirtyRev` (`<commit>-dirty`) instead.
      rev = self.rev or self.dirtyRev or null;
    in
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };

          rustToolchain = pkgs.fenix.complete.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rustc"
            "rustfmt"
            "llvm-tools-preview"
          ];

          ridal = import ./default.nix {
            inherit pkgs rustToolchain rev;
          };
        in
        {
          devShells.default = import ./shell.nix {
            inherit pkgs rustToolchain;
          };

          packages = {
            inherit ridal;
            default = ridal;
          };
        }
      ) // {
      overlays.default = final: prev: {
        ridal = import ./default.nix {
          pkgs = final;
          rustToolchain = null;
          inherit rev;
        };
      };
    };
}
