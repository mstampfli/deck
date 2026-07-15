//! Adapters that discover commands and tool state from project files and host tools.
//!
//! This module converts `deck.toml`, Cargo, npm, Make, just, git, and tool
//! availability into Deck's internal project model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::{ConfigCommandKind, DeckConfig};
use crate::model::{
    CommandCategory, CommandKind, CommandSource, CommandSpec, GitStatus, ProjectKind,
    ToolAvailability,
};

pub fn detect_kinds(root: &Path) -> Vec<ProjectKind> {
    let mut kinds = BTreeSet::new();
    if root.join("deck.toml").exists() {
        kinds.insert(ProjectKind::Deck);
    }
    if root.join(".git").exists() {
        kinds.insert(ProjectKind::Git);
    }
    if root.join("Cargo.toml").exists() {
        kinds.insert(ProjectKind::Rust);
    }
    if root.join("package.json").exists() {
        kinds.insert(ProjectKind::Node);
    }
    if root.join("go.mod").exists() {
        kinds.insert(ProjectKind::Go);
    }
    if root.join("Makefile").exists() {
        kinds.insert(ProjectKind::Make);
    }
    if root.join("justfile").exists() || root.join("Justfile").exists() {
        kinds.insert(ProjectKind::Just);
    }
    if root.join("docker-compose.yml").exists()
        || root.join("docker-compose.yaml").exists()
        || root.join("compose.yml").exists()
        || root.join("compose.yaml").exists()
    {
        kinds.insert(ProjectKind::Docker);
    }
    kinds.into_iter().collect()
}

pub fn collect_tools() -> BTreeMap<String, ToolAvailability> {
    [
        "git",
        "cargo",
        "npm",
        "node",
        "make",
        "just",
        "docker",
        "gh",
        "rg",
        "ssh",
        "journalctl",
        "tmux",
    ]
    .into_iter()
    .map(|tool| (tool.to_string(), detect_tool(tool)))
    .collect()
}

pub fn collect_commands(
    root: &Path,
    config: Option<&DeckConfig>,
    tools: &BTreeMap<String, ToolAvailability>,
) -> Result<Vec<CommandSpec>> {
    let mut commands = BTreeMap::<String, CommandSpec>::new();

    if root.join("Cargo.toml").exists() {
        for (name, command, category) in [
            ("check", "cargo check", CommandCategory::Check),
            ("test", "cargo test", CommandCategory::Test),
            ("run", "cargo run", CommandCategory::Run),
            ("fmt", "cargo fmt --all", CommandCategory::Format),
        ] {
            insert_if_absent(
                &mut commands,
                command_spec(
                    name,
                    CommandSource::Cargo,
                    command,
                    root,
                    category,
                    tool_is_available(tools, "cargo"),
                    "cargo is not available",
                ),
            );
        }
    }

    for command in npm_commands(root, tools)? {
        insert_if_absent(&mut commands, command);
    }
    for command in make_commands(root, tools)? {
        insert_if_absent(&mut commands, command);
    }
    for command in just_commands(root, tools)? {
        insert_if_absent(&mut commands, command);
    }

    if let Some(config) = config {
        for (name, command_config) in &config.commands {
            let command = command_config.command()?;
            commands.insert(
                name.clone(),
                CommandSpec {
                    name: name.clone(),
                    source: CommandSource::DeckToml,
                    command: command.clone(),
                    argv: command_config.argv().map(|argv| argv.to_vec()),
                    cwd: root.to_path_buf(),
                    kind: config_kind_to_model(command_config.kind()),
                    port: command_config.port(),
                    category: infer_category(name, &command),
                    available: true,
                    unavailable_reason: None,
                },
            );
        }
    }

    Ok(commands.into_values().collect())
}

pub fn git_status(root: &Path, tools: &BTreeMap<String, ToolAvailability>) -> Option<GitStatus> {
    if !root.join(".git").exists() || !tool_is_available(tools, "git") {
        return None;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("status")
        .arg("--short")
        .arg("--branch")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let branch_line = lines.next().unwrap_or_default();
    let changed = lines.count() as u32;
    let (branch, ahead, behind) = parse_branch_line(branch_line);

    Some(GitStatus {
        branch,
        ahead,
        behind,
        changed,
    })
}

fn npm_commands(
    root: &Path,
    tools: &BTreeMap<String, ToolAvailability>,
) -> Result<Vec<CommandSpec>> {
    let path = root.join("package.json");
    if !path.exists() {
        return Ok(Vec::new());
    }

    #[derive(Deserialize)]
    struct PackageJson {
        #[serde(default)]
        scripts: BTreeMap<String, String>,
    }

    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let package: PackageJson =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let available = tool_is_available(tools, "npm");

    Ok(package
        .scripts
        .into_iter()
        .map(|(name, script)| {
            let command = format!("npm run {name}");
            CommandSpec {
                name: format!("npm:{name}"),
                source: CommandSource::Npm,
                command,
                argv: None,
                cwd: root.to_path_buf(),
                kind: inferred_command_kind(&name, &script),
                port: None,
                category: infer_category(&name, &script),
                available,
                unavailable_reason: (!available).then(|| "npm is not available".to_string()),
            }
        })
        .collect())
}

fn make_commands(
    root: &Path,
    tools: &BTreeMap<String, ToolAvailability>,
) -> Result<Vec<CommandSpec>> {
    let path = root.join("Makefile");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let available = tool_is_available(tools, "make");

    Ok(parse_make_targets(&raw)
        .into_iter()
        .map(|target| {
            let command = format!("make {target}");
            CommandSpec {
                name: format!("make:{target}"),
                source: CommandSource::Make,
                command,
                argv: None,
                cwd: root.to_path_buf(),
                kind: inferred_command_kind(&target, ""),
                port: None,
                category: infer_category(&target, ""),
                available,
                unavailable_reason: (!available).then(|| "make is not available".to_string()),
            }
        })
        .collect())
}

fn just_commands(
    root: &Path,
    tools: &BTreeMap<String, ToolAvailability>,
) -> Result<Vec<CommandSpec>> {
    let path = if root.join("justfile").exists() {
        root.join("justfile")
    } else if root.join("Justfile").exists() {
        root.join("Justfile")
    } else {
        return Ok(Vec::new());
    };
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let available = tool_is_available(tools, "just");

    Ok(parse_just_targets(&raw)
        .into_iter()
        .map(|target| {
            let command = format!("just {target}");
            CommandSpec {
                name: format!("just:{target}"),
                source: CommandSource::Just,
                command,
                argv: None,
                cwd: root.to_path_buf(),
                kind: inferred_command_kind(&target, ""),
                port: None,
                category: infer_category(&target, ""),
                available,
                unavailable_reason: (!available).then(|| "just is not available".to_string()),
            }
        })
        .collect())
}

fn command_spec(
    name: &str,
    source: CommandSource,
    command: &str,
    cwd: &Path,
    category: CommandCategory,
    available: bool,
    unavailable_reason: &str,
) -> CommandSpec {
    CommandSpec {
        name: name.to_string(),
        source,
        command: command.to_string(),
        argv: None,
        cwd: cwd.to_path_buf(),
        kind: inferred_command_kind(name, command),
        port: None,
        category,
        available,
        unavailable_reason: (!available).then(|| unavailable_reason.to_string()),
    }
}

fn insert_if_absent(commands: &mut BTreeMap<String, CommandSpec>, command: CommandSpec) {
    commands.entry(command.name.clone()).or_insert(command);
}

fn detect_tool(tool: &str) -> ToolAvailability {
    for dir in preferred_tool_dirs() {
        let candidate = dir.join(tool);
        if candidate.is_file() {
            return ToolAvailability {
                available: true,
                path: Some(candidate),
                reason: None,
            };
        }
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(tool);
        if candidate.is_file() {
            return ToolAvailability {
                available: true,
                path: Some(candidate),
                reason: None,
            };
        }
    }
    ToolAvailability {
        available: false,
        path: None,
        reason: Some(format!("{tool} is not on PATH")),
    }
}

fn preferred_tool_dirs() -> Vec<std::path::PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| vec![dirs.home_dir().join(".cargo/bin")])
        .unwrap_or_default()
}

fn tool_is_available(tools: &BTreeMap<String, ToolAvailability>, tool: &str) -> bool {
    tools
        .get(tool)
        .is_some_and(|availability| availability.available)
}

fn parse_branch_line(line: &str) -> (String, u32, u32) {
    let line = line.trim_start_matches("## ").trim();
    let (name, tracking) = line.split_once("...").unwrap_or((line, ""));
    let branch = name.trim().to_string();
    let ahead = extract_count(tracking, "ahead ");
    let behind = extract_count(tracking, "behind ");
    (branch, ahead, behind)
}

fn extract_count(text: &str, marker: &str) -> u32 {
    text.split(marker)
        .nth(1)
        .and_then(|tail| {
            tail.chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn config_kind_to_model(kind: ConfigCommandKind) -> CommandKind {
    match kind {
        ConfigCommandKind::Once => CommandKind::Once,
        ConfigCommandKind::Server => CommandKind::Server,
    }
}

fn inferred_command_kind(name: &str, command: &str) -> CommandKind {
    let text = format!("{name} {command}").to_ascii_lowercase();
    if text.contains("serve") || text.contains("dev") || text.contains("watch") {
        CommandKind::Server
    } else {
        CommandKind::Once
    }
}

fn infer_category(name: &str, command: &str) -> CommandCategory {
    let text = format!("{name} {command}").to_ascii_lowercase();
    if text.contains("test") {
        CommandCategory::Test
    } else if text.contains("fmt") || text.contains("format") {
        CommandCategory::Format
    } else if text.contains("check") || text.contains("clippy") || text.contains("lint") {
        CommandCategory::Check
    } else if text.contains("build") {
        CommandCategory::Build
    } else if text.contains("serve") || text.contains("dev") || text.contains("watch") {
        CommandCategory::Dev
    } else if text.contains("run") || text.contains("start") {
        CommandCategory::Run
    } else {
        CommandCategory::Utility
    }
}

fn parse_make_targets(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim_end();
            if trimmed.starts_with('#') || trimmed.starts_with('.') || trimmed.starts_with('\t') {
                return None;
            }
            let (target, _) = trimmed.split_once(':')?;
            if target.contains('=') || target.contains(' ') || target.is_empty() {
                return None;
            }
            Some(target.to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_just_targets(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with('@')
                || trimmed.starts_with("set ")
                || trimmed.starts_with("export ")
            {
                return None;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                return None;
            }
            let head = trimmed.split(':').next()?;
            let name = head.split_whitespace().next()?;
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_make_targets_conservatively() {
        let raw = "test:\n\tcargo test\nVAR := no\n.PHONY: test\nbuild: src\n";
        assert_eq!(parse_make_targets(raw), vec!["build", "test"]);
    }

    #[test]
    fn parses_just_targets_conservatively() {
        let raw = "default:\n    just --list\ncheck:\n    cargo test\nexport FOO := \"bar\"\n";
        assert_eq!(parse_just_targets(raw), vec!["check", "default"]);
    }
}
