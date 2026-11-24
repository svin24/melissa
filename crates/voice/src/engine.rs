//! Speech to text: an [`Engine`] trait (tests use a fake), whisper.cpp's `whisper-cli` run as a
//! separate process ([`WhisperCli`]), and the [`Transcriber`] queue in front of it: one clip
//! at a time, a few waiting at most, each run under a timeout.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::future::BoxFuture;
use tokio::{io::AsyncWriteExt, sync::Semaphore};

use crate::{wav::Clip, VoiceError};

/// Turns a clip into text.
pub trait Engine: Send + Sync {
    fn transcribe<'a>(&'a self, clip: &'a Clip) -> BoxFuture<'a, Result<String, VoiceError>>;
}

/// whisper.cpp's command-line program. Each clip is one run: the clip goes to a private
/// temporary WAV file (removed when the run ends, whatever happens) and the text comes back
/// on stdout. The process is killed if the run is dropped (timeout, client gone).
pub struct WhisperCli {
    pub program: PathBuf,
    pub model: PathBuf,
    /// A private directory for the temporary clip (e.g. `/run/melissa/voice`, tmpfs).
    pub scratch: PathBuf,
    pub threads: usize,
}

/// How many threads whisper.cpp gets: the CPUs, at most 8 (it scales poorly beyond that,
/// and the apps on the box need the rest).
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
}

impl WhisperCli {
    async fn run(&self, clip: &Clip) -> Result<String, VoiceError> {
        let file = ScratchFile::create(&self.scratch, &clip.to_wav()).await?;
        let out = tokio::process::Command::new(&self.program)
            .arg("--model")
            .arg(&self.model)
            .arg("--file")
            .arg(&file.path)
            .args(["--language", "auto", "--no-timestamps", "--no-prints"])
            .args(["--threads", &self.threads.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| {
                VoiceError::Engine(format!("cannot start {}: {e}", self.program.display()))
            })?;
        drop(file);
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: String = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            tracing::warn!(status = %out.status, "whisper-cli failed: {tail}");
            return Err(VoiceError::Engine(format!(
                "whisper-cli ended with {}",
                out.status
            )));
        }
        Ok(clean(&String::from_utf8_lossy(&out.stdout)))
    }
}

impl Engine for WhisperCli {
    fn transcribe<'a>(&'a self, clip: &'a Clip) -> BoxFuture<'a, Result<String, VoiceError>> {
        Box::pin(self.run(clip))
    }
}

/// whisper.cpp's text as one paragraph: segments joined by spaces, markers for silence or
/// noise (`[BLANK_AUDIO]`, `(music)`, ...) dropped.
pub fn clean(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .split_inclusive(['[', ']', '(', ')'])
        .fold((String::new(), 0i32), |(mut out, depth), piece| {
            // Text outside brackets is kept; a bracketed marker is dropped.
            let (text, close) = match piece.chars().last() {
                Some('[' | '(') => (&piece[..piece.len() - 1], 1),
                Some(']' | ')') => (&piece[..piece.len() - 1], -1),
                _ => (piece, 0),
            };
            if depth == 0 {
                out.push_str(text);
            }
            (out, (depth + close).max(0))
        })
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A temporary file only Melissa can read, removed on drop.
struct ScratchFile {
    path: PathBuf,
}

impl ScratchFile {
    async fn create(dir: &Path, data: &[u8]) -> Result<Self, VoiceError> {
        static N: AtomicU64 = AtomicU64::new(0);
        let io = |e: std::io::Error| VoiceError::Io(format!("cannot write the clip: {e}"));
        tokio::fs::create_dir_all(dir).await.map_err(io)?;
        let path = dir.join(format!(
            "clip-{}-{}.wav",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = tokio::fs::remove_file(&path).await;
        let this = Self { path };
        let mut f = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&this.path)
            .await
            .map_err(io)?;
        f.write_all(data).await.map_err(io)?;
        f.flush().await.map_err(io)?;
        Ok(this)
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        // Audio is never kept after a run.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One clip at a time; at most `max_waiting` more wait their turn, the rest are told it's
/// busy. Each run gets `timeout`.
pub struct Transcriber {
    engine: Arc<dyn Engine>,
    gate: Semaphore,
    waiting: AtomicUsize,
    max_waiting: usize,
    timeout: Duration,
}

/// The default limits: three clips waiting, two minutes per run (a two-minute clip takes
/// seconds with the base model on a desktop CPU).
pub const MAX_WAITING: usize = 3;
pub const TIMEOUT: Duration = Duration::from_secs(120);

impl Transcriber {
    pub fn new(engine: Arc<dyn Engine>, max_waiting: usize, timeout: Duration) -> Self {
        Self {
            engine,
            gate: Semaphore::new(1),
            waiting: AtomicUsize::new(0),
            max_waiting,
            timeout,
        }
    }

    pub async fn transcribe(&self, clip: &Clip) -> Result<String, VoiceError> {
        // Counted from arrival to the end: one running plus `max_waiting` waiting.
        if self.waiting.fetch_add(1, Ordering::SeqCst) > self.max_waiting {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return Err(VoiceError::Busy);
        }
        let _queued = Queued(&self.waiting);
        let _turn = self
            .gate
            .acquire()
            .await
            .map_err(|_| VoiceError::Engine("closed".into()))?;
        tokio::time::timeout(self.timeout, self.engine.transcribe(clip))
            .await
            .map_err(|_| VoiceError::TimedOut)?
    }
}

/// Counts a request from arrival until it is done (waiting or running).
struct Queued<'a>(&'a AtomicUsize);

impl Drop for Queued<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// Says how many samples it got, after `delay`; counts how many run at once.
    struct Fake {
        delay: Duration,
        running: AtomicUsize,
        most: AtomicUsize,
    }

    impl Engine for Fake {
        fn transcribe<'a>(&'a self, clip: &'a Clip) -> BoxFuture<'a, Result<String, VoiceError>> {
            Box::pin(async move {
                let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.most.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(self.delay).await;
                self.running.fetch_sub(1, Ordering::SeqCst);
                Ok(format!("{} samples", clip.samples().len()))
            })
        }
    }

    fn fake(delay_ms: u64) -> Arc<Fake> {
        Arc::new(Fake {
            delay: Duration::from_millis(delay_ms),
            running: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn clips_run_one_at_a_time_and_extra_ones_are_turned_away() {
        let engine = fake(50);
        let t = Arc::new(Transcriber::new(engine.clone(), 2, Duration::from_secs(5)));
        let clip = Clip::from_samples(vec![0; 1600]);
        let runs: Vec<_> = (0..5)
            .map(|_| {
                let (t, clip) = (t.clone(), clip.clone());
                tokio::spawn(async move { t.transcribe(&clip).await })
            })
            .collect();
        let mut ok = 0;
        let mut busy = 0;
        for r in runs {
            match r.await.unwrap() {
                Ok(text) => {
                    assert_eq!(text, "1600 samples");
                    ok += 1;
                }
                Err(VoiceError::Busy) => busy += 1,
                Err(e) => panic!("{e}"),
            }
        }
        // One running + two waiting; the other two were refused.
        assert_eq!((ok, busy), (3, 2));
        assert_eq!(engine.most.load(Ordering::SeqCst), 1);
        // The queue empties again.
        assert_eq!(t.transcribe(&clip).await.unwrap(), "1600 samples");
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_run_times_out_and_frees_the_queue() {
        let t = Transcriber::new(fake(10_000), 0, Duration::from_millis(30));
        let clip = Clip::from_samples(vec![0; 16]);
        assert!(matches!(
            t.transcribe(&clip).await,
            Err(VoiceError::TimedOut)
        ));
        assert!(matches!(
            t.transcribe(&clip).await,
            Err(VoiceError::TimedOut)
        ));
    }

    #[test]
    fn whisper_output_becomes_one_clean_paragraph() {
        assert_eq!(
            clean(" Hello there.\n And [BLANK_AUDIO] welcome (music) back.\n"),
            "Hello there. And welcome back."
        );
        assert_eq!(clean("[BLANK_AUDIO]\n"), "");
        assert_eq!(clean("  Plain text  "), "Plain text");
    }

    /// The real engine, by hand: `MELISSA_TEST_WHISPER_CLI=<whisper-cli>
    /// MELISSA_TEST_WHISPER_MODEL=<ggml-base.bin> MELISSA_TEST_WAV=<speech.wav>
    /// cargo nextest run -p melissa-voice --run-ignored only real_whisper`.
    #[tokio::test]
    #[ignore = "needs whisper-cli, the model and a recording"]
    async fn real_whisper_transcribes_a_recording() {
        let var = |k: &str| PathBuf::from(std::env::var_os(k).unwrap_or_default());
        let clip = crate::wav::decode(&std::fs::read(var("MELISSA_TEST_WAV")).unwrap()).unwrap();
        let dir = crate::model::tests::temp_dir("real");
        let cli = WhisperCli {
            program: var("MELISSA_TEST_WHISPER_CLI"),
            model: var("MELISSA_TEST_WHISPER_MODEL"),
            scratch: dir.clone(),
            threads: default_threads(),
        };
        let started = std::time::Instant::now();
        let text = cli.run(&clip).await.unwrap();
        println!(
            "{} ms of audio in {:?}: {text}",
            clip.millis(),
            started.elapsed()
        );
        assert!(!text.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn whisper_cli_gets_a_private_clip_and_it_is_removed() {
        // A stand-in program: prints the arguments it got and copies the clip's size.
        let dir = crate::model::tests::temp_dir("cli");
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("fake-whisper");
        std::fs::write(
            &program,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in --file) f=\"$2\"; shift;; esac; shift; done\n\
             echo \" heard $(wc -c < \"$f\") bytes\"\n\
             stat -c %a \"$f\" >&2\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cli = WhisperCli {
            program,
            model: dir.join("model.bin"),
            scratch: dir.join("scratch"),
            threads: 2,
        };
        let clip = Clip::from_samples(vec![0; 100]);
        assert_eq!(cli.run(&clip).await.unwrap(), "heard 244 bytes");
        // Nothing is left behind.
        assert_eq!(std::fs::read_dir(dir.join("scratch")).unwrap().count(), 0);
        // A program that fails is an engine error.
        let cli = WhisperCli {
            program: PathBuf::from("/bin/false"),
            ..cli
        };
        assert!(matches!(cli.run(&clip).await, Err(VoiceError::Engine(_))));
        assert_eq!(std::fs::read_dir(dir.join("scratch")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
