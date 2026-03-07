# module.nix — nix-darwin module for libkrun-based Linux builder
# Replaces nix-rosetta-builder: uses krunkit (Apple Hypervisor.framework) instead of Lima
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.libkrun-builder;
  inherit (lib) mkEnableOption mkOption mkIf types;
in {
  options.services.libkrun-builder = {
    enable = mkEnableOption "libkrun-based Linux builder for Nix";

    cores = mkOption {
      type = types.int;
      default = 6;
      description = "Number of CPU cores for the VM.";
    };

    memory = mkOption {
      type = types.str;
      default = "8GiB";
      description = "Memory for the VM (e.g. '8GiB', '8192').";
    };

    diskSize = mkOption {
      type = types.str;
      default = "80GiB";
      description = "Disk size for the guest image.";
    };

    sshPort = mkOption {
      type = types.int;
      default = 31122;
      description = "Host port forwarded to guest SSH (port 22).";
    };

    systems = mkOption {
      type = types.listOf types.str;
      default = ["aarch64-linux" "x86_64-linux"];
      description = "Architectures the builder supports.";
    };

    guestImage = mkOption {
      type = types.str;
      description = "Path to the NixOS guest qcow2 image (runtime path, not imported into store).";
    };

    package = mkOption {
      type = types.package;
      description = "The libkrun-builder package.";
    };
  };

  config = mkIf cfg.enable {
    # System user/group for the daemon
    users.users._libkrunbuilder = {
      uid = 349;
      gid = 349;
      description = "libkrun-builder daemon user";
      home = "/var/lib/libkrun-builder";
    };

    users.groups._libkrunbuilder = {
      gid = 349;
    };

    # Working directory
    system.activationScripts.libkrun-builder-workdir = {
      text = ''
        mkdir -p /var/lib/libkrun-builder
        chown _libkrunbuilder:_libkrunbuilder /var/lib/libkrun-builder
        chmod 750 /var/lib/libkrun-builder
      '';
    };

    # launchd daemon
    launchd.daemons.libkrun-builderd = {
      serviceConfig = {
        Label = "io.pleme.libkrun-builderd";
        UserName = "_libkrunbuilder";
        GroupName = "_libkrunbuilder";
        WorkingDirectory = "/var/lib/libkrun-builder";
        KeepAlive = true;
        RunAtLoad = true;
        StandardOutPath = "/var/log/libkrun-builder.log";
        StandardErrorPath = "/var/log/libkrun-builder.log";
        EnvironmentVariables = {
          LIBKRUN_IMAGE = "${cfg.guestImage}";
          LIBKRUN_WORKDIR = "/var/lib/libkrun-builder";
          LIBKRUN_CORES = toString cfg.cores;
          LIBKRUN_MEMORY = cfg.memory;
          LIBKRUN_SSH_PORT = toString cfg.sshPort;
          # PATH: krunkit, gvproxy, openssh, coreutils, /usr/bin (codesign)
          PATH = lib.makeBinPath [
            pkgs.krunkit
            pkgs.gvproxy
            pkgs.openssh
            pkgs.coreutils
          ] + ":/usr/bin";
        };
        ProgramArguments = [
          "${cfg.package}/bin/libkrun-builder"
          "start"
        ];
      };
    };

    # Register as a nix build machine
    nix.buildMachines = [{
      hostName = "libkrun-builder";
      systems = cfg.systems;
      protocol = "ssh-ng";
      maxJobs = cfg.cores;
      supportedFeatures = ["kvm" "big-parallel"];
    }];

    nix.distributedBuilds = true;
  };
}
