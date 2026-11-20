//! The narrow tool API agents act through.
//!
//! Agents never get a shell. They call named tools with JSON arguments; each tool carries
//! [`Labels`] and the [`policy`] decides per call whether it runs now or waits for a human
//! ([`ConfirmationStore`]). Every call, confirmation and refusal is audited with a hash of
//! its arguments. Before the policy, the registry checks the task's [`Scope`]: agents
//! change only resources that belong to them ([`scope`]). A confirmation the owner answered
//! for good ("Always allow" rules, or No permission prompts in developer mode) is approved
//! without asking ([`permissions`]); refusals stay refusals, and an agent's call on another
//! agent's workspace waits for the user's own yes, every time.

mod builtin;
mod confirm;
#[cfg(test)]
mod permission_tests;
pub mod permissions;
pub mod policy;
pub mod scope;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub use builtin::DeviceInfo;
pub use confirm::{Confirmation, ConfirmationEvent, ConfirmationId, ConfirmationStore, Verdict};
pub use permissions::{
    ActionKey, Grant, Permissions, PermissionsError, Principal, Rule, RuleOffer,
};
pub use policy::{check_tool_set, decide, Decision, Effect, Labels, TaskContext, RUNS_SOFTWARE};
pub use scope::{
    crossing, describe_agent, Access, AgentNames, Owner, ResourceKind, ResourceOwners, Scope,
    ScopeDecision, Target, WorkspaceInfo, WorkspaceStore,
};

use crate::{AuditLog, AuditRecord, BoxFuture, TaskId};

/// Static description of a tool, also what agents see (name, description, JSON schema).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub labels: Labels,
    /// JSON Schema for the arguments object.
    pub params: Value,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("invalid arguments: {0}")]
    BadArgs(String),
    #[error("denied by a human")]
    Denied,
    /// Out of the task's scope (not its resource): refused without asking anyone.
    #[error("refused: {0}")]
    Refused(String),
    #[error("confirmation timed out: nobody answered in time, so the request was withdrawn")]
    ConfirmationTimeout,
    /// The request was taken back before anyone answered (its run ended, a new chat).
    #[error("confirmation withdrawn: the run ended before anyone answered")]
    Withdrawn,
    #[error("tool failed: {0}")]
    Failed(String),
}

/// A tool implementation. Validate `args` strictly; never shell out with them.
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>>;

    /// Like [`Tool::call`], for tools that need to know who is calling: the task and its
    /// policy context. A tool may set `ctx.tainted` when it hands back untrusted content
    /// (e.g. the output of a tainted agent). Default: [`Tool::call`].
    fn call_in<'a>(
        &'a self,
        _task: TaskId,
        _ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        self.call(args)
    }

    /// Only sessions that may delegate ([`TaskContext::delegate`], the lead agent) see and may
    /// call this tool (e.g. `start_agent`). Default: false.
    fn lead_only(&self) -> bool {
        false
    }

    /// The resource this tool works on, named by one of its arguments. The registry checks
    /// the caller's [`Scope`] against it before the label policy (see [`scope`]). Every tool
    /// that changes or reads into a resource with an owner must declare it. Default: none.
    fn target(&self) -> Option<Target> {
        None
    }

    /// Check the arguments before a confirmation is asked for, so a call that would fail
    /// anyway fails at once instead of after someone approved it. Default: nothing to check.
    fn check_args(&self, _args: &Value) -> Result<(), ToolError> {
        Ok(())
    }

    /// Calls only the owner may approve, every time (e.g. `schedule_job` creating a schedule:
    /// a standing order to act later). `Ok(Some(reason))` asks the owner whatever the label
    /// policy says, and the confirmation offers no "Always allow": no rule covers it, so the
    /// owner sees each one (No permission prompts, the developer's approve-everything switch,
    /// still answers it). `Err` refuses the call before anyone is asked (e.g. a schedule for
    /// another agent). Runs after the argument and scope checks. Default: `Ok(None)`.
    fn owner_approval(
        &self,
        _ctx: &TaskContext,
        _args: &Value,
    ) -> Result<Option<String>, ToolError> {
        Ok(None)
    }

    /// Confirmations of this tool are the owner's alone (e.g. `delete_agent`): an agent's
    /// Full access never answers them; rules and No permission prompts still may. Tools
    /// with [`Effect::Ownership`] and [`Tool::owner_approval`] calls count as owner-only
    /// anyway. Default: false.
    fn owner_only(&self) -> bool {
        false
    }

    /// A short description of this call for the person asked to confirm it (e.g. "move workspace
    /// `nextcloud` to agent `Bo`"). Default: none (the arguments are shown anyway).
    fn describe_call(&self, _args: &Value) -> Option<String> {
        None
    }

    /// What the `tool.call` audit line records about this call besides its outcome, for
    /// tools whose calls must be traceable in full (e.g. `app_exec`: who ran which command
    /// where, and its exit status). Arguments are otherwise only hashed, so this must leave
    /// out anything secret (say how long an input was, not what it was). Default: nothing.
    fn audit_detail(
        &self,
        _ctx: &TaskContext,
        _args: &Value,
        _result: &Result<Value, ToolError>,
    ) -> Option<String> {
        None
    }
}

/// All tools available on this device, plus the confirmation inbox.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<BTreeMap<&'static str, Arc<dyn Tool>>>,
    confirmations: ConfirmationStore,
    audit: Option<AuditLog>,
    confirm_timeout: Duration,
    /// Who owns workspaces (None: every workspace counts as existing and unassigned, the strictest).
    owners: Option<Arc<dyn ResourceOwners>>,
    /// Agent names for messages and confirmations.
    agents: Option<Arc<dyn AgentNames>>,
    /// The owner's standing permissions (rules, bypass). Default: none, in memory.
    permissions: Permissions,
}

/// Why "Always allow" could not save a rule.
#[derive(Debug, thiserror::Error)]
pub enum AlwaysAllowError {
    #[error("no pending confirmation {0}")]
    Gone(ConfirmationId),
    #[error("this request cannot become a rule (it is not an agent's)")]
    NotOffered,
    #[error(transparent)]
    Save(#[from] PermissionsError),
}

/// What "Always allow" did.
#[derive(Debug, Clone)]
pub struct AlwaysAllowed {
    pub rule: Rule,
    /// The confirmations it approved: the one asked about first, then any other pending
    /// one the rule matches.
    pub approved: Vec<Confirmation>,
}

/// Which standing permissions may answer a confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answerable {
    /// The asking agent's own call: rules, No permission prompts and its Full access.
    Own,
    /// Not the agent's own (owner-only steps, the lead agent's calls on its agents'
    /// workspaces): rules and No permission prompts, never an agent's Full access.
    Owner,
    /// Only the user's own answer ([`Confirmation::user_only`]).
    UserOnly,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>, audit: Option<AuditLog>) -> Self {
        let tools = tools.into_iter().map(|t| (t.spec().name, t)).collect();
        Self {
            tools: Arc::new(tools),
            confirmations: ConfirmationStore::new(),
            audit,
            confirm_timeout: Duration::from_secs(15 * 60),
            owners: None,
            agents: None,
            permissions: Permissions::default(),
        }
    }

    /// The owner's standing permissions ("Always allow" rules, No permission prompts).
    /// Default: none, in memory only.
    pub fn with_permissions(mut self, permissions: Permissions) -> Self {
        self.permissions = permissions;
        self
    }

    /// The owner's standing permissions (read-only here; changes go through
    /// [`ToolRegistry::always_allow`], [`ToolRegistry::remove_rule`] and
    /// [`ToolRegistry::set_bypass`], which audit them).
    pub fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    /// Where the scope check learns who owns a workspace. Without it, every workspace is treated as
    /// existing and unassigned (the strictest answer).
    pub fn with_owners(mut self, owners: Arc<dyn ResourceOwners>) -> Self {
        self.owners = Some(owners);
        self
    }

    /// Where messages and confirmations look up agent names.
    pub fn with_agents(mut self, agents: Arc<dyn AgentNames>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// The agent names, if wired.
    pub fn agent_names(&self) -> Option<&dyn AgentNames> {
        self.agents.as_deref()
    }

    /// Who owns workspace `name` of `kind` now (`Ok(None)`: it does not exist).
    pub async fn owner_of(&self, kind: ResourceKind, name: &str) -> Result<Option<Owner>, String> {
        match &self.owners {
            Some(o) => o.owner(kind, name).await,
            None => Ok(Some(Owner::Unassigned)),
        }
    }

    pub fn with_confirm_timeout(mut self, timeout: Duration) -> Self {
        self.confirm_timeout = timeout;
        self
    }

    /// Every built-in tool that works on any agent.
    pub fn builtin(audit: Option<AuditLog>) -> Self {
        Self::new(vec![Arc::new(DeviceInfo::new())], audit)
    }

    pub fn confirmations(&self) -> &ConfirmationStore {
        &self.confirmations
    }

    /// Every tool spec, including Lead-only tools, limited by `allow` (None = all). For
    /// recognising Melissa's tool names; what an agent sees is [`ToolRegistry::visible_to`].
    pub fn visible(&self, allow: Option<&[&str]>) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| t.spec())
            .filter(|s| allow.is_none_or(|a| a.contains(&s.name)))
            .collect()
    }

    /// Tool specs an agent with policy context `ctx` may see. `allow` limits the set
    /// (None = all); Lead-only tools need `ctx.delegate`. Hidden tools are not merely
    /// refused, the agent never learns they exist.
    /// Tools that change resources outside `ctx.scope` are hidden too, and a targeted tool's
    /// argument description lists the resources in scope.
    pub fn visible_to(&self, ctx: &TaskContext, allow: Option<&[&str]>) -> Vec<ToolSpec> {
        self.tools
            .values()
            .filter(|t| ctx.delegate || !t.lead_only())
            .filter_map(|t| {
                let mut spec = t.spec();
                match t.target() {
                    Some(target) if !scope::present(&ctx.scope, &target, &mut spec.params) => None,
                    _ => Some(spec),
                }
            })
            .filter(|s| allow.is_none_or(|a| a.contains(&s.name)))
            .collect()
    }

    /// The registry's own `&'static` name for `name`, if such a tool exists.
    pub fn name(&self, name: &str) -> Option<&'static str> {
        self.tools.get_key_value(name).map(|(k, _)| *k)
    }

    /// Is `name` a Lead-only tool?
    pub fn is_lead_only(&self, name: &str) -> bool {
        self.tools.get(name).is_some_and(|t| t.lead_only())
    }

    /// Call a tool for `task`, applying the policy. May wait for a human confirmation.
    /// Updates `ctx.tainted` when the tool returns untrusted content.
    pub async fn call(
        &self,
        task: TaskId,
        ctx: &mut TaskContext,
        name: &str,
        args: Value,
    ) -> Result<Value, ToolError> {
        let tool = self
            .tools
            .get(name)
            .cloned()
            .ok_or_else(|| ToolError::UnknownTool(name.to_string()))?;
        if tool.lead_only() && !ctx.delegate {
            // Same answer as for a nonexistent tool: agents never learn these exist.
            return Err(ToolError::UnknownTool(name.to_string()));
        }
        if !args.is_object() && !args.is_null() {
            return Err(ToolError::BadArgs("arguments must be a JSON object".into()));
        }
        let spec = tool.spec();
        let hash = args_hash(&args);
        if let Err(e) = tool.check_args(&args) {
            // Audited like any failed call, so a refused spec can be looked up later.
            self.audit(task, "tool.call", name, &hash, &format!("error: {e}"));
            return Err(e);
        }

        let mut reasons = Vec::new();
        let mut note = None;
        // Only the user's own yes, for this call (another agent's workspace).
        let mut user_only = false;
        // Owner-only: an agent's Full access never answers it.
        let mut owner_only = tool.owner_only() || spec.labels.effect == Effect::Ownership;
        // Who acts on whose workspace, when it is not the caller's own (audited both sides).
        let mut crossed = None;
        // Refusals end here (the scope check), before any rule or the bypass is looked at.
        let mut offer = match tool.target() {
            Some(target) => {
                let (decision, crossing) = self
                    .check_scope(task, ctx, name, &target, &args, &hash)
                    .await?;
                crossed = crossing;
                match decision {
                    ScopeDecision::Confirm(reason) => reasons.push(reason),
                    ScopeDecision::AskUser(reason) => {
                        // Asked whatever the label policy says; whose it is goes first.
                        user_only = true;
                        note = Some(reason);
                    }
                    ScopeDecision::Note(n) => note = Some(n),
                    ScopeDecision::Allow | ScopeDecision::Refuse(_) => {}
                }
                args.get(target.arg)
                    .and_then(Value::as_str)
                    .and_then(|r| RuleOffer::new(&ctx.scope, name, Some((&target, r))))
            }
            None => RuleOffer::new(&ctx.scope, name, None),
        };
        if let Decision::Confirm(reason) = decide(name, &spec.labels, ctx) {
            reasons.push(reason);
        }
        if user_only {
            // Nothing to remember: no rule may ever cover it.
            offer = None;
        }
        match tool.owner_approval(ctx, &args) {
            Ok(None) => {}
            Ok(Some(reason)) => {
                // Asked every time: nothing to remember, so no rule can answer it.
                offer = None;
                owner_only = true;
                reasons.push(reason);
            }
            Err(e) => {
                self.audit(task, "tool.refused", name, &hash, &e.to_string());
                return Err(e);
            }
        }
        if !reasons.is_empty() || user_only {
            // Whoever answers sees whose workspace it is first, then what the call does.
            if let Some(d) = tool.describe_call(&args) {
                reasons.insert(0, d);
            }
            if let Some(n) = note {
                reasons.insert(0, n);
            }
            if let Some(c) = &crossed {
                self.audit(
                    task,
                    "tool.cross_owner",
                    name,
                    &hash,
                    &format!("asked: {c}"),
                );
            }
            let answerable = if user_only {
                Answerable::UserOnly
            } else if owner_only || crossed.is_some() {
                Answerable::Owner
            } else {
                Answerable::Own
            };
            let answer = self
                .ask(
                    task,
                    offer,
                    name,
                    &args,
                    &reasons.join("; "),
                    ctx.confirm_wait,
                    answerable,
                )
                .await;
            if let Some(c) = &crossed {
                let outcome = match &answer {
                    Ok(()) if user_only => "approved by the user (this call only)".to_string(),
                    Ok(()) => "approved".to_string(),
                    Err(ToolError::Denied) => "denied by the user".to_string(),
                    Err(e) => format!("not approved ({e})"),
                };
                self.audit(
                    task,
                    "tool.cross_owner",
                    name,
                    &hash,
                    &format!("{outcome}: {c}"),
                );
            }
            answer?;
        } else if let Some(c) = &crossed {
            // The lead agent's override (its device's agents' workspaces): allowed, on record.
            self.audit(
                task,
                "tool.cross_owner",
                name,
                &hash,
                &format!("allowed (lead agent): {c}"),
            );
        }

        let audited = args.clone();
        let result = tool.call_in(task, ctx, args).await;
        if spec.labels.reads_untrusted && result.is_ok() {
            ctx.tainted = true;
        }
        let mut outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        if let Some(detail) = tool.audit_detail(ctx, &audited, &result) {
            outcome = format!("{outcome}; {detail}");
        }
        self.audit(task, "tool.call", name, &hash, &outcome);
        result
    }

    /// The scope check for a targeted call: `Allow`, `Note`, `Confirm` or `AskUser`, with who
    /// acts on whose workspace when it is not the caller's ([`crossing`]); out of scope is
    /// `Err(Refused)` (audited `tool.refused`, and `tool.cross_owner` when it had an owner).
    async fn check_scope(
        &self,
        task: TaskId,
        ctx: &TaskContext,
        name: &str,
        target: &Target,
        args: &Value,
        hash: &str,
    ) -> Result<(ScopeDecision, Option<String>), ToolError> {
        let resource = match args.get(target.arg) {
            Some(Value::String(s)) => s.as_str(),
            _ => {
                return Err(ToolError::BadArgs(format!(
                    "`{}` (string) is required",
                    target.arg
                )))
            }
        };
        let owner = if ctx.scope.needs_owner(target.access) {
            match self.owner_of(target.kind, resource).await {
                Ok(owner) => owner,
                Err(e) => {
                    // Fail closed: without knowing the owner, nothing is changed.
                    let reason = format!("could not check who owns `{resource}`: {e}");
                    self.audit(task, "tool.refused", name, hash, &reason);
                    return Err(ToolError::Failed(reason));
                }
            }
        } else {
            None
        };
        let crossed = crossing(
            &ctx.scope,
            target,
            resource,
            owner.as_ref(),
            self.agent_names(),
        );
        match scope::check(
            &ctx.scope,
            target,
            resource,
            owner.as_ref(),
            self.agent_names(),
        ) {
            ScopeDecision::Refuse(reason) => {
                self.audit(task, "tool.refused", name, hash, &reason);
                if let Some(c) = &crossed {
                    let detail = format!("refused: {c}");
                    self.audit(task, "tool.cross_owner", name, hash, &detail);
                }
                Err(ToolError::Refused(reason))
            }
            other => Ok((other, crossed)),
        }
    }

    /// Ask a human to approve `name` with `args` for `task` and wait for the answer, at most
    /// the confirmation timeout. Audited (`tool.confirm_requested`, then `tool.approved`,
    /// `tool.denied` or `tool.confirm_timeout`). If the returned future is dropped (task
    /// cancelled), the confirmation is withdrawn from the inbox.
    ///
    /// `rule`: what "Always allow" would save. A saved rule matching it, No permission
    /// prompts, or the asking agent's Full access (this is its own call: e.g. its agent CLI's
    /// shell) approves at once without asking (audited `tool.auto_approved`, actor `rule` or
    /// `bypass`). Call this only for calls that passed the scope and argument checks.
    pub async fn confirm(
        &self,
        task: TaskId,
        rule: Option<RuleOffer>,
        name: &str,
        args: &Value,
        reason: &str,
    ) -> Result<(), ToolError> {
        self.confirm_within(task, rule, name, args, reason, None)
            .await
    }

    /// [`ToolRegistry::confirm`], waiting `wait` instead of the confirmation timeout when
    /// given (a scheduled run's confirmations: [`TaskContext::confirm_wait`]).
    pub async fn confirm_within(
        &self,
        task: TaskId,
        rule: Option<RuleOffer>,
        name: &str,
        args: &Value,
        reason: &str,
        wait: Option<Duration>,
    ) -> Result<(), ToolError> {
        self.ask(task, rule, name, args, reason, wait, Answerable::Own)
            .await
    }

    /// [`ToolRegistry::confirm_within`]; `answerable`: which standing permissions may answer
    /// ([`Answerable`]; user-only: none, [`Confirmation::user_only`]).
    #[allow(clippy::too_many_arguments)]
    async fn ask(
        &self,
        task: TaskId,
        rule: Option<RuleOffer>,
        name: &str,
        args: &Value,
        reason: &str,
        wait: Option<Duration>,
        answerable: Answerable,
    ) -> Result<(), ToolError> {
        let user_only = answerable == Answerable::UserOnly;
        let hash = args_hash(args);
        let key = rule.as_ref().map(|r| r.key.clone());
        let grant = || match answerable {
            Answerable::UserOnly => None,
            Answerable::Owner => self.permissions.grant(key.as_ref()),
            Answerable::Own => self.permissions.grant_own(key.as_ref()),
        };
        if let Some(grant) = grant() {
            self.audit_grant(task, "tool.auto_approved", name, &hash, grant, reason);
            return Ok(());
        }
        let (info, rx) = if user_only {
            self.confirmations
                .request_user_only(task, name, args, reason)
        } else {
            self.confirmations
                .request_with(task, name, args, reason, rule)
        };
        let _withdraw = Withdraw {
            store: &self.confirmations,
            id: info.id,
        };
        self.audit(task, "tool.confirm_requested", name, &hash, reason);
        // A rule saved (or the bypass switched on) while this was being registered may have
        // looked at the inbox just before it: check once more.
        if let Some(grant) = grant() {
            self.approve_granted(&info, grant);
        }
        let verdict = match tokio::time::timeout(wait.unwrap_or(self.confirm_timeout), rx).await {
            Ok(Ok(v)) => v,
            // Withdrawn without an answer (the run ended, a new chat; audited where it was
            // withdrawn): the call does not run.
            Ok(Err(_)) => {
                self.audit(task, "tool.withdrawn", name, &hash, "");
                return Err(ToolError::Withdrawn);
            }
            Err(_) => {
                self.audit(task, "tool.confirm_timeout", name, &hash, "");
                return Err(ToolError::ConfirmationTimeout);
            }
        };
        if verdict == Verdict::Denied {
            self.audit(task, "tool.denied", name, &hash, reason);
            return Err(ToolError::Denied);
        }
        self.audit(task, "tool.approved", name, &hash, reason);
        Ok(())
    }

    /// The owner's "Always allow" on pending confirmation `id`: saves the rule it offers
    /// (audited `permissions.rule_added`, actor `ui`), approves it (`confirmation.approved`)
    /// and every other pending confirmation the rule matches (`confirmation.auto_approved`,
    /// actor `rule`). Never reachable from a tool: only the owner's UI and API call this.
    pub fn always_allow(&self, id: ConfirmationId) -> Result<AlwaysAllowed, AlwaysAllowError> {
        let asked = self
            .confirmations
            .get(id)
            .ok_or(AlwaysAllowError::Gone(id))?;
        let offer = asked.rule.clone().ok_or(AlwaysAllowError::NotOffered)?;
        let (rule, new) = self.permissions.add(&offer)?;
        if new {
            self.audit_owner(
                asked.task,
                "permissions.rule_added",
                &format!("rule {}: {}", rule.id, self.describe_rule(&rule)),
                Some((&asked.tool, &args_hash(&asked.args))),
            );
        }
        let mut approved = Vec::new();
        // The one asked about first, as the owner's own yes.
        if let Some(c) = self.confirmations.resolve(id, Verdict::Approved) {
            self.audit_owner(
                c.task,
                "confirmation.approved",
                &format!("confirmation {id} (always, rule {}): {}", rule.id, c.reason),
                Some((&c.tool, &args_hash(&c.args))),
            );
            approved.push(c);
        }
        for c in self.confirmations.pending() {
            if c.rule.as_ref().is_some_and(|o| o.key == rule.key) {
                if let Some(c) = self.approve_granted(&c, Grant::Rule(rule.id)) {
                    approved.push(c);
                }
            }
        }
        Ok(AlwaysAllowed { rule, approved })
    }

    /// The owner removes rule `id` (audited `permissions.rule_removed`). None: no such rule.
    pub fn remove_rule(&self, id: u64) -> Result<Option<Rule>, PermissionsError> {
        let removed = self.permissions.remove(id)?;
        if let Some(rule) = &removed {
            let detail = format!("rule {}: {}", rule.id, self.describe_rule(rule));
            self.audit_owner(0, "permissions.rule_removed", &detail, None);
        }
        Ok(removed)
    }

    /// The owner switches No permission prompts (audited `permissions.bypass`, `on`/`off`).
    /// Switching it on approves every pending confirmation (`confirmation.auto_approved`, actor
    /// `bypass`) except user-only ones ([`Confirmation::user_only`]), and returns them.
    pub fn set_bypass(&self, on: bool) -> Result<Vec<Confirmation>, PermissionsError> {
        if self.permissions.set_bypass(on)? {
            let detail = if on { "on" } else { "off" };
            self.audit_owner(0, "permissions.bypass", detail, None);
        }
        if !on {
            return Ok(Vec::new());
        }
        Ok(self
            .confirmations
            .pending()
            .iter()
            // Another agent's workspace: the user's own yes only, never the bypass.
            .filter(|c| !c.user_only)
            .filter_map(|c| self.approve_granted(c, Grant::Bypass))
            .collect())
    }

    /// The owner switches the developer option "Full access per agent" (audited
    /// `permissions.full_access_allowed`, `on`/`off`). Off puts every agent back on Ask.
    pub fn set_full_access_allowed(&self, on: bool) -> Result<(), PermissionsError> {
        if self.permissions.set_full_access_allowed(on)? {
            let detail = if on {
                "on"
            } else {
                "off (every agent back on Ask)"
            };
            self.audit_owner(0, "permissions.full_access_allowed", detail, None);
        }
        Ok(())
    }

    /// The owner puts `who` on Full access or back on Ask (audited `permissions.access`,
    /// `full`/`ask`). Applies to its next confirmation; pending ones still wait for an
    /// answer. Full access needs the developer option ([`PermissionsError::FullAccessOff`]).
    pub fn set_access(&self, who: &Principal, full: bool) -> Result<(), PermissionsError> {
        if self.permissions.set_full_access(who, full)? {
            let name = match who {
                Principal::Lead => "the lead agent".to_string(),
                Principal::Agent(id) => describe_agent(self.agent_names(), id),
            };
            let level = if full { "full" } else { "ask" };
            self.audit_owner(0, "permissions.access", &format!("{name}: {level}"), None);
        }
        Ok(())
    }

    /// `let agent Ada (d-….a1) update minecraft (tool update_app)`, for the audit log.
    pub fn describe_rule(&self, rule: &Rule) -> String {
        let who = match &rule.key.principal {
            Principal::Lead => "the lead agent".to_string(),
            Principal::Agent(id) => describe_agent(self.agent_names(), id),
        };
        let what = match &rule.key.target {
            Some(t) => format!("{} {t}", rule.verb),
            None => rule.verb.clone(),
        };
        format!("let {who} {what} (tool {})", rule.key.tool)
    }

    /// Approve pending confirmation `c` because of `grant` (audited
    /// `confirmation.auto_approved`). None if it was no longer pending.
    fn approve_granted(&self, c: &Confirmation, grant: Grant) -> Option<Confirmation> {
        let c = self.confirmations.resolve(c.id, Verdict::Approved)?;
        let detail = format!(
            "confirmation {} by {}: {}",
            c.id,
            grant.describe(),
            c.reason
        );
        if let Some(log) = &self.audit {
            let record = AuditRecord::now("confirmation.auto_approved", c.task, "", detail)
                .by(grant.actor())
                .with_tool(&c.tool, &args_hash(&c.args));
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
        Some(c)
    }

    fn audit_grant(
        &self,
        task: TaskId,
        action: &str,
        tool: &str,
        hash: &str,
        grant: Grant,
        reason: &str,
    ) {
        if let Some(log) = &self.audit {
            let detail = format!("{}: {reason}", grant.describe());
            let record = AuditRecord::now(action, task, "", detail)
                .by(grant.actor())
                .with_tool(tool, hash);
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }

    /// An audit record for the owner's own change (actor `ui`).
    fn audit_owner(&self, task: TaskId, action: &str, detail: &str, tool: Option<(&str, &str)>) {
        if let Some(log) = &self.audit {
            let mut record = AuditRecord::now(action, task, "", detail).by("ui");
            if let Some((tool, hash)) = tool {
                record = record.with_tool(tool, hash);
            }
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }

    /// Write an agent action to the audit log (actor `agent`, arguments hashed, not logged).
    pub fn audit_agent(
        &self,
        task: TaskId,
        backend: &str,
        action: &str,
        tool: &str,
        args: &Value,
        detail: &str,
    ) {
        self.write_audit(task, backend, action, tool, &args_hash(args), detail);
    }

    /// The audit log, if any (shared with the task board and the lead agent).
    pub fn audit_log(&self) -> Option<&AuditLog> {
        self.audit.as_ref()
    }

    fn audit(&self, task: TaskId, action: &str, tool: &str, hash: &str, detail: &str) {
        self.write_audit(task, "", action, tool, hash, detail);
    }

    fn write_audit(
        &self,
        task: TaskId,
        backend: &str,
        action: &str,
        tool: &str,
        hash: &str,
        detail: &str,
    ) {
        if let Some(log) = &self.audit {
            let mut record = AuditRecord::now(action, task, backend, detail).with_tool(tool, hash);
            // Tool calls are made by the task's agent; the person who started the task is
            // recorded on the `task.dispatch` line.
            record.actor = "agent".into();
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }
}

/// Removes a confirmation from the inbox when its waiter goes away (answered, timed out or
/// dropped). Removing an already resolved one is a no-op.
struct Withdraw<'a> {
    store: &'a ConfirmationStore,
    id: ConfirmationId,
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        self.store.withdraw(self.id);
    }
}

/// SHA-256 of the canonical (compact, key-sorted) JSON arguments.
pub(crate) fn args_hash(args: &Value) -> String {
    // Sort explicitly: serde_json's `preserve_order` feature (enabled by a dependency
    // anywhere in the build) makes `Map` keep insertion order instead of sorting.
    let bytes = serde_json::to_vec(&canonical(args)).unwrap_or_default();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `value` with every object's keys in sorted order.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Fake {
        name: &'static str,
        labels: Labels,
    }
    impl Tool for Fake {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name,
                description: "test",
                labels: self.labels,
                params: json!({"type": "object"}),
            }
        }
        fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
            Box::pin(async move { Ok(json!({"echo": args})) })
        }
    }

    fn registry(audit: Option<AuditLog>) -> ToolRegistry {
        ToolRegistry::new(
            vec![
                Arc::new(Fake {
                    name: "status",
                    labels: Labels::READ_ONLY,
                }),
                Arc::new(Fake {
                    name: "read_logs",
                    labels: Labels {
                        reads_untrusted: true,
                        ..Labels::READ_ONLY
                    },
                }),
                Arc::new(Fake {
                    name: "remove_app",
                    labels: Labels {
                        effect: Effect::Destructive,
                        ..Labels::READ_ONLY
                    },
                }),
                Arc::new(Fake {
                    name: "scale",
                    labels: Labels {
                        effect: Effect::Reversible,
                        ..Labels::READ_ONLY
                    },
                }),
            ],
            audit,
        )
    }

    fn attended() -> TaskContext {
        TaskContext {
            attended: true,
            ..TaskContext::default()
        }
    }

    #[tokio::test]
    async fn read_only_runs_without_confirmation() {
        let reg = registry(None);
        let out = reg
            .call(1, &mut attended(), "status", json!({"a": 1}))
            .await
            .unwrap();
        assert_eq!(out["echo"]["a"], 1);
        assert!(reg.confirmations().pending().is_empty());
    }

    #[tokio::test]
    async fn unknown_tools_and_bad_args_are_rejected() {
        let reg = registry(None);
        assert_eq!(
            reg.call(1, &mut attended(), "rm_rf", json!({})).await,
            Err(ToolError::UnknownTool("rm_rf".into()))
        );
        assert!(matches!(
            reg.call(1, &mut attended(), "status", json!([1])).await,
            Err(ToolError::BadArgs(_))
        ));
    }

    #[tokio::test]
    async fn destructive_waits_for_approval() {
        let reg = registry(None);
        let waiter = {
            let reg = reg.clone();
            tokio::spawn(async move {
                reg.call(1, &mut attended(), "remove_app", json!({"app": "x"}))
                    .await
            })
        };
        let pending = loop {
            let p = reg.confirmations().pending();
            if !p.is_empty() {
                break p;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(pending[0].tool, "remove_app");
        assert_eq!(pending[0].reason, "destructive action");
        reg.confirmations()
            .resolve(pending[0].id, Verdict::Approved);
        assert!(waiter.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn denial_and_timeout_stop_the_call() {
        let reg = registry(None).with_confirm_timeout(Duration::from_millis(30));
        assert_eq!(
            reg.call(1, &mut attended(), "remove_app", json!({})).await,
            Err(ToolError::ConfirmationTimeout)
        );
        assert!(reg.confirmations().pending().is_empty());

        let reg = registry(None);
        let waiter = {
            let reg = reg.clone();
            tokio::spawn(async move { reg.call(2, &mut attended(), "remove_app", json!({})).await })
        };
        let id = loop {
            if let Some(c) = reg.confirmations().pending().first() {
                break c.id;
            }
            tokio::task::yield_now().await;
        };
        reg.confirmations().resolve(id, Verdict::Denied);
        assert_eq!(waiter.await.unwrap(), Err(ToolError::Denied));
    }

    #[tokio::test(start_paused = true)]
    async fn a_scheduled_runs_confirmations_wait_its_own_time() {
        // The registry's timeout is long; a task with its own wait (a scheduled run) is
        // withdrawn after that wait instead.
        let reg = registry(None).with_confirm_timeout(Duration::from_secs(4 * 60));
        let mut ctx = attended();
        ctx.confirm_wait = Some(Duration::from_secs(10 * 60));
        let started = tokio::time::Instant::now();
        assert_eq!(
            reg.call(1, &mut ctx, "remove_app", json!({})).await,
            Err(ToolError::ConfirmationTimeout)
        );
        assert_eq!(started.elapsed(), Duration::from_secs(10 * 60));
        assert!(reg.confirmations().pending().is_empty());
        // Without one (interactive): the registry's timeout.
        let started = tokio::time::Instant::now();
        assert_eq!(
            reg.call(2, &mut attended(), "remove_app", json!({})).await,
            Err(ToolError::ConfirmationTimeout)
        );
        assert_eq!(started.elapsed(), Duration::from_secs(4 * 60));
    }

    #[tokio::test]
    async fn untrusted_output_taints_the_task() {
        let reg = registry(None).with_confirm_timeout(Duration::from_millis(30));
        let mut ctx = attended();
        reg.call(1, &mut ctx, "scale", json!({})).await.unwrap();
        reg.call(1, &mut ctx, "read_logs", json!({})).await.unwrap();
        assert!(ctx.tainted);
        // After reading untrusted logs, even an attended reversible action needs approval.
        assert_eq!(
            reg.call(1, &mut ctx, "scale", json!({})).await,
            Err(ToolError::ConfirmationTimeout)
        );
    }

    struct LeadOnly;
    impl Tool for LeadOnly {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "start_agent",
                description: "test",
                labels: Labels::READ_ONLY,
                params: json!({"type": "object"}),
            }
        }
        fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
            Box::pin(async { Ok(json!("started")) })
        }
        fn lead_only(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn lead_only_tools_need_delegation() {
        let reg = ToolRegistry::new(vec![Arc::new(LeadOnly)], None);
        let mut agent = attended();
        assert!(reg.visible_to(&agent, None).is_empty());
        assert_eq!(
            reg.call(1, &mut agent, "start_agent", json!({})).await,
            Err(ToolError::UnknownTool("start_agent".into()))
        );
        let mut lead = TaskContext {
            delegate: true,
            ..attended()
        };
        assert_eq!(reg.visible_to(&lead, None).len(), 1);
        assert_eq!(
            reg.call(1, &mut lead, "start_agent", json!({})).await,
            Ok(json!("started"))
        );
        // Name matching for permission requests still knows the tool.
        assert_eq!(reg.visible(None).len(), 1);
        assert!(reg.is_lead_only("start_agent"));
    }

    #[test]
    fn visibility_filter_hides_tools() {
        let reg = registry(None);
        assert_eq!(reg.visible(None).len(), 4);
        let names: Vec<_> = reg
            .visible(Some(&["status"]))
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["status"]);
    }

    #[tokio::test]
    async fn calls_are_audited_with_arg_hash() {
        let path =
            std::env::temp_dir().join(format!("melissa-tools-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let reg = registry(Some(AuditLog::new(&path)));
        reg.call(5, &mut attended(), "status", json!({"b": 2, "a": 1}))
            .await
            .unwrap();
        let line: Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(line["action"], "tool.call");
        assert_eq!(line["actor"], "agent");
        assert_eq!(line["tool"], "status");
        assert_eq!(line["args_sha256"], args_hash(&json!({"a": 1, "b": 2})));
        // Canonical form is sorted keys, whatever serde_json's map ordering feature says.
        let expected: String = Sha256::digest(br#"{"a":1,"b":2}"#)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(line["args_sha256"], expected.as_str());
        assert_eq!(
            args_hash(&json!({"z": {"y": 1, "x": [{"b": 1, "a": 2}]}})),
            args_hash(&json!({"z": {"x": [{"a": 2, "b": 1}], "y": 1}}))
        );
        std::fs::remove_file(&path).unwrap();
    }
}
