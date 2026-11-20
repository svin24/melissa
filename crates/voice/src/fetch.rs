//! Downloading: a small trait so tests use a local fake (no network), and the real HTTPS
//! client (hyper + rustls with the system's root certificates; already in the build through
//! kube-rs). Redirects are followed, to `https://` only, at most [`MAX_REDIRECTS`] times.

use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use http_body_util::{BodyExt, Empty};
use hyper::{body::Incoming, header, Request, StatusCode, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};

use crate::VoiceError;

pub const MAX_REDIRECTS: usize = 5;
/// From sending a request to its response headers.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest wait for the next piece of a body.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Opens a URL for reading.
pub trait Fetch: Send + Sync {
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Body>, VoiceError>>;
}

/// A response body, read piece by piece.
pub trait Body: Send {
    /// The length the server announced, if any.
    fn len_hint(&self) -> Option<u64>;
    /// The next piece; `None` at the end.
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, VoiceError>>;
}

/// The real client: HTTPS only, HTTP/1.1.
#[derive(Clone)]
pub struct Https {
    client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>>,
}

impl Https {
    /// Fails when the system has no usable root certificates.
    pub fn new() -> Result<Self, VoiceError> {
        let connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|e| VoiceError::Download(format!("no root certificates: {e}")))?
            .https_only()
            .enable_http1()
            .build();
        Ok(Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
        })
    }

    async fn open(&self, url: &str) -> Result<Box<dyn Body>, VoiceError> {
        let mut uri: Uri = url
            .parse()
            .map_err(|_| VoiceError::Download("bad URL".into()))?;
        for _ in 0..=MAX_REDIRECTS {
            if uri.scheme_str() != Some("https") {
                return Err(VoiceError::Download("refused a non-https URL".into()));
            }
            let req = Request::get(&uri)
                // A generic name; nothing about the device or its owner.
                .header(header::USER_AGENT, "melissa")
                .body(Empty::new())
                .map_err(|e| VoiceError::Download(e.to_string()))?;
            let res = tokio::time::timeout(RESPONSE_TIMEOUT, self.client.request(req))
                .await
                .map_err(|_| VoiceError::Download("the server didn't answer".into()))?
                .map_err(|e| VoiceError::Download(format!("couldn't connect: {e}")))?;
            let status = res.status();
            if status.is_redirection() {
                let location = res
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| VoiceError::Download("redirect without a location".into()))?;
                uri = next_uri(&uri, location)?;
                continue;
            }
            if status != StatusCode::OK {
                return Err(VoiceError::Download(format!("the server said {status}")));
            }
            let len = res
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            return Ok(Box::new(HttpsBody {
                body: res.into_body(),
                len,
            }));
        }
        Err(VoiceError::Download("too many redirects".into()))
    }
}

impl Fetch for Https {
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Body>, VoiceError>> {
        Box::pin(self.open(url))
    }
}

/// Where a redirect goes: an absolute URL, or a path on the same host.
fn next_uri(from: &Uri, location: &str) -> Result<Uri, VoiceError> {
    let bad = || VoiceError::Download("bad redirect".into());
    if location.starts_with('/') && !location.starts_with("//") {
        let authority = from.authority().ok_or_else(bad)?;
        return format!("https://{authority}{location}")
            .parse()
            .map_err(|_| bad());
    }
    location.parse().map_err(|_| bad())
}

struct HttpsBody {
    body: Incoming,
    len: Option<u64>,
}

impl Body for HttpsBody {
    fn len_hint(&self) -> Option<u64> {
        self.len
    }

    fn next(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, VoiceError>> {
        Box::pin(async move {
            loop {
                let frame = tokio::time::timeout(READ_TIMEOUT, self.body.frame())
                    .await
                    .map_err(|_| VoiceError::Download("the download stalled".into()))?;
                match frame {
                    None => return Ok(None),
                    Some(Err(e)) => return Err(VoiceError::Download(e.to_string())),
                    Some(Ok(f)) => {
                        // Trailers and other non-data frames are skipped.
                        if let Ok(data) = f.into_data() {
                            return Ok(Some(data));
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_resolve_to_absolute_https_or_same_host_paths() {
        let from: Uri = "https://example.com/a/b".parse().unwrap();
        assert_eq!(
            next_uri(&from, "/c?d=1").unwrap().to_string(),
            "https://example.com/c?d=1"
        );
        assert_eq!(
            next_uri(&from, "https://cdn.example.net/x")
                .unwrap()
                .to_string(),
            "https://cdn.example.net/x"
        );
        // A plain-http target parses, and `open` then refuses it by scheme.
        assert_eq!(
            next_uri(&from, "http://example.org/x")
                .unwrap()
                .scheme_str(),
            Some("http")
        );
    }
}
