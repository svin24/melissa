//! Recorded ACP sessions ("transcripts") for tests.
//!
//! Format: JSON Lines. The first line is a header
//! `{"type":"transcript","agent":"grok","agent_version":"…","protocol":"acp-v1","notes":"…"}`;
//! every other line is one JSON-RPC message seen by Melissa (the client), in order:
//! `{"dir":"send","msg":{…}}` (Melissa → agent) or `{"dir":"recv","msg":{…}}` (agent → Melissa).
//!
//! The [`Recorder`] writes transcripts from real sessions (enable with `record_dir` on an ACP
//! backend) and scrubs what it knows is private: the home directory, the working directory,
//! the MCP session token, the user and host name, and every UUID (session ids, agent ids),
//! which become fixed placeholders. **Still review every transcript by hand before
//! committing it** (prompts and replies can contain anything).
//!
//! In tests, a replay agent plays the agent's side: it expects each `send` message (matched
//! structurally; the string `"<any>"` matches any value, `"<contains:text>"` any string
//! containing `text`, request ids are mapped) and emits each `recv` message. Test-only steps:
//! `{"dir":"mcp","msg":{"call":{"name":…,"arguments":…},"expect":…}}` makes the agent call
//! one of Melissa's tools through the MCP session it was given (as a real agent would), and
//! `{"dir":"exit"}` makes it die (both pipes closed).

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde_json::{json, Value};

/// Which way a message went, seen from Melissa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Melissa → agent.
    Send,
    /// Agent → Melissa.
    Recv,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Recv => "recv",
        }
    }
}

/// Writes a scrubbed transcript of one session.
pub struct Recorder {
    path: PathBuf,
    inner: Mutex<RecorderInner>,
}

struct RecorderInner {
    file: std::fs::File,
    scrub: Scrubber,
}

impl Recorder {
    /// Create `<dir>/<name>-<unix-seconds>-<pid>.jsonl` (mode 0600) and write the header.
    /// `secrets` are strings replaced by `<redacted>` (e.g. the MCP token).
    pub fn create(
        dir: &Path,
        name: &str,
        home: Option<&Path>,
        workdir: &Path,
        secrets: Vec<String>,
    ) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(dir)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("{name}-{ts}-{}.jsonl", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let header = json!({
            "type": "transcript",
            "agent": name,
            "protocol": "acp-v1",
            "notes": "Recorded by Melissa. Scrubbed: home → /home/user, workdir → /work, \
                      MCP token → <redacted>, user/host names, UUIDs → fixed ids. Review by hand.",
        });
        writeln!(file, "{header}")?;
        Ok(Self {
            path,
            inner: Mutex::new(RecorderInner {
                file,
                scrub: Scrubber::new(home, workdir, secrets),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one message. Recording is best effort: errors are logged, never fatal.
    pub fn record(&self, dir: Direction, line: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let scrubbed = inner.scrub.apply(line);
        let msg: Value = serde_json::from_str(&scrubbed).unwrap_or(Value::String(scrubbed));
        let entry = json!({"dir": dir.as_str(), "msg": msg});
        if let Err(e) = writeln!(inner.file, "{entry}") {
            tracing::warn!(path = %self.path.display(), "ACP transcript write failed: {e}");
        }
    }
}

/// Replaces private strings in transcript lines.
pub struct Scrubber {
    /// (needle, replacement), longest needles first.
    fixed: Vec<(String, String)>,
    /// Whole words replaced (user and host name).
    words: Vec<(String, String)>,
    uuids: HashMap<String, String>,
}

impl Scrubber {
    pub fn new(home: Option<&Path>, workdir: &Path, secrets: Vec<String>) -> Self {
        let mut fixed: Vec<(String, String)> = secrets
            .into_iter()
            .filter(|s| s.len() >= 8)
            .map(|s| (s, "<redacted>".to_string()))
            .collect();
        fixed.push((workdir.display().to_string(), "/work".into()));
        if let Some(home) = home {
            let home = home.display().to_string();
            // Fedora Atomic: /home is a symlink to /var/home; paths show up both ways.
            if let Some(rest) = home.strip_prefix("/var/home/") {
                fixed.push((format!("/home/{rest}"), "/home/user".into()));
            }
            fixed.push((home, "/home/user".into()));
        }
        fixed.retain(|(n, _)| n.len() > 1);
        fixed.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
        let mut words = Vec::new();
        for (var, replacement) in [("USER", "user"), ("LOGNAME", "user")] {
            if let Some(v) = std::env::var_os(var).and_then(|v| v.into_string().ok()) {
                words.push((v, replacement.to_string()));
            }
        }
        if let Ok(host) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
            words.push((host.trim().to_string(), "host".to_string()));
        }
        words.retain(|(w, _)| w.len() >= 3);
        Self {
            fixed,
            words,
            uuids: HashMap::new(),
        }
    }

    pub fn apply(&mut self, line: &str) -> String {
        let mut out = line.to_string();
        for (needle, replacement) in &self.fixed {
            out = out.replace(needle.as_str(), replacement);
        }
        out = self.replace_uuids(&out);
        for (word, replacement) in &self.words {
            out = replace_word(&out, word, replacement);
        }
        out
    }

    /// Replace each distinct UUID with `00000000-0000-4000-8000-<n>` (stable per transcript).
    fn replace_uuids(&mut self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < bytes.len() {
            if i + 36 <= bytes.len() && is_uuid(&bytes[i..i + 36]) {
                let found = &text[i..i + 36];
                let next = self.uuids.len() + 1;
                let fixed = self
                    .uuids
                    .entry(found.to_ascii_lowercase())
                    .or_insert_with(|| format!("00000000-0000-4000-8000-{next:012}"));
                out.push_str(fixed);
                i += 36;
            } else {
                // Copy one UTF-8 character.
                let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&text[i..i + ch_len]);
                i += ch_len;
            }
        }
        out
    }
}

fn is_uuid(b: &[u8]) -> bool {
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// Replace `word` where it is not part of a longer identifier.
fn replace_word(text: &str, word: &str, replacement: &str) -> String {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(word) {
        let before = rest[..pos].chars().next_back();
        let after = rest[pos + word.len()..].chars().next();
        out.push_str(&rest[..pos]);
        if before.is_some_and(is_ident) || after.is_some_and(is_ident) {
            out.push_str(word);
        } else {
            out.push_str(replacement);
        }
        rest = &rest[pos + word.len()..];
    }
    out.push_str(rest);
    out
}

/// Does `actual` match the `expected` pattern? Objects match if every expected key matches
/// (extra keys in `actual` are fine); arrays must have the same length; `"<any>"` matches
/// anything.
pub fn matches(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::String(s), _) if s == "<any>" => true,
        (Value::String(s), Value::String(a)) if s.starts_with("<contains:") && s.ends_with('>') => {
            a.contains(&s["<contains:".len()..s.len() - 1])
        }
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|av| matches(v, av))),
        (Value::Array(e), Value::Array(a)) => {
            e.len() == a.len() && e.iter().zip(a).all(|(e, a)| matches(e, a))
        }
        _ => expected == actual,
    }
}

#[cfg(test)]
pub(crate) mod agent {
    //! The replay agent (tests only).

    use std::collections::HashMap;

    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::matches;
    use crate::mcp::McpServer;

    /// Which way a step goes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Dir {
        /// The client sends this (expected).
        Send,
        /// The agent sends this.
        Recv,
        /// The agent calls a Melissa tool over MCP (test only).
        Mcp,
        /// The agent dies (test only).
        Exit,
    }

    /// One step of a transcript.
    #[derive(Debug, Clone)]
    pub struct Step {
        pub dir: Dir,
        pub msg: Value,
    }

    impl Step {
        pub fn send(msg: Value) -> Self {
            Self {
                dir: Dir::Send,
                msg,
            }
        }
        pub fn recv(msg: Value) -> Self {
            Self {
                dir: Dir::Recv,
                msg,
            }
        }
        /// Call tool `name`; the result must match `expect`.
        #[allow(dead_code)] // used by the Queen tests (next commit)
        pub fn mcp(name: &str, arguments: Value, expect: Value) -> Self {
            Self {
                dir: Dir::Mcp,
                msg: json!({"call": {"name": name, "arguments": arguments}, "expect": expect}),
            }
        }
        pub fn exit() -> Self {
            Self {
                dir: Dir::Exit,
                msg: Value::Null,
            }
        }
    }

    /// Parse a transcript (header line skipped).
    pub fn parse(text: &str) -> Vec<Step> {
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Value>(l).expect("transcript line is JSON"))
            .filter(|v| v.get("type").is_none())
            .map(|v| Step {
                dir: match v["dir"].as_str() {
                    Some("send") => Dir::Send,
                    Some("mcp") => Dir::Mcp,
                    Some("exit") => Dir::Exit,
                    _ => Dir::Recv,
                },
                msg: v["msg"].clone(),
            })
            .collect()
    }

    /// Play the agent's side of `steps` over the given streams. Returns an error describing
    /// the first mismatch, or extra messages after the transcript ended.
    pub async fn run<R, W>(steps: Vec<Step>, reader: R, writer: W) -> Result<(), String>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        run_with(steps, reader, writer, None).await
    }

    /// Like [`run`]; `mcp` serves the agent's `mcp` steps (the session token comes from the
    /// MCP URL the client passed in `session/new` or `session/resume`).
    pub async fn run_with<R, W>(
        steps: Vec<Step>,
        reader: R,
        mut writer: W,
        mcp: Option<McpServer>,
    ) -> Result<(), String>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let mut lines = BufReader::new(reader).lines();
        // Transcript request id → id the client actually used.
        let mut ids: HashMap<String, Value> = HashMap::new();
        let mut token = None;
        for (n, step) in steps.into_iter().enumerate() {
            if step.dir == Dir::Exit {
                return Ok(());
            }
            if step.dir == Dir::Mcp {
                let server = mcp.as_ref().ok_or(format!("step {n}: no MCP server"))?;
                let token: &String = token.as_ref().ok_or(format!("step {n}: no MCP session"))?;
                let request = json!({"jsonrpc": "2.0", "id": n, "method": "tools/call",
                                     "params": step.msg["call"]});
                let reply = server
                    .handle(token, request)
                    .await
                    .map_err(|_| format!("step {n}: unknown MCP session"))?
                    .ok_or(format!("step {n}: no MCP reply"))?;
                if !matches(&step.msg["expect"], &reply) {
                    return Err(format!(
                        "step {n}: MCP reply {reply}\n         expected {}",
                        step.msg["expect"]
                    ));
                }
                continue;
            }
            if step.dir == Dir::Send {
                let line = lines
                    .next_line()
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("step {n}: client closed, expected {}", step.msg))?;
                let actual: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                let mut expected = step.msg.clone();
                let is_request = expected.get("method").is_some();
                if let (true, Some(id), Some(actual_id)) =
                    (is_request, expected.get("id").cloned(), actual.get("id"))
                {
                    ids.insert(id.to_string(), actual_id.clone());
                    expected["id"] = actual_id.clone();
                }
                if !matches(&expected, &actual) {
                    return Err(format!(
                        "step {n}: expected {expected}\n         got      {actual}"
                    ));
                }
                if matches!(
                    actual["method"].as_str(),
                    Some("session/new" | "session/resume")
                ) {
                    token = actual["params"]["mcpServers"][0]["url"]
                        .as_str()
                        .and_then(|u| u.rsplit('/').next())
                        .map(str::to_string);
                }
            } else {
                let mut msg = step.msg;
                // A response to one of the client's requests: use the client's id.
                if msg.get("method").is_none() {
                    if let Some(actual) = msg.get("id").and_then(|id| ids.get(&id.to_string())) {
                        msg["id"] = actual.clone();
                    }
                }
                writer
                    .write_all(format!("{msg}\n").as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                writer.flush().await.map_err(|e| e.to_string())?;
            }
        }
        // Script done: the client may close; anything else it sends is unexpected.
        match lines.next_line().await {
            Ok(None) | Err(_) => Ok(()),
            Ok(Some(extra)) => Err(format!("unexpected message after the transcript: {extra}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubber_removes_private_strings() {
        let mut s = Scrubber {
            fixed: vec![
                ("/var/home/alice/work/task-1".into(), "/work".into()),
                ("/var/home/alice".into(), "/home/user".into()),
                ("0123456789abcdef".into(), "<redacted>".into()),
            ],
            words: vec![
                ("alice".into(), "user".into()),
                ("laptop7".into(), "host".into()),
            ],
            uuids: HashMap::new(),
        };
        let line = r#"{"cwd":"/var/home/alice/work/task-1","home":"/var/home/alice/.grok","url":"http://127.0.0.1:8080/mcp/0123456789abcdef","sessionId":"01A1ED73-93a7-7c01-9c83-5f092143800e","again":"01a1ed73-93a7-7c01-9c83-5f092143800e-5","other":"b8dabb51-a357-57a7-b622-d91ebf07c6cf","hostname":"laptop7","who":"alice","name":"malice_x"}"#;
        let out = s.apply(line);
        assert!(!out.contains("alice\""), "{out}");
        assert!(out.contains(r#""cwd":"/work""#));
        assert!(out.contains(r#""home":"/home/user/.grok""#));
        assert!(out.contains("/mcp/<redacted>"));
        assert!(out.contains(r#""sessionId":"00000000-0000-4000-8000-000000000001""#));
        assert!(out.contains(r#""again":"00000000-0000-4000-8000-000000000001-5""#));
        assert!(out.contains(r#""other":"00000000-0000-4000-8000-000000000002""#));
        assert!(out.contains(r#""hostname":"host""#));
        assert!(out.contains(r#""who":"user""#));
        assert!(
            out.contains(r#""name":"malice_x""#),
            "words inside identifiers stay"
        );
    }

    #[test]
    fn pattern_matching() {
        let pat = json!({"method": "initialize", "params": {"clientInfo": "<any>", "v": [1, 2]}});
        assert!(matches(
            &pat,
            &json!({"method": "initialize", "id": 0, "params": {"clientInfo": {"name": "m"}, "v": [1, 2]}})
        ));
        assert!(!matches(
            &pat,
            &json!({"method": "initialize", "params": {"v": [1, 2]}})
        ));
        assert!(!matches(
            &pat,
            &json!({"method": "initialize", "params": {"clientInfo": 1, "v": [1]}})
        ));
        assert!(matches(&json!("<contains:lo w>"), &json!("hello world")));
        assert!(!matches(&json!("<contains:xyz>"), &json!("hello world")));
        assert!(!matches(&json!("<contains:1>"), &json!(1)));
    }
}
