#![forbid(unsafe_code)]
//! Voice input for the chat composer, with speech turned into text on this device.
//!
//! - [`wav`]: the browser's WAV clip → 16 kHz mono 16-bit PCM, in safe Rust (no ffmpeg).
//! - [`model`]: the pinned whisper.cpp model, downloaded once when voice input is turned on
//!   and checked against its SHA-256.
//! - [`engine`]: whisper.cpp's `whisper-cli` run as a separate process per clip, behind a
//!   queue (one at a time, a timeout).
//! - [`fetch`]: the HTTPS client for the model download (the only network call here).
//!
//! Audio is never kept: the clip lives in memory and in one private temporary file for the
//! length of a run.

pub mod engine;
pub mod fetch;
pub mod model;
pub mod wav;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use engine::{Engine, Transcriber, WhisperCli};
use fetch::Fetch;
use model::{ModelSpec, ModelState, ModelStore, OnDone};
use wav::Clip;

/// Errors. Display texts are short and may be shown in the UI.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("Voice input isn't available on this install.")]
    Unavailable,
    #[error("The speech model isn't ready yet.")]
    NotReady,
    #[error("{0}")]
    Audio(#[from] wav::AudioError),
    #[error("Still working on another recording. Try again in a moment.")]
    Busy,
    #[error("That took too long.")]
    TimedOut,
    #[error("Couldn't turn that into text.")]
    Engine(String),
    #[error("Download failed: {0}")]
    Download(String),
    #[error("The download didn't match the expected file.")]
    Mismatch,
    #[error("{0}")]
    Io(String),
}

/// Where voice input finds its parts.
pub struct VoiceConfig {
    /// Melissa's data directory (`$XDG_DATA_HOME/melissa`); the model goes to `models/`.
    pub data: PathBuf,
    /// A private directory for temporary clips (preferably tmpfs, e.g. `/run/melissa`).
    pub scratch: PathBuf,
    /// whisper.cpp's CLI, if this install has one (see [`find_program`]).
    pub program: Option<PathBuf>,
}

/// The environment variable that names whisper.cpp's CLI (set by the Nix module).
pub const PROGRAM_ENV: &str = "MELISSA_WHISPER_CLI";
/// Where the container image installs it.
pub const IMAGE_PROGRAM: &str = "/usr/lib/melissa/whisper/whisper-cli";

/// whisper.cpp's CLI: `$MELISSA_WHISPER_CLI`, else the container image's copy, else
/// `whisper-cli` on `PATH`.
pub fn find_program() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(PROGRAM_ENV).map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let image = Path::new(IMAGE_PROGRAM);
    if image.is_file() {
        return Some(image.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("whisper-cli"))
            .find(|p| p.is_file())
    })
}

/// Voice input on this device: the model and the transcriber. Cheap to clone.
#[derive(Clone)]
pub struct Voice {
    inner: Arc<Inner>,
}

struct Inner {
    model: ModelStore,
    fetch: Option<Arc<dyn Fetch>>,
    /// `None` where no engine is installed.
    transcriber: Option<Transcriber>,
}

impl Voice {
    /// The real thing: [`model::BASE`], HTTPS downloads, whisper-cli if installed.
    pub fn new(config: VoiceConfig) -> Self {
        let model = ModelStore::open(&config.data.join("models"), model::BASE);
        let fetch = match fetch::Https::new() {
            Ok(h) => Some(Arc::new(h) as Arc<dyn Fetch>),
            Err(e) => {
                tracing::warn!("voice input can't download its model: {e}");
                None
            }
        };
        let engine = config.program.map(|program| {
            Arc::new(WhisperCli {
                program,
                model: model.path().to_path_buf(),
                scratch: config.scratch.join("voice"),
                threads: engine::default_threads(),
            }) as Arc<dyn Engine>
        });
        Self::from_parts(model, fetch, engine)
    }

    /// Assembled from parts (tests: a fake fetch and engine).
    pub fn from_parts(
        model: ModelStore,
        fetch: Option<Arc<dyn Fetch>>,
        engine: Option<Arc<dyn Engine>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                model,
                fetch,
                transcriber: engine
                    .map(|e| Transcriber::new(e, engine::MAX_WAITING, engine::TIMEOUT)),
            }),
        }
    }

    /// No engine, no downloads (routers built without a server config).
    pub fn unavailable() -> Self {
        let dir = std::env::temp_dir().join("melissa-voice-unavailable");
        Self::from_parts(ModelStore::open(&dir, model::BASE), None, None)
    }

    /// An engine is installed and the model can be fetched (or is here).
    pub fn available(&self) -> bool {
        self.inner.transcriber.is_some()
            && (self.inner.fetch.is_some() || self.inner.model.is_ready())
    }

    pub fn model(&self) -> &ModelStore {
        &self.inner.model
    }

    pub fn spec(&self) -> &ModelSpec {
        self.inner.model.spec()
    }

    pub fn state(&self) -> ModelState {
        self.inner.model.state()
    }

    /// Voice input can be used now.
    pub fn ready(&self) -> bool {
        self.inner.transcriber.is_some() && self.inner.model.is_ready()
    }

    /// Start the model download if needed; false if nothing started.
    pub fn download(&self, done: OnDone) -> bool {
        match &self.inner.fetch {
            Some(f) if self.inner.transcriber.is_some() => {
                self.inner.model.download(f.clone(), done)
            }
            _ => false,
        }
    }

    /// Transcribe one clip (queued; one at a time).
    pub async fn transcribe(&self, clip: &Clip) -> Result<String, VoiceError> {
        let t = self
            .inner
            .transcriber
            .as_ref()
            .ok_or(VoiceError::Unavailable)?;
        if !self.inner.model.is_ready() {
            return Err(VoiceError::NotReady);
        }
        t.transcribe(clip).await
    }
}
