//! Workflow selection and sequential execution.
//!
//! Workflows run existing one-shot commands in order and stop at the first
//! failing step while recording completed run summaries.

use anyhow::{Context, Result};

use crate::model::{CommandKind, Project, WorkflowRunResult, WorkflowSpec};
use crate::process::run_command_stream;
use crate::state::{State, StatePaths};

pub fn select_workflow<'a>(project: &'a Project, query: &str) -> Result<&'a WorkflowSpec> {
    project
        .workflows
        .iter()
        .find(|workflow| workflow.name == query)
        .with_context(|| format!("{} has no workflow {query:?}", project.name))
}

pub fn run_workflow_stream<F>(
    project: &Project,
    workflow: &WorkflowSpec,
    state: &mut State,
    paths: &StatePaths,
    mut on_output: F,
) -> Result<WorkflowRunResult>
where
    F: FnMut(&str) -> Result<()>,
{
    let mut completed_steps = Vec::new();

    for step in &workflow.steps {
        let command = project
            .commands
            .iter()
            .find(|command| command.name == *step)
            .with_context(|| {
                format!(
                    "workflow {} references missing command {step:?}",
                    workflow.name
                )
            })?;
        if command.kind != CommandKind::Once {
            anyhow::bail!(
                "workflow {} step {} is a {} command; workflows only run one-shot commands",
                workflow.name,
                command.name,
                command.kind.label()
            );
        }

        on_output(&format!("==> {}:{}\n", workflow.name, step))?;
        let result =
            run_command_stream(project, command, state, paths, None, |line| on_output(line))?;
        let exit_code = result.summary.exit_code;
        completed_steps.push(result.summary);
        if exit_code != Some(0) {
            return Ok(WorkflowRunResult {
                workflow_name: workflow.name.clone(),
                completed_steps,
                failed_step: Some(step.clone()),
            });
        }
    }

    Ok(WorkflowRunResult {
        workflow_name: workflow.name.clone(),
        completed_steps,
        failed_step: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CommandCategory, CommandKind, CommandSource, CommandSpec, ProjectKind, ToolAvailability,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn fixture_project(root: PathBuf, steps: Vec<&str>) -> Project {
        Project {
            id: "fixture".to_string(),
            name: "fixture".to_string(),
            root: root.clone(),
            kinds: vec![ProjectKind::Deck],
            commands: steps
                .into_iter()
                .map(|name| CommandSpec {
                    name: name.to_string(),
                    source: CommandSource::DeckToml,
                    command: format!("printf '{name}\\n'"),
                    argv: None,
                    cwd: root.clone(),
                    kind: CommandKind::Once,
                    port: None,
                    category: CommandCategory::Utility,
                    available: true,
                    unavailable_reason: None,
                })
                .collect(),
            workflows: vec![WorkflowSpec {
                name: "ship".to_string(),
                steps: vec!["fmt".to_string(), "test".to_string()],
            }],
            plugins: Vec::new(),
            git: None,
            tools: BTreeMap::<String, ToolAvailability>::new(),
            last_run: None,
            processes: Vec::new(),
        }
    }

    #[test]
    fn runs_workflow_steps_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let project = fixture_project(temp.path().to_path_buf(), vec!["fmt", "test"]);
        let workflow = select_workflow(&project, "ship").unwrap().clone();
        let paths = StatePaths {
            state_file: temp.path().join("state.toml"),
            runs_dir: temp.path().join("runs"),
        };
        let mut state = State::default();
        let mut output = String::new();

        let result = run_workflow_stream(&project, &workflow, &mut state, &paths, |line| {
            output.push_str(line);
            Ok(())
        })
        .unwrap();

        assert_eq!(result.failed_step, None);
        assert_eq!(result.completed_steps.len(), 2);
        assert!(output.contains("==> ship:fmt"));
        assert!(output.contains("fmt"));
        assert_eq!(state.runs.len(), 2);
    }

    #[test]
    fn workflow_stops_on_first_failing_step() {
        let temp = tempfile::tempdir().unwrap();
        let mut project = fixture_project(temp.path().to_path_buf(), vec!["ok", "fail", "skip"]);
        project.commands[1].command = "exit 7".to_string();
        project.workflows[0].steps = vec!["ok".to_string(), "fail".to_string(), "skip".to_string()];
        let workflow = project.workflows[0].clone();
        let paths = StatePaths {
            state_file: temp.path().join("state.toml"),
            runs_dir: temp.path().join("runs"),
        };
        let mut state = State::default();

        let result =
            run_workflow_stream(&project, &workflow, &mut state, &paths, |_| Ok(())).unwrap();

        assert_eq!(result.failed_step, Some("fail".to_string()));
        assert_eq!(result.completed_steps.len(), 2);
        assert_eq!(state.runs.len(), 2);
    }
}
