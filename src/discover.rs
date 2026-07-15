//! Filesystem project discovery.
//!
//! Discovery scans configured roots, skips noisy directories, identifies project
//! kinds, and builds fresh `Project` views from adapters plus persisted state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

use crate::adapters::{collect_commands, collect_tools, detect_kinds, git_status};
use crate::config::load_deck_config;
use crate::model::{PluginSource, PluginSpec, Project, WorkflowSpec};
use crate::state::State;

const SKIP_DIRS: &[&str] = &[
    ".cache",
    ".cargo",
    ".claude",
    ".codex",
    ".config",
    ".git",
    ".local",
    ".mozilla",
    ".npm",
    ".rustup",
    ".ssh",
    ".var",
    ".vscode-oss",
    ".vscode-oss-shared",
    "Downloads",
    "build",
    "dist",
    "go",
    "node_modules",
    "sdk",
    "target",
];

pub fn discover_projects(roots: &[PathBuf], state: &State) -> Result<Vec<Project>> {
    let scan_roots = if roots.is_empty() {
        vec![home_dir()?]
    } else {
        roots.to_vec()
    };
    let tools = collect_tools();
    let mut projects = BTreeMap::new();

    for root in scan_roots {
        let root = root.canonicalize().unwrap_or(root);
        discover_root(&root, &tools, state, &mut projects)?;
    }

    Ok(projects.into_values().collect())
}

fn discover_root(
    root: &Path,
    tools: &BTreeMap<String, crate::model::ToolAvailability>,
    state: &State,
    projects: &mut BTreeMap<PathBuf, Project>,
) -> Result<()> {
    let skip_root_project = home_dir().is_ok_and(|home| home == root);
    let root_is_project = is_project_dir(root) && !skip_root_project;
    let filter_root = root.to_path_buf();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(false)
        .git_ignore(false)
        .filter_entry(move |entry| {
            let Some(name) = entry.file_name().to_str() else {
                return true;
            };
            if entry.path() == filter_root {
                return true;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return true;
            }
            if root_is_project {
                return false;
            }
            !SKIP_DIRS.contains(&name) && !has_project_ancestor(entry.path(), &filter_root)
        });

    for entry in builder.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if skip_root_project && path == root {
            continue;
        }
        if !is_project_dir(path) {
            continue;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        projects.entry(canonical.clone()).or_insert_with(|| {
            build_project(&canonical, tools, state)
                .unwrap_or_else(|error| fallback_project(&canonical, error.to_string()))
        });
    }

    Ok(())
}

fn build_project(
    root: &Path,
    tools: &BTreeMap<String, crate::model::ToolAvailability>,
    state: &State,
) -> Result<Project> {
    let config = load_deck_config(root)?;
    let kinds = detect_kinds(root);
    let name = config
        .as_ref()
        .and_then(|config| config.name.clone())
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| root.display().to_string());
    let id = project_id(root);
    let last_run = state.last_run_for(&id);
    let processes = state.running_processes_for(&id);
    let plugins = collect_plugins(config.as_ref(), state);

    Ok(Project {
        id: id.clone(),
        name,
        root: root.to_path_buf(),
        kinds,
        commands: collect_commands(root, config.as_ref(), tools)?,
        workflows: collect_workflows(config.as_ref()),
        plugins,
        git: git_status(root, tools),
        tools: tools.clone(),
        last_run,
        processes,
    })
}

fn fallback_project(root: &Path, error: String) -> Project {
    Project {
        id: project_id(root),
        name: root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_string(),
        root: root.to_path_buf(),
        kinds: Vec::new(),
        commands: Vec::new(),
        workflows: Vec::new(),
        plugins: Vec::new(),
        git: None,
        tools: BTreeMap::new(),
        last_run: None,
        processes: Vec::new(),
    }
    .with_error_command(error)
}

fn collect_workflows(config: Option<&crate::config::DeckConfig>) -> Vec<WorkflowSpec> {
    config
        .map(|config| {
            config
                .workflows
                .iter()
                .map(|(name, workflow)| WorkflowSpec {
                    name: name.clone(),
                    steps: workflow.steps.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn collect_plugins(config: Option<&crate::config::DeckConfig>, state: &State) -> Vec<PluginSpec> {
    let mut plugins = state
        .global_plugins()
        .into_iter()
        .map(|plugin| (plugin.name.clone(), plugin))
        .collect::<BTreeMap<_, _>>();

    if let Some(config) = config {
        for (name, plugin) in &config.plugins {
            plugins.insert(
                name.clone(),
                PluginSpec {
                    name: name.clone(),
                    cmd: plugin.cmd.clone(),
                    source: PluginSource::Project,
                },
            );
        }
    }

    plugins.into_values().collect()
}

trait WithErrorCommand {
    fn with_error_command(self, error: String) -> Self;
}

impl WithErrorCommand for Project {
    fn with_error_command(mut self, error: String) -> Self {
        self.commands.push(crate::model::CommandSpec {
            name: "discovery-error".to_string(),
            source: crate::model::CommandSource::DeckToml,
            command: String::new(),
            argv: None,
            cwd: self.root.clone(),
            kind: crate::model::CommandKind::Once,
            port: None,
            category: crate::model::CommandCategory::Utility,
            available: false,
            unavailable_reason: Some(error),
        });
        self
    }
}

fn is_project_dir(path: &Path) -> bool {
    [
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "Makefile",
        "justfile",
        "Justfile",
        "deck.toml",
    ]
    .iter()
    .any(|marker| path.join(marker).exists())
}

fn has_project_ancestor(path: &Path, root: &Path) -> bool {
    let mut current = path.parent();
    while let Some(parent) = current {
        if parent == root {
            return false;
        }
        if is_project_dir(parent) {
            return true;
        }
        current = parent.parent();
    }
    false
}

pub fn project_id(root: &Path) -> String {
    let text = root.to_string_lossy();
    let hash = fnv1a64(text.as_bytes());
    format!("{hash:016x}")
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn home_dir() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .context("could not find home directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn project_id_is_stable() {
        assert_eq!(
            project_id(Path::new("/tmp/example")),
            project_id(Path::new("/tmp/example"))
        );
        assert_ne!(
            project_id(Path::new("/tmp/example")),
            project_id(Path::new("/tmp/other"))
        );
    }

    #[test]
    fn discovers_projects_and_skips_noisy_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let rust_project = root.join("app");
        let ignored_project = root.join("target").join("generated");
        fs::create_dir_all(&rust_project).unwrap();
        fs::create_dir_all(&ignored_project).unwrap();
        fs::write(
            rust_project.join("Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            ignored_project.join("Cargo.toml"),
            "[package]\nname='ignored'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();

        let projects = discover_projects(&[root.to_path_buf()], &State::default()).unwrap();

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "app");
        assert!(
            projects[0]
                .commands
                .iter()
                .any(|command| command.name == "test")
        );
    }

    #[test]
    fn project_root_scan_does_not_discover_nested_members() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let nested = root.join("crates").join("member");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='root'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            nested.join("Cargo.toml"),
            "[package]\nname='member'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();

        let projects = discover_projects(&[root.to_path_buf()], &State::default()).unwrap();

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].root, root);
    }
}
