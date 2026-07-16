//! Run history views and rerun support.
//!
//! History is backed by the existing `State.runs` list; this module does not
//! create a second history store.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::contracts::{ProjectRef, Render, emit, project_ref};
use crate::model::RunSummary;
use crate::selection::{load_projects, select_project};

#[derive(Debug, Serialize)]
struct RecentJson<'a> {
    ok: bool,
    project: Option<ProjectRef<'a>>,
    runs: Vec<RecentRunJson>,
}

#[derive(Debug, Serialize)]
struct RecentRunJson {
    project_name: String,
    #[serde(flatten)]
    run: RunSummary,
}

impl Render for RecentJson<'_> {
    fn human(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        for entry in &self.runs {
            let exit = if !entry.run.finished && crate::state::is_run_alive(&entry.run) {
                "running".to_string()
            } else {
                entry.run.exit_label()
            };
            writeln!(
                out,
                "{:<24} {:<18} exit={exit:<7} log={}",
                entry.project_name,
                entry.run.command_name,
                entry.run.log_path.display()
            )?;
        }
        Ok(())
    }
}

pub fn recent(project_query: Option<&str>, limit: usize, json: bool) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let project = project_query
        .map(|query| select_project(&projects, query))
        .transpose()?;
    let runs = recent_runs(
        &state.runs,
        project.map(|project| project.id.as_str()),
        limit,
    )
    .into_iter()
    .map(|run| RecentRunJson {
        project_name: projects
            .iter()
            .find(|project| project.id == run.project_id)
            .map(|project| project.name.clone())
            .unwrap_or_else(|| run.project_id.clone()),
        run,
    })
    .collect();
    emit(
        &RecentJson {
            ok: true,
            project: project.map(project_ref),
            runs,
        },
        json,
    )
}

pub fn rerun(
    project_query: Option<&str>,
    command_query: Option<&str>,
    json: bool,
    dry_run: bool,
    timeout_seconds: Option<u64>,
) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let (project_id, command_name) = match (project_query, command_query) {
        (Some(project_query), Some(command_query)) => {
            let project = select_project(&projects, project_query)?;
            (project.id.clone(), command_query.to_string())
        }
        (Some(project_query), None) => {
            let project = select_project(&projects, project_query)?;
            let run = state
                .last_run_for(&project.id)
                .with_context(|| format!("{} has no recent runs", project.name))?;
            (project.id.clone(), run.command_name)
        }
        (None, None) => {
            let run = state
                .runs
                .iter()
                .next_back()
                .cloned()
                .context("no recent runs")?;
            (run.project_id, run.command_name)
        }
        (None, Some(_)) => unreachable!("clap positional ordering prevents this shape"),
    };
    crate::commands::run_project_command(&project_id, &command_name, json, dry_run, timeout_seconds)
}

pub(crate) fn recent_runs(
    runs: &[RunSummary],
    project_id: Option<&str>,
    limit: usize,
) -> Vec<RunSummary> {
    runs.iter()
        .rev()
        .filter(|run| project_id.is_none_or(|project_id| run.project_id == project_id))
        .take(limit)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    fn run(project_id: &str, command_name: &str) -> RunSummary {
        RunSummary {
            project_id: project_id.to_string(),
            command_name: command_name.to_string(),
            command: command_name.to_string(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
            exit_code: Some(0),
            log_path: PathBuf::from("unused"),
            pid: None,
            finished: true,
            timed_out: false,
        }
    }

    #[test]
    fn recent_runs_filters_project_and_limits() {
        let runs = vec![run("a", "one"), run("b", "two"), run("a", "three")];

        let recent = recent_runs(&runs, Some("a"), 1);

        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].command_name, "three");
    }
}
