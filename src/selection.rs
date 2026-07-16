//! Shared project, command, process, and run selection helpers.
//!
//! Feature modules use these helpers to load current projects and state, then
//! resolve user queries into concrete project and command references.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::discover::discover_projects;
use crate::model::{CommandSpec, Project};
use crate::state::{ProcessView, State, StatePaths, state_paths};

pub fn load_projects(roots: &[PathBuf]) -> Result<(Vec<Project>, State, StatePaths)> {
    let paths = state_paths()?;
    let state = State::load(&paths)?;
    let scan_roots = effective_scan_roots(&state, roots);
    let projects = discover_projects(&scan_roots, &state)?;
    Ok((projects, state, paths))
}

/// The roots a scan actually covers: the given ones, or every registered
/// project root when none are given.
pub fn effective_scan_roots(state: &State, roots: &[PathBuf]) -> Vec<PathBuf> {
    if roots.is_empty() && !state.projects.is_empty() {
        state
            .projects
            .values()
            .map(|project| project.root.clone())
            .collect()
    } else {
        roots.to_vec()
    }
}

pub fn select_project<'a>(projects: &'a [Project], query: &str) -> Result<&'a Project> {
    if let Some(project) = projects
        .iter()
        .find(|project| project.id == query || project.name == query)
    {
        return Ok(project);
    }

    let matches = projects
        .iter()
        .filter(|project| project.root.to_string_lossy().contains(query))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [project] => Ok(*project),
        [] => anyhow::bail!("no project matches {query:?}"),
        many => {
            let names = many
                .iter()
                .map(|project| format!("{} ({})", project.name, project.root.display()))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("project query {query:?} is ambiguous: {names}")
        }
    }
}

pub fn select_projects<'a>(
    projects: &'a [Project],
    query: Option<&str>,
) -> Result<Vec<&'a Project>> {
    if let Some(query) = query {
        Ok(vec![select_project(projects, query)?])
    } else {
        Ok(projects.iter().collect())
    }
}

pub fn select_command<'a>(project: &'a Project, query: &str) -> Result<&'a CommandSpec> {
    project
        .commands
        .iter()
        .find(|command| command.name == query)
        .with_context(|| format!("{} has no command {query:?}", project.name))
}

pub fn filtered_processes(state: &State, project: Option<&Project>) -> Vec<ProcessView> {
    state
        .all_processes()
        .into_iter()
        .filter(|view| project.is_none_or(|project| view.process.project_id == project.id))
        .collect()
}
