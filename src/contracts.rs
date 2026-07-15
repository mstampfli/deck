//! Public JSON response contracts.
//!
//! Shared output structs live here when their shape is consumed by scripts,
//! agents, or other stable Deck command surfaces.

use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::config::DeckConfig;
use crate::model::{CommandSpec, GitStatus, PluginSpec, Project, RunSummary, WorkflowSpec};
use crate::safety::CommandSafety;

#[derive(Debug, Serialize)]
pub struct ProjectListItem<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub root: &'a PathBuf,
    pub kinds: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct ProjectCommands<'a> {
    pub project: ProjectRef<'a>,
    pub commands: Vec<CommandView<'a>>,
}

#[derive(Debug, Serialize)]
pub struct CommandView<'a> {
    #[serde(flatten)]
    pub command: &'a CommandSpec,
    pub safety: CommandSafety,
}

#[derive(Debug, Serialize)]
pub struct ProjectWorkflows<'a> {
    pub project: ProjectRef<'a>,
    pub workflows: &'a [WorkflowSpec],
}

#[derive(Debug, Serialize)]
pub struct ProjectPlugins<'a> {
    pub project: ProjectRef<'a>,
    pub plugins: &'a [PluginSpec],
}

#[derive(Debug, Serialize)]
pub struct ProjectStatus<'a> {
    pub project: ProjectRef<'a>,
    pub git: &'a Option<GitStatus>,
    pub process_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ProjectRef<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub root: &'a PathBuf,
}

#[derive(Debug, Serialize)]
pub struct RunJson<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub command: &'a str,
    pub exit_code: Option<i32>,
    pub log_path: &'a PathBuf,
}

#[derive(Debug, Serialize)]
pub struct WorkflowRunJson<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub workflow: &'a str,
    pub completed_steps: &'a [RunSummary],
    pub failed_step: &'a Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProcessJson {
    pub process: crate::model::ProcessRecord,
    pub alive: bool,
}

#[derive(Debug, Serialize)]
pub struct CommandPlan<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub command: &'a CommandSpec,
    pub mutates_state: bool,
    pub streams_output: bool,
    pub log_dir: &'a PathBuf,
}

#[derive(Debug, Serialize)]
pub struct WorkflowPlan<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub workflow: &'a WorkflowSpec,
    pub steps: Vec<&'a CommandSpec>,
    pub mutates_state: bool,
    pub streams_output: bool,
    pub log_dir: &'a PathBuf,
}

#[derive(Debug, Serialize)]
pub struct AgentInspect {
    pub project: crate::context::ContextProject,
    pub git: Option<GitStatus>,
    pub commands: Vec<CommandSpec>,
    pub workflows: Vec<WorkflowSpec>,
    pub plugins: Vec<PluginSpec>,
    pub processes: Vec<ProcessJson>,
    pub recent_runs: Vec<RunSummary>,
    pub context: AgentContextSuggestion,
}

#[derive(Debug, Serialize)]
pub struct AgentContextSuggestion {
    pub command: Vec<String>,
    pub json_command: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ConfigEditJson<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub path: PathBuf,
    pub action: &'a str,
    pub dry_run: bool,
    pub changed: bool,
    pub config: DeckConfig,
}

#[derive(Debug, Serialize)]
pub struct JsonError<'a> {
    ok: bool,
    error: JsonErrorBody<'a>,
}

#[derive(Debug, Serialize)]
pub struct JsonErrorBody<'a> {
    kind: &'a str,
    message: String,
}

pub fn project_ref(project: &Project) -> ProjectRef<'_> {
    ProjectRef {
        id: &project.id,
        name: &project.name,
        root: &project.root,
    }
}

pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_error_json(kind: &str, message: String) -> Result<()> {
    print_json(&JsonError {
        ok: false,
        error: JsonErrorBody { kind, message },
    })
}
