{
  description = "Melissa: development shell (devenv), Nix package, NixOS module and VM test";

  inputs = {
    nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
    devenv.url = "github:cachix/devenv";
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  nixConfig = {
    extra-trusted-public-keys = "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAng4A1Ry2Iy4Lq2nTEExiTOyDvo=";
    extra-substituters = "https://devenv.cachix.org";
  };

  outputs =
    {
      self,
      nixpkgs,
      devenv,
      ...
    }@inputs:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      # Melissa as a Nix service: see docs/agent-skills/nix-service.md.
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          melissa = pkgs.callPackage ./packaging/nix/package.nix { };
        in
        {
          inherit melissa;
          default = melissa;
          melissa-integration-tests = pkgs.callPackage ./packaging/nix/tests/integration-tests.nix {
            inherit melissa;
          };
          # Non-NixOS hosts with Nix (prototype): package + units + `melissa-host install`.
          melissa-host = pkgs.callPackage ./packaging/nix/host { inherit melissa; };
        }
      );

      overlays.default = final: _prev: {
        melissa = final.callPackage ./packaging/nix/package.nix { };
      };

      nixosModules.melissa = import ./packaging/nix/module.nix { inherit self; };
      nixosModules.default = self.nixosModules.melissa;

      # nix build .#checks.x86_64-linux.melissa-nixos -L   (needs KVM)
      checks.x86_64-linux.melissa-nixos = import ./packaging/nix/tests/vm.nix {
        inherit self;
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
      };

      # Use with: nix develop --no-pure-eval   (or just use `devenv shell` / direnv)
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = devenv.lib.mkShell {
            inherit inputs pkgs;
            modules = [ ./devenv.nix ];
          };
        }
      );
    };
}
