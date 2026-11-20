//! `my_files`: an agent's own file box through Melissa's tools, for agents without file
//! tools of their own (Claude Code runs with none). Only the caller's box: the lead agent's
//! or the calling agent's, from its task scope; there is no argument naming a box.
//!
//! The labels say read-only: nothing outside the caller's own box changes, and the box is
//! already the agent's to write in its sandbox. Every call is audited by the registry
//! (`tool.call`); deletions also as `files.deleted`.

use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::{AddedBy, BoxFile, FileBoxError, FileBoxes};
use crate::{
    attachments::human_size,
    tools::{permissions::Principal, policy::Labels, TaskContext, Tool, ToolError, ToolSpec},
    TaskId,
};

/// Largest text `read` returns.
pub const MAX_READ_BYTES: usize = 256 * 1024;
/// Largest text `save` writes.
pub const MAX_SAVE_BYTES: usize = 1024 * 1024;

/// The `my_files` tool.
pub struct MyFilesTool {
    files: FileBoxes,
}

impl MyFilesTool {
    pub fn new(files: FileBoxes) -> Self {
        Self { files }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn name_arg(args: &Value) -> Result<&str, ToolError> {
    str_arg(args, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            ToolError::BadArgs("`name` (the file's name in your box) is required".into())
        })
}

fn failed(e: FileBoxError) -> ToolError {
    match e {
        FileBoxError::NotFound => ToolError::BadArgs("no such file in your box".into()),
        FileBoxError::Invalid(m) => ToolError::BadArgs(m),
        other => ToolError::Failed(other.to_string()),
    }
}

fn view(f: &BoxFile) -> Value {
    json!({
        "name": f.name,
        "type": f.mime,
        "size": human_size(f.size),
        "added_by": match f.by { AddedBy::Owner => "user", AddedBy::Agent => "you" },
        "added": f.added,
    })
}

impl Tool for MyFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "my_files",
            description: "Your file box: files the user sent you and files you keep or hand to \
                          the user (they see the box next to your chat and can download from \
                          it). `list` shows the files; `read` returns a text file's contents \
                          (up to 256 KiB); `save` writes a text file (up to 1 MiB; replaces a \
                          file of the same name); `delete` removes one. Files are removed after \
                          the user's retention time.",
            labels: Labels::READ_ONLY,
            params: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "read", "save", "delete"]},
                    "name": {"type": "string", "description": "read, save, delete: the file's name in your box, e.g. `report.md`."},
                    "text": {"type": "string", "description": "save: the file's contents (text)."},
                },
                "required": ["action"],
                "additionalProperties": false,
            }),
        }
    }

    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async {
            Err(ToolError::Failed(
                "my_files needs to know who is calling".into(),
            ))
        })
    }

    fn check_args(&self, args: &Value) -> Result<(), ToolError> {
        match str_arg(args, "action") {
            Some("list") => Ok(()),
            Some("read" | "delete") => name_arg(args).map(|_| ()),
            Some("save") => {
                name_arg(args)?;
                let text = str_arg(args, "text")
                    .ok_or_else(|| ToolError::BadArgs("`text` is required to save".into()))?;
                if text.len() > MAX_SAVE_BYTES {
                    return Err(ToolError::BadArgs(format!(
                        "at most {} per file",
                        human_size(MAX_SAVE_BYTES as u64)
                    )));
                }
                Ok(())
            }
            _ => Err(ToolError::BadArgs(
                "`action` must be `list`, `read`, `save` or `delete`".into(),
            )),
        }
    }

    fn call_in<'a>(
        &'a self,
        _task: TaskId,
        ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let owner = Principal::of(&ctx.scope).ok_or_else(|| {
                ToolError::Refused("only the lead agent and agents have a file box".into())
            })?;
            let files = self.files.clone();
            let dir = files
                .dir(&owner)
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            tokio::task::spawn_blocking(move || -> Result<Value, ToolError> {
                match str_arg(&args, "action") {
                    Some("list") => {
                        let list: Vec<Value> = files.list(&owner).iter().map(view).collect();
                        let keep = files.keep_for(&owner).words();
                        Ok(json!({"box": dir, "files": list, "kept_for": keep}))
                    }
                    Some("read") => {
                        let (f, bytes, cut) = files
                            .read_named(&owner, name_arg(&args)?, MAX_READ_BYTES)
                            .map_err(failed)?;
                        if f.mime != "text/plain" {
                            return Ok(json!({
                                "file": view(&f),
                                "note": format!("not a text file; open it with your own tools at {dir}/{}", f.name),
                            }));
                        }
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        Ok(json!({"file": view(&f), "text": text, "truncated": cut}))
                    }
                    Some("save") => {
                        let text = str_arg(&args, "text").unwrap_or_default();
                        let f = files
                            .save_named(&owner, name_arg(&args)?, text.as_bytes())
                            .map_err(failed)?;
                        Ok(json!({"saved": view(&f), "path": format!("{dir}/{}", f.name)}))
                    }
                    Some("delete") => {
                        let name = name_arg(&args)?;
                        let id = files
                            .list(&owner)
                            .into_iter()
                            .find(|f| f.name == name)
                            .map(|f| f.id)
                            .ok_or_else(|| ToolError::BadArgs("no such file in your box".into()))?;
                        let actor = match owner {
                            Principal::Lead => "lead",
                            Principal::Agent(_) => "agent",
                        };
                        let f = files.delete(&owner, &id, actor).map_err(failed)?;
                        Ok(json!({"deleted": f.name}))
                    }
                    _ => Err(ToolError::BadArgs("unknown action".into())),
                }
            })
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?
        })
    }
}
