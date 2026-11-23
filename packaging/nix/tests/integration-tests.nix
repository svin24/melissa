# The sandbox, egress and escape integration tests of crates/agent as standalone executables, for
# running on a real NixOS system in the VM test (bwrap, user namespaces, melissa-netbridge, the proxy).
# Each test binary finds `sh` and `curl` on PATH; MELISSA_NETBRIDGE points at the packaged bridge.
# The escape suite also gets bin/melissa-escape-probe (crates/escape-probe: its own Cargo.lock, test-only,
# never part of the melissa package) through MELISSA_ESCAPE_PROBE.
{
  lib,
  rustPlatform,
  jq,
  melissa,
}:
let
  probeSrc = ../../../crates/escape-probe;
  probe = rustPlatform.buildRustPackage {
    pname = "melissa-escape-probe";
    version = "0.0.1";
    src = lib.fileset.toSource {
      root = probeSrc;
      fileset = lib.fileset.unions [
        (probeSrc + "/Cargo.toml")
        (probeSrc + "/Cargo.lock")
        (probeSrc + "/src")
      ];
    };
    cargoLock.lockFile = probeSrc + "/Cargo.lock";
    # Its unit tests make real syscalls (ptrace, perf) that the Nix build sandbox may refuse.
    doCheck = false;
    meta = melissa.meta // {
      description = "Probe helper of Melissa's sandbox escape suite (test-only)";
      mainProgram = "melissa-escape-probe";
    };
  };
in
rustPlatform.buildRustPackage {
  pname = "melissa-integration-tests";
  inherit (melissa.unwrapped) version src cargoDeps;

  nativeBuildInputs = [ jq ];
  dontCargoBuild = true;
  dontCargoInstall = true;
  doCheck = false;

  buildPhase = ''
    runHook preBuild
    cargo test --release --frozen --no-run -p melissa-agent --test sandbox --test egress --test escape \
      --message-format=json-render-diagnostics > build.json
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    jq -r 'select(.reason == "compiler-artifact" and .profile.test == true and .executable != null)
      | "\(.target.name) \(.executable)"' build.json |
      while read -r name exe; do install -Dm0755 "$exe" "$out/bin/melissa-it-$name"; done
    test -x $out/bin/melissa-it-sandbox && test -x $out/bin/melissa-it-egress && test -x $out/bin/melissa-it-escape
    # A copy, not a link: the suite bind-mounts it read-only into each probe's sandbox.
    install -m0755 ${probe}/bin/melissa-escape-probe $out/bin/melissa-escape-probe
    runHook postInstall
  '';

  passthru = { inherit probe; };

  meta = melissa.meta // {
    description = "Melissa sandbox, egress and escape integration tests (for the NixOS VM test)";
    mainProgram = "melissa-it-egress";
  };
}
