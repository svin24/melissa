{ pkgs, ... }:
{
  # Rust toolchain from fenix (see devenv.yaml): rustc, cargo, clippy, rustfmt, rust-analyzer.
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
  ];

  # Containers and bootc come from the host (Kinoite ships podman, buildah and bootc);
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
