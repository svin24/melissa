//! Helpers shared by the integration tests.

use std::path::{Path, PathBuf};

use melissa_agent::sandbox;

/// A fresh, empty directory under the system temp dir, named for this test process.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("melissa-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A host program: `/usr/bin/<name>` (Fedora) or the first `<name>` on `PATH` (NixOS).
pub fn host_program(name: &str) -> Option<PathBuf> {
    let usr = Path::new("/usr/bin").join(name);
    if usr.is_file() {
        return Some(usr);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// `melissa-netbridge`: `MELISSA_NETBRIDGE` when set (test binaries run outside their
/// build tree, e.g. in the NixOS VM test), else the one cargo built.
pub fn netbridge() -> PathBuf {
    std::env::var_os("MELISSA_NETBRIDGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_melissa-netbridge").into())
}

fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == "1")
}

/// Must the sandbox tests run here? `MELISSA_SANDBOX_REQUIRED=1`, or the escape gate's
/// `MELISSA_ESCAPE_REQUIRED=1` (`just escape-test`, the image and VM tests).
pub fn required() -> bool {
    flag("MELISSA_SANDBOX_REQUIRED") || flag("MELISSA_ESCAPE_REQUIRED")
}

/// Not runnable here: a failure when required, else a SKIP line and `None`.
pub fn unavailable<T>(why: &str) -> Option<T> {
    assert!(
        !required(),
        "sandbox tests required (MELISSA_SANDBOX_REQUIRED / MELISSA_ESCAPE_REQUIRED) but \
         cannot run: {why}"
    );
    eprintln!("SKIP: {why}");
    None
}

/// A host that can run the agent sandbox.
pub struct SandboxHost {
    pub bwrap: PathBuf,
    pub sh: PathBuf,
}

/// bwrap, `sh` and the host programs in `needs`, and bwrap able to create the sandbox's
/// namespaces (probed with the host's root bound read-only, so the check does not depend on
/// the distribution's layout). `None` (SKIP) when not, or a failure when [`required`].
pub fn sandbox_host(needs: &[&str]) -> Option<SandboxHost> {
    let Some(bwrap) = sandbox::find_bwrap() else {
        return unavailable("bwrap not found; install bubblewrap to run the sandbox tests");
    };
    let Some(sh) = host_program("sh") else {
        return unavailable("no sh found");
    };
    if let Some(missing) = needs.iter().find(|n| host_program(n).is_none()) {
        return unavailable(&format!("{missing} not found"));
    }
    let namespaces = std::process::Command::new(&bwrap)
        .args(["--unshare-all", "--ro-bind", "/", "/", "--"])
        .arg(&sh)
        .args(["-c", "true"])
        .status()
        .is_ok_and(|s| s.success());
    if !namespaces {
        return unavailable(
            "bwrap cannot create namespaces here (unprivileged user namespaces off?)",
        );
    }
    Some(SandboxHost { bwrap, sh })
}
