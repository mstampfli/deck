//! Explicit Bubblewrap sandbox planning, execution, and diagnostics.
//!
//! Sandboxing is opt-in through `deck sandbox`. This module validates sandbox
//! policy, builds Bubblewrap argv, runs commands with optional timeouts, provides
//! presets, and diagnoses parent-environment namespace restrictions.

use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::Serialize;

use crate::config::{SandboxBackend, SandboxConfig, SandboxPreset, load_deck_config};
use crate::contracts::{Render, emit, project_ref};
use crate::errors::Reported;
use crate::model::{CommandSpec, Project};
use crate::selection::{load_projects, select_command, select_project};

const WORKSPACE: &str = "/workspace";
const DEFAULT_WRITABLE: &[&str] = &["./target", "./tmp"];

#[derive(Debug, Subcommand)]
pub enum SandboxCommand {
    Plan {
        project: String,
        command: String,
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        timeout_seconds: Option<u64>,
    },
    Run {
        project: String,
        command: String,
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        timeout_seconds: Option<u64>,
    },
    Doctor,
}

#[derive(Debug, Clone)]
struct SandboxPlan {
    json: SandboxPlanJson,
    process_argv: Vec<String>,
    timeout: Option<Duration>,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxPlanJson {
    ok: bool,
    project: SandboxProject,
    command: SandboxCommandJson,
    profile: String,
    backend: SandboxBackendJson,
    network: bool,
    readonly_project: bool,
    writable: Vec<PathBuf>,
    env: Vec<String>,
    timeout_seconds: Option<u64>,
    allow_shell: bool,
    argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxRunJson {
    ok: bool,
    project: SandboxProject,
    command: String,
    profile: String,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
    diagnosis: Option<SandboxFailureDiagnosis>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxDoctorJson {
    ok: bool,
    bwrap: DoctorCheck,
    filesystem_sandbox: DoctorCheck,
    network_sandbox: DoctorCheck,
    parent: ParentSandboxJson,
    recommendation: String,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorCheck {
    ok: bool,
    detail: String,
}

#[derive(Debug, Clone, Serialize)]
struct ParentSandboxJson {
    no_new_privs: Option<bool>,
    seccomp_mode: Option<u32>,
    effective_capabilities: Option<String>,
    bounded_capabilities: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxFailureDiagnosis {
    kind: &'static str,
    message: String,
    recommendation: String,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxProject {
    id: String,
    name: String,
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxCommandJson {
    name: String,
    shell: String,
    argv: Option<Vec<String>>,
    cwd: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum SandboxBackendJson {
    Bwrap,
}

pub fn run(action: SandboxCommand, json: bool) -> Result<()> {
    match action {
        SandboxCommand::Plan {
            project,
            command,
            profile,
            timeout_seconds,
        } => plan_command(&project, &command, &profile, timeout_seconds, json),
        SandboxCommand::Run {
            project,
            command,
            profile,
            timeout_seconds,
        } => run_command(&project, &command, &profile, timeout_seconds, json),
        SandboxCommand::Doctor => doctor(json),
    }
}

fn plan_command(
    project_query: &str,
    command_query: &str,
    profile: &str,
    timeout_seconds: Option<u64>,
    json: bool,
) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let command = select_command(project, command_query)?;
    let plan = build_plan(project, command, profile, timeout_seconds)?;
    emit(&plan.json, json)
}

fn run_command(
    project_query: &str,
    command_query: &str,
    profile: &str,
    timeout_seconds: Option<u64>,
    json: bool,
) -> Result<()> {
    let (projects, _, _) = load_projects(&[])?;
    let project = select_project(&projects, project_query)?;
    let command = select_command(project, command_query)?;
    let plan = build_plan(project, command, profile, timeout_seconds)?;
    ensure_bwrap_available()?;
    prepare_writable_paths(project, &plan.json.writable)?;
    if json {
        let output = run_bwrap_capture(&plan.process_argv, plan.timeout)?;
        let report = SandboxRunJson {
            ok: output.output.status.success() && !output.timed_out,
            project: plan.json.project,
            command: command.name.clone(),
            profile: profile.to_string(),
            exit_code: output.output.status.code(),
            timed_out: output.timed_out,
            stdout: String::from_utf8_lossy(&output.output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.output.stderr).into_owned(),
            diagnosis: diagnose_sandbox_failure(
                output.timed_out,
                output.output.status.code(),
                &String::from_utf8_lossy(&output.output.stderr),
            ),
        };
        emit(&report, true)?;
        if !report.ok {
            return Err(Reported.into());
        }
        return Ok(());
    }
    let (status, timed_out) = run_bwrap_status(&plan.process_argv, plan.timeout)?;
    if timed_out {
        anyhow::bail!("sandboxed command timed out");
    }
    if !status.success() {
        if let Some(diagnosis) = diagnose_sandbox_failure(false, status.code(), "") {
            anyhow::bail!("{}", diagnosis.message);
        }
        anyhow::bail!("sandboxed command exited with status {status}");
    }
    Ok(())
}

pub fn preset_profile(preset: SandboxPreset) -> SandboxConfig {
    match preset {
        SandboxPreset::Locked => SandboxConfig {
            backend: SandboxBackend::Bwrap,
            network: false,
            readonly_project: true,
            writable: DEFAULT_WRITABLE.iter().map(PathBuf::from).collect(),
            env: vec!["PATH".to_string()],
            timeout_seconds: Some(300),
            allow_shell: false,
        },
        SandboxPreset::Test => SandboxConfig {
            backend: SandboxBackend::Bwrap,
            network: false,
            readonly_project: true,
            writable: DEFAULT_WRITABLE.iter().map(PathBuf::from).collect(),
            env: vec!["PATH".to_string()],
            timeout_seconds: Some(300),
            allow_shell: true,
        },
        SandboxPreset::Dev => SandboxConfig {
            backend: SandboxBackend::Bwrap,
            network: true,
            readonly_project: false,
            writable: Vec::new(),
            env: vec!["PATH".to_string(), "HOME".to_string()],
            timeout_seconds: None,
            allow_shell: true,
        },
    }
}

fn build_plan(
    project: &Project,
    command: &CommandSpec,
    profile_name: &str,
    timeout_override: Option<u64>,
) -> Result<SandboxPlan> {
    if !command.available {
        anyhow::bail!(
            "{} is unavailable: {}",
            command.name,
            command
                .unavailable_reason
                .as_deref()
                .unwrap_or("missing required tool")
        );
    }
    let mut profile = load_profile(project, profile_name)?;
    if let Some(timeout_seconds) = timeout_override {
        profile.timeout_seconds = Some(timeout_seconds);
    }
    validate_profile(&profile)?;
    let writable = validated_writable_paths(&profile)?;
    let bwrap = bwrap_argv(project, command, &profile, &writable)?;
    let timeout = profile.timeout_seconds.map(Duration::from_secs);
    Ok(SandboxPlan {
        json: SandboxPlanJson {
            ok: true,
            project: SandboxProject {
                id: project_ref(project).id.to_string(),
                name: project_ref(project).name.to_string(),
                root: project.root.clone(),
            },
            command: SandboxCommandJson {
                name: command.name.clone(),
                shell: command.command.clone(),
                argv: command.argv.clone(),
                cwd: command.cwd.clone(),
            },
            profile: profile_name.to_string(),
            backend: match profile.backend {
                SandboxBackend::Bwrap => SandboxBackendJson::Bwrap,
            },
            network: profile.network,
            readonly_project: profile.readonly_project,
            writable,
            env: profile.env.clone(),
            timeout_seconds: profile.timeout_seconds,
            allow_shell: profile.allow_shell,
            argv: bwrap.display_argv,
        },
        process_argv: bwrap.process_argv,
        timeout,
    })
}

fn load_profile(project: &Project, profile_name: &str) -> Result<SandboxConfig> {
    let Some(config) = load_deck_config(&project.root)? else {
        return Ok(default_profile());
    };
    Ok(config
        .sandbox
        .get(profile_name)
        .cloned()
        .unwrap_or_else(default_profile))
}

fn default_profile() -> SandboxConfig {
    SandboxConfig {
        backend: SandboxBackend::Bwrap,
        network: false,
        readonly_project: true,
        writable: DEFAULT_WRITABLE.iter().map(PathBuf::from).collect(),
        env: vec!["PATH".to_string()],
        timeout_seconds: None,
        allow_shell: true,
    }
}

fn validate_profile(profile: &SandboxConfig) -> Result<()> {
    if let Some(timeout_seconds) = profile.timeout_seconds {
        validate_timeout_seconds(timeout_seconds)?;
    }
    for name in &profile.env {
        validate_env_name(name)?;
    }
    Ok(())
}

fn validated_writable_paths(profile: &SandboxConfig) -> Result<Vec<PathBuf>> {
    profile
        .writable
        .iter()
        .map(|path| validate_writable_path(path.as_path()))
        .collect()
}

pub fn validate_writable_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("sandbox writable path cannot be empty");
    }
    if path.is_absolute() {
        anyhow::bail!(
            "sandbox writable path must be project-relative: {}",
            path.display()
        );
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::bail!(
                    "sandbox writable path cannot escape project: {}",
                    path.display()
                );
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "sandbox writable path must be project-relative: {}",
                    path.display()
                );
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        anyhow::bail!("sandbox writable path cannot be empty");
    }
    Ok(normalized)
}

pub fn validate_env_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("invalid sandbox env name: empty");
    }
    let mut chars = name.chars();
    let first = chars.next().expect("checked non-empty");
    if !(first == '_' || first.is_ascii_alphabetic()) {
        anyhow::bail!("invalid sandbox env name: {name}");
    }
    if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        anyhow::bail!("invalid sandbox env name: {name}");
    }
    Ok(())
}

pub fn validate_timeout_seconds(timeout_seconds: u64) -> Result<()> {
    if timeout_seconds == 0 {
        anyhow::bail!("sandbox timeout_seconds must be greater than zero");
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct BwrapArgv {
    process_argv: Vec<String>,
    display_argv: Vec<String>,
}

fn bwrap_argv(
    project: &Project,
    command: &CommandSpec,
    profile: &SandboxConfig,
    writable: &[PathBuf],
) -> Result<BwrapArgv> {
    match profile.backend {
        SandboxBackend::Bwrap => {}
    }
    let mut args = BwrapArgBuilder::new();
    args.push("bwrap");
    args.extend([
        "--die-with-parent",
        "--unshare-pid",
        "--new-session",
        "--clearenv",
    ]);
    add_existing_ro_bind(&mut args, "/usr");
    add_existing_ro_bind(&mut args, "/bin");
    add_existing_ro_bind(&mut args, "/lib");
    add_existing_ro_bind(&mut args, "/lib64");
    add_existing_ro_bind(&mut args, "/etc");
    args.extend(["--proc", "/proc"]);
    args.extend(["--dev", "/dev"]);
    args.extend(["--tmpfs", "/tmp"]);
    args.extend(["--dir", "/tmp/home"]);
    args.extend(["--setenv", "HOME", "/tmp/home"]);
    add_allowed_env(&mut args, &profile.env)?;
    if !profile.network {
        args.push("--unshare-net");
    }
    if profile.readonly_project {
        args.push("--ro-bind");
        args.push(project.root.display().to_string());
        args.push(WORKSPACE);
    } else {
        args.push("--bind");
        args.push(project.root.display().to_string());
        args.push(WORKSPACE);
    }
    for relative in writable {
        let host = project.root.join(relative);
        let guest = Path::new(WORKSPACE).join(relative);
        args.extend([
            "--bind".to_string(),
            host.display().to_string(),
            guest.display().to_string(),
        ]);
    }
    args.push("--chdir");
    args.push(workspace_cwd(project, command)?.display().to_string());
    args.push("--");
    add_command_args(&mut args, command, profile)?;
    Ok(args.finish())
}

fn workspace_cwd(project: &Project, command: &CommandSpec) -> Result<PathBuf> {
    let cwd = command
        .cwd
        .strip_prefix(&project.root)
        .with_context(|| format!("command cwd {} is outside project", command.cwd.display()))?;
    Ok(Path::new(WORKSPACE).join(cwd))
}

fn add_existing_ro_bind(argv: &mut BwrapArgBuilder, path: &str) {
    if Path::new(path).exists() {
        argv.extend(["--ro-bind", path, path]);
    }
}

fn add_allowed_env(args: &mut BwrapArgBuilder, env_names: &[String]) -> Result<()> {
    for name in env_names {
        validate_env_name(name)?;
        if name == "HOME" {
            continue;
        }
        if let Ok(value) = std::env::var(name) {
            args.extend(["--setenv", name.as_str()]);
            args.push_redacted(value, "<redacted>");
        }
    }
    Ok(())
}

fn add_command_args(
    args: &mut BwrapArgBuilder,
    command: &CommandSpec,
    profile: &SandboxConfig,
) -> Result<()> {
    if let Some(argv) = command.argv.as_ref() {
        let (program, rest) = argv
            .split_first()
            .filter(|(program, _)| !program.is_empty())
            .with_context(|| format!("{} has empty argv", command.name))?;
        args.push(program);
        args.extend(rest.iter().cloned());
        return Ok(());
    }

    if !profile.allow_shell {
        anyhow::bail!("sandbox policy denied: shell commands are disabled by profile");
    }
    args.extend(["/bin/sh", "-lc", command.command.as_str()]);
    Ok(())
}

#[derive(Debug, Clone)]
struct BwrapArgBuilder {
    process_argv: Vec<String>,
    display_argv: Vec<String>,
}

impl BwrapArgBuilder {
    fn new() -> Self {
        Self {
            process_argv: Vec::new(),
            display_argv: Vec::new(),
        }
    }

    fn push(&mut self, value: impl Into<String>) {
        let value = value.into();
        self.process_argv.push(value.clone());
        self.display_argv.push(value);
    }

    fn push_redacted(
        &mut self,
        process_value: impl Into<String>,
        display_value: impl Into<String>,
    ) {
        self.process_argv.push(process_value.into());
        self.display_argv.push(display_value.into());
    }

    fn extend<I, S>(&mut self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for value in values {
            self.push(value);
        }
    }

    fn finish(self) -> BwrapArgv {
        BwrapArgv {
            process_argv: self.process_argv,
            display_argv: self.display_argv,
        }
    }
}

fn prepare_writable_paths(project: &Project, writable: &[PathBuf]) -> Result<()> {
    for relative in writable {
        let path = project.root.join(relative);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("creating writable sandbox path {}", path.display()))?;
    }
    Ok(())
}

#[derive(Debug)]
struct SandboxOutput {
    output: Output,
    timed_out: bool,
}

fn run_bwrap_capture(argv: &[String], timeout: Option<Duration>) -> Result<SandboxOutput> {
    let mut child = bwrap_command(argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "running bubblewrap sandbox")?;
    let timed_out = wait_for_child(&mut child, timeout)?;
    let output = child
        .wait_with_output()
        .with_context(|| "collecting sandbox output")?;
    Ok(SandboxOutput { output, timed_out })
}

fn run_bwrap_status(argv: &[String], timeout: Option<Duration>) -> Result<(ExitStatus, bool)> {
    let mut child = bwrap_command(argv)
        .spawn()
        .with_context(|| "running bubblewrap sandbox")?;
    let timed_out = wait_for_child(&mut child, timeout)?;
    let status = child.wait().with_context(|| "waiting for sandbox")?;
    Ok((status, timed_out))
}

fn doctor(json: bool) -> Result<()> {
    let report = doctor_report();
    emit(&report, json)?;
    if !report.ok {
        return Err(Reported.into());
    }
    Ok(())
}

pub fn doctor_report() -> SandboxDoctorJson {
    let bwrap = match Command::new("bwrap").arg("--version").output() {
        Ok(output) if output.status.success() => DoctorCheck {
            ok: true,
            detail: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        },
        Ok(output) => DoctorCheck {
            ok: false,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        Err(error) => DoctorCheck {
            ok: false,
            detail: format!("bubblewrap is not available: {error}"),
        },
    };
    let filesystem_sandbox = probe_bwrap(false);
    let network_sandbox = probe_bwrap(true);
    let parent = parent_sandbox();
    let recommendation = sandbox_recommendation(&bwrap, &filesystem_sandbox, &network_sandbox);
    SandboxDoctorJson {
        ok: bwrap.ok && filesystem_sandbox.ok && network_sandbox.ok,
        bwrap,
        filesystem_sandbox,
        network_sandbox,
        parent,
        recommendation,
    }
}

fn probe_bwrap(unshare_network: bool) -> DoctorCheck {
    if Command::new("bwrap").arg("--version").output().is_err() {
        return DoctorCheck {
            ok: false,
            detail: "bubblewrap is not installed".to_string(),
        };
    }

    let mut argv = vec!["bwrap".to_string()];
    for path in ["/usr", "/bin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            argv.extend(["--ro-bind".to_string(), path.to_string(), path.to_string()]);
        }
    }
    argv.extend([
        "--proc".to_string(),
        "/proc".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
        "--clearenv".to_string(),
        "--setenv".to_string(),
        "PATH".to_string(),
        "/usr/bin:/bin".to_string(),
    ]);
    if unshare_network {
        argv.push("--unshare-net".to_string());
    }
    argv.extend(["--".to_string(), "/bin/true".to_string()]);

    match bwrap_command(&argv).output() {
        Ok(output) if output.status.success() => DoctorCheck {
            ok: true,
            detail: "ok".to_string(),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let diagnosis = diagnose_sandbox_failure(false, output.status.code(), &stderr);
            DoctorCheck {
                ok: false,
                detail: diagnosis.map_or(stderr, |diagnosis| diagnosis.message),
            }
        }
        Err(error) => DoctorCheck {
            ok: false,
            detail: error.to_string(),
        },
    }
}

fn parent_sandbox() -> ParentSandboxJson {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    ParentSandboxJson {
        no_new_privs: status_value(&status, "NoNewPrivs").map(|value| value == "1"),
        seccomp_mode: status_value(&status, "Seccomp").and_then(|value| value.parse().ok()),
        effective_capabilities: status_value(&status, "CapEff").map(str::to_string),
        bounded_capabilities: status_value(&status, "CapBnd").map(str::to_string),
    }
}

fn status_value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status.lines().find_map(|line| {
        let (line_key, value) = line.split_once(':')?;
        (line_key == key).then(|| value.trim())
    })
}

fn sandbox_recommendation(
    bwrap: &DoctorCheck,
    filesystem_sandbox: &DoctorCheck,
    network_sandbox: &DoctorCheck,
) -> String {
    if !bwrap.ok {
        "install bubblewrap or choose another sandbox backend when Deck supports one".to_string()
    } else if !filesystem_sandbox.ok {
        "bubblewrap cannot create a basic filesystem sandbox in this environment".to_string()
    } else if !network_sandbox.ok {
        "filesystem sandboxing works, but network isolation is blocked by the parent environment; run Deck from a normal terminal or use a network=true profile only for this restricted session".to_string()
    } else {
        "bubblewrap filesystem and network isolation are available".to_string()
    }
}

fn format_check(check: &DoctorCheck) -> String {
    if check.ok {
        format!("ok ({})", check.detail)
    } else {
        format!("failed ({})", check.detail)
    }
}

pub fn diagnose_sandbox_failure(
    timed_out: bool,
    _exit_code: Option<i32>,
    stderr: &str,
) -> Option<SandboxFailureDiagnosis> {
    if timed_out {
        return Some(SandboxFailureDiagnosis {
            kind: "timeout",
            message: "sandboxed command timed out".to_string(),
            recommendation: "increase timeout_seconds or make the command terminate faster"
                .to_string(),
        });
    }
    if stderr.contains("NETLINK_ROUTE") || stderr.contains("unshare failed") {
        return Some(SandboxFailureDiagnosis {
            kind: "network_namespace_blocked",
            message: "network namespace creation is blocked by the parent environment".to_string(),
            recommendation: "run Deck from an unrestricted terminal, or use a network=true sandbox profile only when network isolation is not required".to_string(),
        });
    }
    if stderr.contains("No permissions to create new namespace")
        || stderr.contains("Operation not permitted")
    {
        return Some(SandboxFailureDiagnosis {
            kind: "namespace_blocked",
            message: "namespace creation is blocked by the parent environment".to_string(),
            recommendation:
                "run Deck from an unrestricted terminal or relax the parent sandbox policy"
                    .to_string(),
        });
    }
    None
}

fn bwrap_command(argv: &[String]) -> Command {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
}

fn wait_for_child(child: &mut Child, timeout: Option<Duration>) -> Result<bool> {
    let Some(timeout) = timeout else {
        return Ok(false);
    };

    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .with_context(|| "polling sandbox")?
            .is_some()
        {
            return Ok(false);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn ensure_bwrap_available() -> Result<()> {
    if Command::new("bwrap").arg("--version").output().is_ok() {
        Ok(())
    } else {
        anyhow::bail!("sandbox backend missing: bubblewrap is not installed")
    }
}

impl Render for SandboxPlanJson {
    fn human(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(
            out,
            "project: {} ({})",
            self.project.name,
            self.project.root.display()
        )?;
        writeln!(out, "command: {}", self.command.name)?;
        writeln!(out, "profile: {}", self.profile)?;
        writeln!(out, "backend: bwrap")?;
        writeln!(out, "network: {}", self.network)?;
        writeln!(out, "readonly_project: {}", self.readonly_project)?;
        writeln!(out, "allow_shell: {}", self.allow_shell)?;
        if let Some(timeout_seconds) = self.timeout_seconds {
            writeln!(out, "timeout_seconds: {timeout_seconds}")?;
        } else {
            writeln!(out, "timeout_seconds: none")?;
        }
        writeln!(out, "env:")?;
        for name in &self.env {
            writeln!(out, "  {name}")?;
        }
        writeln!(out, "writable:")?;
        for path in &self.writable {
            writeln!(out, "  {}", path.display())?;
        }
        writeln!(out, "argv:")?;
        for arg in &self.argv {
            writeln!(out, "  {arg}")?;
        }
        Ok(())
    }
}

impl Render for SandboxRunJson {
    fn human(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        write!(out, "{}", self.stdout)?;
        write!(out, "{}", self.stderr)?;
        if self.timed_out {
            writeln!(out, "sandboxed command timed out")?;
        } else if !self.ok {
            match self.exit_code {
                Some(code) => writeln!(out, "sandboxed command failed with exit code {code}")?,
                None => writeln!(out, "sandboxed command was terminated by a signal")?,
            }
        }
        if let Some(diagnosis) = &self.diagnosis {
            writeln!(out, "diagnosis: {}", diagnosis.message)?;
            writeln!(out, "recommendation: {}", diagnosis.recommendation)?;
        }
        Ok(())
    }
}

impl Render for SandboxDoctorJson {
    fn human(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(out, "bwrap: {}", format_check(&self.bwrap))?;
        writeln!(
            out,
            "filesystem sandbox: {}",
            format_check(&self.filesystem_sandbox)
        )?;
        writeln!(
            out,
            "network sandbox: {}",
            format_check(&self.network_sandbox)
        )?;
        writeln!(out, "parent no_new_privs: {:?}", self.parent.no_new_privs)?;
        writeln!(out, "parent seccomp_mode: {:?}", self.parent.seccomp_mode)?;
        writeln!(out, "recommendation: {}", self.recommendation)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CommandCategory, CommandKind, CommandSource, ProjectKind, ToolAvailability,
    };
    use std::collections::BTreeMap;

    fn project(root: PathBuf) -> Project {
        Project {
            id: "fixture".into(),
            name: "fixture".into(),
            root: root.clone(),
            kinds: vec![ProjectKind::Rust],
            commands: Vec::new(),
            workflows: Vec::new(),
            plugins: Vec::new(),
            git: None,
            tools: BTreeMap::<String, ToolAvailability>::new(),
            last_run: None,
            processes: Vec::new(),
        }
    }

    fn command(root: PathBuf) -> CommandSpec {
        CommandSpec {
            name: "test".into(),
            source: CommandSource::DeckToml,
            command: "cargo test".into(),
            argv: None,
            cwd: root,
            kind: CommandKind::Once,
            port: None,
            category: CommandCategory::Test,
            available: true,
            unavailable_reason: None,
        }
    }

    #[test]
    fn rejects_writable_paths_that_escape_project() {
        assert!(validate_writable_path(&PathBuf::from("../bad")).is_err());
        assert!(validate_writable_path(&PathBuf::from("/tmp")).is_err());
    }

    #[test]
    fn builds_bwrap_plan_for_project_command() {
        let temp = tempfile::tempdir().unwrap();
        let project = project(temp.path().to_path_buf());
        let command = command(temp.path().to_path_buf());

        let plan = build_plan(&project, &command, "default", None).unwrap();

        assert!(plan.json.argv.contains(&"--unshare-net".to_string()));
        assert!(plan.json.argv.contains(&"/workspace".to_string()));
        assert_eq!(
            plan.json.writable,
            vec![PathBuf::from("target"), PathBuf::from("tmp")]
        );
    }

    #[test]
    fn direct_argv_commands_do_not_use_shell() {
        let temp = tempfile::tempdir().unwrap();
        let project = project(temp.path().to_path_buf());
        let mut command = command(temp.path().to_path_buf());
        command.argv = Some(vec!["printf".into(), "hello".into()]);

        let plan = build_plan(&project, &command, "default", None).unwrap();

        let separator = plan
            .json
            .argv
            .iter()
            .position(|arg| arg == "--")
            .expect("bwrap separator");
        assert_eq!(
            &plan.json.argv[separator + 1..],
            [String::from("printf"), String::from("hello")]
        );
    }

    #[test]
    fn shell_commands_can_be_disabled_by_profile() {
        let temp = tempfile::tempdir().unwrap();
        let project = project(temp.path().to_path_buf());
        let command = command(temp.path().to_path_buf());
        let error = bwrap_argv(
            &project,
            &command,
            &SandboxConfig {
                backend: SandboxBackend::Bwrap,
                network: false,
                readonly_project: true,
                writable: Vec::new(),
                env: vec!["PATH".into()],
                timeout_seconds: None,
                allow_shell: false,
            },
            &[],
        )
        .unwrap_err();

        assert!(error.to_string().contains("shell commands are disabled"));
    }
}
