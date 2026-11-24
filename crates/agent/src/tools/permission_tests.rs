//! "Always allow" rules and No permission prompts, through the registry.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde_json::{json, Value};

use super::*;
use crate::test_support::scratch;
use crate::test_support::TestTool;

const ADA: &str = "d-0a1b2c3d.a1";
const BO: &str = "d-0a1b2c3d.a2";

/// A tool on a workspace (`app`) that always needs a yes (like `update_app`).
fn update() -> Arc<dyn Tool> {
    TestTool::new("update_app")
        .effect(Effect::Deploy)
        .target(Target {
            kind: ResourceKind::Workspace,
            arg: "app",
            access: Access::Change,
            verb: "update",
        })
        .reply(|_| json!("updated"))
        .arc()
}

/// A tool without a target that needs a yes while nobody watches (reversible).
fn restart() -> Arc<dyn Tool> {
    TestTool::new("restart")
        .effect(Effect::Reversible)
        .reply(|_| json!("restarted"))
        .arc()
}

/// minecraft and nextcloud are Ada's, valheim is Bo's.
struct Owners(BTreeMap<&'static str, &'static str>);
impl ResourceOwners for Owners {
    fn owner<'a>(
        &'a self,
        _: ResourceKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Owner>, String>> {
        let owner = self.0.get(name).map(|id| Owner::Agent((*id).into()));
        Box::pin(async move { Ok(owner) })
    }
}

fn registry(audit: Option<AuditLog>, permissions: Permissions) -> ToolRegistry {
    ToolRegistry::new(vec![update(), restart()], audit)
        .with_owners(Arc::new(Owners(BTreeMap::from([
            ("minecraft", ADA),
            ("nextcloud", ADA),
            ("valheim", BO),
            ("far", "d-ffffffff.a1"),
        ]))))
        .with_permissions(permissions)
        .with_confirm_timeout(Duration::from_secs(10))
}

fn agent(id: &str) -> TaskContext {
    TaskContext {
        scope: Scope::Agent { id: id.into() },
        ..TaskContext::default()
    }
}

/// Start a call in the background.
fn spawn(
    reg: &ToolRegistry,
    task: TaskId,
    who: &str,
    tool: &'static str,
    args: Value,
) -> tokio::task::JoinHandle<Result<Value, ToolError>> {
    let reg = reg.clone();
    let mut ctx = agent(who);
    tokio::spawn(async move { reg.call(task, &mut ctx, tool, args).await })
}

/// Wait until `n` confirmations are pending.
async fn pending(reg: &ToolRegistry, n: usize) -> Vec<Confirmation> {
    for _ in 0..10_000 {
        let p = reg.confirmations().pending();
        if p.len() >= n {
            return p;
        }
        tokio::task::yield_now().await;
    }
    panic!("expected {n} pending confirmations");
}

/// A call that must finish without asking anyone.
async fn runs_now(
    reg: &ToolRegistry,
    who: &str,
    tool: &str,
    args: Value,
) -> Result<Value, ToolError> {
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        reg.call(9, &mut agent(who), tool, args),
    )
    .await
    .expect("the call waited for a human");
    assert!(reg.confirmations().pending().is_empty());
    out
}

fn audit_lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn always_allow_saves_a_narrow_rule_and_later_calls_skip_the_prompt() {
    let dir = scratch("rule");
    let log = dir.join("audit.jsonl");
    let reg = registry(Some(AuditLog::new(&log)), Permissions::in_memory());
    let first = spawn(&reg, 1, ADA, "update_app", json!({"app": "minecraft"}));
    let c = pending(&reg, 1).await.remove(0);
    let offer = c.rule.clone().expect("an agent's request offers a rule");
    assert_eq!(
        offer.key,
        ActionKey {
            principal: Principal::Agent(ADA.into()),
            tool: "update_app".into(),
            target: Some("minecraft".into()),
        }
    );
    assert_eq!(offer.verb, "update");

    let done = reg.always_allow(c.id).unwrap();
    assert_eq!(done.approved.len(), 1);
    assert_eq!(first.await.unwrap().unwrap(), json!("updated"));

    // The same agent, tool and workspace: no prompt.
    runs_now(&reg, ADA, "update_app", json!({"app": "minecraft"}))
        .await
        .unwrap();
    let lines = audit_lines(&log);
    let auto = lines
        .iter()
        .find(|l| l["action"] == "tool.auto_approved")
        .expect("audited");
    assert_eq!(auto["actor"], "rule");
    assert!(auto["detail"]
        .as_str()
        .unwrap()
        .starts_with(&format!("rule {}", done.rule.id)));
    assert!(lines
        .iter()
        .any(|l| l["action"] == "permissions.rule_added" && l["actor"] == "ui"));

    // Another workspace of the same agent still asks.
    let other = spawn(&reg, 2, ADA, "update_app", json!({"app": "nextcloud"}));
    let c = pending(&reg, 1).await.remove(0);
    assert_eq!(c.rule.unwrap().key.target.as_deref(), Some("nextcloud"));
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(other.await.unwrap(), Err(ToolError::Denied));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn rules_and_bypass_never_cross_agents_or_owners() {
    let reg = registry(None, Permissions::in_memory());
    // Ada may restart (no target) and update minecraft for good.
    for (tool, args) in [
        ("restart", json!({})),
        ("update_app", json!({"app": "minecraft"})),
    ] {
        let call = spawn(&reg, 1, ADA, tool, args);
        let c = pending(&reg, 1).await.remove(0);
        reg.always_allow(c.id).unwrap();
        call.await.unwrap().unwrap();
    }
    assert_eq!(reg.permissions().rules().len(), 2);

    // Bo's restart still asks: rules belong to one agent.
    let bo = spawn(&reg, 2, BO, "restart", json!({}));
    let c = pending(&reg, 1).await.remove(0);
    assert_eq!(
        c.rule.as_ref().unwrap().key.principal,
        Principal::Agent(BO.into())
    );
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(bo.await.unwrap(), Err(ToolError::Denied));

    // Bo touching Ada's minecraft waits for the user's own yes, rule or bypass: neither
    // answers it, "Always allow" can't make a rule of it, switching the bypass on skips it.
    reg.set_bypass(true).unwrap();
    for (who, app) in [(BO, "minecraft"), (ADA, "valheim")] {
        let call = spawn(&reg, 3, who, "update_app", json!({"app": app}));
        let c = pending(&reg, 1).await.remove(0);
        assert!(c.user_only && c.rule.is_none(), "{c:?}");
        assert!(matches!(
            reg.always_allow(c.id),
            Err(AlwaysAllowError::NotOffered)
        ));
        assert!(reg.set_bypass(true).unwrap().is_empty());
        assert_eq!(reg.confirmations().pending().len(), 1);
        reg.confirmations().resolve(c.id, Verdict::Denied);
        assert_eq!(call.await.unwrap(), Err(ToolError::Denied));
    }
    assert_eq!(reg.permissions().rules().len(), 2);
    // Bad arguments too.
    assert!(matches!(
        runs_now(&reg, ADA, "update_app", json!({})).await,
        Err(ToolError::BadArgs(_))
    ));
}

#[tokio::test]
async fn a_rename_keeps_the_rule() {
    // Rules hold the agent's id; the display name is looked up only for wording.
    struct Names(&'static str);
    impl AgentNames for Names {
        fn name_of(&self, id: &str) -> Option<String> {
            (id == ADA).then(|| self.0.to_string())
        }
        fn resolve_id(&self, n: &str) -> Result<String, String> {
            Err(format!("no agent `{n}`"))
        }
    }
    let permissions = Permissions::in_memory();
    let reg = registry(None, permissions.clone()).with_agents(Arc::new(Names("Ada")));
    let call = spawn(&reg, 1, ADA, "update_app", json!({"app": "minecraft"}));
    let c = pending(&reg, 1).await.remove(0);
    let rule = reg.always_allow(c.id).unwrap().rule;
    call.await.unwrap().unwrap();
    assert!(reg.describe_rule(&rule).contains("agent Ada"));
    // Ada is now called Ida: same id, same rule.
    let reg = registry(None, permissions).with_agents(Arc::new(Names("Ida")));
    runs_now(&reg, ADA, "update_app", json!({"app": "minecraft"}))
        .await
        .unwrap();
    assert!(reg.describe_rule(&rule).contains("agent Ida"));
}

#[tokio::test]
async fn a_new_rule_approves_matching_pending_requests_only() {
    let dir = scratch("pending");
    let log = dir.join("audit.jsonl");
    let reg = registry(Some(AuditLog::new(&log)), Permissions::in_memory());
    let a = spawn(&reg, 1, ADA, "update_app", json!({"app": "minecraft"}));
    pending(&reg, 1).await;
    let b = spawn(&reg, 2, ADA, "update_app", json!({"app": "minecraft"}));
    pending(&reg, 2).await;
    let other = spawn(&reg, 3, ADA, "update_app", json!({"app": "nextcloud"}));
    let all = pending(&reg, 3).await;

    let done = reg.always_allow(all[0].id).unwrap();
    assert_eq!(
        done.approved.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![all[0].id, all[1].id]
    );
    a.await.unwrap().unwrap();
    b.await.unwrap().unwrap();
    // nextcloud still waits.
    let left = reg.confirmations().pending();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, all[2].id);
    reg.confirmations().resolve(left[0].id, Verdict::Denied);
    assert_eq!(other.await.unwrap(), Err(ToolError::Denied));

    let lines = audit_lines(&log);
    let auto: Vec<_> = lines
        .iter()
        .filter(|l| l["action"] == "confirmation.auto_approved")
        .collect();
    assert_eq!(auto.len(), 1);
    assert_eq!(auto[0]["actor"], "rule");
    assert!(lines
        .iter()
        .any(|l| l["action"] == "confirmation.approved" && l["actor"] == "ui"));
    // A gone confirmation cannot become a rule.
    assert!(matches!(
        reg.always_allow(all[0].id),
        Err(AlwaysAllowError::Gone(_))
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bypass_approves_everything_pending_and_new_and_audits_it() {
    let dir = scratch("bypass");
    let log = dir.join("audit.jsonl");
    let reg = registry(Some(AuditLog::new(&log)), Permissions::in_memory());
    let a = spawn(&reg, 1, ADA, "update_app", json!({"app": "minecraft"}));
    pending(&reg, 1).await;
    let b = spawn(&reg, 2, BO, "restart", json!({}));
    pending(&reg, 2).await;

    assert_eq!(reg.set_bypass(true).unwrap().len(), 2);
    a.await.unwrap().unwrap();
    b.await.unwrap().unwrap();
    runs_now(&reg, BO, "update_app", json!({"app": "valheim"}))
        .await
        .unwrap();
    // The user's own calls too (they have no rules, only the bypass).
    let mut user = TaskContext {
        scope: Scope::User,
        ..TaskContext::default()
    };
    reg.call(0, &mut user, "update_app", json!({"app": "minecraft"}))
        .await
        .unwrap();

    let lines = audit_lines(&log);
    let count = |action: &str| {
        lines
            .iter()
            .filter(|l| l["action"] == action && l["actor"] == "bypass")
            .count()
    };
    assert_eq!(count("confirmation.auto_approved"), 2);
    assert_eq!(count("tool.auto_approved"), 2);
    assert!(lines
        .iter()
        .any(|l| l["action"] == "permissions.bypass" && l["detail"] == "on"));

    // Off again: prompts are back. No rule was saved along the way.
    assert!(reg.set_bypass(false).unwrap().is_empty());
    let c = spawn(&reg, 3, BO, "restart", json!({}));
    let p = pending(&reg, 1).await.remove(0);
    reg.confirmations().resolve(p.id, Verdict::Denied);
    assert_eq!(c.await.unwrap(), Err(ToolError::Denied));
    assert!(reg.permissions().rules().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn only_the_owner_makes_rules() {
    let reg = registry(None, Permissions::in_memory());
    // A plain yes is a one-off: no rule.
    let call = spawn(&reg, 1, ADA, "update_app", json!({"app": "minecraft"}));
    let c = pending(&reg, 1).await.remove(0);
    reg.confirmations().resolve(c.id, Verdict::Approved);
    call.await.unwrap().unwrap();
    assert!(reg.permissions().rules().is_empty());
    // No tool an agent can see or call manages permissions: the registry's tools are the
    // only way in, and none of them reaches `Permissions` (its changes are crate-private and
    // go through `always_allow`, `remove_rule` and `set_bypass`, called by the web UI/API).
    for spec in reg.visible_to(&agent(ADA), None) {
        assert!(!spec.name.contains("permission") && !spec.name.contains("rule"));
    }
    // The user's own requests offer no rule.
    let mut user = TaskContext {
        scope: Scope::User,
        ..TaskContext::default()
    };
    let reg2 = reg.clone();
    let call = tokio::spawn(async move {
        reg2.call(0, &mut user, "update_app", json!({"app": "minecraft"}))
            .await
    });
    let c = pending(&reg, 1).await.remove(0);
    assert!(c.rule.is_none());
    assert!(matches!(
        reg.always_allow(c.id),
        Err(AlwaysAllowError::NotOffered)
    ));
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(call.await.unwrap(), Err(ToolError::Denied));
}

#[tokio::test]
async fn rules_and_bypass_survive_a_restart() {
    let dir = scratch("restart");
    let path = dir.join("permissions.json");
    {
        let reg = registry(None, Permissions::open(&path).unwrap());
        let call = spawn(&reg, 1, ADA, "update_app", json!({"app": "minecraft"}));
        let c = pending(&reg, 1).await.remove(0);
        reg.always_allow(c.id).unwrap();
        call.await.unwrap().unwrap();
    }
    let reg = registry(None, Permissions::open(&path).unwrap());
    runs_now(&reg, ADA, "update_app", json!({"app": "minecraft"}))
        .await
        .unwrap();
    let rule = reg.permissions().rules().remove(0);
    reg.remove_rule(rule.id).unwrap().unwrap();
    reg.set_bypass(true).unwrap();
    let reg = registry(None, Permissions::open(&path).unwrap());
    assert!(reg.permissions().rules().is_empty());
    assert!(reg.permissions().bypass());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn refusals_with_an_owner_are_audited_with_both_sides() {
    let dir = scratch("cross-refused");
    let path = dir.join("audit.jsonl");
    let reg = registry(Some(AuditLog::new(&path)), Permissions::in_memory());
    reg.set_bypass(true).unwrap();
    // Another device's workspace is that device's lead agent's: refused, bypass or not.
    let mut lead = TaskContext {
        scope: Scope::Lead {
            device: "d-0a1b2c3d".into(),
        },
        ..TaskContext::default()
    };
    let r = reg
        .call(1, &mut lead, "update_app", json!({"app": "far"}))
        .await;
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    let crossed: Vec<Value> = audit_lines(&path)
        .into_iter()
        .filter(|l| l["action"] == "tool.cross_owner")
        .collect();
    assert_eq!(crossed.len(), 1);
    assert_eq!(
        crossed[0]["detail"],
        "refused: the lead agent of d-0a1b2c3d → update `far` of agent d-ffffffff.a1"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn full_access_approves_only_the_agents_own_confirmations_and_audits_them() {
    let dir = scratch("full");
    let log = dir.join("audit.jsonl");
    let reg = ToolRegistry::new(
        vec![
            update(),
            restart(),
            // Only the owner may approve, every time (like `schedule_job`).
            TestTool::new("plan")
                .owner_approval("a standing order")
                .reply(|_| json!("planned"))
                .arc(),
            // Owner-only, and the label policy asks (like `delete_agent`).
            TestTool::new("remove")
                .effect(Effect::Destructive)
                .owner_only()
                .reply(|_| json!("removed"))
                .arc(),
        ],
        Some(AuditLog::new(&log)),
    )
    .with_owners(Arc::new(Owners(BTreeMap::from([
        ("minecraft", ADA),
        ("valheim", BO),
    ]))))
    .with_permissions(Permissions::in_memory())
    .with_confirm_timeout(Duration::from_secs(10));
    let ada = Principal::Agent(ADA.into());
    // Needs the developer option.
    assert!(matches!(
        reg.set_access(&ada, true),
        Err(PermissionsError::FullAccessOff)
    ));
    reg.set_full_access_allowed(true).unwrap();
    reg.set_access(&ada, true).unwrap();

    // Ada's own calls run without asking: a tool without a target, her own workspace, and
    // her agent CLI's own tools (confirmed through `confirm`).
    runs_now(&reg, ADA, "restart", json!({})).await.unwrap();
    runs_now(&reg, ADA, "update_app", json!({"app": "minecraft"}))
        .await
        .unwrap();
    let shell = RuleOffer::new(&Scope::Agent { id: ADA.into() }, "agent:execute", None);
    reg.confirm(4, shell, "agent:execute", &json!({}), "run a command")
        .await
        .unwrap();
    assert!(reg.confirmations().pending().is_empty());

    // Not hers: Bo's calls, Bo's workspace (the user's own yes), owner-only steps.
    for (task, who, tool, args) in [
        (5, BO, "restart", json!({})),
        (6, ADA, "update_app", json!({"app": "valheim"})),
        (7, ADA, "plan", json!({})),
        (8, ADA, "remove", json!({})),
    ] {
        let call = spawn(&reg, task, who, tool, args);
        let c = pending(&reg, 1).await.remove(0);
        reg.confirmations().resolve(c.id, Verdict::Denied);
        assert_eq!(call.await.unwrap(), Err(ToolError::Denied), "{tool}");
    }

    // The lead agent on Full access: its call on Ada's workspace is not its own.
    reg.set_access(&Principal::Lead, true).unwrap();
    let lead = {
        let reg = reg.clone();
        tokio::spawn(async move {
            let mut ctx = TaskContext {
                scope: Scope::Lead {
                    device: "d-0a1b2c3d".into(),
                },
                delegate: true,
                ..TaskContext::default()
            };
            reg.call(9, &mut ctx, "update_app", json!({"app": "minecraft"}))
                .await
        })
    };
    let c = pending(&reg, 1).await.remove(0);
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(lead.await.unwrap(), Err(ToolError::Denied));

    // Back on Ask: Ada is asked again.
    reg.set_access(&ada, false).unwrap();
    let call = spawn(&reg, 10, ADA, "restart", json!({}));
    let c = pending(&reg, 1).await.remove(0);
    reg.confirmations().resolve(c.id, Verdict::Approved);
    call.await.unwrap().unwrap();

    let lines = audit_lines(&log);
    let auto: Vec<&Value> = lines
        .iter()
        .filter(|l| l["action"] == "tool.auto_approved")
        .collect();
    assert_eq!(auto.len(), 3);
    assert!(auto.iter().all(|l| l["actor"] == "bypass"
        && l["detail"]
            .as_str()
            .unwrap()
            .starts_with("bypass (full access)")));
    let access: Vec<&str> = lines
        .iter()
        .filter(|l| l["action"] == "permissions.access")
        .map(|l| l["detail"].as_str().unwrap())
        .collect();
    assert_eq!(access.len(), 3);
    assert!(access[0].ends_with(": full") && access[2].ends_with(": ask"));
    assert!(lines
        .iter()
        .any(|l| l["action"] == "permissions.full_access_allowed" && l["actor"] == "ui"));
    let _ = std::fs::remove_dir_all(dir);
}
