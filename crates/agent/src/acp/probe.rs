//! Checking that a program speaks ACP before Melissa saves it as a provider (Settings →
//! Providers → Add provider): start it, send `initialize`, read the answer, stop it. No
//! session is opened and no prompt is sent.

use std::{path::Path, process::Stdio, time::Duration};

use agent_client_protocol::{
    schema::ProtocolVersion,
    v1::{
        schema::{ClientCapabilities, Implementation, InitializeRequest},
        ConnectionTo,
    },
    Agent, Client, ConnectTo,
};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{client::StderrTail, transport};
use crate::AgentError;

/// How long a probe may take, start to answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// What an agent said about itself in `initialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeInfo {
    /// "name version" from `agentInfo`, if it sent one.
    pub agent: Option<String>,
    /// Its sign-in methods' names.
    pub auth_methods: Vec<String>,
    /// It accepts MCP servers over HTTP (Melissa's tools need that).
    pub mcp_http: bool,
}

/// `initialize` over an established transport.
pub async fn probe_on<T>(transport: T) -> Result<ProbeInfo, AgentError>
where
    T: ConnectTo<Client> + Send + 'static,
{
    let init = Client
        .v1()
        .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
            let req = InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(ClientCapabilities::new())
                .client_info(Implementation::new("melissa", env!("CARGO_PKG_VERSION")));
            cx.send_request(req).block_task().await
        })
        .await
        .map_err(|e| AgentError::Protocol(e.to_string()))?;
    if init.protocol_version != ProtocolVersion::V1 {
        return Err(AgentError::Protocol(format!(
            "it speaks ACP protocol {:?}; Melissa supports 1",
            init.protocol_version
        )));
    }
    Ok(ProbeInfo {
        agent: init
            .agent_info
            .as_ref()
            .map(|i| format!("{} {}", i.name, i.version)),
        auth_methods: init
            .auth_methods
            .iter()
            .map(|m| m.name().to_string())
            .collect(),
        mcp_http: init.agent_capabilities.mcp_capabilities.http,
    })
}

/// Start `program args` in `cwd` with only `env` set, run [`probe_on`] and stop it. The
/// program runs as Melissa's user without the sandbox, like an installer: the owner chose it
/// (or Melissa checked the download's checksum), and it only answers one request.
pub async fn probe(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<ProbeInfo, AgentError> {
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| AgentError::Spawn {
            command: program.display().to_string(),
            source,
        })?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(AgentError::Protocol("agent pipes missing".into()));
    };
    let tail = StderrTail::default();
    tokio::spawn({
        let tail = tail.clone();
        async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tail.push(line);
            }
        }
    });
    let result =
        tokio::time::timeout(timeout, probe_on(transport::lines(stdout, stdin, None))).await;
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    match result {
        Ok(Ok(info)) => Ok(info),
        Ok(Err(e)) => Err(match e {
            AgentError::Protocol(m) => AgentError::Protocol(format!("{m}{}", tail.summary())),
            other => other,
        }),
        Err(_) => Err(AgentError::Timeout(timeout)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake ACP agent: answers `initialize` with `reply` (`ID` is replaced by the request
    /// id), then waits.
    fn fake_agent(dir: &Path, name: &str, reply: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        // The request id is a string (a UUID) or a number; it goes back as it came.
        let script = format!(
            "#!/bin/sh\nread -r line\nid=$(printf '%s' \"$line\" | sed -n 's/^{{\"jsonrpc\":\"2.0\",\"id\":\\(\"[^\"]*\"\\|[0-9]*\\),.*/\\1/p')\n\
             printf '%s\\n' '{reply}' | sed \"s/ID/$id/\"\ncat >/dev/null\n"
        );
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn env() -> Vec<(String, String)> {
        vec![("PATH".into(), "/usr/bin:/bin".into())]
    }

    #[tokio::test]
    async fn a_fake_agent_answers_initialize() {
        let dir = std::env::temp_dir().join(format!("melissa-probe-ok-{}", std::process::id()));
        let agent = fake_agent(
            &dir,
            "fake-agent",
            r#"{"jsonrpc":"2.0","id":ID,"result":{"protocolVersion":1,"agentCapabilities":{"mcpCapabilities":{"http":true}},"agentInfo":{"name":"fake","version":"0.1.0"},"authMethods":[{"id":"key","name":"API key"}]}}"#,
        );
        let info = probe(&agent, &["acp".into()], &dir, &env(), PROBE_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(info.agent.as_deref(), Some("fake 0.1.0"));
        assert_eq!(info.auth_methods, ["API key"]);
        assert!(info.mcp_http);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn non_acp_programs_fail_or_time_out() {
        let dir = std::env::temp_dir().join(format!("melissa-probe-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Exits at once: the connection closes before an answer.
        let r = probe(
            Path::new("/bin/true"),
            &[],
            &dir,
            &env(),
            Duration::from_secs(5),
        )
        .await;
        assert!(r.is_err(), "{r:?}");
        // Prints something that isn't ACP and keeps running: an error or the time limit.
        let chatty = fake_agent(&dir, "chatty", "hello, I am not an agent");
        let r = probe(&chatty, &[], &dir, &env(), Duration::from_secs(2)).await;
        assert!(r.is_err(), "{r:?}");
        // Not there at all.
        let r = probe(&dir.join("missing"), &[], &dir, &env(), PROBE_TIMEOUT).await;
        assert!(matches!(r, Err(AgentError::Spawn { .. })), "{r:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
