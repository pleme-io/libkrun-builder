# guest.nix — Minimal NixOS guest for libkrun VM
# Provides aarch64-linux (native ARM) + x86_64-linux (Rosetta 2 translation)
# Built from upstream nixpkgs so the image fetches from cache.nixos.org
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}: {
  imports = [
    (modulesPath + "/profiles/qemu-guest.nix")
    (modulesPath + "/profiles/minimal.nix")
  ];

  # --- Boot ---
  boot.loader.systemd-boot.enable = true;
  boot.loader.efi.canTouchEfiVariables = true;

  # Rosetta 2 binfmt registration — run x86_64 ELF binaries via Rosetta
  boot.binfmt.registrations.rosetta = {
    interpreter = "/run/rosetta/rosetta";
    fixBinary = true;
    matchType = "magic";
    magicOrExtension = ''\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00'';
    mask = ''\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff'';
  };

  # --- Filesystems ---
  # Root disk (virtio)
  fileSystems."/" = {
    device = "/dev/vda2";
    fsType = "ext4";
  };
  fileSystems."/boot" = {
    device = "/dev/vda1";
    fsType = "vfat";
  };

  # Rosetta runtime from host via virtiofs
  fileSystems."/run/rosetta" = {
    device = "rosetta";
    fsType = "virtiofs";
  };

  # SSH keys from host via virtiofs
  fileSystems."/run/host-ssh-keys" = {
    device = "ssh-keys";
    fsType = "virtiofs";
  };

  # --- Networking ---
  networking.hostName = "libkrun-builder";
  networking.useDHCP = true;

  # --- SSH ---
  services.openssh = {
    enable = true;
    settings = {
      PermitRootLogin = "prohibit-password";
      PasswordAuthentication = false;
    };
  };

  # Copy authorized keys from virtiofs mount before sshd starts
  systemd.services.sshd-keys = {
    description = "Install SSH authorized keys from host";
    wantedBy = ["multi-user.target"];
    before = ["sshd.service"];
    serviceConfig.Type = "oneshot";
    script = ''
      mkdir -p /root/.ssh
      chmod 700 /root/.ssh
      if [ -f /run/host-ssh-keys/ssh_host_ed25519_key.pub ]; then
        cp /run/host-ssh-keys/ssh_host_ed25519_key.pub /root/.ssh/authorized_keys
        chmod 600 /root/.ssh/authorized_keys
      fi
    '';
  };

  # --- Nix ---
  nix = {
    settings = {
      trusted-users = ["root"];
      extra-platforms = ["x86_64-linux"];
      experimental-features = ["nix-command" "flakes"];
    };
  };

  # --- Disk image ---
  # Wire make-disk-image.nix into system.build.diskImage
  # qemu-vm's virtualisation.diskSize only affects ephemeral runtime disks
  system.build.diskImage = import "${pkgs.path}/nixos/lib/make-disk-image.nix" {
    inherit config lib pkgs;
    format = "qcow2";
    partitionTableType = "efi";
    diskSize = "auto";
    additionalSpace = "2048M";
    installBootLoader = true;
    copyChannel = false;
  };

  # Minimal system — no docs, no GUI
  documentation.enable = false;

  system.stateVersion = "25.11";
}
