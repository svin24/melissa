//! Web UI: server-rendered with maud, htmx embedded in the binary (no CDN).
//!
//! UI rule: only components defined in this crate may be rendered. Agents never
//! generate markup; they select components and fill manifests. Skeleton only.

use axum::{extract::Path, http::header, response::IntoResponse, routing::get};
use maud::{html, Markup, DOCTYPE};
use rust_embed::RustEmbed;

pub use axum::Router;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
}

pub async fn serve(listener: tokio::net::TcpListener, app: Router) -> std::io::Result<()> {
    axum::serve(listener, app).await
}

async fn index() -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                title { "Melissa" }
                script src="/assets/htmx.min.js" {}
            }
            body { h1 { "Melissa" } p { "Queen bee is up." } }
        }
    }
}

async fn asset(Path(path): Path<String>) -> impl IntoResponse {
    match Assets::get(&path) {
        Some(file) => {
            let mime = mime_for(&path);
            ([(header::CONTENT_TYPE, mime)], file.data.into_owned()).into_response()
        }
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

fn mime_for(path: &str) -> &'static str {
    if path.ends_with(".js") {
        "text/javascript"
    } else if path.ends_with(".css") {
        "text/css"
    } else {
        "application/octet-stream"
    }
}
