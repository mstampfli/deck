//! Shared command handlers for project lists, command runs, processes, and workflows.
//!
//! These handlers sit between CLI routing and lower-level execution modules.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::contracts::{
    ForgetJson, ProcessJson, ProjectListItem, RunJson, WorkflowRunJson, emit, project_list_item,
    project_ref,
};
use crate::errors::Reported;
use crate::planner::{command_plan, workflow_plan};
use crate::process::run_command_stream;
use crate::selection::{filtered_processes, load_projects, select_command, select_project};
use crate::state::{State, is_process_alive, state_paths};

pub fn list(json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let mut output = projects.iter().map(project_list_item).collect::<Vec<_>>();
    // Registry entries whose root no longer exists are not discovered; keep
    // them visible and marked so they can be forgotten instead of vanishing.
    for entry in state.projects.values() {
        if !projects.iter().any(|project| project.id == entry.id) && !entry.root.exists() {
            output.push(ProjectListItem {
                id: &entry.id,
                name: &entry.name,
                root: &entry.root,
                kinds: Vec::new(),
                missing: true,
            });
        }
    }
    emit(&output, json)
}

/// Remove a project from the registry without touching its files.
///
/// Works on registry entries directly so stale roots can be forgotten, and
/// refuses while the project still has a live tracked process.
pub fn forget(project_query: &str, json: bool) -> Result<()> {
    let paths = state_paths()?;
    let mut state = State::load(&paths)?;
    let entry = state
        .projects
        .values()
        .find(|entry| entry.id == project_query || entry.name == project_query)
        .cloned()
        .with_context(|| format!("no registered project matches {project_query:?}"))?;
    if let Some(process) = state
        .processes
        .iter()
        .find(|process| process.project_id == entry.id && is_process_alive(process))
    {
        anyhow::bail!(
            "{} still has a running process ({} pid {}); stop it first",
            entry.name,
            process.command_name,
            process.pid
        );
    }
    state.projects.remove(&entry.id);
    let before = state.processes.len();
    state
        .processes
        .retain(|process| process.project_id != entry.id);
    let removed_process_records = before - state.processes.len();
    state.save(&paths)?;
    emit(
        &ForgetJson {
            ok: true,
            id: entry.id,
            name: entry.name,
            root: entry.root,
            removed_process_records,
        },
        json,
    )
}

pub fn run_project_command(
    project_query: &str,
    command_query: &str,
    json: bool,
    dry_run: bool,
    timeout_seconds: Option<u64>,
) -> Result<()> {
    let (projects, mut state, paths) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let command = select_command(project, command_query)?;
    if dry_run {
        return command_plan(project, command, &paths.runs_dir, json);
    }
    if let Some(timeout_seconds) = timeout_seconds {
        crate::sandbox::validate_timeout_seconds(timeout_seconds)?;
    }
    let timeout = timeout_seconds.map(Duration::from_secs);
    let result = if json {
        run_command_stream(project, command, &mut state, &paths, timeout, |_| Ok(()))?
    } else {
        run_command_stream(project, command, &mut state, &paths, timeout, |line| {
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
            timed_out: result.summary.timed_out,
            log_path: &result.summary.log_path,
        },
        json,
    )?;
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
