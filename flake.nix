{
  description = "den — Linux battery history recorder and terminal viewer";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.plainix = {
    url = "github:yawarakatai/plainix";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    inputs@{ self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forSystems = nixpkgs.lib.genAttrs systems;
      environment = inputs.plainix.lib.project inputs ./plainix.nix;
    in
    {
      inherit (environment) devShells;

      packages = forSystems (
        system:
        let
          den = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { };
        in
        {
          inherit den;
          default = den;
        }
      );

      apps = forSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.den}/bin/den";
          meta.description = "Open the den battery history viewer";
        };
      });

      nixosModules.default = import ./nix/module.nix {
        packageFor = system: self.packages.${system}.den;
      };

      checks = forSystems (system: {
        package = self.packages.${system}.den;
        module = import ./nix/module-test.nix { inherit self nixpkgs system; };
      });
    };
}
