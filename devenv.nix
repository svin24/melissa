{ pkgs, ... }:
let
  # UI screenshot harness (`just ui-shots`, scripts/ui-shots): Playwright's Python bindings and
  # the Chromium build that matches them (from Nix, so nothing is downloaded at run time),
  # Pillow for the pixel diffs, libfaketime so every run shows the same clock, and a fixed
  # font set for the browser so shots don't depend on the host's fonts.
  shotsPython = pkgs.python3.withPackages (ps: [
    ps.playwright
    ps.pillow
  ]);
  shotsBrowsers = pkgs.playwright-driver.browsers.override {
    withFirefox = false;
    withWebkit = false;
  };
  shotsFonts = pkgs.makeFontsConf {
    fontDirectories = [ pkgs.dejavu_fonts ];
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
    # License report for the Rust dependencies shipped in the image (`just licenses`, about.toml).
    pkgs.cargo-about
    pkgs.cargo-nextest
    pkgs.cargo-watch
    pkgs.just
    pkgs.pkg-config
    shotsPython
  ];

  env = {
    PLAYWRIGHT_BROWSERS_PATH = "${shotsBrowsers}";
    PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS = "true";
    # Only scripts/ui-shots uses these: the browser's FONTCONFIG_FILE, and the LD_PRELOAD of
    # the throwaway server it starts.
    MELISSA_SHOTS_FONTCONFIG = "${shotsFonts}";
    MELISSA_SHOTS_FAKETIME_LIB = "${pkgs.libfaketime}/lib/libfaketimeMT.so.1";
  };

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
