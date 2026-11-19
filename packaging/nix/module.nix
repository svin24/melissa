# NixOS module: Melissa as a system service (`services.melissa`). See
# docs/agent-skills/nix-service.md.
#
# What it sets up, matching the bootc images (image/rootfs, see common.nix):
# - users `melissa` (the queen and its agents) and group `melissa-kube`, tmpfiles from the package;
# - melissa-queen.service: the shared unit (hardening, state/config/runtime dirs) with a drop-in
#   for PATH (bubblewrap, the agent CLIs in /var/lib/melissa/home, the NixOS system profile);
# - /etc/melissa/queen.toml generated from the options (providers.toml stays the queen's to write);
# - k3s through nixpkgs' `services.k3s`, with Melissa's RBAC, admission policies and Traefik
#   config as auto-deploy manifests, the node IP pinned like the images do, airgap images optional;
# - melissa-kubeconfig.service: the queen's read-only and app-install kubeconfigs;
# - the polkit rule that lets the queen restart itself (Settings → Providers);
# - nix-ld, so the vendors' agent CLIs that `melissa-setup agent <name>` downloads can run.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.melissa;
  toml = pkgs.formats.toml { };
  home = "/var/lib/melissa/home";

  queenToml = toml.generate "queen.toml" (
    lib.recursiveUpdate (
      {
        inherit (cfg) listen;
        apps = {
          catalog = toString cfg.apps.catalog;
          port_offset = cfg.apps.portOffset;
        }
        // lib.optionalAttrs (cfg.apps.addresses != null) { inherit (cfg.apps) addresses; };
      }
      // lib.optionalAttrs (cfg.backends != [ ]) { backend = cfg.backends; }
    ) cfg.settings
  );

  # Pin k3s's node IP to the IPv4 address of the default route at each start, as the images'
  # k3s-prepare does: otherwise a global IPv6 address that SLAAC adds after network-online makes
  # k3s see "NodeIPs changed" and exit. Skipped when `k3s.nodeIp` is set.
  k3sNodeIp = pkgs.writeShellScript "melissa-k3s-node-ip" ''
    set -eu
    PATH=${
      lib.makeBinPath [
        pkgs.iproute2
        pkgs.gnused
        pkgs.coreutils
      ]
    }
    ip4=$(ip -4 -o route show default 2>/dev/null | sed -n '1s/.* src \([0-9.]*\).*/\1/p')
    if [ -z "$ip4" ]; then
      dev=$(ip -4 -o route show default 2>/dev/null | sed -n '1s/.* dev \([^ ]*\).*/\1/p')
      if [ -n "$dev" ]; then
        ip4=$(ip -4 -o addr show dev "$dev" scope global 2>/dev/null | sed -n '1s/.* inet \([0-9.]*\)\/.*/\1/p')
      fi
    fi
    [ -n "$ip4" ] || echo "melissa: no IPv4 default route; k3s picks the node IP" >&2
    printf 'MELISSA_K3S_ARGS=%s\n' "''${ip4:+--node-ip=$ip4}" > /run/melissa-k3s/env
  '';

  manifestsDir = "${cfg.package}/share/melissa/k3s/manifests";
  # Names from the shared source tree (no import-from-derivation); files from the package.
  manifestNames = builtins.attrNames (
    lib.filterAttrs (n: t: t == "regular" && lib.hasSuffix ".yaml" n) (
      builtins.readDir ((import ./common.nix).root + "/usr/lib/melissa/k3s/manifests")
    )
  );
  airgapImages =
    let
      arch = if pkgs.stdenv.hostPlatform.isAarch64 then "arm64" else "amd64";
    in
    cfg.k3s.package."airgap-images-${arch}-tar-zst";
in
{
  options.services.melissa = {
    enable = lib.mkEnableOption "Melissa, the queen bee with its web UI, agent sandbox and k3s apps";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.melissa;
      defaultText = lib.literalExpression "melissa.packages.\${system}.melissa";
      description = "The Melissa package.";
    };

    listen = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:8080";
      description = ''
        Address of the web UI. The queen refuses a non-loopback address until the UI has logins;
        reach it with `ssh -L 8080:127.0.0.1:8080` or a local reverse proxy you secure yourself.
      '';
    };

    backends = lib.mkOption {
      type = lib.types.listOf toml.type;
      default = [ ];
      example = lib.literalExpression ''
        [ { kind = "echo"; name = "echo-demo"; } ]
      '';
      description = ''
        Extra agent backends: `[[backend]]` entries of queen.toml (see docs/agent-skills/
        agent-cli-backends.md). The built-in ones (claude, grok, opencode) need no entry; they are
        used once their CLI is installed with `sudo melissa-setup agent <name>`.
      '';
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        { queen.backends = [ "claude-session" ]; task_timeout_secs = 600; }
      '';
      description = ''
        Further queen.toml settings, merged over the ones the module derives from its options.
        Provider settings made in the web UI go to /etc/melissa/providers.toml instead and win
        over these.
      '';
    };

    apps = {
      catalog = lib.mkOption {
        type = lib.types.either lib.types.path lib.types.str;
        default = "${cfg.package}/share/melissa/catalog";
        defaultText = lib.literalExpression ''"''${cfg.package}/share/melissa/catalog"'';
        description = "Directory of app manifests (`<name>.toml`), `[apps] catalog`.";
      };

      addresses = lib.mkOption {
        type = lib.types.nullOr (lib.types.listOf lib.types.str);
        default = null;
        example = [
          "192.0.2.10"
          "box.example.com"
        ];
        description = ''
          Addresses people reach this machine at, used in app links `http://<address>:<port>`
          (`[apps] addresses`). Null: found automatically (default route and Tailscale addresses).
        '';
      };

      portOffset = lib.mkOption {
        type = lib.types.int;
        default = 0;
        description = ''
          Added to an app's NodePort (30000-30079) in links, when something in front publishes
          the NodePorts at other ports (`[apps] port_offset`).
        '';
      };

      openFirewall = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Open the app NodePorts 30000-30079 (TCP) in the firewall, for the LAN and Tailscale.";
      };
    };

    k3s = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Run k3s (nixpkgs `services.k3s`) for apps. Without it the queen and its agents still
          work; app tools report that no cluster is there.
        '';
      };

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.k3s_1_37 or pkgs.k3s;
        defaultText = lib.literalExpression "pkgs.k3s_1_37";
        description = ''
          The k3s package. Melissa's Kubernetes client is built for the 1.37 API (the images pin
          v1.37.2); never skip a minor version when upgrading.
        '';
      };

      airgapImages = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Import k3s's own images (pause, CoreDNS, Traefik, ...) from the Nix store instead of
          pulling them on first start. Needed offline; adds about 200 MB to the closure.
        '';
      };

      images = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = [ ];
        description = "Further image archives to import into k3s at start (`services.k3s.images`).";
      };

      nodeIp = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          Node IP for k3s. Null: the IPv4 address of the default route at each start, as the
          Melissa images do.
        '';
      };
    };

    agents = {
      nixLd = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Enable nix-ld, so the agent CLIs installed by `melissa-setup agent <name>` (the vendors'
          prebuilt Linux binaries, linked against /lib64/ld-linux-x86-64.so.2) run on NixOS.
        '';
      };

      packages = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = [ ];
        example = lib.literalExpression "[ pkgs.opencode ]";
        description = ''
          Agent CLIs from Nix instead of the vendors' installers, put on the queen's PATH (after
          the ones `melissa-setup` installs). The binary's file name decides its sandbox preset and
          egress allowlist, so it must be called `claude`, `grok` or `opencode`. Logins still live in
          /var/lib/melissa/home (`sudo melissa-setup login <name>`).
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        assertions = [
          {
            assertion = config.security.allowUserNamespaces;
            message = "services.melissa: the agent sandbox (bubblewrap) needs security.allowUserNamespaces.";
          }
        ];

        users.users.melissa = {
          isSystemUser = true;
          group = "melissa";
          description = "Melissa queen and its agents";
          inherit home;
          shell = "${pkgs.shadow}/bin/nologin";
        };
        users.groups.melissa = { };
        # May read the queen's k3s kubeconfigs in /run/melissa-kube; only the queen's unit has it.
        users.groups.melissa-kube = { };

        environment.systemPackages = [ cfg.package ];
        systemd.packages = [ cfg.package ];
        systemd.tmpfiles.packages = [ cfg.package ];
        environment.etc."melissa/queen.toml".source = queenToml;

        systemd.services.melissa-queen = {
          wantedBy = [ "multi-user.target" ];
          # The unit sets PATH for the image; this drop-in replaces it. Agent CLIs first (as on
          # the image), then the sandbox and the system profile (systemctl, sh for the agents).
          path = [
            "${home}/.local"
            "${home}/.grok"
            "${home}/.opencode"
            pkgs.bubblewrap
          ]
          ++ cfg.agents.packages
          ++ [ "/run/current-system/sw" ];
          environment.MELISSA_VERSION = "${cfg.package.version}-nix";
          restartTriggers = [ queenToml ];
        };

        # Settings → Providers restarts the queen through polkit (rule from the package; the
        # update units it also names do not exist here, see nix-service.md).
        security.polkit.enable = true;

        programs.nix-ld.enable = lib.mkIf cfg.agents.nixLd true;

        networking.firewall.allowedTCPPortRanges = lib.mkIf cfg.apps.openFirewall [
          {
            from = 30000;
            to = 30079;
          }
        ];
      }

      (lib.mkIf cfg.k3s.enable {
        services.k3s = {
          enable = true;
          inherit (cfg.k3s) package;
          role = "server";
          nodeLabel = [ "melissa.io/managed=true" ];
          nodeIP = cfg.k3s.nodeIp;
          extraFlags = [
            "--write-kubeconfig-mode=0600"
          ]
          ++ lib.optional (cfg.k3s.nodeIp == null) "$MELISSA_K3S_ARGS";
          manifests = lib.listToAttrs (
            map (
              name:
              lib.nameValuePair "melissa-${lib.removeSuffix ".yaml" name}" {
                source = "${manifestsDir}/${name}";
                target = "melissa-${name}";
              }
            ) manifestNames
          );
          images = lib.optional cfg.k3s.airgapImages airgapImages ++ cfg.k3s.images;
        };

        systemd.services.k3s.serviceConfig = lib.mkIf (cfg.k3s.nodeIp == null) {
          RuntimeDirectory = "melissa-k3s";
          ExecStartPre = [ "${k3sNodeIp}" ];
          EnvironmentFile = lib.mkForce (
            lib.optional (config.services.k3s.environmentFile != null) config.services.k3s.environmentFile
            ++ [ "-/run/melissa-k3s/env" ]
          );
        };

        # Pods reach the API server and kubelet on the host through cni0 (the images' firewalld
        # zone trusts the pod and service networks the same way). Opens nothing to the LAN.
        networking.firewall.trustedInterfaces = [ "cni0" ];

        systemd.services.melissa-kubeconfig = {
          description = "Melissa queen's k3s credentials (read-only ServiceAccount kubeconfig)";
          after = [ "k3s.service" ];
          wantedBy = [ "k3s.service" ];
          path = [ cfg.k3s.package ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = "${cfg.package}/libexec/melissa/melissa-kubeconfig";
            # Waits up to 300 s for the API and the tokens on first start.
            TimeoutStartSec = 330;
            Restart = "on-failure";
            RestartSec = "15s";
          };
        };
      })
    ]
  );
}
