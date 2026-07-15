//! Dry-run planning for commands and workflows.
//!
//! Plans describe what Deck would run without spawning processes or mutating
//! runtime state.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::contracts::{CommandPlan, WorkflowPlan, print_json, project_ref};
use crate::model::{CommandSpec, Project, WorkflowSpec};
use crate::selection::select_command;

pub fn command_plan(
    project: &Project,
    command: &CommandSpec,
    runs_dir: &PathBuf,
    json: bool,
) -> Result<()> {
    if json {
        return print_json(&CommandPlan {
            ok: command.available,
            project: project_ref(project),
            command,
            mutates_state: true,
            streams_output: false,
            log_dir: runs_dir,
        });
    }
    println!("project: {} ({})", project.name, project.root.display());
    println!("command: {}", command.name);
    println!("cwd: {}", command.cwd.display());
    println!("shell: {}", command.command);
    println!("available: {}", command.available);
    if let Some(reason) = &command.unavailable_reason {
        println!("unavailable: {reason}");
    }
    println!("mutates_state: true");
    println!("streams_output: true");
    println!("log_dir: {}", runs_dir.display());
    Ok(())
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
    if json {
        return print_json(&WorkflowPlan {
            ok: steps.iter().all(|command| command.available),
            project: project_ref(project),
            workflow,
            steps,
            mutates_state: true,
            streams_output: false,
            log_dir: runs_dir,
        });
    }
    println!("project: {} ({})", project.name, project.root.display());
    println!("workflow: {}", workflow.name);
    println!("steps: {}", workflow.steps.join(" -> "));
    println!(
        "available: {}",
        steps.iter().all(|command| command.available)
    );
    println!("mutates_state: true");
    println!("streams_output: true");
    println!("log_dir: {}", runs_dir.display());
    Ok(())
}
