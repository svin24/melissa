//! `schedule_job`: agents propose scheduled jobs; the owner approves each one.
//!
//! - `list`: the caller's schedules (the lead agent and the owner: all).
//! - `create`: a schedule for the caller itself; the lead agent may name any agent (`agent`).
//! - `remove`: one of the caller's schedules (the lead agent: any).
//!
//! Create and remove always ask the owner ([`Tool::owner_approval`]): a schedule is a standing
//! order to act later without anyone watching, so one approval must never turn into many.
//! "Always allow" therefore cannot cover them (the confirmation offers no rule); the
//! developer's No permission prompts still answers them, as it answers every confirmation.
//! Anything about another agent's schedules is refused before anyone is asked ("not yours").
//! The tool's labels say read-only because nothing changes without the owner's yes.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::{
    agent_words, check_input, describe, Creator, Principal, Schedule, ScheduleInput, Schedules,
};
use crate::{
    tools::{policy::Labels, AgentNames, Scope, TaskContext, Tool, ToolError, ToolSpec},
    TaskId,
};

/// The `schedule_job` tool.
pub struct ScheduleJobTool {
    schedules: Schedules,
    names: Option<Arc<dyn AgentNames>>,
}

enum Action {
    List,
    Create,
    Remove(u64),
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn not_yours() -> ToolError {
    ToolError::Refused(
        "not yours: an agent sets up and removes only its own schedules (ask the lead agent \
         or the owner)"
            .into(),
    )
}

impl ScheduleJobTool {
    pub fn new(schedules: Schedules, names: Option<Arc<dyn AgentNames>>) -> Self {
        Self { schedules, names }
    }

    fn names(&self) -> Option<&dyn AgentNames> {
        self.names.as_deref()
    }

    fn action(&self, args: &Value) -> Result<Action, ToolError> {
        match str_arg(args, "action") {
            Some("list") => Ok(Action::List),
            Some("create") => Ok(Action::Create),
            Some("remove") => {
                let id = args
                    .get("id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| ToolError::BadArgs("`id` (number) is required".into()))?;
                Ok(Action::Remove(id))
            }
            _ => Err(ToolError::BadArgs(
                "`action` must be `list`, `create` or `remove`".into(),
            )),
        }
    }

    /// The agent a `create` is for, checked against the caller's scope: an agent only
    /// itself, the lead agent (and the owner) anyone.
    fn target(&self, scope: &Scope, args: &Value) -> Result<Principal, ToolError> {
        let named = str_arg(args, "agent")
            .map(str::trim)
            .filter(|a| !a.is_empty());
        match scope {
            Scope::Nothing => Err(not_yours()),
            Scope::Agent { id } => match named {
                None => Ok(Principal::Agent(id.clone())),
                Some(n) => match self.names().and_then(|names| names.resolve_id(n)) {
                    Some(other) if other == *id => Ok(Principal::Agent(other)),
                    _ => Err(not_yours()),
                },
            },
            Scope::Lead { .. } | Scope::User => match named {
                None => Ok(Principal::Lead),
                Some(n) if n.eq_ignore_ascii_case("lead") => Ok(Principal::Lead),
                Some(n) => self
                    .names()
                    .and_then(|names| names.resolve_id(n))
                    .map(Principal::Agent)
                    .ok_or_else(|| ToolError::BadArgs(format!("no agent `{n}` on this device"))),
            },
        }
    }

    /// May the caller change a schedule for `agent`?
    fn may_change(scope: &Scope, agent: &Principal) -> Result<(), ToolError> {
        match scope {
            Scope::Lead { .. } | Scope::User => Ok(()),
            Scope::Agent { id } if *agent == Principal::Agent(id.clone()) => Ok(()),
            _ => Err(not_yours()),
        }
    }

    fn input(args: &Value, agent: Principal) -> ScheduleInput {
        ScheduleInput {
            name: str_arg(args, "name").unwrap_or_default().to_string(),
            agent,
            instruction: str_arg(args, "instruction").unwrap_or_default().to_string(),
            cron: str_arg(args, "schedule").unwrap_or_default().to_string(),
            enabled: true,
            max_minutes: args
                .get("max_minutes")
                .and_then(Value::as_u64)
                .map(|m| u32::try_from(m).unwrap_or(u32::MAX)),
        }
    }

    fn existing(&self, id: u64) -> Result<Schedule, ToolError> {
        self.schedules
            .get(id)
            .ok_or_else(|| ToolError::BadArgs(format!("no schedule {id}")))
    }

    fn view(&self, s: &Schedule) -> Value {
        let tz = self.schedules.time_zone();
        let next = self.schedules.next_run(s).map(|t| {
            t.to_zoned(tz.clone())
                .strftime("%Y-%m-%d %H:%M %Z")
                .to_string()
        });
        json!({
            "id": s.id,
            "name": s.name,
            "agent": agent_words(&s.agent, self.names()),
            "instruction": s.instruction,
            "schedule": s.cron,
            "when": describe(&s.cron),
            "enabled": s.enabled,
            "max_minutes": s.max_minutes,
            "next_run": next,
            "last_run": s.last_run.as_ref().map(|r| json!({
                "outcome": r.outcome.words(),
                "detail": r.detail,
            })),
        })
    }
}

impl Tool for ScheduleJobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "schedule_job",
            description: "Scheduled jobs: an instruction Melissa sends to an agent on a cron \
                          schedule (the box's local time), e.g. nightly updates. `create` \
                          proposes one for yourself (the lead agent may name any agent); the \
                          owner must approve every create and remove, so say why in your \
                          reply. Runs happen unattended: whatever needs the owner's approval \
                          waits in their inbox unless they gave an \"Always allow\" rule for \
                          it. `list` shows your schedules with their next and last run; \
                          `remove` deletes one by id.",
            labels: Labels::READ_ONLY,
            params: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "create", "remove"]},
                    "name": {"type": "string", "description": "create: a short name the owner sees, e.g. \"Nightly updates\" (at most 60 characters)."},
                    "instruction": {"type": "string", "description": "create: what to do on each run, self-contained (you get it as a message with no one watching)."},
                    "schedule": {"type": "string", "description": "create: 5-field cron expression `minute hour day-of-month month day-of-week` in local time, e.g. `0 3 * * *` (daily 03:00), `30 * * * *` (hourly at :30), `0 4 * * 1` (Mondays 04:00)."},
                    "agent": {"type": "string", "description": "create, lead agent only: the agent (name or id) to run it; default yourself."},
                    "max_minutes": {"type": "integer", "minimum": 1, "maximum": 1440, "description": "create: cancel a run still going after this long (default 60)."},
                    "id": {"type": "integer", "description": "remove: the schedule's id (from list)."},
                },
                "required": ["action"],
                "additionalProperties": false,
            }),
        }
    }

    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async {
            Err(ToolError::Failed(
                "schedule_job needs to know who is calling".into(),
            ))
        })
    }

    fn check_args(&self, args: &Value) -> Result<(), ToolError> {
        match self.action(args)? {
            Action::List => Ok(()),
            // The agent is checked with the caller's scope (owner_approval).
            Action::Create => check_input(&Self::input(args, Principal::Lead))
                .map_err(|e| ToolError::BadArgs(e.to_string())),
            Action::Remove(id) => self.existing(id).map(|_| ()),
        }
    }

    fn owner_approval(&self, ctx: &TaskContext, args: &Value) -> Result<Option<String>, ToolError> {
        match self.action(args)? {
            Action::List => Ok(None),
            Action::Create => {
                self.target(&ctx.scope, args)?;
                Ok(Some(
                    "sets up a job that runs on its own, while nobody watches; only you can \
                     approve schedules"
                        .into(),
                ))
            }
            Action::Remove(id) => {
                Self::may_change(&ctx.scope, &self.existing(id)?.agent)?;
                Ok(Some(
                    "removes a scheduled job; only you can approve that".into(),
                ))
            }
        }
    }

    fn describe_call(&self, args: &Value) -> Option<String> {
        match self.action(args).ok()? {
            Action::List => None,
            Action::Create => {
                let agent = str_arg(args, "agent").unwrap_or("itself");
                Some(format!(
                    "schedule \"{}\" for {agent}: {}",
                    str_arg(args, "name").unwrap_or_default().trim(),
                    describe(str_arg(args, "schedule").unwrap_or_default()),
                ))
            }
            Action::Remove(id) => {
                let s = self.schedules.get(id)?;
                Some(format!(
                    "remove schedule {id} \"{}\" ({}, {})",
                    s.name,
                    agent_words(&s.agent, self.names()),
                    describe(&s.cron)
                ))
            }
        }
    }

    fn call_in<'a>(
        &'a self,
        _task: TaskId,
        ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let by = Creator::of(&ctx.scope).ok_or_else(not_yours)?;
            match self.action(&args)? {
                Action::List => {
                    let mine: Vec<Value> = self
                        .schedules
                        .list()
                        .iter()
                        .filter(|s| Self::may_change(&ctx.scope, &s.agent).is_ok())
                        .map(|s| self.view(s))
                        .collect();
                    Ok(json!({"schedules": mine}))
                }
                Action::Create => {
                    let agent = self.target(&ctx.scope, &args)?;
                    let s = self
                        .schedules
                        .create(&Self::input(&args, agent), &by)
                        .map_err(|e| ToolError::Failed(e.to_string()))?;
                    Ok(json!({
                        "created": self.view(&s),
                        "note": "Runs unattended. Actions that need the owner wait in their inbox \
                                 unless they gave an \"Always allow\" rule for them.",
                    }))
                }
                Action::Remove(id) => {
                    Self::may_change(&ctx.scope, &self.existing(id)?.agent)?;
                    match self
                        .schedules
                        .remove(id, &by)
                        .map_err(|e| ToolError::Failed(e.to_string()))?
                    {
                        Some(s) => Ok(json!({"removed": s.id, "name": s.name})),
                        None => Err(ToolError::BadArgs(format!("no schedule {id}"))),
                    }
                }
            }
        })
    }
}
