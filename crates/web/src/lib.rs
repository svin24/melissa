#![forbid(unsafe_code)]
//! Web UI: server-rendered with maud, htmx and CSS embedded in the binary (no CDN).
//!
//! UI rule: only components defined in `components` may be rendered. Agents never
//! generate markup; they select components and fill manifests.

mod agents;
mod api;
pub mod components;
mod feed;
mod markdown;
mod pages;
mod settings;
mod state;
mod ui;

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
pub use settings::{CheckFn, Settings, SettingsParts};

use components::Tone;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// Content Security Policy: everything from this origin only, no inline scripts or styles.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'";

/// The task UI, without agent tools or the Queen (`/` goes to the task list).
pub fn router(board: TaskBoard) -> Router {
    ui_routes(board)
        .route("/", get(|| async { Redirect::to("/tasks") }))
        .layer(middleware::from_fn(security))
}

/// The task UI plus the MCP endpoint (`POST /mcp/{token}`) agent CLIs use to call tools.
pub fn router_with_mcp(board: TaskBoard, mcp: McpServer) -> Router {
    ui_routes(board)
        .route("/", get(|| async { Redirect::to("/tasks") }))
        .merge(
            Router::new()
                .route("/mcp/{token}", post(mcp_post).get(mcp_get))
                .with_state(mcp),
        )
        .layer(middleware::from_fn(security))
}

/// The full UI: the chat with the Queen as home (see [`ui`]), the task pages, the MCP
/// endpoint and the Queen's JSON API (`/api/...`, see [`api`]).
pub fn router_with_queen(board: TaskBoard, mcp: McpServer, queen: Queen) -> Router {
    router_with_settings(board, mcp, queen, Settings::detached())
}

/// [`router_with_queen`] plus the Settings pages and API (`/settings/...`,
/// `/api/settings/...`) backed by the queen's provider settings and update helper.
pub fn router_with_settings(
    board: TaskBoard,
    mcp: McpServer,
    queen: Queen,
    settings: Settings,
) -> Router {
    let ui = ui::Ui::new(
        queen.clone(),
        state::HiveCache::new(mcp.clone(), Some(queen.clone())),
        settings,
    );
    ui_routes(board)
        .merge(ui::routes(ui.clone()))
        .merge(settings::api_routes(ui))
        .merge(api::tool_routes(mcp.clone()))
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
        .route("/healthz", get(|| async { "ok" }))
        .route("/tasks", get(dashboard).post(create_task))
        .route("/tasks/fragment", get(tasks_fragment))
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
                // Fonts never change under the same name. CSS and scripts change with a new
                // binary, so browsers revalidate them every time (small, served on the LAN).
                (
                    header::CACHE_CONTROL,
                    if path.starts_with("fonts/") {
                        "public, max-age=31536000, immutable"
                    } else {
                        "no-cache"
                    },
                ),
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
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub(crate) fn is_htmx(headers: &HeaderMap) -> bool {
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
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    fn post(uri: &str) -> axum::http::request::Builder {
        axum::http::Request::post(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::HOST, "127.0.0.1:8080")
    }

    #[tokio::test]
    async fn dashboard_lists_backends_and_sets_csp() {
        let (app, _) = app();
        // Without a Queen, home is the task list.
        let (status, headers, _) = send(
            &app,
            axum::http::Request::get("/").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], "/tasks");
        let (status, headers, body) = send(
            &app,
            axum::http::Request::get("/tasks")
                .body(Body::empty())
                .unwrap(),
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
            ("/assets/htmx-ext-sse.min.js", "text/javascript"),
            ("/assets/melissa.js", "text/javascript"),
            ("/assets/melissa.css", "text/css"),
            ("/assets/mark.svg", "image/svg+xml"),
            (
                "/assets/fonts/AtkinsonHyperlegibleNext-Variable.woff2",
                "font/woff2",
            ),
            (
                "/assets/fonts/BricolageGrotesque-Variable.woff2",
                "font/woff2",
            ),
            (
                "/assets/fonts/AtkinsonHyperlegibleNext-OFL.txt",
                "text/plain",
            ),
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
        assert!(body.contains("comb_info"));
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
                bees::{BeeLimits, BeePool},
                events::{EventLog, QueenEvent},
                registry::BeeRegistry,
                transcript::Transcript,
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

        /// A reversible tool: an unattended call needs a confirmation.
        struct Touch;
        impl Tool for Touch {
            fn spec(&self) -> melissa_agent::tools::ToolSpec {
                melissa_agent::tools::ToolSpec {
                    name: "touch",
                    description: "test",
                    labels: melissa_agent::tools::Labels {
                        effect: melissa_agent::tools::Effect::Reversible,
                        ..melissa_agent::tools::Labels::READ_ONLY
                    },
                    params: serde_json::json!({"type": "object"}),
                }
            }
            fn call<'a>(
                &'a self,
                args: Value,
            ) -> BoxFuture<'a, Result<Value, melissa_agent::tools::ToolError>> {
                Box::pin(async move { Ok(serde_json::json!({"touched": args["what"]})) })
            }
        }

        /// A read-only tool with a fixed answer (stands in for the hive's tools).
        struct Fixed(&'static str, Value);
        impl Tool for Fixed {
            fn spec(&self) -> melissa_agent::tools::ToolSpec {
                melissa_agent::tools::ToolSpec {
                    name: self.0,
                    description: "test",
                    labels: melissa_agent::tools::Labels::READ_ONLY,
                    params: serde_json::json!({"type": "object"}),
                }
            }
            fn call<'a>(
                &'a self,
                _: Value,
            ) -> BoxFuture<'a, Result<Value, melissa_agent::tools::ToolError>> {
                Box::pin(async move { Ok(self.1.clone()) })
            }
        }

        fn hive_tools() -> Vec<Arc<dyn Tool>> {
            vec![
                Arc::new(Fixed(
                    "list_apps",
                    serde_json::json!({
                        "apps": [
                            {"app": "nextcloud", "phase": "ready", "url": "http://nextcloud.example.com"},
                            {"app": "immich", "phase": "starting"},
                        ],
                        "not_installed": ["jellyfin"],
                    }),
                )),
                Arc::new(Fixed(
                    "cluster_health",
                    serde_json::json!({"nodes": [{"name": "comb-1", "ready": true, "allocatable": {"cpu": "4"}}]}),
                )),
                Arc::new(Fixed(
                    "comb_info",
                    serde_json::json!({"os": "Fedora Linux 45", "mem_total_kib": 8_388_608, "mem_available_kib": 4_194_304,
                        "disks": [{"path": "/", "total_bytes": 10_000_000_000u64, "available_bytes": 5_000_000_000u64, "used_percent": 50},
                                  {"path": "/var", "total_bytes": 20_000_000_000u64, "available_bytes": 1_800_000_000u64, "used_percent": 91}]}),
                )),
            ]
        }

        fn app_with(page_settings: Option<Settings>) -> (Router, Queen) {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "melissa-web-queen-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let events = EventLog::new(Some(Transcript::new(
                dir.join("queen/transcript.jsonl"),
                1 << 20,
            )));
            let registry = BeeRegistry::open(&dir.join("bees"), "c-0a1b2c3d").unwrap();
            let pool = BeePool::new(BeeLimits::default(), events.clone(), registry, None);
            let mut tools: Vec<Arc<dyn Tool>> = pool.tools();
            tools.push(Arc::new(Touch));
            tools.extend(hive_tools());
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
                // `gone` is not configured: shown as not installed.
                backends: vec!["stuck".into(), "echo-b".into(), "gone".into()],
                system_prompt: "test".into(),
                bee_prompt: "test".into(),
                bee_backend: Some("stuck".into()),
                state_dir: dir.join("queen"),
                workdir: dir.join("work/queen"),
                tools: None,
                comb: "c-0a1b2c3d".into(),
            };
            let queen = Queen::new(
                settings,
                vec![
                    Arc::new(Stuck),
                    Arc::new(melissa_agent::EchoSessionBackend::new(
                        &melissa_agent::EchoSpec {
                            name: "echo-b".into(),
                        },
                    )),
                ],
                board.clone(),
                events,
                &pool,
            )
            .unwrap();
            let router = match page_settings {
                Some(s) => router_with_settings(board, mcp, queen.clone(), s),
                None => router_with_queen(board, mcp, queen.clone()),
            };
            (router, queen)
        }

        fn app() -> (Router, Queen) {
            app_with(None)
        }

        // --- Settings ------------------------------------------------------------------------

        use melissa_settings::{
            providers::{BackendFacts, ProviderStore},
            updates::{HostKind, Release, UpdateEnv},
        };

        struct SettingsFixture {
            dir: std::path::PathBuf,
        }

        impl SettingsFixture {
            fn new(name: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "melissa-web-settings-{name}-{}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(dir.join("bin")).unwrap();
                // A fake `grok` that answers --version.
                let grok = dir.join("bin/grok");
                std::fs::write(&grok, "#!/bin/sh\necho 'grok 0.9.1'\n").unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&grok, std::fs::Permissions::from_mode(0o755)).unwrap();
                Self { dir }
            }

            fn settings(&self, kind: HostKind, helper: bool) -> Settings {
                let fact = |name: &str, provider: &str, session: bool, command: Option<String>| {
                    BackendFacts {
                        name: name.into(),
                        provider: provider.into(),
                        session,
                        command,
                        enabled: true,
                    }
                };
                let root = self.dir.join("root");
                if helper {
                    for f in [
                        "usr/lib/systemd/system/melissa-update-check.service",
                        "usr/share/polkit-1/rules.d/50-melissa.rules",
                    ] {
                        std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
                        std::fs::write(root.join(f), "").unwrap();
                    }
                }
                let check: CheckFn = Arc::new(|_, o| {
                    if o.args.iter().any(|a| a == "--bad") {
                        Err("claude-session: argument `--bad` is not allowed here".into())
                    } else {
                        Ok(())
                    }
                });
                Settings::new(SettingsParts {
                    store: ProviderStore::new(
                        self.dir.join("config/providers.toml"),
                        Default::default(),
                    ),
                    backends: vec![
                        // The Queen's first backend: Melissa runs on Claude.
                        fact("stuck", "claude", true, Some("/nonexistent/claude".into())),
                        fact(
                            "grok-acp",
                            "grok",
                            true,
                            Some(self.dir.join("bin/grok").display().to_string()),
                        ),
                        fact("echo-b", "echo", true, None),
                        fact("echo", "echo", false, None),
                    ],
                    bee_default: Some("stuck".into()),
                    check: Some(check),
                    audit: Some(melissa_agent::AuditLog::new(self.dir.join("audit.jsonl"))),
                    updates: UpdateEnv {
                        root,
                        kind,
                        state_dir: self.dir.join("update"),
                    },
                    release: Release {
                        version: "0.3.1".into(),
                        variant: Some("headless".into()),
                        source: "image",
                    },
                    home: Some("/var/lib/melissa/home".into()),
                })
            }

            fn audit(&self) -> String {
                std::fs::read_to_string(self.dir.join("audit.jsonl")).unwrap_or_default()
            }
        }

        impl Drop for SettingsFixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        fn form(uri: &str, body: &str, htmx: bool) -> axum::http::Request<Body> {
            let mut b = post(uri);
            if htmx {
                b = b.header("hx-request", "true");
            }
            b.body(Body::from(body.to_string())).unwrap()
        }

        #[tokio::test]
        async fn settings_pages_render_and_link_from_the_rail() {
            let fx = SettingsFixture::new("render");
            let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
            let (status, headers, _) = send(&app, get("/settings")).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/settings/providers");
            let (status, _, html) = send(&app, get("/settings/providers?p=grok")).await;
            assert_eq!(status, StatusCode::OK);
            for want in [
                "href=\"/settings\"",
                "Settings",
                "Providers",
                "Claude Code",
                "Grok",
                "v0.9.1",
                "role=\"switch\"",
                "Melissa runs on it now",
                "Checked just now",
            ] {
                assert!(html.contains(want), "missing {want}");
            }
            // Claude isn't installed here, but this is no Melissa image: no melissa-setup line.
            assert!(!html.contains("melissa-setup"));
            let (status, _, html) = send(&app, get("/settings/updates")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("0.3.1") && html.contains("From source"));
            assert!(html.contains("git pull"));
            assert!(html.contains("version 0.9.1"));
        }

        #[tokio::test]
        async fn secrets_are_write_only_everywhere() {
            let fx = SettingsFixture::new("secrets");
            let (app, _) = app_with(Some(fx.settings(HostKind::Bootc, false)));
            let secret = "xai-very-secret-123";
            let (status, _, html) = send(
                &app,
                form(
                    "/ui/settings/providers/grok/env",
                    &format!("name=XAI_API_KEY&value={secret}"),
                    true,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("XAI_API_KEY saved"));
            assert!(html.contains("XAI_API_KEY") && !html.contains(secret));
            let (_, _, html) = send(
                &app,
                form(
                    "/ui/settings/providers/grok/env",
                    "name=XAI_BASE_URL&value=https://llm.example.com",
                    true,
                ),
            )
            .await;
            // Non-secret values are shown.
            assert!(html.contains("https://llm.example.com"));
            let (_, _, page) = send(&app, get("/settings/providers?p=grok")).await;
            assert!(!page.contains(secret));
            assert!(page.contains("API key set"));
            let (status, list) = send_json(&app, get("/api/settings/providers")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!list.to_string().contains(secret));
            let grok = list["providers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == "grok")
                .unwrap();
            let env = grok["settings"]["env"].as_array().unwrap();
            let key = env.iter().find(|v| v["name"] == "XAI_API_KEY").unwrap();
            assert_eq!(key["secret"], true);
            assert!(key.get("value").is_none());
            assert_eq!(list["restart_needed"], true);
            // The API too: set through JSON, never echoed back.
            let (status, body) = send_json(
                &app,
                json_post(
                    "/api/settings/providers/grok",
                    r#"{"set_env": {"GROK_TOKEN": "tok-abc-999"}}"#,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(!body.to_string().contains("tok-abc-999"));
            // Audit: what changed and a hash, never the value.
            let audit = fx.audit();
            assert!(
                audit.contains("settings.provider")
                    && audit.contains("grok: variable XAI_API_KEY set")
            );
            assert!(audit.contains(&melissa_settings::sha256_hex(secret.as_bytes())));
            assert!(!audit.contains(secret) && !audit.contains("tok-abc-999"));
            // The file has it (0600), for the agent.
            let path = fx.dir.join("config/providers.toml");
            assert!(std::fs::read_to_string(&path).unwrap().contains(secret));
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            // Remove.
            let (_, _, html) = send(
                &app,
                form(
                    "/ui/settings/providers/grok/env/remove",
                    "name=XAI_API_KEY",
                    true,
                ),
            )
            .await;
            assert!(html.contains("XAI_API_KEY removed"));
        }

        #[tokio::test]
        async fn provider_changes_are_checked() {
            let fx = SettingsFixture::new("checks");
            let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
            // Melissa runs on Claude: refused.
            let (status, body) = send_json(
                &app,
                json_post("/api/settings/providers/claude", r#"{"enabled": false}"#),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(body["error"].as_str().unwrap().contains("runs on it now"));
            // The toggle form gets the same answer in the region.
            let (status, _, html) = send(
                &app,
                form("/ui/settings/providers/claude/enabled", "", true),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Can&#39;t disable it") || html.contains("Can't disable it"));
            // Grok can go; it applies after a restart.
            let (status, body) = send_json(
                &app,
                json_post("/api/settings/providers/grok", r#"{"enabled": false}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["enabled"], false);
            assert_eq!(body["running"], true);
            assert_eq!(body["restart_needed"], true);
            // Bad input: our own checks, then the backend's.
            for (body, want) in [
                (r#"{"args": ["--yolo"]}"#, "--yolo"),
                (r#"{"args": ["--bad"]}"#, "--bad"),
                (r#"{"command": "/usr/bin/other"}"#, "must be called"),
                (r#"{"set_env": {"PATH": "/tmp"}}"#, "PATH"),
            ] {
                let (status, b) =
                    send_json(&app, json_post("/api/settings/providers/claude", body)).await;
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
                assert!(b["error"].as_str().unwrap().contains(want), "{b}");
            }
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/providers/nope", r#"{"enabled": false}"#),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/providers/echo", r#"{"command": "/x/echo"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            // Saved settings and what is escaped.
            let (status, _, _) = send(
                &app,
                form(
                    "/ui/settings/providers/claude",
                    "display_name=%3Cb%3EMine%3C%2Fb%3E&command=&args=--effort%0D%0Ahigh",
                    false,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            let (_, _, html) = send(&app, get("/settings/providers?p=claude")).await;
            assert!(html.contains("&lt;b&gt;Mine&lt;/b&gt;") && !html.contains("<b>Mine"));
            assert!(html.contains("--effort\nhigh"));
            assert!(fx.audit().contains("claude: extra arguments set (2)"));
            // Cross-site posts are refused like every other.
            let req = axum::http::Request::post("/api/settings/providers/grok")
                .header(header::CONTENT_TYPE, "application/json")
                .header("sec-fetch-site", "cross-site")
                .body(Body::from(r#"{"enabled": true}"#))
                .unwrap();
            let (status, _, _) = send(&app, req).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn updates_need_a_yes_and_the_helper() {
            let fx = SettingsFixture::new("updates");
            std::fs::create_dir_all(fx.dir.join("update")).unwrap();
            std::fs::write(
                fx.dir.join("update/status.json"),
                include_str!("../../settings/tests/fixtures/bootc-status-staged.json"),
            )
            .unwrap();
            std::fs::write(
                fx.dir.join("update/last.json"),
                r#"{"action":"check","ok":false,"at":1,"message":"<i>registry down</i>"}"#,
            )
            .unwrap();
            let (app, _) = app_with(Some(fx.settings(HostKind::Bootc, true)));
            let (_, _, html) = send(&app, get("/settings/updates")).await;
            for want in [
                "Ready for the next start",
                "43.20261119.0",
                "sha256:5b0d3c1e9f2a",
                "Previous (for a rollback)",
                "Apply update (restarts)",
                "Roll back (restarts)",
                "&lt;i&gt;registry down",
            ] {
                assert!(html.contains(want), "missing {want}");
            }
            // Apply without a yes: the confirmation card, nothing started.
            let (status, _, html) = send(&app, form("/ui/settings/updates/apply", "", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Install and restart") && html.contains("name=\"confirm\""));
            let (_, _, html) = send(&app, form("/ui/settings/updates/rollback", "", true)).await;
            assert!(html.contains("Roll back and restart"));
            let (status, _) = send_json(&app, json_post("/api/settings/updates/apply", "{}")).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(fx.audit().is_empty(), "nothing was requested");
            let (status, body) = send_json(&app, get("/api/settings/updates")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["host"], "bootc");
            assert_eq!(body["release"]["version"], "0.3.1");
            assert_eq!(
                body["actions"],
                serde_json::json!(["check", "apply", "rollback", "restart"])
            );
            // Without the helper: only commands, and a 501 for actions.
            let fx2 = SettingsFixture::new("updates-nohelper");
            let (app, _) = app_with(Some(fx2.settings(HostKind::Bootc, false)));
            let (_, _, html) = send(&app, get("/settings/updates")).await;
            assert!(
                html.contains("sudo bootc upgrade --apply")
                    && !html.contains("Apply update (restarts)")
            );
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/updates/apply", r#"{"confirm": true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
            // Container mode says how to update.
            let fx3 = SettingsFixture::new("updates-container");
            let (app, _) = app_with(Some(fx3.settings(HostKind::Container, false)));
            let (_, _, html) = send(&app, get("/settings/updates")).await;
            assert!(html.contains("container-run.sh build"));
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
            let (_, body) = send_json(&app, get("/api/bees")).await;
            assert_eq!(body["bees"], serde_json::json!([]));
        }

        #[tokio::test]
        async fn the_user_talks_to_a_bee_directly() {
            let (app, queen) = app();
            let ada = queen
                .bees()
                .registry()
                .create("Ada", "stuck", None, "the Queen (task 1)")
                .unwrap();
            let (_, body) = send_json(&app, get("/api/bees")).await;
            assert_eq!(body["bees"][0]["name"], "Ada");
            assert_eq!(body["bees"][0]["status"], "idle");

            // By name (any case) or id; same shapes as the Queen's chat.
            let (status, body) = send_json(
                &app,
                json_post("/api/bees/ada/messages", r#"{"text":"Hello Ada"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            assert_eq!(body["turn"], 1);
            let since = body["since"].as_u64().unwrap();
            let uri = format!("/api/bees/{}/messages", ada.id);
            let (status, _) = send_json(&app, json_post(&uri, r#"{"text":"Again"}"#)).await;
            assert_eq!(status, StatusCode::CONFLICT);
            let (_, st) = send_json(&app, get("/api/bees/Ada/status")).await;
            assert_eq!(st["turn"], 1);
            let (_, b) = send_json(&app, get("/api/bees/Ada")).await;
            assert_eq!(b["status"], "working");
            let (_, body) = send_json(&app, json_post("/api/bees/Ada/cancel", "")).await;
            assert_eq!(body["cancelled"], true);
            let (status, page) = send_json(
                &app,
                get(&format!("/api/bees/Ada/events?since={since}&wait=1")),
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
            assert_eq!(page["events"][0]["text"], "Hello Ada");
            // The Queen's log did not get the bee's conversation.
            let (_, page) = send_json(&app, get("/api/queen/events?since=0")).await;
            assert_eq!(page["events"], serde_json::json!([]));

            // The user renames; names stay unique.
            let (status, body) = send_json(
                &app,
                json_post("/api/bees/Ada/rename", r#"{"name":"Ada Lovelace"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["id"], ada.id.as_str());
            queen
                .bees()
                .registry()
                .create("Bo", "stuck", None, "the user")
                .unwrap();
            let (status, _) = send_json(
                &app,
                json_post("/api/bees/Bo/rename", r#"{"name":"ada lovelace"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            for (req, code) in [
                (get("/api/bees/nobody"), StatusCode::NOT_FOUND),
                (
                    json_post("/api/bees/nobody/messages", r#"{"text":"hi"}"#),
                    StatusCode::NOT_FOUND,
                ),
                // No cell store here (no k3s).
                (get("/api/cells"), StatusCode::BAD_GATEWAY),
                (
                    json_post("/api/cells/nextcloud/reassign", r#"{"to":"nobody"}"#),
                    StatusCode::NOT_FOUND,
                ),
            ] {
                assert_eq!(send_json(&app, req).await.0, code);
            }
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
        async fn operator_tool_calls_wait_for_the_inbox() {
            let (app, queen) = app();
            // Unknown and Queen-only tools look the same: not found.
            for name in ["nope", "create_bee", "reassign_cell"] {
                let (status, _) =
                    send_json(&app, json_post(&format!("/api/tools/{name}"), "{}")).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{name}");
            }
            let call = {
                let app = app.clone();
                tokio::spawn(async move {
                    send_json(&app, json_post("/api/tools/touch", r#"{"what":"x"}"#)).await
                })
            };
            let pending = loop {
                if let Some(c) = queen.confirmations().pending().first().cloned() {
                    break c;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            assert_eq!(pending.tool, "touch");
            assert_eq!(pending.reason, "changes state while nobody is watching");
            let uri = format!("/api/confirmations/{}/approve", pending.id);
            assert_eq!(send_json(&app, json_post(&uri, "")).await.0, StatusCode::OK);
            let (status, body) = call.await.unwrap();
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["result"]["touched"], "x");

            // Denied → 403.
            let call = {
                let app = app.clone();
                tokio::spawn(
                    async move { send_json(&app, json_post("/api/tools/touch", "{}")).await },
                )
            };
            let pending = loop {
                if let Some(c) = queen.confirmations().pending().first().cloned() {
                    break c;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            let uri = format!("/api/confirmations/{}/deny", pending.id);
            send_json(&app, json_post(&uri, "")).await;
            assert_eq!(call.await.unwrap().0, StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn cross_site_api_calls_are_rejected() {
            let (app, queen) = app();
            for uri in [
                "/api/queen/messages",
                "/api/queen/cancel",
                "/api/bees/Ada/messages",
                "/api/bees/Ada/rename",
                "/api/cells/nextcloud/reassign",
                "/api/confirmations/1/approve",
                "/api/tools/touch",
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

        fn htmx_post(uri: &str, body: &str) -> axum::http::Request<Body> {
            post(uri)
                .header("hx-request", "true")
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        async fn get_html(app: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
            send(app, get(uri)).await
        }

        /// The next SSE frame of a streaming response, as text.
        async fn next_frame(body: &mut Body) -> String {
            let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
                .await
                .expect("an SSE frame within 5 s")
                .expect("stream still open")
                .unwrap();
            String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
        }

        #[tokio::test]
        async fn home_is_the_chat_with_the_app_strip() {
            let (app, queen) = app();
            let (status, headers, body) = get_html(&app, "/").await;
            assert_eq!(status, StatusCode::OK);
            assert!(headers[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("script-src 'self'"));
            assert!(body.contains("Pre-alpha."));
            assert!(body.contains("Hi, I'm Melissa."));
            assert!(body.contains(&format!(
                "sse-connect=\"/ui/stream?since=0&amp;epoch={}\"",
                queen.events().epoch()
            )));
            assert!(body.contains("/assets/htmx-ext-sse.min.js"));
            assert!(body.contains("Nextcloud running, Immich installing"));
            assert!(body.contains("comb-1"));
            // Before the first message: the agent it will start on, as a menu.
            assert!(
                body.contains("Runs on <details class=\"agent-menu\">"),
                "{body}"
            );
            assert!(body.contains("id=\"agent-choice\""));
            assert!(!body.contains("style="), "no inline styles");
            assert!(!body.contains("<script>"), "no inline scripts");

            // A conversation with a tool call and a pending confirmation.
            queen.events().push(QueenEvent::UserMessage {
                turn: 1,
                text: "Install <b>Immich</b>".into(),
            });
            queen.events().push(QueenEvent::ToolCall {
                turn: Some(1),
                id: "x\" onclick=\"alert(1)".into(),
                title: "melissa__list_apps".into(),
                status: "done".into(),
            });
            let (c, _rx) = queen.confirmations().request(
                1,
                "install_app",
                &serde_json::json!({"app": "immich"}),
                "this task has read untrusted content",
            );
            queen.events().push(QueenEvent::ConfirmationNeeded {
                id: c.id,
                task: 1,
                tool: "install_app".into(),
                reason: c.reason.clone(),
            });
            let (_, _, body) = get_html(&app, "/?draft=Install%20jellyfin").await;
            assert!(body.contains("Install &lt;b&gt;Immich&lt;/b&gt;"));
            assert!(body.contains("Listed your apps"));
            assert!(
                body.contains("id=\"f-t1\""),
                "agent ids never reach the DOM"
            );
            assert!(!body.contains("onclick"));
            assert!(body.contains("Install Immich?"));
            assert!(body.contains(&format!("hx-post=\"/ui/confirmations/{}/approve\"", c.id)));
            assert!(body.contains(">Install jellyfin</textarea>"));
            assert!(body.contains("sse-connect=\"/ui/stream?since=3&amp;epoch="));
            // One waiting in the rail.
            assert!(body.contains("<span class=\"count\">1"));
        }

        #[tokio::test]
        async fn hive_and_strip_show_apps_and_devices() {
            let (app, _) = app();
            let (status, _, body) = get_html(&app, "/hive").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("class=\"hex h-ok\""));
            assert!(body.contains("class=\"hex h-busy\""));
            assert!(body.contains("href=\"http://nextcloud.example.com\""));
            assert!(body.contains("Add an app"));
            assert!(body.contains("href=\"/?draft=Install%20jellyfin\""));
            assert!(body.contains("Fedora Linux 45"));
            assert!(body.contains("4.0 GB of 8.0 GB used"));
            // comb_info's /var is 91% used: a storage meter and a warning on the card.
            assert!(body.contains("<span>Storage</span>"));
            assert!(body.contains("value=\"91\""));
            assert!(body.contains("Storage is almost full: 1.7 GB left."));
            let (status, _, body) = get_html(&app, "/ui/strip").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.starts_with("<section class=\"strip\""));
            assert!(body
                .contains("<span class=\"strip-warn\">Storage almost full (1.7 GB left)</span>"));
            let (_, _, body) = get_html(&app, "/ui/hive/apps").await;
            assert!(body.starts_with("<div id=\"hive-apps\""));
        }

        #[tokio::test]
        async fn approvals_page_and_approve_deny() {
            let (app, queen) = app();
            let (_, _, body) = get_html(&app, "/approvals").await;
            assert!(body.contains("Nothing waiting."));

            let (a, rx_a) = queen.confirmations().request(
                1,
                "uninstall_app",
                &serde_json::json!({"app": "nextcloud", "delete_data": true}),
                "destructive action",
            );
            let (b, rx_b) =
                queen
                    .confirmations()
                    .request(1, "touch", &serde_json::json!({}), "because");
            let (_, _, body) = get_html(&app, "/approvals").await;
            assert!(body.contains("Remove Nextcloud?"));
            assert!(body.contains("Can't be undone."));
            assert!(body.contains("approval destructive"));
            assert!(body.contains("Allow touch?"));
            assert!(body.contains("view=approvals"));

            // htmx: the card is replaced by its decided state.
            let uri = format!("/ui/confirmations/{}/approve", a.id);
            let (status, _, html) = send(&app, htmx_post(&uri, "")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("You allowed this."));
            assert!(!html.contains("<button"));
            assert_eq!(rx_a.await.unwrap(), melissa_agent::tools::Verdict::Approved);
            // Again: already gone.
            let (status, _, html) = send(&app, htmx_post(&uri, "")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Already decided"));

            // Without htmx: a redirect back to the list.
            let uri = format!("/ui/confirmations/{}/deny", b.id);
            let (status, headers, _) = send(&app, post(&uri).body(Body::empty()).unwrap()).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/approvals");
            assert_eq!(rx_b.await.unwrap(), melissa_agent::tools::Verdict::Denied);

            let (status, _, _) = send(&app, htmx_post("/ui/confirmations/1/maybe", "")).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn sending_a_message_from_the_composer() {
            let (app, queen) = app();
            let (status, _, html) = send(&app, htmx_post("/ui/messages", "text=Hello+there")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.starts_with("<form class=\"composer\" id=\"composer\""));
            assert!(html.contains("Working on it"));
            assert!(html.contains("></textarea>"), "box is cleared");
            assert_eq!(queen.status().turn, Some(1));

            // Busy: the text stays and the reason is shown.
            let (_, _, html) = send(&app, htmx_post("/ui/messages", "text=Second")).await;
            assert!(html.contains("Still working on your last message"));
            assert!(html.contains(">Second</textarea>"));

            // Stop.
            let (status, _, _) = send(&app, htmx_post("/ui/cancel", "")).await;
            assert_eq!(status, StatusCode::NO_CONTENT);
            assert_eq!(queen.status().turn, None);

            // Plain form post: redirect home.
            let req = post("/ui/messages").body(Body::from("text=Hi")).unwrap();
            let (status, headers, _) = send(&app, req).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/");
        }

        #[tokio::test]
        async fn chatting_with_a_helper() {
            let (app, queen) = app();
            let ada = queen
                .bees()
                .registry()
                .create("Ada", "stuck", None, "the Queen (task 1)")
                .unwrap();
            // The rail lists her; her page is a chat.
            let (_, _, home) = get_html(&app, "/").await;
            assert!(home.contains(&format!("href=\"/bees/{}\"", ada.id)));
            assert!(home.contains(">Ada<"));
            for uri in [format!("/bees/{}", ada.id), "/bees/ada".to_string()] {
                let (status, _, body) = get_html(&app, &uri).await;
                assert_eq!(status, StatusCode::OK, "{uri}");
                assert!(body.contains("Nothing here yet with Ada."));
                assert!(body.contains(&format!(
                    "sse-connect=\"/ui/bees/{}/stream?since=0&amp;epoch=",
                    ada.id
                )));
                assert!(body.contains(&format!("hx-post=\"/ui/bees/{}/messages\"", ada.id)));
                assert!(body.contains("aria-current=\"page\""));
            }
            assert_eq!(
                get_html(&app, "/bees/nobody").await.0,
                StatusCode::NOT_FOUND
            );

            // A message goes to her conversation, not the Queen's.
            let res = app
                .clone()
                .oneshot(get(&format!("/ui/bees/{}/stream?since=0", ada.id)))
                .await
                .unwrap();
            let mut body = res.into_body();
            let uri = format!("/ui/bees/{}/messages", ada.id);
            let (status, _, html) = send(&app, htmx_post(&uri, "text=Hello+Ada")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Working on it"));
            assert!(html.contains(&format!("hx-post=\"/ui/bees/{}/cancel\"", ada.id)));
            let frame = next_frame(&mut body).await;
            assert!(frame.contains("Hello Ada"), "{frame}");
            assert_eq!(queen.status().turn, None);
            let (status, _, _) =
                send(&app, htmx_post(&format!("/ui/bees/{}/cancel", ada.id), "")).await;
            assert_eq!(status, StatusCode::NO_CONTENT);

            // The Queen's feed links a helper's job to her conversation.
            queen.events().push(QueenEvent::BeeStarted {
                bee: ada.id.clone(),
                name: "Ada".into(),
                goal: "Check <the> device".into(),
            });
            let (_, _, home) = get_html(&app, "/").await;
            assert!(home.contains(&format!(
                "<a href=\"/bees/{}\">Ada is looking into it</a>",
                ada.id
            )));
            assert!(home.contains("Check &lt;the&gt; device"));
        }

        #[tokio::test]
        async fn the_taint_note_shows_in_the_chat() {
            let (app, queen) = app();
            queen.events().push(QueenEvent::Tainted {
                tool: Some("app_logs".into()),
                subject: Some("nextcloud".into()),
            });
            let (_, _, home) = get_html(&app, "/").await;
            assert!(home.contains("class=\"note guard\""), "{home}");
            assert!(home.contains(
                "Melissa read text from outside (Nextcloud's logs). Changes now need your OK \
                 until you start a new chat."
            ));
        }

        #[tokio::test]
        async fn a_new_chat_archives_the_old_one_and_reloads_pages() {
            let (app, queen) = app();
            queen.events().push(QueenEvent::UserMessage {
                turn: 1,
                text: "Hi <there>".into(),
            });
            queen.events().push(QueenEvent::TurnFinished {
                turn: 1,
                stop_reason: "end_turn".into(),
            });
            let (c, _rx) = queen.confirmations().request(
                queen.task_id(),
                "touch",
                &serde_json::json!({}),
                "because",
            );
            let (other, _rx2) =
                queen
                    .confirmations()
                    .request(99, "touch", &serde_json::json!({}), "a bee's");
            let res = app
                .clone()
                .oneshot(get(&format!(
                    "/ui/stream?since={}&epoch={}",
                    queen.events().last_seq(),
                    queen.events().epoch()
                )))
                .await
                .unwrap();
            let mut stream = res.into_body();

            let req = post("/ui/new-chat")
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap();
            let (status, headers, _) = send(&app, req).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/");
            // The Queen's confirmation is withdrawn, a bee's stays.
            let pending: Vec<u64> = queen
                .confirmations()
                .pending()
                .iter()
                .map(|p| p.id)
                .collect();
            assert_eq!(pending, [other.id]);
            assert!(next_frame(&mut stream).await.starts_with("event: reload"));

            let (_, _, home) = get_html(&app, "/").await;
            assert!(home.contains("Hi, I'm Melissa."));
            assert!(!home.contains("<li class=\"entry\""), "{home}");
            assert!(home.contains("Past chats"));
            assert!(home.contains("Hi &lt;there&gt;"));
            let chats = queen.past_chats();
            assert_eq!(chats.len(), 1);
            let (status, _, past) = get_html(&app, &format!("/chats/{}", chats[0].id)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(past.contains("Hi &lt;there&gt;"));
            assert!(!past.contains("sse-connect"), "read-only");
            assert!(!past.contains("name=\"text\""), "no composer");
            assert_eq!(get_html(&app, "/chats/1").await.0, StatusCode::NOT_FOUND);
            let _ = c;

            // Cross-site posts are refused.
            let req = post("/ui/new-chat")
                .header("sec-fetch-site", "cross-site")
                .body(Body::empty())
                .unwrap();
            assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);
            // The JSON API does the same.
            let (status, body) = send_json(&app, json_post("/api/queen/new-chat", "{}")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["archived"], Value::Null, "nothing said since");
        }

        #[tokio::test]
        async fn a_stream_from_another_epoch_reloads() {
            let (app, queen) = app();
            queen.events().push(QueenEvent::UserMessage {
                turn: 1,
                text: "Hi".into(),
            });
            let mut body = app
                .clone()
                .oneshot(get("/ui/stream?since=1&epoch=12345"))
                .await
                .unwrap()
                .into_body();
            assert!(next_frame(&mut body).await.starts_with("event: reload"));
            let req = axum::http::Request::get("/ui/stream")
                .header("last-event-id", "12345-1")
                .body(Body::empty())
                .unwrap();
            let mut body = app.clone().oneshot(req).await.unwrap().into_body();
            assert!(next_frame(&mut body).await.starts_with("event: reload"));
        }

        #[tokio::test]
        async fn ui_posts_need_the_same_origin() {
            let (app, queen) = app();
            let (c, _rx) =
                queen
                    .confirmations()
                    .request(1, "touch", &serde_json::json!({}), "because");
            for uri in [
                "/ui/messages".to_string(),
                "/ui/cancel".to_string(),
                format!("/ui/confirmations/{}/approve", c.id),
                "/ui/bees/ada/messages".to_string(),
                "/ui/bees/ada/cancel".to_string(),
            ] {
                let req = post(&uri)
                    .header("sec-fetch-site", "cross-site")
                    .body(Body::from("text=hi"))
                    .unwrap();
                assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN, "{uri}");
            }
            assert_eq!(queen.status().turn, None);
            assert_eq!(queen.confirmations().pending().len(), 1);
        }

        #[tokio::test]
        async fn the_stream_sends_rendered_updates() {
            let (app, queen) = app();
            queen.events().push(QueenEvent::UserMessage {
                turn: 1,
                text: "Hi".into(),
            });
            queen.events().push(QueenEvent::Text {
                turn: 1,
                text: "Hello".into(),
            });
            let res = app
                .clone()
                .oneshot(get("/ui/stream?since=2"))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert!(res.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream"));
            let mut body = res.into_body();

            // More reply text updates the existing bubble out of band.
            queen.events().push(QueenEvent::Text {
                turn: 1,
                text: " <there>".into(),
            });
            let frame = next_frame(&mut body).await;
            assert!(frame.starts_with("event: feed\n"), "{frame}");
            assert!(frame.contains(&format!("id: {}-3\n", queen.events().epoch())));
            assert!(frame.contains("hx-swap-oob=\"innerHTML:#f-r2\""));
            assert!(frame.contains("Hello &lt;there&gt;"));

            // A new entry is appended; the turn's end updates the composer status.
            queen.events().push(QueenEvent::ToolCall {
                turn: Some(1),
                id: "1".into(),
                title: "melissa__bee_info".into(),
                status: "running".into(),
            });
            let frame = next_frame(&mut body).await;
            assert!(frame.contains("<li class=\"entry\" id=\"f-t1\">"));
            assert!(frame.contains("Checking this device"));
            queen.events().push(QueenEvent::TurnFinished {
                turn: 1,
                stop_reason: "end_turn".into(),
            });
            let frame = next_frame(&mut body).await;
            assert!(frame.contains("innerHTML:#composer-status"));
            assert!(frame.contains("Everything is logged."));

            // A reconnect (Last-Event-ID) from the future asks the page to reload.
            let req = axum::http::Request::get("/ui/stream")
                .header("last-event-id", "999")
                .body(Body::empty())
                .unwrap();
            let mut body = app.clone().oneshot(req).await.unwrap().into_body();
            assert!(next_frame(&mut body).await.starts_with("event: reload"));
        }

        #[tokio::test]
        async fn the_approvals_stream_only_sends_approvals() {
            let (app, queen) = app();
            let res = app
                .clone()
                .oneshot(get("/ui/stream?since=0&view=approvals"))
                .await
                .unwrap();
            let mut body = res.into_body();
            queen.events().push(QueenEvent::UserMessage {
                turn: 1,
                text: "Hi".into(),
            });
            let (c, _rx) = queen.confirmations().request(
                1,
                "install_app",
                &serde_json::json!({"app": "immich"}),
                "because",
            );
            queen.events().push(QueenEvent::ConfirmationNeeded {
                id: c.id,
                task: 1,
                tool: "install_app".into(),
                reason: "because".into(),
            });
            let frame = next_frame(&mut body).await;
            assert!(!frame.contains("Hi"), "{frame}");
            assert!(frame.contains("Install Immich?"));
            assert!(frame.contains("hx-swap-oob=\"true\""), "waiting count");
        }

        #[tokio::test]
        async fn backends_are_listed_switched_between_turns_and_refused_when_unusable() {
            let (app, queen) = app();
            let (status, body) = send_json(&app, get("/api/queen/backends")).await;
            assert_eq!(status, StatusCode::OK);
            let list = body["backends"].as_array().unwrap();
            let names: Vec<&str> = list.iter().map(|b| b["name"].as_str().unwrap()).collect();
            assert_eq!(names, ["stuck", "echo-b", "gone"]);
            assert_eq!(list[0]["current"], true);
            assert_eq!(list[1]["label"], "echo-b");
            assert_eq!(list[2]["available"], false);
            assert_eq!(list[2]["reason"], "not_installed");
            assert!(list[1].get("models").is_none(), "no models invented");

            // Refused: not installed, unknown, a model nobody reported, cross-site.
            let (status, body) =
                send_json(&app, json_post("/api/queen/backend", r#"{"name":"gone"}"#)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(body["error"].as_str().unwrap().contains("not installed"));
            for bad in [r#"{"name":"nope"}"#, r#"{"name":"echo-b","model":"x"}"#] {
                let (status, _) = send_json(&app, json_post("/api/queen/backend", bad)).await;
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
            }
            let mut cross = json_post("/api/queen/backend", r#"{"name":"echo-b"}"#);
            cross
                .headers_mut()
                .insert(header::ORIGIN, "http://evil.example.com".parse().unwrap());
            assert_eq!(send(&app, cross).await.0, StatusCode::FORBIDDEN);
            assert_eq!(queen.backend_choice(), None);

            // A turn is running (its session never opens): the switch waits for it.
            let (status, _) = send_json(
                &app,
                json_post("/api/queen/messages", r#"{"text":"hello"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            let (status, body) = send_json(
                &app,
                json_post("/api/queen/backend", r#"{"name":"echo-b"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["changed"], true);
            assert_eq!(body["after_turn"], true);
            let (_, body) = send_json(&app, get("/api/queen/backends")).await;
            assert_eq!(body["backends"][1]["current"], true);
            assert!(queen
                .events()
                .since(0, 100)
                .events
                .iter()
                .any(|r| matches!(&r.event, QueenEvent::BackendSwitched { backend, .. } if backend == "echo-b")));

            // The chat shows it quietly, and the menu has the new current agent.
            let (_, _, page) = send(&app, get("/")).await;
            assert!(page.contains("Switched to echo-b, picking up where we left off"));
            assert!(
                page.contains("<option value=\"echo-b\" selected>"),
                "{page}"
            );
            assert!(page.contains("gone (not set up)"));
            assert!(
                page.contains("gone isn&#39;t set up on this device.")
                    || page.contains("gone isn't set up on this device."),
                "{page}"
            );

            // The UI form: htmx gets the status line back; a refusal says why.
            let ui_post = |choice: &str, htmx: bool| {
                let mut b = post("/ui/backend");
                if htmx {
                    b = b.header("hx-request", "true");
                }
                b.body(Body::from(format!("text=&choice={choice}")))
                    .unwrap()
            };
            let (status, _, body) = send(&app, ui_post("stuck", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body.contains("Runs on <details class=\"agent-menu\">"),
                "{body}"
            );
            assert!(!body.contains("agent-error"));
            let (status, _, body) = send(&app, ui_post("gone", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body.contains("gone isn&#39;t ready on this device yet.")
                    || body.contains("gone isn't ready on this device yet."),
                "{body}"
            );
            let (status, headers, _) = send(&app, ui_post("echo-b", false)).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/");

            // A bee: any session backend; saved in the registry.
            queen
                .bees()
                .registry()
                .create("Ada", "stuck", None, "the user")
                .unwrap();
            let (status, body) = send_json(&app, get("/api/bees/ada/backends")).await;
            assert_eq!(status, StatusCode::OK);
            let names: Vec<&str> = body["backends"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, ["stuck", "echo-b"]);
            let (status, body) = send_json(
                &app,
                json_post("/api/bees/Ada/backend", r#"{"name":"echo-b"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["changed"], true);
            let (_, bee) = send_json(&app, get("/api/bees/Ada")).await;
            assert_eq!(bee["backend"], "echo-b");
            let (_, _, page) = send(&app, get("/bees/Ada")).await;
            assert!(page.contains("action=\"/ui/bees/"), "{page}");

            // Bees: unknown ones are 404.
            assert_eq!(
                send(&app, get("/api/bees/nobody/backends")).await.0,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                send(
                    &app,
                    json_post("/api/bees/nobody/backend", r#"{"name":"echo-b"}"#)
                )
                .await
                .0,
                StatusCode::NOT_FOUND
            );
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
