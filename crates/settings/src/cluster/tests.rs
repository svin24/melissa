use std::{cell::RefCell, path::PathBuf, time::Duration};

use super::*;
use crate::{factory_reset::Host, test_support::scratch};

/// `own`: the caller's cgroup; k3s-prepare runs in k3s.service's (`.control` with Delegate=yes).
struct FakeHost {
    own: Option<String>,
    calls: RefCell<Vec<String>>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            own: Some("/system.slice/k3s.service/.control".into()),
            calls: RefCell::default(),
        }
    }
}

impl Host for FakeHost {
    fn unit_active(&self, _: &str) -> bool {
        false
    }
    fn tailscale_logout(&self) -> Result<(), String> {
        Ok(())
    }
    fn stop_units(&self, _: &[&str]) -> Result<(), String> {
        Ok(())
    }
    fn own_cgroup(&self) -> Option<String> {
        self.own.clone()
    }
    fn kill_cgroup(&self, cgroup: &str) {
        let own = self.own.as_deref().unwrap_or("/system.slice/k3s.service");
        assert!(
            own != cgroup && !own.starts_with(&format!("{cgroup}/")),
            "killed the caller's own cgroup {cgroup}"
        );
        self.calls.borrow_mut().push(format!("kill {cgroup}"));
    }
    fn kill_shims(&self) {
        self.calls.borrow_mut().push("kill shims".into());
    }
    fn mounts(&self) -> Result<Vec<PathBuf>, String> {
        Ok(Vec::new())
    }
    fn unmount(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn journal_detach(&self) -> Result<(), String> {
        Ok(())
    }
    fn journal_attach(&self) {}
    fn restore_layout(&self) -> Result<(), String> {
        Ok(())
    }
    fn start_units(&self, _: &[&str]) -> Result<(), String> {
        Ok(())
    }
    fn sleep(&self, _: Duration) {}
}

fn input(device: Option<&str>) -> PrepareInput {
    PrepareInput {
        device_name: device.map(str::to_string),
        tailscale_name: Some("melissa-desktop".into()),
        host_name: Some("melissa".into()),
    }
}

struct Tree {
    root: PathBuf,
    config: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Self {
        let root = scratch(name);
        let config = root.join("etc/melissa");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(at(&root, K3S_CONFIG_D)).unwrap();
        std::fs::write(
            at(&root, K3S_CONFIG_D).join("10-melissa.yaml"),
            "write-kubeconfig-mode: \"0600\"\n",
        )
        .unwrap();
        std::fs::write(
            at(&root, K3S_CONFIG_D).join("20-container.yaml"),
            "selinux: false\n",
        )
        .unwrap();
        Self { root, config }
    }

    fn prepare(&self, device: Option<&str>) -> Result<Status, String> {
        prepare(
            &self.root,
            &self.config,
            &input(device),
            &FakeHost::default(),
            &mut |_| {},
        )
    }

    fn read(&self, abs: &str) -> String {
        std::fs::read_to_string(at(&self.root, abs)).unwrap_or_default()
    }

    /// Root's setting (what root's own actions write).
    fn set(&self, s: &Setting) {
        write_trusted(&self.root, s).unwrap();
    }

    /// Root's setting, unreadable.
    fn break_setting(&self) {
        let state = at(&self.root, STATE_DIR);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join(STATE_SETTING), "role = \"agent\"\n").unwrap();
    }

    /// The agent's join token as root keeps it.
    fn give_token(&self) {
        let state = at(&self.root, STATE_DIR);
        std::fs::create_dir_all(&state).unwrap();
        write_root(&state.join("agent-token"), b"K10abc::server:secret", 0o600).unwrap();
    }

    fn token(&self) -> PathBuf {
        at(&self.root, STATE_DIR).join("agent-token")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn agent_setting() -> Setting {
    Setting {
        role: Role::Agent,
        server_url: Some("https://100.64.0.1:6443".into()),
        node_ip: Some("100.64.0.2".parse().unwrap()),
        flannel_iface: Some("tailscale0".into()),
        peers: vec![Peer {
            name: "desktop".into(),
            address: "100.64.0.1".parse().unwrap(),
        }],
    }
}

#[test]
fn node_labels_are_dns_1123() {
    assert_eq!(
        node_label("Living room box").as_deref(),
        Some("living-room-box")
    );
    assert_eq!(node_label("  --Desktop_2--").as_deref(), Some("desktop-2"));
    assert_eq!(node_label("ÄÖ"), None);
    assert_eq!(node_label(&"a".repeat(80)).map(|n| n.len()), Some(63));
    assert_eq!(
        desired_name(Some("Laptop"), Some("melissa-x"), None),
        "laptop"
    );
    assert_eq!(
        desired_name(Some("ÄÖ"), Some("melissa-x"), None),
        "melissa-x"
    );
    assert_eq!(desired_name(None, None, Some("box")), "box");
    assert_eq!(desired_name(None, None, None), "melissa");
}

#[test]
fn names_are_pinned_and_only_renamed_while_harmless() {
    let facts = NameFacts {
        pinned: None,
        desired: "laptop".into(),
        legacy: "melissa".into(),
        data_exists: false,
        app_volumes: false,
        clustered: false,
    };
    assert_eq!(decide_name(&facts), NameDecision::New("laptop".into()));
    // An install from before pinning: k3s registered the host name; no app volumes: rename.
    let legacy = NameFacts {
        data_exists: true,
        ..facts.clone()
    };
    assert_eq!(
        decide_name(&legacy),
        NameDecision::Rename {
            from: "melissa".into(),
            to: "laptop".into()
        }
    );
    // App volumes are bound to the old node: keep it.
    let with_apps = NameFacts {
        app_volumes: true,
        ..legacy.clone()
    };
    assert_eq!(
        decide_name(&with_apps),
        NameDecision::Keep {
            name: "melissa".into(),
            wanted: Some("laptop".into())
        }
    );
    let clustered = NameFacts {
        pinned: Some("old".into()),
        clustered: true,
        ..facts.clone()
    };
    assert_eq!(decide_name(&clustered).name(), "old");
    let same = NameFacts {
        pinned: Some("laptop".into()),
        ..facts
    };
    assert_eq!(
        decide_name(&same),
        NameDecision::Keep {
            name: "laptop".into(),
            wanted: None
        }
    );
}

#[test]
fn settings_are_checked() {
    assert!(Setting::default().validate().is_ok());
    assert!(agent_setting().validate().is_ok());
    let mut s = agent_setting();
    s.server_url = Some("http://100.64.0.1:6443".into());
    assert!(s.validate().is_err());
    s.server_url = Some("https://evil\n:6443".into());
    assert!(s.validate().is_err());
    s.server_url = Some("https://[fd7a:115c:a1e0::1]:6443".into());
    assert!(s.validate().is_ok());
    s.server_url = Some("https://desktop.example.ts.net:6443".into());
    assert!(s.validate().is_ok());
    s.flannel_iface = Some("tail scale".into());
    assert!(s.validate().is_err());
    let mut s = agent_setting();
    s.node_ip = None;
    assert!(s.validate().is_err(), "an agent needs its node IP");
    let mut s = agent_setting();
    s.peers[0].name = "Not A Label".into();
    assert!(s.validate().is_err());
    // Unknown keys are refused (root reads what the server's user writes).
    let dir = scratch("cluster-setting");
    let p = dir.join(FILE_NAME);
    std::fs::write(&p, "role = \"agent\"\ntoken = \"x\"\n").unwrap();
    assert!(read_setting(&p).is_err());
    std::fs::remove_file(&p).unwrap();
    assert_eq!(read_setting(&p).unwrap(), Setting::default());
    std::os::unix::fs::symlink("/etc/hostname", &p).unwrap();
    assert!(read_setting(&p).is_err(), "a symlink is refused");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn default_is_a_single_node_server_with_a_unique_name() {
    let t = Tree::new("cluster-default");
    let st = t.prepare(None).unwrap();
    assert_eq!(st.role, Role::Server);
    // No device name yet: the Tailscale name from install.sh, not the shared `melissa`.
    assert_eq!(st.node_name, "melissa-desktop");
    let dropin = t.read(SERVER_DROPIN);
    assert!(dropin.contains("node-name: \"melissa-desktop\""), "{dropin}");
    assert!(
        !dropin.contains("node-ip"),
        "single node: k3s-node-ip picks it"
    );
    assert_eq!(t.read(ENV_FILE), "MELISSA_K3S_MODE=server\n");
    // Pinned: a device name set later doesn't rename a node whose apps keep data.
    std::fs::create_dir_all(at(&t.root, K3S_DATA).join("storage/pvc-1")).unwrap();
    let st = t.prepare(Some("Desktop")).unwrap();
    assert_eq!(st.node_name, "melissa-desktop");
    assert_eq!(st.wanted_name.as_deref(), Some("desktop"));
    // Status for the server: no secret, the node name.
    let back = read_status(&at(&t.root, STATUS_FILE)).unwrap();
    assert_eq!(back.node_name, "melissa-desktop");
}

#[test]
fn a_server_without_app_volumes_takes_the_device_name() {
    let t = Tree::new("cluster-rename");
    std::fs::create_dir_all(at(&t.root, K3S_DATA).join("server")).unwrap();
    // Legacy install (no pin): k3s used the host name `melissa`.
    let st = t.prepare(Some("Desktop")).unwrap();
    assert_eq!(st.node_name, "desktop");
    assert_eq!(take_stale_node(&t.root).as_deref(), Some("melissa"));
    assert_eq!(take_stale_node(&t.root), None, "only once");
}

#[test]
fn server_with_peers_renders_tailnet_settings() {
    let t = Tree::new("cluster-server");
    t.set(&Setting {
        node_ip: Some("100.64.0.1".parse().unwrap()),
        flannel_iface: Some("tailscale0".into()),
        peers: vec![Peer {
            name: "laptop".into(),
            address: "100.64.0.2".parse().unwrap(),
        }],
        ..Setting::default()
    });
    let st = t.prepare(Some("Desktop")).unwrap();
    assert_eq!((st.role, st.peers), (Role::Server, 1));
    let dropin = t.read(SERVER_DROPIN);
    for want in [
        "node-ip: \"100.64.0.1\"",
        "flannel-iface: \"tailscale0\"",
        "tls-san:\n  - \"100.64.0.1\"",
    ] {
        assert!(dropin.contains(want), "{want} in {dropin}");
    }
}

#[test]
fn join_and_leave_keep_the_single_node_data() {
    let t = Tree::new("cluster-join");
    let data = at(&t.root, K3S_DATA);
    std::fs::create_dir_all(data.join("storage/pvc-app")).unwrap();
    std::fs::write(data.join("storage/pvc-app/file"), "app data").unwrap();
    // An install from before pinning with app data: keeps k3s's old name.
    assert_eq!(t.prepare(Some("Laptop")).unwrap().node_name, "melissa");

    // Agent without a token: stays a server.
    t.set(&agent_setting());
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.role, Role::Server);
    assert!(!st.errors.is_empty());

    // With root's token.
    t.give_token();
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.role, Role::Agent);
    assert_eq!(
        st.node_name, "laptop",
        "a fresh agent takes the device name"
    );
    let token = t.token();
    // The server's data waits in the backup, untouched.
    assert_eq!(
        std::fs::read_to_string(at(&t.root, SERVER_BACKUP).join("storage/pvc-app/file")).unwrap(),
        "app data"
    );
    // A fresh agent data directory, marked as the agent's.
    assert_eq!(t.read(&format!("{K3S_DATA}/{DATA_MARK}")), "agent");
    assert!(!data.join("storage").exists());
    // Agent config: own file, no server-only keys, no token value.
    assert!(!at(&t.root, SERVER_DROPIN).exists());
    let cfg = t.read(AGENT_CONFIG);
    assert!(cfg.contains("server: \"https://100.64.0.1:6443\""), "{cfg}");
    assert!(cfg.contains("token-file: \"/var/lib/melissa-container/cluster/agent-token\""));
    assert!(cfg.contains("node-name: \"laptop\""));
    assert!(!cfg.contains("secret"));
    let d = at(&t.root, AGENT_CONFIG)
        .parent()
        .unwrap()
        .join("config.yaml.d");
    assert!(d.join("20-container.yaml").exists());
    assert!(
        !d.join("10-melissa.yaml").exists(),
        "write-kubeconfig-mode is server-only"
    );
    assert!(t
        .read(ENV_FILE)
        .contains("MELISSA_K3S_MODE=agent\nMELISSA_K3S_ARGS=--config="));
    // The status never carries the token.
    let status = t.read(STATUS_FILE);
    assert!(!status.contains("secret"), "{status}");
    // Again: stays an agent, nothing moves.
    std::fs::create_dir_all(&data).unwrap();
    assert_eq!(t.prepare(Some("Laptop")).unwrap().node_name, "laptop");
    assert!(data.exists());

    // Leave: the server's data and its node name come back; the agent data is a backup.
    t.set(&Setting::default());
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!((st.role, st.node_name.as_str()), (Role::Server, "melissa"));
    assert_eq!(
        std::fs::read_to_string(data.join("storage/pvc-app/file")).unwrap(),
        "app data"
    );
    assert!(at(&t.root, AGENT_BACKUP).exists());
    assert!(!token.exists(), "the agent token goes");
    assert!(!at(&t.root, SERVER_BACKUP).exists());
}

#[test]
fn a_broken_setting_never_makes_an_agent() {
    let t = Tree::new("cluster-broken");
    t.break_setting();
    t.give_token();
    let st = t.prepare(None).unwrap();
    assert_eq!(st.role, Role::Server);
    assert!(!st.errors.is_empty());
}

#[test]
fn an_existing_backup_stops_the_switch() {
    let t = Tree::new("cluster-backup");
    std::fs::create_dir_all(at(&t.root, K3S_DATA).join("server")).unwrap();
    std::fs::create_dir_all(at(&t.root, SERVER_BACKUP).join("server")).unwrap();
    t.set(&agent_setting());
    t.give_token();
    assert!(
        t.prepare(None).is_err(),
        "never overwrite a kept server's data"
    );
    assert!(at(&t.root, K3S_DATA).join("server").exists());
    assert!(at(&t.root, SERVER_BACKUP).join("server").exists());
}

/// Records what root ran; `laptop` is the only node it can remove.
#[derive(Default)]
struct Rec {
    calls: std::sync::Mutex<Vec<String>>,
}

impl Rec {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn push(&self, c: &str) {
        self.calls.lock().unwrap().push(c.into());
    }
}

impl Runner for Rec {
    fn create_token(&self, ttl: &str) -> Result<String, String> {
        assert_eq!(ttl, "1h");
        self.push("token");
        Ok("K10tok::abc.def".into())
    }
    fn remove_node(&self, name: &str) -> Result<(), String> {
        self.push(&format!("remove {name}"));
        (name == "laptop")
            .then_some(())
            .ok_or("no such node".into())
    }
    fn restart_k3s(&self) -> Result<(), String> {
        self.push("restart");
        Ok(())
    }
    fn refresh_firewall(&self) -> Result<(), String> {
        self.push("firewall");
        Ok(())
    }
    fn node_roles(&self) -> Result<String, String> {
        Err("not used".into())
    }
    fn label_node(&self, _: &str, _: Role) -> Result<(), String> {
        Err("not used".into())
    }
}

fn server_setting() -> Setting {
    Setting {
        node_ip: Some("100.64.0.1".parse().unwrap()),
        flannel_iface: Some("tailscale0".into()),
        peers: vec![Peer {
            name: "laptop".into(),
            address: "100.64.0.2".parse().unwrap(),
        }],
        ..Setting::default()
    }
}

fn set_status(root: &Path, name: &str) {
    let status = Status {
        node_name: name.into(),
        ..Status::default()
    };
    write_root(
        &at(root, STATUS_FILE),
        &serde_json::to_vec(&status).unwrap(),
        0o644,
    )
    .unwrap();
}

fn trusted(t: &Tree) -> Setting {
    read_setting(&at(&t.root, STATE_DIR).join(STATE_SETTING)).unwrap()
}

#[test]
fn the_server_labels_its_own_and_the_joined_nodes_roles() {
    let s = server_setting();
    let listed = "desktop server\nlaptop \nstranger \n\n";
    // Its own already has it; the joined laptop gets `agent`; a node that isn't a peer: left.
    assert_eq!(
        role_labels_needed(&s, "desktop", listed),
        vec![("laptop".to_string(), Role::Agent)]
    );
    // A node claiming the wrong role is corrected.
    assert_eq!(
        role_labels_needed(&s, "desktop", "desktop agent\nlaptop agent\n"),
        vec![("desktop".to_string(), Role::Server)]
    );
    // Not joined yet: nothing to do. An agent, or a server without peers: never.
    assert!(role_labels_needed(&s, "desktop", "desktop server\n").is_empty());
    assert!(role_labels_needed(&agent_setting(), "laptop", "desktop \nlaptop \n").is_empty());
    assert!(role_labels_needed(&Setting::default(), "desktop", "desktop \n").is_empty());
}

struct LabelRunner {
    listed: &'static str,
    labelled: RefCell<Vec<(String, Role)>>,
}

impl Runner for LabelRunner {
    fn create_token(&self, _: &str) -> Result<String, String> {
        unreachable!()
    }
    fn remove_node(&self, _: &str) -> Result<(), String> {
        unreachable!()
    }
    fn restart_k3s(&self) -> Result<(), String> {
        unreachable!()
    }
    fn refresh_firewall(&self) -> Result<(), String> {
        unreachable!()
    }
    fn node_roles(&self) -> Result<String, String> {
        Ok(self.listed.into())
    }
    fn label_node(&self, name: &str, role: Role) -> Result<(), String> {
        self.labelled.borrow_mut().push((name.into(), role));
        Ok(())
    }
}

#[test]
fn label_nodes_reads_roots_setting_and_this_nodes_name() {
    let t = Tree::new("cluster-label");
    let runner = LabelRunner {
        listed: "desktop \nlaptop \n",
        labelled: RefCell::default(),
    };
    // No setting: a single device, no kubectl at all (node_roles would be called otherwise).
    assert_eq!(label_nodes(&t.root, &t.config, &runner), Ok(0));
    // The server's user's file is not root's setting.
    std::fs::write(
        t.config.join(FILE_NAME),
        toml::to_string(&server_setting()).unwrap(),
    )
    .unwrap();
    assert_eq!(label_nodes(&t.root, &t.config, &runner), Ok(0));
    t.set(&server_setting());
    set_status(&t.root, "desktop");
    assert_eq!(label_nodes(&t.root, &t.config, &runner), Ok(2));
    assert_eq!(
        *runner.labelled.borrow(),
        vec![
            ("desktop".to_string(), Role::Server),
            ("laptop".to_string(), Role::Agent)
        ]
    );
}

fn ask(t: &Tree, request: &str, runner: &Rec) -> Answer {
    write_private(&t.config.join(REQUEST_FILE), request.as_bytes()).unwrap();
    run_request(&t.root, &t.config, runner)
}

#[test]
fn the_servers_user_can_neither_join_nor_invite() {
    let t = Tree::new("cluster-gate");
    set_status(&t.root, "desktop");
    let before = std::fs::read_dir(at(&t.root, STATE_DIR)).map_or(0, |d| d.count());
    let rec = Rec::default();
    // Joining: refused before the token is read; the handed token goes.
    write_private(&t.config.join(JOIN_TOKEN_FILE), b"K10join::x.y").unwrap();
    let a = ask(
        &t,
        r#"{"action":"join","server_url":"https://100.64.0.9:6443","server_name":"home","own_address":"100.64.0.1"}"#,
        &rec,
    );
    assert!(!a.ok);
    assert!(a.error.unwrap().contains("owner's confirmation"));
    assert!(!t.config.join(JOIN_TOKEN_FILE).exists());
    assert!(!t.config.join(REQUEST_FILE).exists());
    // An invitation: refused, no token minted.
    let a = ask(
        &t,
        r#"{"action":"invite","own_address":"100.64.0.1","peer":{"name":"laptop","address":"100.64.0.2"}}"#,
        &rec,
    );
    assert!(!a.ok);
    assert!(!t.config.join(INVITATION_TOKEN_FILE).exists());
    let answer = std::fs::read_to_string(t.config.join(ANSWER_FILE)).unwrap();
    assert!(!answer.contains("K10"), "{answer}");
    // Nothing ran, root's state is as it was.
    assert!(rec.calls().is_empty(), "{:?}", rec.calls());
    assert_eq!(
        std::fs::read_dir(at(&t.root, STATE_DIR)).map_or(0, |d| d.count()),
        before
    );
    assert!(!t.token().exists());
    // Writing the old files and restarting k3s changes nothing either.
    std::fs::write(
        t.config.join(FILE_NAME),
        toml::to_string(&agent_setting()).unwrap(),
    )
    .unwrap();
    write_private(&t.config.join(JOIN_TOKEN_FILE), b"K10join::x.y").unwrap();
    let st = t.prepare(None).unwrap();
    assert_eq!((st.role, st.peers), (Role::Server, 0));
    assert!(!t.token().exists());
    assert_eq!(trusted(&t), Setting::default());
    assert_eq!(
        read_trusted(&t.root, &t.config).unwrap(),
        Setting::default()
    );
    // A bad request, or none: refused.
    assert!(!ask(&t, r#"{"action":"restart"}"#, &rec).ok);
    assert!(!run_request(&t.root, &t.config, &rec).ok, "no request");
}

#[test]
fn root_itself_joins_and_invites() {
    let t = Tree::new("cluster-root-actions");
    set_status(&t.root, "desktop");
    let rec = Rec::default();
    let invite = |name: &str, addr: &str| Invite {
        own_address: "100.64.0.1".parse().unwrap(),
        peer: Peer {
            name: name.into(),
            address: addr.parse().unwrap(),
        },
    };
    assert!(root_invite(
        &t.root,
        &t.config,
        Origin::Root,
        &invite("desktop", "100.64.0.2"),
        &rec
    )
    .is_err());
    let token = root_invite(
        &t.root,
        &t.config,
        Origin::Root,
        &invite("laptop", "100.64.0.2"),
        &rec,
    )
    .unwrap();
    assert_eq!(token, "K10tok::abc.def");
    assert_eq!(trusted(&t), server_setting());
    assert!(
        at(&t.root, STATE_DIR).join(HAS_PEERS).exists(),
        "the label timer runs"
    );
    // The token from the running server, then the restart that moves the node IP.
    assert_eq!(rec.calls(), ["firewall", "token", "restart"]);
    // A second device: no restart (the node IP is already on the tailnet).
    root_invite(
        &t.root,
        &t.config,
        Origin::Root,
        &invite("tablet", "100.64.0.3"),
        &rec,
    )
    .unwrap();
    assert_eq!(rec.calls().len(), 5);
    assert_eq!(trusted(&t).peers.len(), 2);

    // Joining (another device's view): root's token, 0600, never in the setting.
    let t = Tree::new("cluster-root-join");
    set_status(&t.root, "laptop");
    let rec = Rec::default();
    let join = Join {
        server_url: "https://100.64.0.1:6443".into(),
        server_name: "Desktop".into(),
        own_address: "100.64.0.2".parse().unwrap(),
    };
    assert!(root_join(&t.root, Origin::Server, &join, "K10join::x.y", &rec).is_err());
    assert!(rec.calls().is_empty());
    assert!(root_join(&t.root, Origin::Root, &join, "two words", &rec).is_err());
    let same = Join {
        server_name: "laptop".into(),
        ..join.clone()
    };
    assert!(root_join(&t.root, Origin::Root, &same, "K10join::x.y", &rec).is_err());
    let by_name = Join {
        server_url: "https://desktop.example.ts.net:6443".into(),
        ..join.clone()
    };
    assert!(
        root_join(&t.root, Origin::Root, &by_name, "K10join::x.y", &rec).is_err(),
        "the server is named by its tailnet address (firewall)"
    );
    root_join(&t.root, Origin::Root, &join, "K10join::x.y\n", &rec).unwrap();
    let s = trusted(&t);
    assert_eq!((s.role, s.peers[0].name.as_str()), (Role::Agent, "desktop"));
    let text = std::fs::read_to_string(at(&t.root, STATE_DIR).join(STATE_SETTING)).unwrap();
    assert!(!text.contains("K10join"), "{text}");
    assert_eq!(std::fs::read_to_string(t.token()).unwrap(), "K10join::x.y");
    for f in [t.token(), at(&t.root, STATE_DIR).join(STATE_SETTING)] {
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(rec.calls(), ["firewall", "restart"]);
    // An agent invites nobody.
    assert!(root_invite(
        &t.root,
        &t.config,
        Origin::Root,
        &invite("tablet", "100.64.0.3"),
        &rec
    )
    .is_err());
}

#[test]
fn the_servers_user_may_remove_devices_and_leave() {
    let t = Tree::new("cluster-narrowing");
    let rec = Rec::default();
    let remove = |name: &str| {
        ask(
            &t,
            &format!(r#"{{"action":"remove_node","name":"{name}"}}"#),
            &rec,
        )
    };
    assert!(!remove("x; rm -rf /").ok);
    // Root removes only a device it invited (a peer), never its own node.
    assert!(!remove("laptop").ok, "no peers");
    let mut s = server_setting();
    s.peers.push(Peer {
        name: "tablet".into(),
        address: "100.64.0.3".parse().unwrap(),
    });
    t.set(&s);
    assert!(!remove("laptop").ok, "own node name unknown");
    set_status(&t.root, "desktop");
    assert!(!remove("stranger").ok, "not a peer");
    assert!(!remove("desktop").ok, "this device");
    s.peers.push(Peer {
        name: "desktop".into(),
        address: "100.64.0.4".parse().unwrap(),
    });
    t.set(&s);
    let a = remove("desktop");
    assert!(a.error.unwrap().contains("own node"), "even when listed");
    assert!(rec.calls().is_empty());
    // One of several: its node goes, the firewall closes to it, no restart.
    assert!(remove("laptop").ok);
    assert_eq!(rec.calls(), ["remove laptop", "firewall"]);
    let after = trusted(&t);
    assert_eq!(after.peers.len(), 2);
    assert_eq!(after.node_ip, s.node_ip);
    // The last peers gone: back to single-node settings (no tailnet node IP), applied by a
    // restart. A node that can't be deleted is still dropped (the answer says so).
    t.set(&server_setting());
    let rec2 = Rec::default();
    let a = ask(&t, r#"{"action":"remove_node","name":"laptop"}"#, &rec2);
    assert!(a.ok, "{a:?}");
    assert_eq!(rec2.calls(), ["remove laptop", "firewall", "restart"]);
    assert_eq!(trusted(&t), Setting::default());
    assert!(!at(&t.root, STATE_DIR).join(HAS_PEERS).exists());
    let st = t.prepare(None).unwrap();
    assert_eq!(st.peers, 0);
    assert!(!t.read(SERVER_DROPIN).contains("node-ip"));

    // Leaving: root's setting back to the default, firewall, restart.
    t.set(&agent_setting());
    let rec3 = Rec::default();
    assert!(ask(&t, r#"{"action":"leave"}"#, &rec3).ok);
    assert_eq!(rec3.calls(), ["firewall", "restart"]);
    assert_eq!(trusted(&t), Setting::default());
}

#[test]
fn an_older_install_is_taken_over_once() {
    // k3s ran under the old code (a pinned name) as an agent set up the old way: cluster.toml
    // and the handed token in /etc/melissa, root's token already moved.
    let t = server_with_app("cluster-takeover");
    std::fs::remove_file(at(&t.root, STATE_DIR).join(STATE_SETTING)).unwrap();
    std::fs::write(
        t.config.join(FILE_NAME),
        toml::to_string(&agent_setting()).unwrap(),
    )
    .unwrap();
    write_private(&t.config.join(JOIN_TOKEN_FILE), b"K10abc::server:secret\n").unwrap();
    // Before k3s's start takes it over, the firewall already sees it.
    assert_eq!(read_trusted(&t.root, &t.config).unwrap(), agent_setting());
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.role, Role::Agent);
    assert_eq!(trusted(&t), agent_setting());
    assert_eq!(
        std::fs::read_to_string(t.token()).unwrap(),
        "K10abc::server:secret"
    );
    assert!(!t.config.join(FILE_NAME).exists());
    assert!(!t.config.join(JOIN_TOKEN_FILE).exists());
    // From now on the old file is ignored: another server written there changes nothing.
    let mut other = agent_setting();
    other.server_url = Some("https://100.64.0.66:6443".into());
    std::fs::write(t.config.join(FILE_NAME), toml::to_string(&other).unwrap()).unwrap();
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.server_url.as_deref(), Some("https://100.64.0.1:6443"));
    assert!(t.read(AGENT_CONFIG).contains("100.64.0.1:6443"));

    // A server with the defaults (no cluster.toml): just works, root keeps the default.
    let t = server_with_app("cluster-takeover-server");
    std::fs::remove_file(at(&t.root, STATE_DIR).join(STATE_SETTING)).unwrap();
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!((st.role, st.node_name.as_str()), (Role::Server, "melissa"));
    assert_eq!(trusted(&t), Setting::default());

    // An older install with an unreadable cluster.toml: not taken over until it is fixed.
    let t = server_with_app("cluster-takeover-broken");
    std::fs::remove_file(at(&t.root, STATE_DIR).join(STATE_SETTING)).unwrap();
    std::fs::write(t.config.join(FILE_NAME), "peers = 3\n").unwrap();
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.role, Role::Server);
    assert!(!st.errors.is_empty());
    assert!(!at(&t.root, STATE_DIR).join(STATE_SETTING).exists());
}

#[test]
fn a_factory_reset_wipes_roots_cluster_state() {
    assert!(crate::factory_reset::WIPE.contains(&STATE_DIR));
}

/// Runs [`run_request`] in place of systemd (the root oneshot).
struct InlineStarter {
    root: PathBuf,
    config: PathBuf,
    rec: Rec,
}

impl Starter for InlineStarter {
    fn start(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            run_request(&self.root, &self.config, &self.rec);
            Ok(())
        })
    }
}

#[tokio::test]
async fn control_asks_root_and_audits() {
    let t = Tree::new("cluster-control");
    let starter = Arc::new(InlineStarter {
        root: t.root.clone(),
        config: t.config.clone(),
        rec: Rec::default(),
    });
    let audit = AuditLog::new(t.root.join("audit.jsonl"));
    let c = ClusterControl::new(
        t.config.clone(),
        at(&t.root, STATUS_FILE),
        starter.clone(),
        Some(audit),
    );
    set_status(&t.root, "desktop");
    let join = Join {
        server_url: "https://100.64.0.9:6443".into(),
        server_name: "Desktop".into(),
        own_address: "100.64.0.1".parse().unwrap(),
    };
    // Two devices of one name can't be in one cluster (checked before root is asked).
    let same = Peer {
        name: "desktop".into(),
        address: "100.64.0.2".parse().unwrap(),
    };
    assert!(matches!(
        c.create_invitation("100.64.0.1".parse().unwrap(), same, "owner")
            .await,
        Err(ClusterError::Invalid(_))
    ));
    assert!(matches!(
        c.join(join.clone(), Secret::new("K10join::x.y".into()), "owner")
            .await,
        Err(ClusterError::Invalid(_))
    ));
    set_status(&t.root, "home");
    // Root refuses both for now (Q108); nothing ran, no token anywhere.
    let e = c
        .create_invitation(
            "100.64.0.1".parse().unwrap(),
            Peer {
                name: "laptop".into(),
                address: "100.64.0.2".parse().unwrap(),
            },
            "owner",
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("owner's confirmation"), "{e}");
    let e = c
        .join(join, Secret::new("K10join::x.y".into()), "owner")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("owner's confirmation"), "{e}");
    assert!(!t.config.join(JOIN_TOKEN_FILE).exists());
    assert!(starter.rec.calls().is_empty());

    // Root's own invitation; then the owner removes the device from the server's side.
    root_invite(
        &t.root,
        &t.config,
        Origin::Root,
        &Invite {
            own_address: "100.64.0.1".parse().unwrap(),
            peer: Peer {
                name: "laptop".into(),
                address: "100.64.0.2".parse().unwrap(),
            },
        },
        &Rec::default(),
    )
    .unwrap();
    c.remove_device("laptop", "owner").await.unwrap();
    assert_eq!(trusted(&t), Setting::default());
    assert!(c.remove_device("Bad Name", "owner").await.is_err());
    c.leave("owner").await.unwrap();

    let log = std::fs::read_to_string(t.root.join("audit.jsonl")).unwrap();
    for action in [
        "cluster.invitation_created",
        "cluster.device_removed",
        "cluster.join",
        "cluster.leave",
    ] {
        assert!(log.contains(action), "{action}");
    }
    assert!(!log.contains("K10"), "no token in the audit log");
}

#[test]
fn the_switch_from_inside_k3s_never_kills_its_caller() {
    // k3s-prepare runs `cluster prepare` inside k3s.service's cgroup: a role switch kills the
    // pods and the shims (by name), never that cgroup (Restart=always would loop on it).
    for own in [
        Some("/system.slice/k3s.service/.control"),
        Some("/system.slice/k3s.service"),
        None,
    ] {
        let host = FakeHost {
            own: own.map(str::to_string),
            calls: RefCell::default(),
        };
        crate::factory_reset::kill_leftovers(&host);
        assert_eq!(
            *host.calls.borrow(),
            vec!["kill /kubepods", "kill /kubepods.slice", "kill shims"],
            "{own:?}"
        );
    }
    // Through a real switch: the data moves, the caller lives.
    let t = Tree::new("cluster-own-cgroup");
    std::fs::create_dir_all(at(&t.root, K3S_DATA).join("server")).unwrap();
    t.join();
    let host = FakeHost::default();
    let st = prepare(&t.root, &t.config, &input(None), &host, &mut |_| {}).unwrap();
    assert_eq!(st.role, Role::Agent);
    assert!(host.calls.borrow().contains(&"kill shims".to_string()));
    assert!(!host
        .calls
        .borrow()
        .iter()
        .any(|c| c.contains("k3s.service")));
    // A reset from its own unit does kill k3s.service's leftovers.
    let host = FakeHost {
        own: Some("/system.slice/melissa-factory-reset.service".into()),
        calls: RefCell::default(),
    };
    crate::factory_reset::kill_leftovers(&host);
    assert!(host
        .calls
        .borrow()
        .contains(&"kill /system.slice/k3s.service".to_string()));
}

// --- a role switch cut short anywhere -----------------------------------------------------------

/// A step of a switch, and the state a crash right after it leaves.
type Cut = (&'static str, fn(&Tree));

/// A server with an app's data and the pinned name `melissa` (from before pinning).
fn server_with_app(name: &str) -> Tree {
    let t = Tree::new(name);
    let data = at(&t.root, K3S_DATA);
    std::fs::create_dir_all(data.join("storage/pvc-app")).unwrap();
    std::fs::write(data.join("storage/pvc-app/file"), "app data").unwrap();
    assert_eq!(t.prepare(Some("Laptop")).unwrap().node_name, "melissa");
    t
}

impl Tree {
    fn app_data_at(&self, abs: &str) -> bool {
        std::fs::read_to_string(at(&self.root, abs).join("storage/pvc-app/file")).ok()
            == Some("app data".into())
    }

    fn join(&self) {
        self.set(&agent_setting());
        self.give_token();
    }

    /// Back to a server: the app's data in place, its old name, nothing of it anywhere else.
    fn assert_server_again(&self) {
        self.set(&Setting::default());
        let st = self.prepare(Some("Laptop")).unwrap();
        assert_eq!((st.role, st.node_name.as_str()), (Role::Server, "melissa"));
        assert!(self.app_data_at(K3S_DATA));
        assert!(!at(&self.root, SERVER_BACKUP).exists());
        assert!(!self.app_data_at(AGENT_BACKUP));
        assert!(!at(&self.root, STATE_DIR).join("agent-token").exists());
    }
}

#[test]
fn joining_cut_short_at_any_step_finishes_or_goes_back() {
    // Each step of server → agent, as a crash would leave it.
    let steps: [Cut; 4] = [
        ("token taken, nothing moved", |_| {}),
        ("server name saved", |t| {
            std::fs::write(at(&t.root, STATE_DIR).join("server-node-name"), "melissa").unwrap();
        }),
        ("data moved to the backup", |t| {
            std::fs::write(at(&t.root, STATE_DIR).join("server-node-name"), "melissa").unwrap();
            std::fs::rename(at(&t.root, K3S_DATA), at(&t.root, SERVER_BACKUP)).unwrap();
        }),
        ("an empty data directory made, not marked", |t| {
            std::fs::write(at(&t.root, STATE_DIR).join("server-node-name"), "melissa").unwrap();
            std::fs::rename(at(&t.root, K3S_DATA), at(&t.root, SERVER_BACKUP)).unwrap();
            std::fs::create_dir_all(at(&t.root, K3S_DATA)).unwrap();
        }),
    ];
    for (what, cut) in steps {
        for finish in [true, false] {
            let t = server_with_app("cluster-cut-join");
            t.join();
            cut(&t);
            if finish {
                let st = t.prepare(Some("Laptop")).unwrap();
                assert_eq!(
                    (st.role, st.node_name.as_str()),
                    (Role::Agent, "laptop"),
                    "{what}"
                );
                assert!(t.app_data_at(SERVER_BACKUP), "{what}");
                assert!(!t.app_data_at(K3S_DATA), "{what}");
            }
            // Leaving afterwards (or instead) brings everything back.
            t.assert_server_again();
        }
    }
}

#[test]
fn leaving_cut_short_at_any_step_never_loses_the_servers_data() {
    let steps: [Cut; 4] = [
        ("nothing done", |_| {}),
        ("an old agent backup removed", |_| {}),
        ("agent data moved to its backup", |t| {
            std::fs::rename(at(&t.root, K3S_DATA), at(&t.root, AGENT_BACKUP)).unwrap();
        }),
        // The step that lost data before: the server's data is back, its name and the role
        // file not yet.
        ("server data restored", |t| {
            std::fs::rename(at(&t.root, K3S_DATA), at(&t.root, AGENT_BACKUP)).unwrap();
            std::fs::rename(at(&t.root, SERVER_BACKUP), at(&t.root, K3S_DATA)).unwrap();
            std::fs::write(at(&t.root, STATE_DIR).join("data-role"), "agent").unwrap();
        }),
    ];
    for (what, cut) in steps {
        let t = server_with_app("cluster-cut-leave");
        t.join();
        assert_eq!(t.prepare(Some("Laptop")).unwrap().role, Role::Agent);
        std::fs::create_dir_all(at(&t.root, K3S_DATA).join("agent/images")).unwrap();
        cut(&t);
        t.assert_server_again();
        assert!(
            at(&t.root, AGENT_BACKUP).join("agent/images").exists(),
            "{what}: the agent's data is the backup"
        );
        // Join and leave again: the old agent backup goes, the server's data never does.
        t.join();
        assert_eq!(
            t.prepare(Some("Laptop")).unwrap().role,
            Role::Agent,
            "{what}"
        );
        t.assert_server_again();
    }
}

#[test]
fn an_unmarked_agent_backup_is_kept_aside_not_deleted() {
    let t = server_with_app("cluster-unmarked-backup");
    t.join();
    t.prepare(Some("Laptop")).unwrap();
    // A backup from before marks, maybe a server's data: never removed.
    let backup = at(&t.root, AGENT_BACKUP);
    std::fs::create_dir_all(backup.join("storage")).unwrap();
    std::fs::write(backup.join("storage/only-copy"), "x").unwrap();
    t.set(&Setting::default());
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.role, Role::Server);
    assert!(
        st.errors.iter().any(|e| e.contains("kept as")),
        "{:?}",
        st.errors
    );
    let kept: Vec<_> = std::fs::read_dir(backup.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains("agent-backup.kept-")
        })
        .collect();
    assert_eq!(kept.len(), 1);
    assert!(kept[0].path().join("storage/only-copy").exists());
}

#[test]
fn an_agent_never_leaves_by_itself() {
    let t = server_with_app("cluster-agent-stays");
    t.join();
    assert_eq!(t.prepare(Some("Laptop")).unwrap().role, Role::Agent);
    let token = at(&t.root, STATE_DIR).join("agent-token");
    // A broken setting: k3s waits; nothing moves, the token stays.
    t.break_setting();
    let e = t.prepare(Some("Laptop")).unwrap_err();
    assert!(e.contains("unreadable"), "{e}");
    assert!(token.exists());
    assert!(t.app_data_at(SERVER_BACKUP));
    assert_eq!(t.read(&format!("{K3S_DATA}/{DATA_MARK}")), "agent");
    // A missing token: still an agent, and it says why.
    t.set(&agent_setting());
    std::fs::remove_file(&token).unwrap();
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.role, Role::Agent);
    assert!(!st.errors.is_empty());
    assert!(t.app_data_at(SERVER_BACKUP));
}

#[test]
fn a_broken_setting_on_a_clustered_server_changes_nothing() {
    let t = Tree::new("cluster-broken-server");
    std::fs::create_dir_all(at(&t.root, K3S_DATA).join("server")).unwrap();
    t.set(&server_setting());
    assert_eq!(t.prepare(Some("Desktop")).unwrap().node_name, "melissa");
    t.break_setting();
    let st = t.prepare(Some("Desktop")).unwrap();
    assert_eq!(st.role, Role::Server);
    assert_eq!(
        st.node_name, "melissa",
        "no rename while the peers are unknown"
    );
    assert!(!st.errors.is_empty());
}

#[test]
fn joining_under_the_servers_own_name_is_refused() {
    let t = server_with_app("cluster-same-name");
    let mut s = agent_setting();
    s.peers[0].name = "laptop".into();
    t.set(&s);
    t.give_token();
    let st = t.prepare(Some("Laptop")).unwrap();
    assert_eq!(st.role, Role::Server);
    assert!(
        st.errors.iter().any(|e| e.contains("rename this device")),
        "{:?}",
        st.errors
    );
    assert!(t.app_data_at(K3S_DATA), "nothing moved");
}

#[test]
fn root_never_blocks_on_a_fifo_and_keeps_copied_dropins_private() {
    let dir = scratch("cluster-fifo");
    let fifo = dir.join(FILE_NAME);
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    // Would block forever in open() without O_NONBLOCK (no writer).
    let e = read_setting(&fifo).unwrap_err();
    assert!(e.contains("not a regular file"), "{e}");
    let _ = std::fs::remove_dir_all(dir);

    let t = server_with_app("cluster-dropin-mode");
    std::fs::write(
        at(&t.root, K3S_CONFIG_D).join("90-local.yaml"),
        "token: local\n",
    )
    .unwrap();
    t.join();
    t.prepare(Some("Laptop")).unwrap();
    let copied = at(&t.root, AGENT_CONFIG)
        .parent()
        .unwrap()
        .join("config.yaml.d/90-local.yaml");
    assert_eq!(
        std::fs::metadata(copied).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
