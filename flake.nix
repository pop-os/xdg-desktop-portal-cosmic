{
  description = "XDG desktop portal the COSMIC desktop environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    nix-filter.url = "github:numtide/nix-filter";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, nix-filter, crane, fenix }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        craneLib = crane.lib.${system}.overrideToolchain fenix.packages.${system}.stable.toolchain;

        pkgDef = {
          src = nix-filter.lib.filter {
            root = ./.;
            include = [
              ./src
              ./Cargo.toml
              ./Cargo.lock
              ./Makefile
              ./data
            ];
          };
          nativeBuildInputs = with pkgs; [ pkg-config rustPlatform.bindgenHook ];
          buildInputs = with pkgs; [
            pipewire
            libxkbcommon
            libglvnd
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly pkgDef;
        xdg-desktop-portal-cosmic = craneLib.buildPackage (pkgDef // {
          inherit cargoArtifacts;
        });
      in {
        checks = {
          inherit xdg-desktop-portal-cosmic;
        };

        packages.default = xdg-desktop-portal-cosmic.overrideAttrs (oldAttrs: rec {
          installPhase = ''
            make install prefix=$out
          '';
          passthru.providedSessions = [ "cosmic" ];
        });

        apps.default = flake-utils.lib.mkApp {
          drv = xdg-desktop-portal-cosmic;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = builtins.attrValues self.checks.${system};
        };
      });

  nixConfig = {
    # Cache for the Rust toolchain in fenix
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [ "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs=" ];
  };
}
