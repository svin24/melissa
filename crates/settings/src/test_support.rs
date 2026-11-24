//! Helpers shared by this crate's tests.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

/// A fresh, empty directory under the system temp dir: one per call, named for `name` and
/// this test process.
pub fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "melissa-settings-test-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}
