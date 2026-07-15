//! Project-local configuration schema and safe config file operations.
//!
//! This module owns `deck.toml` parsing, default config generation, project-local
//! config locking, and atomic writes through temporary files.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeckConfig {
    pub name: Option<String>,
    #[serde(default)]
    pub commands: BTreeMap<String, CommandConfig>,
    #[serde(default)]
    pub workflows: BTreeMap<String, WorkflowConfig>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginConfig>,
    #[serde(default)]
    pub sandbox: BTreeMap<String, SandboxConfig>,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskConfig>,
    #[serde(default)]
    pub paths: DeckPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    #[serde(default)]
    pub backend: SandboxBackend,
    #[serde(default)]
    pub network: bool,
    #[serde(default = "default_readonly_project")]
    pub readonly_project: bool,
    #[serde(default)]
    pub writable: Vec<PathBuf>,
    #[serde(default)]
    pub env: Vec<String>,
    pub timeout_seconds: Option<u64>,
    #[serde(default = "default_allow_shell")]
    pub allow_shell: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackend {
    #[default]
    Bwrap,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxPreset {
    #[default]
    Test,
    Locked,
    Dev,
}

fn default_readonly_project() -> bool {
    true
}

fn default_allow_shell() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub cmd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    #[serde(default)]
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConfig {
    pub title: Option<String>,
    #[serde(default)]
    pub status: TaskStatus,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    #[default]
    Todo,
    Doing,
    Done,
    Blocked,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandConfig {
    Simple(String),
    Detailed(DetailedCommandConfig),
}

impl CommandConfig {
    pub fn command(&self) -> Result<String> {
        match self {
            Self::Simple(command) => Ok(command.clone()),
            Self::Detailed(command) => command.command_display(),
        }
    }

    pub fn argv(&self) -> Option<&[String]> {
        match self {
            Self::Simple(_) => None,
            Self::Detailed(command) => command.argv.as_deref(),
        }
    }

    pub fn kind(&self) -> ConfigCommandKind {
        match self {
            Self::Simple(_) => ConfigCommandKind::Once,
            Self::Detailed(command) => command.kind.unwrap_or_default(),
        }
    }

    pub fn port(&self) -> Option<u16> {
        match self {
            Self::Simple(_) => None,
            Self::Detailed(command) => command.port,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailedCommandConfig {
    pub cmd: Option<String>,
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub kind: Option<ConfigCommandKind>,
    pub port: Option<u16>,
}

impl DetailedCommandConfig {
    fn command_display(&self) -> Result<String> {
        match (&self.cmd, &self.argv) {
            (Some(cmd), None) => Ok(cmd.clone()),
            (None, Some(argv)) if !argv.is_empty() => Ok(argv.join(" ")),
            (Some(_), Some(_)) => anyhow::bail!("command config cannot set both cmd and argv"),
            (None, Some(_)) | (None, None) => {
                anyhow::bail!("command config must set cmd or non-empty argv")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigCommandKind {
    #[default]
    Once,
    Server,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeckPaths {
    pub root: Option<PathBuf>,
    pub logs: Option<PathBuf>,
    pub notes: Option<PathBuf>,
}

pub fn load_deck_config(root: &Path) -> Result<Option<DeckConfig>> {
    let path = root.join("deck.toml");
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config = toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(config))
}

pub fn load_or_default_deck_config(root: &Path, name: &str) -> Result<DeckConfig> {
    Ok(load_deck_config(root)?.unwrap_or_else(|| DeckConfig {
        name: Some(name.to_string()),
        ..DeckConfig::default()
    }))
}

pub fn deck_config_path(root: &Path) -> PathBuf {
    root.join("deck.toml")
}

pub fn write_deck_config(root: &Path, config: &DeckConfig) -> Result<PathBuf> {
    let path = deck_config_path(root);
    let raw = toml::to_string_pretty(config).context("serializing deck config")?;
    let temp_path = root.join(format!(
        ".deck.toml.tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    {
        let mut temp = File::create(&temp_path)
            .with_context(|| format!("creating {}", temp_path.display()))?;
        temp.write_all(raw.as_bytes())
            .with_context(|| format!("writing {}", temp_path.display()))?;
        temp.sync_all()
            .with_context(|| format!("syncing {}", temp_path.display()))?;
    }
    fs::rename(&temp_path, &path)
        .with_context(|| format!("renaming {} to {}", temp_path.display(), path.display()))?;
    Ok(path)
}

pub struct DeckConfigLock {
    path: PathBuf,
    _file: File,
}

impl Drop for DeckConfigLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn lock_deck_config(root: &Path) -> Result<DeckConfigLock> {
    let path = root.join(".deck.toml.lock");
    for _ in 0..100 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                writeln!(file, "pid={}", std::process::id())
                    .with_context(|| format!("writing {}", path.display()))?;
                return Ok(DeckConfigLock { path, _file: file });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                remove_stale_lock(&path)?;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("locking {}", path.display()));
            }
        }
    }
    anyhow::bail!("timed out waiting for {}", path.display())
}

fn remove_stale_lock(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(());
    };
    if modified.elapsed().unwrap_or_default() > Duration::from_secs(30) {
        fs::remove_file(path).with_context(|| format!("removing stale lock {}", path.display()))?;
    }
    Ok(())
}

pub fn write_default_deck_config(root: &Path) -> Result<PathBuf> {
    let path = deck_config_path(root);
    if path.exists() {
        anyhow::bail!("{} already exists", path.display());
    }

    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let body = format!(
        r#"name = "{name}"

[commands]
test = "cargo test"
fmt = "cargo fmt --all"

[commands.serve]
cmd = "cargo run"
kind = "server"

[workflows.check]
steps = ["fmt", "test"]

[tasks.next]
title = "Replace this with your next project task"
status = "todo"

[plugins.example]
cmd = "python3 scripts/deck_plugin.py"

[sandbox.default]
backend = "bwrap"
network = false
readonly_project = true
writable = ["./target", "./tmp"]
env = ["PATH", "HOME"]
timeout_seconds = 60
allow_shell = true

[sandbox.locked]
backend = "bwrap"
network = false
readonly_project = true
writable = ["./target", "./tmp"]
env = ["PATH"]
timeout_seconds = 300
allow_shell = false

[sandbox.dev]
backend = "bwrap"
network = true
readonly_project = false
writable = []
env = ["PATH", "HOME"]
allow_shell = true
"#
    );
    fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_and_detailed_commands() {
        let raw = r#"
name = "fixture"

[commands]
test = "cargo test"

[commands.serve]
cmd = "npm run dev"
kind = "server"
port = 3000
"#;

        let config: DeckConfig = toml::from_str(raw).unwrap();

        assert_eq!(config.commands["test"].command().unwrap(), "cargo test");
        assert!(matches!(
            config.commands["test"].kind(),
            ConfigCommandKind::Once
        ));
        assert_eq!(config.commands["serve"].command().unwrap(), "npm run dev");
        assert!(matches!(
            config.commands["serve"].kind(),
            ConfigCommandKind::Server
        ));
        assert_eq!(config.commands["serve"].port(), Some(3000));
    }

    #[test]
    fn parses_workflows() {
        let raw = r#"
[commands]
fmt = "cargo fmt"
test = "cargo test"

[workflows.ship]
steps = ["fmt", "test"]
"#;

        let config: DeckConfig = toml::from_str(raw).unwrap();

        assert_eq!(config.workflows["ship"].steps, vec!["fmt", "test"]);
    }

    #[test]
    fn parses_plugins() {
        let raw = r#"
[plugins.health]
cmd = "python3 scripts/health.py"
"#;

        let config: DeckConfig = toml::from_str(raw).unwrap();

        assert_eq!(config.plugins["health"].cmd, "python3 scripts/health.py");
    }

    #[test]
    fn parses_sandbox_profiles() {
        let raw = r#"
[sandbox.default]
backend = "bwrap"
network = false
readonly_project = true
writable = ["./target", "./tmp"]
env = ["PATH"]
timeout_seconds = 30
allow_shell = false
"#;

        let config: DeckConfig = toml::from_str(raw).unwrap();
        let sandbox = &config.sandbox["default"];

        assert!(matches!(sandbox.backend, SandboxBackend::Bwrap));
        assert!(!sandbox.network);
        assert!(sandbox.readonly_project);
        assert_eq!(
            sandbox.writable,
            vec![PathBuf::from("./target"), PathBuf::from("./tmp")]
        );
        assert_eq!(sandbox.env, vec!["PATH"]);
        assert_eq!(sandbox.timeout_seconds, Some(30));
        assert!(!sandbox.allow_shell);
    }

    #[test]
    fn loads_default_config_when_deck_toml_is_absent() {
        let temp = tempfile::tempdir().unwrap();

        let config = load_or_default_deck_config(temp.path(), "fixture").unwrap();

        assert_eq!(config.name.as_deref(), Some("fixture"));
        assert!(config.commands.is_empty());
        assert!(config.sandbox.is_empty());
        assert_eq!(deck_config_path(temp.path()), temp.path().join("deck.toml"));
    }

    #[test]
    fn writes_and_reads_deck_config() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = DeckConfig {
            name: Some("fixture".to_string()),
            ..DeckConfig::default()
        };
        config.workflows.insert(
            "check".to_string(),
            WorkflowConfig {
                steps: vec!["fmt".to_string(), "test".to_string()],
            },
        );

        let path = write_deck_config(temp.path(), &config).unwrap();
        let loaded = load_deck_config(temp.path()).unwrap().unwrap();

        assert_eq!(path, temp.path().join("deck.toml"));
        assert_eq!(loaded.name.as_deref(), Some("fixture"));
        assert_eq!(loaded.workflows["check"].steps, vec!["fmt", "test"]);
    }
}
