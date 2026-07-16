//! Process execution, output streaming, server startup, and run logs.
//!
//! This module owns shell-vs-argv process construction, run log creation, output
//! capture, and long-running server process records.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::model::{CommandKind, CommandSpec, ProcessRecord, Project, RunResult, RunSummary};
use crate::state::State;
use crate::state::{StatePaths, ensure_dir};

/// Process group of the currently running one-shot command, for the signal
/// forwarder. Zero when nothing is running.
static ONESHOT_PGID: AtomicI32 = AtomicI32::new(0);

/// Forward a fatal signal to the running command's process group, then
/// re-raise it with the default action so Deck dies with the right status.
/// Everything called here is async-signal-safe.
#[cfg(unix)]
extern "C" fn forward_signal(signal: libc::c_int) {
    let pgid = ONESHOT_PGID.load(Ordering::SeqCst);
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

#[cfg(unix)]
fn install_signal_forwarding() {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    let handler = forward_signal as *const () as libc::sighandler_t;
    INSTALL.call_once(|| unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    });
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

pub fn run_command(
    project: &Project,
    command: &CommandSpec,
    state: &mut State,
    paths: &StatePaths,
) -> Result<RunResult> {
    run_command_stream(project, command, state, paths, None, |_| Ok(()))
}

pub fn run_command_stream<F>(
    project: &Project,
    command: &CommandSpec,
    state: &mut State,
    paths: &StatePaths,
    timeout: Option<Duration>,
    mut on_output: F,
) -> Result<RunResult>
where
    F: FnMut(&str) -> Result<()>,
{
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

    ensure_dir(&paths.runs_dir)?;
    let started_at = Utc::now();
    let run_id = format!(
        "{}-{}",
        started_at.format("%Y%m%dT%H%M%S%.3fZ"),
        sanitize_file_name(&command.name)
    );
    let log_path = paths.runs_dir.join(format!("{run_id}.log"));
    let mut log =
        File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;

    writeln!(log, "$ {}", command.command)?;
    writeln!(log, "cwd: {}", command.cwd.display())?;
    writeln!(log)?;

    let mut process = command_process(command, false)?;
    process
        .current_dir(&command.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Run the command in its own process group so a timeout or a fatal
    // signal to Deck takes the whole command tree down with it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
        install_signal_forwarding();
    }
    let mut child = process
        .spawn()
        .with_context(|| format!("spawning {}", command.command))?;
    ONESHOT_PGID.store(child.id() as i32, Ordering::SeqCst);

    state.begin_run(RunSummary {
        project_id: project.id.clone(),
        command_name: command.name.clone(),
        command: command.command.clone(),
        started_at,
        finished_at: started_at,
        exit_code: None,
        log_path: log_path.clone(),
        pid: Some(child.id()),
        finished: false,
        timed_out: false,
    });
    state.save(paths)?;

    let stdout = child.stdout.take().context("capturing stdout")?;
    let stderr = child.stderr.take().context("capturing stderr")?;
    let (tx, rx) = mpsc::channel::<String>();
    spawn_reader(stdout, tx.clone());
    spawn_reader(stderr, tx);

    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut timed_out = false;
    let mut output = String::new();
    loop {
        let line = match deadline {
            None => rx.recv().ok(),
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() && !timed_out {
                    timed_out = true;
                    kill_command(&mut child);
                    continue;
                }
                match rx.recv_timeout(if timed_out {
                    Duration::from_secs(5)
                } else {
                    remaining
                }) {
                    Ok(line) => Some(line),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if timed_out {
                            break;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                }
            }
        };
        let Some(line) = line else { break };
        on_output(&line)?;
        output.push_str(&line);
        log.write_all(line.as_bytes())?;
    }

    let status = child.wait().context("waiting for command")?;
    ONESHOT_PGID.store(0, Ordering::SeqCst);
    let finished_at = Utc::now();
    writeln!(log)?;
    if timed_out {
        writeln!(log, "timed out")?;
    }
    writeln!(log, "exit: {:?}", status.code())?;

    state.finalize_run(&log_path, status.code(), finished_at, timed_out);
    state.save(paths)?;

    Ok(RunResult {
        summary: RunSummary {
            project_id: project.id.clone(),
            command_name: command.name.clone(),
            command: command.command.clone(),
            started_at,
            finished_at,
            exit_code: status.code(),
            log_path,
            pid: Some(child.id()),
            finished: true,
            timed_out,
        },
        output,
    })
}

fn kill_command(child: &mut std::process::Child) {
    #[cfg(unix)]
    kill_group(child.id());
    #[cfg(not(unix))]
    let _ = child.kill();
}

pub fn start_process(
    project: &Project,
    command: &CommandSpec,
    state: &State,
    paths: &StatePaths,
) -> Result<ProcessRecord> {
    if command.kind != CommandKind::Server {
        anyhow::bail!("{} is not a server command", command.name);
    }
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
    if let Some(process) = state.running_process_for(&project.id, &command.name) {
        anyhow::bail!(
            "{} is already running for {} as pid {}",
            command.name,
            project.name,
            process.pid
        );
    }

    ensure_dir(&paths.runs_dir)?;
    let started_at = Utc::now();
    let run_id = format!(
        "{}-{}-server",
        started_at.format("%Y%m%dT%H%M%S%.3fZ"),
        sanitize_file_name(&command.name)
    );
    let log_path = paths.runs_dir.join(format!("{run_id}.log"));
    let mut log =
        File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;
    writeln!(log, "$ {}", command.command)?;
    writeln!(log, "cwd: {}", command.cwd.display())?;
    writeln!(log, "kind: {}", command.kind.label())?;
    if let Some(port) = command.port {
        writeln!(log, "port: {port}")?;
    }
    writeln!(log)?;
    drop(log);

    let stdout = OpenOptions::new()
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("cloning {}", log_path.display()))?;
    let child = command_process(command, true)?
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("spawning {}", command.command))?;

    Ok(ProcessRecord {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        command_name: command.name.clone(),
        command: command.command.clone(),
        pid: child.id(),
        port: command.port,
        started_at,
        stopped_at: None,
        log_path,
    })
}

pub fn stop_process(process: &ProcessRecord) -> Result<()> {
    if !crate::state::is_process_alive(process) {
        return Ok(());
    }

    let status = Command::new("kill")
        .arg(process.pid.to_string())
        .status()
        .with_context(|| format!("stopping pid {}", process.pid))?;
    if !status.success() {
        anyhow::bail!("kill exited with status {status}");
    }
    Ok(())
}

fn command_process(command: &CommandSpec, new_session: bool) -> Result<Command> {
    let mut process = if new_session {
        let mut process = Command::new("setsid");
        if let Some(argv) = command.argv.as_ref() {
            let (program, args) = argv
                .split_first()
                .filter(|(program, _)| !program.is_empty())
                .with_context(|| format!("{} has empty argv", command.name))?;
            process.arg(program).args(args);
        } else {
            process
                .arg(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
                .arg("-c")
                .arg(&command.command);
        }
        process
    } else if let Some(argv) = command.argv.as_ref() {
        let (program, args) = argv
            .split_first()
            .filter(|(program, _)| !program.is_empty())
            .with_context(|| format!("{} has empty argv", command.name))?;
        let mut process = Command::new(program);
        process.args(args);
        process
    } else {
        let mut process =
            Command::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()));
        process.arg("-c").arg(&command.command);
        process
    };
    process.env("PATH", child_path());
    Ok(process)
}

fn spawn_reader<R>(reader: R, tx: mpsc::Sender<String>)
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            let Ok(mut line) = line else {
                break;
            };
            line.push('\n');
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

fn child_path() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let Some(base_dirs) = directories::BaseDirs::new() else {
        return current;
    };
    let cargo_bin = base_dirs.home_dir().join(".cargo/bin");
    if !cargo_bin.is_dir() {
        return current;
    }
    let cargo_bin = cargo_bin.to_string_lossy();
    if current.split(':').any(|entry| entry == cargo_bin) {
        current
    } else if current.is_empty() {
        cargo_bin.into_owned()
    } else {
        format!("{cargo_bin}:{current}")
    }
}

fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommandCategory, CommandSource};
    use std::path::PathBuf;

    #[test]
    fn run_command_streams_and_writes_log() {
        let temp = tempfile::tempdir().unwrap();
        let project = Project {
            id: "fixture".to_string(),
            name: "fixture".to_string(),
            root: temp.path().to_path_buf(),
            kinds: Vec::new(),
            commands: Vec::new(),
            workflows: Vec::new(),
            plugins: Vec::new(),
            git: None,
            tools: Default::default(),
            last_run: None,
            processes: Vec::new(),
        };
        let command = CommandSpec {
            name: "echo".to_string(),
            source: CommandSource::DeckToml,
            command: "printf 'hello\\n'".to_string(),
            argv: None,
            cwd: temp.path().to_path_buf(),
            kind: CommandKind::Once,
            port: None,
            category: CommandCategory::Utility,
            available: true,
            unavailable_reason: None,
        };
        let paths = StatePaths {
            state_file: temp.path().join("state.toml"),
            runs_dir: temp.path().join("runs"),
        };
        let mut state = State::default();
        let mut streamed = String::new();

        let result = run_command_stream(&project, &command, &mut state, &paths, None, |line| {
            streamed.push_str(line);
            Ok(())
        })
        .unwrap();

        assert_eq!(streamed, "hello\n");
        assert_eq!(result.output, "hello\n");
        assert_eq!(result.summary.exit_code, Some(0));
        let log = std::fs::read_to_string(result.summary.log_path).unwrap();
        assert!(log.contains("$ printf"));
        assert!(log.contains("hello"));
    }

    #[test]
    fn start_process_records_pid_and_stop_terminates_it() {
        let temp = tempfile::tempdir().unwrap();
        let project = Project {
            id: "fixture".to_string(),
            name: "fixture".to_string(),
            root: temp.path().to_path_buf(),
            kinds: Vec::new(),
            commands: Vec::new(),
            workflows: Vec::new(),
            plugins: Vec::new(),
            git: None,
            tools: Default::default(),
            last_run: None,
            processes: Vec::new(),
        };
        let command = CommandSpec {
            name: "serve".to_string(),
            source: CommandSource::DeckToml,
            command: "sleep 30".to_string(),
            argv: None,
            cwd: temp.path().to_path_buf(),
            kind: CommandKind::Server,
            port: Some(3000),
            category: CommandCategory::Dev,
            available: true,
            unavailable_reason: None,
        };
        let paths = StatePaths {
            state_file: PathBuf::from("unused"),
            runs_dir: temp.path().join("runs"),
        };
        let state = State::default();

        let process = start_process(&project, &command, &state, &paths).unwrap();

        assert_eq!(process.command_name, "serve");
        assert_eq!(process.port, Some(3000));
        assert!(crate::state::is_process_alive(&process));
        stop_process(&process).unwrap();
    }
}
