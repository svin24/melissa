"""Start a throwaway Melissa for the screenshot harness.

Everything lives in a fresh temporary directory: HOME and all XDG directories point there, so
the run never reads or writes ~/.config/melissa, never talks to another Melissa's host socket,
and listens on a free port on 127.0.0.1 (never 8080). The server runs under libfaketime from a
fixed date, in UTC, so the clock it shows is the same on every run.
"""

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent

# The fixed clock: the server starts at this moment on every run.
FAKE_START = "2026-11-20 10:00:00"
DEVICE_ID = "d-5e1f0a11"
# Owner account for every run (a throwaway server; never a real password).
OWNER = "owner"
PASSWORD = "ui-shots-password-1"

# Helpers with icons, seeded into the agent registry before the server starts (fixed ids).
HELPERS = [
    ("Backup", "shield", "teal", True),
    ("Photos", "cloud", "violet", False),
    ("Updates", "bolt", "orange", False),
]

# Members invited after the claim (scripts/ui-shots/ui_shots.py): Ada and Ben have apps, Cy
# has none (the empty list). Throwaway passwords.
MEMBERS = [("ada", "ui-shots-password-2"), ("ben", "ui-shots-password-3"),
           ("cy", "ui-shots-password-4")]

# The apps the harness shows without k3s (crates/web/src/fake_apps.rs, `fake-apps` feature):
# answers of list_apps, app_status and app_logs. `user:<name>` becomes that person's id.
# Placeholder addresses only (203.0.113.x).
ADDR = "203.0.113.10"


def _app(app, kind, phase, who, port=None, ports=()):
    a = {"app": app, "kind": kind, "phase": phase, "ports": [
        {"name": n, "port": p, "protocol": proto, "web": False,
         "address": f"{ADDR}:{p}"} for n, p, proto in ports]}
    if port:
        a["port"] = port
        a["url"] = f"http://{ADDR}:{port}"
    if who:
        a["installed_for"] = f"user:{who}"
    return a


FAKE_APPS = {
    "list_apps": {"apps": [
        _app("minecraft", "games", "ready", "ben", ports=[("game", 30000, "tcp")]),
        _app("luanti", "games", "stopped", "ada", ports=[("game", 30001, "udp")]),
        _app("jellyfin", "media", "ready", OWNER, port=30096),
        _app("navidrome", "media", "failing", "ben", port=30453),
        _app("home-assistant", "home", "ready", "ada", port=30123),
        _app("syncthing", "files", "ready", OWNER, port=30384),
        _app("pihole", "network", "ready", None, port=30080),
        _app("forgejo", "developer", "stopped", "ben", port=30030),
    ]},
    "app_status": {"minecraft": {
        "k3s_reachable": True, "app": "minecraft", "phase": "ready",
        "agent": "d-5e1f0a11.a1", "agent_name": "Backup", "device": "this device",
        "ports": [{"name": "game", "port": 30000, "protocol": "tcp", "web": False,
                   "address": f"{ADDR}:30000"},
                  {"name": "map", "port": 30010, "protocol": "tcp", "web": False,
                   "address": f"{ADDR}:30010"}],
        "version": "1.21.4", "image": "docker.io/itzg/minecraft-server:java21",
        "installed": True, "replicas": 1, "ready_replicas": 1,
        "pods": [{"name": "minecraft-0", "restarts": 0, "age_secs": 7200}],
    }},
    "app_logs": {"minecraft": {
        "app": "minecraft", "pod": "minecraft-0", "container": "server",
        "containers": ["server", "backup"], "lines": 6, "truncated": False,
        "log": "\n".join([
            "09:41:02 Starting minecraft server version 1.21.4",
            "09:41:05 Preparing level \"world\"",
            "09:41:11 Done (6.2s). For help, type \"help\"",
            "09:52:40 ada joined the game",
            "10:03:17 ben joined the game",
            "10:20:58 Saved the game",
        ]),
    }},
}

CONFIG = """\
listen = "127.0.0.1:{port}"
confirm_timeout_secs = 3600

[[backend]]
kind = "acp"
name = "shots-acp"
command = "{python}"
args = ["{agent}"]
network = "host"
sandbox = {{ enabled = false }}

[[backend]]
kind = "echo"
name = "echo-session"

[lead]
backends = ["shots-acp"]
agent_backend = "shots-acp"
"""


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def seed_state(state: Path):
    """The device id and the helpers, as the server would have saved them."""
    state.mkdir(parents=True, exist_ok=True)
    (state / "device.json").write_text(json.dumps({"id": DEVICE_ID, "name": "this device"}))
    agents = state / "agents"
    agents.mkdir(exist_ok=True)
    created = 1763632800  # 2025-11-20 10:00 UTC, before the fake clock's "now"
    records = []
    for n, (name, shape, color, pinned) in enumerate(HELPERS, start=1):
        record = {
            "id": f"{DEVICE_ID}.a{n}",
            "name": name,
            "backend": "shots-acp",
            "created_by": "the user",
            "created_at": created,
            "icon": {"shape": shape, "color": color},
        }
        if pinned:
            record["pinned"] = True
        records.append(record)
    registry = {"device": DEVICE_ID, "next": len(HELPERS) + 1, "agents": records}
    path = agents / "registry.json"
    path.write_text(json.dumps(registry, indent=2))
    path.chmod(0o600)


def helper_id(name: str) -> str:
    for n, (helper, *_rest) in enumerate(HELPERS, start=1):
        if helper == name:
            return f"{DEVICE_ID}.a{n}"
    raise KeyError(name)


class Server:
    def __init__(self, binary: Path, log: Path):
        self.binary = binary
        self.log_path = log
        self.root = Path(tempfile.mkdtemp(prefix="melissa-ui-shots-"))
        self.port = free_port()
        self.url = f"http://localhost:{self.port}"
        self.proc = None
        self.setup_code = None

    def env(self):
        r = self.root
        for d in ("home", "config/melissa", "data", "state", "run"):
            (r / d).mkdir(parents=True, exist_ok=True)
        (r / "run").chmod(0o700)
        faketime_lib = os.environ["MELISSA_SHOTS_FAKETIME_LIB"]
        env = {
            "HOME": str(r / "home"),
            "XDG_CONFIG_HOME": str(r / "config"),
            "XDG_DATA_HOME": str(r / "data"),
            "XDG_STATE_HOME": str(r / "state"),
            "XDG_RUNTIME_DIR": str(r / "run"),
            # Only what the server needs: provider detection must not see the host's tools.
            "PATH": f"{Path(sys.executable).parent}:/usr/bin:/bin",
            "TZ": "UTC",
            "LANG": "C.UTF-8",
            "RUST_LOG": "warn",
        }
        # The clock starts at FAKE_START and runs on from there, for the server and the
        # scripted agent it starts; children keep the start time instead of resetting it.
        env["LD_PRELOAD"] = faketime_lib
        env["FAKETIME"] = "@" + FAKE_START
        env["FAKETIME_DONT_RESET"] = "1"
        # Read only by a build with the `fake-apps` feature (ui_shots.py builds one).
        env["MELISSA_FAKE_APPS"] = str(self.fake_apps)
        return env

    @property
    def fake_apps(self):
        return self.root / "fake-apps.json"

    def apps_platform(self, present: bool):
        """Fake apps on (the list) or off (no app platform: k3s not set up)."""
        if present:
            self.fake_apps.write_text(json.dumps(FAKE_APPS, indent=2))
        else:
            self.fake_apps.unlink(missing_ok=True)

    def start(self):
        env = self.env()
        seed_state(self.root / "state" / "melissa")
        self.apps_platform(True)
        config = self.root / "config" / "melissa" / "server.toml"
        config.write_text(CONFIG.format(
            port=self.port, python=sys.executable, agent=HERE / "fake_agent.py"))
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [str(self.binary), "--config", str(config), "demo"],
            env=env, stdout=subprocess.PIPE, stderr=self.log, text=True, cwd=self.root,
        )
        deadline = time.time() + 60
        out = []
        while time.time() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                if self.proc.poll() is not None:
                    raise RuntimeError(f"melissa-server exited; see {self.log_path}")
                continue
            out.append(line)
            m = re.search(r"setup code (\S+)", line)
            if m:
                self.setup_code = m.group(1)
                break
        if not self.setup_code:
            raise RuntimeError("no setup code in the server's output:\n" + "".join(out))
        # Keep draining stdout so the server never blocks on a full pipe.
        threading.Thread(target=lambda: [None for _ in self.proc.stdout], daemon=True).start()
        while time.time() < deadline:
            try:
                with urllib.request.urlopen(self.url + "/healthz", timeout=2) as r:
                    if r.status == 200:
                        return
            except OSError:
                time.sleep(0.2)
        raise RuntimeError("the server did not answer /healthz")

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        if getattr(self, "log", None):
            self.log.close()
        shutil.rmtree(self.root, ignore_errors=True)
