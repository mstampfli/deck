//! Registered plugin protocol support.
//!
//! Plugins are ordinary registered commands. This module invokes them for
//! manifests, panels, actions, and action execution without requiring a special
//! filename or `PATH` convention.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{PluginSpec, Project};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub actions: Vec<PluginItem>,
    #[serde(default)]
    pub panels: Vec<PluginItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginItem {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

pub fn select_plugin<'a>(project: &'a Project, name: &str) -> Result<&'a PluginSpec> {
    project
        .plugins
        .iter()
        .find(|plugin| plugin.name == name)
        .with_context(|| format!("{} has no plugin {name:?}", project.name))
}

pub fn manifest(plugin: &PluginSpec, project: &Project) -> Result<PluginManifest> {
    let output = run_json_command(plugin, project, &["manifest", "--json"])?;
    serde_json::from_str(&output).with_context(|| format!("parsing {} manifest", plugin.name))
}

pub fn panels(plugin: &PluginSpec, project: &Project) -> Result<Value> {
    let output = run_json_command(
        plugin,
        project,
        &[
            "panels",
            "--json",
            "--project",
            &project.root.to_string_lossy(),
        ],
    )?;
    serde_json::from_str(&output).with_context(|| format!("parsing {} panels", plugin.name))
}

pub fn actions(plugin: &PluginSpec, project: &Project) -> Result<Value> {
    let output = run_json_command(
        plugin,
        project,
        &[
            "actions",
            "--json",
            "--project",
            &project.root.to_string_lossy(),
        ],
    )?;
    serde_json::from_str(&output).with_context(|| format!("parsing {} actions", plugin.name))
}

pub fn run_action(plugin: &PluginSpec, project: &Project, action: &str) -> Result<String> {
    run_command(
        plugin,
        project,
        &["run", action, "--project", &project.root.to_string_lossy()],
    )
}

fn run_json_command(plugin: &PluginSpec, project: &Project, args: &[&str]) -> Result<String> {
    run_command(plugin, project, args)
}

fn run_command(plugin: &PluginSpec, project: &Project, args: &[&str]) -> Result<String> {
    let command = append_args(&plugin.cmd, args);
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&project.root)
        .output()
        .with_context(|| format!("running plugin {}", plugin.name))?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        anyhow::bail!(
            "plugin {} exited with status {}\n{text}",
            plugin.name,
            output.status
        );
    }
    Ok(text)
}

fn append_args(cmd: &str, args: &[&str]) -> String {
    let mut command = cmd.to_string();
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command
}

fn shell_quote(arg: &str) -> String {
    if arg
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

pub fn command_from_path(path: &Path) -> Result<String> {
    let absolute = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    Ok(shell_quote(&absolute.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PluginSource, ProjectKind, ToolAvailability};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn fixture_project(root: &Path, plugin_cmd: String) -> Project {
        Project {
            id: "fixture".to_string(),
            name: "fixture".to_string(),
            root: root.to_path_buf(),
            kinds: vec![ProjectKind::Deck],
            commands: Vec::new(),
            workflows: Vec::new(),
            plugins: vec![PluginSpec {
                name: "test".to_string(),
                cmd: plugin_cmd,
                source: PluginSource::Project,
            }],
            git: None,
            tools: BTreeMap::<String, ToolAvailability>::new(),
            last_run: None,
            processes: Vec::new(),
        }
    }

    #[test]
    fn quotes_shell_args() {
        assert_eq!(shell_quote("simple/path"), "simple/path");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn runs_manifest_and_action_for_registered_command() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("plugin.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
case "$1" in
  manifest) printf '{"name":"test","actions":[{"id":"hello"}],"panels":[{"id":"main"}]}' ;;
  actions) printf '[{"id":"hello"}]' ;;
  panels) printf '[{"id":"main","text":"ok"}]' ;;
  run) printf 'ran:%s:%s' "$2" "$4" ;;
esac
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
        let project = fixture_project(temp.path(), command_from_path(&script).unwrap());
        let plugin = select_plugin(&project, "test").unwrap();

        let manifest = manifest(plugin, &project).unwrap();
        let actions = actions(plugin, &project).unwrap();
        let panels = panels(plugin, &project).unwrap();
        let run = run_action(plugin, &project, "hello").unwrap();

        assert_eq!(manifest.name, "test");
        assert_eq!(actions[0]["id"], "hello");
        assert_eq!(panels[0]["id"], "main");
        assert!(run.starts_with("ran:hello:"));
    }
}
