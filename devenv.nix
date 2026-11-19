{ pkgs, ... }:
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
    # License report for the Rust dependencies shipped in the image (`just licenses`, about.toml).
    pkgs.cargo-about
    pkgs.cargo-nextest
    pkgs.cargo-watch
    pkgs.just
    pkgs.pkg-config
  ];

  # podman comes from the host; rootless podman from Nix lacks the setuid helpers.
  # `just container-*` calls the host's podman.

  # Pre-commit hooks, installed into .git/hooks on shell entry.
  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
  };

  enterShell = ''
    echo "Melissa dev shell: $(rustc --version)"
  '';
}
