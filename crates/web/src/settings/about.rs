//! Settings → About (2026-11-22, owner): Melissa's version, her public pages, Sponsor, her
//! license and the credits. Everyone signed in may open it. Every text is built into the binary
//! (no network); only the Rust crates' license report is read from the image, where it ships.

use std::path::Path;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

use super::pages::{self, AboutView, Notice};
use crate::ui::Ui;

/// Melissa's license (the repository's `LICENSE`).
const LICENSE: &str = include_str!("../../../../LICENSE");
/// The credits and third-party notices (the repository's `ACKNOWLEDGMENTS.md`), shown whole.
const ACKNOWLEDGMENTS: &str = include_str!("../../../../ACKNOWLEDGMENTS.md");

/// What Melissa's pages bundle (see `crates/web/assets/README.md`), with the license texts
/// that ship next to them in `assets/`.
const NOTICES: [Notice; 6] = [
    Notice {
        name: "htmx 2.0.4",
        license: "BSD Zero Clause License",
        text: None,
    },
    Notice {
        name: "htmx SSE extension 2.2.4",
        license: "BSD Zero Clause License",
        text: Some(include_str!("../../assets/htmx-ext-sse.LICENSE.txt")),
    },
    Notice {
        name: "Atkinson Hyperlegible Next",
        license: "SIL Open Font License 1.1",
        text: Some(include_str!(
            "../../assets/fonts/AtkinsonHyperlegibleNext-OFL.txt"
        )),
    },
    Notice {
        name: "Bricolage Grotesque",
        license: "SIL Open Font License 1.1",
        text: Some(include_str!(
            "../../assets/fonts/BricolageGrotesque-OFL.txt"
        )),
    },
    Notice {
        name: "Grok and Goose logos (LobeHub Icons)",
        license: "MIT License",
        text: Some(include_str!("../../assets/logos/LobeHub-icons-LICENSE.txt")),
    },
    Notice {
        name: "OpenCode, Cursor and Cline logos (Simple Icons)",
        license: "CC0 1.0",
        text: None,
    },
];

/// Where the container image puts the Rust crates' license report (cargo-about, see
/// `packaging/container/Containerfile`). Other installs have none.
const RUST_REPORT: &str = "/usr/share/licenses/melissa/THIRD-PARTY-LICENSES.md";

fn view(ui: &Ui) -> AboutView {
    let release = &ui.settings.parts().release;
    AboutView {
        version: release.version.clone(),
        revision: release.revision.clone(),
        license: LICENSE,
        credits: ACKNOWLEDGMENTS,
        notices: &NOTICES,
        rust_report: Path::new(RUST_REPORT).is_file(),
        device: super::device::this_device(ui),
    }
}

/// The page with "This device" as given (a form's answer without script).
pub(super) async fn page_with_device(ui: &Ui, device: pages::ThisDeviceView) -> maud::Markup {
    let mut v = view(ui);
    v.device = Some(device);
    ui.framed(|f| pages::about_page(f, &v)).await
}

/// The value of About's row in the phone's Settings list.
pub(super) fn version(ui: &Ui) -> String {
    ui.settings.parts().release.version.clone()
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&ui);
    ui.framed(|f| pages::about_page(f, &v))
        .await
        .into_response()
}

/// `GET /settings/about/rust-crates`: the report as plain text, or 404 where none ships.
pub(super) async fn rust_licenses() -> Response {
    report(Path::new(RUST_REPORT)).await
}

async fn report(path: &Path) -> Response {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "No license report on this install.").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_texts_are_built_in() {
        assert!(LICENSE.starts_with("MIT License"));
        assert!(ACKNOWLEDGMENTS.contains("T3 Code"));
        for n in &NOTICES {
            if let Some(t) = n.text {
                assert!(t.len() > 100, "{}", n.name);
            }
        }
    }

    #[tokio::test]
    async fn the_report_is_plain_text_or_missing() {
        let dir = std::env::temp_dir().join(format!("melissa-about-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("THIRD-PARTY-LICENSES.md");
        std::fs::write(&file, "# Third-party licenses\n").unwrap();
        let res = report(&file).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let res = report(&dir.join("missing.md")).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
