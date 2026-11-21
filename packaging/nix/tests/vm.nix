# NixOS VM test for the Melissa service (services.melissa): two VMs on one test network, no
# internet. `server` runs Melissa with k3s (airgap images from the store), `client` stands in for
# a laptop on the LAN.
#
#   nix build .#checks.x86_64-linux.melissa-nixos -L        (KVM; about 10 minutes)
#   nix run .#checks.x86_64-linux.melissa-nixos.driverInteractive   (debug: start_all(), ...)
#
# Checked: the server starts, the UI answers on loopback only, an echo chat turn, the sandbox and
# egress integration tests as the `melissa` user (bwrap, own netns, netbridge, proxy), a
# vendor-style binary (/lib64 loader) running in the sandbox through nix-ld, k3s Ready, Melissa's
# RBAC and admission policies applied, an app deployed from a spec (deploy_app) whose NodePort
# answers from the LAN, stop/start of the app, Settings → Providers writing providers.toml, Settings'
# restart through polkit.
{ self, pkgs }:
let
  inherit (pkgs) lib;
  melissa = self.packages.${pkgs.stdenv.hostPlatform.system}.melissa;
  tests = self.packages.${pkgs.stdenv.hostPlatform.system}.melissa-integration-tests;

  # The test app: busybox httpd serving one page. Imported into k3s at start; the test deploys
  # it with deploy_app, pinned by the digest k3s reports (see the test script).
  helloImage = pkgs.dockerTools.buildImage {
    name = "docker.io/melissa-test/hello";
    tag = "1";
    copyToRoot = pkgs.buildEnv {
      name = "hello-root";
      paths = [
        pkgs.busybox
        (pkgs.writeTextDir "www/index.html" "hello from a melissa app\n")
      ];
    };
    config.Cmd = [
      "/bin/httpd"
      "-f"
      "-p"
      "8080"
      "-h"
      "/www"
    ];
  };

  # A prebuilt Linux binary as vendors ship them: loader /lib64/ld-linux-x86-64.so.2, no RPATH.
  # Runs on NixOS only through nix-ld (`services.melissa.agents.nixLd`).
  foreignHello = pkgs.runCommand "foreign-hello" { nativeBuildInputs = [ pkgs.patchelf ]; } ''
    install -Dm0755 ${pkgs.hello}/bin/hello $out/bin/foreign-hello
    patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 --remove-rpath $out/bin/foreign-hello
  '';

  serverIp = "192.168.1.2";
in
pkgs.testers.runNixOSTest {
  name = "melissa-nixos";

  nodes.server =
    { ... }:
    {
      imports = [ self.nixosModules.melissa ];
      virtualisation = {
        memorySize = 4096;
        cores = 4;
        diskSize = 8192;
      };
      services.melissa = {
        enable = true;
        apps = {
          openFirewall = true;
          addresses = [ serverIp ];
        };
        k3s = {
          airgapImages = true;
          images = [ helloImage ];
        };
        settings.lead.backends = [ "echo-chat" ];
        backends = [
          # The server's session partner (no LLM): repeats the message.
          {
            kind = "echo";
            name = "echo-chat";
          }
          {
            kind = "cli";
            name = "foreign";
            command = "/var/lib/melissa/home/.local/bin/foreign-hello";
            args = [
              "--greeting"
              "{prompt}"
            ];
            network = "none";
          }
        ];
      };
      environment.systemPackages = [
        pkgs.curl
        pkgs.jq
        tests
      ];
    };

  nodes.client =
    { ... }:
    {
      environment.systemPackages = [ pkgs.curl ];
    };

  testScript = ''
    import json, shlex

    # The web UI needs a login; root on the box uses the host socket (acts as the owner).
    SOCK = "--unix-socket /run/melissa/api.sock "

    def api(path, body=None, method=None):
        cmd = "curl -sS " + SOCK + "--max-time 120 -H 'Content-Type: application/json' "
        if body is not None:
            cmd += "-X POST -d " + shlex.quote(json.dumps(body)) + " "
        elif method:
            cmd += "-X " + method + " "
        return server.succeed(cmd + "http://localhost" + path)

    def kubectl(args):
        return server.succeed("k3s kubectl " + args)

    def approve_all():
        # The local tool API is unattended: actions with a confirmation wait in the inbox.
        for c in json.loads(api("/api/confirmations"))["confirmations"]:
            api(f"/api/confirmations/{c['id']}/approve", body={})

    def tool(name, args, confirm=False):
        body = shlex.quote(json.dumps(args))
        out = "/tmp/tool-" + name + ".json"
        server.succeed(
            f"(curl -sS {SOCK}--max-time 600 -H 'Content-Type: application/json' -X POST -d {body} "
            f"http://localhost/api/tools/{name} > {out}; echo done >> {out}.done) >/dev/null 2>&1 &"
        )
        if confirm:
            server.wait_until_succeeds("curl -sS " + SOCK + "http://localhost/api/confirmations | grep -q '\"id\"'", timeout=60)
            approve_all()
        server.wait_for_file(out + ".done", timeout=600)
        text = server.succeed("cat " + out)
        print(name, "->", text)
        return json.loads(text)

    start_all()

    with subtest("server starts, UI on loopback only"):
        server.wait_for_unit("melissa-server.service")
        server.wait_for_open_port(8080, addr="127.0.0.1")
        assert api("/healthz").strip() == "ok"
        server.succeed("curl -sSfL " + SOCK + "http://localhost/tasks | grep -qi melissa")
        # Local addresses of every listener on 8080 (column 4): only loopback.
        listen = server.succeed("ss -Hltn 'sport = :8080' | awk '{print $4}'").split()
        assert listen == ["127.0.0.1:8080"], listen
        client.wait_for_unit("multi-user.target")
        client.fail("curl -sS --max-time 5 http://${serverIp}:8080/healthz")
        server.succeed("systemctl show -p User --value melissa-server | grep -qx melissa")
        server.succeed("stat -c '%U %a' /etc/melissa | grep -qx 'melissa 750'")

    with subtest("chat turn on the echo backend"):
        rc, out = server.execute("melissa-server ask 'hello from nixos' 2>&1")
        print(out)
        if rc != 0 or "echo: hello from nixos" not in out:
            print(server.execute("journalctl -u melissa-server --no-pager | tail -40")[1])
            raise Exception("no echo reply")

    with subtest("sandbox and egress integration tests as the melissa user"):
        env = (
            "env PATH=${pkgs.bubblewrap}/bin:/run/current-system/sw/bin HOME=/var/lib/melissa/home TMPDIR=/tmp "
            "MELISSA_NETBRIDGE=${melissa}/bin/melissa-netbridge "
        )
        for t in ["sandbox", "egress"]:
            out = server.succeed(f"cd /tmp && runuser -u melissa -- {env} melissa-it-{t} --nocapture --test-threads=1 2>&1")
            print(out)
            assert "test result: ok" in out and "SKIP" not in out, out

    with subtest("vendor-style binary in the sandbox through nix-ld"):
        server.succeed(
            "install -D -o melissa -g melissa -m 0755 ${foreignHello}/bin/foreign-hello "
            "/var/lib/melissa/home/.local/bin/foreign-hello"
        )
        server.succeed("runuser -u melissa -- /var/lib/melissa/home/.local/bin/foreign-hello -g direct | grep -qx direct")
        server.succeed("systemctl restart melissa-server.service")
        server.wait_for_open_port(8080, addr="127.0.0.1")
        server.succeed(
            "curl -sS " + SOCK + "-o /dev/null -w '%{redirect_url}' -d backend=foreign -d prompt=sandboxed-hello "
            "http://localhost/tasks > /tmp/task-url"
        )
        server.wait_until_succeeds("curl -sS " + SOCK + "$(cat /tmp/task-url) | grep -q sandboxed-hello", timeout=60)

    with subtest("k3s Ready, Melissa's RBAC and admission policies applied"):
        server.wait_for_unit("k3s.service")
        server.wait_until_succeeds("k3s kubectl get node -o jsonpath='{.items[0].status.conditions[?(@.type==\"Ready\")].status}' | grep -qx True", timeout=300)
        server.succeed("k3s kubectl get node --show-labels | grep -q melissa.io/managed=true")
        # The shared k3s-node-ip pinned the default-route IPv4 (k3s's ExecStartPre).
        server.succeed("grep -qE '^MELISSA_K3S_ARGS=--node-ip=[0-9.]+$' /run/melissa-k3s/env")
        server.wait_until_succeeds("k3s kubectl -n melissa get serviceaccount melissa-server melissa-apps", timeout=120)
        server.wait_until_succeeds("test -n \"$(k3s kubectl get validatingadmissionpolicy -o name)\"", timeout=120)
        print(kubectl("get validatingadmissionpolicy,validatingadmissionpolicybinding -o name"))
        server.wait_for_unit("melissa-kubeconfig.service", timeout=330)
        server.succeed("stat -c '%U %G %a' /run/melissa-kube/server.kubeconfig | grep -qx 'root melissa-kube 640'")
        # The server's identity is read-only; its write identity cannot touch other namespaces.
        server.succeed("k3s kubectl --kubeconfig /run/melissa-kube/server.kubeconfig get nodes")
        server.fail("k3s kubectl --kubeconfig /run/melissa-kube/server.kubeconfig -n default create configmap x")
        server.fail("k3s kubectl --kubeconfig /run/melissa-kube/apps.kubeconfig -n kube-system get secrets")
        out = tool("cluster_health", {})
        print(out)

    with subtest("Traefik (bundled chart) installed offline"):
        try:
            server.wait_until_succeeds("k3s kubectl -n kube-system rollout status deploy/traefik --timeout=10s", timeout=300)
        except Exception:
            print(server.execute("k3s kubectl get pods -A -o wide; k3s kubectl get helmcharts -A")[1])
            print(server.execute("for p in $(k3s kubectl -n kube-system get pods -o name | grep helm-install); do echo == $p; k3s kubectl -n kube-system logs $p --tail=30; done")[1])
            raise

    with subtest("app deployed from a spec; NodePort answers from the LAN"):
        server.wait_until_succeeds("k3s crictl images | grep -q melissa-test/hello", timeout=180)
        digest = server.succeed(
            "k3s crictl inspecti docker.io/melissa-test/hello:1 | jq -r '.status.repoDigests[0]' | sed 's/.*@//'"
        ).strip()
        assert digest.startswith("sha256:"), digest
        spec = {
            "description": "Test app of the NixOS VM test: one static page.",
            "version": "1",
            "web": "http",
            "containers": [{
                "name": "hello",
                "image": "docker.io/melissa-test/hello:1@" + digest,
                "ports": [{"name": "http", "port": 8080}],
                "resources": {"cpu": "200m", "memory": "64Mi", "cpu_request": "10m", "memory_request": "16Mi"},
                "probe": {"port": "http", "path": "/", "startup_timeout_secs": 120, "period_secs": 5},
            }],
        }
        res = tool("deploy_app", {"app": "hello", "spec": spec}, confirm=True)
        if "error" in res:
            raise Exception(res)
        server.wait_until_succeeds("k3s kubectl -n app-hello rollout status deploy --timeout=10s", timeout=240)
        port = server.succeed("k3s kubectl -n app-hello get svc hello-port -o jsonpath='{.spec.ports[0].nodePort}'").strip()
        assert 30000 <= int(port) <= 30079, port
        client.wait_until_succeeds(f"curl -sSf --max-time 5 http://${serverIp}:{port}/ | grep -q 'hello from a melissa app'", timeout=120)
        status = tool("app_status", {"app": "hello"})
        assert "http://${serverIp}:" + port in json.dumps(status), status

    with subtest("stop and start the app"):
        tool("stop_app", {"app": "hello"}, confirm=True)
        client.wait_until_fails(f"curl -sSf --max-time 3 http://${serverIp}:{port}/", timeout=120)
        tool("start_app", {"app": "hello"}, confirm=True)
        client.wait_until_succeeds(f"curl -sSf --max-time 5 http://${serverIp}:{port}/ | grep -q 'hello from'", timeout=180)

    with subtest("settings: providers.toml and restart through polkit"):
        print(api("/api/settings/providers"))
        print(api("/api/settings/providers/foreign-hello", body={"display_name": "Foreign (test)"}))
        server.succeed("stat -c '%U %a' /etc/melissa/providers.toml | grep -qx 'melissa 600'")
        server.succeed("grep -q 'Foreign (test)' /etc/melissa/providers.toml")
        before = server.succeed("systemctl show -p InvocationID --value melissa-server").strip()
        print(api("/api/settings/updates/restart", body={"confirm": True}))
        server.wait_until_succeeds(f"test \"$(systemctl show -p InvocationID --value melissa-server)\" != {before}", timeout=60)
        server.wait_for_open_port(8080, addr="127.0.0.1")
        updates = json.loads(api("/api/settings/updates"))
        print(updates)
        # The store's release file (MELISSA_RELEASE_FILE): a Nix install, restart only, Nix commands.
        assert updates["release"]["runtime"] == "nix", updates["release"]
        assert updates["host"] == "nix", updates["host"]
        assert updates["actions"] == ["restart"], updates["actions"]
        assert any(c["command"] == "sudo nixos-rebuild switch" for c in updates["commands"]), updates["commands"]
        page = server.succeed("curl -sSf " + SOCK + "http://localhost/settings/updates")
        assert "nix flake update" in page and "podman" not in page and "git pull" not in page
        # Agent tasks moved into Settings → Developer.
        page = server.succeed("curl -sSf " + SOCK + "http://localhost/settings/developer")
        assert "Agent tasks" in page and 'href="/settings/developer"' in page
        # Agent installs from Settings are container-only: no Install button, no units, no
        # extra polkit rights on Nix.
        providers = json.loads(api("/api/settings/providers"))["providers"]
        assert all((p.get("install") or {}).get("way") != "button" for p in providers), providers
        server.fail("systemctl cat melissa-agent-install@claude.service")
        server.fail("systemctl cat melissa-agent-login@grok.service")
        server.succeed("test ! -e /etc/polkit-1/rules.d/51-melissa-agent-install.rules")
        server.fail("runuser -u melissa -- systemctl start --no-block melissa-agent-install@claude.service")

    with subtest("melissa-setup on NixOS"):
        print(server.succeed("melissa-setup 2>&1"))
  '';
}
