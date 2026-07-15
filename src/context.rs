//! Deterministic project context bundles for agents and external tools.
//!
//! Context bundles include project metadata, commands, command safety, workflows,
//! plugins, tasks, processes, recent runs, and bounded important files.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{TaskConfig, load_deck_config};
use crate::model::{
    CommandSpec, GitStatus, PluginSpec, ProcessRecord, Project, RunSummary, WorkflowSpec,
};
use crate::safety::{CommandSafety, command_safety};
use crate::state::State;

const MAX_FILE_BYTES: usize = 16 * 1024;
const CONTEXT_FILES: &[&str] = &[
    "deck.toml",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "Makefile",
    "justfile",
    "Justfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
    "README.md",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBundle {
    pub generated_at: DateTime<Utc>,
    pub project: ContextProject,
    pub git: Option<GitStatus>,
    pub commands: Vec<CommandSpec>,
    pub command_safety: Vec<ContextCommandSafety>,
    pub workflows: Vec<WorkflowSpec>,
    pub plugins: Vec<PluginSpec>,
    pub tasks: Vec<ContextTask>,
    pub processes: Vec<ProcessRecord>,
    pub recent_runs: Vec<RunSummary>,
    pub files: Vec<ContextFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextProject {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCommandSafety {
    pub command: String,
    pub safety: CommandSafety,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextTask {
    pub name: String,
    pub config: TaskConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFile {
    pub path: PathBuf,
    pub bytes: usize,
    pub truncated: bool,
    pub text: String,
}

pub fn build_context(project: &Project, state: &State) -> Result<ContextBundle> {
    let tasks = load_deck_config(&project.root)?
        .map(|config| {
            config
                .tasks
                .into_iter()
                .map(|(name, config)| ContextTask { name, config })
                .collect()
        })
        .unwrap_or_default();
    Ok(ContextBundle {
        generated_at: Utc::now(),
        project: ContextProject {
            id: project.id.clone(),
            name: project.name.clone(),
            root: project.root.clone(),
            kinds: project
                .kinds
                .iter()
                .map(|kind| kind.label().to_string())
                .collect(),
        },
        git: project.git.clone(),
        commands: project.commands.clone(),
        command_safety: project
            .commands
            .iter()
            .map(|command| ContextCommandSafety {
                command: command.name.clone(),
                safety: command_safety(command),
            })
            .collect(),
        workflows: project.workflows.clone(),
        plugins: project.plugins.clone(),
        tasks,
        processes: project.processes.clone(),
        recent_runs: recent_runs(project, state, 10),
        files: context_files(&project.root)?,
    })
}

pub fn render_markdown(bundle: &ContextBundle) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Deck Context: {}\n\n", bundle.project.name));
    out.push_str(&format!("- generated_at: `{}`\n", bundle.generated_at));
    out.push_str(&format!("- root: `{}`\n", bundle.project.root.display()));
    out.push_str(&format!("- kinds: `{}`\n", bundle.project.kinds.join(", ")));
    if let Some(git) = &bundle.git {
        out.push_str(&format!(
            "- git: branch `{}`, changed {}, ahead {}, behind {}\n",
            git.branch, git.changed, git.ahead, git.behind
        ));
    }
    out.push('\n');

    out.push_str("## Commands\n\n");
    if bundle.commands.is_empty() {
        out.push_str("none\n\n");
    } else {
        for command in &bundle.commands {
            out.push_str(&format!(
                "- `{}` [{} {}]: `{}`\n",
                command.name,
                command.source.label(),
                command.kind.label(),
                command.command
            ));
        }
        out.push('\n');
    }

    out.push_str("## Workflows\n\n");
    if bundle.workflows.is_empty() {
        out.push_str("none\n\n");
    } else {
        for workflow in &bundle.workflows {
            out.push_str(&format!(
                "- `{}`: {}\n",
                workflow.name,
                workflow.steps.join(" -> ")
            ));
        }
        out.push('\n');
    }

    out.push_str("## Tasks\n\n");
    if bundle.tasks.is_empty() {
        out.push_str("none\n\n");
    } else {
        for task in &bundle.tasks {
            out.push_str(&format!(
                "- `{}` [{}]: {}\n",
                task.name,
                task.config.status.label(),
                task.config.title.as_deref().unwrap_or("")
            ));
        }
        out.push('\n');
    }

    out.push_str("## Plugins\n\n");
    if bundle.plugins.is_empty() {
        out.push_str("none\n\n");
    } else {
        for plugin in &bundle.plugins {
            out.push_str(&format!(
                "- `{}` [{}]: `{}`\n",
                plugin.name,
                plugin.source.label(),
                plugin.cmd
            ));
        }
        out.push('\n');
    }

    out.push_str("## Processes\n\n");
    if bundle.processes.is_empty() {
        out.push_str("none\n\n");
    } else {
        for process in &bundle.processes {
            out.push_str(&format!(
                "- `{}` pid {} port {:?} log `{}`\n",
                process.command_name,
                process.pid,
                process.port,
                process.log_path.display()
            ));
        }
        out.push('\n');
    }

    out.push_str("## Recent Runs\n\n");
    if bundle.recent_runs.is_empty() {
        out.push_str("none\n\n");
    } else {
        for run in &bundle.recent_runs {
            out.push_str(&format!(
                "- `{}` exit {:?} at `{}` log `{}`\n",
                run.command_name,
                run.exit_code,
                run.finished_at,
                run.log_path.display()
            ));
        }
        out.push('\n');
    }

    out.push_str("## Files\n\n");
    for file in &bundle.files {
        out.push_str(&format!(
            "### `{}`{} \n\n",
            file.path.display(),
            if file.truncated { " (truncated)" } else { "" }
        ));
        out.push_str("```text\n");
        out.push_str(&file.text);
        if !file.text.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n\n");
    }

    out
}

fn recent_runs(project: &Project, state: &State, limit: usize) -> Vec<RunSummary> {
    state
        .runs
        .iter()
        .rev()
        .filter(|run| run.project_id == project.id)
        .take(limit)
        .cloned()
        .collect()
}

fn context_files(root: &Path) -> Result<Vec<ContextFile>> {
    CONTEXT_FILES
        .iter()
        .filter_map(|relative| {
            let path = root.join(relative);
            path.exists().then_some((PathBuf::from(relative), path))
        })
        .map(|(relative, path)| read_context_file(&relative, &path))
        .collect()
}

fn read_context_file(relative: &Path, path: &Path) -> Result<ContextFile> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let truncated = bytes.len() > MAX_FILE_BYTES;
    let slice = &bytes[..bytes.len().min(MAX_FILE_BYTES)];
    let text = if slice.contains(&0) {
        "<binary file omitted>\n".to_string()
    } else {
        String::from_utf8_lossy(slice).into_owned()
    };
    Ok(ContextFile {
        path: relative.to_path_buf(),
        bytes: bytes.len(),
        truncated,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProjectKind, ToolAvailability};
    use std::collections::BTreeMap;

    fn fixture_project(root: PathBuf) -> Project {
        Project {
            id: "fixture".to_string(),
            name: "fixture".to_string(),
            root,
            kinds: vec![ProjectKind::Deck],
            commands: Vec::new(),
            workflows: Vec::new(),
            plugins: Vec::new(),
            git: None,
            tools: BTreeMap::<String, ToolAvailability>::new(),
            last_run: None,
            processes: Vec::new(),
        }
    }

    #[test]
    fn builds_context_with_bounded_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("deck.toml"), "name = \"fixture\"\n").unwrap();
        fs::write(temp.path().join("README.md"), "hello\n").unwrap();
        let project = fixture_project(temp.path().to_path_buf());

        let bundle = build_context(&project, &State::default()).unwrap();

        assert_eq!(bundle.project.name, "fixture");
        assert_eq!(bundle.files.len(), 2);
        assert!(render_markdown(&bundle).contains("# Deck Context: fixture"));
    }

    #[test]
    fn context_file_marks_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("README.md");
        fs::write(&path, "x".repeat(MAX_FILE_BYTES + 1)).unwrap();

        let file = read_context_file(Path::new("README.md"), &path).unwrap();

        assert!(file.truncated);
        assert_eq!(file.text.len(), MAX_FILE_BYTES);
    }
}
