#![forbid(unsafe_code)]
//! Web UI: server-rendered with maud, htmx and CSS embedded in the binary (no CDN).
//!
//! UI rule: only components defined in `components` may be rendered. Agents never
//! generate markup; they select components and fill manifests.

mod agents;
mod api;
mod apps;
pub mod components;
mod feed;
mod files;
mod markdown;
mod pages;
mod settings;
mod state;
mod ui;

use axum::{
    extract::{Form, FromRef, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use maud::Markup;
use melissa_agent::{
    lead::Lead,
    mcp::{McpServer, UnknownSession},
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
/// `blob:` images: previews of files picked in the composer (made by the page itself).
const CSP: &str =
    "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; \
                   connect-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'";

/// The task UI, without agent tools or the lead agent (`/` goes to the task list).
pub fn router(board: TaskBoard) -> Router {
    ui_routes(board, None)
        .route("/", get(|| async { Redirect::to("/tasks") }))
        .layer(middleware::from_fn(security))
}

/// The task UI plus the MCP endpoint (`POST /mcp/{token}`) agent CLIs use to call tools.
pub fn router_with_mcp(board: TaskBoard, mcp: McpServer) -> Router {
    ui_routes(board, None)
        .route("/", get(|| async { Redirect::to("/tasks") }))
        .merge(
            Router::new()
                .route("/mcp/{token}", post(mcp_post).get(mcp_get))
                .with_state(mcp),
        )
        .layer(middleware::from_fn(security))
}

/// The full UI: the chat with the lead agent as home (see [`ui`]), the task pages, the MCP
/// endpoint and the lead agent's JSON API (`/api/...`, see [`api`]).
pub fn router_with_lead(board: TaskBoard, mcp: McpServer, lead: Lead) -> Router {
    router_with_settings(board, mcp, lead, Settings::detached())
}

/// [`router_with_lead`] plus the Settings pages and API (`/settings/...`,
/// `/api/settings/...`) backed by the lead agent's provider settings and update helper.
pub fn router_with_settings(
    board: TaskBoard,
    mcp: McpServer,
    lead: Lead,
    settings: Settings,
) -> Router {
    let ui = ui::Ui::new(
        lead.clone(),
        state::ClusterCache::new(mcp.clone(), Some(lead.clone())),
        settings,
    )
    .with_board(board.clone());
    ui_routes(board, Some(ui.clone()))
        .merge(ui::routes(ui.clone()))
        .merge(apps::routes(ui.clone()))
        .merge(settings::api_routes(ui))
        .merge(api::tool_routes(mcp.clone()))
        .merge(
            Router::new()
                .route("/mcp/{token}", post(mcp_post).get(mcp_get))
                .with_state(mcp),
        )
        .merge(api::routes(lead))
        .layer(middleware::from_fn(security))
}

/// State of the task pages: the board, and the lead agent's UI (for the rail) when there is one.
#[derive(Clone)]
struct Tasks {
    board: TaskBoard,
    ui: Option<ui::Ui>,
}

impl FromRef<Tasks> for TaskBoard {
    fn from_ref(t: &Tasks) -> Self {
        t.board.clone()
    }
}

fn ui_routes(board: TaskBoard, ui: Option<ui::Ui>) -> Router {
    let routes = Router::new();
    // With a lead agent the task list lives in Settings → Developer; GET /tasks redirects there.
    let routes = if ui.is_some() {
        routes
            .route(settings::pages::DEVELOPER_URL, get(dashboard))
            .route(
                "/tasks",
                get(|| async { Redirect::to(settings::pages::DEVELOPER_URL) }).post(create_task),
            )
    } else {
        routes.route("/tasks", get(dashboard).post(create_task))
    };
    routes
        .route("/healthz", get(|| async { "ok" }))
        .route("/tasks/fragment", get(tasks_fragment))
        .route("/tasks/{id}", get(task_page))
        .route("/tasks/{id}/fragment", get(task_fragment))
        .route("/tasks/{id}/cancel", post(cancel_task))
        .route("/assets/{*path}", get(asset))
        .fallback(|| async { (StatusCode::NOT_FOUND, pages::not_found()) })
        .with_state(Tasks { board, ui })
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

async fn dashboard(State(t): State<Tasks>) -> Markup {
    let (backends, tasks) = (t.board.backends(), t.board.list());
    match &t.ui {
        Some(ui) => {
            let steps = ui.settings.prefs().show_steps;
            ui.framed(|f| pages::dashboard(Some(f), &backends, &tasks, steps))
                .await
        }
        None => pages::dashboard(None, &backends, &tasks, false),
    }
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
        return match &result {
            // Open the new task, as a plain form post does.
            Ok(id) => [("hx-redirect", format!("/tasks/{id}"))].into_response(),
            Err(e) => {
                let message = format!("Couldn't start the task: {e}.");
                pages::tasks_region(&board.list(), Some((&message, Tone::Error))).into_response()
            }
        };
    }
    match result {
        Ok(id) => Redirect::to(&format!("/tasks/{id}")).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            pages::error_page("Couldn't start the task", &e.to_string()),
        )
            .into_response(),
    }
}

async fn task_page(State(t): State<Tasks>, Path(id): Path<TaskId>) -> Response {
    let Some(task) = t.board.get(id) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    match &t.ui {
        Some(ui) => ui
            .framed(|f| pages::task_page(Some(f), &task))
            .await
            .into_response(),
        None => pages::task_page(None, &task).into_response(),
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
/// (the lead agent only binds to loopback for now).
async fn security(req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe && !same_origin(req.headers()) {
        return (StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    // A handler may set a stricter policy (attachments: `sandbox`).
    h.entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(CSP));
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
        // Without a lead agent, home is the task list.
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
        assert!(body.contains("Agent tasks"));
        // The built-in echo is a test partner: not listed for people.
        assert!(!body.contains("value=\"echo\"") && !body.contains("built-in, no LLM"));
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
    async fn htmx_dispatch_opens_the_task() {
        let (app, board) = app();
        let body = "backend=echo&prompt=hello+cluster";
        let req = post("/tasks")
            .header("hx-request", "true")
            .header("sec-fetch-site", "same-origin")
            .body(Body::from(body))
            .unwrap();
        let (status, headers, html) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.is_empty());
        assert_eq!(headers["hx-redirect"], "/tasks/1");
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
        assert!(body.contains("device_info"));
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

    mod lead_api {
        use melissa_agent::{
            lead::{
                agents::{AgentLimits, AgentPool},
                events::{EventLog, LeadEvent},
                registry::AgentRegistry,
                transcript::Transcript,
                LeadSettings,
            },
            session::{OpenSession, OpenedSession, SessionBackend},
            tools::{Tool, ToolRegistry},
            AgentError, BackendInfo, BoxFuture, McpLink,
        };
        use serde_json::Value;

        use super::*;

        /// A Lead agent backend whose session never finishes opening.
        /// Melissa's first backend (provider claude in the Settings fixture); `signed_out`:
        /// it reports no login, like a Claude CLI without credentials.
        struct Stuck {
            signed_out: bool,
        }
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
            fn not_ready(&self) -> Option<melissa_agent::session::NotReady> {
                self.signed_out
                    .then_some(melissa_agent::session::NotReady::NotLoggedIn)
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

        /// A read-only tool with a fixed answer (stands in for the cluster's tools).
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

        fn cluster_tools() -> Vec<Arc<dyn Tool>> {
            vec![
                Arc::new(Fixed(
                    "list_apps",
                    serde_json::json!({
                        "apps": [
                            {"app": "nextcloud", "phase": "ready", "url": "http://nextcloud.example.com"},
                            {"app": "immich", "phase": "starting"},
                        ],
                    }),
                )),
                Arc::new(Fixed(
                    "app_status",
                    serde_json::json!({
                        "k3s_reachable": true, "app": "nextcloud", "phase": "ready",
                        "agent": "d-0a1b2c3d.a1", "agent_name": "Ada", "device": "d-0a1b2c3d",
                        "port": 30000, "url": "http://203.0.113.10:30000",
                        "version": "34.0.6", "image": "docker.io/library/nextcloud:34.0.6",
                        "installed": true, "replicas": 1, "ready_replicas": 1,
                        "pods": [{"name": "nc-1", "restarts": 2, "age_secs": 120}],
                    }),
                )),
                Arc::new(Fixed(
                    "app_logs",
                    serde_json::json!({
                        "app": "nextcloud", "pod": "nc-1", "container": "nextcloud",
                        "containers": ["nextcloud", "cron"], "lines": 2, "truncated": false,
                        "log": "<script>alert(1)</script>\n**bold** <img src=x onerror=alert(2)>",
                    }),
                )),
                Arc::new(Fixed(
                    "cluster_health",
                    serde_json::json!({"nodes": [{"name": "device-1", "ready": true, "allocatable": {"cpu": "4"}}]}),
                )),
                Arc::new(Fixed(
                    "device_info",
                    serde_json::json!({"os": "Fedora Linux 45", "mem_total_kib": 8_388_608, "mem_available_kib": 4_194_304,
                        "disks": [{"path": "/", "total_bytes": 10_000_000_000u64, "available_bytes": 5_000_000_000u64, "used_percent": 50},
                                  {"path": "/var", "total_bytes": 20_000_000_000u64, "available_bytes": 1_800_000_000u64, "used_percent": 91}]}),
                )),
            ]
        }

        fn app_with(page_settings: Option<Settings>) -> (Router, Lead) {
            app_full(page_settings, None, false)
        }

        fn app_full(
            page_settings: Option<Settings>,
            workspaces: Option<Arc<dyn melissa_agent::tools::WorkspaceStore>>,
            signed_out: bool,
        ) -> (Router, Lead) {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "melissa-web-lead-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let events = EventLog::new(Some(Transcript::new(
                dir.join("lead/transcript.jsonl"),
                1 << 20,
            )));
            let registry = AgentRegistry::open(&dir.join("agents"), "d-0a1b2c3d").unwrap();
            let pool = AgentPool::new(AgentLimits::default(), events.clone(), registry, workspaces);
            let mut tools: Vec<Arc<dyn Tool>> = pool.tools();
            tools.push(Arc::new(Touch));
            tools.extend(cluster_tools());
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
            let settings = LeadSettings {
                // `gone` is not configured: shown as not installed.
                backends: vec!["stuck".into(), "echo-b".into(), "gone".into()],
                system_prompt: "test".into(),
                agent_prompt: "test".into(),
                agent_backend: Some("stuck".into()),
                state_dir: dir.join("lead"),
                workdir: dir.join("work/lead"),
                tools: None,
                device: "d-0a1b2c3d".into(),
                files: melissa_agent::files::FileBoxes::open(dir.join("files")),
            };
            let lead = Lead::new(
                settings,
                vec![
                    Arc::new(Stuck { signed_out }),
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
                Some(s) => router_with_settings(board, mcp, lead.clone(), s),
                None => router_with_lead(board, mcp, lead.clone()),
            };
            (router, lead)
        }

        fn app() -> (Router, Lead) {
            app_with(None)
        }

        // --- Settings ------------------------------------------------------------------------

        use melissa_settings::{
            install::{InstallWay, Installer},
            providers::{BackendFacts, ProviderStore},
            updates::{HostKind, Release, UpdateEnv},
        };

        struct SettingsFixture {
            dir: std::path::PathBuf,
            voice: melissa_voice::Voice,
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
                Self {
                    dir,
                    voice: melissa_voice::Voice::unavailable(),
                }
            }

            fn settings(&self, kind: HostKind, helper: bool) -> Settings {
                self.settings_with(kind, helper, InstallWay::Manual)
            }

            /// A fake `systemctl`: `show` prints `unit-state.<unit>` (else `unit-state`);
            /// `start` logs the call and marks that unit running.
            fn installer(&self, way: InstallWay) -> Installer {
                use std::os::unix::fs::PermissionsExt;
                let ctl = self.dir.join("bin/systemctl");
                let state = self.dir.join("unit-state");
                if !state.exists() {
                    std::fs::write(&state, "LoadState=loaded\nActiveState=inactive\n").unwrap();
                }
                std::fs::write(
                    &ctl,
                    format!(
                        "#!/bin/sh\necho \"$@\" >> '{calls}'\n\
                         f='{state}'.\"$3\"; [ -f \"$f\" ] || f='{state}'\n\
                         if [ \"$1\" = show ]; then cat \"$f\"; exit 0; fi\n\
                         printf 'LoadState=loaded\\nActiveState=activating\\n' > '{state}'.\"$3\"\n",
                        calls = self.dir.join("systemctl-calls").display(),
                        state = state.display(),
                    ),
                )
                .unwrap();
                std::fs::set_permissions(&ctl, std::fs::Permissions::from_mode(0o755)).unwrap();
                std::fs::create_dir_all(self.dir.join("install-logs")).unwrap();
                Installer::new(way, self.dir.join("install-logs"), ctl)
            }

            fn calls(&self) -> String {
                std::fs::read_to_string(self.dir.join("systemctl-calls")).unwrap_or_default()
            }

            fn settings_with(&self, kind: HostKind, helper: bool, way: InstallWay) -> Settings {
                self.settings_with_claude(kind, helper, way, "/nonexistent/claude")
            }

            fn settings_with_claude(
                &self,
                kind: HostKind,
                helper: bool,
                way: InstallWay,
                claude: &str,
            ) -> Settings {
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
                    let f = root.join("usr/share/polkit-1/rules.d/50-melissa.rules");
                    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                    std::fs::write(f, "").unwrap();
                    std::fs::create_dir_all(self.dir.join("update")).unwrap();
                    std::fs::create_dir_all(self.dir.join("update-request")).unwrap();
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
                    prefs: melissa_settings::prefs::PrefsStore::open(
                        self.dir.join("config/ui.toml"),
                    ),
                    backends: vec![
                        // The lead agent's first backend: Melissa runs on Claude.
                        fact("stuck", "claude", true, Some(claude.into())),
                        fact(
                            "grok-acp",
                            "grok",
                            true,
                            Some(self.dir.join("bin/grok").display().to_string()),
                        ),
                        fact("echo-b", "echo", true, None),
                        fact("echo", "echo", false, None),
                    ],
                    agent_default: Some("stuck".into()),
                    check: Some(check),
                    audit: Some(melissa_agent::AuditLog::new(self.dir.join("audit.jsonl"))),
                    updates: UpdateEnv {
                        root,
                        kind,
                        state_dir: self.dir.join("update"),
                        request_dir: self.dir.join("update-request"),
                    },
                    release: Release {
                        version: "0.3.1".into(),
                        revision: Some("abc1234".into()),
                        runtime: Some("container".into()),
                        source: "image",
                    },
                    home: Some("/var/lib/melissa/home".into()),
                    installer: self.installer(way),
                    voice: self.voice.clone(),
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
        }

        // --- Voice input ------------------------------------------------------------------

        mod voice_fakes {
            use axum::body::Bytes;
            use futures::future::BoxFuture;
            use melissa_voice::{
                engine::Engine,
                fetch::{Body, Fetch},
                model::{ModelSpec, ModelStore},
                wav::Clip,
                Voice, VoiceError,
            };

            /// Serves the same bytes for any URL (no network).
            pub struct ServeBytes(pub Vec<u8>);
            struct Once(Option<Bytes>);

            impl Fetch for ServeBytes {
                fn get<'a>(
                    &'a self,
                    _: &'a str,
                ) -> BoxFuture<'a, Result<Box<dyn Body>, VoiceError>> {
                    Box::pin(async move {
                        Ok(Box::new(Once(Some(Bytes::from(self.0.clone())))) as Box<dyn Body>)
                    })
                }
            }

            impl Body for Once {
                fn len_hint(&self) -> Option<u64> {
                    None
                }
                fn next(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, VoiceError>> {
                    let piece = self.0.take();
                    Box::pin(async move { Ok(piece) })
                }
            }

            /// Says how long the clip was.
            pub struct Parrot;

            impl Engine for Parrot {
                fn transcribe<'a>(
                    &'a self,
                    clip: &'a Clip,
                ) -> BoxFuture<'a, Result<String, VoiceError>> {
                    Box::pin(async move { Ok(format!("heard {} ms", clip.millis())) })
                }
            }

            /// Voice input with the model pinned to `pinned`, the fake server serving `serve`.
            pub fn voice(dir: &std::path::Path, serve: &[u8], pinned: &[u8]) -> Voice {
                let spec = ModelSpec {
                    file: "model.bin",
                    url: "https://example.com/model.bin",
                    size: pinned.len() as u64,
                    sha256: Box::leak(melissa_settings::sha256_hex(pinned).into_boxed_str()),
                };
                Voice::from_parts(
                    ModelStore::open(dir, spec),
                    Some(std::sync::Arc::new(ServeBytes(serve.to_vec()))),
                    Some(std::sync::Arc::new(Parrot)),
                )
            }

            pub async fn settled(v: &Voice) -> melissa_voice::model::ModelState {
                for _ in 0..500 {
                    let s = v.state();
                    if !matches!(s, melissa_voice::model::ModelState::Downloading { .. }) {
                        return s;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                v.state()
            }
        }

        fn recording(content_type: &str, body: Vec<u8>) -> axum::http::Request<Body> {
            axum::http::Request::post("/ui/voice/transcribe")
                .header(header::CONTENT_TYPE, content_type)
                .header(header::HOST, "127.0.0.1:8080")
                .body(Body::from(body))
                .unwrap()
        }

        fn seconds_of_silence(s: usize) -> Vec<u8> {
            melissa_voice::wav::Clip::from_samples(vec![0; 16_000 * s]).to_wav()
        }

        #[tokio::test]
        async fn voice_input_downloads_the_model_and_puts_a_mic_in_the_composer() {
            let mut fx = SettingsFixture::new("voice");
            let models = fx.dir.join("data/models");
            fx.voice = voice_fakes::voice(&models, b"model", b"model");
            let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
            let (status, _, html) = send(&app, get("/settings/voice")).await;
            assert_eq!(status, StatusCode::OK);
            for want in [
                "href=\"/settings/voice\"",
                "Voice input",
                "role=\"switch\"",
                "Turning it on downloads a 1 MB speech model once.",
            ] {
                assert!(html.contains(want), "missing {want}: {html}");
            }
            let (_, _, home) = send(&app, get("/")).await;
            assert!(!home.contains("id=\"voice\""));

            let (status, _, _) = send(&app, form("/ui/settings/voice", "enabled=on", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                voice_fakes::settled(&fx.voice).await,
                melissa_voice::model::ModelState::Ready
            );
            assert!(models.join("model.bin").is_file());
            let saved = std::fs::read_to_string(fx.dir.join("config/ui.toml")).unwrap();
            assert!(saved.contains("voice_input = true"), "{saved}");
            let audit = fx.audit();
            for want in [
                "voice_input: on",
                "speech model model.bin download started from https://example.com/model.bin",
                "speech model model.bin downloaded, sha256 verified",
            ] {
                assert!(audit.contains(want), "missing {want}: {audit}");
            }
            let (_, _, html) = send(&app, get("/ui/settings/voice/poll")).await;
            assert!(html.contains("Ready."), "{html}");
            assert!(!html.contains("hx-trigger=\"every"));

            // The composer has the mic now, pointing at the transcribe route.
            let (_, _, home) = send(&app, get("/")).await;
            assert!(home.contains("id=\"voice\""), "{home}");
            assert!(home.contains("data-url=\"/ui/voice/transcribe\""));
            let (status, v) = send_json(&app, recording("audio/wav", seconds_of_silence(1))).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["text"], "heard 1000 ms");

            // Off: no mic, no transcribing, the model can be deleted.
            let (_, _, html) = send(&app, form("/ui/settings/voice", "", true)).await;
            assert!(html.contains("Delete it"), "{html}");
            let (_, _, home) = send(&app, get("/")).await;
            assert!(!home.contains("id=\"voice\""));
            let (status, _) = send_json(&app, recording("audio/wav", seconds_of_silence(1))).await;
            assert_eq!(status, StatusCode::CONFLICT);
            let (_, _, html) = send(&app, form("/ui/settings/voice/delete", "", true)).await;
            assert!(html.contains("Turning it on downloads"), "{html}");
            assert!(!models.join("model.bin").exists());
            let audit = fx.audit();
            assert!(audit.contains("voice_input: off"));
            assert!(audit.contains("speech model model.bin deleted"));
        }

        #[tokio::test]
        async fn a_model_that_does_not_match_its_checksum_is_refused() {
            let mut fx = SettingsFixture::new("voice-bad");
            let models = fx.dir.join("data/models");
            fx.voice = voice_fakes::voice(&models, b"evil!", b"model");
            let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
            send(&app, form("/ui/settings/voice", "enabled=on", true)).await;
            assert!(matches!(
                voice_fakes::settled(&fx.voice).await,
                melissa_voice::model::ModelState::Failed(_)
            ));
            assert!(!models.join("model.bin").exists());
            assert!(!models.join("model.bin.part").exists());
            let (_, _, html) = send(&app, get("/ui/settings/voice/poll")).await;
            assert!(html.contains("didn"), "{html}");
            assert!(html.contains("Try again"));
            assert!(fx.audit().contains("download failed"));
            let (_, _, home) = send(&app, get("/")).await;
            assert!(!home.contains("id=\"voice\""));
            let (status, _) = send_json(&app, recording("audio/wav", seconds_of_silence(1))).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        }

        #[tokio::test]
        async fn the_transcribe_route_checks_what_it_gets() {
            let mut fx = SettingsFixture::new("voice-route");
            let models = fx.dir.join("data/models");
            std::fs::create_dir_all(&models).unwrap();
            // Already downloaded: no download starts.
            std::fs::write(models.join("model.bin"), b"model").unwrap();
            fx.voice = voice_fakes::voice(&models, b"never fetched", b"model");
            let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
            send(&app, form("/ui/settings/voice", "enabled=on", true)).await;
            assert!(fx.voice.ready());
            assert!(!fx.audit().contains("download started"));

            let (status, v) = send_json(&app, recording("text/plain", b"hi".to_vec())).await;
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
            assert_eq!(v["error"], "Send a WAV recording.");
            let (status, _) = send_json(&app, recording("audio/webm", seconds_of_silence(1))).await;
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
            let (status, _, _) =
                send(&app, recording("audio/wav", vec![0; 13 * 1024 * 1024])).await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
            let (status, v) =
                send_json(&app, recording("audio/wav", b"OggS not a wav".to_vec())).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(v["error"], "That isn't a WAV recording.");
            let (status, v) =
                send_json(&app, recording("audio/wav", seconds_of_silence(121))).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(
                v["error"].as_str().unwrap().contains("up to 120 seconds"),
                "{v}"
            );
            let (status, v) = send_json(
                &app,
                recording("audio/wav", Vec::from(&seconds_of_silence(1)[..44])),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(v["error"], "Nothing was recorded.");
            // Another site can't use the box's microphone route.
            let mut cross = recording("audio/wav", seconds_of_silence(1));
            cross
                .headers_mut()
                .insert("sec-fetch-site", "cross-site".parse().unwrap());
            let (status, _, _) = send(&app, cross).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            let (status, v) = send_json(
                &app,
                recording("audio/wav; codecs=1", seconds_of_silence(120)),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(v["text"], "heard 120000 ms");
        }

        #[tokio::test]
        async fn voice_input_is_locked_without_a_speech_engine() {
            let fx = SettingsFixture::new("voice-none");
            let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
            let (_, _, html) = send(&app, get("/settings/voice")).await;
            assert!(html.contains("Not available on this install"), "{html}");
            assert!(html.contains("disabled"));
            let (_, _, html) = send(&app, form("/ui/settings/voice", "enabled=on", true)).await;
            assert!(html.contains("isn"), "{html}");
            let saved = std::fs::read_to_string(fx.dir.join("config/ui.toml")).unwrap_or_default();
            assert!(!saved.contains("voice_input = true"));
        }

        #[tokio::test]
        async fn the_developer_toggle_shows_thinking_and_timings_in_the_chat() {
            let fx = SettingsFixture::new("steps");
            let (app, lead) = app_with(Some(fx.settings(HostKind::Other, false)));
            for e in [
                LeadEvent::UserMessage {
                    turn: 1,
                    text: "Hi".into(),
                    attachments: vec![],
                    scheduled: None,
                },
                LeadEvent::Thought {
                    turn: 1,
                    text: String::new(),
                    ms: None,
                },
                LeadEvent::Thought {
                    turn: 1,
                    text: "Maybe <b>this</b>".into(),
                    ms: Some(2_500),
                },
                LeadEvent::ToolCall {
                    turn: Some(1),
                    id: "1".into(),
                    title: "melissa__device_info".into(),
                    status: "done".into(),
                    ms: Some(1_200),
                },
                // Stopped while a tool call ran: the agent never ends it.
                LeadEvent::ToolCall {
                    turn: Some(1),
                    id: "2".into(),
                    title: "some_tool".into(),
                    status: "running".into(),
                    ms: None,
                },
                LeadEvent::TurnFinished {
                    turn: 1,
                    stop_reason: "cancelled".into(),
                },
            ] {
                lead.events().push(e);
            }
            // Off by default: the chat looks as it always did, and nothing spins after the stop.
            let (_, _, html) = send(&app, get("/")).await;
            assert!(html.contains("Stopped.") && html.contains("Using a tool"));
            assert!(!html.contains("icon spin"), "{html}");
            assert!(!html.contains("Thought for") && !html.contains("1.2s"));
            let (_, _, html) = send(&app, get("/settings/developer")).await;
            assert!(html.contains("Show thinking and timings"));
            assert!(html.contains("/ui/settings/developer/steps"));

            let (status, _, region) = send(
                &app,
                htmx_post("/ui/settings/developer/steps", "enabled=on"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(region.contains("id=\"chat-prefs\"") && region.contains("checked"));
            assert!(std::fs::read_to_string(fx.dir.join("config/ui.toml"))
                .unwrap()
                .contains("show_steps = true"));
            let (_, _, html) = send(&app, get("/")).await;
            assert!(html.contains("Thought for 2.5s"));
            assert!(html.contains("Checked this device · 1.2s"));
            // The thought is agent text: escaped, never markup.
            assert!(html.contains("&lt;b&gt;") && !html.contains("<b>this"));
            assert!(!html.contains("icon spin"));

            // Off again: a plain form post saves and goes back to the page.
            let (status, headers, _) = send(
                &app,
                post("/ui/settings/developer/steps")
                    .header("sec-fetch-site", "same-origin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/settings/developer");
            let (_, _, html) = send(&app, get("/")).await;
            assert!(!html.contains("Thought for"));
        }

        #[tokio::test]
        async fn nixos_updates_show_nix_commands_and_only_restart() {
            let fx = SettingsFixture::new("nixos");
            // helper=true also lays down stray helper mounts: a Nix install must still not
            // offer the container's update buttons.
            let settings = fx.settings(HostKind::Nix, true);
            std::fs::create_dir_all(fx.dir.join("root/etc")).unwrap();
            std::fs::write(fx.dir.join("root/etc/NIXOS"), "").unwrap();
            let (app, _) = app_with(Some(settings));
            let (status, _, html) = send(&app, get("/settings/updates")).await;
            assert_eq!(status, StatusCode::OK);
            for want in [
                "0.3.1",
                "NixOS configuration",
                "nix flake update",
                "sudo nixos-rebuild switch",
                "sudo nixos-rebuild switch --rollback",
            ] {
                assert!(html.contains(want), "missing {want}");
            }
            for unwanted in [
                "git pull",
                "podman",
                "container-build",
                "container image",
                "/ui/settings/updates/apply",
                "/ui/settings/updates/rollback",
            ] {
                assert!(!html.contains(unwanted), "unexpected {unwanted}");
            }
            let (_, _, json) = send(&app, get("/api/settings/updates")).await;
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["host"], "nix");
            assert_eq!(v["actions"], serde_json::json!(["restart"]));
        }

        #[tokio::test]
        async fn echo_is_never_listed() {
            let fx = SettingsFixture::new("echo-hidden");
            let (app, _) = app_with(Some(fx.settings(HostKind::Container, false)));
            let (_, _, html) = send(&app, get("/settings/providers")).await;
            assert!(
                !html.contains("Echo") && !html.contains("?p=echo"),
                "{html}"
            );
            let (_, v) = send_json(&app, get("/api/settings/providers")).await;
            let ids: Vec<&str> = v["providers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, ["claude", "grok"]);
            let (status, _, _) = send(&app, get("/settings/providers?p=echo")).await;
            assert_eq!(status, StatusCode::OK);
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/providers/echo", r#"{"enabled": false}"#),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn install_button_starts_the_fixed_unit_and_polls() {
            let fx = SettingsFixture::new("install");
            let (app, _) = app_with(Some(fx.settings_with(
                HostKind::Container,
                true,
                InstallWay::Button,
            )));
            let (_, _, html) = send(&app, get("/settings/providers?p=claude")).await;
            assert!(html.contains(">Install<"), "{html}");
            assert!(html.contains("/ui/settings/providers/claude/install"));
            assert!(
                html.contains("Anthropic's official installer")
                    || html.contains("Anthropic&#39;s official installer")
            );
            assert!(html.contains("Not installed"));
            // Less text: no internals in the default view, no host commands, no restart banner.
            for unwanted in [
                "melissa-setup",
                "podman exec",
                "Runs as",
                "one-shot task",
                "chat session",
                "Not known yet",
                "Restart Melissa now",
                "ANTHROPIC_API_KEY",
                "hx-trigger=\"every",
            ] {
                assert!(!html.contains(unwanted), "unexpected {unwanted}");
            }
            // Installed providers have no Install button.
            let (_, _, html) = send(&app, get("/settings/providers?p=grok")).await;
            assert!(!html.contains("/ui/settings/providers/grok/install"));

            let (status, _, html) = send(
                &app,
                form("/ui/settings/providers/claude/install", "", true),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Installing…"), "{html}");
            assert!(html.contains("hx-get=\"/ui/settings/providers/poll?p=claude\""));
            assert!(html.contains("every 2s"));
            assert_eq!(
                fx.calls()
                    .lines()
                    .filter(|l| l.starts_with("start"))
                    .collect::<Vec<_>>(),
                ["start --no-block melissa-agent-install@claude.service"]
            );
            assert!(fx.audit().contains("claude: install requested"));
            // A second request while it runs is refused, not started again.
            let (status, v) = send_json(
                &app,
                json_post("/api/settings/providers/claude/install", ""),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{v}");
            assert_eq!(fx.calls().matches("start --no-block").count(), 1);

            // It fails: one line, a retry, the log behind a disclosure; the outcome is audited.
            std::fs::write(
                fx.dir
                    .join("unit-state.melissa-agent-install@claude.service"),
                "LoadState=loaded\nActiveState=failed\nResult=exit-code\nExecMainStatus=1\n\
                 ExecMainExitTimestampMonotonic=9\n",
            )
            .unwrap();
            std::fs::write(
                fx.dir.join("install-logs/claude.log"),
                "melissa-setup: download failed: <https://claude.ai/install.sh>\n",
            )
            .unwrap();
            let (status, _, html) = send(&app, get("/ui/settings/providers/poll?p=claude")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("The install failed.") && html.contains(">Failed<"));
            assert!(html.contains("Try again") && !html.contains("every 2s"));
            assert!(html.contains("<summary>Log</summary>"));
            assert!(html.contains("&lt;https://claude.ai/install.sh&gt;"));
            let (_, v) = send_json(&app, get("/api/settings/providers/claude/install")).await;
            assert_eq!(v["install"]["state"], "failed");
            assert_eq!(v["install"]["way"], "button");
            let mut audited = false;
            for _ in 0..40 {
                if fx
                    .audit()
                    .contains("claude: install failed (exit status 1)")
                {
                    audited = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(audited, "the outcome is audited: {}", fx.audit());

            // Only configured providers from the fixed list; nothing else reaches systemd.
            for bad in [
                "opencode",
                "echo",
                "foo",
                "..",
                "claude.service",
                "grok%2F..%2Fx",
            ] {
                let (status, _) = send_json(
                    &app,
                    json_post(&format!("/api/settings/providers/{bad}/install"), ""),
                )
                .await;
                assert!(status.is_client_error(), "{bad}: {status}");
            }
            // Installed already.
            let (status, _) =
                send_json(&app, json_post("/api/settings/providers/grok/install", "")).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(fx.calls().matches("start --no-block").count(), 1);
            assert!(fx.audit().contains("foo: install refused"));
            // Cross-origin posts never get there.
            let (status, _, _) = send(
                &app,
                axum::http::Request::post("/ui/settings/providers/claude/install")
                    .header(header::HOST, "127.0.0.1:8080")
                    .header(header::ORIGIN, "http://evil.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }

        /// A fake installed Claude (its login is checked by the lead agent, which has no such
        /// backend here, so the page treats it as installed and the sign-in as possible).
        fn fake_claude(fx: &SettingsFixture) -> Settings {
            use std::os::unix::fs::PermissionsExt;
            let claude = fx.dir.join("bin/claude");
            std::fs::write(&claude, "#!/bin/sh\necho '2.1.0 (Claude Code)'\n").unwrap();
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
            fx.settings_with_claude(
                HostKind::Container,
                true,
                InstallWay::Button,
                &claude.display().to_string(),
            )
        }

        #[tokio::test]
        async fn claude_signs_in_with_a_pasted_code_through_the_fifo() {
            use std::io::BufRead;
            let fx = SettingsFixture::new("claude-login");
            let (app, _) = app_full(Some(fake_claude(&fx)), None, true);
            // Not running yet: the Sign in button (no code field, no polling).
            let (_, _, html) = send(&app, get("/settings/providers?p=claude")).await;
            assert!(
                html.contains("/ui/settings/providers/claude/login"),
                "{html}"
            );
            assert!(html.contains(">Sign in<") && !html.contains("hx-trigger=\"every"));
            // The API key is the other way, folded away, without the variable name.
            assert!(html.contains("<summary>Use an API key instead</summary>"));
            assert!(!html.contains("ANTHROPIC_API_KEY") && !html.contains("setup-token"));

            // Start: the fixed unit, then the vendor's link and one code field.
            std::fs::write(
                fx.dir.join("install-logs/claude-login.log"),
                "Opening browser to sign in…\nIf the browser didn't open, visit: \
                 https://claude.com/cai/oauth/authorize?code=true&state=abc\n\
                 Paste code here if prompted > ",
            )
            .unwrap();
            let (status, _, html) =
                send(&app, form("/ui/settings/providers/claude/login", "", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                fx.calls()
                    .lines()
                    .filter(|l| l.starts_with("start"))
                    .collect::<Vec<_>>(),
                ["start --no-block melissa-agent-login@claude.service"]
            );
            assert!(html.contains(
                "href=\"https://claude.com/cai/oauth/authorize?code=true&amp;state=abc\""
            ));
            assert!(html.contains("rel=\"noopener noreferrer\""));
            assert!(html.contains("name=\"code\"") && html.contains("/login/code"));
            assert!(html.contains("/login/cancel"));
            // The page doesn't poll while the owner types the code.
            assert!(!html.contains("hx-trigger=\"every"), "{html}");

            // No reader yet (the unit isn't really there): a clean error, nothing hangs.
            let (status, _, html) = send(
                &app,
                form(
                    "/ui/settings/providers/claude/login/code",
                    "code=abc%23def",
                    false,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(html.contains("No sign-in is waiting"), "{html}");
            // Bad codes are refused before anything is written.
            let (status, _, _) = send(
                &app,
                form(
                    "/ui/settings/providers/claude/login/code",
                    "code=a+b",
                    false,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

            // The unit reads the FIFO (like melissa-setup: read-write, so opening never blocks).
            let fifo = fx
                .dir
                .join("install-logs")
                .join(melissa_settings::install::CODE_FIFO);
            assert!(std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success());
            let reader = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&fifo)
                .unwrap();
            let (status, _, html) = send(
                &app,
                form(
                    "/ui/settings/providers/claude/login/code",
                    "code=+the-code%23state+",
                    true,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{html}");
            assert!(html.contains("Signing in…") && html.contains("every 2s"));
            let mut lines = std::io::BufReader::new(reader).lines();
            assert_eq!(lines.next().unwrap().unwrap(), "the-code#state");
            // The code is never in the audit log or the page.
            assert!(!fx.audit().contains("the-code"));
            assert!(!html.contains("the-code"));
            assert!(fx.audit().contains("claude: sign-in code sent"));

            // A wrong code: the unit exits 3; the page says so and offers Sign in again.
            std::fs::write(
                fx.dir.join("unit-state.melissa-agent-login@claude.service"),
                "LoadState=loaded\nActiveState=failed\nResult=exit-code\nExecMainStatus=3\n\
                 ExecMainExitTimestampMonotonic=77\n",
            )
            .unwrap();
            let (_, _, html) = send(&app, get("/ui/settings/providers/poll?p=claude")).await;
            assert!(
                html.contains("That code didn") && html.contains(">Sign in<"),
                "{html}"
            );
            let (_, v) = send_json(&app, get("/api/settings/providers/claude/install")).await;
            assert_eq!(v["sign_in"]["step"], "sign_in");
            let mut audited = false;
            for _ in 0..40 {
                if fx
                    .audit()
                    .contains("claude: sign-in failed (code not accepted)")
                {
                    audited = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(audited, "{}", fx.audit());

            // Only Claude takes a code.
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/providers/grok/login/code", r#"{"code":"x"}"#),
            )
            .await;
            assert!(status.is_client_error());
        }

        #[tokio::test]
        async fn claude_sign_in_can_be_cancelled() {
            use std::io::BufRead;
            let fx = SettingsFixture::new("claude-cancel");
            let (app, _) = app_full(Some(fake_claude(&fx)), None, true);
            std::fs::write(
                fx.dir.join("unit-state.melissa-agent-login@claude.service"),
                "LoadState=loaded\nActiveState=activating\n",
            )
            .unwrap();
            let fifo = fx
                .dir
                .join("install-logs")
                .join(melissa_settings::install::CODE_FIFO);
            std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
            assert!(std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success());
            let reader = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&fifo)
                .unwrap();
            let (status, _, _) = send(
                &app,
                form("/ui/settings/providers/claude/login/cancel", "", true),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let mut lines = std::io::BufReader::new(reader).lines();
            assert_eq!(lines.next().unwrap().unwrap(), "", "an empty line cancels");
            assert!(fx.audit().contains("claude: sign-in cancel sent"));
        }

        #[tokio::test]
        async fn grok_signs_in_with_a_device_code() {
            let fx = SettingsFixture::new("login");
            let (app, _) = app_with(Some(fx.settings_with(
                HostKind::Container,
                true,
                InstallWay::Button,
            )));
            // Claude isn't installed here; OpenCode and unknown ids have no sign-in unit.
            let (status, _) =
                send_json(&app, json_post("/api/settings/providers/claude/login", "")).await;
            assert!(status.is_client_error());
            for bad in ["opencode", "echo", "foo"] {
                let (status, _) = send_json(
                    &app,
                    json_post(&format!("/api/settings/providers/{bad}/login"), ""),
                )
                .await;
                assert!(status.is_client_error(), "{bad}");
            }
            assert!(fx.calls().lines().all(|l| !l.starts_with("start")));
            std::fs::write(
                fx.dir.join("install-logs/grok-login.log"),
                "To sign in, open this URL in your browser:\n\
                 https://accounts.x.ai/oauth2/device?user_code=ABCD-1234\nABCD-1234\n",
            )
            .unwrap();
            let (status, _, html) =
                send(&app, form("/ui/settings/providers/grok/login", "", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                fx.calls()
                    .lines()
                    .filter(|l| l.starts_with("start"))
                    .collect::<Vec<_>>(),
                ["start --no-block melissa-agent-login@grok.service"]
            );
            assert!(html.contains("ABCD-1234") && html.contains("Open the sign-in page"));
            assert!(
                html.contains("every 2s"),
                "the page follows the device sign-in"
            );
            assert!(fx.audit().contains("grok: sign-in requested"));
            assert!(fx.audit().contains("claude: sign-in refused"));
        }

        #[tokio::test]
        async fn the_api_key_goes_under_the_fixed_name_and_is_never_echoed() {
            let fx = SettingsFixture::new("api-key");
            let (app, _) = app_full(Some(fake_claude(&fx)), None, true);
            let (status, _, html) = send(
                &app,
                form(
                    "/ui/settings/providers/claude/key",
                    "value=sk-test-secret-1",
                    true,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(!html.contains("sk-test-secret-1"));
            assert!(html.contains("API key set") && html.contains("Saved."));
            let file = std::fs::read_to_string(fx.dir.join("config/providers.toml")).unwrap();
            assert!(file.contains("ANTHROPIC_API_KEY"));
            assert!(!fx.audit().contains("sk-test-secret-1"));
            let (_, v) = send_json(&app, get("/api/settings/providers")).await;
            assert!(!v.to_string().contains("sk-test-secret-1"));
            assert_eq!(v["providers"][0]["api_key"]["set"], true);
            // Melissa applies it herself (the helper is there): no restart button.
            assert!(!html.contains("Restart Melissa now"));
            assert!(html.contains("Applying"), "{html}");
            // Remove it again; providers without a key variable take none.
            let (_, _, html) = send(
                &app,
                form("/ui/settings/providers/claude/key", "value=", true),
            )
            .await;
            assert!(html.contains("Removed."));
            let (status, _, _) = send(
                &app,
                form("/ui/settings/providers/opencode/key", "value=x", false),
            )
            .await;
            assert!(status.is_client_error());
        }

        #[tokio::test]
        async fn nix_installs_point_at_the_nix_configuration() {
            let fx = SettingsFixture::new("nix");
            let (app, _) = app_with(Some(fx.settings_with(
                HostKind::Other,
                true,
                InstallWay::Nix,
            )));
            let (_, _, html) = send(&app, get("/settings/providers?p=claude")).await;
            assert!(html.contains("This requires you add Claude Code via your Nix configuration."));
            assert!(!html.contains(">Install<") && !html.contains("melissa-setup"));
            let (status, v) = send_json(
                &app,
                json_post("/api/settings/providers/claude/install", ""),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(v["error"].as_str().unwrap().contains("Nix configuration"));
            assert!(fx.calls().is_empty(), "the unit is never used on Nix");
        }

        #[tokio::test]
        async fn without_the_unit_the_container_shows_no_host_command() {
            let fx = SettingsFixture::new("manual");
            let (app, _) = app_with(Some(fx.settings(HostKind::Container, false)));
            let (_, _, html) = send(&app, get("/settings/providers?p=claude")).await;
            assert!(!html.contains("melissa-setup") && !html.contains("podman"));
            assert!(!html.contains(">Install<"));
            let (_, v) = send_json(&app, get("/api/settings/providers")).await;
            assert_eq!(v["providers"][0]["install"]["way"], "manual");
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/providers/claude/install", ""),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
        }

        #[tokio::test]
        async fn secrets_are_write_only_everywhere() {
            let fx = SettingsFixture::new("secrets");
            let (app, _) = app_with(Some(fx.settings(HostKind::Container, false)));
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
            // Echo is never a provider people see or change.
            assert_eq!(status, StatusCode::NOT_FOUND);
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
                include_str!("../../settings/tests/fixtures/container-status-staged.json"),
            )
            .unwrap();
            std::fs::write(
                fx.dir.join("update/last.json"),
                r#"{"action":"check","ok":false,"at":1,"message":"<i>registry down</i>"}"#,
            )
            .unwrap();
            let (app, _) = app_with(Some(fx.settings(HostKind::Container, true)));
            let (_, _, html) = send(&app, get("/settings/updates")).await;
            for want in [
                "Downloaded, used at the next start",
                "0.1.1",
                "sha256:5b0d3c1e9f2a",
                "Previous (for a rollback)",
                "Ready to install",
                ">Update<",
                ">Roll back<",
                "&lt;i&gt;registry down",
                "On (daily)",
                "abc1234",
                "<summary>Details</summary>",
            ] {
                assert!(html.contains(want), "missing {want}");
            }
            // Apply without a yes: the confirmation card, nothing requested.
            let (status, _, html) = send(&app, form("/ui/settings/updates/apply", "", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                html.contains("Update and restart Melissa?") && html.contains("name=\"confirm\"")
            );
            let (_, _, html) = send(&app, form("/ui/settings/updates/rollback", "", true)).await;
            assert!(html.contains("Go back to the previous version?"));
            let (status, _) = send_json(&app, json_post("/api/settings/updates/apply", "{}")).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(fx.audit().is_empty(), "nothing was requested");
            assert_eq!(
                std::fs::read_dir(fx.dir.join("update-request"))
                    .unwrap()
                    .count(),
                0
            );
            let (status, body) = send_json(&app, get("/api/settings/updates")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["host"], "container");
            assert_eq!(body["release"]["version"], "0.3.1");
            assert_eq!(
                body["actions"],
                serde_json::json!(["check", "apply", "rollback", "restart"])
            );
            // With a yes: an empty request file for the host helper, and an audit line.
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/updates/apply", r#"{"confirm": true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            let req = fx.dir.join("update-request/apply");
            assert_eq!(std::fs::metadata(&req).unwrap().len(), 0);
            assert!(fx.audit().contains("apply requested"));
            // Without the helper: one short line, no host commands, and a 501 for actions.
            let fx2 = SettingsFixture::new("updates-nohelper");
            let (app, _) = app_with(Some(fx2.settings(HostKind::Container, false)));
            let (_, _, html) = send(&app, get("/settings/updates")).await;
            assert!(
                html.contains("Updating from here isn&#39;t set up")
                    || html.contains("Updating from here isn't set up")
            );
            for unwanted in [
                "melissa-host-update",
                "podman",
                "container-build",
                "/ui/settings/updates/apply",
            ] {
                assert!(!html.contains(unwanted), "unexpected {unwanted}");
            }
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/updates/apply", r#"{"confirm": true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
            // From source: no image to update.
            let fx3 = SettingsFixture::new("updates-source");
            let (app, _) = app_with(Some(fx3.settings(HostKind::Other, false)));
            let (_, _, html) = send(&app, get("/settings/updates")).await;
            assert!(html.contains("git pull"));
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
            let (status, body) =
                send_json(&app, json_post("/api/lead/messages", r#"{"text":"Hello"}"#)).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(body["turn"], 1);
            let since = body["since"].as_u64().unwrap();

            // One turn at a time.
            let (status, body) =
                send_json(&app, json_post("/api/lead/messages", r#"{"text":"Again"}"#)).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(body["error"].as_str().unwrap().contains("previous message"));
            let (status, _) =
                send_json(&app, json_post("/api/lead/messages", r#"{"text":"  "}"#)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            let (_, st) = send_json(&app, get("/api/lead/status")).await;
            assert_eq!(st["turn"], 1);

            let (status, body) = send_json(&app, json_post("/api/lead/cancel", "")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["cancelled"], true);

            let (status, page) =
                send_json(&app, get(&format!("/api/lead/events?since={since}&wait=1"))).await;
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
            let (_, page) = send_json(&app, get(&format!("/api/lead/events?since={next}"))).await;
            assert_eq!(page["events"], serde_json::json!([]));
            let (_, body) = send_json(&app, json_post("/api/lead/cancel", "")).await;
            assert_eq!(body["cancelled"], false);
            let (_, body) = send_json(&app, get("/api/agents")).await;
            assert_eq!(body["agents"], serde_json::json!([]));
        }

        #[tokio::test]
        async fn the_user_talks_to_a_agent_directly() {
            let (app, lead) = app();
            let ada = lead
                .agents()
                .registry()
                .create("Ada", "stuck", None, "the lead agent (task 1)")
                .unwrap();
            let (_, body) = send_json(&app, get("/api/agents")).await;
            assert_eq!(body["agents"][0]["name"], "Ada");
            assert_eq!(body["agents"][0]["status"], "idle");

            // By name (any case) or id; same shapes as the lead agent's chat.
            let (status, body) = send_json(
                &app,
                json_post("/api/agents/ada/messages", r#"{"text":"Hello Ada"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            assert_eq!(body["turn"], 1);
            let since = body["since"].as_u64().unwrap();
            let uri = format!("/api/agents/{}/messages", ada.id);
            let (status, _) = send_json(&app, json_post(&uri, r#"{"text":"Again"}"#)).await;
            assert_eq!(status, StatusCode::CONFLICT);
            let (_, st) = send_json(&app, get("/api/agents/Ada/status")).await;
            assert_eq!(st["turn"], 1);
            let (_, b) = send_json(&app, get("/api/agents/Ada")).await;
            assert_eq!(b["status"], "working");
            let (_, body) = send_json(&app, json_post("/api/agents/Ada/cancel", "")).await;
            assert_eq!(body["cancelled"], true);
            let (status, page) = send_json(
                &app,
                get(&format!("/api/agents/Ada/events?since={since}&wait=1")),
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
            // The lead agent's log did not get the agent's conversation.
            let (_, page) = send_json(&app, get("/api/lead/events?since=0")).await;
            assert_eq!(page["events"], serde_json::json!([]));

            // The user renames; names stay unique.
            let (status, body) = send_json(
                &app,
                json_post("/api/agents/Ada/rename", r#"{"name":"Ada Lovelace"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["id"], ada.id.as_str());
            lead.agents()
                .registry()
                .create("Bo", "stuck", None, "the user")
                .unwrap();
            let (status, _) = send_json(
                &app,
                json_post("/api/agents/Bo/rename", r#"{"name":"ada lovelace"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            for (req, code) in [
                (get("/api/agents/nobody"), StatusCode::NOT_FOUND),
                (
                    json_post("/api/agents/nobody/messages", r#"{"text":"hi"}"#),
                    StatusCode::NOT_FOUND,
                ),
                // No workspace store here (no k3s).
                (get("/api/workspaces"), StatusCode::BAD_GATEWAY),
                (
                    json_post("/api/workspaces/nextcloud/reassign", r#"{"to":"nobody"}"#),
                    StatusCode::NOT_FOUND,
                ),
            ] {
                assert_eq!(send_json(&app, req).await.0, code);
            }
        }

        #[tokio::test]
        async fn confirmations_can_be_listed_and_resolved() {
            let (app, lead) = app();
            let (c, rx) = lead.confirmations().request(
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
            let (app, lead) = app();
            // Unknown and Lead-only tools look the same: not found.
            for name in ["nope", "create_agent", "reassign_workspace"] {
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
                if let Some(c) = lead.confirmations().pending().first().cloned() {
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
                if let Some(c) = lead.confirmations().pending().first().cloned() {
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
            let (app, lead) = app();
            for uri in [
                "/api/lead/messages",
                "/api/lead/cancel",
                "/api/agents/Ada/messages",
                "/api/agents/Ada/rename",
                "/api/workspaces/nextcloud/reassign",
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
            assert_eq!(lead.status().turn, None);
            // A form post (no JSON content type) is not a message either.
            let req = post("/api/lead/messages")
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

        async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
            send_json(app, get(uri)).await
        }

        async fn post_json(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
            send_json(app, json_post(uri, &body.to_string())).await
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
            let (app, lead) = app();
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
                lead.events().epoch()
            )));
            assert!(body.contains("/assets/htmx-ext-sse.min.js"));
            assert!(body.contains("Nextcloud running, Immich starting"));
            assert!(body.contains("device-1"));
            // Before the first message: the agent it will start on, as the model chip that
            // opens the picker (a popover, no script needed to open it).
            assert!(body.contains("popovertarget=\"model-picker\""), "{body}");
            assert!(body.contains("id=\"model-picker\" popover=\"auto\""));
            assert!(body.contains("placeholder=\"Message Melissa\""));
            assert!(!body.contains("style="), "no inline styles");
            assert!(!body.contains("<script>"), "no inline scripts");

            // A conversation with a tool call and a pending confirmation.
            lead.events().push(LeadEvent::UserMessage {
                turn: 1,
                text: "Install <b>Immich</b>".into(),
                attachments: vec![],
                scheduled: None,
            });
            lead.events().push(LeadEvent::ToolCall {
                turn: Some(1),
                id: "x\" onclick=\"alert(1)".into(),
                title: "melissa__list_apps".into(),
                status: "done".into(),
                ms: None,
            });
            let (c, _rx) = lead.confirmations().request(
                1,
                "install_app",
                &serde_json::json!({"app": "immich"}),
                "this task has read untrusted content",
            );
            lead.events().push(LeadEvent::ConfirmationNeeded {
                id: c.id,
                task: 1,
                tool: "install_app".into(),
                reason: c.reason.clone(),
                agent: None,
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
        async fn cluster_and_strip_show_apps_and_devices() {
            let (app, _) = app();
            let (status, _, body) = get_html(&app, "/cluster").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("class=\"hex h-ok\""));
            assert!(body.contains("class=\"hex h-busy\""));
            assert!(body.contains("href=\"http://nextcloud.example.com\""));
            assert!(body.contains("Add an app"));
            assert!(body.contains("Fedora Linux 45"));
            assert!(body.contains("4.0 GB of 8.0 GB used"));
            // device_info's /var is 91% used: a storage meter and a warning on the card.
            assert!(body.contains("<span>Storage</span>"));
            assert!(body.contains("value=\"91\""));
            assert!(body.contains("Storage is almost full: 1.7 GB left."));
            let (status, _, body) = get_html(&app, "/ui/strip").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.starts_with("<section class=\"strip\""));
            assert!(body
                .contains("<span class=\"strip-warn\">Storage almost full (1.7 GB left)</span>"));
            let (_, _, body) = get_html(&app, "/ui/cluster/apps").await;
            assert!(body.starts_with("<div id=\"cluster-apps\""));
        }

        // --- App details --------------------------------------------------------------------

        /// One workspace, `nextcloud`, owned by an agent that does not exist (the lead agent only is told).
        #[derive(Default)]
        struct OneWorkspace {
            deleted: std::sync::Mutex<Vec<String>>,
        }
        impl melissa_agent::tools::WorkspaceStore for OneWorkspace {
            fn workspaces(
                &self,
            ) -> BoxFuture<'_, Result<Vec<melissa_agent::tools::WorkspaceInfo>, String>>
            {
                Box::pin(async move {
                    if !self.deleted.lock().unwrap().is_empty() {
                        return Ok(Vec::new());
                    }
                    Ok(vec![melissa_agent::tools::WorkspaceInfo {
                        name: "nextcloud".into(),
                        owner: melissa_agent::tools::Owner::Agent("d-0a1b2c3d.a7".into()),
                        device: Some("d-0a1b2c3d".into()),
                        assigned_by: None,
                        assigned_at: None,
                        legacy_owner: None,
                    }])
                })
            }
            fn assign<'a>(
                &'a self,
                _: &'a str,
                _: &'a str,
                _: &'a str,
            ) -> BoxFuture<'a, Result<(), String>> {
                Box::pin(async { Err("no".into()) })
            }
            fn force_delete<'a>(
                &'a self,
                workspace: &'a str,
            ) -> BoxFuture<'a, Result<Vec<String>, String>> {
                Box::pin(async move {
                    self.deleted.lock().unwrap().push(workspace.into());
                    Ok(vec!["Namespace (all objects, volumes and data)".into()])
                })
            }
        }

        #[tokio::test]
        async fn app_tiles_open_details_with_status_logs_and_actions() {
            let (app, _) = app();
            let (_, _, body) = get_html(&app, "/cluster").await;
            assert!(
                body.contains("<dialog class=\"sheet\" id=\"app-dialog\""),
                "{body}"
            );
            assert!(body.contains("hx-get=\"/ui/apps/nextcloud\""));
            assert!(body.contains("cluster-changed from:body"));

            let req = axum::http::Request::get("/ui/apps/nextcloud")
                .header(header::HOST, "192.0.2.7:8080")
                .body(Body::empty())
                .unwrap();
            let (status, _, body) = send(&app, req).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body.contains("<h2 id=\"app-dialog-h\">Nextcloud</h2>"),
                "{body}"
            );
            assert!(body.contains("Running"));
            assert!(body.contains("<dd>Ada</dd>"));
            assert!(body.contains("href=\"http://192.0.2.7:30000\""));
            assert!(body.contains("<dd>1/1 ready</dd>"));
            assert!(body.contains("docker.io/library/nextcloud:34.0.6"));
            assert!(body.contains("hx-get=\"/ui/apps/nextcloud/logs\""));
            assert!(body.contains(">Stop</button>"));
            assert!(body.contains("Deletes Nextcloud and all its data."));
            assert!(body.contains("<form method=\"dialog\">"));
            // Bad names never reach a tool.
            assert_eq!(
                get_html(&app, "/ui/apps/..%2Fx").await.0,
                StatusCode::NOT_FOUND
            );
        }

        #[tokio::test]
        async fn hostile_log_lines_are_only_ever_text() {
            let (app, _) = app();
            let (status, _, body) = get_html(&app, "/ui/apps/nextcloud/logs").await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
                "{body}"
            );
            assert!(body.contains("**bold** &lt;img src=x onerror=alert(2)&gt;"));
            assert!(
                !body.contains("<script>") && !body.contains("<img") && !body.contains("<strong>")
            );
            // Several containers: a choice.
            assert!(body.contains("<option value=\"cron\">cron</option>"));
            // The JSON mirror.
            let (status, v) = send_json(&app, get("/api/apps/nextcloud/logs")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(v["pod"], "nc-1");
            let (status, v) = send_json(&app, get("/api/apps/nextcloud")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(v["agent_name"], "Ada");
        }

        #[tokio::test]
        async fn force_delete_needs_the_name_and_then_removes_and_tells_the_lead() {
            let workspaces = Arc::new(OneWorkspace::default());
            let (app, lead) = app_full(None, Some(workspaces.clone()), false);
            // No or wrong confirmation: refused, nothing deleted.
            let (status, _, html) = send(
                &app,
                htmx_post("/ui/apps/nextcloud/force-delete", "confirm=yes"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Type nextcloud to confirm."));
            let (status, v) =
                send_json(&app, json_post("/api/apps/nextcloud/force-delete", r#"{}"#)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
            // Cross-site: rejected before anything runs.
            let req = axum::http::Request::post("/api/apps/nextcloud/force-delete")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "http://evil.example.com")
                .header(header::HOST, "127.0.0.1:8080")
                .body(Body::from(r#"{"confirm":"nextcloud"}"#))
                .unwrap();
            assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);
            assert!(workspaces.deleted.lock().unwrap().is_empty());

            let (status, headers, html) = send(
                &app,
                htmx_post("/ui/apps/nextcloud/force-delete", "confirm=nextcloud"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Nextcloud and its data are gone."), "{html}");
            assert_eq!(headers["hx-trigger"], "cluster-changed");
            assert_eq!(
                *workspaces.deleted.lock().unwrap(),
                vec!["nextcloud".to_string()]
            );
            // The owning agent no longer exists: only the lead agent hears of it.
            assert!(lead
                .events()
                .since(0, 100)
                .events
                .iter()
                .any(|r| matches!(&r.event, LeadEvent::WorkspaceForceDeleted { workspace, agent: None } if workspace == "nextcloud")));
            // Gone now.
            let (status, _) = send_json(
                &app,
                json_post(
                    "/api/apps/nextcloud/force-delete",
                    r#"{"confirm":"nextcloud"}"#,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        }

        #[tokio::test]
        async fn approvals_page_and_approve_deny() {
            let (app, lead) = app();
            let (_, _, body) = get_html(&app, "/approvals").await;
            assert!(body.contains("Nothing waiting."));

            let (a, rx_a) = lead.confirmations().request(
                1,
                "uninstall_app",
                &serde_json::json!({"app": "nextcloud", "delete_data": true}),
                "destructive action",
            );
            let (b, rx_b) =
                lead.confirmations()
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

        /// "Always allow" on a helper's request: the card asks with the rule's words, saves
        /// the rule, and Settings → Permissions lists it until it is removed.
        #[tokio::test]
        async fn always_allow_saves_a_rule_listed_in_settings() {
            use melissa_agent::tools::{ActionKey, Principal, RuleOffer, Verdict};
            let (app, lead) = app();
            let ada = lead
                .agents()
                .registry()
                .create("Ada", "games", None, "the lead agent (task 1)")
                .unwrap();
            let offer = RuleOffer {
                key: ActionKey {
                    principal: Principal::Agent(ada.id.clone()),
                    tool: "update_app".into(),
                    target: Some("minecraft".into()),
                },
                verb: "update".into(),
            };
            let args = serde_json::json!({"app": "minecraft"});
            let (c, rx) = lead.confirmations().request_with(
                1,
                "update_app",
                &args,
                "runs software",
                Some(offer.clone()),
            );
            let (_, _, body) = get_html(&app, "/approvals").await;
            assert!(body.contains(&format!("hx-post=\"/ui/confirmations/{}/always\"", c.id)));
            assert!(body.contains("Always let Ada update Minecraft?"));
            // The user's own request (no rule to offer) has no such button.
            let (own, _own_rx) =
                lead.confirmations()
                    .request(0, "touch", &serde_json::json!({}), "because");
            let (_, _, body) = get_html(&app, "/approvals").await;
            assert!(!body.contains(&format!("/ui/confirmations/{}/always", own.id)));
            let (status, body) = send_json(
                &app,
                json_post(&format!("/api/confirmations/{}/always", own.id), "{}"),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");

            let uri = format!("/ui/confirmations/{}/always", c.id);
            let (status, _, html) = send(&app, htmx_post(&uri, "")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("You allowed this."));
            assert_eq!(rx.await.unwrap(), Verdict::Approved);
            let rules = lead.server().registry().permissions().rules();
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].key, offer.key);

            let (status, _, body) = get_html(&app, "/settings/permissions").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("Ada"));
            assert!(body.contains("update Minecraft"));
            let (status, body) = send_json(&app, get("/api/settings/permissions")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["rules"][0]["target"], "minecraft");
            assert_eq!(body["bypass"], false);

            let remove = format!("/ui/settings/permissions/{}/remove", rules[0].id);
            let (status, _, html) = send(&app, htmx_post(&remove, "")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Nothing yet."));
            assert!(lead.server().registry().permissions().rules().is_empty());
        }

        /// Settings → Schedules: how long a scheduled run's request waits for the owner.
        #[tokio::test]
        async fn schedules_page_sets_the_wait_for_a_runs_requests() {
            use melissa_agent::lead::schedules::{Schedules, TimeZone};
            let (app, lead) = app();
            assert_eq!(
                get_json(&app, "/api/settings/schedules").await.0,
                StatusCode::SERVICE_UNAVAILABLE
            );
            let sched = lead.start_schedules(Schedules::in_memory(TimeZone::UTC));
            let (_, _, body) = get_html(&app, "/settings/schedules").await;
            assert!(body.contains("When a run needs your OK"), "{body}");
            assert!(body.contains("name=\"minutes\""));
            assert!(body.contains("value=\"10\""), "default 10 minutes: {body}");
            let (status, _, html) =
                send(&app, htmx_post("/ui/settings/schedules/wait", "minutes=45")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Saved."), "{html}");
            assert_eq!(sched.schedules().approval_wait_minutes(), 45);
            for bad in ["minutes=0", "minutes=1441", "minutes=soon"] {
                let (_, _, html) = send(&app, htmx_post("/ui/settings/schedules/wait", bad)).await;
                assert!(html.contains("Couldn"), "{bad}: {html}");
            }
            assert_eq!(sched.schedules().approval_wait_minutes(), 45);
            let (status, body) = post_json(
                &app,
                "/api/settings/schedules",
                serde_json::json!({"approval_wait_minutes": 120}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["approval_wait_minutes"], 120);
            let (status, _) = post_json(
                &app,
                "/api/settings/schedules",
                serde_json::json!({"approval_wait_minutes": 0}),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(
                get_json(&app, "/api/settings/schedules").await.1["approval_wait_minutes"],
                120
            );
        }

        /// A file added to the box updates an open chat's file panel over the stream.
        #[tokio::test]
        async fn the_file_panel_updates_live() {
            let (app, lead) = app();
            let since = lead.events().last_seq();
            let epoch = lead.events().epoch();
            let mut body = app
                .clone()
                .oneshot(get(&format!("/ui/stream?since={since}&epoch={epoch}")))
                .await
                .unwrap()
                .into_body();
            lead.files()
                .save_named(
                    &melissa_agent::tools::permissions::Principal::Lead,
                    "notes.txt",
                    b"saved by Melissa",
                )
                .unwrap();
            let frame = next_frame(&mut body).await;
            assert!(frame.contains("innerHTML:#file-box-body"), "{frame}");
            assert!(frame.contains("notes.txt"), "{frame}");
            assert!(
                frame.contains("id=\"file-count\" hx-swap-oob=\"true\">1<"),
                "{frame}"
            );
        }

        /// Settings → Schedules: add (presets), edit, switch off, run now (labelled in the
        /// chat), remove; bad input keeps what was typed.
        #[tokio::test]
        async fn schedules_page_adds_edits_runs_and_removes() {
            use melissa_agent::lead::schedules::{Creator, Schedules, TimeZone};
            let (app, lead) = app();
            let (_, _, body) = get_html(&app, "/settings/schedules").await;
            assert!(body.contains("Schedules don't run on this device."));
            let sched = lead.start_schedules(Schedules::in_memory(TimeZone::UTC));
            let (status, _, body) = get_html(&app, "/settings/schedules").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("Nothing scheduled yet."));
            assert!(body.contains("href=\"/settings/schedules\""));
            assert!(body.contains("action=\"/ui/settings/schedules\""));

            let form = "name=Nightly+updates&agent=lead&instruction=Update+all+apps.\
                        &repeat=daily&day=1&time=03%3A00&cron=&max_minutes=60";
            let (status, _, html) = send(&app, htmx_post("/ui/settings/schedules", form)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Added."), "{html}");
            assert!(html.contains("Every day at 03:00"));
            let made = sched.schedules().list().remove(0);
            assert_eq!(made.cron, "0 3 * * *");
            assert_eq!(made.created_by, Creator::Owner);

            // A bad cron expression: an error, and the form keeps what was typed.
            let bad = "name=Odd+one&agent=lead&instruction=x&repeat=custom&cron=at+three";
            let (_, _, html) = send(&app, htmx_post("/ui/settings/schedules", bad)).await;
            assert!(
                html.contains("Couldn&#39;t save that") || html.contains("Couldn't save that"),
                "{html}"
            );
            assert!(html.contains("value=\"Odd one\""));
            assert_eq!(sched.schedules().list().len(), 1);

            let (_, _, body) =
                get_html(&app, &format!("/settings/schedules?edit={}", made.id)).await;
            assert!(body.contains("value=\"Nightly updates\""));
            assert!(body.contains("<option value=\"daily\" selected"));
            let edit = format!("/ui/settings/schedules/{}", made.id);
            let weekly = "name=Nightly+updates&agent=lead&instruction=Update+all+apps.\
                          &repeat=weekly&day=1&time=04%3A30&max_minutes=90";
            let (_, _, html) = send(&app, htmx_post(&edit, weekly)).await;
            assert!(html.contains("Every Monday at 04:30"), "{html}");
            let s = sched.schedules().get(made.id).unwrap();
            assert_eq!((s.cron.as_str(), s.max_minutes), ("30 4 * * 1", 90));

            let (_, _, html) = send(&app, htmx_post(&format!("{edit}/enabled"), "")).await;
            assert!(html.contains(">Off<"), "{html}");
            assert!(!sched.schedules().get(made.id).unwrap().enabled);

            // Run now works while off; the chat shows the instruction with its label.
            let (_, _, html) = send(&app, htmx_post(&format!("{edit}/run"), "")).await;
            assert!(html.contains("Started."), "{html}");
            let (_, _, chat) = get_html(&app, "/").await;
            assert!(chat.contains("Scheduled: Nightly updates"), "{chat}");
            assert!(chat.contains("Update all apps."));
            let (_, _, html) = send(&app, htmx_post(&format!("{edit}/run"), "")).await;
            assert!(
                html.contains("Not started: the previous run was still going."),
                "{html}"
            );

            let (_, _, html) = send(&app, htmx_post(&format!("{edit}/remove"), "")).await;
            assert!(html.contains("Removed."));
            assert!(sched.schedules().list().is_empty());
            lead.cancel().await;
        }

        #[tokio::test]
        async fn schedules_api() {
            use melissa_agent::lead::schedules::{Schedules, TimeZone};
            let (app, lead) = app();
            let (status, _) = send_json(&app, get("/api/schedules")).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            lead.start_schedules(Schedules::in_memory(TimeZone::UTC));
            let body = r#"{"name": "Nightly", "instruction": "Update all apps.", "schedule": "0 3 * * *"}"#;
            let (status, made) = send_json(&app, json_post("/api/schedules", body)).await;
            assert_eq!(status, StatusCode::CREATED, "{made}");
            assert_eq!(made["agent"], "lead");
            assert_eq!(made["when"], "Every day at 03:00");
            assert_eq!(made["created_by"], "owner");
            assert!(made["next_run"].is_i64());
            for bad in [
                r#"{"name": "x", "instruction": "y", "schedule": "nope"}"#,
                r#"{"name": "x", "instruction": "y", "schedule": "0 3 * * *", "agent": "Nobody"}"#,
                r#"{"name": "", "instruction": "y", "schedule": "0 3 * * *"}"#,
            ] {
                let (status, _) = send_json(&app, json_post("/api/schedules", bad)).await;
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
            }
            let id = made["id"].as_u64().unwrap();
            let (status, changed) = send_json(
                &app,
                json_post(
                    &format!("/api/schedules/{id}"),
                    r#"{"schedule": "0 4 * * *"}"#,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                (changed["name"].as_str(), changed["schedule"].as_str()),
                (Some("Nightly"), Some("0 4 * * *"))
            );
            let (_, off) = send_json(
                &app,
                json_post(&format!("/api/schedules/{id}/enabled"), r#"{"on": false}"#),
            )
            .await;
            assert_eq!(off["enabled"], false);
            assert_eq!(off["next_run"], Value::Null);
            let (status, list) = send_json(&app, get("/api/schedules")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(list["schedules"].as_array().unwrap().len(), 1);
            assert_eq!(list["time_zone"], "UTC");
            let (status, run) =
                send_json(&app, json_post(&format!("/api/schedules/{id}/run"), "{}")).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(run["run"]["outcome"], "running");
            assert_eq!(run["run"]["manual"], true);
            let (status, _) = send_json(&app, json_post("/api/schedules/99/remove", "{}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let (status, gone) = send_json(
                &app,
                json_post(&format!("/api/schedules/{id}/remove"), "{}"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(gone["id"], id);
            lead.cancel().await;
        }

        /// No permission prompts: off by default, on only after the confirmation card, then
        /// waiting requests allowed (no banner on the pages: owner's call); off again at once.
        #[tokio::test]
        async fn no_permission_prompts_needs_a_yes() {
            let (app, lead) = app();
            const WARNING: &str = "No permission prompts is on.";
            let (_, _, body) = get_html(&app, "/settings/developer").await;
            assert!(body.contains("action=\"/ui/settings/developer/bypass\""));
            assert!(!body.contains(WARNING));

            // The switch alone only asks.
            let (status, _, html) = send(
                &app,
                htmx_post("/ui/settings/developer/bypass", "enabled=on"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("/ui/settings/developer/bypass/on"));
            assert!(html.contains("name=\"confirm\" value=\"yes\""));
            assert!(!lead.server().registry().permissions().bypass());
            // The API wants the yes too.
            let (status, _) = send_json(
                &app,
                json_post("/api/settings/permissions/bypass", r#"{"on": true}"#),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(!lead.server().registry().permissions().bypass());

            let (_c, rx) =
                lead.confirmations()
                    .request(1, "touch", &serde_json::json!({}), "because");
            let (status, headers, _) = send(
                &app,
                htmx_post("/ui/settings/developer/bypass/on", "confirm=yes"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(!headers.contains_key("hx-refresh"));
            assert!(lead.server().registry().permissions().bypass());
            assert_eq!(rx.await.unwrap(), melissa_agent::tools::Verdict::Approved);
            for page in [
                "/",
                "/approvals",
                "/cluster",
                "/settings/providers",
                "/settings/developer",
            ] {
                let (_, _, body) = get_html(&app, page).await;
                assert!(!body.contains(WARNING), "{page}");
            }

            // Off: no confirmation needed.
            let (status, _, _) = send(&app, htmx_post("/ui/settings/developer/bypass", "")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!lead.server().registry().permissions().bypass());
            let (_, _, body) = get_html(&app, "/").await;
            assert!(!body.contains(WARNING));
        }

        /// The SSE frames of `body` until one contains `needle` (returns it).
        async fn frame_with(body: &mut Body, needle: &str) -> String {
            loop {
                let frame = next_frame(body).await;
                if frame.contains(needle) {
                    return frame;
                }
            }
        }

        /// A helper's confirmation shows in her own chat (no label) and in Melissa's and
        /// Waiting for you (labelled with her name); deciding it in one updates the others.
        #[tokio::test]
        async fn a_helpers_approval_shows_in_her_chat_and_melissas() {
            let (app, lead) = app();
            let ada = lead
                .agents()
                .registry()
                .create("Ada", "stuck", None, "the lead agent (task 1)")
                .unwrap();
            let page = format!("/agents/{}", ada.id);
            // Opening her chat sets up her conversation (and its task).
            assert_eq!(get_html(&app, &page).await.0, StatusCode::OK);
            let task = (1..1000)
                .find(|t| lead.agents().agent_of_task(*t).as_deref() == Some(ada.id.as_str()))
                .expect("her conversation has a task");
            let open = |uri: String| {
                let app = app.clone();
                async move { app.oneshot(get(&uri)).await.unwrap().into_body() }
            };
            let mut hers = open(format!("/ui/agents/{}/stream?since=0", ada.id)).await;
            let mut main = open("/ui/stream?since=0".into()).await;

            let (c, rx) = lead.confirmations().request(
                task,
                "uninstall_app",
                &serde_json::json!({"app": "blog"}),
                "destructive action",
            );
            let approve = format!("hx-post=\"/ui/confirmations/{}/approve\"", c.id);
            let frame = frame_with(&mut hers, "Remove Blog?").await;
            assert!(
                frame.contains(&approve) && !frame.contains("Ada asks"),
                "{frame}"
            );
            let frame = frame_with(&mut main, "Ada asks: Remove Blog?").await;
            assert!(frame.contains(&approve), "{frame}");
            // The same on a fresh page load.
            let (_, _, body) = get_html(&app, &page).await;
            assert!(body.contains("Remove Blog?") && body.contains(&approve));
            assert!(!body.contains("Ada asks"));
            for uri in ["/", "/approvals"] {
                let (_, _, body) = get_html(&app, uri).await;
                assert!(body.contains("Ada asks: Remove Blog?"), "{uri}");
                assert!(body.contains(&approve), "{uri}");
            }

            // Approved from her chat: the card keeps her chat's words.
            let req = post(&format!("/ui/confirmations/{}/approve", c.id))
                .header("hx-request", "true")
                .header("hx-current-url", format!("http://203.0.113.5:8080{page}"))
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap();
            let (status, _, html) = send(&app, req).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("You allowed this.") && !html.contains("Ada asks"));
            assert_eq!(rx.await.unwrap(), melissa_agent::tools::Verdict::Approved);
            // Both chats update live and after a reload.
            frame_with(&mut hers, "You allowed this.").await;
            let frame = frame_with(&mut main, "You allowed this.").await;
            assert!(frame.contains("Ada asks: Remove Blog?"), "{frame}");
            for uri in [page.as_str(), "/"] {
                let (_, _, body) = get_html(&app, uri).await;
                assert!(body.contains("You allowed this."), "{uri}");
                assert!(!body.contains(&approve), "{uri}");
            }
            let (_, _, body) = get_html(&app, "/approvals").await;
            assert!(body.contains("Nothing waiting."));

            // From Melissa's chat, the card stays labelled; a withdrawn one shows in both.
            let (c, _rx) = lead.confirmations().request(
                task,
                "uninstall_app",
                &serde_json::json!({"app": "blog"}),
                "destructive action",
            );
            frame_with(&mut main, &format!("/ui/confirmations/{}/deny", c.id)).await;
            let (c2, _rx2) = lead.confirmations().request(
                task,
                "stop_app",
                &serde_json::json!({"app": "blog"}),
                "destructive action",
            );
            let uri = format!("/ui/confirmations/{}/deny", c.id);
            let req = post(&uri)
                .header("hx-request", "true")
                .header("hx-current-url", "http://203.0.113.5:8080/")
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap();
            let (_, _, html) = send(&app, req).await;
            assert!(html.contains("Ada asks: Remove Blog?"), "{html}");
            frame_with(&mut hers, "Stop Blog?").await;
            lead.confirmations().withdraw(c2.id);
            frame_with(&mut hers, "No longer needed").await;
            frame_with(&mut main, "No longer needed").await;
            let (_, _, body) = get_html(&app, &page).await;
            assert!(!body.contains(&format!("/ui/confirmations/{}/deny", c2.id)));
            // (Decided cards lose the app's name on reload: arguments live only while pending.)
            assert!(body.contains("No longer needed"));
        }

        #[tokio::test]
        async fn sending_a_message_from_the_composer() {
            let (app, lead) = app();
            let (status, _, html) = send(&app, htmx_post("/ui/messages", "text=Hello+there")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.starts_with("<form class=\"composer\" id=\"composer\""));
            assert!(html.contains("Working on it"));
            assert!(html.contains("></textarea>"), "box is cleared");
            assert_eq!(lead.status().turn, Some(1));

            // Busy: the text stays and the reason is shown.
            let (_, _, html) = send(&app, htmx_post("/ui/messages", "text=Second")).await;
            assert!(html.contains("Still working on your last message"));
            assert!(html.contains(">Second</textarea>"));

            // Stop.
            let (status, _, _) = send(&app, htmx_post("/ui/cancel", "")).await;
            assert_eq!(status, StatusCode::NO_CONTENT);
            assert_eq!(lead.status().turn, None);

            // Plain form post: redirect home.
            let req = post("/ui/messages").body(Body::from("text=Hi")).unwrap();
            let (status, headers, _) = send(&app, req).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/");
        }

        type Part<'a> = (&'a str, Option<&'a str>, &'a [u8]);

        /// A multipart form post as htmx sends it (`files` parts carry a file name).
        fn multipart(uri: &str, parts: &[Part<'_>]) -> axum::http::Request<Body> {
            let boundary = "melissa-test-boundary";
            let mut body = Vec::new();
            for (name, file, bytes) in parts {
                body.extend(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"")
                        .as_bytes(),
                );
                if let Some(f) = file {
                    body.extend(format!("; filename=\"{f}\"").as_bytes());
                    body.extend(b"\r\nContent-Type: application/octet-stream");
                }
                body.extend(b"\r\n\r\n");
                body.extend(*bytes);
                body.extend(b"\r\n");
            }
            body.extend(format!("--{boundary}--\r\n").as_bytes());
            axum::http::Request::post(uri)
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::HOST, "127.0.0.1:8080")
                .header("hx-request", "true")
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(body))
                .unwrap()
        }

        #[tokio::test]
        async fn sending_and_serving_files() {
            const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
            let (app, lead) = app();
            let (_, _, page) = get_html(&app, "/").await;
            assert!(page.contains("hx-encoding=\"multipart/form-data\""));
            assert!(page.contains("name=\"files\" multiple"));
            assert!(page.contains("<template id=\"file-chip\">"));

            // Too large: refused while it streams in; the text stays, the files must be
            // attached again, no turn starts.
            let big = vec![0u8; melissa_agent::attachments::MAX_FILE_BYTES + 1];
            let req = multipart(
                "/ui/messages",
                &[("text", None, b"Look"), ("files", Some("big.bin"), &big)],
            );
            let (status, _, html) = send(&app, req).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("big.bin is larger than 20 MiB"), "{html}");
            // htmx (melissa.js) keeps the picked files; a plain post is asked for them again.
            assert!(!html.contains("Attach the files again."));
            let mut req = multipart(
                "/ui/messages",
                &[("text", None, b"Look"), ("files", Some("big.bin"), &big)],
            );
            req.headers_mut().remove("hx-request");
            let (status, _, html) = send(&app, req).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(html.contains("Attach the files again."), "{html}");
            assert!(html.contains(">Look</textarea>"));
            assert_eq!(lead.status().turn, None);

            // An image and a text file, no text; the empty part of an unused input is skipped.
            let req = multipart(
                "/ui/messages",
                &[
                    ("text", None, b""),
                    ("files", Some("shot.png"), PNG),
                    ("files", Some("my notes.txt"), b"hello"),
                    ("files", Some(""), b""),
                ],
            );
            let (status, _, html) = send(&app, req).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("></textarea>"), "box is cleared: {html}");
            assert_eq!(lead.status().turn, Some(1));
            let files = lead
                .events()
                .since(0, 100)
                .events
                .into_iter()
                .find_map(|r| match r.event {
                    LeadEvent::UserMessage { attachments, .. } => Some(attachments),
                    _ => None,
                })
                .unwrap();
            let names: Vec<&str> = files.iter().map(|a| a.name.as_str()).collect();
            assert_eq!(names, ["shot.png", "my_notes.txt"]);

            // The chat shows a thumbnail and a download chip, served from Melissa's box.
            let id = |f: &melissa_agent::attachments::Attachment| {
                melissa_agent::files::attachment_id(&f.file).to_string()
            };
            let (_, _, page) = get_html(&app, "/").await;
            let png = format!("/ui/files/lead/{}", id(&files[0]));
            let txt = format!("/ui/files/lead/{}", id(&files[1]));
            assert!(
                page.contains(&format!("<img src=\"{png}\" alt=\"shot.png\"")),
                "{page}"
            );
            assert!(page.contains(&format!("href=\"{txt}\" download=\"my_notes.txt\"")));
            // The file panel lists both, newest first, sent by "You".
            assert!(page.contains("<aside class=\"file-panel\""), "{page}");
            assert!(page.contains("data-remember=\"file-panel\""));
            assert!(page.contains("2 files, "), "{page}");
            assert!(page.contains("Kept for 5 days (default)"), "{page}");
            assert!(page.contains(" · You<"), "{page}");

            // Served: images inline, the rest as downloads; never script, never embedded
            // by another site.
            let (status, headers, _) = send(&app, get(&png)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers[header::CONTENT_TYPE], "image/png");
            assert_eq!(headers[header::CONTENT_DISPOSITION], "inline");
            assert_eq!(
                headers[header::CONTENT_SECURITY_POLICY],
                "default-src 'none'; sandbox"
            );
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
            let (_, headers, body) = send(&app, get(&txt)).await;
            assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
            assert_eq!(
                headers[header::CONTENT_DISPOSITION],
                "attachment; filename=\"my_notes.txt\""
            );
            assert_eq!(body, "hello");
            // Only generated ids of real boxes; another box doesn't have Melissa's files.
            for bad in [
                "/ui/files/lead/..%2F..%2Fetc%2Fpasswd".to_string(),
                "/ui/files/lead/my_notes.txt".to_string(),
                "/ui/files/lead/0123456789abcdef0123456789abcdef".to_string(),
                "/ui/files/..%2Flead/0123456789abcdef0123456789abcdef".to_string(),
                format!("/ui/files/d-0a1b2c3d.a1/{}", id(&files[0])),
                format!("/api/files/d-0a1b2c3d.a1/{}", id(&files[0])),
            ] {
                assert_eq!(
                    send(&app, get(&bad)).await.0,
                    StatusCode::NOT_FOUND,
                    "{bad}"
                );
            }

            // Delete from the panel: the body comes back without it, and the message shows
            // the file as expired.
            let (status, _, html) = send(
                &app,
                htmx_post(&format!("/ui/files/lead/{}/delete", id(&files[1])), ""),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("id=\"file-box-body\""), "{html}");
            assert!(html.contains("1 file, "), "{html}");
            assert!(!html.contains("my_notes.txt"), "{html}");
            assert_eq!(send(&app, get(&txt)).await.0, StatusCode::NOT_FOUND);
            let (_, _, page) = get_html(&app, "/").await;
            assert!(page.contains("file-chip expired"), "{page}");
            assert!(page.contains(">expired<"), "{page}");
            assert!(!page.contains(&format!("href=\"{txt}\"")));
        }

        #[tokio::test]
        async fn a_helpers_files_and_retention() {
            let (app, lead) = app();
            let ada = lead
                .agents()
                .registry()
                .create("Ada", "stuck", None, "the lead agent (task 1)")
                .unwrap();
            let owner = melissa_agent::tools::permissions::Principal::Agent(ada.id.clone());
            // A file the helper saved in her box shows in her panel, by her.
            let dir = lead.files().ensure_dir(&owner).unwrap();
            std::fs::write(dir.join("report.md"), "# Weekly report\n").unwrap();
            let (status, _, page) = get_html(&app, &format!("/agents/{}", ada.id)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(page.contains("report.md"), "{page}");
            assert!(page.contains(" · Ada<"), "{page}");
            // Melissa's panel doesn't show it.
            let (_, _, home) = get_html(&app, "/").await;
            assert!(!home.contains("report.md"));

            // The API lists it and serves it.
            let (status, body) = get_json(&app, &format!("/api/files/{}", ada.id)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["files"][0]["name"], "report.md");
            assert_eq!(body["files"][0]["by"], "agent");
            assert_eq!(body["keep_days"], 5);
            assert_eq!(body["override"], false);
            let file_id = body["files"][0]["id"].as_str().unwrap().to_string();
            let (status, _, text) =
                send(&app, get(&format!("/api/files/{}/{file_id}", ada.id))).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(text, "# Weekly report\n");
            assert_eq!(
                get_json(&app, "/api/files/nobody").await.0,
                StatusCode::NOT_FOUND
            );

            // Her own retention, from the panel: forever.
            let (status, _, html) = send(
                &app,
                htmx_post(&format!("/ui/files/{}/retention", ada.id), "keep=forever"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Kept forever"), "{html}");
            assert_eq!(
                lead.files().keep_for(&owner),
                melissa_agent::files::Keep::Forever
            );
            // Through the API: 2 days, then back to the default; bounds checked.
            let (status, body) = post_json(
                &app,
                &format!("/api/files/{}/retention", ada.id),
                serde_json::json!({"keep_days": 2}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["keep_days"], 2);
            let (status, _) = post_json(
                &app,
                &format!("/api/files/{}/retention", ada.id),
                serde_json::json!({"keep_days": 0}),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            // Settings → Files lists boxes with their own setting.
            let (status, _, settings) = get_html(&app, "/settings/files").await;
            assert_eq!(status, StatusCode::OK);
            assert!(settings.contains("Boxes with their own setting"));
            assert!(
                settings.contains(&format!("href=\"/agents/{}\"", ada.id)),
                "{settings}"
            );
            let (_, body) = post_json(
                &app,
                &format!("/api/files/{}/retention", ada.id),
                serde_json::json!({"default": true}),
            )
            .await;
            assert_eq!(body["override"], false);

            // The default, in Settings and the API.
            let (status, _, html) = send(&app, htmx_post("/ui/settings/files", "keep=14")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Saved."), "{html}");
            assert_eq!(
                lead.files().keep_for(&owner),
                melissa_agent::files::Keep::Days(14)
            );
            let (_, _, html) = send(&app, htmx_post("/ui/settings/files", "keep=0")).await;
            assert!(html.contains("Couldn"), "{html}");
            let (status, body) = post_json(
                &app,
                "/api/settings/files",
                serde_json::json!({"keep_days": null}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["keep_days"], serde_json::Value::Null);
            assert_eq!(
                get_json(&app, "/api/settings/files").await.1["keep_days"],
                serde_json::Value::Null
            );

            // Delete through the API.
            let (status, body) = post_json(
                &app,
                &format!("/api/files/{}/{file_id}/delete", ada.id),
                serde_json::json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["deleted"]["name"], "report.md");
            assert!(!dir.join("report.md").exists());
        }

        /// The send reply can reach the browser after the stream already ended the turn
        /// (seen with the echo backend: "Working on it" stuck). The stream owns the status:
        /// the reply's copy is marked `hx-preserve` so htmx keeps the live one, and the
        /// stream's updates swap the status's children, which `hx-preserve` doesn't block.
        #[tokio::test]
        async fn a_late_send_reply_does_not_overwrite_the_live_status() {
            let (app, lead) = app();
            let res = app
                .clone()
                .oneshot(get("/ui/stream?since=0"))
                .await
                .unwrap();
            let mut stream = res.into_body();
            let swap = "hx-swap-oob=\"innerHTML:#composer-status\"";
            let (_, _, reply) = send(&app, htmx_post("/ui/messages", "text=Hello")).await;
            let busy = next_frame(&mut stream).await;
            assert!(
                busy.contains(swap) && busy.contains("Working on it"),
                "{busy}"
            );
            // The turn ends before the reply is swapped in.
            send(&app, htmx_post("/ui/cancel", "")).await;
            assert_eq!(lead.status().turn, None);
            let mut idle = String::new();
            while !idle.contains(swap) {
                idle = next_frame(&mut stream).await;
            }
            assert!(!idle.contains("Working on it"), "{idle}");
            assert!(!busy.contains("hx-preserve") && !idle.contains("hx-preserve"));

            let status_div = "id=\"composer-status\" hx-preserve";
            assert!(reply.contains(status_div), "{reply}");
            // The page renders the same element, so the reply's copy is always discarded.
            let (_, _, page) = get_html(&app, "/").await;
            assert!(page.contains(status_div));
        }

        #[tokio::test]
        async fn chatting_with_a_helper() {
            let (app, lead) = app();
            let ada = lead
                .agents()
                .registry()
                .create("Ada", "stuck", None, "the lead agent (task 1)")
                .unwrap();
            // The rail lists her; her page is a chat.
            let (_, _, home) = get_html(&app, "/").await;
            assert!(home.contains(&format!("href=\"/agents/{}\"", ada.id)));
            assert!(home.contains(">Ada<"));
            for uri in [format!("/agents/{}", ada.id), "/agents/ada".to_string()] {
                let (status, _, body) = get_html(&app, &uri).await;
                assert_eq!(status, StatusCode::OK, "{uri}");
                assert!(body.contains("Nothing here yet with Ada."));
                assert!(body.contains(&format!(
                    "sse-connect=\"/ui/agents/{}/stream?since=0&amp;epoch=",
                    ada.id
                )));
                assert!(body.contains(&format!("hx-post=\"/ui/agents/{}/messages\"", ada.id)));
                assert!(body.contains("aria-current=\"page\""));
            }
            assert_eq!(
                get_html(&app, "/agents/nobody").await.0,
                StatusCode::NOT_FOUND
            );
            // The task pages sit in Settings → Developer; GET /tasks redirects there and the
            // rail has no Agent tasks link any more.
            let req = axum::http::Request::post("/tasks")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("backend=echo&prompt=hi"))
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::SEE_OTHER);
            let task = res.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .to_string();
            let res = app.clone().oneshot(get("/tasks")).await.unwrap();
            assert_eq!(res.status(), StatusCode::SEE_OTHER);
            assert_eq!(res.headers()[header::LOCATION], "/settings/developer");
            for uri in ["/settings/developer", task.as_str()] {
                let (status, _, body) = get_html(&app, uri).await;
                assert_eq!(status, StatusCode::OK, "{uri}");
                assert!(body.contains("<nav class=\"rail\""), "{uri}");
                assert!(!body.contains("href=\"/tasks\""), "{uri}");
                assert!(
                    body.contains("href=\"/settings\" aria-current=\"page\""),
                    "{uri}"
                );
                assert!(
                    body.contains("href=\"/settings/developer\" aria-current=\"page\""),
                    "{uri}"
                );
            }
            let (_, _, body) = get_html(&app, task.as_str()).await;
            // Breadcrumb: Settings / Developer (a link) / Task #n.
            assert!(body.contains("<a href=\"/settings/developer\">Developer</a>"));
            assert!(body.contains("<title>Task #") && body.contains("Developer · Settings"));
            // Every Settings page lists the Developer section.
            let (_, _, body) = get_html(&app, "/settings/updates").await;
            assert!(body.contains("<a href=\"/settings/developer\">Developer</a>"));

            // A message goes to her conversation, not the lead agent's.
            let res = app
                .clone()
                .oneshot(get(&format!("/ui/agents/{}/stream?since=0", ada.id)))
                .await
                .unwrap();
            let mut body = res.into_body();
            let uri = format!("/ui/agents/{}/messages", ada.id);
            let (status, _, html) = send(&app, htmx_post(&uri, "text=Hello+Ada")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(html.contains("Working on it"));
            assert!(html.contains(&format!("hx-post=\"/ui/agents/{}/cancel\"", ada.id)));
            let frame = next_frame(&mut body).await;
            assert!(frame.contains("Hello Ada"), "{frame}");
            assert_eq!(lead.status().turn, None);
            let (status, _, _) = send(
                &app,
                htmx_post(&format!("/ui/agents/{}/cancel", ada.id), ""),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT);

            // The lead agent's feed links a helper's job to her conversation.
            lead.events().push(LeadEvent::AgentStarted {
                agent: ada.id.clone(),
                name: "Ada".into(),
                goal: "Check <the> device".into(),
            });
            let (_, _, home) = get_html(&app, "/").await;
            assert!(home.contains(&format!(
                "<a href=\"/agents/{}\">Ada is looking into it</a>",
                ada.id
            )));
            assert!(home.contains("Check &lt;the&gt; device"));
        }

        #[tokio::test]
        async fn the_taint_note_shows_in_the_chat() {
            let (app, lead) = app();
            lead.events().push(LeadEvent::Tainted {
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
            let (app, lead) = app();
            lead.events().push(LeadEvent::UserMessage {
                turn: 1,
                text: "Hi <there>".into(),
                attachments: vec![],
                scheduled: None,
            });
            lead.events().push(LeadEvent::TurnFinished {
                turn: 1,
                stop_reason: "end_turn".into(),
            });
            let (c, _rx) = lead.confirmations().request(
                lead.task_id(),
                "touch",
                &serde_json::json!({}),
                "because",
            );
            let (other, _rx2) =
                lead.confirmations()
                    .request(99, "touch", &serde_json::json!({}), "an agent's");
            let res = app
                .clone()
                .oneshot(get(&format!(
                    "/ui/stream?since={}&epoch={}",
                    lead.events().last_seq(),
                    lead.events().epoch()
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
            // The lead agent's confirmation is withdrawn, an agent's stays.
            let pending: Vec<u64> = lead
                .confirmations()
                .pending()
                .iter()
                .map(|p| p.id)
                .collect();
            assert_eq!(pending, [other.id]);
            assert!(next_frame(&mut stream).await.starts_with("event: reload"));

            let (_, _, home) = get_html(&app, "/").await;
            assert!(home.contains("Hi, I'm Melissa."));
            // Only the agent's confirmation, still waiting, carries over into the new chat.
            assert_eq!(home.matches("<li class=\"entry\"").count(), 1, "{home}");
            assert!(home.contains(&format!("/ui/confirmations/{}/approve", other.id)));
            assert!(!home.contains(&format!("/ui/confirmations/{}/approve", c.id)));
            assert!(home.contains("Past chats"));
            assert!(home.contains("Hi &lt;there&gt;"));
            let chats = lead.past_chats();
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
            let (status, body) = send_json(&app, json_post("/api/lead/new-chat", "{}")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["archived"], Value::Null, "nothing said since");
        }

        #[tokio::test]
        async fn a_stream_from_another_epoch_reloads() {
            let (app, lead) = app();
            lead.events().push(LeadEvent::UserMessage {
                turn: 1,
                text: "Hi".into(),
                attachments: vec![],
                scheduled: None,
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
            let (app, lead) = app();
            let (c, _rx) =
                lead.confirmations()
                    .request(1, "touch", &serde_json::json!({}), "because");
            for uri in [
                "/ui/messages".to_string(),
                "/ui/cancel".to_string(),
                format!("/ui/confirmations/{}/approve", c.id),
                "/ui/agents/ada/messages".to_string(),
                "/ui/agents/ada/cancel".to_string(),
            ] {
                let req = post(&uri)
                    .header("sec-fetch-site", "cross-site")
                    .body(Body::from("text=hi"))
                    .unwrap();
                assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN, "{uri}");
            }
            assert_eq!(lead.status().turn, None);
            assert_eq!(lead.confirmations().pending().len(), 1);
        }

        #[tokio::test]
        async fn the_stream_sends_rendered_updates() {
            let (app, lead) = app();
            lead.events().push(LeadEvent::UserMessage {
                turn: 1,
                text: "Hi".into(),
                attachments: vec![],
                scheduled: None,
            });
            lead.events().push(LeadEvent::Text {
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
            lead.events().push(LeadEvent::Text {
                turn: 1,
                text: " <there>".into(),
            });
            let frame = next_frame(&mut body).await;
            assert!(frame.starts_with("event: feed\n"), "{frame}");
            assert!(frame.contains(&format!("id: {}-3\n", lead.events().epoch())));
            assert!(frame.contains("hx-swap-oob=\"innerHTML:#f-r2\""));
            assert!(frame.contains("Hello &lt;there&gt;"));

            // A new entry is appended; the turn's end updates the composer status.
            lead.events().push(LeadEvent::ToolCall {
                turn: Some(1),
                id: "1".into(),
                title: "melissa__device_info".into(),
                status: "running".into(),
                ms: None,
            });
            let frame = next_frame(&mut body).await;
            assert!(frame.contains("<li class=\"entry\" id=\"f-t1\">"));
            assert!(frame.contains("Checking this device"));
            lead.events().push(LeadEvent::TurnFinished {
                turn: 1,
                stop_reason: "end_turn".into(),
            });
            let frame = next_frame(&mut body).await;
            assert!(frame.contains("innerHTML:#composer-status"));
            assert!(frame.contains("Everything is logged."));
            // The tool call the agent never finished stops spinning with the turn.
            assert!(frame.contains("innerHTML:#f-t1"), "{frame}");
            assert!(!frame.contains("icon spin"), "{frame}");

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
            let (app, lead) = app();
            let res = app
                .clone()
                .oneshot(get("/ui/stream?since=0&view=approvals"))
                .await
                .unwrap();
            let mut body = res.into_body();
            lead.events().push(LeadEvent::UserMessage {
                turn: 1,
                text: "Hi".into(),
                attachments: vec![],
                scheduled: None,
            });
            let (c, _rx) = lead.confirmations().request(
                1,
                "install_app",
                &serde_json::json!({"app": "immich"}),
                "because",
            );
            lead.events().push(LeadEvent::ConfirmationNeeded {
                id: c.id,
                task: 1,
                tool: "install_app".into(),
                reason: "because".into(),
                agent: None,
            });
            let frame = next_frame(&mut body).await;
            assert!(!frame.contains("Hi"), "{frame}");
            assert!(frame.contains("Install Immich?"));
            assert!(frame.contains("hx-swap-oob=\"true\""), "waiting count");
        }

        #[tokio::test]
        async fn backends_are_listed_switched_between_turns_and_refused_when_unusable() {
            let (app, lead) = app();
            let (status, body) = send_json(&app, get("/api/lead/backends")).await;
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
                send_json(&app, json_post("/api/lead/backend", r#"{"name":"gone"}"#)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(body["error"].as_str().unwrap().contains("not installed"));
            for bad in [r#"{"name":"nope"}"#, r#"{"name":"echo-b","model":"x"}"#] {
                let (status, _) = send_json(&app, json_post("/api/lead/backend", bad)).await;
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
            }
            let mut cross = json_post("/api/lead/backend", r#"{"name":"echo-b"}"#);
            cross
                .headers_mut()
                .insert(header::ORIGIN, "http://evil.example.com".parse().unwrap());
            assert_eq!(send(&app, cross).await.0, StatusCode::FORBIDDEN);
            assert_eq!(lead.backend_choice(), None);

            // A turn is running (its session never opens): the switch waits for it.
            let (status, _) =
                send_json(&app, json_post("/api/lead/messages", r#"{"text":"hello"}"#)).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            let (status, body) =
                send_json(&app, json_post("/api/lead/backend", r#"{"name":"echo-b"}"#)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["changed"], true);
            assert_eq!(body["after_turn"], true);
            let (_, body) = send_json(&app, get("/api/lead/backends")).await;
            assert_eq!(body["backends"][1]["current"], true);
            assert!(lead
                .events()
                .since(0, 100)
                .events
                .iter()
                .any(|r| matches!(&r.event, LeadEvent::BackendSwitched { backend, .. } if backend == "echo-b")));

            // The chat shows it quietly, and the picker has the new current agent.
            let (_, _, page) = send(&app, get("/")).await;
            assert!(page.contains("Switched to echo-b, picking up where we left off"));
            assert!(
                page.contains("data-filter=\"echo-b\" aria-pressed=\"true\""),
                "{page}"
            );
            // Not installed: never offered, and no setup hints in the picker.
            assert!(!page.contains("data-filter=\"gone\""));
            assert!(!page.contains("isn&#39;t installed") && !page.contains("isn't installed"));

            // The UI form: htmx gets the picker (and the chip) back; a refusal says why.
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
            assert!(body.starts_with("<div class=\"model-picker\""), "{body}");
            assert!(body.contains("hx-swap-oob=\"innerHTML:#model-chip\""));
            assert!(!body.contains("picker-error") && !body.contains("data-error"));
            let (status, _, body) = send(&app, ui_post("gone", true)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body.contains("gone isn&#39;t ready on this device yet.")
                    || body.contains("gone isn't ready on this device yet."),
                "{body}"
            );
            assert!(body.contains("data-error"), "reopens to say why");
            let (status, headers, _) = send(&app, ui_post("echo-b", false)).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/");

            // An agent: any session backend; saved in the registry.
            lead.agents()
                .registry()
                .create("Ada", "stuck", None, "the user")
                .unwrap();
            let (status, body) = send_json(&app, get("/api/agents/ada/backends")).await;
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
                json_post("/api/agents/Ada/backend", r#"{"name":"echo-b"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["changed"], true);
            let (_, agent) = send_json(&app, get("/api/agents/Ada")).await;
            assert_eq!(agent["backend"], "echo-b");
            let (_, _, page) = send(&app, get("/agents/Ada")).await;
            assert!(page.contains("action=\"/ui/agents/"), "{page}");

            // Agents: unknown ones are 404.
            assert_eq!(
                send(&app, get("/api/agents/nobody/backends")).await.0,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                send(
                    &app,
                    json_post("/api/agents/nobody/backend", r#"{"name":"echo-b"}"#)
                )
                .await
                .0,
                StatusCode::NOT_FOUND
            );
        }

        // --- Model picker (2026-11-20) -------------------------------------------------------

        /// What `echo-b` reported: m1 with the levels low and high, m2 without levels.
        fn seed_models(lead: &Lead) {
            use melissa_agent::session::{EffortOption, ModelList, ModelOption};
            let level = |id: &str, label: &str| EffortOption {
                id: id.into(),
                label: label.into(),
            };
            let mut m1 = ModelOption::new("m1", "Model One");
            m1.efforts = vec![level("low", "Low"), level("high", "High")];
            lead.models().put(
                "echo-b",
                &ModelList {
                    current: None,
                    options: vec![m1, ModelOption::new("m2", "Model Two")],
                },
            );
        }

        fn chip(text: &str) -> String {
            format!("<span class=\"chip-text\">{text}</span>")
        }

        #[tokio::test]
        async fn the_model_picker_switches_model_and_reasoning_level() {
            let (app, lead) = app();
            seed_models(&lead);
            let (_, _, page) = get_html(&app, "/").await;
            // Only usable backends; the current one is pressed on the rail and on the chip.
            assert!(
                page.contains("data-filter=\"stuck\" aria-pressed=\"true\""),
                "{page}"
            );
            assert!(page.contains("data-filter=\"echo-b\" aria-pressed=\"false\""));
            assert!(page.contains("data-filter=\"favorites\""));
            assert!(!page.contains("data-filter=\"gone\""), "not installed");
            assert!(page.contains(&chip("stuck")));
            assert!(page.contains("name=\"choice\" value=\"echo-b m1\""));
            // `stuck` reports no levels: no Reasoning column.
            assert!(!page.contains("picker-levels"));

            // Pick a model (htmx): the picker comes back (closed) with the chip out of band.
            let (status, _, body) = send(
                &app,
                htmx_post("/ui/backend", "current=stuck&choice=echo-b+m1"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains(&chip("Model One")), "{body}");
            assert!(body.contains("hx-swap-oob=\"innerHTML:#model-chip\""));
            assert!(body.contains(">Reasoning</h3>"));
            assert!(body.contains("name=\"effort\" value=\"high\""));
            let choice = |lead: &Lead| {
                lead.backend_choice()
                    .map(|c| (c.backend, c.model, c.effort))
                    .unwrap_or_default()
            };
            assert_eq!(choice(&lead), ("echo-b".into(), Some("m1".into()), None));

            // A level for the current model.
            let (_, _, body) = send(
                &app,
                htmx_post("/ui/backend", "current=echo-b+m1&effort=high"),
            )
            .await;
            assert!(body.contains(&chip("Model One · High")), "{body}");
            assert_eq!(choice(&lead).2.as_deref(), Some("high"));
            // A model without that level drops it (the column goes too).
            let (_, _, body) = send(
                &app,
                htmx_post("/ui/backend", "current=echo-b+m1&choice=echo-b+m2"),
            )
            .await;
            assert!(body.contains(&chip("Model Two")), "{body}");
            assert!(!body.contains("picker-levels"));
            assert_eq!(choice(&lead), ("echo-b".into(), Some("m2".into()), None));
            // A level the model doesn't offer is refused, and the picker says why.
            let (_, _, body) = send(
                &app,
                htmx_post("/ui/backend", "current=echo-b+m2&effort=high"),
            )
            .await;
            assert!(body.contains("data-error"), "{body}");
            assert!(body.contains("picker-error"));
            assert_eq!(choice(&lead).1.as_deref(), Some("m2"));

            // Without script: a plain form post, then the page shows it.
            let req = post("/ui/backend")
                .body(Body::from("current=echo-b+m1&effort=low"))
                .unwrap();
            let (status, headers, _) = send(&app, req).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/");
            let (_, _, page) = get_html(&app, "/").await;
            assert!(page.contains(&chip("Model One · Low")), "{page}");
            assert!(page.contains("name=\"choice\" value=\"echo-b m1\" aria-current=\"true\""));
            assert!(page.contains("name=\"effort\" value=\"low\" aria-current=\"true\""));

            // The JSON API takes the level too, and lists it.
            let (status, body) = send_json(
                &app,
                json_post(
                    "/api/lead/backend",
                    r#"{"name":"echo-b","model":"m1","effort":"high"}"#,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["effort"], "high");
            let (_, body) = send_json(&app, get("/api/lead/backends")).await;
            assert_eq!(body["backends"][1]["effort"], "high");
            assert_eq!(body["backends"][1]["models"][0]["efforts"][1]["id"], "high");
            let (status, _) = send_json(
                &app,
                json_post(
                    "/api/lead/backend",
                    r#"{"name":"echo-b","model":"m2","effort":"high"}"#,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

            // A helper: saved in the registry; its chat's picker posts to its own URL.
            let ada = lead
                .agents()
                .registry()
                .create("Ada", "stuck", None, "the user")
                .unwrap()
                .id;
            let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
            assert!(page.contains(&format!("action=\"/ui/agents/{ada}/backend\"")));
            assert!(page.contains("placeholder=\"Message Ada\""));
            let url = format!("/ui/agents/{ada}/backend");
            let (_, _, body) = send(&app, htmx_post(&url, "current=stuck&choice=echo-b+m1")).await;
            assert!(body.contains(&chip("Model One")), "{body}");
            let (_, _, body) = send(&app, htmx_post(&url, "current=echo-b+m1&effort=low")).await;
            assert!(body.contains(&chip("Model One · Low")), "{body}");
            let (_, agent) = send_json(&app, get("/api/agents/Ada")).await;
            assert_eq!(agent["backend"], "echo-b");
            assert_eq!(agent["model"], "m1");
            assert_eq!(agent["effort"], "low");
        }

        /// `echo-b` with a fast mode on m1 only (m2 is a fast model already).
        fn seed_fast_models(lead: &Lead) {
            use melissa_agent::session::{ModelList, ModelOption};
            let mut m1 = ModelOption::new("m1", "Model One");
            m1.fast = true;
            lead.models().put(
                "echo-b",
                &ModelList {
                    current: None,
                    options: vec![m1, ModelOption::new("m2", "Model Two Fast")],
                },
            );
        }

        #[tokio::test]
        async fn the_picker_offers_speed_and_access_only_when_they_apply() {
            let (app, lead) = app();
            seed_fast_models(&lead);
            let (_, _, body) = send(
                &app,
                htmx_post("/ui/backend", "current=stuck&choice=echo-b+m1"),
            )
            .await;
            // m1 has a fast mode: Speed shows; Access doesn't (developer option off).
            assert!(body.contains(">Speed</span>"), "{body}");
            assert!(body.contains("name=\"fast\" value=\"on\" aria-pressed=\"false\""));
            assert!(!body.contains(">Access</span>"));
            let (_, _, body) =
                send(&app, htmx_post("/ui/backend", "current=echo-b+m1&fast=on")).await;
            assert!(
                body.contains("<span class=\"chip-tag\">Fast</span>"),
                "{body}"
            );
            assert!(lead.backend_choice().is_some_and(|c| c.fast));
            // Another model without a fast mode drops it, and the footer goes.
            let (_, _, body) = send(
                &app,
                htmx_post("/ui/backend", "current=echo-b+m1&choice=echo-b+m2"),
            )
            .await;
            assert!(!body.contains("picker-footer"), "{body}");
            assert!(!body.contains("chip-tag"));
            assert!(lead.backend_choice().is_some_and(|c| !c.fast));
            // Asking for Fast there is refused, and the picker says why.
            let (_, _, body) =
                send(&app, htmx_post("/ui/backend", "current=echo-b+m2&fast=on")).await;
            assert!(body.contains("picker-error"), "{body}");
            assert!(lead.backend_choice().is_some_and(|c| !c.fast));

            // Access: refused while the developer option is off.
            let (_, _, body) = send(&app, htmx_post("/ui/backend", "access=full")).await;
            assert!(body.contains("Settings → Developer"), "{body}");
            // The Developer page has the switch; on adds Access to the picker.
            let (_, _, page) = get_html(&app, "/settings/developer").await;
            assert!(page.contains("id=\"full-access\""), "{page}");
            let (status, _, body) = send(
                &app,
                htmx_post("/ui/settings/developer/full-access", "enabled=on"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("checked"), "{body}");
            let (_, _, page) = get_html(&app, "/").await;
            assert!(page.contains(">Access</span>"), "{page}");
            // Full access asks in the popover first (no browser dialog), then marks the chip.
            assert!(page.contains("<summary class=\"seg\">Full access</summary>"));
            assert!(page.contains("Allow full access"));
            assert!(!page.contains("hx-confirm"));
            assert!(!page.contains("chip-warn"));
            // Without script: a plain form post, then the page shows it.
            let req = post("/ui/backend").body(Body::from("access=full")).unwrap();
            let (status, headers, _) = send(&app, req).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/");
            let (_, _, page) = get_html(&app, "/").await;
            assert!(page.contains("chip-warn"), "{page}");
            assert!(page.contains("name=\"access\" value=\"full\" aria-pressed=\"true\""));
            let (_, perms) = send_json(&app, get("/api/settings/permissions")).await;
            assert_eq!(perms["full_access_allowed"], true);
            assert_eq!(perms["full_access"], serde_json::json!(["lead"]));
            // Back to Ask.
            send(&app, htmx_post("/ui/backend", "access=ask")).await;
            let (_, _, page) = get_html(&app, "/").await;
            assert!(!page.contains("chip-warn"));

            // A helper, through the API: Full needs `confirm`.
            lead.agents()
                .registry()
                .create("Ada", "stuck", None, "the user")
                .unwrap();
            let (status, _) = send_json(
                &app,
                json_post(
                    "/api/settings/permissions/access",
                    r#"{"agent":"Ada","full":true}"#,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            let (status, body) = send_json(
                &app,
                json_post(
                    "/api/settings/permissions/access",
                    r#"{"agent":"Ada","full":true,"confirm":true}"#,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            // Switching the option off puts everyone back on Ask and hides Access.
            send(&app, htmx_post("/ui/settings/developer/full-access", "")).await;
            let (_, perms) = send_json(&app, get("/api/settings/permissions")).await;
            assert_eq!(perms["full_access"], serde_json::json!([]));
            let (_, _, page) = get_html(&app, "/").await;
            assert!(!page.contains(">Access</span>"));
        }

        // --- Deleting a helper (2026-11-20) --------------------------------------------------

        /// One workspace, `nextcloud`, looked after by the agent set in it (None: nobody).
        #[derive(Default)]
        struct OwnedBy(std::sync::Mutex<Option<String>>);

        impl melissa_agent::tools::WorkspaceStore for OwnedBy {
            fn workspaces(
                &self,
            ) -> BoxFuture<'_, Result<Vec<melissa_agent::tools::WorkspaceInfo>, String>>
            {
                let owner = match self.0.lock().unwrap().clone() {
                    Some(id) => melissa_agent::tools::Owner::Agent(id),
                    None => melissa_agent::tools::Owner::Unassigned,
                };
                Box::pin(async move {
                    Ok(vec![melissa_agent::tools::WorkspaceInfo {
                        name: "nextcloud".into(),
                        owner,
                        device: Some("d-0a1b2c3d".into()),
                        assigned_by: None,
                        assigned_at: None,
                        legacy_owner: None,
                    }])
                })
            }
            fn assign<'a>(
                &'a self,
                _: &'a str,
                _: &'a str,
                _: &'a str,
            ) -> BoxFuture<'a, Result<(), String>> {
                Box::pin(async { Err("no".into()) })
            }
            fn force_delete<'a>(
                &'a self,
                _: &'a str,
            ) -> BoxFuture<'a, Result<Vec<String>, String>> {
                Box::pin(async { Err("no".into()) })
            }
        }

        #[tokio::test]
        async fn helpers_are_deleted_with_their_chat_box_and_schedules_but_not_with_apps() {
            use melissa_agent::{
                lead::schedules::{Creator, ScheduleInput, Schedules, TimeZone},
                tools::permissions::Principal,
            };
            let store = Arc::new(OwnedBy::default());
            let (app, lead) = app_full(None, Some(store.clone()), false);
            let sched = lead.start_schedules(Schedules::in_memory(TimeZone::UTC));
            let reg = lead.agents().registry().clone();
            let ada = reg.create("Ada", "stuck", None, "the user").unwrap().id;
            reg.create("Bo", "stuck", None, "the user").unwrap();
            *store.0.lock().unwrap() = Some(ada.clone());
            let schedule = |agent: Principal| ScheduleInput {
                name: "Backup".into(),
                agent,
                instruction: "Check the backups".into(),
                cron: "0 3 * * *".into(),
                enabled: true,
                max_minutes: None,
            };
            let s = sched.schedules();
            s.create(&schedule(Principal::Agent(ada.clone())), &Creator::Owner)
                .unwrap();
            s.create(&schedule(Principal::Lead), &Creator::Owner)
                .unwrap();
            // Its file box and its chat.
            let owner = Principal::Agent(ada.clone());
            let box_dir = lead.files().ensure_dir(&owner).unwrap();
            std::fs::write(box_dir.join("notes.txt"), "hi").unwrap();
            let agent_dir = reg.agent_dir(&ada);
            std::fs::create_dir_all(&agent_dir).unwrap();
            std::fs::write(agent_dir.join("transcript.jsonl"), "{}\n").unwrap();

            // Settings → Agents: Melissa (no Delete) and the helpers.
            let (status, _, page) = get_html(&app, "/settings/agents").await;
            assert_eq!(status, StatusCode::OK);
            assert!(page.contains("Lead agent"), "{page}");
            assert!(page.contains(&format!("action=\"/ui/settings/agents/{ada}/delete\"")));
            assert!(page.contains("Delete Ada? Its chat, file box and schedules go with it."));
            assert_eq!(
                page.matches("action=\"/ui/settings/agents/").count(),
                2,
                "only the helpers"
            );

            // Refused while it looks after an app: says which, nothing removed.
            let del = format!("/ui/settings/agents/{ada}/delete");
            let (_, _, body) = send(&app, htmx_post(&del, "")).await;
            assert!(
                body.contains(
                    "Ada still looks after nextcloud. Reassign or remove its apps first."
                ),
                "{body}"
            );
            let req = axum::http::Request::delete("/api/agents/Ada")
                .body(Body::empty())
                .unwrap();
            assert_eq!(send(&app, req).await.0, StatusCode::CONFLICT);
            assert!(lead.agents().get("Ada").is_some());
            assert_eq!(s.list().len(), 2);
            assert!(box_dir.exists());

            // The app went to someone else: now it goes, with what was its own.
            *store.0.lock().unwrap() = None;
            let (_, _, body) = send(&app, htmx_post(&del, "")).await;
            assert!(body.contains("Deleted Ada."), "{body}");
            assert!(!body.contains(&format!("{ada}/delete")));
            assert!(lead.agents().get("Ada").is_none());
            let left: Vec<Principal> = s.list().into_iter().map(|s| s.agent).collect();
            assert_eq!(left, [Principal::Lead], "its schedule only");
            assert!(!box_dir.exists());
            assert!(!agent_dir.exists());
            assert!(lead.events().since(0, 100).events.iter().any(|r| matches!(
                &r.event,
                LeadEvent::AgentDeleted { name, .. } if name == "Ada"
            )));
            // Its id is never reused.
            let cy = reg.create("Cy", "stuck", None, "the user").unwrap().id;
            assert_ne!(cy, ada);

            // The JSON API.
            let delete = |uri: &str| {
                axum::http::Request::delete(uri)
                    .body(Body::empty())
                    .unwrap()
            };
            let (status, body) = send_json(&app, delete("/api/agents/Bo")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["name"], "Bo");
            assert_eq!(
                send(&app, get("/api/agents/Bo")).await.0,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                send(&app, delete("/api/agents/Bo")).await.0,
                StatusCode::NOT_FOUND
            );
            // Melissa herself can't be deleted.
            let (status, body) = send_json(&app, delete("/api/agents/melissa")).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(body["error"].as_str().unwrap().contains("can't be deleted"));
            // Cross-site: refused.
            let mut cross = delete("/api/agents/Cy");
            cross
                .headers_mut()
                .insert(header::ORIGIN, "http://evil.example.com".parse().unwrap());
            assert_eq!(send(&app, cross).await.0, StatusCode::FORBIDDEN);
            // Without script: a plain post redirects back to the list.
            let req = post(&format!("/ui/settings/agents/{cy}/delete"))
                .body(Body::empty())
                .unwrap();
            let (status, headers, _) = send(&app, req).await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[header::LOCATION], "/settings/agents");
            assert!(lead.agents().get("Cy").is_none());
        }
    }

    #[tokio::test]
    async fn the_built_in_echo_is_not_offered() {
        let get = |app: Router| async move {
            send(
                &app,
                axum::http::Request::get("/tasks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .2
        };
        // Only the built-in echo: nothing to offer, point to Providers.
        let (app, _) = self::app();
        let body = get(app).await;
        assert!(!body.contains("<option"));
        assert!(body.contains("No agent to try yet."));
        assert!(body.contains("href=\"/settings/providers\""));
        // An echo the user configured is offered; the built-in one still isn't.
        let board = TaskBoard::new(
            vec![
                Arc::new(EchoBackend::new()),
                Arc::new(melissa_agent::EchoSessionBackend::new(
                    &melissa_agent::EchoSpec {
                        name: "echo-b".into(),
                    },
                )),
            ],
            Duration::from_secs(5),
            None,
        );
        let body = get(router(board)).await;
        assert!(body.contains("<option value=\"echo-b\">echo-b</option>"));
        assert!(!body.contains("<option value=\"echo\""));
    }

    #[tokio::test]
    async fn task_page_renders_the_reply_as_markdown() {
        let (app, board) = app();
        let id = board
            .dispatch("echo", "**bold** <script>x</script>")
            .unwrap();
        board.wait(id, Duration::from_secs(5)).await.unwrap();
        let (status, _, body) = send(
            &app,
            axum::http::Request::get(format!("/tasks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<div class=\"md\">"));
        assert!(body.contains("<strong>bold</strong>"));
        assert!(!body.contains("<script>x"));
        assert!(body.contains("Done"));
        assert!(body.contains("aria-label=\"Breadcrumb\""));
        // Finished: the region no longer polls.
        assert!(!body.contains(&format!("hx-get=\"/tasks/{id}/fragment\"")));
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
