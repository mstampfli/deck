//! Machine-readable capability manifest.
//!
//! `deck capabilities` is the one machine-only surface: it enumerates every
//! command with its argv shape and output type so agents can discover the CLI
//! without parsing `--help`. Everything it lists is the same surface humans
//! use; agents add the global `--json` flag for structured output.

use anyhow::Result;
use serde::Serialize;

use crate::contracts::print_json;

#[derive(Serialize)]
struct Capability {
    argv: &'static [&'static str],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    options: &'static [&'static str],
    output: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    note: &'static str,
}

const fn capability(
    argv: &'static [&'static str],
    options: &'static [&'static str],
    output: &'static str,
) -> Capability {
    Capability {
        argv,
        options,
        output,
        note: "",
    }
}

#[rustfmt::skip]
const COMMANDS: &[(&str, Capability)] = &[
    ("scan", capability(&["deck", "scan", "[ROOT..]"], &[], "ScanJson")),
    ("list", capability(&["deck", "list"], &[], "ProjectListItem[]")),
    ("commands", capability(&["deck", "commands", "[PROJECT]"], &[], "ProjectCommands[]")),
    ("run", capability(&["deck", "run", "PROJECT", "COMMAND"], &["--dry-run"], "RunJson or CommandPlan")),
    ("start", capability(&["deck", "start", "PROJECT", "COMMAND"], &[], "ProcessActionJson")),
    ("stop", capability(&["deck", "stop", "PROJECT", "COMMAND"], &[], "ProcessActionJson")),
    ("restart", capability(&["deck", "restart", "PROJECT", "COMMAND"], &[], "ProcessActionJson")),
    ("ps", capability(&["deck", "ps", "[PROJECT]"], &[], "ProcessJson[]")),
    ("logs", capability(&["deck", "logs", "PROJECT", "COMMAND"], &[], "LogsJson")),
    ("git", capability(&["deck", "git", "PROJECT", "diff|branches|commits"], &[], "ToolOutputJson")),
    ("docker", capability(&["deck", "docker", "[PROJECT]"], &[], "ToolOutputJson")),
    ("gh", capability(&["deck", "gh", "PROJECT", "issues"], &[], "ToolOutputJson")),
    ("search", capability(&["deck", "search", "PROJECT", "QUERY"], &["--limit N"], "ToolOutputJson")),
    ("ssh_hosts", capability(&["deck", "ssh-hosts"], &[], "ToolOutputJson")),
    ("journal", capability(&["deck", "journal", "[UNIT]"], &["--lines N"], "ToolOutputJson")),
    ("workflow_list", capability(&["deck", "workflow", "list", "PROJECT"], &[], "ProjectWorkflows")),
    ("workflow_run", capability(&["deck", "workflow", "run", "PROJECT", "WORKFLOW"], &["--dry-run"], "WorkflowRunJson or WorkflowPlan")),
    ("plugin_add", capability(&["deck", "plugin", "add", "NAME", "--cmd", "COMMAND"], &[], "PluginRegistryJson")),
    ("plugin_add_path", capability(&["deck", "plugin", "add-path", "NAME", "PATH"], &[], "PluginRegistryJson")),
    ("plugin_remove", capability(&["deck", "plugin", "remove", "NAME"], &[], "PluginRegistryJson")),
    ("plugin_list", capability(&["deck", "plugin", "list", "[PROJECT]"], &[], "ProjectPlugins or PluginSpec[]")),
    ("plugin_manifest", capability(&["deck", "plugin", "manifest", "PROJECT", "NAME"], &[], "plugin manifest JSON (always JSON)")),
    ("plugin_run", capability(&["deck", "plugin", "run", "PROJECT", "NAME", "ACTION"], &[], "PluginRunJson")),
    ("context", capability(&["deck", "context", "PROJECT"], &["--output PATH"], "ContextBundle")),
    ("status", capability(&["deck", "status", "[PROJECT]"], &[], "ProjectStatus[]")),
    ("summary", Capability {
        argv: &["deck", "summary", "PROJECT"],
        options: &[],
        output: "SummaryJson",
        note: "highest-level startup bundle: context, command safety, sandbox profiles, tasks, suggested next commands",
    }),
    ("sandbox_plan", capability(&["deck", "sandbox", "plan", "PROJECT", "COMMAND"], &["--profile PROFILE", "--timeout-seconds SECONDS"], "SandboxPlanJson")),
    ("sandbox_run", capability(&["deck", "sandbox", "run", "PROJECT", "COMMAND"], &["--profile PROFILE", "--timeout-seconds SECONDS"], "SandboxRunJson")),
    ("sandbox_doctor", capability(&["deck", "sandbox", "doctor"], &[], "SandboxDoctorJson")),
    ("tasks_list", capability(&["deck", "tasks", "list", "PROJECT"], &[], "TaskListJson")),
    ("tasks_add", capability(&["deck", "tasks", "add", "PROJECT", "NAME"], &["--title TITLE", "--status todo|doing|done|blocked", "--notes NOTES", "--replace", "--dry-run"], "ConfigEditJson")),
    ("tasks_set", capability(&["deck", "tasks", "set", "PROJECT", "NAME"], &["--title TITLE", "--status todo|doing|done|blocked", "--notes NOTES", "--dry-run"], "ConfigEditJson")),
    ("tasks_remove", capability(&["deck", "tasks", "remove", "PROJECT", "NAME"], &["--dry-run"], "ConfigEditJson")),
    ("config_add_command", capability(&["deck", "config", "add-command", "PROJECT", "NAME", "--cmd", "COMMAND"], &["--kind once|server", "--port PORT", "--replace", "--dry-run"], "ConfigEditJson")),
    ("config_add_argv_command", capability(&["deck", "config", "add-argv-command", "PROJECT", "NAME", "--arg", "PROGRAM", "--arg", "ARG"], &["--arg VALUE repeated", "--kind once|server", "--port PORT", "--replace", "--dry-run"], "ConfigEditJson")),
    ("config_remove_command", capability(&["deck", "config", "remove-command", "PROJECT", "NAME"], &["--dry-run"], "ConfigEditJson")),
    ("config_add_workflow", capability(&["deck", "config", "add-workflow", "PROJECT", "NAME", "--step", "COMMAND"], &["--step COMMAND repeated", "--replace", "--dry-run"], "ConfigEditJson")),
    ("config_remove_workflow", capability(&["deck", "config", "remove-workflow", "PROJECT", "NAME"], &["--dry-run"], "ConfigEditJson")),
    ("config_add_plugin", capability(&["deck", "config", "add-plugin", "PROJECT", "NAME", "--cmd", "COMMAND"], &["--replace", "--dry-run"], "ConfigEditJson")),
    ("config_add_plugin_path", capability(&["deck", "config", "add-plugin-path", "PROJECT", "NAME", "PATH"], &["--replace", "--dry-run"], "ConfigEditJson")),
    ("config_remove_plugin", capability(&["deck", "config", "remove-plugin", "PROJECT", "NAME"], &["--dry-run"], "ConfigEditJson")),
    ("config_add_sandbox", capability(&["deck", "config", "add-sandbox", "PROJECT", "NAME"], &["--preset locked|test|dev", "--backend bwrap", "--network true|false", "--readonly-project true|false", "--writable PATH repeated", "--env NAME repeated", "--timeout-seconds SECONDS", "--allow-shell true|false", "--replace", "--dry-run"], "ConfigEditJson")),
    ("config_remove_sandbox", capability(&["deck", "config", "remove-sandbox", "PROJECT", "NAME"], &["--dry-run"], "ConfigEditJson")),
    ("recent", capability(&["deck", "recent", "[PROJECT]"], &["--limit N"], "RecentJson")),
    ("rerun", capability(&["deck", "rerun", "[PROJECT]", "[COMMAND]"], &["--dry-run"], "RunJson or CommandPlan")),
    ("init", capability(&["deck", "init"], &[], "InitJson")),
    ("clear_runs", capability(&["deck", "clear-runs"], &[], "ClearRunsJson")),
    ("capabilities", capability(&["deck", "capabilities"], &[], "CapabilityManifest")),
];

pub fn capabilities() -> Result<()> {
    let commands = COMMANDS
        .iter()
        .map(|(name, capability)| Ok((name.to_string(), serde_json::to_value(capability)?)))
        .collect::<Result<serde_json::Map<_, _>>>()?;
    print_json(&serde_json::json!({
        "name": "deck",
        "version": env!("CARGO_PKG_VERSION"),
        "json": {
            "flag": "--json",
            "scope": "global: every command accepts it and prints structured JSON",
            "errors": "failures use the JsonError envelope and a nonzero exit code",
            "failed_runs": "run/workflow/sandbox failures print one result document with ok=false and exit nonzero"
        },
        "commands": commands,
        "json_error": {
            "output": "JsonError",
            "shape": {
                "ok": false,
                "error": {
                    "kind": "string",
                    "message": "string"
                }
            }
        }
    }))
}
