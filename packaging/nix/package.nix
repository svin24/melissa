# Melissa as a Nix package: the Rust binaries (built from Cargo.lock), the app catalog, and the
# host-agnostic system files from `common.nix` with their /usr paths pointed at the store.
#
#   bin/melissa-queen, melissa-comb, melissa-netbridge   the workspace binaries
#   bin/melissa-setup                                     agent CLI installer (vendor installers)
#   libexec/melissa/melissa-kubeconfig                    root helper: the queen's k3s credentials
#   lib/systemd/system/melissa-queen.service              the shared unit, ExecStart in the store
#   lib/tmpfiles.d/melissa-{config,home,kube}.conf        the shared tmpfiles rules
#   share/melissa/catalog/*.toml                          the reviewed app manifests
#   share/melissa/k3s/manifests/*.yaml                    RBAC, admission policies, Traefik config
#   share/polkit-1/rules.d/50-melissa.rules               lets the queen restart itself (Settings)
#   lib/melissa/release                                   MELISSA_VERSION for melissa-setup
{
  lib,
  rustPlatform,
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
}:
let
  inherit (import ./common.nix) root;
  repo = ../..;
  cargo = lib.importTOML (repo + "/crates/queen/Cargo.toml");
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
in
rustPlatform.buildRustPackage {
  pname = "melissa";
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
    "melissa-queen"
    "-p"
    "melissa-comb"
    "-p"
    "melissa-agent"
  ];
  # The unit tests run in the dev shell (`just check`); several expect a usr-merged host. The
  # sandbox and egress integration tests run on NixOS in the VM test (`melissa-tests`).
  doCheck = false;

  nativeBuildInputs = [ makeWrapper ];
  # Interpreters for the helper scripts' #! lines (patchShebangs looks them up here).
  buildInputs = [ bash ];

  postInstall = ''
    install -Dm0644 -t $out/share/melissa/catalog ${repo + "/manifests"}/*.toml
    install -Dm0644 -t $out/share/melissa/k3s/manifests ${root + "/usr/lib/melissa/k3s/manifests"}/*.yaml
    install -Dm0644 ${root + "/usr/share/polkit-1/rules.d/50-melissa.rules"} \
      $out/share/polkit-1/rules.d/50-melissa.rules
    for f in melissa-config melissa-home melissa-kube; do
      install -Dm0644 ${root + "/usr/lib/tmpfiles.d"}/$f.conf $out/lib/tmpfiles.d/$f.conf
    done
    mkdir -p $out/lib/melissa
    printf 'MELISSA_VERSION=%s\n' "$version" > $out/lib/melissa/release

    # The shared unit with its binary in the store. PATH and the rest are set by the NixOS
    # module (or the non-NixOS unit) in a drop-in.
    install -Dm0644 ${root + "/usr/lib/systemd/system/melissa-queen.service"} \
      $out/lib/systemd/system/melissa-queen.service
    substituteInPlace $out/lib/systemd/system/melissa-queen.service \
      --replace-fail "ExecStart=/usr/bin/melissa-queen" "ExecStart=$out/bin/melissa-queen"

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
  '';

  meta = {
    description = "Melissa: a self-hosted hive of AI agents that runs apps on k3s";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "melissa-queen";
  };
}
