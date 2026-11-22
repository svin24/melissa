//! Settings → Voice (voice input for the chat composer) and the route the composer sends its
//! recordings to. Speech becomes text on this device ([`melissa_voice`]); turning voice input
//! on downloads the pinned speech model once (audited, with its SHA-256).

use std::time::Instant;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use melissa_voice::{model::ModelState, wav, VoiceError};
use serde_json::json;

use super::{
    pages::{self, VoiceModel, VoiceView, VOICE_URL},
    Settings,
};
use crate::{is_htmx, ui::Ui};

/// Where the composer posts a recording (body: a WAV file, `Content-Type: audio/wav`);
/// answers `{"text": "..."}` or `{"error": "..."}`.
pub(crate) const TRANSCRIBE_URL: &str = "/ui/voice/transcribe";
/// The largest recording accepted. Two minutes of what the browser sends (16 kHz mono 16-bit)
/// is 3.8 MB; this leaves room for other WAV files up to 48 kHz mono.
pub(super) const MAX_UPLOAD: usize = 12 * 1024 * 1024;
const AUDIT: &str = "settings.voice";

pub(super) fn view(s: &Settings, error: Option<String>) -> VoiceView {
    let voice = s.voice();
    VoiceView {
        on: s.prefs().voice_input,
        available: voice.available(),
        model: match voice.state() {
            ModelState::Absent => VoiceModel::Absent,
            ModelState::Downloading { done, total } => VoiceModel::Downloading { done, total },
            ModelState::Ready => VoiceModel::Ready,
            ModelState::Failed(why) => VoiceModel::Failed(why),
        },
        size: pages::mb(voice.spec().size),
        error,
    }
}

/// `GET /settings/voice`.
pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&ui.settings, None);
    ui.framed(|f| pages::voice_page(f, &v))
        .await
        .into_response()
}

/// `GET /ui/settings/voice/poll`: the region again (polled while the model downloads).
pub(super) async fn poll(State(ui): State<Ui>) -> Response {
    pages::voice_region(&view(&ui.settings, None)).into_response()
}

/// After a form: htmx gets the region, a plain form post a redirect.
fn answer(s: &Settings, headers: &HeaderMap, error: Option<String>) -> Response {
    if !is_htmx(headers) {
        // First-run setup's Extras step posts here too: back to it.
        let back = if super::setup::from_setup(headers) {
            super::setup::EXTRAS_URL
        } else {
            VOICE_URL
        };
        return match error {
            None => Redirect::to(back).into_response(),
            Some(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        };
    }
    pages::voice_region(&view(s, error)).into_response()
}

/// Start the model download (if it isn't here or coming); audited when it starts and when it
/// ends.
fn start_download(s: &Settings) {
    let spec = *s.voice().spec();
    let later = s.clone();
    let started = s.voice().download(Box::new(move |result| match result {
        Ok(()) => {
            later.audit(
                AUDIT,
                "ui",
                &format!("speech model {} downloaded, sha256 verified", spec.file),
                Some(spec.sha256.to_string()),
            );
            // Open chats get the mic now.
            later.touch();
        }
        Err(e) => later.audit(
            AUDIT,
            "ui",
            &format!("speech model {} download failed: {e}", spec.file),
            None,
        ),
    }));
    if started {
        s.audit(
            AUDIT,
            "ui",
            &format!(
                "speech model {} download started from {}",
                spec.file, spec.url
            ),
            Some(spec.sha256.to_string()),
        );
    }
}

/// `POST /ui/settings/voice` (`enabled=on` or nothing): turn voice input on (and fetch the
/// model) or off (a running download stops; a downloaded model stays until deleted).
pub(super) async fn toggle(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let s = &ui.settings;
    let on = form.iter().any(|(k, v)| k == "enabled" && v == "on");
    if on && !s.voice().available() {
        return answer(s, &headers, Some(VoiceError::Unavailable.to_string()));
    }
    if let Err(e) = s.parts().prefs.update(|p| p.voice_input = on) {
        tracing::warn!("cannot save the UI preferences: {e}");
        return answer(s, &headers, Some("Couldn't save.".into()));
    }
    s.audit(
        AUDIT,
        "ui",
        &format!("voice_input: {}", if on { "on" } else { "off" }),
        None,
    );
    if on {
        start_download(s);
    } else if matches!(s.voice().state(), ModelState::Downloading { .. }) {
        s.voice().model().cancel().await;
        s.audit(AUDIT, "ui", "speech model download stopped", None);
    }
    answer(s, &headers, None)
}

/// `POST /ui/settings/voice/retry`: download the model again (voice input must be on).
pub(super) async fn retry(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let s = &ui.settings;
    if !s.prefs().voice_input {
        return answer(s, &headers, Some("Voice input is off.".into()));
    }
    start_download(s);
    answer(s, &headers, None)
}

/// `POST /ui/settings/voice/delete`: delete the downloaded model (voice input must be off).
pub(super) async fn delete(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let s = &ui.settings;
    if s.prefs().voice_input {
        return answer(s, &headers, Some("Turn voice input off first.".into()));
    }
    let error = match s.voice().model().delete().await {
        Ok(()) => {
            s.audit(
                AUDIT,
                "ui",
                &format!("speech model {} deleted", s.voice().spec().file),
                None,
            );
            None
        }
        Err(e) => {
            tracing::warn!("{e}");
            Some("Couldn't delete it.".into())
        }
    };
    answer(s, &headers, error)
}

fn fail(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// `POST /ui/voice/transcribe`: a WAV recording in, its text out. The clip is decoded in
/// memory, transcribed (one at a time) and dropped; nothing is stored.
pub(super) async fn transcribe(State(ui): State<Ui>, headers: HeaderMap, body: Bytes) -> Response {
    let s = &ui.settings;
    if !s.prefs().voice_input {
        return fail(StatusCode::CONFLICT, "Voice input is off.");
    }
    let kind = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(
        kind.as_str(),
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave"
    ) {
        return fail(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Send a WAV recording.");
    }
    let voice = s.voice().clone();
    if !voice.ready() {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            VoiceError::NotReady.to_string(),
        );
    }
    // Decoding and resampling is CPU work: off the async threads.
    let clip = match tokio::task::spawn_blocking(move || wav::decode(&body)).await {
        Ok(Ok(clip)) => clip,
        Ok(Err(e)) => return fail(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        Err(_) => {
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Couldn't read the recording.",
            )
        }
    };
    let started = Instant::now();
    match voice.transcribe(&clip).await {
        Ok(text) => {
            // Lengths only: what was said stays out of the log.
            tracing::info!(
                audio_ms = clip.millis(),
                took_ms = started.elapsed().as_millis() as u64,
                chars = text.chars().count(),
                "voice clip transcribed"
            );
            Json(json!({ "text": text })).into_response()
        }
        Err(e) => {
            let status = match &e {
                VoiceError::Busy => StatusCode::TOO_MANY_REQUESTS,
                VoiceError::TimedOut => StatusCode::GATEWAY_TIMEOUT,
                VoiceError::NotReady | VoiceError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
                VoiceError::Audio(_) => StatusCode::UNPROCESSABLE_ENTITY,
                _ => {
                    tracing::warn!("voice transcription failed: {e:?}");
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            };
            fail(status, e.to_string())
        }
    }
}
