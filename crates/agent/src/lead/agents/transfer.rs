//! `transfer_file`: the lead agent sends a file from one file box to another (its own or a
//! helper's), copy by default. Lead-only (helpers never see it and can't reach other boxes),
//! and every call waits for the owner's yes naming both boxes and the file
//! ([`Tool::owner_approval`]: no "Always allow" rule covers it). The transfer itself is
//! [`crate::files::FileBoxes::transfer`] (receiving box's limits, audited `files.transferred`,
//! actor `lead`).

use serde_json::{json, Value};

use super::{agent_param, lead_only, needs_context, object, str_arg, with_note, AgentPool};
use crate::lead::registry::{Who, LEAD_KEY};
use crate::{
    attachments::human_size,
    files::{BoxFile, FileBoxError, Transfer},
    tools::{
        describe_agent, permissions::Principal, Effect, Labels, TaskContext, Tool, ToolError,
        ToolSpec,
    },
    BoxFuture, TaskId,
};

/// What the owner is told the confirmation is about.
const TRANSFER_REASON: &str =
    "puts a file in another agent's file box (only you decide who sees which files)";

pub(super) struct TransferFile(pub(super) AgentPool);

const KNOWN: &[&str] = &["from_agent", "file", "to_agent", "move"];

/// A resolved box: its owner and how to name it to the owner.
struct BoxRef {
    owner: Principal,
    /// For the lead agent's results: `Melissa (lead)` or `Bo (<id>)`.
    name: String,
    /// For the confirmation: `Melissa (the lead agent)` or `agent Bo (<id>)`.
    described: String,
    /// Named by name, not id: the result says which id was used.
    note: Option<String>,
}

impl TransferFile {
    fn box_of(&self, who: &str) -> Result<BoxRef, ToolError> {
        let key = who.trim();
        let registry = &self.0.inner.registry;
        let found = if ["me", "you"].iter().any(|k| key.eq_ignore_ascii_case(k)) {
            registry.lookup(LEAD_KEY)
        } else {
            registry.lookup(key)
        }
        .map_err(|e| ToolError::BadArgs(e.to_string()))?;
        let note = found
            .note(key)
            .filter(|_| !key.eq_ignore_ascii_case("me") && !key.eq_ignore_ascii_case("you"));
        Ok(match found.who {
            Who::Lead => BoxRef {
                owner: Principal::Lead,
                described: format!("{} (the lead agent)", registry.lead_name()),
                name: found.label,
                note,
            },
            Who::Agent(agent) => BoxRef {
                described: describe_agent(Some(registry), &agent.id),
                name: found.label,
                owner: Principal::Agent(agent.id),
                note,
            },
        })
    }

    /// Both boxes and the file, checked: different boxes, the file exists.
    fn plan(&self, args: &Value) -> Result<(BoxRef, BoxFile, BoxRef, Transfer), ToolError> {
        let args = object(args, KNOWN)?;
        let from = self.box_of(str_arg(&args, "from_agent")?)?;
        let to = self.box_of(str_arg(&args, "to_agent")?)?;
        let name = str_arg(&args, "file")?;
        let mode = match args.get("move") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => Transfer::Copy,
            Some(Value::Bool(true)) => Transfer::Move,
            Some(_) => return Err(ToolError::BadArgs("`move` must be true or false".into())),
        };
        if from.owner == to.owner {
            return Err(ToolError::BadArgs(
                "`from_agent` and `to_agent` are the same file box".into(),
            ));
        }
        let files = &self.0.attached()?.files;
        let file = files
            .list(&from.owner)
            .into_iter()
            .find(|f| f.name == name)
            .ok_or_else(|| {
                ToolError::BadArgs(format!("no file `{name}` in the file box of {}", from.name))
            })?;
        Ok((from, file, to, mode))
    }
}

impl Tool for TransferFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "transfer_file",
            description: "Send a file from one file box to another: yours (`lead`) or an \
                          agent's. Copies by default (both keep it); `move: true` removes it from \
                          the sender. Use it when the user asks you to pass a file between \
                          agents; agents can't reach each other's boxes. Always asks the user \
                          first. The file counts as new in the receiving box.",
            labels: Labels {
                effect: Effect::Reversible,
                ..Labels::READ_ONLY
            },
            params: json!({
                "type": "object",
                "properties": {
                    "from_agent": agent_param("Whose box the file is in (`lead` for yours)"),
                    "file": {"type": "string", "description": "The file's name in that box, e.g. `report.pdf`."},
                    "to_agent": agent_param("Whose box gets it (`lead` for yours)"),
                    "move": {"type": "boolean", "description": "Remove it from the sender (default false: copy)."},
                },
                "required": ["from_agent", "file", "to_agent"],
                "additionalProperties": false,
            }),
        }
    }

    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        needs_context()
    }

    fn lead_only(&self) -> bool {
        true
    }

    fn check_args(&self, args: &Value) -> Result<(), ToolError> {
        // An unknown agent or file fails at once, before anyone is asked.
        self.plan(args).map(|_| ())
    }

    fn owner_approval(
        &self,
        ctx: &TaskContext,
        _args: &Value,
    ) -> Result<Option<String>, ToolError> {
        lead_only(ctx)?;
        Ok(Some(TRANSFER_REASON.into()))
    }

    fn describe_call(&self, args: &Value) -> Option<String> {
        let (from, file, to, mode) = self.plan(args).ok()?;
        let verb = match mode {
            Transfer::Copy => "copy",
            Transfer::Move => "move",
        };
        Some(format!(
            "the lead agent wants to {verb} `{}` ({}) from the file box of {} to the file box of {}",
            file.name,
            human_size(file.size),
            from.described,
            to.described
        ))
    }

    fn call_in<'a>(
        &'a self,
        _task: TaskId,
        ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            lead_only(ctx)?;
            let (from, file, to, mode) = self.plan(&args)?;
            let files = self.0.attached()?.files.clone();
            let (src, dst, id) = (from.owner.clone(), to.owner.clone(), file.id.clone());
            let done =
                tokio::task::spawn_blocking(move || files.transfer(&src, &id, &dst, mode, "lead"))
                    .await
                    .map_err(|e| ToolError::Failed(e.to_string()))?;
            match done {
                Ok(t) => {
                    let mut out = json!({
                        "file": t.file.name,
                        "from": from.name,
                        "to": to.name,
                        "size": human_size(t.file.size),
                        "moved": t.mode == Transfer::Move,
                    });
                    let notes: Vec<String> = from.note.into_iter().chain(to.note).collect();
                    with_note(&mut out, (!notes.is_empty()).then(|| notes.join("; ")));
                    Ok(out)
                }
                Err(FileBoxError::NotFound) => Err(ToolError::BadArgs(format!(
                    "no file `{}` in the file box of {}",
                    file.name, from.name
                ))),
                Err(FileBoxError::Full(_)) => Err(ToolError::Refused(format!(
                    "the file box of {} is full",
                    to.name
                ))),
                Err(e @ FileBoxError::Invalid(_)) => Err(ToolError::BadArgs(e.to_string())),
                Err(e) => Err(ToolError::Failed(e.to_string())),
            }
        })
    }
}
