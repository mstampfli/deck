//! Agent session startup bundles.
//!
//! A session combines project context, command safety metadata, sandbox profile
//! summaries, tasks, and suggested next Deck commands into one JSON response.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::{SandboxConfig, load_deck_config};
use crate::context::ContextBundle;
use crate::contracts::{ProjectRef, print_json, project_ref};
use crate::safety::{CommandSafety, command_safety};
use crate::selection::{load_projects, select_project};

#[derive(Debug, Serialize)]
pub struct AgentSessionJson<'a> {
    ok: bool,
    started_at: DateTime<Utc>,
    project: ProjectRef<'a>,
    context: ContextBundle,
    commands: Vec<AgentSessionCommandSafety>,
    sandbox_profiles: Vec<AgentSessionSandboxProfile>,
    suggested_next_commands: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct AgentSessionCommandSafety {
    name: String,
    safety: CommandSafety,
}

#[derive(Debug, Serialize)]
struct AgentSessionSandboxProfile {
    name: String,
    config: SandboxConfig,
}

pub fn start(project_query: &str) -> Result<()> {
    let (projects, state, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let context = crate::context::build_context(project, &state)?;
    let commands = project
        .commands
        .iter()
        .map(|command| AgentSessionCommandSafety {
            name: command.name.clone(),
            safety: command_safety(command),
        })
        .collect::<Vec<_>>();
    let sandbox_profiles = load_deck_config(&project.root)?
        .map(|config| {
            config
                .sandbox
                .into_iter()
                .map(|(name, config)| AgentSessionSandboxProfile { name, config })
                .collect()
        })
        .unwrap_or_default();
    print_json(&AgentSessionJson {
        ok: true,
        started_at: Utc::now(),
        project: project_ref(project),
        context,
        commands,
        sandbox_profiles,
        suggested_next_commands: vec![
            vec![
                "deck".into(),
                "commands".into(),
                project.name.clone(),
                "--json".into(),
            ],
            vec![
                "deck".into(),
                "tasks".into(),
                "list".into(),
                project.name.clone(),
                "--json".into(),
            ],
            vec![
                "deck".into(),
                "sandbox".into(),
                "doctor".into(),
                "--json".into(),
            ],
        ],
    })
}
