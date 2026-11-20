//! The speech model: a pinned file, downloaded once when voice input is turned on, checked
//! against its SHA-256 and size before it is used, kept under Melissa's data directory.
//!
//! The download streams to `<file>.part` while hashing; only a file of the exact size and
//! hash is renamed into place. Anything else is deleted.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, task::AbortHandle};

use crate::{fetch::Fetch, VoiceError};

/// A model file Melissa may download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// The file name under the model directory.
    pub file: &'static str,
    /// Pinned to a repository revision, so the bytes behind it cannot change.
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// whisper.cpp's multilingual `base` model (ggml, f16, 142 MiB) from the whisper.cpp
/// project's model repository on Hugging Face, pinned to revision 5359861c. See
/// docs/DECISIONS.md (2026-11-20, voice input) for why `base`.
pub const BASE: ModelSpec = ModelSpec {
    file: "ggml-base.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin",
    size: 147_951_465,
    sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
};

/// Where the model is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    /// Not on this device.
    Absent,
    Downloading {
        done: u64,
        total: u64,
    },
    /// On disk and checked.
    Ready,
    /// The last download failed (a short reason for the UI).
    Failed(String),
}

/// Download `spec` from `fetch` into `dest`, reporting bytes written to `progress`. On any
/// error nothing is left at `dest` or `dest.part`.
pub async fn fetch_verified(
    fetch: &dyn Fetch,
    spec: &ModelSpec,
    dest: &Path,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), VoiceError> {
    let part = part_path(dest);
    let result = fetch_to(fetch, spec, &part, progress).await;
    match result {
        Ok(()) => tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| VoiceError::Io(format!("cannot move the model into place: {e}"))),
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

async fn fetch_to(
    fetch: &dyn Fetch,
    spec: &ModelSpec,
    part: &Path,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), VoiceError> {
    if let Some(dir) = part.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| VoiceError::Io(format!("cannot create {}: {e}", dir.display())))?;
    }
    let mut body = fetch.get(spec.url).await?;
    if body.len_hint().is_some_and(|n| n != spec.size) {
        return Err(VoiceError::Mismatch);
    }
    // A leftover from an interrupted run is replaced.
    let _ = tokio::fs::remove_file(part).await;
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(part)
        .await
        .map_err(|e| VoiceError::Io(format!("cannot write {}: {e}", part.display())))?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    while let Some(chunk) = body.next().await? {
        done += chunk.len() as u64;
        if done > spec.size {
            return Err(VoiceError::Mismatch);
        }
        hash.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| VoiceError::Io(format!("cannot write the model: {e}")))?;
        progress(done);
    }
    file.sync_all()
        .await
        .map_err(|e| VoiceError::Io(format!("cannot write the model: {e}")))?;
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != spec.size || digest != spec.sha256 {
        return Err(VoiceError::Mismatch);
    }
    Ok(())
}

/// Ready if a file of the pinned size is there, else absent.
fn on_disk(path: &Path, spec: &ModelSpec) -> ModelState {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() == spec.size => ModelState::Ready,
        _ => ModelState::Absent,
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_os_string();
    p.push(".part");
    PathBuf::from(p)
}

/// The model on this device and its download. Cheap to clone.
#[derive(Clone)]
pub struct ModelStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    spec: ModelSpec,
    path: PathBuf,
    state: Mutex<ModelState>,
    task: Mutex<Option<AbortHandle>>,
}

/// The outcome of a download, for the caller's audit log.
pub type OnDone = Box<dyn FnOnce(Result<(), VoiceError>) + Send>;

impl ModelStore {
    /// The model `spec` in `dir`. A file of the right size counts as ready (it was checked
    /// when it was downloaded; the directory is Melissa's own).
    pub fn open(dir: &Path, spec: ModelSpec) -> Self {
        let path = dir.join(spec.file);
        let state = on_disk(&path, &spec);
        Self {
            inner: Arc::new(StoreInner {
                spec,
                path,
                state: Mutex::new(state),
                task: Mutex::new(None),
            }),
        }
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.inner.spec
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn state(&self) -> ModelState {
        lock(&self.inner.state).clone()
    }

    pub fn is_ready(&self) -> bool {
        self.state() == ModelState::Ready
    }

    fn set(&self, s: ModelState) {
        *lock(&self.inner.state) = s;
    }

    /// Start downloading in the background unless the model is ready or already coming.
    /// Returns whether a download started. `done` hears the outcome (not on [`cancel`]).
    ///
    /// [`cancel`]: Self::cancel
    pub fn download(&self, fetch: Arc<dyn Fetch>, done: OnDone) -> bool {
        {
            let mut state = lock(&self.inner.state);
            if matches!(*state, ModelState::Ready | ModelState::Downloading { .. }) {
                return false;
            }
            *state = ModelState::Downloading {
                done: 0,
                total: self.inner.spec.size,
            };
        }
        let store = self.clone();
        let task = tokio::spawn(async move {
            let total = store.inner.spec.size;
            let progress = {
                let store = store.clone();
                move |n: u64| store.set(ModelState::Downloading { done: n, total })
            };
            let result = fetch_verified(
                fetch.as_ref(),
                &store.inner.spec,
                &store.inner.path,
                &progress,
            )
            .await;
            store.set(match &result {
                Ok(()) => ModelState::Ready,
                Err(e) => ModelState::Failed(e.to_string()),
            });
            *lock(&store.inner.task) = None;
            done(result);
        });
        *lock(&self.inner.task) = Some(task.abort_handle());
        true
    }

    /// Stop a running download (its partial file is removed).
    pub async fn cancel(&self) {
        let task = lock(&self.inner.task).take();
        if let Some(t) = task {
            t.abort();
            let _ = tokio::fs::remove_file(part_path(&self.inner.path)).await;
            // A download that finished just before the abort has its file in place.
            self.set(on_disk(&self.inner.path, &self.inner.spec));
        }
    }

    /// Cancel any download and delete the model.
    pub async fn delete(&self) -> Result<(), VoiceError> {
        self.cancel().await;
        match tokio::fs::remove_file(&self.inner.path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(VoiceError::Io(format!("cannot delete the model: {e}"))),
        }
        self.set(ModelState::Absent);
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the value is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use bytes::Bytes;
    use futures::future::BoxFuture;

    use super::*;
    use crate::fetch::Body;

    /// Serves fixed bytes in small pieces, announcing `len` as the length (if any).
    pub(crate) struct FakeFetch {
        pub data: Vec<u8>,
        pub len: Option<u64>,
        pub fail: bool,
    }

    struct FakeBody {
        rest: Vec<u8>,
        len: Option<u64>,
    }

    impl Fetch for FakeFetch {
        fn get<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Box<dyn Body>, VoiceError>> {
            Box::pin(async move {
                if self.fail {
                    return Err(VoiceError::Download("the server said 404 Not Found".into()));
                }
                Ok(Box::new(FakeBody {
                    rest: self.data.clone(),
                    len: self.len,
                }) as Box<dyn Body>)
            })
        }
    }

    impl Body for FakeBody {
        fn len_hint(&self) -> Option<u64> {
            self.len
        }
        fn next(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, VoiceError>> {
            Box::pin(async move {
                if self.rest.is_empty() {
                    return Ok(None);
                }
                let n = self.rest.len().min(7);
                Ok(Some(Bytes::from(self.rest.drain(..n).collect::<Vec<_>>())))
            })
        }
    }

    fn sha(data: &[u8]) -> &'static str {
        let hex: String = Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Box::leak(hex.into_boxed_str())
    }

    pub(crate) fn spec_for(data: &[u8]) -> ModelSpec {
        ModelSpec {
            file: "model.bin",
            url: "https://example.com/model.bin",
            size: data.len() as u64,
            sha256: sha(data),
        }
    }

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "melissa-voice-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn a_matching_download_lands_and_reports_progress() {
        let data = b"pretend this is a speech model".to_vec();
        let spec = spec_for(&data);
        let dir = temp_dir("ok");
        let dest = dir.join(spec.file);
        let seen = AtomicU64::new(0);
        let fake = FakeFetch {
            data: data.clone(),
            len: None,
            fail: false,
        };
        fetch_verified(&fake, &spec, &dest, &|n| seen.store(n, Ordering::Relaxed))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(seen.load(Ordering::Relaxed), data.len() as u64);
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_checksum_mismatch_is_rejected_and_leaves_nothing() {
        let good = b"the real model bytes".to_vec();
        let spec = spec_for(&good);
        let dir = temp_dir("bad");
        let dest = dir.join(spec.file);
        // Same length, different bytes.
        let mut evil = good.clone();
        evil[3] ^= 1;
        for fake in [
            FakeFetch {
                data: evil,
                len: None,
                fail: false,
            },
            // Longer than pinned, no length announced: stopped at the first extra byte.
            FakeFetch {
                data: [good.clone(), b"more".to_vec()].concat(),
                len: None,
                fail: false,
            },
            // Announced length differs: refused before writing.
            FakeFetch {
                data: good.clone(),
                len: Some(5),
                fail: false,
            },
            // Shorter than pinned.
            FakeFetch {
                data: good[..5].to_vec(),
                len: Some(spec.size),
                fail: false,
            },
        ] {
            let r = fetch_verified(&fake, &spec, &dest, &|_| {}).await;
            assert!(matches!(r, Err(VoiceError::Mismatch)), "{r:?}");
            assert!(!dest.exists());
            assert!(!part_path(&dest).exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_store_downloads_once_and_deletes() {
        let data = b"model".repeat(100);
        let spec = spec_for(&data);
        let dir = temp_dir("store");
        let store = ModelStore::open(&dir, spec);
        assert_eq!(store.state(), ModelState::Absent);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let fake: Arc<dyn Fetch> = Arc::new(FakeFetch {
            data: data.clone(),
            len: None,
            fail: false,
        });
        assert!(store.download(
            fake.clone(),
            Box::new(move |r| {
                let _ = tx.send(r.is_ok());
            })
        ));
        assert!(rx.await.unwrap());
        assert_eq!(store.state(), ModelState::Ready);
        // Ready: no second download; a new store sees it too.
        assert!(!store.download(fake, Box::new(|_| {})));
        assert!(ModelStore::open(&dir, spec).is_ready());
        store.delete().await.unwrap();
        assert_eq!(store.state(), ModelState::Absent);
        assert!(!store.path().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_download_says_why_and_can_be_retried() {
        let data = b"x".repeat(64);
        let spec = spec_for(&data);
        let dir = temp_dir("retry");
        let store = ModelStore::open(&dir, spec);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let failing: Arc<dyn Fetch> = Arc::new(FakeFetch {
            data: data.clone(),
            len: None,
            fail: true,
        });
        store.download(failing, Box::new(move |r| drop(tx.send(r))));
        assert!(rx.await.unwrap().is_err());
        assert!(matches!(store.state(), ModelState::Failed(m) if m.contains("404")));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ok: Arc<dyn Fetch> = Arc::new(FakeFetch {
            data,
            len: None,
            fail: false,
        });
        assert!(store.download(ok, Box::new(move |r| drop(tx.send(r)))));
        assert!(rx.await.unwrap().is_ok());
        assert!(store.is_ready());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
