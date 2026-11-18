//! Melissa's MCP endpoint, served on a sandbox's `mcp.sock`: only `POST /mcp/<token>` for
//! that sandbox's own session tokens. The rest of the queen's HTTP API (the Queen chat,
//! confirmations) is not reachable from inside a sandbox.

use std::convert::Infallible;

use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    body::{Bytes, Incoming},
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use tokio::{net::UnixListener, task::JoinSet};

use crate::mcp::{McpServer, UnknownSession};

/// Largest MCP request body accepted.
const MAX_BODY: usize = 4 * 1024 * 1024;

pub async fn serve(listener: UnixListener, server: McpServer, tokens: Vec<String>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let server = server.clone();
                    let tokens = tokens.clone();
                    conns.spawn(async move {
                        let service = hyper::service::service_fn(move |req| {
                            handle(req, server.clone(), tokens.clone())
                        });
                        let conn = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service);
                        if let Err(e) = conn.await {
                            tracing::debug!("sandbox MCP connection ended: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("sandbox MCP accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

fn reply(status: StatusCode, body: impl Into<Bytes>, json: bool) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(body.into()));
    *res.status_mut() = status;
    if json {
        res.headers_mut().insert(
            hyper::header::CONTENT_TYPE,
            hyper::header::HeaderValue::from_static("application/json"),
        );
    }
    res
}

async fn handle(
    req: Request<Incoming>,
    server: McpServer,
    tokens: Vec<String>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let token = req
        .uri()
        .path()
        .strip_prefix("/mcp/")
        .filter(|t| tokens.iter().any(|own| own == t))
        .map(str::to_string);
    let Some(token) = token else {
        return Ok(reply(StatusCode::NOT_FOUND, "", false));
    };
    if req.method() != Method::POST {
        // No server-initiated event stream, no session DELETE (same as the queen's endpoint).
        return Ok(reply(StatusCode::METHOD_NOT_ALLOWED, "", false));
    }
    let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(reply(StatusCode::PAYLOAD_TOO_LARGE, "", false)),
    };
    let Ok(message) = serde_json::from_slice::<serde_json::Value>(&body) else {
        let err = r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}"#;
        return Ok(reply(StatusCode::BAD_REQUEST, err, true));
    };
    Ok(match server.handle(&token, message).await {
        Ok(Some(answer)) => reply(StatusCode::OK, answer.to_string(), true),
        Ok(None) => reply(StatusCode::ACCEPTED, "", false),
        Err(UnknownSession) => reply(StatusCode::NOT_FOUND, "", false),
    })
}
