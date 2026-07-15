//! Stable machine-facing `deck agent` command namespace.
//!
//! Agent commands always emit JSON and expose a capability manifest so external
//! tools can discover Deck's supported operations.

use anyhow::Result;
use serde::Serialize;

use crate::config_edit::AgentConfigCommand;
use crate::contracts::{ProcessJson, print_json};
use crate::model::{CommandSpec, GitStatus, PluginSpec, WorkflowSpec};
use crate::planner::{command_plan, workflow_plan};
use crate::selection::{
    context_project, filtered_processes, load_projects, recent_runs_for, select_command,
    select_project,
};

#[derive(Debug, clap::Subcommand)]
pub enum AgentCommand {
    Projects,
    Inspect {
        project: String,
    },
    Plan {
        project: String,
        command: String,
    },
    Run {
        project: String,
        command: String,
        #[arg(long)]
        dry_run: bool,
    },
    Workflow {
        project: String,
        workflow: String,
        #[arg(long)]
        dry_run: bool,
    },
    Processes {
        project: Option<String>,
    },
    Session {
        #[command(subcommand)]
        action: AgentSessionCommand,
    },
    Config {
        #[command(subcommand)]
        action: AgentConfigCommand,
    },
    Capabilities,
}

#[derive(Debug, clap::Subcommand)]
pub enum AgentSessionCommand {
    Start { project: String },
}

#[derive(Debug, Serialize)]
struct AgentInspect {
    project: crate::context::ContextProject,
    git: Option<GitStatus>,
    commands: Vec<CommandSpec>,
    workflows: Vec<WorkflowSpec>,
    plugins: Vec<PluginSpec>,
    processes: Vec<ProcessJson>,
    recent_runs: Vec<crate::model::RunSummary>,
    context: AgentContextSuggestion,
}

#[derive(Debug, Serialize)]
struct AgentContextSuggestion {
    command: Vec<String>,
    json_command: Vec<String>,
}

pub fn run(action: AgentCommand) -> Result<()> {
    match action {
        AgentCommand::Projects => crate::commands::list(true),
        AgentCommand::Inspect { project } => inspect(&project),
        AgentCommand::Plan { project, command } => plan(&project, &command),
        AgentCommand::Run {
            project,
            command,
            dry_run,
        } => {
            if dry_run {
                plan(&project, &command)
            } else {
                crate::commands::run_project_command(&project, &command, true, false)
            }
        }
        AgentCommand::Workflow {
            project,
            workflow,
            dry_run,
        } => {
            if dry_run {
                workflow_dry_run(&project, &workflow)
            } else {
                crate::commands::run_workflow(&project, &workflow, true, false)
            }
        }
        AgentCommand::Processes { project } => crate::commands::ps(project.as_deref(), true),
        AgentCommand::Session { action } => match action {
            AgentSessionCommand::Start { project } => crate::summary::summary(&project, true),
        },
        AgentCommand::Config { action } => crate::config_edit::run(action, true),
        AgentCommand::Capabilities => capabilities(),
    }
}

fn inspect(project_query: &str) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let processes = filtered_processes(&state, Some(project))
        .into_iter()
        .map(|view| ProcessJson {
            process: view.process,
            alive: view.alive,
        })
        .collect();
    print_json(&AgentInspect {
        project: context_project(project),
        git: project.git.clone(),
        commands: project.commands.clone(),
        workflows: project.workflows.clone(),
        plugins: project.plugins.clone(),
        processes,
        recent_runs: recent_runs_for(project, &state, 10),
        context: AgentContextSuggestion {
            command: vec!["deck".into(), "context".into(), project.name.clone()],
            json_command: vec![
                "deck".into(),
                "context".into(),
                project.name.clone(),
                "--json".into(),
            ],
        },
    })
}

fn plan(project_query: &str, command_query: &str) -> Result<()> {
    let (projects, _, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let command = select_command(project, command_query)?;
    command_plan(project, command, &paths.runs_dir, true)
}

fn workflow_dry_run(project_query: &str, workflow_query: &str) -> Result<()> {
    let (projects, _, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let workflow = crate::workflow::select_workflow(project, workflow_query)?;
    workflow_plan(project, workflow, &paths.runs_dir, true)
}

fn capabilities() -> Result<()> {
    print_json(&serde_json::json!({
        "name": "deck-agent",
        "version": env!("CARGO_PKG_VERSION"),
        "commands": {
            "projects": {
                "argv": ["deck", "agent", "projects"],
                "output": "ProjectListItem[]"
            },
            "inspect": {
                "argv": ["deck", "agent", "inspect", "PROJECT"],
                "output": "AgentInspect"
            },
            "plan": {
                "argv": ["deck", "agent", "plan", "PROJECT", "COMMAND"],
                "output": "CommandPlan"
            },
            "run": {
                "argv": ["deck", "agent", "run", "PROJECT", "COMMAND"],
                "output": "RunJson",
                "dry_run": ["deck", "agent", "run", "PROJECT", "COMMAND", "--dry-run"]
            },
            "workflow": {
                "argv": ["deck", "agent", "workflow", "PROJECT", "WORKFLOW"],
                "output": "WorkflowRunJson",
                "dry_run": ["deck", "agent", "workflow", "PROJECT", "WORKFLOW", "--dry-run"]
            },
            "processes": {
                "argv": ["deck", "agent", "processes", "[PROJECT]"],
                "output": "ProcessJson[]"
            },
            "session_start": {
                "argv": ["deck", "agent", "session", "start", "PROJECT"],
                "output": "AgentSessionJson"
            },
            "config_add_command": {
                "argv": ["deck", "agent", "config", "add-command", "PROJECT", "NAME", "--cmd", "COMMAND"],
                "options": ["--kind once|server", "--port PORT", "--replace", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_add_argv_command": {
                "argv": ["deck", "agent", "config", "add-argv-command", "PROJECT", "NAME", "--arg", "PROGRAM", "--arg", "ARG"],
                "options": ["--arg VALUE repeated", "--kind once|server", "--port PORT", "--replace", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_remove_command": {
                "argv": ["deck", "agent", "config", "remove-command", "PROJECT", "NAME"],
                "options": ["--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_add_workflow": {
                "argv": ["deck", "agent", "config", "add-workflow", "PROJECT", "NAME", "--step", "COMMAND"],
                "options": ["--step COMMAND repeated", "--replace", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_remove_workflow": {
                "argv": ["deck", "agent", "config", "remove-workflow", "PROJECT", "NAME"],
                "options": ["--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_add_plugin": {
                "argv": ["deck", "agent", "config", "add-plugin", "PROJECT", "NAME", "--cmd", "COMMAND"],
                "options": ["--replace", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_add_plugin_path": {
                "argv": ["deck", "agent", "config", "add-plugin-path", "PROJECT", "NAME", "PATH"],
                "options": ["--replace", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_remove_plugin": {
                "argv": ["deck", "agent", "config", "remove-plugin", "PROJECT", "NAME"],
                "options": ["--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_add_sandbox": {
                "argv": ["deck", "agent", "config", "add-sandbox", "PROJECT", "NAME"],
                "options": ["--preset locked|test|dev", "--backend bwrap", "--network true|false", "--readonly-project true|false", "--writable PATH repeated", "--env NAME repeated", "--timeout-seconds SECONDS", "--allow-shell true|false", "--replace", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "config_remove_sandbox": {
                "argv": ["deck", "agent", "config", "remove-sandbox", "PROJECT", "NAME"],
                "options": ["--dry-run"],
                "output": "ConfigEditJson"
            },
            "sandbox_plan": {
                "argv": ["deck", "sandbox", "plan", "PROJECT", "COMMAND"],
                "options": ["--profile PROFILE", "--timeout-seconds SECONDS", "--json"],
                "output": "SandboxPlanJson"
            },
            "sandbox_run": {
                "argv": ["deck", "sandbox", "run", "PROJECT", "COMMAND"],
                "options": ["--profile PROFILE", "--timeout-seconds SECONDS", "--json"],
                "output": "SandboxRunJson"
            },
            "sandbox_doctor": {
                "argv": ["deck", "sandbox", "doctor"],
                "options": ["--json"],
                "output": "SandboxDoctorJson"
            },
            "tasks_list": {
                "argv": ["deck", "tasks", "list", "PROJECT"],
                "options": ["--json"],
                "output": "TaskListJson"
            },
            "tasks_add": {
                "argv": ["deck", "tasks", "add", "PROJECT", "NAME"],
                "options": ["--title TITLE", "--status todo|doing|done|blocked", "--notes NOTES", "--replace", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "tasks_set": {
                "argv": ["deck", "tasks", "set", "PROJECT", "NAME"],
                "options": ["--title TITLE", "--status todo|doing|done|blocked", "--notes NOTES", "--dry-run"],
                "output": "ConfigEditJson"
            },
            "tasks_remove": {
                "argv": ["deck", "tasks", "remove", "PROJECT", "NAME"],
                "options": ["--dry-run"],
                "output": "ConfigEditJson"
            },
            "recent": {
                "argv": ["deck", "recent", "[PROJECT]"],
                "options": ["--limit N", "--json"],
                "output": "RecentJson"
            },
            "rerun": {
                "argv": ["deck", "rerun", "[PROJECT]", "[COMMAND]"],
                "options": ["--dry-run", "--json"],
                "output": "RunJson or CommandPlan"
            },
            "capabilities": {
                "argv": ["deck", "agent", "capabilities"],
                "output": "CapabilityManifest"
            }
        },
        "json_error": {
            "output": "JsonError",
            "shape": {
                "ok": false,
                "error": {
                    "kind": "string",
                    "message": "string"
                }
            }
        }
    }))
}
