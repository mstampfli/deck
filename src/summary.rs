//! Project startup summaries.
//!
//! `deck summary` combines project context, command safety metadata, sandbox
//! profile summaries, tasks, and suggested next Deck commands into one bundle.
//! Humans get a cockpit overview; `--json` emits the same bundle for agents
//! bootstrapping into a project.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::{SandboxConfig, load_deck_config};
use crate::context::ContextBundle;
use crate::contracts::{ProjectRef, Render, emit, project_ref};
use crate::safety::{CommandSafety, command_safety};
use crate::selection::{load_projects, select_project};

#[derive(Debug, Serialize)]
pub struct SummaryJson<'a> {
    ok: bool,
    generated_at: DateTime<Utc>,
    project: ProjectRef<'a>,
    context: ContextBundle,
    commands: Vec<SummaryCommandSafety>,
    sandbox_profiles: Vec<SummarySandboxProfile>,
    suggested_next_commands: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct SummaryCommandSafety {
    name: String,
    safety: CommandSafety,
}

#[derive(Debug, Serialize)]
struct SummarySandboxProfile {
    name: String,
    config: SandboxConfig,
}

pub fn summary(project_query: &str, json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let context = crate::context::build_context(project, &state)?;
    let commands = project
        .commands
        .iter()
        .map(|command| SummaryCommandSafety {
            name: command.name.clone(),
            safety: command_safety(command),
        })
        .collect::<Vec<_>>();
    let sandbox_profiles = load_deck_config(&project.root)?
        .map(|config| {
            config
                .sandbox
                .into_iter()
                .map(|(name, config)| SummarySandboxProfile { name, config })
                .collect()
        })
        .unwrap_or_default();
    let name = &project.name;
    emit(
        &SummaryJson {
            ok: true,
            generated_at: Utc::now(),
            project: project_ref(project),
            context,
            commands,
            sandbox_profiles,
            suggested_next_commands: vec![
                vec!["deck".into(), "commands".into(), name.clone()],
                vec!["deck".into(), "tasks".into(), "list".into(), name.clone()],
                vec!["deck".into(), "context".into(), name.clone()],
                vec!["deck".into(), "sandbox".into(), "doctor".into()],
            ],
        },
        json,
    )
}

impl Render for SummaryJson<'_> {
    fn human(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        let bundle = &self.context;
        writeln!(
            out,
            "{} ({}) [{}]",
            self.project.name,
            self.project.root.display(),
            bundle.project.kinds.join(", ")
        )?;
        if let Some(description) = &bundle.project.description {
            writeln!(out, "{description}")?;
        }
        match &bundle.git {
            Some(git) => writeln!(
                out,
                "git: {} changed={} ahead={} behind={}",
                git.branch, git.changed, git.ahead, git.behind
            )?,
            None => writeln!(out, "git: no status")?,
        }

        writeln!(out, "\ncommands:")?;
        if self.commands.is_empty() {
            writeln!(out, "  none")?;
        }
        for entry in &self.commands {
            let command = bundle
                .commands
                .iter()
                .find(|command| command.name == entry.name);
            let mut traits = Vec::new();
            if entry.safety.server {
                traits.push("server");
            }
            traits.push(if entry.safety.direct_argv {
                "argv"
            } else {
                "shell"
            });
            if entry.safety.locked_sandbox_compatible {
                traits.push("locked-ok");
            }
            writeln!(
                out,
                "  {:<18} [{}] {}",
                entry.name,
                traits.join(","),
                command
                    .map(|command| command.command.as_str())
                    .unwrap_or("")
            )?;
        }

        writeln!(out, "\nworkflows:")?;
        if bundle.workflows.is_empty() {
            writeln!(out, "  none")?;
        }
        for workflow in &bundle.workflows {
            writeln!(
                out,
                "  {:<18} {}",
                workflow.name,
                workflow.steps.join(" -> ")
            )?;
        }

        writeln!(out, "\ntasks:")?;
        if bundle.tasks.is_empty() {
            writeln!(out, "  none")?;
        }
        for task in &bundle.tasks {
            writeln!(
                out,
                "  {:<18} {:<8} {}",
                task.name,
                task.config.status.label(),
                task.config.title.as_deref().unwrap_or("")
            )?;
        }

        writeln!(out, "\nsandbox profiles:")?;
        if self.sandbox_profiles.is_empty() {
            writeln!(out, "  none")?;
        }
        for profile in &self.sandbox_profiles {
            writeln!(
                out,
                "  {:<18} network={} readonly_project={} allow_shell={}",
                profile.name,
                profile.config.network,
                profile.config.readonly_project,
                profile.config.allow_shell
            )?;
        }

        writeln!(out, "\nprocesses:")?;
        if bundle.processes.is_empty() {
            writeln!(out, "  none")?;
        }
        for process in &bundle.processes {
            writeln!(
                out,
                "  {:<18} pid {} log {}",
                process.command_name,
                process.pid,
                process.log_path.display()
            )?;
        }

        writeln!(out, "\nrecent runs:")?;
        if bundle.recent_runs.is_empty() {
            writeln!(out, "  none")?;
        }
        for run in &bundle.recent_runs {
            let exit = run.exit_label();
            writeln!(
                out,
                "  {:<18} exit={} {}",
                run.command_name, exit, run.finished_at
            )?;
        }

        writeln!(out, "\nnext:")?;
        for suggestion in &self.suggested_next_commands {
            writeln!(out, "  {}", suggestion.join(" "))?;
        }
        writeln!(
            out,
            "\nkey file snippets: deck context {}",
            self.project.name
        )
    }
}
