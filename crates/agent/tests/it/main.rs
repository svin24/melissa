//! melissa-agent's integration tests, one binary (each test binary links the crate again):
//!
//! - `sandbox`: a sandboxed shell script checked from the outside (files, processes, seccomp).
//! - `egress`: deny-by-default network, end to end through the real proxy and netbridge.
//! - `escape`: the adversarial sandbox escape suite (`just escape-test`).
//! - `acp_live`: live checks against a real ACP agent, ignored by default.
//!
//! The sandbox, egress and escape tests need bubblewrap and unprivileged user namespaces.
//! Without them they print SKIP and pass, so local development works on any host; with
//! `MELISSA_SANDBOX_REQUIRED=1` (or the escape gate's `MELISSA_ESCAPE_REQUIRED=1`) they fail
//! instead (see `support::sandbox_host`).

// Test-only binary: panicking on setup failure is the intended behaviour.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod acp_live;
mod egress;
mod escape;
mod sandbox;
mod support;
