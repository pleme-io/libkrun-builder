{
  description = "libkrun-builder — lightweight krunkit-based Linux builder for Apple Silicon";

  nixConfig = {
    allow-import-from-derivation = true;
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    crate2nix.url = "github:nix-community/crate2nix";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    substrate = {
      url = "github:pleme-io/substrate";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.fenix.follows = "fenix";
    };
  };

  outputs = {
    self,
    nixpkgs,
    crate2nix,
    flake-utils,
    substrate,
    ...
  }:
    (import "${substrate}/lib/rust-tool-release-flake.nix" {
      inherit nixpkgs crate2nix flake-utils;
    }) {
      toolName = "libkrun-builder";
      src = self;
      repo = "pleme-io/libkrun-builder";
    }
    // {
      darwinModules.default = ./module.nix;

      # NixOS guest image — built from upstream nixpkgs for cache.nixos.org hits
      # Build with: nix build .#nixosConfigurations.guest.config.system.build.diskImage
      nixosConfigurations.guest = nixpkgs.lib.nixosSystem {
        system = "aarch64-linux";
        modules = [./guest.nix];
      };
    };
}
