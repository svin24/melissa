# NixOS module: Melissa as a system service (`services.melissa`). See
# docs/agent-skills/nix-service.md.
#
# What it sets up, from the files every packaging shares (packaging/common, see common.nix):
# - users `melissa` (the server and its agents) and group `melissa-kube`, tmpfiles from the package;
# - melissa-server.service: the shared unit (hardening, state/config/runtime dirs) with a drop-in
#   for PATH (bubblewrap, the agent CLIs in /var/lib/melissa/home, the NixOS system profile);
# - /etc/melissa/server.toml generated from the options (providers.toml stays the server's to write);
# - k3s through nixpkgs' `services.k3s`, with Melissa's RBAC, admission policies and Traefik
#   config as auto-deploy manifests, the shared k3s config, the node IP pinned by the shared
#   k3s-node-ip helper (as in the container), airgap images optional;
# - melissa-kubeconfig.service: the server's read-only and app-install kubeconfigs;
# - the polkit rule that lets the server restart itself (Settings → Restart), nothing else;
# - the release file (MELISSA_RUNTIME=nix) for Settings, through MELISSA_RELEASE_FILE;
# - agent CLIs: `agents.packages` (the Nix way), or nix-ld for the vendors' installers.
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

  serverToml = toml.generate "server.toml" (
    lib.recursiveUpdate (
      {
        inherit (cfg) listen;
        apps = {
          port_offset = cfg.apps.portOffset;
        }
        // lib.optionalAttrs (cfg.apps.addresses != null) { inherit (cfg.apps) addresses; };
      }
      // lib.optionalAttrs (cfg.backends != [ ]) { backend = cfg.backends; }
    ) cfg.settings
  );


  manifestsDir = "${cfg.package}/share/melissa/k3s/manifests";
  gvisorDir = "${cfg.package}/share/melissa/k3s/gvisor";
  # k3s's containerd template with the runsc runtime entry (the container's
  # packaging/common/usr/lib/melissa/k3s/gvisor/config-v3.toml.tmpl, with store paths). Written as
  # config-v3.toml.tmpl, the name k3s 1.37 (containerd 2) reads; nixpkgs' containerdConfigTemplate
  # option writes config.toml.tmpl.
  gvisorTemplate = pkgs.writeText "config-v3.toml.tmpl" ''
    {{ template "base" . }}

    # Melissa: gVisor for apps with isolation `gvisor` (RuntimeClass `gvisor`, handler `runsc`).
    [plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.runsc]
      runtime_type = "io.containerd.runsc.v1"

    [plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.runsc.options]
      TypeUrl = "io.containerd.runsc.v1.options"
      ConfigPath = "${gvisorDir}/runsc.toml"
  '';
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
    enable = lib.mkEnableOption "Melissa, the server with its web UI, agent sandbox and k3s apps";

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
        Address of the web UI. The server refuses a non-loopback address until the UI has logins;
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
        Extra agent backends: `[[backend]]` entries of server.toml (see docs/agent-skills/
        agent-cli-backends.md). The built-in ones (claude, grok, opencode) need no entry; they are
        used once their CLI is installed with `sudo melissa-setup agent <name>`.
      '';
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        { lead.backends = [ "claude-session" ]; task_timeout_secs = 600; }
      '';
      description = ''
        Further server.toml settings, merged over the ones the module derives from its options.
        Provider settings made in the web UI go to /etc/melissa/providers.toml instead and win
        over these.
      '';
    };

    apps = {
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
          k3s serves NodePorts on every host address, so there is no bind setting to match; the
          app tools still try each address at run time and leave out ones that don't answer.
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
        description = "Open the app NodePorts 30000-30079 (TCP and UDP) in the firewall, for the LAN and Tailscale.";
      };
    };

    k3s = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Run k3s (nixpkgs `services.k3s`) for apps. Without it the server and its agents still
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

      gvisor = {
        enable = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = ''
            gVisor for apps (docs/DECISIONS.md 2026-11-23): runsc and its containerd shim on k3s's
            PATH, containerd's `runsc` runtime entry and the `gvisor` RuntimeClass. Apps run under it
            by default (`isolation` in the app spec); without it those apps are not deployed (never
            on the default runtime instead).
          '';
        };
        package = lib.mkPackageOption pkgs "gvisor" { };
      };

      nodeIp = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          Node IP for k3s. Null: the IPv4 address of the default route at each start (the shared
          `k3s-node-ip` helper, as in the container), unless a `node-ip` is set in
          /etc/rancher/k3s/config.yaml(.d).
        '';
      };
    };

    agents = {
      nixLd = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Enable nix-ld, so agent CLIs installed by hand with `melissa-setup agent <name>` (the
          vendors' prebuilt Linux binaries, linked against /lib64/ld-linux-x86-64.so.2) run on
          NixOS. The NixOS way to add agents is `agents.packages`.
        '';
      };

      packages = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = [ ];
        example = lib.literalExpression "[ pkgs.claude-code pkgs.opencode ]";
        description = ''
          The agent CLIs (Claude Code, Grok, OpenCode, Gemini CLI, Codex's ACP adapter, Muse Code),
          from Nix: the documented way to add a provider on NixOS (Settings → Providers does not
          install agents on Nix installs). They are put on the server's PATH (after any that
          `melissa-setup` installed by hand: it also installs `gemini` and `codex-acp` from npm with
          its own Node.js). The binary's file name decides its sandbox preset and egress allowlist,
          so it must be called `claude`, `grok`, `opencode`, `gemini`, `codex-acp` or `muse`. Logins
          live in /var/lib/melissa/home (`sudo melissa-setup login <name>`).
        '';
      };
    };

    voice.package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = pkgs.whisper-cpp.override {
        withSDL = false;
        withFFmpegSupport = false;
      };
      defaultText = lib.literalExpression ''
        pkgs.whisper-cpp.override { withSDL = false; withFFmpegSupport = false; }
      '';
      description = ''
        whisper.cpp, for voice input in the chat (Settings → Voice): speech becomes text on this
        machine with its `whisper-cli`. Turning voice input on downloads the speech model once
        (about 148 MB, SHA-256 pinned) into /var/lib/melissa/models. Without SDL and ffmpeg, which
        Melissa doesn't need (it hands whisper-cli plain 16 kHz WAV files). `null`: no voice input.
      '';
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
          description = "Melissa server and its agents";
          inherit home;
          shell = "${pkgs.shadow}/bin/nologin";
          # Agent users (crates/agent/src/sandbox/users.rs): each agent sandbox runs as its own host ids
          # from this range, mapped by NixOS's newuidmap/newgidmap wrappers. 0x70000000: a block systemd's
          # id layout leaves unused, clear of the automatic ranges of normal users.
          subUidRanges = [
            {
              startUid = 1879048192;
              count = 65536;
            }
          ];
          subGidRanges = [
            {
              startGid = 1879048192;
              count = 65536;
            }
          ];
        };
        users.groups.melissa = { };
        # May read the server's k3s kubeconfigs in /run/melissa-kube; only the server's unit has it.
        users.groups.melissa-kube = { };

        environment.systemPackages = [ cfg.package ];
        systemd.packages = [ cfg.package ];
        systemd.tmpfiles.packages = [ cfg.package ];
        environment.etc."melissa/server.toml".source = serverToml;

        systemd.services.melissa-server = {
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
          # runsc for the startup summary's gVisor line (k3s runs it, see below).
          ++ lib.optional (cfg.k3s.enable && cfg.k3s.gvisor.enable) cfg.k3s.gvisor.package
          ++ [ "/run/current-system/sw" ];
          # The release file (version, revision, MELISSA_RUNTIME=nix) is in the store, not /usr.
          environment = {
            MELISSA_RELEASE_FILE = "${cfg.package}/lib/melissa/release";
          }
          // lib.optionalAttrs (cfg.voice.package != null) {
            MELISSA_WHISPER_CLI = "${cfg.voice.package}/bin/whisper-cli";
          };
          restartTriggers = [ serverToml ];
        };

        # Settings restarts the server through polkit (the shared rule from the package: restart
        # melissa-server.service, nothing else). Updates come from the NixOS configuration.
        security.polkit.enable = true;

        programs.nix-ld.enable = lib.mkIf cfg.agents.nixLd true;

        networking.firewall.allowedTCPPortRanges = lib.mkIf cfg.apps.openFirewall [
          {
            from = 30000;
            to = 30079;
          }
        ];
        networking.firewall.allowedUDPPortRanges = lib.mkIf cfg.apps.openFirewall [
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
          nodeIP = cfg.k3s.nodeIp;
          extraFlags = lib.optional (cfg.k3s.nodeIp == null) "$MELISSA_K3S_ARGS";
          manifests = lib.listToAttrs (
            map (
              name:
              lib.nameValuePair "melissa-${lib.removeSuffix ".yaml" name}" {
                source = "${manifestsDir}/${name}";
                target = "melissa-${name}";
              }
            ) manifestNames
          )
          // lib.optionalAttrs cfg.k3s.gvisor.enable {
            melissa-gvisor-runtimeclass = {
              source = "${gvisorDir}/runtimeclass.yaml";
              target = "melissa-gvisor-runtimeclass.yaml";
            };
          };
          images = lib.optional cfg.k3s.airgapImages airgapImages ++ cfg.k3s.images;
        };

        # The shared k3s config (kubeconfig mode, node label); no SELinux setting (per packaging).
        environment.etc."rancher/k3s/config.yaml.d/10-melissa.yaml".source =
          (import ./common.nix).root + "/etc/rancher/k3s/config.yaml.d/10-melissa.yaml";

        systemd.services.k3s.serviceConfig = lib.mkIf (cfg.k3s.nodeIp == null) {
          RuntimeDirectory = "melissa-k3s";
          ExecStartPre = [ "${cfg.package}/libexec/melissa/k3s-node-ip /run/melissa-k3s/env" ];
          EnvironmentFile = lib.mkForce (
            lib.optional (config.services.k3s.environmentFile != null) config.services.k3s.environmentFile
            ++ [ "-/run/melissa-k3s/env" ]
          );
        };

        # gVisor: containerd (a child of k3s) finds containerd-shim-runsc-v1 and runsc on k3s's PATH.
        systemd.services.k3s.path = lib.mkIf cfg.k3s.gvisor.enable [ cfg.k3s.gvisor.package ];
        systemd.tmpfiles.settings."10-melissa-gvisor" = lib.mkIf cfg.k3s.gvisor.enable {
          "/var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl"."L+".argument =
            "${gvisorTemplate}";
        };

        # Pods reach the API server and kubelet on the host through cni0 (the images' firewalld
        # zone trusts the pod and service networks the same way). Opens nothing to the LAN.
        networking.firewall.trustedInterfaces = [ "cni0" ];

        systemd.services.melissa-kubeconfig = {
          description = "Melissa server's k3s credentials (read-only ServiceAccount kubeconfig)";
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
