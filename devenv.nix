{ pkgs, ... }:
let
  # UEFI firmware with Secure Boot and the Microsoft keys pre-enrolled (Fedora's shim is MS-signed).
  ovmfSecure = pkgs.OVMF.override {
    secureBoot = true;
    msVarsTemplate = true;
  };
in
{
  # Rust toolchain from rust-overlay (see devenv.yaml): rustc, cargo, clippy, rustfmt, rust-analyzer.
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
      "rust-src"
    ];
  };

  packages = [
    pkgs.cargo-nextest
    pkgs.cargo-watch
    pkgs.just
    pkgs.pkg-config
    # VM testing of bootc images (KVM from the host). QEMU ships the edk2 UEFI firmware.
    pkgs.qemu_kvm
    pkgs.swtpm
  ];

  # Used by scripts/vm-run.sh (VM_SECUREBOOT=1).
  env.MELISSA_OVMF_DIR = "${ovmfSecure.fd}/FV";

  # Containers, bootc and skopeo come from the host (Kinoite ships podman, buildah and bootc);
  # rootless podman from Nix lacks the setuid helpers. `just image-*` calls the host tools.

  # Pre-commit hooks, installed into .git/hooks on shell entry.
  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
  };

  enterShell = ''
    echo "Melissa dev shell: $(rustc --version)"
  '';
}
