//! Runs a tiny shell script as a sandboxed CLI backend and checks the isolation from the
//! outside. Skips (with a message) when bubblewrap is missing or cannot create namespaces.

// Test-only file: panicking on setup failure is the intended behaviour.
#![allow(clippy::unwrap_used)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use melissa_agent::{
    sandbox, Backend, CliBackend, CliSpec, EventSink, NetworkMode, RunRequest, SandboxSpec,
};

/// A fresh directory under the system temp dir.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "melissa-sandbox-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A host program: `/usr/bin/<name>` (Fedora) or the first `<name>` on `PATH` (NixOS).
fn host_program(name: &str) -> Option<PathBuf> {
    let usr = Path::new("/usr/bin").join(name);
    if usr.is_file() {
        return Some(usr);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Can bwrap create the sandbox's namespaces here? Probes with the host's root bound
/// read-only, so the check does not depend on the distribution's layout.
fn namespaces_work(bwrap: &Path, sh: &Path) -> bool {
    std::process::Command::new(bwrap)
        .args(["--unshare-all", "--ro-bind", "/", "/", "--"])
        .arg(sh)
        .args(["-c", "true"])
        .status()
        .is_ok_and(|s| s.success())
}

fn bwrap_works() -> bool {
    let Some(bwrap) = sandbox::find_bwrap() else {
        eprintln!("SKIP: bwrap not found; install bubblewrap to run the sandbox tests");
        return false;
    };
    let Some(sh) = host_program("sh") else {
        eprintln!("SKIP: no sh found");
        return false;
    };
    let ok = namespaces_work(&bwrap, &sh);
    if !ok {
        eprintln!("SKIP: bwrap cannot create namespaces here (unprivileged user namespaces off?)");
    }
    ok
}

/// `sh -c <script>` as a sandboxed backend whose home is `home`.
fn backend(script: &str, home: &Path, workdir: &Path) -> CliBackend {
    let sh = host_program("sh").unwrap();
    let sh = sh.to_str().unwrap();
    let spec = CliSpec::new("sh", sh, &["-c", script, "melissa", "{prompt}"])
        .with_sandbox(SandboxSpec {
            ro: vec![".fakecli".into()],
            ..SandboxSpec::default()
        })
        // These tests are about files and processes; tests/egress.rs covers the network.
        .with_network(NetworkMode::None);
    let backend = CliBackend::new(spec, workdir.to_path_buf()).with_home(home.to_path_buf());
    assert!(backend.info().available, "{:?}", backend.info());
    backend
}

#[tokio::test]
async fn sandbox_hides_home_and_confines_writes() {
    if !bwrap_works() {
        return;
    }
    let root = scratch("fs");
    let home = root.join("home");
    let workdir = root.join("work");
    let outside = root.join("outside");
    std::fs::create_dir_all(home.join(".fakecli")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(home.join("secret.txt"), "private").unwrap();
    std::fs::write(home.join(".fakecli/token"), "cli-token").unwrap();

    let out = outside.display();
    let script = format!(
        r#"
        cat "$HOME/secret.txt" 2>/dev/null && echo LEAK-secret
        echo "token=$(cat "$HOME/.fakecli/token")"
        touch "$HOME/new" 2>/dev/null && echo LEAK-home-write
        touch "$HOME/.fakecli/new" 2>/dev/null && echo LEAK-ro-write
        touch /usr/new /etc/new 2>/dev/null && echo LEAK-system-write
        touch "{out}/new" 2>/dev/null && echo LEAK-outside-write
        echo "inside $1" > inside.txt && echo wrote-inside
        echo private-tmp > /tmp/melissa-probe
        echo "pwd=$PWD"
        echo "env-secret=${{MELISSA_TEST_SECRET:-unset}}"
        "#
    );
    let backend = backend(&script, &home, &workdir);
    let (sink, mut rx) = EventSink::channel();
    let req = RunRequest {
        prompt: "hello".into(),
        workdir: Some(workdir.clone()),
        ..RunRequest::default()
    };
    let result = backend.run(&req, sink).await;
    let mut lines = Vec::new();
    while let Ok(line) = rx.try_recv() {
        lines.push(line.text);
    }
    assert!(result.is_ok(), "{result:?} {lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains("LEAK")),
        "sandbox leaked: {lines:?}"
    );
    assert!(lines.contains(&"token=cli-token".to_string()), "{lines:?}");
    assert!(lines.contains(&"wrote-inside".to_string()), "{lines:?}");
    assert!(
        lines.contains(&format!("pwd={}", workdir.display())),
        "{lines:?}"
    );
    assert!(lines.contains(&"env-secret=unset".to_string()), "{lines:?}");
    assert_eq!(
        std::fs::read_to_string(workdir.join("inside.txt")).unwrap(),
        "inside hello\n"
    );
    assert!(!home.join("new").exists());
    assert!(!outside.join("new").exists());
    assert!(!Path::new("/tmp/melissa-probe").exists());
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn cancelling_kills_background_grandchildren() {
    if !bwrap_works() {
        return;
    }
    let root = scratch("kill");
    let home = root.join("home");
    let workdir = root.join("work");
    std::fs::create_dir_all(&home).unwrap();
    // A detached grandchild that keeps writing a heartbeat file, then a long sleep.
    let script = r#"
        setsid sh -c 'while :; do date +%s%N > beat; sleep 0.05; done' </dev/null >/dev/null 2>&1 &
        sleep 1000
    "#;
    let backend = backend(script, &home, &workdir);
    let (sink, _rx) = EventSink::channel();
    let req = RunRequest {
        prompt: "x".into(),
        workdir: Some(workdir.clone()),
        ..RunRequest::default()
    };
    // Dropping the run future (as the board does on cancel/timeout) must kill everything.
    let run = tokio::time::timeout(Duration::from_millis(1500), backend.run(&req, sink)).await;
    assert!(run.is_err(), "script should still be running: {run:?}");
    let beat = workdir.join("beat");
    assert!(beat.exists(), "grandchild never started");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = std::fs::read_to_string(&beat).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = std::fs::read_to_string(&beat).unwrap();
    assert_eq!(before, after, "grandchild survived the cancel");
    std::fs::remove_dir_all(&root).unwrap();
}

/// An agent's sandbox has its own file box, read-write at the same path, and nothing of the
/// other boxes (not even their names).
#[test]
fn an_agent_sees_its_own_file_box_and_no_other() {
    if !bwrap_works() {
        return;
    }
    let dir = scratch("filebox");
    let home = dir.join("home");
    let work = dir.join("work/agents/d-0a1b2c3d.a1");
    let boxes = dir.join("files/boxes");
    let mine = boxes.join("d-0a1b2c3d.a1");
    let other = boxes.join("d-0a1b2c3d.a2");
    for d in [&home, &work, &mine, &other] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(mine.join("from-owner.txt"), "hello").unwrap();
    std::fs::write(other.join("secret.txt"), "not for a1").unwrap();
    let bwrap = sandbox::find_bwrap().unwrap();
    let sh = host_program("sh").unwrap();
    let plan = sandbox::SandboxPlan::resolve(
        &SandboxSpec {
            network: NetworkMode::None,
            ..SandboxSpec::default()
        },
        &bwrap,
        Some(&home),
        &sh,
    )
    .unwrap()
    .with_rw_dirs(std::slice::from_ref(&mine))
    .unwrap();
    let (m, o, b) = (mine.display(), other.display(), boxes.display());
    let script = format!(
        "cat {m}/from-owner.txt; echo; echo saved > {m}/report.txt; \
         if cat {o}/secret.txt 2>/dev/null; then echo LEAK-FILE; fi; \
         if ls {b} | grep -q a2; then echo LEAK-NAME; fi; \
         echo x > {b}/new.txt 2>/dev/null; \
         echo done"
    );
    let (program, argv) = sandbox::command(&plan, &work, &[], &["-c".into(), script]);
    let out = std::process::Command::new(program)
        .args(argv)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello"),
        "{stdout} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("done"), "{stdout}");
    assert!(!stdout.contains("LEAK"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(mine.join("report.txt")).unwrap(),
        "saved\n"
    );
    // Whatever it writes beside its box stays in the sandbox's private /tmp.
    assert!(!boxes.join("new.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
