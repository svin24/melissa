# The sandbox and egress integration tests of crates/agent as standalone executables, for running
# on a real NixOS system in the VM test (bwrap, user namespaces, melissa-netbridge, the proxy).
# Each test binary finds `sh` and `curl` on PATH; MELISSA_NETBRIDGE points at the packaged bridge.
{
  lib,
  rustPlatform,
  jq,
  melissa,
}:
rustPlatform.buildRustPackage {
  pname = "melissa-integration-tests";
  inherit (melissa) version src cargoDeps;

  nativeBuildInputs = [ jq ];
  dontCargoBuild = true;
  dontCargoInstall = true;
  doCheck = false;

  buildPhase = ''
    runHook preBuild
    cargo test --release --frozen --no-run -p melissa-agent --test sandbox --test egress \
      --message-format=json-render-diagnostics > build.json
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    jq -r 'select(.reason == "compiler-artifact" and .profile.test == true and .executable != null)
      | "\(.target.name) \(.executable)"' build.json |
      while read -r name exe; do install -Dm0755 "$exe" "$out/bin/melissa-it-$name"; done
    test -x $out/bin/melissa-it-sandbox && test -x $out/bin/melissa-it-egress
    runHook postInstall
  '';

  meta = melissa.meta // {
    description = "Melissa sandbox and egress integration tests (for the NixOS VM test)";
    mainProgram = "melissa-it-egress";
  };
}
