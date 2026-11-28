//! Open Melissa from other devices (MVP #1, owner 2026-11-28; DECISIONS "HTTPS access from
//! other devices"; research `docs/design/HTTPS-ACCESS-2026-11-28.md`).
//!
//! Melissa's own tailscaled serves the web UI over HTTPS on this node's MagicDNS name
//! (`tailscale serve --bg --https=443 http://127.0.0.1:<proxy port>`): tailscaled terminates
//! TLS and renews the certificate, and forwards to the server's second loopback listener,
//! the only one that believes `X-Forwarded-*` (`melissa_web::front`). The server runs the CLI
//! as tailscaled's operator.
//!
//! - **Off by default.** The owner turns it on in Settings → Tailscale with their password
//!   ([`TailscaleControl::serve_on`]), optionally renaming the node first: the name ends up in
//!   public certificate transparency logs. Audited `tailscale.serve_on` / `tailscale.serve_off`.
//! - **HTTPS off on the tailnet.** The CLI prints a tailscale.com page to allow it and waits;
//!   the page goes to the owner's browser only (like the sign-in page), the CLI keeps waiting
//!   in the background and finishes by itself ([`Serve::NeedsHttps`]).
//! - **Every start** re-applies it while the setting says so, and removes a leftover
//!   configuration while it doesn't ([`TailscaleControl::resume_serve`]). Turn off and Disable
//!   remove it (`tailscale serve reset`; Melissa's tailscaled serves nothing else).
//! - **Agents** see the state read-only (`tailscale_status`), never a way to turn it on.
//! - **Never Funnel**: a configuration that opens it to the internet is not counted as on and
//!   is replaced. The melissa uid can still rewrite it (like Q108); see the research note.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot},
};

use super::{update_file, valid_node_name, LoginUrl, State, TailscaleControl, TailscaleError};

/// "Open Melissa from your other devices", as the page and the agents see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Serve {
    /// Not offered: the host runs Tailscale, Melissa's is off or not connected yet, or this
    /// server has no listener for it.
    Unavailable,
    Off,
    /// The tailnet has HTTPS off; the owner allows it on this tailscale.com page (never shown
    /// to agents). `None`: Tailscale gave no page.
    NeedsHttps(Option<LoginUrl>),
    /// Being set up (or set up again after a start).
    TurningOn,
    /// On at this address (`https://<MagicDNS name>`).
    On {
        url: String,
    },
}

impl Serve {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Off => "off",
            Self::NeedsHttps(_) => "needs_https",
            Self::TurningOn => "turning_on",
            Self::On { .. } => "on",
        }
    }
}

/// Why turning it on didn't work (shown once, then the row is Off again).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeFailure {
    /// The CLI's exit, for the log and "Show details" (never Tailscale's own output).
    pub detail: String,
}

/// What `tailscale serve` did.
pub enum ServeStart {
    /// Configured.
    Done,
    /// Waiting for the tailnet to allow HTTPS: the page to allow it, the CLI's end, and a
    /// sender whose drop stops the CLI.
    Waiting {
        url: Option<LoginUrl>,
        done: oneshot::Receiver<Result<(), String>>,
        cancel: oneshot::Sender<()>,
    },
}

/// What is going on in the background.
pub(crate) enum Pending {
    Idle,
    Applying {
        id: u64,
    },
    Waiting {
        id: u64,
        url: Option<LoginUrl>,
        /// Dropped (cancel, Turn off, Disable, a new try): the waiting CLI is stopped.
        _cancel: oneshot::Sender<()>,
    },
    Failed(ServeFailure),
}

impl Pending {
    fn id(&self) -> Option<u64> {
        match self {
            Self::Applying { id } | Self::Waiting { id, .. } => Some(*id),
            _ => None,
        }
    }
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// How a serve configuration (`tailscale serve status --json`) looks to Melissa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeConfig {
    /// HTTPS on 443 to `target` and nothing else (no Funnel, no other port).
    pub ours: bool,
    /// Nothing configured.
    pub empty: bool,
}

/// Read `tailscale serve status --json` against Melissa's `target`.
pub fn parse_serve_config(json: &str, target: &str) -> ServeConfig {
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap_or_default();
    let nonempty = |key: &str| v[key].as_object().is_some_and(|o| !o.is_empty());
    let empty = !nonempty("TCP") && !nonempty("Web") && !nonempty("Services");
    let funnel = v["AllowFunnel"]
        .as_object()
        .is_some_and(|o| o.values().any(|b| b == true));
    let only_443 = v["TCP"]
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key("443") && o["443"]["HTTPS"] == true);
    let proxied = v["Web"].as_object().is_some_and(|web| {
        !web.is_empty()
            && web.iter().all(|(host, site)| {
                host.ends_with(":443")
                    && site["Handlers"].as_object().is_some_and(|h| {
                        h.len() == 1 && h.get("/").is_some_and(|r| r["Proxy"] == target)
                    })
            })
    });
    ServeConfig {
        ours: only_443 && proxied && !funnel,
        empty,
    }
}

/// The first tailscale.com page in a line of the CLI's output.
fn page_in(line: &str) -> Option<LoginUrl> {
    line.split_whitespace().find_map(LoginUrl::parse)
}

/// Run `tailscale serve --bg --https=443 <target>` (the real CLI): wait until it is done,
/// or until it prints a page to allow HTTPS (then it keeps waiting in the background), at
/// most `timeout`. Its output is read for that page only and never kept or logged.
pub(super) async fn run_serve(
    mut cmd: tokio::process::Command,
    target: &str,
    timeout: Duration,
) -> Result<ServeStart, String> {
    cmd.args(["serve", "--bg", "--https=443", target])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run tailscale: {}", e.kind()))?;
    let (tx, mut lines) = mpsc::channel::<String>(64);
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(out).lines();
            while let Ok(Some(l)) = r.next_line().await {
                if tx.send(l).await.is_err() {
                    break;
                }
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(async move {
            let mut r = BufReader::new(err).lines();
            while let Ok(Some(l)) = r.next_line().await {
                if tx.send(l).await.is_err() {
                    break;
                }
            }
        });
    }
    let exited = |s: std::io::Result<std::process::ExitStatus>| match s {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("`tailscale serve` failed ({s})")),
        Err(e) => Err(format!("`tailscale serve`: {}", e.kind())),
    };
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let url = loop {
        tokio::select! {
            s = child.wait() => return exited(s).map(|()| ServeStart::Done),
            Some(line) = lines.recv() => {
                if let Some(url) = page_in(&line) {
                    break Some(url);
                }
            }
            _ = &mut deadline => break None,
        }
    };
    let (done_tx, done) = oneshot::channel();
    let (cancel, mut cancelled) = oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                s = child.wait() => {
                    let _ = done_tx.send(exited(s));
                    break;
                }
                _ = &mut cancelled => {
                    let _ = child.kill().await;
                    break;
                }
                // Keep reading so the CLI never blocks on a full pipe.
                Some(_) = lines.recv() => {}
            }
        }
    });
    Ok(ServeStart::Waiting { url, done, cancel })
}

impl TailscaleControl {
    /// The saved "serve" setting (off when unreadable).
    pub(crate) fn serve_setting(&self) -> bool {
        super::read_file(&self.inner.setting).is_ok_and(|f| f.serve)
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        // A panic while holding this lock can't leave it inconsistent (plain assignments).
        self.inner
            .serve
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn serve_offered(&self) -> bool {
        self.inner.management == super::Management::Melissa && self.inner.serve_target.is_some()
    }

    /// The state now. While the setting says on and the configuration isn't Melissa's (a
    /// restart, someone changed it), it is set up again.
    pub async fn serve_state(&self) -> Serve {
        if !self.serve_offered() {
            return Serve::Unavailable;
        }
        let State::Connected(node) = self.state().await else {
            return Serve::Unavailable;
        };
        match &*self.pending() {
            Pending::Waiting { url, .. } => return Serve::NeedsHttps(url.clone()),
            Pending::Applying { .. } => return Serve::TurningOn,
            Pending::Idle | Pending::Failed(_) => {}
        }
        if !self.serve_setting() {
            return Serve::Off;
        }
        let ours = match (&self.inner.serve_target, &node.dns_name) {
            (Some(target), Some(_)) => self
                .inner
                .cli
                .serve_status()
                .await
                .is_ok_and(|j| parse_serve_config(&j, target).ours),
            _ => false,
        };
        match node.dns_name {
            Some(dns) if ours => Serve::On {
                url: format!("https://{dns}"),
            },
            _ => {
                self.spawn_apply("melissa");
                Serve::TurningOn
            }
        }
    }

    /// Why the last try failed, once (the page shows it, then the row is Off).
    pub fn take_serve_failure(&self) -> Option<ServeFailure> {
        let mut p = self.pending();
        match std::mem::replace(&mut *p, Pending::Idle) {
            Pending::Failed(f) => Some(f),
            other => {
                *p = other;
                None
            }
        }
    }

    /// Turn it on (the owner, password checked by the caller): rename the node first when
    /// `name` differs, then set up `tailscale serve` in the background. Answers once it is
    /// on, waiting for HTTPS to be allowed, or still at it (after a few seconds).
    pub async fn serve_on(&self, actor: &str, name: Option<&str>) -> Result<Serve, TailscaleError> {
        if self.inner.management != super::Management::Melissa {
            return Err(TailscaleError::NotManaged);
        }
        if self.inner.serve_target.is_none() {
            return Err(TailscaleError::Not("without a listener for it"));
        }
        {
            let _one = self.inner.lock.lock().await;
            self.forget();
            let State::Connected(node) = self.state().await else {
                return Err(TailscaleError::Not("not connected"));
            };
            if let Some(raw) = name {
                let name = valid_node_name(raw).ok_or(TailscaleError::BadName)?;
                if name != node.name {
                    self.inner
                        .cli
                        .set_hostname(&name)
                        .await
                        .map_err(TailscaleError::Failed)?;
                    update_file(&self.inner.setting, |f| f.name = Some(name.clone()))?;
                    self.audit(
                        "settings.tailscale",
                        actor,
                        &format!("renamed from {} to {name}", node.name),
                    );
                    // The MagicDNS name follows (the certificate is for the new one).
                    let _ = self
                        .wait_for(|d| d.node.as_ref().is_some_and(|n| n.name == name))
                        .await;
                    self.forget();
                }
            }
        }
        *self.pending() = Pending::Idle;
        self.spawn_apply(actor);
        let until = Instant::now() + self.inner.wait;
        while matches!(&*self.pending(), Pending::Applying { .. }) && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(self.serve_state().await)
    }

    /// Stop waiting for HTTPS to be allowed (the waiting CLI stops; nothing was set up).
    pub fn serve_cancel(&self) {
        let mut p = self.pending();
        if matches!(&*p, Pending::Waiting { .. } | Pending::Applying { .. }) {
            *p = Pending::Idle;
        }
    }

    /// Turn it off (no confirmation: easy to turn on again).
    pub async fn serve_off(&self, actor: &str) -> Result<Serve, TailscaleError> {
        if self.inner.management != super::Management::Melissa {
            return Err(TailscaleError::NotManaged);
        }
        {
            let _one = self.inner.lock.lock().await;
            self.serve_off_locked(actor).await;
        }
        Ok(self.serve_state().await)
    }

    /// Turn off and remove the configuration (also part of Disable). Holds the control's lock.
    pub(super) async fn serve_off_locked(&self, actor: &str) {
        let was_pending = !matches!(
            std::mem::replace(&mut *self.pending(), Pending::Idle),
            Pending::Idle | Pending::Failed(_)
        );
        if !(self.serve_setting() || was_pending) {
            return;
        }
        let reset = match self.inner.cli.serve_reset().await {
            Ok(()) => "removed".to_string(),
            Err(e) => {
                tracing::warn!("tailscale serve reset: {e}");
                format!("not removed: {e}")
            }
        };
        if let Err(e) = update_file(&self.inner.setting, |f| f.serve = false) {
            tracing::error!("{e}; the HTTPS address may come back after a restart");
        }
        self.audit("tailscale.serve_off", actor, &reset);
        self.forget();
    }

    /// At each start: once Melissa's Tailscale is connected, set the HTTPS address up again
    /// when the setting says so, and remove a configuration that is left over when it
    /// doesn't. Runs until that is done or Tailscale is off.
    pub async fn resume_serve(&self) {
        let Some(target) = self.inner.serve_target.clone() else {
            return;
        };
        if self.inner.management != super::Management::Melissa {
            return;
        }
        loop {
            if !self.enabled() {
                return;
            }
            if matches!(self.state().await, State::Connected(_)) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
        let config = match self.inner.cli.serve_status().await {
            Ok(j) => parse_serve_config(&j, &target),
            Err(e) => {
                tracing::warn!("tailscale serve status: {e}");
                return;
            }
        };
        if self.serve_setting() {
            if !config.ours {
                tracing::info!("setting up the HTTPS address on the tailnet again");
                self.spawn_apply("melissa");
            }
        } else if !config.empty {
            tracing::info!("removing a tailscale serve configuration Melissa didn't set up");
            let _one = self.inner.lock.lock().await;
            if let Err(e) = self.inner.cli.serve_reset().await {
                tracing::warn!("tailscale serve reset: {e}");
            }
            self.audit(
                "tailscale.serve_off",
                "melissa",
                "leftover configuration removed",
            );
        }
    }

    /// Start setting it up in the background unless that already runs.
    fn spawn_apply(&self, actor: &str) {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        {
            let mut p = self.pending();
            if p.id().is_some() {
                return;
            }
            *p = Pending::Applying { id };
        }
        let (this, actor) = (self.clone(), actor.to_string());
        tokio::spawn(async move { this.apply(id, &actor).await });
    }

    async fn apply(&self, id: u64, actor: &str) {
        let Some(target) = self.inner.serve_target.clone() else {
            return;
        };
        // Melissa's tailscaled serves this alone: start from nothing (never a Funnel).
        if let Err(e) = self.inner.cli.serve_reset().await {
            tracing::warn!("tailscale serve reset: {e}");
        }
        let result = match self.inner.cli.serve_start(&target).await {
            Ok(ServeStart::Done) => Ok(()),
            Ok(ServeStart::Waiting { url, done, cancel }) => {
                {
                    let mut p = self.pending();
                    if p.id() != Some(id) {
                        return; // cancelled meanwhile: `cancel` drops and the CLI stops
                    }
                    *p = Pending::Waiting {
                        id,
                        url,
                        _cancel: cancel,
                    };
                }
                match done.await {
                    Ok(r) => r,
                    Err(_) => return, // stopped
                }
            }
            Err(e) => Err(e),
        };
        self.finish(id, actor, result);
    }

    fn finish(&self, id: u64, actor: &str, result: Result<(), String>) {
        let mut p = self.pending();
        if p.id() != Some(id) {
            return;
        }
        match result {
            Ok(()) => match update_file(&self.inner.setting, |f| f.serve = true) {
                Ok(()) => {
                    *p = Pending::Idle;
                    drop(p);
                    let target = self.inner.serve_target.as_deref().unwrap_or("?");
                    self.audit(
                        "tailscale.serve_on",
                        actor,
                        &format!(
                            "HTTPS on the tailnet for node {} → {target}",
                            self.node_name()
                        ),
                    );
                }
                Err(e) => {
                    *p = Pending::Failed(ServeFailure {
                        detail: e.to_string(),
                    });
                }
            },
            Err(detail) => {
                tracing::warn!("tailscale serve: {detail}");
                *p = Pending::Failed(ServeFailure {
                    detail: detail.clone(),
                });
                drop(p);
                let _ = update_file(&self.inner.setting, |f| f.serve = false);
                self.audit(
                    "tailscale.serve_off",
                    actor,
                    &format!("didn't turn on: {detail}"),
                );
            }
        }
        self.forget();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "http://127.0.0.1:8082";

    #[test]
    fn serve_configs() {
        let ours = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"melissa-box.example-tailnet.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:8082"}}}}}"#;
        assert_eq!(
            parse_serve_config(ours, TARGET),
            ServeConfig {
                ours: true,
                empty: false
            }
        );
        assert!(parse_serve_config("{}", TARGET).empty);
        assert!(parse_serve_config("", TARGET).empty);
        assert!(
            !parse_serve_config(ours, "http://127.0.0.1:8080").ours,
            "another target"
        );
        let funnel = ours.replace(
            "}}}}}",
            r#"}}}},"AllowFunnel":{"melissa-box.example-tailnet.ts.net:443":true}}"#,
        );
        assert!(!parse_serve_config(&funnel, TARGET).ours, "never Funnel");
        let more = ours.replace(
            r#""TCP":{"#,
            r#""TCP":{"22":{"TCPForward":"127.0.0.1:22"},"#,
        );
        assert!(!parse_serve_config(&more, TARGET).ours, "another port too");
        let path = ours.replace(r#""/":"#, r#""/x":{"Path":"/"},"/":"#);
        assert!(
            !parse_serve_config(&path, TARGET).ours,
            "another handler too"
        );
    }

    /// A stand-in for the CLI: a shell script.
    fn script(tag: &str, body: &str) -> tokio::process::Command {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::scratch(tag);
        let path = dir.join("tailscale");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        tokio::process::Command::new(path)
    }

    #[tokio::test]
    async fn the_real_cli_runner() {
        let t = Duration::from_secs(5);
        let done = run_serve(script("serve-done", "echo ok; exit 0"), TARGET, t).await;
        assert!(matches!(done, Ok(ServeStart::Done)));
        let failed = run_serve(script("serve-fail", "echo nope >&2; exit 3"), TARGET, t).await;
        let Err(e) = failed else { panic!("an error") };
        assert!(!e.contains("nope"), "the CLI's output is never quoted: {e}");

        let marker = crate::test_support::scratch("serve-wait-marker").join("ran-on");
        let body = format!(
            "echo 'Serve is not enabled on your tailnet.'\necho 'To enable, visit:'\n\
             echo '         https://login.tailscale.com/f/serve?node=n0000000000001'\n\
             sleep 3\ntouch {}",
            marker.display()
        );
        let waiting = run_serve(script("serve-wait", &body), TARGET, t).await;
        let Ok(ServeStart::Waiting { url, cancel, .. }) = waiting else {
            panic!("waiting")
        };
        assert!(url.is_some());
        drop(cancel);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!marker.exists(), "stopped when cancelled");
    }

    #[test]
    fn the_allow_https_page_is_found_in_the_cli_output() {
        let url = page_in("         https://login.tailscale.com/f/serve?node=n0000000000001");
        assert_eq!(
            url.as_ref().map(LoginUrl::expose),
            Some("https://login.tailscale.com/f/serve?node=n0000000000001")
        );
        assert!(page_in(
            "Available within your tailnet: https://melissa-box.example-tailnet.ts.net/"
        )
        .is_none());
        assert!(page_in("Serve is not enabled on your tailnet.").is_none());
    }
}
