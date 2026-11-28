//! The agents' Tailscale tools: `tailscale_status` (read only) and `enable_tailscale`, which
//! asks the owner every time ([`Tool::owner_approval`]: no "Always allow"). Agents never get
//! the sign-in URL: the owner signs in from Settings → Tailscale, and nothing here returns it
//! ([`State`] doesn't hold it). Turning Tailscale off is the owner's, on the Settings page.

use melissa_agent::{
    tools::{Effect, Labels, TaskContext, Tool, ToolError, ToolSpec},
    BoxFuture,
};
use serde_json::{json, Value};

use super::{Management, Serve, State, TailscaleControl, TailscaleError};

/// The tool result for a state: never a sign-in URL.
pub fn report(control: &TailscaleControl, state: &State) -> Value {
    let mut out = json!({
        "state": state.as_str(),
        "summary": state.sentence(),
        "managed_by": match control.management() {
            Management::Melissa => "melissa",
            Management::Host => "host",
            Management::Unavailable => "none",
        },
    });
    match state {
        State::Connected(n) => {
            out["node"] = json!({
                "name": n.name,
                "dns_name": n.dns_name,
                "ipv4": n.ipv4.map(|ip| ip.to_string()),
                "ipv6": n.ipv6.map(|ip| ip.to_string()),
            });
        }
        State::NotManaged if control.management() == Management::Host => {
            out["note"] = json!(
                "The host runs Tailscale itself; Melissa uses it as it is (Settings → Apps \
                 decides whether tailnet devices may open apps)."
            );
        }
        State::NotManaged => {
            out["note"] =
                json!("This install has no Tailscale of Melissa's; the host's setup decides.");
        }
        State::Off => {
            out["next"] = json!(
                "enable_tailscale (the user approves it), then the user signs in from \
                 Settings → Tailscale."
            );
        }
        State::SignInNeeded => {
            out["next"] = json!(
                "The user signs in from Settings → Tailscale (Sign in). You never see the \
                 sign-in link; don't ask for it."
            );
        }
        _ => {}
    }
    out
}

fn no_args(args: &Value) -> Result<(), ToolError> {
    match args.as_object() {
        Some(o) if o.is_empty() => Ok(()),
        Some(o) => Err(ToolError::BadArgs(format!(
            "unknown argument `{}`",
            o.keys().next().map_or("", String::as_str)
        ))),
        None if args.is_null() => Ok(()),
        None => Err(ToolError::BadArgs("arguments must be an object".into())),
    }
}

/// `tailscale_status`: whether this device is on a tailnet through Melissa's Tailscale.
pub struct TailscaleStatus(pub TailscaleControl);

impl Tool for TailscaleStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tailscale_status",
            description: "Melissa's own Tailscale on this device (Settings → Tailscale): \
                          `not_managed` (the host runs Tailscale itself, or this install has \
                          none of Melissa's), `off`, `starting`, `sign_in_needed`, \
                          `needs_approval`, `connected` (with the node's name and tailnet \
                          addresses) or `error`; `web_ui_https`: whether the web UI opens \
                          over HTTPS from tailnet devices (`on` with its `url`, `off`, \
                          `needs_https`, `turning_on`, `unavailable`; only the user changes \
                          it). Read only.",
            labels: Labels::READ_ONLY,
            params: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        }
    }

    fn check_args(&self, args: &Value) -> Result<(), ToolError> {
        no_args(args)
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            no_args(&args)?;
            let mut out = report(&self.0, &self.0.state().await);
            add_serve(&mut out, &self.0.serve_state().await);
            Ok(out)
        })
    }
}

/// "Open Melissa from your other devices", read only: never the page that allows HTTPS, and
/// no tool turns it on (the owner does, with their password, in Settings → Tailscale).
pub fn add_serve(out: &mut Value, serve: &Serve) {
    let mut s = json!({ "state": serve.as_str() });
    match serve {
        Serve::On { url } => s["url"] = json!(url),
        Serve::Off | Serve::NeedsHttps(_) | Serve::TurningOn => {
            s["note"] = json!(
                "Only the user turns this on or off, in Settings → Tailscale (Open Melissa \
                 from your other devices). Don't offer to do it."
            );
        }
        Serve::Unavailable => {}
    }
    out["web_ui_https"] = s;
}

/// `enable_tailscale`: turn Melissa's Tailscale on; the owner approves it every time.
pub struct EnableTailscale(pub TailscaleControl);

/// Why the owner is asked (shown on the card).
const WHY: &str = "connects this device to Tailscale (it then talks to Tailscale's servers)";

impl Tool for EnableTailscale {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "enable_tailscale",
            description: "Turn on Melissa's own Tailscale (off by default) so this device can \
                          join the user's tailnet; only when tailscale_status says `off`. Always \
                          asks the user first. Afterwards the user signs in from Settings → \
                          Tailscale (you never see the sign-in link), and set_app_access \
                          `tailnet` lets tailnet devices open apps. Turning it off is the \
                          user's, in Settings.",
            labels: Labels {
                effect: Effect::Reversible,
                ..Labels::READ_ONLY
            },
            params: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        }
    }

    fn check_args(&self, args: &Value) -> Result<(), ToolError> {
        no_args(args)
    }

    fn owner_approval(
        &self,
        _ctx: &TaskContext,
        args: &Value,
    ) -> Result<Option<String>, ToolError> {
        no_args(args)?;
        Ok(Some(WHY.to_string()))
    }

    fn describe_call(&self, _args: &Value) -> Option<String> {
        Some(format!(
            "turn on Tailscale on this device as `{}` (you sign in next, in Settings → Tailscale)",
            self.0.node_name()
        ))
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            no_args(&args)?;
            match self.0.enable("agent").await {
                Ok(state) => Ok(report(&self.0, &state)),
                Err(TailscaleError::NotManaged) => Ok(report(&self.0, &State::NotManaged)),
                Err(e @ TailscaleError::Save(_)) | Err(e @ TailscaleError::Failed(_)) => {
                    Err(ToolError::Failed(e.to_string()))
                }
                Err(e @ TailscaleError::Not(_)) | Err(e @ TailscaleError::BadName) => {
                    Err(ToolError::Failed(e.to_string()))
                }
            }
        })
    }
}
