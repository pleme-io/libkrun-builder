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

    # Separate nixpkgs for the NixOS guest image — must NOT be followed
    # so it matches upstream cache.nixos.org and packages are fetched, not built.
    # The nix-rosetta-builder had the same design: "Does NOT follow our nixpkgs —
    # the VM image must match upstream's nixpkgs so it can be fetched from cache."
    nixpkgs-guest.url = "github:NixOS/nixpkgs/nixos-25.11";
  };

  outputs = {
    self,
    nixpkgs,
    nixpkgs-guest,
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

      # NixOS guest image — uses nixpkgs-guest (upstream, NOT followed)
      # so all packages are fetched from cache.nixos.org instead of built locally.
      # Build with: nix build .#nixosConfigurations.guest.config.system.build.diskImage
      nixosConfigurations.guest = nixpkgs-guest.lib.nixosSystem {
        system = "aarch64-linux";
        modules = [./guest.nix];
      };
    };
}
