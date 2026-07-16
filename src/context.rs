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
    "README",
    "ARCHITECTURE.md",
    "docs/ARCHITECTURE.md",
    "CLAUDE.md",
    "AGENTS.md",
    "CONTRIBUTING.md",
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
    /// One-line answer to "what is this project", from the manifest
    /// description or the README's first paragraph.
    #[serde(default)]
    pub description: Option<String>,
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
            description: project_description(&project.root),
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
    if let Some(description) = &bundle.project.description {
        out.push_str(&format!("{description}\n\n"));
    }
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

const MAX_DESCRIPTION_CHARS: usize = 240;

/// Resolve a one-line project description: curated manifest fields first
/// (Cargo.toml, package.json), then the README's first prose paragraph.
pub fn project_description(root: &Path) -> Option<String> {
    cargo_description(root)
        .or_else(|| package_json_description(root))
        .or_else(|| readme_description(root))
}

fn cargo_description(root: &Path) -> Option<String> {
    let raw = fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let value: toml::Value = toml::from_str(&raw).ok()?;
    let description = value.get("package")?.get("description")?.as_str()?;
    normalized_description(description)
}

fn package_json_description(root: &Path) -> Option<String> {
    let raw = fs::read_to_string(root.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    normalized_description(value.get("description")?.as_str()?)
}

fn readme_description(root: &Path) -> Option<String> {
    let raw = ["README.md", "README"]
        .iter()
        .find_map(|name| fs::read_to_string(root.join(name)).ok())?;
    let mut paragraph: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if paragraph.is_empty() {
                continue;
            }
            break;
        }
        // Skip headings, badges, images, HTML, and horizontal rules until
        // the first real prose paragraph.
        if paragraph.is_empty()
            && (trimmed.starts_with('#')
                || trimmed.starts_with("![")
                || trimmed.starts_with("[!")
                || trimmed.starts_with('<')
                || trimmed.starts_with("---")
                || trimmed.starts_with("==="))
        {
            continue;
        }
        paragraph.push(trimmed);
    }
    normalized_description(&paragraph.join(" "))
}

fn normalized_description(raw: &str) -> Option<String> {
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_DESCRIPTION_CHARS {
        return Some(text);
    }
    let truncated: String = text.chars().take(MAX_DESCRIPTION_CHARS).collect();
    Some(format!("{}...", truncated.trim_end()))
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
    fn description_prefers_manifest_over_readme() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\ndescription = \"A tiny cockpit\"\n",
        )
        .unwrap();
        fs::write(temp.path().join("README.md"), "# X\n\nProse here.\n").unwrap();

        assert_eq!(
            project_description(temp.path()).as_deref(),
            Some("A tiny cockpit")
        );
    }

    #[test]
    fn description_falls_back_to_readme_prose() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("README.md"),
            "# Title\n\n![badge](x.svg)\n\nDoes one thing\nwell.\n\nMore text.\n",
        )
        .unwrap();

        assert_eq!(
            project_description(temp.path()).as_deref(),
            Some("Does one thing well.")
        );
    }

    #[test]
    fn context_includes_agent_doc_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("deck.toml"), "name = \"fixture\"\n").unwrap();
        fs::write(temp.path().join("CLAUDE.md"), "agent notes\n").unwrap();
        fs::write(temp.path().join("ARCHITECTURE.md"), "theory\n").unwrap();
        let project = fixture_project(temp.path().to_path_buf());

        let bundle = build_context(&project, &State::default()).unwrap();
        let names: Vec<String> = bundle
            .files
            .iter()
            .map(|file| file.path.display().to_string())
            .collect();

        assert!(names.contains(&"CLAUDE.md".to_string()), "{names:?}");
        assert!(names.contains(&"ARCHITECTURE.md".to_string()), "{names:?}");
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
