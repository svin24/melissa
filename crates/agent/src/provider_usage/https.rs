//! The few HTTPS calls Melissa's own process makes for usage numbers: a small trait so tests
//! use a fake (no network), and the real client (hyper + rustls with the system's root
//! certificates, already in the build through kube-rs and the voice downloads).
//!
//! The real client refuses anything but `https://` to the hosts in [`ALLOWED_HOSTS`] (the
//! usage endpoints documented in docs/DECISIONS.md, 2026-11-22), follows no redirects, waits
//! at most [`TIMEOUT`] and reads at most [`MAX_BODY`]. A sign-in token travels only in the
//! `Authorization` header of such a request; error messages never include it.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{header, Method, Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};

use crate::BoxFuture;

/// Hosts Melissa itself may call for usage numbers. Anything else is refused before a
/// connection is made, so a sign-in token cannot be sent elsewhere by mistake.
pub const ALLOWED_HOSTS: &[&str] = &["cloudcode-pa.googleapis.com", "opencode.ai"];
/// From sending a request to the end of its body.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The largest answer read.
pub const MAX_BODY: usize = 1 << 20;

/// A sign-in token. Never printed (its `Debug` is redacted, it has no `Display` and is not
/// serializable); read only to put it into a request header.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// One request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// `GET` or `POST`.
    pub post: bool,
    pub url: String,
    /// Sent as `Authorization: Bearer <token>`.
    pub bearer: Option<Secret>,
    /// A JSON body (POST).
    pub json: Option<serde_json::Value>,
}

impl HttpRequest {
    pub fn get(url: &str, bearer: Secret) -> Self {
        Self {
            post: false,
            url: url.into(),
            bearer: Some(bearer),
            json: None,
        }
    }

    pub fn post_json(url: &str, bearer: Secret, json: serde_json::Value) -> Self {
        Self {
            post: true,
            url: url.into(),
            bearer: Some(bearer),
            json: Some(json),
        }
    }

    /// The URL's host, if it parses.
    pub fn host(&self) -> Option<String> {
        self.url
            .parse::<Uri>()
            .ok()
            .and_then(|u| u.host().map(str::to_string))
    }
}

/// An answer: its status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("refused to call {0}: not a usage host Melissa may call")]
    NotAllowed(String),
    #[error("bad URL")]
    BadUrl,
    #[error("no answer within {} s", TIMEOUT.as_secs())]
    Timeout,
    #[error("couldn't connect: {0}")]
    Connect(String),
    #[error("the answer was too large or broken")]
    Body,
    #[error("no root certificates: {0}")]
    NoRoots(String),
}

/// Sends one request.
pub trait HttpClient: Send + Sync {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, HttpError>>;
}

/// The real client: HTTPS only, HTTP/1.1, [`ALLOWED_HOSTS`] only.
#[derive(Clone)]
pub struct Https {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl Https {
    /// Fails when the system has no usable root certificates.
    pub fn new() -> Result<Self, HttpError> {
        let connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|e| HttpError::NoRoots(e.to_string()))?
            .https_only()
            .enable_http1()
            .build();
        Ok(Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
        })
    }

    async fn call(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let uri: Uri = req.url.parse().map_err(|_| HttpError::BadUrl)?;
        check_target(&uri)?;
        let body = match &req.json {
            Some(v) => Bytes::from(serde_json::to_vec(v).map_err(|_| HttpError::BadUrl)?),
            None => Bytes::new(),
        };
        let mut builder = Request::builder()
            .method(if req.post { Method::POST } else { Method::GET })
            .uri(uri)
            // A generic name; nothing about the device or its owner.
            .header(header::USER_AGENT, "melissa")
            .header(header::ACCEPT, "application/json");
        if req.json.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(token) = &req.bearer {
            let mut value = header::HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                .map_err(|_| HttpError::BadUrl)?;
            value.set_sensitive(true);
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let request = builder
            .body(Full::new(body))
            .map_err(|_| HttpError::BadUrl)?;
        let work = async {
            let res = self
                .client
                .request(request)
                .await
                .map_err(|e| HttpError::Connect(e.to_string()))?;
            let status = res.status().as_u16();
            let body = Limited::new(res.into_body(), MAX_BODY)
                .collect()
                .await
                .map_err(|_| HttpError::Body)?
                .to_bytes()
                .to_vec();
            Ok(HttpResponse { status, body })
        };
        tokio::time::timeout(TIMEOUT, work)
            .await
            .map_err(|_| HttpError::Timeout)?
    }
}

impl HttpClient for Https {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, HttpError>> {
        Box::pin(self.call(req))
    }
}

/// `https://` to one of [`ALLOWED_HOSTS`] on the default port, or an error.
fn check_target(uri: &Uri) -> Result<(), HttpError> {
    let host = uri.host().ok_or(HttpError::BadUrl)?;
    let ok = uri.scheme_str() == Some("https")
        && uri.port_u16().is_none_or(|p| p == 443)
        && ALLOWED_HOSTS.contains(&host);
    if ok {
        Ok(())
    } else {
        Err(HttpError::NotAllowed(host.to_string()))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scripted client for tests: answers by URL, records what was asked.
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    pub struct FakeHttp {
        pub answers: Vec<(String, Result<HttpResponse, HttpError>)>,
        pub asked: Mutex<Vec<HttpRequest>>,
    }

    impl FakeHttp {
        pub fn answer(mut self, url_prefix: &str, status: u16, body: &str) -> Self {
            self.answers.push((
                url_prefix.into(),
                Ok(HttpResponse {
                    status,
                    body: body.as_bytes().to_vec(),
                }),
            ));
            self
        }

        pub fn fail(mut self, url_prefix: &str, err: HttpError) -> Self {
            self.answers.push((url_prefix.into(), Err(err)));
            self
        }

        pub fn asked(&self) -> Vec<HttpRequest> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl HttpClient for FakeHttp {
        fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, HttpError>> {
            let answer = self
                .answers
                .iter()
                .find(|(prefix, _)| req.url.starts_with(prefix.as_str()))
                .map(|(_, a)| a.clone())
                .unwrap_or(Err(HttpError::Connect("no fake answer".into())));
            self.asked.lock().unwrap().push(req);
            Box::pin(async move { answer })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_usage_hosts_over_https_are_allowed() {
        let ok: Uri = "https://opencode.ai/zen/go/v1/usage".parse().unwrap();
        assert!(check_target(&ok).is_ok());
        for bad in [
            "http://opencode.ai/zen/go/v1/usage",
            "https://example.com/x",
            "https://opencode.ai:8443/x",
            "https://evil.opencode.ai.example.com/x",
        ] {
            let uri: Uri = bad.parse().unwrap();
            assert!(check_target(&uri).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_token_never_shows_in_debug_output() {
        let req = HttpRequest::get("https://opencode.ai/x", Secret::new("tok-placeholder-123"));
        let printed = format!("{req:?}");
        assert!(!printed.contains("tok-placeholder-123"), "{printed}");
    }

    #[tokio::test]
    async fn the_real_client_refuses_other_hosts_before_connecting() {
        let Ok(client) = Https::new() else { return };
        let err = client
            .send(HttpRequest::get(
                "https://example.com/",
                Secret::new("placeholder"),
            ))
            .await
            .unwrap_err();
        assert_eq!(err, HttpError::NotAllowed("example.com".into()));
    }
}
