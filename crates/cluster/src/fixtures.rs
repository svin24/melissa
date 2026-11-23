//! Test-only example specs.

use melissa_appspec::AppSpec;

/// A real web app as an example: a generated admin password shown as setup credentials, a
/// host-checking app (`device_hosts`), an entrypoint hook file and an HTTP probe that sends
/// the `Host` header. No `isolation`: it gets the default (gVisor) like any spec.
pub(crate) fn nextcloud() -> AppSpec {
    AppSpec::from_text(include_str!("../tests/fixtures/nextcloud.json"))
        .unwrap_or_else(|e| panic!("tests/fixtures/nextcloud.json: {e}"))
}
