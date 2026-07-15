//! CLI mutations for project-local `deck.toml`.
//!
//! `deck config` edits commands, workflows, plugins, and sandbox profiles
//! through the shared config lock and atomic write path, for humans and
//! agents alike.

use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;

use crate::config::{
    CommandConfig, ConfigCommandKind, DeckConfig, DetailedCommandConfig, PluginConfig,
    SandboxBackend, SandboxConfig, SandboxPreset, WorkflowConfig, deck_config_path,
    load_or_default_deck_config, lock_deck_config, write_deck_config,
};
use crate::contracts::{ConfigEditJson, emit, project_ref};
use crate::model::Project;
use crate::selection::{load_projects, select_command, select_project};

#[derive(Debug, clap::Subcommand)]
pub enum ConfigCommand {
    AddCommand {
        project: String,
        name: String,
        #[arg(long)]
        cmd: String,
        #[arg(long, default_value_t = CommandKindArg::Once)]
        kind: CommandKindArg,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
    },
    AddArgvCommand {
        project: String,
        name: String,
        #[arg(long = "arg", required = true)]
        argv: Vec<String>,
        #[arg(long, default_value_t = CommandKindArg::Once)]
        kind: CommandKindArg,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
    },
    RemoveCommand {
        project: String,
        name: String,
        #[arg(long)]
        dry_run: bool,
    },
    AddWorkflow {
        project: String,
        name: String,
        #[arg(long = "step", required = true)]
        steps: Vec<String>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
    },
    RemoveWorkflow {
        project: String,
        name: String,
        #[arg(long)]
        dry_run: bool,
    },
    AddPlugin {
        project: String,
        name: String,
        #[arg(long)]
        cmd: String,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
    },
    AddPluginPath {
        project: String,
        name: String,
        path: PathBuf,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
    },
    RemovePlugin {
        project: String,
        name: String,
        #[arg(long)]
        dry_run: bool,
    },
    AddSandbox {
        project: String,
        name: String,
        #[arg(long)]
        preset: Option<SandboxPresetArg>,
        #[arg(long, default_value_t = SandboxBackendArg::Bwrap)]
        backend: SandboxBackendArg,
        #[arg(
            long,
            action = clap::ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true"
        )]
        network: Option<bool>,
        #[arg(
            long,
            action = clap::ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true"
        )]
        readonly_project: Option<bool>,
        #[arg(long = "writable")]
        writable: Vec<PathBuf>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        timeout_seconds: Option<u64>,
        #[arg(
            long,
            action = clap::ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true"
        )]
        allow_shell: Option<bool>,
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        dry_run: bool,
    },
    RemoveSandbox {
        project: String,
        name: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum CommandKindArg {
    #[default]
    Once,
    Server,
}

impl std::fmt::Display for CommandKindArg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Once => "once",
            Self::Server => "server",
        })
    }
}

impl From<CommandKindArg> for ConfigCommandKind {
    fn from(kind: CommandKindArg) -> Self {
        match kind {
            CommandKindArg::Once => Self::Once,
            CommandKindArg::Server => Self::Server,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum SandboxBackendArg {
    #[default]
    Bwrap,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SandboxPresetArg {
    Locked,
    Test,
    Dev,
}

impl From<SandboxPresetArg> for SandboxPreset {
    fn from(preset: SandboxPresetArg) -> Self {
        match preset {
            SandboxPresetArg::Locked => Self::Locked,
            SandboxPresetArg::Test => Self::Test,
            SandboxPresetArg::Dev => Self::Dev,
        }
    }
}

impl std::fmt::Display for SandboxBackendArg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Bwrap => "bwrap",
        })
    }
}

impl From<SandboxBackendArg> for SandboxBackend {
    fn from(backend: SandboxBackendArg) -> Self {
        match backend {
            SandboxBackendArg::Bwrap => Self::Bwrap,
        }
    }
}

pub fn run(action: ConfigCommand, json: bool) -> Result<()> {
    match action {
        ConfigCommand::AddCommand {
            project,
            name,
            cmd,
            kind,
            port,
            replace,
            dry_run,
        } => edit_project_config(
            &project,
            "add-command",
            dry_run,
            json,
            |config, _project| {
                validate_config_key("command", &name)?;
                if port.is_some() && !matches!(kind, CommandKindArg::Server) {
                    anyhow::bail!("command port requires kind=server");
                }
                insert_config_entry(&mut config.commands, &name, replace, || {
                    command_config(cmd, kind, port)
                })
            },
        ),
        ConfigCommand::AddArgvCommand {
            project,
            name,
            argv,
            kind,
            port,
            replace,
            dry_run,
        } => edit_project_config(
            &project,
            "add-argv-command",
            dry_run,
            json,
            |config, _project| {
                validate_config_key("command", &name)?;
                validate_argv(&argv)?;
                if port.is_some() && !matches!(kind, CommandKindArg::Server) {
                    anyhow::bail!("command port requires kind=server");
                }
                insert_config_entry(&mut config.commands, &name, replace, || {
                    argv_command_config(argv, kind, port)
                })
            },
        ),
        ConfigCommand::RemoveCommand {
            project,
            name,
            dry_run,
        } => edit_project_config(
            &project,
            "remove-command",
            dry_run,
            json,
            |config, _project| remove_config_entry(&mut config.commands, "command", &name),
        ),
        ConfigCommand::AddWorkflow {
            project,
            name,
            steps,
            replace,
            dry_run,
        } => edit_project_config(
            &project,
            "add-workflow",
            dry_run,
            json,
            |config, project| {
                validate_config_key("workflow", &name)?;
                validate_workflow_steps(project, &steps)?;
                insert_config_entry(&mut config.workflows, &name, replace, || WorkflowConfig {
                    steps,
                })
            },
        ),
        ConfigCommand::RemoveWorkflow {
            project,
            name,
            dry_run,
        } => edit_project_config(
            &project,
            "remove-workflow",
            dry_run,
            json,
            |config, _project| remove_config_entry(&mut config.workflows, "workflow", &name),
        ),
        ConfigCommand::AddPlugin {
            project,
            name,
            cmd,
            replace,
            dry_run,
        } => edit_project_config(&project, "add-plugin", dry_run, json, |config, _project| {
            validate_config_key("plugin", &name)?;
            insert_config_entry(&mut config.plugins, &name, replace, || PluginConfig { cmd })
        }),
        ConfigCommand::AddPluginPath {
            project,
            name,
            path,
            replace,
            dry_run,
        } => {
            let cmd = crate::plugin::command_from_path(&path)?;
            edit_project_config(&project, "add-plugin", dry_run, json, |config, _project| {
                validate_config_key("plugin", &name)?;
                insert_config_entry(&mut config.plugins, &name, replace, || PluginConfig { cmd })
            })
        }
        ConfigCommand::RemovePlugin {
            project,
            name,
            dry_run,
        } => edit_project_config(
            &project,
            "remove-plugin",
            dry_run,
            json,
            |config, _project| remove_config_entry(&mut config.plugins, "plugin", &name),
        ),
        ConfigCommand::AddSandbox {
            project,
            name,
            preset,
            backend,
            network,
            readonly_project,
            writable,
            env,
            timeout_seconds,
            allow_shell,
            replace,
            dry_run,
        } => edit_project_config(
            &project,
            "add-sandbox",
            dry_run,
            json,
            |config, _project| {
                validate_config_key("sandbox", &name)?;
                for path in &writable {
                    crate::sandbox::validate_writable_path(path)?;
                }
                for name in &env {
                    crate::sandbox::validate_env_name(name)?;
                }
                if let Some(timeout_seconds) = timeout_seconds {
                    crate::sandbox::validate_timeout_seconds(timeout_seconds)?;
                }
                insert_config_entry(&mut config.sandbox, &name, replace, || {
                    sandbox_config(SandboxConfigInput {
                        preset,
                        backend,
                        network,
                        readonly_project,
                        writable,
                        env,
                        timeout_seconds,
                        allow_shell,
                    })
                })
            },
        ),
        ConfigCommand::RemoveSandbox {
            project,
            name,
            dry_run,
        } => edit_project_config(
            &project,
            "remove-sandbox",
            dry_run,
            json,
            |config, _project| remove_config_entry(&mut config.sandbox, "sandbox", &name),
        ),
    }
}

fn edit_project_config<F>(
    project_query: &str,
    action: &str,
    dry_run: bool,
    json: bool,
    mutate: F,
) -> Result<()>
where
    F: FnOnce(&mut DeckConfig, &Project) -> Result<bool>,
{
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let _lock = lock_deck_config(&project.root)?;
    let mut config = load_or_default_deck_config(&project.root, &project.name)?;
    let changed = mutate(&mut config, project)?;
    let path = deck_config_path(&project.root);
    if changed && !dry_run {
        write_deck_config(&project.root, &config)?;
    }
    emit(
        &ConfigEditJson {
            ok: true,
            project: project_ref(project),
            path,
            action,
            dry_run,
            changed,
            config,
        },
        json,
    )
}

fn command_config(cmd: String, kind: CommandKindArg, port: Option<u16>) -> CommandConfig {
    if matches!(kind, CommandKindArg::Once) && port.is_none() {
        CommandConfig::Simple(cmd)
    } else {
        CommandConfig::Detailed(DetailedCommandConfig {
            cmd: Some(cmd),
            argv: None,
            kind: Some(kind.into()),
            port,
        })
    }
}

fn argv_command_config(
    argv: Vec<String>,
    kind: CommandKindArg,
    port: Option<u16>,
) -> CommandConfig {
    CommandConfig::Detailed(DetailedCommandConfig {
        cmd: None,
        argv: Some(argv),
        kind: Some(kind.into()),
        port,
    })
}

struct SandboxConfigInput {
    preset: Option<SandboxPresetArg>,
    backend: SandboxBackendArg,
    network: Option<bool>,
    readonly_project: Option<bool>,
    writable: Vec<PathBuf>,
    env: Vec<String>,
    timeout_seconds: Option<u64>,
    allow_shell: Option<bool>,
}

fn sandbox_config(input: SandboxConfigInput) -> SandboxConfig {
    let mut config = input
        .preset
        .map(|preset| crate::sandbox::preset_profile(preset.into()))
        .unwrap_or_else(|| SandboxConfig {
            backend: input.backend.into(),
            network: false,
            readonly_project: true,
            writable: Vec::new(),
            env: Vec::new(),
            timeout_seconds: None,
            allow_shell: true,
        });
    config.backend = input.backend.into();
    if let Some(network) = input.network {
        config.network = network;
    }
    if let Some(readonly_project) = input.readonly_project {
        config.readonly_project = readonly_project;
    }
    if !input.writable.is_empty() {
        config.writable = input.writable;
    }
    if !input.env.is_empty() {
        config.env = input.env;
    }
    if input.timeout_seconds.is_some() {
        config.timeout_seconds = input.timeout_seconds;
    }
    if let Some(allow_shell) = input.allow_shell {
        config.allow_shell = allow_shell;
    }
    config
}

fn insert_config_entry<T, F>(
    entries: &mut std::collections::BTreeMap<String, T>,
    name: &str,
    replace: bool,
    value: F,
) -> Result<bool>
where
    F: FnOnce() -> T,
{
    if entries.contains_key(name) && !replace {
        anyhow::bail!("{name:?} already exists; pass --replace to overwrite it");
    }
    entries.insert(name.to_string(), value());
    Ok(true)
}

fn remove_config_entry<T>(
    entries: &mut std::collections::BTreeMap<String, T>,
    kind: &str,
    name: &str,
) -> Result<bool> {
    validate_config_key(kind, name)?;
    if entries.remove(name).is_none() {
        anyhow::bail!("no {kind} named {name:?}");
    }
    Ok(true)
}

fn validate_config_key(kind: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("{kind} name cannot be empty");
    }
    Ok(())
}

fn validate_workflow_steps(project: &Project, steps: &[String]) -> Result<()> {
    if steps.is_empty() {
        anyhow::bail!("workflow must contain at least one step");
    }
    for step in steps {
        validate_config_key("workflow step", step)?;
        select_command(project, step)?;
    }
    Ok(())
}

fn validate_argv(argv: &[String]) -> Result<()> {
    let Some(program) = argv.first() else {
        anyhow::bail!("argv command must contain at least one arg");
    };
    if program.trim().is_empty() {
        anyhow::bail!("argv command program cannot be empty");
    }
    Ok(())
}
