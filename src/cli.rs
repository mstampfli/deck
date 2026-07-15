//! CLI parsing and top-level command dispatch.
//!
//! This module owns argument parsing, the global `--json` flag, JSON-error
//! selection, and delegation to feature modules. Feature behavior should stay
//! out of this file when a focused module can own it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::config::write_default_deck_config;
use crate::config_edit::ConfigCommand;
use crate::contracts::{
    ClearRunsJson, CommandView, InitJson, LogsJson, PluginRegistryJson, PluginRunJson,
    ProcessActionJson, ProjectCommands, ProjectPlugins, ProjectStatus, ProjectWorkflows, ScanJson,
    ToolOutputJson, emit, print_error_json, project_list_item, project_ref,
};
use crate::model::Project;
use crate::process::{start_process, stop_process};
use crate::selection::{load_projects, select_command, select_project, select_projects};
use crate::state::{State, state_paths};

#[derive(Debug, Parser)]
#[command(
    name = "deck",
    about = "A terminal cockpit for existing dev tools",
    after_help = "Start with:\n  deck scan ~             index your projects\n  deck summary PROJECT    one-screen project overview\n\nAgents: every command accepts --json; run 'deck capabilities' for a\nmachine-readable manifest of the full surface."
)]
pub struct Args {
    /// Print structured JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Discover projects under the given roots and remember them
    Scan { roots: Vec<PathBuf> },
    /// List all known projects
    List,
    /// Show a project's detected and configured commands
    #[command(name = "commands")]
    ShowCommands { project: Option<String> },
    /// Run a project command once and record the run
    Run {
        project: String,
        command: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Start a server command in the background
    Start { project: String, command: String },
    /// Stop a tracked background command
    Stop { project: String, command: String },
    /// Restart a tracked background command
    Restart { project: String, command: String },
    /// List tracked background processes
    Ps { project: Option<String> },
    /// Print the log of a tracked command
    Logs { project: String, command: String },
    /// Show git diff, branches, or commits for a project
    Git {
        project: String,
        #[arg(value_enum)]
        action: GitCliAction,
    },
    /// Show docker containers, optionally scoped to a project
    Docker { project: Option<String> },
    /// Show GitHub information for a project via gh
    Gh {
        project: String,
        #[command(subcommand)]
        action: GhCommand,
    },
    /// Search a project's files for text
    Search {
        project: String,
        query: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// List hosts from your SSH config
    SshHosts,
    /// Show systemd journal entries
    Journal {
        unit: Option<String>,
        #[arg(short, long, default_value_t = 100)]
        lines: usize,
    },
    /// List or run multi-step workflows
    Workflow {
        #[command(subcommand)]
        action: WorkflowCommand,
    },
    /// Manage and run registered plugins
    Plugin {
        #[command(subcommand)]
        action: PluginCommand,
    },
    /// Emit a deterministic project context bundle (markdown, or JSON with --json)
    Context {
        project: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show git and process status per project
    Status { project: Option<String> },
    /// One-screen project overview; with --json, the full startup bundle for agents
    Summary { project: String },
    /// Plan, run, and diagnose Bubblewrap-sandboxed commands
    Sandbox {
        #[command(subcommand)]
        action: crate::sandbox::SandboxCommand,
    },
    /// Manage project-local tasks stored in deck.toml
    Tasks {
        #[command(subcommand)]
        action: crate::tasks::TaskCommand,
    },
    /// Show recent command runs
    Recent {
        project: Option<String>,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Re-run the most recent command, optionally scoped to a project
    Rerun {
        project: Option<String>,
        command: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Edit a project's deck.toml: commands, workflows, plugins, sandbox profiles
    Config {
        #[command(subcommand)]
        action: ConfigCommand,
    },
    /// Print the machine-readable command manifest for agents (always JSON)
    Capabilities,
    /// Open the interactive terminal UI (the default when no command is given)
    Tui,
    /// Write a starter deck.toml in the current directory
    Init,
    /// Clear recorded run history
    ClearRuns,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum GitCliAction {
    Diff,
    Branches,
    Commits,
}

#[derive(Debug, Subcommand)]
enum GhCommand {
    /// List open issues
    Issues,
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    /// List a project's workflows
    List { project: String },
    /// Run a workflow's steps in order, stopping at the first failure
    Run {
        project: String,
        workflow: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    /// Register a global plugin command
    Add {
        name: String,
        #[arg(long)]
        cmd: String,
    },
    /// Register a global plugin from an executable path
    AddPath { name: String, path: PathBuf },
    /// Remove a global plugin
    Remove { name: String },
    /// List plugins, global or for one project
    List { project: Option<String> },
    /// Print a plugin's manifest
    Manifest { project: String, plugin: String },
    /// Print a plugin's panels
    Panels { project: String, plugin: String },
    /// Print a plugin's actions
    Actions { project: String, plugin: String },
    /// Run a plugin action
    Run {
        project: String,
        plugin: String,
        action: String,
    },
}

pub fn run() -> Result<()> {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(err) => {
            if raw_args_want_json_error() {
                let code = err.exit_code();
                print_error_json("cli_parse_error", err.to_string())?;
                std::process::exit(code);
            }
            err.exit();
        }
    };
    let json = args.json;
    let command = args.command.unwrap_or(Command::Tui);
    if let Err(err) = dispatch(command, json) {
        if err.downcast_ref::<crate::errors::Reported>().is_some() {
            std::process::exit(1);
        }
        if json {
            print_error_json(crate::errors::classify(&err).as_str(), err.to_string())?;
            std::process::exit(1);
        }
        return Err(err);
    }
    Ok(())
}

fn dispatch(command: Command, json: bool) -> Result<()> {
    match command {
        Command::Scan { roots } => scan(&roots, json),
        Command::List => crate::commands::list(json),
        Command::ShowCommands { project } => commands(project.as_deref(), json),
        Command::Run {
            project,
            command,
            dry_run,
        } => crate::commands::run_project_command(&project, &command, json, dry_run),
        Command::Start { project, command } => start_project_command(&project, &command, json),
        Command::Stop { project, command } => stop_project_command(&project, &command, json),
        Command::Restart { project, command } => restart_project_command(&project, &command, json),
        Command::Ps { project } => crate::commands::ps(project.as_deref(), json),
        Command::Logs { project, command } => logs(&project, &command, json),
        Command::Git { project, action } => git_tool(&project, action, json),
        Command::Docker { project } => docker_tool(project.as_deref(), json),
        Command::Gh { project, action } => gh_tool(&project, action, json),
        Command::Search {
            project,
            query,
            limit,
        } => search_tool(&project, &query, limit, json),
        Command::SshHosts => emit_tool("ssh-hosts", None, crate::tools::ssh_hosts(), json),
        Command::Journal { unit, lines } => emit_tool(
            "journal",
            None,
            crate::tools::journal(unit.as_deref(), lines),
            json,
        ),
        Command::Workflow { action } => workflow(action, json),
        Command::Plugin { action } => plugin(action, json),
        Command::Context { project, output } => context(&project, json, output.as_ref()),
        Command::Status { project } => status(project.as_deref(), json),
        Command::Summary { project } => crate::summary::summary(&project, json),
        Command::Sandbox { action } => crate::sandbox::run(action, json),
        Command::Tasks { action } => crate::tasks::run(action, json),
        Command::Recent { project, limit } => {
            crate::history::recent(project.as_deref(), limit, json)
        }
        Command::Rerun {
            project,
            command,
            dry_run,
        } => crate::history::rerun(project.as_deref(), command.as_deref(), json, dry_run),
        Command::Config { action } => crate::config_edit::run(action, json),
        Command::Capabilities => crate::capabilities::capabilities(),
        Command::Tui => {
            if json {
                anyhow::bail!("the TUI is interactive and has no JSON output");
            }
            crate::tui::run_tui()
        }
        Command::Init => {
            let path = write_default_deck_config(&std::env::current_dir()?)?;
            emit(&InitJson { ok: true, path }, json)
        }
        Command::ClearRuns => {
            let paths = state_paths()?;
            let mut state = State::load(&paths)?;
            state.clear_runs(&paths)?;
            state.save(&paths)?;
            emit(&ClearRunsJson { ok: true }, json)
        }
    }
}

fn scan(roots: &[PathBuf], json: bool) -> Result<()> {
    let (projects, mut state, paths) = load_projects(roots)?;
    state.update_projects(&projects);
    state.save(&paths)?;
    emit(
        &ScanJson {
            ok: true,
            projects: projects.iter().map(project_list_item).collect(),
        },
        json,
    )
}

fn commands(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let selected = select_projects(&projects, project_query)?;
    let output = selected
        .iter()
        .map(|project| ProjectCommands {
            project: project_ref(project),
            commands: project
                .commands
                .iter()
                .map(|command| CommandView {
                    command,
                    safety: crate::safety::command_safety(command),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    emit(&output, json)
}

fn start_project_command(project_query: &str, command_query: &str, json: bool) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let command = select_command(project, command_query)?;
    let process = start_process(project, command, &state, &paths)?;
    emit(
        &ProcessActionJson {
            ok: true,
            action: "started",
            project: &project.name,
            command: &command.name,
            pid: process.pid,
            log_path: Some(&process.log_path),
        },
        json,
    )?;
    state.record_process(process);
    state.save(&paths)?;
    Ok(())
}

fn stop_project_command(project_query: &str, command_query: &str, json: bool) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let process = state
        .running_process_for(&project.id, command_query)
        .with_context(|| format!("{} has no running process {command_query:?}", project.name))?;
    stop_process(&process)?;
    state.mark_process_stopped(&project.id, command_query);
    state.save(&paths)?;
    emit(
        &ProcessActionJson {
            ok: true,
            action: "stopped",
            project: &project.name,
            command: command_query,
            pid: process.pid,
            log_path: None,
        },
        json,
    )
}

fn restart_project_command(project_query: &str, command_query: &str, json: bool) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    if let Some(process) = state.running_process_for(&project.id, command_query) {
        stop_process(&process)?;
        state.mark_process_stopped(&project.id, command_query);
    }
    let command = select_command(project, command_query)?;
    let process = start_process(project, command, &state, &paths)?;
    emit(
        &ProcessActionJson {
            ok: true,
            action: "restarted",
            project: &project.name,
            command: &command.name,
            pid: process.pid,
            log_path: Some(&process.log_path),
        },
        json,
    )?;
    state.record_process(process);
    state.save(&paths)?;
    Ok(())
}

fn logs(project_query: &str, command_query: &str, json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let process = state
        .running_process_for(&project.id, command_query)
        .or_else(|| {
            state
                .processes
                .iter()
                .rev()
                .find(|process| {
                    process.project_id == project.id && process.command_name == command_query
                })
                .cloned()
        })
        .with_context(|| format!("{} has no process logs for {command_query:?}", project.name))?;
    let content = std::fs::read_to_string(&process.log_path)
        .with_context(|| format!("reading {}", process.log_path.display()))?;
    emit(
        &LogsJson {
            ok: true,
            project: project_ref(project),
            command: command_query,
            log_path: &process.log_path,
            content,
        },
        json,
    )
}

fn git_tool(project_query: &str, action: GitCliAction, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let action = match action {
        GitCliAction::Diff => crate::tools::GitAction::Diff,
        GitCliAction::Branches => crate::tools::GitAction::Branches,
        GitCliAction::Commits => crate::tools::GitAction::Commits,
    };
    emit_tool(
        "git",
        Some(project),
        crate::tools::git(project, action),
        json,
    )
}

fn docker_tool(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = project_query
        .map(|query| select_project(&projects, query))
        .transpose()?;
    emit_tool("docker", project, crate::tools::docker_ps(project), json)
}

fn gh_tool(project_query: &str, action: GhCommand, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    match action {
        GhCommand::Issues => emit_tool("gh", Some(project), crate::tools::gh_issues(project), json),
    }
}

fn search_tool(project_query: &str, query: &str, limit: usize, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    emit_tool(
        "search",
        Some(project),
        crate::tools::search(project, query, limit),
        json,
    )
}

fn emit_tool(
    tool: &'static str,
    project: Option<&Project>,
    output: Result<String>,
    json: bool,
) -> Result<()> {
    emit(
        &ToolOutputJson {
            ok: true,
            tool,
            project: project.map(project_ref),
            output: output?,
        },
        json,
    )
}

fn workflow(action: WorkflowCommand, json: bool) -> Result<()> {
    match action {
        WorkflowCommand::List { project } => list_workflows(&project, json),
        WorkflowCommand::Run {
            project,
            workflow,
            dry_run,
        } => crate::commands::run_workflow(&project, &workflow, json, dry_run),
    }
}

fn plugin(action: PluginCommand, json: bool) -> Result<()> {
    match action {
        PluginCommand::Add { name, cmd } => add_plugin(name, cmd, json),
        PluginCommand::AddPath { name, path } => {
            let cmd = crate::plugin::command_from_path(&path)?;
            add_plugin(name, cmd, json)
        }
        PluginCommand::Remove { name } => remove_plugin(&name, json),
        PluginCommand::List { project } => list_plugins(project.as_deref(), json),
        PluginCommand::Manifest { project, plugin } => plugin_manifest(&project, &plugin),
        PluginCommand::Panels { project, plugin } => plugin_panels(&project, &plugin),
        PluginCommand::Actions { project, plugin } => plugin_actions(&project, &plugin),
        PluginCommand::Run {
            project,
            plugin,
            action,
        } => plugin_run(&project, &plugin, &action, json),
    }
}

fn add_plugin(name: String, cmd: String, json: bool) -> Result<()> {
    let paths = state_paths()?;
    let mut state = State::load(&paths)?;
    state.add_plugin(name.clone(), cmd.clone());
    state.save(&paths)?;
    emit(
        &PluginRegistryJson {
            ok: true,
            action: "registered",
            name: &name,
            cmd: Some(&cmd),
        },
        json,
    )
}

fn remove_plugin(name: &str, json: bool) -> Result<()> {
    let paths = state_paths()?;
    let mut state = State::load(&paths)?;
    if state.remove_plugin(name).is_none() {
        anyhow::bail!("no global plugin named {name:?}");
    }
    state.save(&paths)?;
    emit(
        &PluginRegistryJson {
            ok: true,
            action: "removed",
            name,
            cmd: None,
        },
        json,
    )
}

fn list_plugins(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    if let Some(query) = project_query {
        let project = select_project(&projects, query)?;
        return emit(
            &ProjectPlugins {
                project: project_ref(project),
                plugins: &project.plugins,
            },
            json,
        );
    }
    emit(&state.global_plugins(), json)
}

fn plugin_manifest(project_query: &str, plugin_query: &str) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let plugin = crate::plugin::select_plugin(project, plugin_query)?;
    let manifest = crate::plugin::manifest(plugin, project)?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

fn plugin_panels(project_query: &str, plugin_query: &str) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let plugin = crate::plugin::select_plugin(project, plugin_query)?;
    let panels = crate::plugin::panels(plugin, project)?;
    println!("{}", serde_json::to_string_pretty(&panels)?);
    Ok(())
}

fn plugin_actions(project_query: &str, plugin_query: &str) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let plugin = crate::plugin::select_plugin(project, plugin_query)?;
    let actions = crate::plugin::actions(plugin, project)?;
    println!("{}", serde_json::to_string_pretty(&actions)?);
    Ok(())
}

fn plugin_run(project_query: &str, plugin_query: &str, action: &str, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let plugin = crate::plugin::select_plugin(project, plugin_query)?;
    let output = crate::plugin::run_action(plugin, project, action)?;
    emit(
        &PluginRunJson {
            ok: true,
            project: project_ref(project),
            plugin: &plugin.name,
            action,
            output,
        },
        json,
    )
}

fn context(project_query: &str, json: bool, output: Option<&PathBuf>) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let bundle = crate::context::build_context(project, &state)?;
    let rendered = if json {
        serde_json::to_string_pretty(&bundle)?
    } else {
        crate::context::render_markdown(&bundle)
    };
    if let Some(output) = output {
        std::fs::write(output, rendered)
            .with_context(|| format!("writing context bundle to {}", output.display()))?;
    } else {
        print!("{rendered}");
    }
    Ok(())
}

fn list_workflows(project_query: &str, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    emit(
        &ProjectWorkflows {
            project: project_ref(project),
            workflows: &project.workflows,
        },
        json,
    )
}

fn status(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let selected = select_projects(&projects, project_query)?;
    let output = selected
        .iter()
        .map(|project| ProjectStatus {
            project: project_ref(project),
            git: &project.git,
            process_count: project.processes.len(),
        })
        .collect::<Vec<_>>();
    emit(&output, json)
}

fn raw_args_want_json_error() -> bool {
    std::env::args().skip(1).any(|arg| arg == "--json")
}
