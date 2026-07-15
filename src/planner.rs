//! Dry-run planning for commands and workflows.
//!
//! Plans describe what Deck would run without spawning processes or mutating
//! runtime state.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::contracts::{CommandPlan, WorkflowPlan, emit, project_ref};
use crate::model::{CommandSpec, Project, WorkflowSpec};
use crate::selection::select_command;

pub fn command_plan(
    project: &Project,
    command: &CommandSpec,
    runs_dir: &PathBuf,
    json: bool,
) -> Result<()> {
    emit(
        &CommandPlan {
            ok: command.available,
            project: project_ref(project),
            command,
            mutates_state: true,
            streams_output: !json,
            log_dir: runs_dir,
        },
        json,
    )
}

pub fn workflow_plan(
    project: &Project,
    workflow: &WorkflowSpec,
    runs_dir: &PathBuf,
    json: bool,
) -> Result<()> {
    let steps = workflow
        .steps
        .iter()
        .map(|step| {
            select_command(project, step).with_context(|| {
                format!(
                    "workflow {} references missing command {step:?}",
                    workflow.name
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    emit(
        &WorkflowPlan {
            ok: steps.iter().all(|command| command.available),
            project: project_ref(project),
            workflow,
            steps,
            mutates_state: true,
            streams_output: !json,
            log_dir: runs_dir,
        },
        json,
    )
}
