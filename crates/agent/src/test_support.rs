//! Helpers shared by this crate's unit tests: temp folders and a configurable fake tool.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use serde_json::{json, Value};

use crate::{
    tools::{Effect, Labels, Target, TaskContext, Tool, ToolError, ToolSpec},
    BoxFuture,
};

/// A fresh, empty directory under the system temp dir: one per call, named for `name` and
/// this test process.
pub fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "melissa-agent-test-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fresh path under the system temp dir that does not exist yet (for stores that must
/// create their folder themselves).
pub fn fresh_path(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::remove_dir(&dir).unwrap();
    dir
}

/// A fake tool: read-only and answering `"ok"` unless configured otherwise.
pub struct TestTool {
    name: &'static str,
    labels: Labels,
    params: Value,
    reply: fn(Value) -> Value,
    target: Option<Target>,
    lead_only: bool,
    owner_only: bool,
    owner_citation: Option<&'static str>,
    owner_approval: Option<&'static str>,
}

impl TestTool {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            labels: Labels::READ_ONLY,
            params: json!({"type": "object"}),
            reply: |_| json!("ok"),
            target: None,
            lead_only: false,
            owner_only: false,
            owner_citation: None,
            owner_approval: None,
        }
    }

    /// Read-only otherwise, with this effect.
    pub fn effect(mut self, effect: Effect) -> Self {
        self.labels = Labels {
            effect,
            ..Labels::READ_ONLY
        };
        self
    }

    pub fn labels(mut self, labels: Labels) -> Self {
        self.labels = labels;
        self
    }

    pub fn params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }

    /// What a call answers, from its arguments.
    pub fn reply(mut self, reply: fn(Value) -> Value) -> Self {
        self.reply = reply;
        self
    }

    pub fn target(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }

    pub fn lead_only(mut self) -> Self {
        self.lead_only = true;
        self
    }

    pub fn owner_only(mut self) -> Self {
        self.owner_only = true;
        self
    }

    /// Calls may cite one of the owner's messages in this argument.
    pub fn citing(mut self, arg: &'static str) -> Self {
        self.owner_citation = Some(arg);
        self
    }

    /// Every call needs the owner's own yes, for this reason.
    pub fn owner_approval(mut self, reason: &'static str) -> Self {
        self.owner_approval = Some(reason);
        self
    }

    pub fn arc(self) -> Arc<dyn Tool> {
        Arc::new(self)
    }
}

impl Tool for TestTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name,
            description: "test",
            labels: self.labels,
            params: self.params.clone(),
        }
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        let reply = self.reply;
        Box::pin(async move { Ok(reply(args)) })
    }

    fn lead_only(&self) -> bool {
        self.lead_only
    }

    fn owner_only(&self) -> bool {
        self.owner_only
    }

    fn owner_citation(&self) -> Option<&'static str> {
        self.owner_citation
    }

    fn target(&self) -> Option<Target> {
        self.target
    }

    fn owner_approval(&self, _: &TaskContext, _: &Value) -> Result<Option<String>, ToolError> {
        Ok(self.owner_approval.map(str::to_string))
    }
}
