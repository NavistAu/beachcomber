{
  description = "Daemon that caches shell environment state for instant prompt rendering";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "beachcomber";
          version = "0.3.0";

          src = ../..;

          cargoLock = {
            lockFile = ../../Cargo.lock;
          };

          nativeBuildInputs = [ pkgs.pkg-config ];

          meta = with pkgs.lib; {
            description = "Daemon that caches shell environment state for instant prompt rendering";
            homepage = "https://github.com/NavistAu/beachcomber";
            license = licenses.mit;
            mainProgram = "comb";
          };
        };
      });
}
