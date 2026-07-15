//! Project-local task management backed by `deck.toml`.
//!
//! Tasks are small project notes with status, title, and optional notes. Edits
//! use the same lock and atomic write path as other project config changes.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use serde::Serialize;

use crate::config::{
    DeckConfig, TaskConfig, TaskStatus, deck_config_path, load_or_default_deck_config,
    lock_deck_config, write_deck_config,
};
use crate::contracts::{ConfigEditJson, ProjectRef, Render, emit, project_ref};
use crate::selection::{load_projects, select_project};

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List a project's tasks
    List { project: String },
    /// Add a task
    Add {
        project: String,
        name: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value_t = TaskStatusArg::Todo)]
        status: TaskStatusArg,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Update a task's title, status, or notes
    Set {
        project: String,
        name: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        status: Option<TaskStatusArg>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a task
    Remove {
        project: String,
        name: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum TaskStatusArg {
    #[default]
    Todo,
    Doing,
    Done,
    Blocked,
}

impl std::fmt::Display for TaskStatusArg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Blocked => "blocked",
        })
    }
}

impl From<TaskStatusArg> for TaskStatus {
    fn from(status: TaskStatusArg) -> Self {
        match status {
            TaskStatusArg::Todo => Self::Todo,
            TaskStatusArg::Doing => Self::Doing,
            TaskStatusArg::Done => Self::Done,
            TaskStatusArg::Blocked => Self::Blocked,
        }
    }
}

#[derive(Debug, Serialize)]
struct TaskListJson<'a> {
    ok: bool,
    project: ProjectRef<'a>,
    tasks: Vec<TaskItemJson>,
}

#[derive(Debug, Serialize)]
struct TaskItemJson {
    name: String,
    title: Option<String>,
    status: &'static str,
    notes: Option<String>,
}

impl Render for TaskListJson<'_> {
    fn human(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(
            out,
            "{} ({})",
            self.project.name,
            self.project.root.display()
        )?;
        for task in &self.tasks {
            writeln!(
                out,
                "  {:<18} {:<8} {}",
                task.name,
                task.status,
                task.title.as_deref().unwrap_or_default()
            )?;
            if let Some(notes) = &task.notes {
                writeln!(out, "    {notes}")?;
            }
        }
        Ok(())
    }
}

pub fn run(action: TaskCommand, json: bool) -> Result<()> {
    match action {
        TaskCommand::List { project } => list(&project, json),
        TaskCommand::Add {
            project,
            name,
            title,
            status,
            notes,
            replace,
            dry_run,
        } => edit(&project, "add-task", dry_run, json, |config| {
            validate_task_name(&name)?;
            if config.tasks.contains_key(&name) && !replace {
                anyhow::bail!("{name:?} already exists; pass --replace to overwrite it");
            }
            config.tasks.insert(
                name,
                TaskConfig {
                    title,
                    status: status.into(),
                    notes,
                },
            );
            Ok(true)
        }),
        TaskCommand::Set {
            project,
            name,
            title,
            status,
            notes,
            dry_run,
        } => edit(&project, "set-task", dry_run, json, |config| {
            validate_task_name(&name)?;
            let task = config
                .tasks
                .get_mut(&name)
                .ok_or_else(|| anyhow::anyhow!("no task named {name:?}"))?;
            if let Some(title) = title {
                task.title = Some(title);
            }
            if let Some(status) = status {
                task.status = status.into();
            }
            if let Some(notes) = notes {
                task.notes = Some(notes);
            }
            Ok(true)
        }),
        TaskCommand::Remove {
            project,
            name,
            dry_run,
        } => edit(&project, "remove-task", dry_run, json, |config| {
            validate_task_name(&name)?;
            if config.tasks.remove(&name).is_none() {
                anyhow::bail!("no task named {name:?}");
            }
            Ok(true)
        }),
    }
}

fn list(project_query: &str, json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let config = load_or_default_deck_config(&project.root, &project.name)?;
    let tasks = config
        .tasks
        .into_iter()
        .map(|(name, task)| TaskItemJson {
            name,
            title: task.title,
            status: task.status.label(),
            notes: task.notes,
        })
        .collect::<Vec<_>>();
    emit(
        &TaskListJson {
            ok: true,
            project: project_ref(project),
            tasks,
        },
        json,
    )
}

fn edit<F>(
    project_query: &str,
    action: &'static str,
    dry_run: bool,
    json: bool,
    mutate: F,
) -> Result<()>
where
    F: FnOnce(&mut DeckConfig) -> Result<bool>,
{
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let _lock = lock_deck_config(&project.root)?;
    let mut config = load_or_default_deck_config(&project.root, &project.name)?;
    let changed = mutate(&mut config)?;
    let path = deck_config_path(&project.root);
    if changed && !dry_run {
        write_deck_config(&project.root, &config)?;
    }
    emit(
        &ConfigEditJson {
            ok: true,
            project: project_ref(project),
            path,
            action,
            dry_run,
            changed,
            config,
        },
        json,
    )
}

fn validate_task_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("task name cannot be empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_arg_maps_to_config_status() {
        assert!(matches!(
            TaskStatus::from(TaskStatusArg::Doing),
            TaskStatus::Doing
        ));
        assert_eq!(TaskStatus::Done.label(), "done");
    }
}
