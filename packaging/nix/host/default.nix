# Melissa on a non-NixOS host that has Nix (Fedora, Kinoite, Debian, ...): the package plus the
# system files a distribution would ship, and a small root script that links them into /etc.
# No NixOS, no extra tool. Prototype; see docs/agent-skills/nix-service.md ("Other distributions").
#
#   sudo nix profile install --profile /nix/var/nix/profiles/melissa <flake>#melissa-host
#   sudo /nix/var/nix/profiles/melissa/bin/melissa-host install     # links units, starts the queen
#   sudo nix profile upgrade --profile /nix/var/nix/profiles/melissa --all \
#     && sudo /nix/var/nix/profiles/melissa/bin/melissa-host install   # update
#   sudo /nix/var/nix/profiles/melissa/bin/melissa-host uninstall   # keeps /etc/melissa, /var/lib/melissa
#
# The profile is the GC root, and /etc links point through it, so an update is a profile switch
# plus `systemctl daemon-reload` and a restart (`nix profile rollback` goes back).
#
# k3s is not part of it: install k3s the distribution's way (on SELinux hosts with k3s-selinux, which
# labels /usr/bin/k3s, not a store path), then `melissa-host install` links the queen's RBAC,
# admission policies and Traefik config into k3s's manifests directory and adds
# melissa-kubeconfig.service.
{
  lib,
  runCommand,
  writeShellScript,
  melissa,
}:
let
  inherit (import ../common.nix) root;
  profile = "/nix/var/nix/profiles/melissa";
  home = "/var/lib/melissa/home";

  # PATH as on the image (vendor CLIs in the melissa user's home, then /usr/bin with the
  # distribution's bwrap and systemctl). Replaces the unit's own PATH line.
  dropIn = ''
    [Service]
    Environment=PATH=${home}/.local/bin:${home}/.grok/bin:${home}/.opencode/bin:/usr/bin:/usr/sbin
    Environment=MELISSA_RELEASE_FILE=${profile}/lib/melissa/release
  '';

  kubeconfigUnit = ''
    [Unit]
    Description=Melissa queen's k3s credentials (read-only ServiceAccount kubeconfig)
    After=k3s.service
    ConditionPathExists=/etc/rancher/k3s/k3s.yaml

    [Service]
    Type=oneshot
    RemainAfterExit=yes
    Environment=PATH=/usr/local/bin:/usr/bin:/usr/sbin
    ExecStart=${profile}/libexec/melissa/melissa-kubeconfig
    TimeoutStartSec=330
    Restart=on-failure
    RestartSec=15s

    [Install]
    WantedBy=k3s.service
  '';

  script = writeShellScript "melissa-host" ''
    set -eu
    # Prefer the profile path (stable across updates); fall back to this store path.
    base=${profile}
    [ -e "$base/lib/melissa-host" ] || base=@out@
    die() { echo "melissa-host: $*" >&2; exit 1; }
    [ "$(id -u)" = 0 ] || die "run it as root (sudo)"
    link() { mkdir -p "$(dirname "$2")"; ln -sfn "$1" "$2"; }

    case ''${1:-} in
      install)
        link "$base/lib/systemd/system/melissa-queen.service" /etc/systemd/system/melissa-queen.service
        link "$base/lib/melissa-host/10-nix.conf" /etc/systemd/system/melissa-queen.service.d/10-nix.conf
        link "$base/lib/sysusers.d/melissa.conf" /etc/sysusers.d/melissa.conf
        for f in melissa-config melissa-home melissa-kube; do
          link "$base/lib/tmpfiles.d/$f.conf" "/etc/tmpfiles.d/$f.conf"
        done
        # polkit reads /etc/polkit-1/rules.d; a copy, since polkitd may not follow links into /nix.
        install -D -m 0644 "$base/share/polkit-1/rules.d/50-melissa.rules" /etc/polkit-1/rules.d/50-melissa.rules
        for b in melissa-queen melissa-setup; do link "$base/bin/$b" "/usr/local/bin/$b"; done
        systemd-sysusers /etc/sysusers.d/melissa.conf
        systemd-tmpfiles --create /etc/tmpfiles.d/melissa-config.conf /etc/tmpfiles.d/melissa-home.conf \
          /etc/tmpfiles.d/melissa-kube.conf
        if [ -e /etc/rancher/k3s/k3s.yaml ] || command -v k3s >/dev/null 2>&1; then
          link "$base/lib/melissa-host/melissa-kubeconfig.service" /etc/systemd/system/melissa-kubeconfig.service
          m=/var/lib/rancher/k3s/server/manifests
          mkdir -p "$m"
          for f in "$base"/share/melissa/k3s/manifests/*.yaml; do
            install -m 0600 "$f" "$m/melissa-$(basename "$f")"
          done
          systemctl daemon-reload
          systemctl enable melissa-kubeconfig.service
        else
          echo "melissa-host: no k3s here; apps need k3s (see nix-service.md). The queen runs without it."
        fi
        systemctl daemon-reload
        systemctl enable melissa-queen.service
        systemctl restart melissa-queen.service
        echo "melissa-host: installed $(sed -n 's/^MELISSA_VERSION=//p' "$base/lib/melissa/release"); web UI on http://127.0.0.1:8080"
        ;;
      uninstall)
        systemctl disable --now melissa-queen.service melissa-kubeconfig.service 2>/dev/null || true
        rm -f /etc/systemd/system/melissa-queen.service /etc/systemd/system/melissa-kubeconfig.service \
          /etc/sysusers.d/melissa.conf /etc/tmpfiles.d/melissa-config.conf /etc/tmpfiles.d/melissa-home.conf \
          /etc/tmpfiles.d/melissa-kube.conf /etc/polkit-1/rules.d/50-melissa.rules \
          /usr/local/bin/melissa-queen /usr/local/bin/melissa-setup
        rm -rf /etc/systemd/system/melissa-queen.service.d
        rm -f /var/lib/rancher/k3s/server/manifests/melissa-*.yaml
        systemctl daemon-reload
        echo "melissa-host: removed; /etc/melissa and /var/lib/melissa are kept"
        ;;
      *) die "usage: melissa-host install|uninstall" ;;
    esac
  '';
in
runCommand "melissa-host-${melissa.version}"
  {
    inherit (melissa) version;
    passAsFile = [
      "dropIn"
      "kubeconfigUnit"
    ];
    inherit dropIn kubeconfigUnit;
    meta = melissa.meta // {
      description = "Melissa for non-NixOS hosts with Nix: package, units and an installer";
      mainProgram = "melissa-host";
    };
  }
  ''
    mkdir -p $out/bin $out/lib/melissa-host $out/lib/sysusers.d
    for d in lib/systemd lib/tmpfiles.d lib/melissa libexec share; do ln -s ${melissa}/$d $out/$d; done
    for b in ${melissa}/bin/*; do ln -s "$b" $out/bin/; done
    cp $dropInPath $out/lib/melissa-host/10-nix.conf
    cp $kubeconfigUnitPath $out/lib/melissa-host/melissa-kubeconfig.service
    install -m0644 ${root + "/usr/lib/sysusers.d/melissa.conf"} $out/lib/sysusers.d/melissa.conf
    substitute ${script} $out/bin/melissa-host --subst-var out
    chmod 0755 $out/bin/melissa-host
  ''
