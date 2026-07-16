//! Core domain model shared across Deck modules.
//!
//! These structs represent discovered projects, commands, workflows, plugins,
//! process records, run summaries, git status, and tool availability.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub kinds: Vec<ProjectKind>,
    pub commands: Vec<CommandSpec>,
    pub workflows: Vec<WorkflowSpec>,
    pub plugins: Vec<PluginSpec>,
    pub git: Option<GitStatus>,
    pub tools: BTreeMap<String, ToolAvailability>,
    pub last_run: Option<RunSummary>,
    pub processes: Vec<ProcessRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSpec {
    pub name: String,
    pub cmd: String,
    pub source: PluginSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginSource {
    Global,
    Project,
}

impl PluginSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub name: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowRunResult {
    pub workflow_name: String,
    pub completed_steps: Vec<RunSummary>,
    pub failed_step: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectKind {
    Deck,
    Git,
    Rust,
    Node,
    Go,
    Make,
    Just,
    Docker,
}

impl ProjectKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Deck => "deck",
            Self::Git => "git",
            Self::Rust => "rust",
            Self::Node => "node",
            Self::Go => "go",
            Self::Make => "make",
            Self::Just => "just",
            Self::Docker => "docker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    pub source: CommandSource,
    pub command: String,
    pub argv: Option<Vec<String>>,
    pub cwd: PathBuf,
    pub kind: CommandKind,
    pub port: Option<u16>,
    pub category: CommandCategory,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
    Once,
    Server,
}

impl CommandKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandSource {
    DeckToml,
    Cargo,
    Npm,
    Make,
    Just,
}

impl CommandSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::DeckToml => "deck.toml",
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Make => "make",
            Self::Just => "just",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandCategory {
    Build,
    Check,
    Dev,
    Format,
    Run,
    Test,
    Utility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub changed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAvailability {
    pub available: bool,
    pub path: Option<PathBuf>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub project_id: String,
    pub command_name: String,
    pub command: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub exit_code: Option<i32>,
    pub log_path: PathBuf,
    /// Pid of the spawned command, for liveness checks on unfinished runs.
    #[serde(default)]
    pub pid: Option<u32>,
    /// False while the run is in flight; stays false if Deck dies mid-run.
    #[serde(default = "default_run_finished")]
    pub finished: bool,
    #[serde(default)]
    pub timed_out: bool,
}

/// Records written before these fields existed are all completed runs.
fn default_run_finished() -> bool {
    true
}

impl RunSummary {
    /// Human label for how the run ended: an exit code, "signal", "timeout",
    /// or "interrupted" for a run that was never finalized.
    pub fn exit_label(&self) -> String {
        if self.timed_out {
            return "timeout".to_string();
        }
        if !self.finished {
            return "interrupted".to_string();
        }
        match self.exit_code {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub project_id: String,
    pub project_name: String,
    pub command_name: String,
    pub command: String,
    pub pid: u32,
    pub port: Option<u16>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub log_path: PathBuf,
}

impl ProcessRecord {
    pub fn is_marked_running(&self) -> bool {
        self.stopped_at.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub summary: RunSummary,
    pub output: String,
}
