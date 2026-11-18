#![forbid(unsafe_code)]
//! Web UI: server-rendered with maud, htmx and CSS embedded in the binary (no CDN).
//!
//! UI rule: only components defined in `components` may be rendered. Agents never
//! generate markup; they select components and fill manifests.

mod api;
pub mod components;
mod pages;

use axum::{
    extract::{Form, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use maud::Markup;
use melissa_agent::{
    mcp::{McpServer, UnknownSession},
    queen::Queen,
    TaskBoard, TaskId,
};
use rust_embed::RustEmbed;
use serde::Deserialize;

pub use axum::Router;

use components::Tone;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// Content Security Policy: everything from this origin only, no inline scripts or styles.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'";

/// The UI router, without agent tools.
pub fn router(board: TaskBoard) -> Router {
    ui_routes(board).layer(middleware::from_fn(security))
}

/// The UI router plus the MCP endpoint (`POST /mcp/{token}`) agent CLIs use to call tools.
pub fn router_with_mcp(board: TaskBoard, mcp: McpServer) -> Router {
    ui_routes(board)
        .merge(
            Router::new()
                .route("/mcp/{token}", post(mcp_post).get(mcp_get))
                .with_state(mcp),
        )
        .layer(middleware::from_fn(security))
}

/// The UI router, the MCP endpoint and the Queen's JSON API (`/api/...`, see [`api`]).
pub fn router_with_queen(board: TaskBoard, mcp: McpServer, queen: Queen) -> Router {
    ui_routes(board)
        .merge(
            Router::new()
                .route("/mcp/{token}", post(mcp_post).get(mcp_get))
                .with_state(mcp),
        )
        .merge(api::routes(queen))
        .layer(middleware::from_fn(security))
}

fn ui_routes(board: TaskBoard) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/healthz", get(|| async { "ok" }))
        .route("/tasks", get(tasks_fragment).post(create_task))
        .route("/tasks/{id}", get(task_page))
        .route("/tasks/{id}/fragment", get(task_fragment))
        .route("/tasks/{id}/cancel", post(cancel_task))
        .route("/assets/{*path}", get(asset))
        .fallback(|| async { (StatusCode::NOT_FOUND, pages::not_found()) })
        .with_state(board)
}

/// MCP streamable-HTTP endpoint, JSON responses only. The token in the path selects the
/// task's session; unknown or closed tokens get 404.
async fn mcp_post(
    State(mcp): State<McpServer>,
    Path(token): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(message) = serde_json::from_slice::<serde_json::Value>(&body) else {
        let err = serde_json::json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}});
        return (StatusCode::BAD_REQUEST, axum::Json(err)).into_response();
    };
    match mcp.handle(&token, message).await {
        Ok(Some(reply)) => axum::Json(reply).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(UnknownSession) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// No server-initiated event stream is offered.
async fn mcp_get() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

pub async fn serve(listener: tokio::net::TcpListener, app: Router) -> std::io::Result<()> {
    axum::serve(listener, app).await
}

async fn dashboard(State(board): State<TaskBoard>) -> Markup {
    pages::dashboard(&board.backends(), &board.list())
}

async fn tasks_fragment(State(board): State<TaskBoard>) -> Markup {
    pages::tasks_region(&board.list(), None)
}

#[derive(Deserialize)]
struct NewTask {
    backend: String,
    prompt: String,
}

async fn create_task(
    State(board): State<TaskBoard>,
    headers: HeaderMap,
    Form(form): Form<NewTask>,
) -> Response {
    let result = board.dispatch(&form.backend, &form.prompt);
    if is_htmx(&headers) {
        let message = match &result {
            Ok(id) => (format!("Dispatched task #{id}."), Tone::Ok),
            Err(e) => (format!("Could not dispatch: {e}."), Tone::Error),
        };
        return pages::tasks_region(&board.list(), Some((&message.0, message.1))).into_response();
    }
    match result {
        Ok(id) => Redirect::to(&format!("/tasks/{id}")).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            pages::error_page("Could not dispatch", &e.to_string()),
        )
            .into_response(),
    }
}

async fn task_page(State(board): State<TaskBoard>, Path(id): Path<TaskId>) -> Response {
    match board.get(id) {
        Some(task) => pages::task_page(&task).into_response(),
        None => (StatusCode::NOT_FOUND, pages::not_found()).into_response(),
    }
}

async fn task_fragment(State(board): State<TaskBoard>, Path(id): Path<TaskId>) -> Response {
    match board.get(id) {
        Some(task) => pages::task_region(&task).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn cancel_task(
    State(board): State<TaskBoard>,
    headers: HeaderMap,
    Path(id): Path<TaskId>,
) -> Response {
    if board.cancel(id).is_err() {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    }
    match (is_htmx(&headers), board.get(id)) {
        (true, Some(task)) => pages::task_region(&task).into_response(),
        _ => Redirect::to(&format!("/tasks/{id}")).into_response(),
    }
}

async fn asset(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(file) => (
            [
                (header::CONTENT_TYPE, mime_for(&path)),
                (header::CACHE_CONTROL, "public, max-age=3600"),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

/// Cross-site request check for state-changing requests, plus security headers.
///
/// Until accounts and session tokens exist, CSRF protection relies on the browser's
/// `Sec-Fetch-Site` and `Origin` headers: a mutating request is rejected unless it
/// comes from this origin. Non-browser clients that send neither header are allowed
/// (the queen only binds to loopback for now).
async fn security(req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe && !same_origin(req.headers()) {
        return (StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    res
}

fn same_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return site == "same-origin" || site == "none";
    }
    match (headers.get(header::ORIGIN), headers.get(header::HOST)) {
        (None, _) => true,
        (Some(origin), Some(host)) => {
            let (Ok(origin), Ok(host)) = (origin.to_str(), host.to_str()) else {
                return false;
            };
            origin == format!("http://{host}") || origin == format!("https://{host}")
        }
        (Some(_), None) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use axum::body::Body;
    use http_body_util::BodyExt;
    use melissa_agent::EchoBackend;
    use tower::ServiceExt;

    use super::*;

    fn app() -> (Router, TaskBoard) {
        let board = TaskBoard::new(
            vec![Arc::new(EchoBackend::new())],
            Duration::from_secs(5),
            None,
        );
        (router(board.clone()), board)
    }

    async fn send(app: &Router, req: axum::http::Request<Body>) -> (StatusCode, HeaderMap, String) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    fn post(uri: &str) -> axum::http::request::Builder {
        axum::http::Request::post(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::HOST, "127.0.0.1:8080")
    }

    #[tokio::test]
    async fn dashboard_lists_backends_and_sets_csp() {
        let (app, _) = app();
        let (status, headers, body) = send(
            &app,
            axum::http::Request::get("/").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("echo"));
        assert!(body.contains("/assets/htmx.min.js"));
        assert!(headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("default-src 'self'"));
    }

    #[tokio::test]
    async fn assets_are_embedded() {
        let (app, _) = app();
        for (path, mime) in [
            ("/assets/htmx.min.js", "text/javascript"),
            ("/assets/melissa.css", "text/css"),
        ] {
            let (status, headers, body) = send(
                &app,
                axum::http::Request::get(path).body(Body::empty()).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with(mime));
            assert!(!body.is_empty());
        }
    }

    #[tokio::test]
    async fn htmx_dispatch_returns_task_list() {
        let (app, board) = app();
        let body = "backend=echo&prompt=hello+hive";
        let req = post("/tasks")
            .header("hx-request", "true")
            .header("sec-fetch-site", "same-origin")
            .body(Body::from(body))
            .unwrap();
        let (status, _, html) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Dispatched task #1"));
        assert_eq!(board.list().len(), 1);
    }

    #[tokio::test]
    async fn plain_dispatch_redirects_and_errors_are_shown() {
        let (app, _) = app();
        let ok = "backend=echo&prompt=hi";
        let (status, headers, _) = send(&app, post("/tasks").body(Body::from(ok)).unwrap()).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], "/tasks/1");

        let bad = "backend=missing&prompt=hi";
        let (status, _, html) = send(&app, post("/tasks").body(Body::from(bad)).unwrap()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(html.contains("unknown backend"));
    }

    #[tokio::test]
    async fn cross_site_posts_are_rejected() {
        let (app, board) = app();
        let body = "backend=echo&prompt=hi";
        let req = post("/tasks")
            .header("sec-fetch-site", "cross-site")
            .body(Body::from(body))
            .unwrap();
        assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);
        let req = post("/tasks")
            .header(header::ORIGIN, "http://evil.example.com")
            .body(Body::from(body))
            .unwrap();
        assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);
        let req = post("/tasks")
            .header(header::ORIGIN, "http://127.0.0.1:8080")
            .body(Body::from(body))
            .unwrap();
        assert_eq!(send(&app, req).await.0, StatusCode::SEE_OTHER);
        assert_eq!(board.list().len(), 1);
    }

    #[tokio::test]
    async fn mcp_endpoint_serves_tools_per_session() {
        use melissa_agent::tools::{TaskContext, ToolRegistry};
        let board = TaskBoard::new(
            vec![Arc::new(EchoBackend::new())],
            Duration::from_secs(5),
            None,
        );
        let mcp = McpServer::new(ToolRegistry::builtin(None));
        let token = mcp.open_session(1, TaskContext::default(), None).unwrap();
        let app = router_with_mcp(board, mcp);
        let call = |path: String, body: &'static str| {
            axum::http::Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap()
        };
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let (status, _, body) = send(&app, call(format!("/mcp/{token}"), list)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("bee_info"));
        let note = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(
            send(&app, call(format!("/mcp/{token}"), note)).await.0,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            send(&app, call("/mcp/nope".into(), list)).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send(&app, call(format!("/mcp/{token}"), "{oops")).await.0,
            StatusCode::BAD_REQUEST
        );
    }

    mod queen_api {
        use melissa_agent::{
            queen::{
                events::EventLog,
                workers::{WorkerLimits, WorkerPool},
                QueenSettings,
            },
            session::{OpenSession, OpenedSession, SessionBackend},
            tools::{Tool, ToolRegistry},
            AgentError, BackendInfo, BoxFuture, McpLink,
        };
        use serde_json::Value;

        use super::*;

        /// A Queen backend whose session never finishes opening.
        struct Stuck;
        impl SessionBackend for Stuck {
            fn info(&self) -> BackendInfo {
                BackendInfo {
                    name: "stuck".into(),
                    kind: "test",
                    detail: String::new(),
                    available: true,
                    tools: true,
                }
            }
            fn open(&self, _: OpenSession) -> BoxFuture<'_, Result<OpenedSession, AgentError>> {
                Box::pin(std::future::pending())
            }
        }

        fn app() -> (Router, Queen) {
            let dir =
                std::env::temp_dir().join(format!("melissa-web-queen-{}", std::process::id()));
            let events = EventLog::new(None);
            let pool = WorkerPool::new(WorkerLimits::default(), events.clone());
            let tools: Vec<Arc<dyn Tool>> = pool.tools();
            let mcp = McpServer::new(ToolRegistry::new(tools, None));
            let board = TaskBoard::with_tools(
                vec![Arc::new(EchoBackend::new())],
                Duration::from_secs(5),
                None,
                McpLink {
                    server: mcp.clone(),
                    base_url: "http://127.0.0.1:9/mcp/".into(),
                    workroot: dir.join("work"),
                },
            );
            let settings = QueenSettings {
                backends: vec!["stuck".into()],
                system_prompt: "test".into(),
                worker_prompt: "test".into(),
                worker_backend: Some("echo".into()),
                state_dir: dir.join("queen"),
                workdir: dir.join("work/queen"),
                tools: None,
            };
            let queen = Queen::new(
                settings,
                vec![Arc::new(Stuck)],
                board.clone(),
                events,
                &pool,
            )
            .unwrap();
            (router_with_queen(board, mcp, queen.clone()), queen)
        }

        fn json_post(uri: &str, body: &str) -> axum::http::Request<Body> {
            axum::http::Request::post(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::HOST, "127.0.0.1:8080")
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn get(uri: &str) -> axum::http::Request<Body> {
            axum::http::Request::get(uri).body(Body::empty()).unwrap()
        }

        async fn send_json(app: &Router, req: axum::http::Request<Body>) -> (StatusCode, Value) {
            let (status, _, body) = send(app, req).await;
            (status, serde_json::from_str(&body).unwrap_or(Value::Null))
        }

        #[tokio::test]
        async fn messages_events_busy_and_cancel() {
            let (app, _) = app();
            let (status, body) = send_json(
                &app,
                json_post("/api/queen/messages", r#"{"text":"Hello"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(body["turn"], 1);
            let since = body["since"].as_u64().unwrap();

            // One turn at a time.
            let (status, body) = send_json(
                &app,
                json_post("/api/queen/messages", r#"{"text":"Again"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(body["error"].as_str().unwrap().contains("previous message"));
            let (status, _) =
                send_json(&app, json_post("/api/queen/messages", r#"{"text":"  "}"#)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            let (_, st) = send_json(&app, get("/api/queen/status")).await;
            assert_eq!(st["turn"], 1);

            let (status, body) = send_json(&app, json_post("/api/queen/cancel", "")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["cancelled"], true);

            let (status, page) = send_json(
                &app,
                get(&format!("/api/queen/events?since={since}&wait=1")),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let types: Vec<&str> = page["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["type"].as_str().unwrap())
                .collect();
            assert_eq!(types, ["user_message", "turn_finished"]);
            assert_eq!(page["events"][1]["stop_reason"], "cancelled");
            assert_eq!(page["missed"], false);
            let next = page["next"].as_u64().unwrap();
            let (_, page) = send_json(&app, get(&format!("/api/queen/events?since={next}"))).await;
            assert_eq!(page["events"], serde_json::json!([]));
            let (_, body) = send_json(&app, json_post("/api/queen/cancel", "")).await;
            assert_eq!(body["cancelled"], false);
            let (_, body) = send_json(&app, get("/api/workers")).await;
            assert_eq!(body["workers"], serde_json::json!([]));
        }

        #[tokio::test]
        async fn confirmations_can_be_listed_and_resolved() {
            let (app, queen) = app();
            let (c, rx) = queen.confirmations().request(
                7,
                "agent:execute",
                &serde_json::json!({"title": "Execute `ls`"}),
                "destructive action",
            );
            let (status, body) = send_json(&app, get("/api/confirmations")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["confirmations"][0]["id"], c.id);
            assert_eq!(body["confirmations"][0]["tool"], "agent:execute");
            let uri = format!("/api/confirmations/{}/approve", c.id);
            let (status, body) = send_json(&app, json_post(&uri, "")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["verdict"], "approved");
            assert_eq!(rx.await.unwrap(), melissa_agent::tools::Verdict::Approved);
            let (status, _) = send_json(&app, json_post(&uri, "")).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let bad = format!("/api/confirmations/{}/maybe", c.id);
            assert_eq!(
                send_json(&app, json_post(&bad, "")).await.0,
                StatusCode::NOT_FOUND
            );
        }

        #[tokio::test]
        async fn cross_site_api_calls_are_rejected() {
            let (app, queen) = app();
            for uri in [
                "/api/queen/messages",
                "/api/queen/cancel",
                "/api/confirmations/1/approve",
            ] {
                let req = axum::http::Request::post(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ORIGIN, "http://evil.example.com")
                    .header(header::HOST, "127.0.0.1:8080")
                    .body(Body::from(r#"{"text":"hi"}"#))
                    .unwrap();
                assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN, "{uri}");
            }
            assert_eq!(queen.status().turn, None);
            // A form post (no JSON content type) is not a message either.
            let req = post("/api/queen/messages")
                .body(Body::from("text=hi"))
                .unwrap();
            assert_eq!(send(&app, req).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
    }

    #[tokio::test]
    async fn task_page_and_missing_task() {
        let (app, board) = app();
        let id = board.dispatch("echo", "ping").unwrap();
        let (status, _, _) = send(
            &app,
            axum::http::Request::get(format!("/tasks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = send(
            &app,
            axum::http::Request::get("/tasks/42")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
