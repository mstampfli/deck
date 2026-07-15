//! Human-facing CLI parsing and top-level command dispatch.
//!
//! This module owns argument parsing, JSON-error mode selection, and delegation
//! to feature modules. Feature behavior should stay out of this file when a
//! focused module can own it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::agent::AgentCommand;
use crate::config::write_default_deck_config;
use crate::contracts::{
    CommandView, ProjectCommands, ProjectPlugins, ProjectStatus, ProjectWorkflows,
    print_error_json, print_json, project_ref,
};
use crate::process::{start_process, stop_process};
use crate::selection::{load_projects, select_command, select_project, select_projects};
use crate::state::{State, state_paths};

#[derive(Debug, Parser)]
#[command(name = "deck", about = "A terminal cockpit for existing dev tools")]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Scan {
        roots: Vec<PathBuf>,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    #[command(name = "commands")]
    ShowCommands {
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Run {
        project: String,
        command: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dry_run: bool,
    },
    Start {
        project: String,
        command: String,
    },
    Stop {
        project: String,
        command: String,
    },
    Restart {
        project: String,
        command: String,
    },
    Ps {
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Logs {
        project: String,
        command: String,
    },
    Git {
        project: String,
        #[arg(value_enum)]
        action: GitCliAction,
    },
    Docker {
        project: Option<String>,
    },
    Gh {
        project: String,
        #[command(subcommand)]
        action: GhCommand,
    },
    Search {
        project: String,
        query: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    SshHosts,
    Journal {
        unit: Option<String>,
        #[arg(short, long, default_value_t = 100)]
        lines: usize,
    },
    Workflow {
        #[command(subcommand)]
        action: WorkflowCommand,
    },
    Plugin {
        #[command(subcommand)]
        action: PluginCommand,
    },
    Context {
        project: String,
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Status {
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Sandbox {
        #[command(subcommand)]
        action: crate::sandbox::SandboxCommand,
    },
    Tasks {
        #[command(subcommand)]
        action: crate::tasks::TaskCommand,
    },
    Recent {
        project: Option<String>,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    Rerun {
        project: Option<String>,
        command: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dry_run: bool,
    },
    Agent {
        #[command(subcommand)]
        action: AgentCommand,
    },
    Tui,
    Init,
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
    Issues,
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    List {
        project: String,
        #[arg(long)]
        json: bool,
    },
    Run {
        project: String,
        workflow: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    Add {
        name: String,
        #[arg(long)]
        cmd: String,
    },
    AddPath {
        name: String,
        path: PathBuf,
    },
    Remove {
        name: String,
    },
    List {
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Manifest {
        project: String,
        plugin: String,
    },
    Panels {
        project: String,
        plugin: String,
    },
    Actions {
        project: String,
        plugin: String,
    },
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
    let command = args.command.unwrap_or(Command::Tui);
    let json_errors = command_wants_json_errors(&command);
    if let Err(err) = dispatch(command) {
        if json_errors {
            print_error_json(crate::errors::classify(&err).as_str(), err.to_string())?;
            std::process::exit(1);
        }
        return Err(err);
    }
    Ok(())
}

fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Scan { roots } => scan(&roots),
        Command::List { json } => crate::commands::list(json),
        Command::ShowCommands { project, json } => commands(project.as_deref(), json),
        Command::Run {
            project,
            command,
            json,
            dry_run,
        } => crate::commands::run_project_command(&project, &command, json, dry_run),
        Command::Start { project, command } => start_project_command(&project, &command),
        Command::Stop { project, command } => stop_project_command(&project, &command),
        Command::Restart { project, command } => restart_project_command(&project, &command),
        Command::Ps { project, json } => crate::commands::ps(project.as_deref(), json),
        Command::Logs { project, command } => logs(&project, &command),
        Command::Git { project, action } => git_tool(&project, action),
        Command::Docker { project } => docker_tool(project.as_deref()),
        Command::Gh { project, action } => gh_tool(&project, action),
        Command::Search {
            project,
            query,
            limit,
        } => search_tool(&project, &query, limit),
        Command::SshHosts => print_tool_output(crate::tools::ssh_hosts()),
        Command::Journal { unit, lines } => {
            print_tool_output(crate::tools::journal(unit.as_deref(), lines))
        }
        Command::Workflow { action } => workflow(action),
        Command::Plugin { action } => plugin(action),
        Command::Context {
            project,
            json,
            output,
        } => context(&project, json, output.as_ref()),
        Command::Status { project, json } => status(project.as_deref(), json),
        Command::Sandbox { action } => crate::sandbox::run(action),
        Command::Tasks { action } => crate::tasks::run(action),
        Command::Recent {
            project,
            limit,
            json,
        } => crate::history::recent(project.as_deref(), limit, json),
        Command::Rerun {
            project,
            command,
            json,
            dry_run,
        } => crate::history::rerun(project.as_deref(), command.as_deref(), json, dry_run),
        Command::Agent { action } => crate::agent::run(action),
        Command::Tui => crate::tui::run_tui(),
        Command::Init => {
            let path = write_default_deck_config(&std::env::current_dir()?)?;
            println!("wrote {}", path.display());
            Ok(())
        }
        Command::ClearRuns => {
            let paths = state_paths()?;
            let mut state = State::load(&paths)?;
            state.clear_runs(&paths)?;
            state.save(&paths)?;
            println!("cleared run history");
            Ok(())
        }
    }
}

fn scan(roots: &[PathBuf]) -> Result<()> {
    let (projects, mut state, paths) = load_projects(roots)?;
    state.update_projects(&projects);
    state.save(&paths)?;
    println!("found {} projects", projects.len());
    for project in projects {
        println!("{}  {}", project.id, project.root.display());
    }
    Ok(())
}

fn commands(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let selected = select_projects(&projects, project_query)?;
    if json {
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
        return print_json(&output);
    }
    for project in selected {
        println!("{} ({})", project.name, project.root.display());
        for command in &project.commands {
            let marker = if command.available { " " } else { "!" };
            println!(
                "  {marker} {:<18} {:<10} {:<7} {:<5} {}",
                command.name,
                command.source.label(),
                command.kind.label(),
                if command.argv.is_some() {
                    "argv"
                } else {
                    "shell"
                },
                command.command
            );
            if let Some(reason) = &command.unavailable_reason {
                println!("    unavailable: {reason}");
            }
        }
    }
    Ok(())
}

fn start_project_command(project_query: &str, command_query: &str) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let command = select_command(project, command_query)?;
    let process = start_process(project, command, &state, &paths)?;
    println!(
        "started {} {} as pid {} log: {}",
        project.name,
        command.name,
        process.pid,
        process.log_path.display()
    );
    state.record_process(process);
    state.save(&paths)?;
    Ok(())
}

fn stop_project_command(project_query: &str, command_query: &str) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let process = state
        .running_process_for(&project.id, command_query)
        .with_context(|| format!("{} has no running process {command_query:?}", project.name))?;
    stop_process(&process)?;
    state.mark_process_stopped(&project.id, command_query);
    state.save(&paths)?;
    println!(
        "stopped {} {} pid {}",
        project.name, command_query, process.pid
    );
    Ok(())
}

fn restart_project_command(project_query: &str, command_query: &str) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    if let Some(process) = state.running_process_for(&project.id, command_query) {
        stop_process(&process)?;
        state.mark_process_stopped(&project.id, command_query);
    }
    let command = select_command(project, command_query)?;
    let process = start_process(project, command, &state, &paths)?;
    println!(
        "restarted {} {} as pid {} log: {}",
        project.name,
        command.name,
        process.pid,
        process.log_path.display()
    );
    state.record_process(process);
    state.save(&paths)?;
    Ok(())
}

fn logs(project_query: &str, command_query: &str) -> Result<()> {
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
    let raw = std::fs::read_to_string(&process.log_path)
        .with_context(|| format!("reading {}", process.log_path.display()))?;
    print!("{raw}");
    Ok(())
}

fn git_tool(project_query: &str, action: GitCliAction) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let action = match action {
        GitCliAction::Diff => crate::tools::GitAction::Diff,
        GitCliAction::Branches => crate::tools::GitAction::Branches,
        GitCliAction::Commits => crate::tools::GitAction::Commits,
    };
    print_tool_output(crate::tools::git(project, action))
}

fn docker_tool(project_query: Option<&str>) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = project_query
        .map(|query| select_project(&projects, query))
        .transpose()?;
    print_tool_output(crate::tools::docker_ps(project))
}

fn gh_tool(project_query: &str, action: GhCommand) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    match action {
        GhCommand::Issues => print_tool_output(crate::tools::gh_issues(project)),
    }
}

fn search_tool(project_query: &str, query: &str, limit: usize) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    print_tool_output(crate::tools::search(project, query, limit))
}

fn print_tool_output(output: Result<String>) -> Result<()> {
    print!("{}", output?);
    Ok(())
}

fn workflow(action: WorkflowCommand) -> Result<()> {
    match action {
        WorkflowCommand::List { project, json } => list_workflows(&project, json),
        WorkflowCommand::Run {
            project,
            workflow,
            json,
            dry_run,
        } => crate::commands::run_workflow(&project, &workflow, json, dry_run),
    }
}

fn plugin(action: PluginCommand) -> Result<()> {
    match action {
        PluginCommand::Add { name, cmd } => add_plugin(name, cmd),
        PluginCommand::AddPath { name, path } => {
            let cmd = crate::plugin::command_from_path(&path)?;
            add_plugin(name, cmd)
        }
        PluginCommand::Remove { name } => remove_plugin(&name),
        PluginCommand::List { project, json } => list_plugins(project.as_deref(), json),
        PluginCommand::Manifest { project, plugin } => plugin_manifest(&project, &plugin),
        PluginCommand::Panels { project, plugin } => plugin_panels(&project, &plugin),
        PluginCommand::Actions { project, plugin } => plugin_actions(&project, &plugin),
        PluginCommand::Run {
            project,
            plugin,
            action,
        } => plugin_run(&project, &plugin, &action),
    }
}

fn add_plugin(name: String, cmd: String) -> Result<()> {
    let paths = state_paths()?;
    let mut state = State::load(&paths)?;
    state.add_plugin(name.clone(), cmd.clone());
    state.save(&paths)?;
    println!("registered plugin {name}: {cmd}");
    Ok(())
}

fn remove_plugin(name: &str) -> Result<()> {
    let paths = state_paths()?;
    let mut state = State::load(&paths)?;
    if state.remove_plugin(name).is_some() {
        state.save(&paths)?;
        println!("removed plugin {name}");
    } else {
        anyhow::bail!("no global plugin named {name:?}");
    }
    Ok(())
}

fn list_plugins(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    if let Some(query) = project_query {
        let project = select_project(&projects, query)?;
        if json {
            return print_json(&ProjectPlugins {
                project: project_ref(project),
                plugins: &project.plugins,
            });
        }
        println!("{} ({})", project.name, project.root.display());
        for plugin in &project.plugins {
            println!(
                "  {:<18} {:<8} {}",
                plugin.name,
                plugin.source.label(),
                plugin.cmd
            );
        }
    } else {
        if json {
            return print_json(&state.global_plugins());
        }
        for plugin in state.global_plugins() {
            println!(
                "  {:<18} {:<8} {}",
                plugin.name,
                plugin.source.label(),
                plugin.cmd
            );
        }
    }
    Ok(())
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

fn plugin_run(project_query: &str, plugin_query: &str, action: &str) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let plugin = crate::plugin::select_plugin(project, plugin_query)?;
    print!("{}", crate::plugin::run_action(plugin, project, action)?);
    Ok(())
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
    if json {
        return print_json(&ProjectWorkflows {
            project: project_ref(project),
            workflows: &project.workflows,
        });
    }
    println!("{} ({})", project.name, project.root.display());
    for workflow in &project.workflows {
        println!("  {:<18} {}", workflow.name, workflow.steps.join(" -> "));
    }
    Ok(())
}

fn status(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let selected = select_projects(&projects, project_query)?;
    if json {
        let output = selected
            .iter()
            .map(|project| ProjectStatus {
                project: project_ref(project),
                git: &project.git,
                process_count: project.processes.len(),
            })
            .collect::<Vec<_>>();
        return print_json(&output);
    }
    for project in selected {
        let git = project
            .git
            .as_ref()
            .map_or("no git status".to_string(), |git| {
                format!(
                    "{} changed={} ahead={} behind={}",
                    git.branch, git.changed, git.ahead, git.behind
                )
            });
        let process_count = project.processes.len();
        println!(
            "{:<24} {:<45} processes={}",
            project.name, git, process_count
        );
    }
    Ok(())
}

fn command_wants_json_errors(command: &Command) -> bool {
    match command {
        Command::List { json }
        | Command::ShowCommands { json, .. }
        | Command::Run { json, .. }
        | Command::Ps { json, .. }
        | Command::Context { json, .. }
        | Command::Status { json, .. }
        | Command::Recent { json, .. }
        | Command::Rerun { json, .. } => *json,
        Command::Tasks { action } => match action {
            crate::tasks::TaskCommand::List { json, .. } => *json,
            crate::tasks::TaskCommand::Add { .. }
            | crate::tasks::TaskCommand::Set { .. }
            | crate::tasks::TaskCommand::Remove { .. } => true,
        },
        Command::Workflow { action } => match action {
            WorkflowCommand::List { json, .. } | WorkflowCommand::Run { json, .. } => *json,
        },
        Command::Plugin { action } => match action {
            PluginCommand::List { json, .. } => *json,
            PluginCommand::Manifest { .. }
            | PluginCommand::Panels { .. }
            | PluginCommand::Actions { .. }
            | PluginCommand::Run { .. } => true,
            PluginCommand::Add { .. }
            | PluginCommand::AddPath { .. }
            | PluginCommand::Remove { .. } => false,
        },
        Command::Agent { .. } => true,
        Command::Sandbox { action } => match action {
            crate::sandbox::SandboxCommand::Plan { json, .. }
            | crate::sandbox::SandboxCommand::Run { json, .. }
            | crate::sandbox::SandboxCommand::Doctor { json } => *json,
        },
        Command::Scan { .. }
        | Command::Start { .. }
        | Command::Stop { .. }
        | Command::Restart { .. }
        | Command::Logs { .. }
        | Command::Git { .. }
        | Command::Docker { .. }
        | Command::Gh { .. }
        | Command::Search { .. }
        | Command::SshHosts
        | Command::Journal { .. }
        | Command::Tui
        | Command::Init
        | Command::ClearRuns => false,
    }
}

fn raw_args_want_json_error() -> bool {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    args.iter().any(|arg| arg == "--json") || args.first().is_some_and(|arg| arg == "agent")
}
