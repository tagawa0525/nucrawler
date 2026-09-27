{
  description = "nucrawler - 原子力（軽水炉）関係のサイトを巡回し、和訳・要約して推薦する";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      # systemd の user timer で動かすので Linux のみ
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (system: {
        nucrawler = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { };
        default = self.packages.${system}.nucrawler;
      });

      overlays.default = final: prev: {
        nucrawler = final.callPackage ./nix/package.nix { };
      };

      # Home Manager module（パッケージはこの flake のものを既定にする）
      homeManagerModules.default = { lib, pkgs, ... }: {
        imports = [ ./nix/home-manager.nix ];
        services.nucrawler.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      };

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              rustup
              sqlite
            ];
          };
        }
      );
    };
}
