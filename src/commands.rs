//! Shared command handlers for project lists, command runs, processes, and workflows.
//!
//! These handlers sit between CLI routing and lower-level execution modules.

use anyhow::Result;

use crate::contracts::{
    ProcessJson, ProjectListItem, RunJson, WorkflowRunJson, print_json, project_ref,
};
use crate::planner::{command_plan, workflow_plan};
use crate::process::run_command_stream;
use crate::selection::{filtered_processes, load_projects, select_command, select_project};

pub fn list(json: bool) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    if json {
        let output = projects
            .iter()
            .map(|project| ProjectListItem {
                id: &project.id,
                name: &project.name,
                root: &project.root,
                kinds: project.kinds.iter().map(|kind| kind.label()).collect(),
            })
            .collect::<Vec<_>>();
        return print_json(&output);
    }
    for project in projects {
        let kinds = project
            .kinds
            .iter()
            .map(|kind| kind.label())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{:<18} {:<24} {:<20} {}",
            project.id,
            project.name,
            kinds,
            project.root.display()
        );
    }
    Ok(())
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
    if json {
        print_json(&RunJson {
            ok: result.summary.exit_code == Some(0),
            project: project_ref(project),
            command: &result.summary.command_name,
            exit_code: result.summary.exit_code,
            log_path: &result.summary.log_path,
        })?;
    } else {
        println!("log: {}", result.summary.log_path.display());
    }
    state.record_run(result.summary);
    state.save(&paths)?;
    Ok(())
}

pub fn ps(project_query: Option<&str>, json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let selected_project = project_query
        .map(|query| select_project(&projects, query))
        .transpose()?;
    let views = filtered_processes(&state, selected_project);
    if json {
        let output = views
            .into_iter()
            .map(|view| ProcessJson {
                process: view.process,
                alive: view.alive,
            })
            .collect::<Vec<_>>();
        return print_json(&output);
    }
    for view in views {
        let status = if view.alive { "running" } else { "stopped" };
        let port = view
            .process
            .port
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        println!(
            "{:<8} pid={:<8} {:<18} {:<18} {} {}",
            status,
            view.process.pid,
            view.process.project_name,
            view.process.command_name,
            port,
            view.process.log_path.display()
        );
    }
    Ok(())
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
    if json {
        let output = WorkflowRunJson {
            ok: result.failed_step.is_none(),
            project: project_ref(project),
            workflow: &result.workflow_name,
            completed_steps: &result.completed_steps,
            failed_step: &result.failed_step,
        };
        print_json(&output)?;
        if result.failed_step.is_some() {
            anyhow::bail!("workflow {} failed", result.workflow_name);
        }
        return Ok(());
    }
    if let Some(step) = result.failed_step {
        anyhow::bail!("workflow {} failed at step {step}", result.workflow_name);
    }
    println!(
        "workflow {} completed {} steps",
        result.workflow_name,
        result.completed_steps.len()
    );
    Ok(())
}
