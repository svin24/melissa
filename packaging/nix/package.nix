# Melissa as a Nix package: the Rust binaries (built from Cargo.lock), the app catalog, and the
# host-agnostic system files from `common.nix` with their /usr paths pointed at the store.
#
#   bin/melissa-server, melissa-device, melissa-netbridge   the workspace binaries
#   bin/melissa-setup                                     agent CLI installer (vendor installers)
#   libexec/melissa/melissa-kubeconfig                    root helper: the server's k3s credentials
#   libexec/melissa/k3s-node-ip                           pins k3s's node IP (shared with k3s-prepare)
#   lib/systemd/system/melissa-server.service              the shared unit, ExecStart in the store
#   lib/tmpfiles.d/melissa-{config,home,kube}.conf        the shared tmpfiles rules
#   share/melissa/catalog/*.toml                          the reviewed app manifests
#   share/melissa/k3s/manifests/*.yaml                    RBAC, admission policies, Traefik config
#   share/polkit-1/rules.d/50-melissa.rules               lets the server restart itself (Settings)
#   lib/melissa/release                                   MELISSA_VERSION/REVISION, MELISSA_RUNTIME=nix
#                                                         (the server reads it via MELISSA_RELEASE_FILE)
#
# Two derivations: the Rust build (`passthru.unwrapped`, rebuilt only when crates/ or Cargo.lock
# change) and this cheap one around it, so a new revision or a packaging change never recompiles.
{
  lib,
  rustPlatform,
  stdenvNoCC,
  makeWrapper,
  bash,
  coreutils,
  curl,
  findutils,
  gawk,
  gnugrep,
  gnused,
  gnutar,
  gzip,
  iproute2,
  systemd,
  unzip,
  util-linux,
  which,
  xz,
  # The source revision for the release file (the flake passes self.shortRev or dirtyShortRev).
  revision ? null,
}:
let
  inherit (import ./common.nix) root;
  repo = ../..;
  cargo = lib.importTOML (repo + "/crates/server/Cargo.toml");
  # Tools the vendors' install scripts and the helpers expect on PATH (the image has them in /usr/bin).
  setupPath = lib.makeBinPath [
    bash
    coreutils
    curl
    findutils
    gawk
    gnugrep
    gnused
    gnutar
    gzip
    iproute2
    systemd
    unzip
    util-linux
    which
    xz
  ];
  meta = {
    description = "Melissa: a self-hosted cluster of AI agents that runs apps on k3s";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "melissa-server";
  };

  bins = rustPlatform.buildRustPackage {
    pname = "melissa-unwrapped";
    inherit (cargo.package) version;

    # Only what the build reads: a change to docs or packaging does not rebuild the binaries.
    src = lib.fileset.toSource {
      root = repo;
      fileset = lib.fileset.unions [
        (repo + "/Cargo.toml")
        (repo + "/Cargo.lock")
        (repo + "/clippy.toml")
        (repo + "/crates")
      ];
    };
    cargoLock.lockFile = repo + "/Cargo.lock";

    # melissa-agent's only binary is melissa-netbridge (runs inside every agent sandbox).
    cargoBuildFlags = [
      "-p"
      "melissa-server"
      "-p"
      "melissa-device"
      "-p"
      "melissa-agent"
    ];
    # The unit tests run in the dev shell (`just check`); several expect a usr-merged host. The
    # sandbox and egress integration tests run on NixOS in the VM test (`melissa-tests`).
    doCheck = false;

    inherit meta;
  };
in
stdenvNoCC.mkDerivation {
  pname = "melissa";
  inherit (cargo.package) version;
  dontUnpack = true;

  nativeBuildInputs = [ makeWrapper ];
  # Interpreters for the helper scripts' #! lines (patchShebangs looks them up here).
  buildInputs = [ bash ];

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    # Copies, not links: the sandbox bind-mounts melissa-netbridge, and bwrap can't mount on a symlink.
    install -m0755 -t $out/bin ${bins}/bin/*
    install -Dm0644 -t $out/share/melissa/catalog ${repo + "/manifests"}/*.toml
    install -Dm0644 -t $out/share/melissa/k3s/manifests ${root + "/usr/lib/melissa/k3s/manifests"}/*.yaml
    install -Dm0644 ${root + "/usr/share/polkit-1/rules.d/50-melissa.rules"} \
      $out/share/polkit-1/rules.d/50-melissa.rules
    for f in melissa-config melissa-home melissa-kube; do
      install -Dm0644 ${root + "/usr/lib/tmpfiles.d"}/$f.conf $out/lib/tmpfiles.d/$f.conf
    done
    # Like the container's /usr/lib/melissa/release. The server finds it through
    # MELISSA_RELEASE_FILE (set by the NixOS module and the melissa-host unit); MELISSA_RUNTIME=nix
    # tells Settings that updates come from the Nix configuration.
    mkdir -p $out/lib/melissa
    {
      printf 'MELISSA_VERSION=%s\n' "$version"
      ${lib.optionalString (revision != null) "printf 'MELISSA_REVISION=%s\\n' ${lib.escapeShellArg revision}"}
      printf 'MELISSA_RUNTIME=nix\n'
    } > $out/lib/melissa/release

    # The shared unit with its binary in the store. PATH and the rest are set by the NixOS
    # module (or the non-NixOS unit) in a drop-in.
    install -Dm0644 ${root + "/usr/lib/systemd/system/melissa-server.service"} \
      $out/lib/systemd/system/melissa-server.service
    substituteInPlace $out/lib/systemd/system/melissa-server.service \
      --replace-fail "ExecStart=/usr/bin/melissa-server" "ExecStart=$out/bin/melissa-server"

    # melissa-setup: the vendors' installers run as `melissa` with a clean PATH; give them the
    # usual tools (and the NixOS system profile) instead of /usr/bin alone.
    install -Dm0755 ${root + "/usr/bin/melissa-setup"} $out/bin/melissa-setup
    substituteInPlace $out/bin/melissa-setup \
      --replace-fail 'release=/usr/lib/melissa/release' "release=$out/lib/melissa/release" \
      --replace-fail '$home/.opencode/bin:/usr/bin"' "\$home/.opencode/bin:${setupPath}:/run/current-system/sw/bin:/usr/bin\""
    patchShebangs $out/bin/melissa-setup
    wrapProgram $out/bin/melissa-setup --prefix PATH : ${setupPath}

    # melissa-kubeconfig reads the RBAC manifests (to recreate a rotated ServiceAccount). It runs
    # `k3s kubectl`: the k3s on the unit's PATH (the host's k3s package), not one baked in here.
    install -Dm0755 ${root + "/usr/libexec/melissa/melissa-kubeconfig"} \
      $out/libexec/melissa/melissa-kubeconfig
    substituteInPlace $out/libexec/melissa/melissa-kubeconfig \
      --replace-fail 'manifests=/usr/lib/melissa/k3s/manifests' "manifests=$out/share/melissa/k3s/manifests"
    patchShebangs $out/libexec/melissa/melissa-kubeconfig
    wrapProgram $out/libexec/melissa/melissa-kubeconfig \
      --suffix PATH : ${
        lib.makeBinPath [
          coreutils
          gnugrep
        ]
      }

    # k3s-node-ip: the node-IP pin k3s-prepare uses in the container, for the NixOS module's k3s.
    install -Dm0755 ${root + "/usr/libexec/melissa/k3s-node-ip"} $out/libexec/melissa/k3s-node-ip
    patchShebangs $out/libexec/melissa/k3s-node-ip
    wrapProgram $out/libexec/melissa/k3s-node-ip \
      --prefix PATH : ${
        lib.makeBinPath [
          coreutils
          gnugrep
          gnused
          iproute2
        ]
      }
    runHook postInstall
  '';

  passthru.unwrapped = bins;
  inherit meta;
}
