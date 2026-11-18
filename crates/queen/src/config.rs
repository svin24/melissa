//! Queen bee configuration, read from `$XDG_CONFIG_HOME/melissa/queen.toml`.
//! Every field is optional; a missing file means "use the defaults".

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context};
use melissa_agent::CliSpec;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QueenConfig {
    /// Address for the web UI. Must be loopback until accounts and sessions exist.
    pub listen: SocketAddr,
    /// Maximum run time for one agent task, in seconds.
    pub task_timeout_secs: u64,
    /// Agent CLI backends. The built-in `echo` backend is always present in addition.
    #[serde(rename = "backend")]
    pub backends: Vec<CliSpec>,
}

impl Default for QueenConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            task_timeout_secs: 300,
            backends: default_backends(),
        }
    }
}

/// Headless invocations of the agent CLIs Melissa knows about. Backends whose binary is
/// missing show up as "not installed" and cannot be selected.
pub fn default_backends() -> Vec<CliSpec> {
    vec![
        CliSpec::new("claude", "claude", &["-p", "{prompt}"]),
        CliSpec::new("grok", "grok", &["-p", "{prompt}"]),
        CliSpec::new("opencode", "opencode", &["run", "{prompt}"]),
    ]
}

impl QueenConfig {
    /// Load from `path`. A missing file yields the defaults; a malformed one is an error.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let config: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.listen.ip().is_loopback() {
            bail!(
                "listen = {} is not a loopback address; the UI has no login yet, so only \
                 127.0.0.1/::1 are allowed",
                self.listen
            );
        }
        if self.task_timeout_secs == 0 {
            bail!("task_timeout_secs must be greater than 0");
        }
        let mut names = vec!["echo"];
        for b in &self.backends {
            if names.contains(&b.name.as_str()) {
                bail!("duplicate backend name `{}`", b.name);
            }
            names.push(&b.name);
        }
        Ok(())
    }

    pub fn task_timeout(&self) -> Duration {
        Duration::from_secs(self.task_timeout_secs)
    }
}

/// XDG base directories for Melissa (`~/.config/melissa`, `~/.local/state/melissa`).
pub struct Dirs {
    pub config: PathBuf,
    pub state: PathBuf,
}

impl Dirs {
    pub fn from_env() -> anyhow::Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let base = |var: &str, fallback: &str| -> anyhow::Result<PathBuf> {
            match std::env::var_os(var).map(PathBuf::from) {
                Some(p) if p.is_absolute() => Ok(p),
                _ => home
                    .as_ref()
                    .map(|h| h.join(fallback))
                    .with_context(|| format!("neither {var} nor HOME is set")),
            }
        };
        Ok(Self {
            config: base("XDG_CONFIG_HOME", ".config")?.join("melissa"),
            state: base("XDG_STATE_HOME", ".local/state")?.join("melissa"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_means_defaults() {
        let config: QueenConfig = toml::from_str("").unwrap();
        assert_eq!(config, QueenConfig::default());
        config.validate().unwrap();
    }

    #[test]
    fn parses_custom_backends() {
        let config: QueenConfig = toml::from_str(
            r#"
            listen = "127.0.0.1:9000"
            task_timeout_secs = 60
            [[backend]]
            name = "gemma"
            command = "opencode"
            args = ["run", "-m", "ollama/gemma3", "{prompt}"]
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.listen.port(), 9000);
        assert_eq!(config.backends.len(), 1);
        assert_eq!(
            config.backends[0].render_args("hi"),
            ["run", "-m", "ollama/gemma3", "hi"]
        );
    }

    #[test]
    fn rejects_unsafe_or_invalid_config() {
        let public: QueenConfig = toml::from_str(r#"listen = "0.0.0.0:8080""#).unwrap();
        assert!(public.validate().is_err());
        let dup: QueenConfig =
            toml::from_str("[[backend]]\nname = \"echo\"\ncommand = \"x\"").unwrap();
        assert!(dup.validate().is_err());
        assert!(toml::from_str::<QueenConfig>("nonsense = 1").is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::env::temp_dir().join("melissa-no-such-config.toml");
        assert_eq!(QueenConfig::load(&path).unwrap(), QueenConfig::default());
    }
}
