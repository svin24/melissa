//! `set_app_access`: an agent changes who may open the apps on this device. Opening apps to
//! more devices asks the owner every time ([`Tool::owner_approval`]: no "Always allow");
//! closing them to fewer runs under the usual policy (a reversible change). Audited
//! `settings.app_access`, actor `agent`, besides the registry's own `tool.call` line.

use melissa_agent::{
    tools::{Effect, Labels, TaskContext, Tool, ToolError, ToolSpec},
    BoxFuture,
};
use serde_json::{json, Value};

use super::{AppAccess, AppAccessControl, AppAccessError, View};

pub struct SetAppAccess(pub AppAccessControl);

/// Why the owner is asked (shown on the card).
const WIDENS: &str = "lets more devices open the apps on this device";

fn wanted(args: &Value) -> Result<AppAccess, ToolError> {
    let obj = args
        .as_object()
        .ok_or_else(|| ToolError::BadArgs("arguments must be an object".into()))?;
    if let Some(k) = obj.keys().find(|k| *k != "open_to") {
        return Err(ToolError::BadArgs(format!("unknown argument `{k}`")));
    }
    let value = obj
        .get("open_to")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadArgs("`open_to` is required".into()))?;
    AppAccess::parse(value).ok_or_else(|| {
        ToolError::BadArgs("`open_to` must be this_device, tailnet or home_network".into())
    })
}

/// The tool result: what is set, what is in force, and any problem.
pub fn report(view: &View) -> Value {
    let mut out = json!({
        "setting": view.setting.as_str(),
        "open_to": view.open_to().map(AppAccess::as_str),
        "who": view.setting.sentence(),
    });
    if let Some(p) = view.problem() {
        out["problem"] = json!(p);
        out["errors"] = json!(view.errors());
    }
    out
}

impl Tool for SetAppAccess {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "set_app_access",
            description: "Choose who may open the apps on this device (Settings → Apps): \
                          `this_device` (only this device), `tailnet` (also devices on the \
                          user's Tailscale network) or `home_network` (also devices on the \
                          home network). Use it when app_status leaves an address out of the \
                          links because apps aren't open to it (`reachability.access`): \
                          propose the smallest level that covers it, instead of asking the \
                          user to reinstall. Opening to more devices always asks the user \
                          first; closing doesn't. Applies at once, no restart.",
            labels: Labels {
                effect: Effect::Reversible,
                ..Labels::READ_ONLY
            },
            params: json!({
                "type": "object",
                "properties": {
                    "open_to": {
                        "type": "string",
                        "enum": ["this_device", "tailnet", "home_network"],
                        "description": "Who may open apps. Each level includes the ones before it."
                    }
                },
                "required": ["open_to"],
                "additionalProperties": false,
            }),
        }
    }

    fn check_args(&self, args: &Value) -> Result<(), ToolError> {
        wanted(args).map(|_| ())
    }

    fn owner_approval(
        &self,
        _ctx: &TaskContext,
        args: &Value,
    ) -> Result<Option<String>, ToolError> {
        let want = wanted(args)?;
        Ok((want > self.0.setting()).then(|| WIDENS.to_string()))
    }

    fn describe_call(&self, args: &Value) -> Option<String> {
        let want = wanted(args).ok()?;
        let now = self.0.setting();
        Some(format!(
            "open apps on this device to: {} ({} Now: {}.)",
            want.label(),
            want.sentence(),
            now.label()
        ))
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let want = wanted(&args)?;
            match self.0.set(want, "agent").await {
                Ok(view) => Ok(report(&view)),
                // Saved but not in force: say what is (not an error the agent should retry).
                Err(AppAccessError::Apply(_)) => {
                    let mut out = report(&self.0.view());
                    out["applied"] = json!(false);
                    Ok(out)
                }
                Err(AppAccessError::Save(e)) => Err(ToolError::Failed(e.to_string())),
            }
        })
    }
}
