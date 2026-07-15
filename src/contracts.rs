//! Output contracts and rendering.
//!
//! Every command output is a serializable struct that also knows how to render
//! itself for humans. `emit` picks the rendering from the global `--json` flag,
//! so JSON/human parity holds by construction: a new output type cannot exist
//! without both forms.

use std::io;
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::config::DeckConfig;
use crate::model::{CommandSpec, GitStatus, PluginSpec, Project, RunSummary, WorkflowSpec};
use crate::safety::CommandSafety;

/// A command output: one struct, two renderings.
///
/// JSON comes from `Serialize`; the human form comes from `human`. Implement
/// this on every top-level output type and print through [`emit`].
pub trait Render: Serialize {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()>;
}

/// Print `value` as pretty JSON or as human text, per the global `--json` flag.
pub fn emit<T: Render>(value: &T, json: bool) -> Result<()> {
    if json {
        return print_json(value);
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    value.human(&mut out)?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ProjectListItem<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub root: &'a PathBuf,
    pub kinds: Vec<&'static str>,
}

impl Render for Vec<ProjectListItem<'_>> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        for project in self {
            writeln!(
                out,
                "{:<18} {:<24} {:<20} {}",
                project.id,
                project.name,
                project.kinds.join(","),
                project.root.display()
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct ProjectCommands<'a> {
    pub project: ProjectRef<'a>,
    pub commands: Vec<CommandView<'a>>,
}

impl Render for Vec<ProjectCommands<'_>> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        for entry in self {
            writeln!(
                out,
                "{} ({})",
                entry.project.name,
                entry.project.root.display()
            )?;
            for view in &entry.commands {
                let command = view.command;
                let marker = if command.available { " " } else { "!" };
                writeln!(
                    out,
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
                )?;
                if let Some(reason) = &command.unavailable_reason {
                    writeln!(out, "    unavailable: {reason}")?;
                }
            }
        }
        Ok(())
    }
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

impl Render for ProjectWorkflows<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(
            out,
            "{} ({})",
            self.project.name,
            self.project.root.display()
        )?;
        for workflow in self.workflows {
            writeln!(
                out,
                "  {:<18} {}",
                workflow.name,
                workflow.steps.join(" -> ")
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct ProjectPlugins<'a> {
    pub project: ProjectRef<'a>,
    pub plugins: &'a [PluginSpec],
}

impl Render for ProjectPlugins<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(
            out,
            "{} ({})",
            self.project.name,
            self.project.root.display()
        )?;
        render_plugin_rows(self.plugins, out)
    }
}

impl Render for Vec<PluginSpec> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        render_plugin_rows(self, out)
    }
}

fn render_plugin_rows(plugins: &[PluginSpec], out: &mut dyn io::Write) -> io::Result<()> {
    for plugin in plugins {
        writeln!(
            out,
            "  {:<18} {:<8} {}",
            plugin.name,
            plugin.source.label(),
            plugin.cmd
        )?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ProjectStatus<'a> {
    pub project: ProjectRef<'a>,
    pub git: &'a Option<GitStatus>,
    pub process_count: usize,
}

impl Render for Vec<ProjectStatus<'_>> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        for status in self {
            let git = status
                .git
                .as_ref()
                .map_or("no git status".to_string(), |git| {
                    format!(
                        "{} changed={} ahead={} behind={}",
                        git.branch, git.changed, git.ahead, git.behind
                    )
                });
            writeln!(
                out,
                "{:<24} {:<45} processes={}",
                status.project.name, git, status.process_count
            )?;
        }
        Ok(())
    }
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

impl Render for RunJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(out, "log: {}", self.log_path.display())?;
        if !self.ok {
            match self.exit_code {
                Some(code) => {
                    writeln!(out, "command {} failed with exit code {code}", self.command)?
                }
                None => writeln!(out, "command {} was terminated by a signal", self.command)?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct WorkflowRunJson<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub workflow: &'a str,
    pub completed_steps: &'a [RunSummary],
    pub failed_step: &'a Option<String>,
}

impl Render for WorkflowRunJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        match self.failed_step {
            Some(step) => writeln!(
                out,
                "workflow {} failed at step {step} after {} completed steps",
                self.workflow,
                self.completed_steps.len()
            ),
            None => writeln!(
                out,
                "workflow {} completed {} steps",
                self.workflow,
                self.completed_steps.len()
            ),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProcessJson {
    pub process: crate::model::ProcessRecord,
    pub alive: bool,
}

impl Render for Vec<ProcessJson> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        for view in self {
            let status = if view.alive { "running" } else { "stopped" };
            let port = view
                .process
                .port
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            writeln!(
                out,
                "{:<8} pid={:<8} {:<18} {:<18} {} {}",
                status,
                view.process.pid,
                view.process.project_name,
                view.process.command_name,
                port,
                view.process.log_path.display()
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct ProcessActionJson<'a> {
    pub ok: bool,
    pub action: &'static str,
    pub project: &'a str,
    pub command: &'a str,
    pub pid: u32,
    pub log_path: Option<&'a PathBuf>,
}

impl Render for ProcessActionJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        match self.log_path {
            Some(log_path) => writeln!(
                out,
                "{} {} {} as pid {} log: {}",
                self.action,
                self.project,
                self.command,
                self.pid,
                log_path.display()
            ),
            None => writeln!(
                out,
                "{} {} {} pid {}",
                self.action, self.project, self.command, self.pid
            ),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LogsJson<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub command: &'a str,
    pub log_path: &'a PathBuf,
    pub content: String,
}

impl Render for LogsJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        write!(out, "{}", self.content)
    }
}

#[derive(Debug, Serialize)]
pub struct ToolOutputJson<'a> {
    pub ok: bool,
    pub tool: &'static str,
    pub project: Option<ProjectRef<'a>>,
    pub output: String,
}

impl Render for ToolOutputJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        write!(out, "{}", self.output)
    }
}

#[derive(Debug, Serialize)]
pub struct ScanJson<'a> {
    pub ok: bool,
    pub projects: Vec<ProjectListItem<'a>>,
}

impl Render for ScanJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(out, "found {} projects", self.projects.len())?;
        for project in &self.projects {
            writeln!(out, "{}  {}", project.id, project.root.display())?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct ClearRunsJson {
    pub ok: bool,
}

impl Render for ClearRunsJson {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(out, "cleared run history")
    }
}

#[derive(Debug, Serialize)]
pub struct PluginRegistryJson<'a> {
    pub ok: bool,
    pub action: &'static str,
    pub name: &'a str,
    pub cmd: Option<&'a str>,
}

impl Render for PluginRegistryJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        match self.cmd {
            Some(cmd) => writeln!(out, "{} plugin {}: {cmd}", self.action, self.name),
            None => writeln!(out, "{} plugin {}", self.action, self.name),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PluginRunJson<'a> {
    pub ok: bool,
    pub project: ProjectRef<'a>,
    pub plugin: &'a str,
    pub action: &'a str,
    pub output: String,
}

impl Render for PluginRunJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        write!(out, "{}", self.output)
    }
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

impl Render for CommandPlan<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(
            out,
            "project: {} ({})",
            self.project.name,
            self.project.root.display()
        )?;
        writeln!(out, "command: {}", self.command.name)?;
        writeln!(out, "cwd: {}", self.command.cwd.display())?;
        writeln!(out, "shell: {}", self.command.command)?;
        writeln!(out, "available: {}", self.command.available)?;
        if let Some(reason) = &self.command.unavailable_reason {
            writeln!(out, "unavailable: {reason}")?;
        }
        writeln!(out, "mutates_state: {}", self.mutates_state)?;
        writeln!(out, "streams_output: {}", self.streams_output)?;
        writeln!(out, "log_dir: {}", self.log_dir.display())
    }
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

impl Render for WorkflowPlan<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(
            out,
            "project: {} ({})",
            self.project.name,
            self.project.root.display()
        )?;
        writeln!(out, "workflow: {}", self.workflow.name)?;
        writeln!(out, "steps: {}", self.workflow.steps.join(" -> "))?;
        writeln!(out, "available: {}", self.ok)?;
        writeln!(out, "mutates_state: {}", self.mutates_state)?;
        writeln!(out, "streams_output: {}", self.streams_output)?;
        writeln!(out, "log_dir: {}", self.log_dir.display())
    }
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

impl Render for ConfigEditJson<'_> {
    fn human(&self, out: &mut dyn io::Write) -> io::Result<()> {
        if !self.changed {
            return writeln!(out, "{} {}: no change", self.action, self.project.name);
        }
        let verb = if self.dry_run { "would write" } else { "wrote" };
        writeln!(
            out,
            "{} {}: {verb} {}",
            self.action,
            self.project.name,
            self.path.display()
        )
    }
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

pub fn project_list_item(project: &Project) -> ProjectListItem<'_> {
    ProjectListItem {
        id: &project.id,
        name: &project.name,
        root: &project.root,
        kinds: project.kinds.iter().map(|kind| kind.label()).collect(),
    }
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
