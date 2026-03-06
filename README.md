# libkrun-builder

Lightweight krunkit-based Linux builder for Apple Silicon.

## Overview

libkrun-builder provisions and manages a NixOS virtual machine on macOS using krunkit (libkrun). It provides a fast Linux build environment for Apple Silicon Macs, enabling native `aarch64-linux` Nix builds without emulation. The VM image is built from upstream nixpkgs to maximize binary cache hits, and configuration is layered via YAML files and environment variables (using figment).

## Usage

```bash
# Build the CLI tool
nix build

# Build the NixOS guest disk image
nix build .#nixosConfigurations.guest.config.system.build.diskImage

# Use as a nix-darwin module
{
  imports = [ libkrun-builder.darwinModules.default ];
}
```

## Configuration

Config is loaded from `/etc/libkrun-builder/config.yaml` (or `$LIBKRUN_CONFIG`), with environment variable overrides using the `LIBKRUN_` prefix.

```yaml
image: /path/to/nixos-guest.qcow2
workdir: /var/lib/libkrun-builder
cores: 6
memory: 8GiB
ssh_port: 31122
```

## License

MIT
