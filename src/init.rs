//! Project-aware `deck init`.
//!
//! Init writes a `deck.toml` seeded with what Deck already detects about the
//! project (Cargo, npm, Make, just commands), instead of a static example.
//! Flags configure the one-dimensional decisions in the same pass: a sandbox
//! profile from a preset, its shell policy, and which detected commands are
//! long-running servers. The result lands in one atomic write.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::adapters::{collect_commands, collect_tools};
use crate::config::{
    CommandConfig, ConfigCommandKind, DeckConfig, DetailedCommandConfig, WorkflowConfig,
    deck_config_path, write_deck_config,
};
use crate::config_edit::SandboxPresetArg;
use crate::contracts::{Render, emit};
use crate::model::CommandKind;

#[derive(Debug, Serialize)]
pub struct InitJson {
    ok: bool,
    path: std::path::PathBuf,
    name: String,
    commands: Vec<InitCommandJson>,
    workflows: Vec<String>,
    sandbox_profiles: Vec<String>,
    shell_commands_blocked_by_profile: usize,
}

#[derive(Debug, Serialize)]
struct InitCommandJson {
    name: String,
    command: String,
    kind: &'static str,
    port: Option<u16>,
}

pub fn init(
    sandbox: Option<SandboxPresetArg>,
    allow_shell: Option<bool>,
    servers: &[String],
    json: bool,
) -> Result<()> {
    let root = std::env::current_dir()?;
    let path = deck_config_path(&root);
    if path.exists() {
        anyhow::bail!("{} already exists", path.display());
    }
    if allow_shell.is_some() && sandbox.is_none() {
        anyhow::bail!("--allow-shell configures a sandbox profile; pass --sandbox as well");
    }

    let name = project_name(&root);
    let tools = collect_tools();
    let detected = collect_commands(&root, None, &tools)?;

    let mut config = DeckConfig {
        name: Some(name.clone()),
        ..DeckConfig::default()
    };
    for spec in &detected {
        let entry = if spec.kind == CommandKind::Server || spec.port.is_some() {
            CommandConfig::Detailed(DetailedCommandConfig {
                cmd: Some(spec.command.clone()),
                argv: None,
                kind: Some(ConfigCommandKind::Server),
                port: spec.port,
            })
        } else {
            CommandConfig::Simple(spec.command.clone())
        };
        config.commands.insert(spec.name.clone(), entry);
    }

    for server in servers {
        let (command_name, port) = parse_server(server)?;
        let entry = config.commands.get(command_name).with_context(|| {
            format!(
                "--server {command_name:?} does not match a detected command (detected: {})",
                config
                    .commands
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let command = entry.command()?;
        config.commands.insert(
            command_name.to_string(),
            CommandConfig::Detailed(DetailedCommandConfig {
                cmd: Some(command),
                argv: None,
                kind: Some(ConfigCommandKind::Server),
                port,
            }),
        );
    }

    if config.commands.contains_key("fmt") && config.commands.contains_key("test") {
        config.workflows.insert(
            "check".to_string(),
            WorkflowConfig {
                steps: vec!["fmt".to_string(), "test".to_string()],
            },
        );
    }

    if let Some(preset) = sandbox {
        let mut profile = crate::sandbox::preset_profile(preset.into());
        if let Some(allow_shell) = allow_shell {
            profile.allow_shell = allow_shell;
        }
        config.sandbox.insert("default".to_string(), profile);
    }

    let shell_denied = config.sandbox.values().any(|profile| !profile.allow_shell);
    let shell_blocked = if shell_denied {
        config
            .commands
            .values()
            .filter(|command| command.argv().is_none())
            .count()
    } else {
        0
    };

    let path = write_deck_config(&root, &config)?;
    emit(
        &InitJson {
            ok: true,
            path,
            name,
            commands: config
                .commands
                .iter()
                .map(|(name, command)| {
                    Ok(InitCommandJson {
                        name: name.clone(),
                        command: command.command()?,
                        kind: match command.kind() {
                            ConfigCommandKind::Once => "once",
                            ConfigCommandKind::Server => "server",
                        },
                        port: command.port(),
                    })
                })
                .collect::<Result<_>>()?,
            workflows: config.workflows.keys().cloned().collect(),
            sandbox_profiles: config.sandbox.keys().cloned().collect(),
            shell_commands_blocked_by_profile: shell_blocked,
        },
        json,
    )
}

fn project_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_string()
}

/// Parse a `--server NAME[:PORT]` value.
///
/// Detected command names can themselves contain colons (`npm:dev`), so only
/// a digits-only suffix after the last colon counts as the port.
fn parse_server(value: &str) -> Result<(&str, Option<u16>)> {
    match value.rsplit_once(':') {
        Some((name, port))
            if !name.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            let port = port
                .parse::<u16>()
                .with_context(|| format!("invalid port in --server {value:?}"))?;
            Ok((name, Some(port)))
        }
        _ => Ok((value, None)),
    }
}

impl Render for InitJson {
    fn human(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(out, "wrote {}", self.path.display())?;
        writeln!(out, "name: {}", self.name)?;
        if self.commands.is_empty() {
            writeln!(
                out,
                "commands: none detected; add with `deck config add-command`"
            )?;
        } else {
            writeln!(out, "commands:")?;
            for command in &self.commands {
                let port = command
                    .port
                    .map(|port| format!(" :{port}"))
                    .unwrap_or_default();
                writeln!(
                    out,
                    "  {:<18} {:<7}{} {}",
                    command.name, command.kind, port, command.command
                )?;
            }
        }
        for workflow in &self.workflows {
            writeln!(out, "workflow {workflow}: fmt -> test")?;
        }
        for profile in &self.sandbox_profiles {
            writeln!(out, "sandbox profile: {profile}")?;
        }
        if self.shell_commands_blocked_by_profile > 0 {
            writeln!(
                out,
                "note: {} shell-backed commands cannot run under the profile's \
                 allow_shell=false; convert them with `deck config add-argv-command --replace`",
                self.shell_commands_blocked_by_profile
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_values() {
        assert_eq!(parse_server("web").unwrap(), ("web", None));
        assert_eq!(parse_server("web:3000").unwrap(), ("web", Some(3000)));
        assert_eq!(parse_server("npm:dev").unwrap(), ("npm:dev", None));
        assert_eq!(
            parse_server("npm:dev:5173").unwrap(),
            ("npm:dev", Some(5173))
        );
        assert!(parse_server("web:99999").is_err());
    }
}
