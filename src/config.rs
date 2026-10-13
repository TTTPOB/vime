use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub hotkey: HotkeyConfig,
    pub output: OutputConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub connect_timeout_ms: u64,
    pub final_timeout_ms: u64,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:8765/v1/realtime".into(),
            model: "mock-asr".into(),
            api_key_env: None,
            connect_timeout_ms: 10_000,
            final_timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub key: String,
    pub mode: HotkeyMode,
}
impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            key: "Alt+F9".into(),
            mode: HotkeyMode::PushToTalk,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyMode {
    PushToTalk,
    Toggle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub clipboard: bool,
    pub command: Vec<String>,
    pub recovery_command: Option<Vec<String>>,
    pub script_timeout_ms: u64,
}
impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            clipboard: true,
            command: vec![],
            recovery_command: None,
            script_timeout_ms: 10_000,
        }
    }
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let config: Self = toml::from_str(&data).context("invalid TOML configuration")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        let url = url::Url::parse(&self.server.url).context("invalid server URL")?;
        if !matches!(url.scheme(), "ws" | "wss") || url.host_str().is_none() {
            bail!("server.url must be a ws:// or wss:// URL with a host");
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("URL credentials are unsupported; use server.api_key_env");
        }
        if url.fragment().is_some() {
            bail!("server.url must not contain a fragment");
        }
        if self.server.model.trim().is_empty() {
            bail!("server.model must not be empty");
        }
        if let Some(name) = &self.server.api_key_env {
            if name.is_empty()
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                || name.starts_with(|c: char| c.is_ascii_digit())
            {
                bail!("server.api_key_env must be an environment variable name");
            }
        }
        crate::platform::hotkey::validate_key(&self.hotkey.key)?;
        for (name, value) in [
            ("connect_timeout_ms", self.server.connect_timeout_ms),
            ("final_timeout_ms", self.server.final_timeout_ms),
            ("script_timeout_ms", self.output.script_timeout_ms),
        ] {
            if value == 0 || value > 3_600_000 {
                bail!("{name} must be between 1 and 3600000 milliseconds");
            }
        }
        for (name, command) in [
            ("output.command", Some(&self.output.command)),
            (
                "output.recovery_command",
                self.output.recovery_command.as_ref(),
            ),
        ] {
            if let Some(command) = command {
                if command.first().is_some_and(|s| s.trim().is_empty()) {
                    bail!("{name} executable must not be empty");
                }
                if command.iter().any(|s| s.contains('\0')) {
                    bail!("{name} must not contain NUL");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_partial_config() {
        let c: Config = toml::from_str("[hotkey]\nmode = 'toggle'\n").unwrap();
        c.validate().unwrap();
        assert_eq!(c.hotkey.mode, HotkeyMode::Toggle);
        assert_eq!(c.server.model, "mock-asr");
        assert_eq!(Config::default().hotkey.mode, HotkeyMode::PushToTalk);
    }
    #[test]
    fn rejects_bad_configuration() {
        assert!(toml::from_str::<Config>("typo = true").is_err());
        let mut c = Config::default();
        c.server.url = "https://example.com".into();
        assert!(c.validate().is_err());
        c.server.url = "wss://example.com/v1/realtime".into();
        c.server.final_timeout_ms = 0;
        assert!(c.validate().is_err());
        c.server.final_timeout_ms = 1;
        c.output.command = vec!["".into()];
        assert!(c.validate().is_err());
        c.output.command.clear();
        c.output.recovery_command = Some(vec!["".into()]);
        assert!(c.validate().is_err());
        c.output.recovery_command = Some(vec![]);
        assert!(c.validate().is_ok());
    }
    #[test]
    fn rejects_invalid_hotkeys() {
        let mut c = Config::default();
        for key in ["Alt+F99", "Alt+F9+F10"] {
            c.hotkey.key = key.into();
            assert!(c.validate().is_err());
        }
    }
}
