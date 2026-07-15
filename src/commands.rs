//! Shared command handlers for project lists, command runs, processes, and workflows.
//!
//! These handlers sit between CLI routing and lower-level execution modules.

use anyhow::Result;

use crate::contracts::{
    ProcessJson, RunJson, WorkflowRunJson, emit, project_list_item, project_ref,
};
use crate::errors::Reported;
use crate::planner::{command_plan, workflow_plan};
use crate::process::run_command_stream;
use crate::selection::{filtered_processes, load_projects, select_command, select_project};

pub fn list(json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let output = projects.iter().map(project_list_item).collect::<Vec<_>>();
    emit(&output, json)
}

pub fn run_project_command(
    project_query: &str,
    command_query: &str,
    json: bool,
    dry_run: bool,
) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let command = select_command(project, command_query)?;
    if dry_run {
        return command_plan(project, command, &paths.runs_dir, json);
    }
    let result = if json {
        run_command_stream(project, command, &paths, |_| Ok(()))?
    } else {
        run_command_stream(project, command, &paths, |line| {
            print!("{line}");
            Ok(())
        })?
    };
    let ok = result.summary.exit_code == Some(0);
    emit(
        &RunJson {
            ok,
            project: project_ref(project),
            command: &result.summary.command_name,
            exit_code: result.summary.exit_code,
            log_path: &result.summary.log_path,
        },
        json,
    )?;
    state.record_run(result.summary);
    state.save(&paths)?;
    if !ok {
        return Err(Reported.into());
    }
    Ok(())
}

pub fn ps(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let selected_project = project_query
        .map(|query| select_project(&projects, query))
        .transpose()?;
    let output = filtered_processes(&state, selected_project)
        .into_iter()
        .map(|view| ProcessJson {
            process: view.process,
            alive: view.alive,
        })
        .collect::<Vec<_>>();
    emit(&output, json)
}

pub fn run_workflow(
    project_query: &str,
    workflow_query: &str,
    json: bool,
    dry_run: bool,
) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let workflow = crate::workflow::select_workflow(project, workflow_query)?;
    if dry_run {
        return workflow_plan(project, workflow, &paths.runs_dir, json);
    }
    let result = if json {
        crate::workflow::run_workflow_stream(project, workflow, &mut state, &paths, |_| Ok(()))?
    } else {
        crate::workflow::run_workflow_stream(project, workflow, &mut state, &paths, |line| {
            print!("{line}");
            Ok(())
        })?
    };
    state.save(&paths)?;
    emit(
        &WorkflowRunJson {
            ok: result.failed_step.is_none(),
            project: project_ref(project),
            workflow: &result.workflow_name,
            completed_steps: &result.completed_steps,
            failed_step: &result.failed_step,
        },
        json,
    )?;
    if result.failed_step.is_some() {
        return Err(Reported.into());
    }
    Ok(())
}
