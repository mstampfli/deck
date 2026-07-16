//! User-local persisted runtime state.
//!
//! State stores scanned projects, run history, process records, and global
//! plugins under the XDG state directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{PluginSource, PluginSpec, ProcessRecord, Project, RunSummary};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectState>,
    #[serde(default)]
    pub runs: Vec<RunSummary>,
    #[serde(default)]
    pub processes: Vec<ProcessRecord>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectState {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginState {
    pub name: String,
    pub cmd: String,
}

#[derive(Debug, Clone)]
pub struct StatePaths {
    pub state_file: PathBuf,
    pub runs_dir: PathBuf,
}

impl State {
    pub fn load(paths: &StatePaths) -> Result<Self> {
        if !paths.state_file.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&paths.state_file)
            .with_context(|| format!("reading {}", paths.state_file.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", paths.state_file.display()))
    }

    pub fn save(&self, paths: &StatePaths) -> Result<()> {
        if let Some(parent) = paths.state_file.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("serializing deck state")?;
        fs::write(&paths.state_file, raw)
            .with_context(|| format!("writing {}", paths.state_file.display()))
    }

    /// Merge freshly scanned projects into the registry.
    ///
    /// Entries under any scanned root are replaced by what the scan found, so
    /// projects deleted from those roots are pruned, while entries outside
    /// the scanned roots are left alone: scanning one directory never erases
    /// the rest of the registry.
    pub fn update_projects(&mut self, projects: &[Project], scanned_roots: &[PathBuf]) {
        self.projects.retain(|_, existing| {
            !scanned_roots
                .iter()
                .any(|root| existing.root.starts_with(root))
        });
        for project in projects {
            self.projects.insert(
                project.id.clone(),
                ProjectState {
                    id: project.id.clone(),
                    name: project.name.clone(),
                    root: project.root.clone(),
                },
            );
        }
    }

    /// Record a run the moment it starts, so interrupted runs stay visible.
    pub fn begin_run(&mut self, run: RunSummary) {
        self.runs.push(run);
        if self.runs.len() > 200 {
            self.runs.drain(0..self.runs.len() - 200);
        }
    }

    /// Mark the run identified by its unique log path as completed.
    pub fn finalize_run(
        &mut self,
        log_path: &Path,
        exit_code: Option<i32>,
        finished_at: chrono::DateTime<chrono::Utc>,
        timed_out: bool,
    ) {
        if let Some(run) = self
            .runs
            .iter_mut()
            .rev()
            .find(|run| run.log_path == log_path)
        {
            run.exit_code = exit_code;
            run.finished_at = finished_at;
            run.finished = true;
            run.timed_out = timed_out;
        }
    }

    pub fn record_process(&mut self, process: ProcessRecord) {
        self.processes.push(process);
    }

    pub fn running_process_for(
        &self,
        project_id: &str,
        command_name: &str,
    ) -> Option<ProcessRecord> {
        self.processes
            .iter()
            .rev()
            .find(|process| {
                process.project_id == project_id
                    && process.command_name == command_name
                    && is_process_alive(process)
            })
            .cloned()
    }

    pub fn running_processes_for(&self, project_id: &str) -> Vec<ProcessRecord> {
        self.processes
            .iter()
            .filter(|process| process.project_id == project_id && is_process_alive(process))
            .cloned()
            .collect()
    }

    pub fn all_processes(&self) -> Vec<ProcessView> {
        self.processes
            .iter()
            .map(|process| ProcessView {
                process: process.clone(),
                alive: is_process_alive(process),
            })
            .collect()
    }

    pub fn mark_process_stopped(
        &mut self,
        project_id: &str,
        command_name: &str,
    ) -> Option<ProcessRecord> {
        let process = self.processes.iter_mut().rev().find(|process| {
            process.project_id == project_id
                && process.command_name == command_name
                && process.is_marked_running()
        })?;
        process.stopped_at = Some(chrono::Utc::now());
        Some(process.clone())
    }

    pub fn add_plugin(&mut self, name: String, cmd: String) {
        self.plugins.insert(name.clone(), PluginState { name, cmd });
    }

    pub fn remove_plugin(&mut self, name: &str) -> Option<PluginState> {
        self.plugins.remove(name)
    }

    pub fn global_plugins(&self) -> Vec<PluginSpec> {
        self.plugins
            .values()
            .map(|plugin| PluginSpec {
                name: plugin.name.clone(),
                cmd: plugin.cmd.clone(),
                source: PluginSource::Global,
            })
            .collect()
    }

    pub fn last_run_for(&self, project_id: &str) -> Option<RunSummary> {
        self.runs
            .iter()
            .rev()
            .find(|run| run.project_id == project_id)
            .cloned()
    }

    pub fn clear_runs(&mut self, paths: &StatePaths) -> Result<()> {
        self.runs.clear();
        self.processes.clear();
        if paths.runs_dir.exists() {
            fs::remove_dir_all(&paths.runs_dir)
                .with_context(|| format!("removing {}", paths.runs_dir.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ProcessView {
    pub process: ProcessRecord,
    pub alive: bool,
}

pub fn is_process_alive(process: &ProcessRecord) -> bool {
    process.is_marked_running() && pid_runs_command(process.pid, &process.command)
}

/// Whether `pid` is alive and still executing `command` (not a recycled pid).
pub fn pid_runs_command(pid: u32, command: &str) -> bool {
    let Some(command_line) = process_command_line(pid) else {
        return false;
    };
    command_matches_process_line(command, &command_line)
}

/// Whether an unfinished run's process is still alive.
pub fn is_run_alive(run: &RunSummary) -> bool {
    run.pid
        .is_some_and(|pid| pid_runs_command(pid, &run.command))
}

fn process_command_line(pid: u32) -> Option<String> {
    let raw = fs::read(Path::new("/proc").join(pid.to_string()).join("cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.into_iter()
            .map(|byte| if byte == 0 { b' ' } else { byte })
            .map(char::from)
            .collect::<String>(),
    )
}

fn command_matches_process_line(command: &str, process_line: &str) -> bool {
    if process_line.contains(command) {
        return true;
    }

    let tokens = command
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '\'' | '"' | ';' | ',' | '(' | ')' | '[' | ']' | '{' | '}'
                )
            })
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    !tokens.is_empty() && tokens.iter().all(|token| process_line.contains(token))
}

pub fn state_paths() -> Result<StatePaths> {
    let project_dirs = directories::ProjectDirs::from("", "", "deck")
        .context("could not resolve deck state directory")?;
    let state_dir = project_dirs
        .state_dir()
        .unwrap_or_else(|| project_dirs.data_local_dir());
    Ok(StatePaths {
        state_file: state_dir.join("state.toml"),
        runs_dir: state_dir.join("runs"),
    })
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn command_line_match_accepts_shell_and_exec_forms() {
        assert!(command_matches_process_line(
            "while true; do echo tick; sleep 1; done",
            "/bin/sh -c while true; do echo tick; sleep 1; done"
        ));
        assert!(command_matches_process_line("sleep 30", "sleep 30"));
    }

    #[test]
    fn command_line_match_rejects_reused_pid_commands() {
        assert!(!command_matches_process_line(
            "while true; do echo tick; sleep 1; done",
            "/usr/bin/python3 unrelated.py"
        ));
    }

    #[test]
    fn stopped_process_is_never_alive() {
        let process = ProcessRecord {
            project_id: "project".to_string(),
            project_name: "project".to_string(),
            command_name: "serve".to_string(),
            command: "sleep 30".to_string(),
            pid: std::process::id(),
            port: None,
            started_at: Utc::now(),
            stopped_at: Some(Utc::now()),
            log_path: PathBuf::from("unused"),
        };

        assert!(!is_process_alive(&process));
    }
}
