//! Terminal UI entry point and keyboard navigation.
//!
//! The TUI renders discovered projects, command/status panes, processes, and
//! logs using the same project model as the CLI.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::model::{CommandKind, Project, RunSummary};
use crate::process::{run_command, start_process, stop_process};
use crate::selection::load_projects;
use crate::state::State;

pub fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_app(&mut terminal);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    let mut app = App::load()?;
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Down | KeyCode::Char('j') => app.next_project(),
                KeyCode::Up | KeyCode::Char('k') => app.previous_project(),
                KeyCode::Right | KeyCode::Char('l') => app.next_command(),
                KeyCode::Left | KeyCode::Char('h') => app.previous_command(),
                KeyCode::Char('r') => app.reload()?,
                KeyCode::Char('g') => app.reload()?,
                KeyCode::Enter => app.run_selected()?,
                KeyCode::Char('w') => app.run_first_workflow()?,
                _ => {}
            }
        }
    }
    Ok(())
}

struct App {
    projects: Vec<Project>,
    state: State,
    paths: crate::state::StatePaths,
    project_state: ListState,
    command_state: ListState,
    output: String,
}

impl App {
    fn load() -> Result<Self> {
        let (projects, state, paths) = load_projects(&[])?;
        let mut project_state = ListState::default();
        if !projects.is_empty() {
            project_state.select(Some(0));
        }
        let mut command_state = ListState::default();
        if projects
            .first()
            .is_some_and(|project| !project.commands.is_empty())
        {
            command_state.select(Some(0));
        }
        Ok(Self {
            projects,
            state,
            paths,
            project_state,
            command_state,
            output: "Enter runs/toggles commands. w runs first workflow. r refreshes. q quits."
                .to_string(),
        })
    }

    fn reload(&mut self) -> Result<()> {
        let selected = self.project_state.selected().unwrap_or(0);
        let (projects, state, paths) = load_projects(&[])?;
        self.projects = projects;
        self.state = state;
        self.paths = paths;
        if self.projects.is_empty() {
            self.project_state.select(None);
            self.command_state.select(None);
        } else {
            self.project_state
                .select(Some(selected.min(self.projects.len() - 1)));
            self.sync_command_selection();
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(28),
                Constraint::Percentage(32),
                Constraint::Percentage(40),
            ])
            .split(frame.area());

        let project_items = self
            .projects
            .iter()
            .map(|project| {
                let git = project
                    .git
                    .as_ref()
                    .map(|git| format!(" {} +{}", git.branch, git.changed))
                    .unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(&project.name, Style::default().fg(Color::Cyan)),
                    Span::raw(git),
                ]))
            })
            .collect::<Vec<_>>();
        let projects = List::new(project_items)
            .block(Block::default().title("Projects").borders(Borders::ALL))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(projects, chunks[0], &mut self.project_state);

        let command_items = self
            .selected_project()
            .map(|project| {
                project
                    .commands
                    .iter()
                    .map(|command| {
                        let marker = if command.available { " " } else { "!" };
                        let running = project
                            .processes
                            .iter()
                            .any(|process| process.command_name == command.name);
                        let process_marker = match (command.kind, running) {
                            (CommandKind::Server, true) => "run",
                            (CommandKind::Server, false) => "srv",
                            (CommandKind::Once, _) => "cmd",
                        };
                        ListItem::new(format!(
                            "{marker} {:<15} {:<10} {:<3} {}",
                            command.name,
                            command.source.label(),
                            process_marker,
                            command.command
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let commands = List::new(command_items)
            .block(Block::default().title("Commands").borders(Borders::ALL))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(commands, chunks[1], &mut self.command_state);

        let detail = self.detail_text();
        let output = Paragraph::new(detail)
            .block(
                Block::default()
                    .title("Status / Output")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(output, chunks[2]);
    }

    fn detail_text(&self) -> String {
        let Some(project) = self.selected_project() else {
            return "No projects found. Press r to rescan.".to_string();
        };
        let kinds = project
            .kinds
            .iter()
            .map(|kind| kind.label())
            .collect::<Vec<_>>()
            .join(", ");
        let git = project
            .git
            .as_ref()
            .map_or("no git status".to_string(), |git| {
                format!(
                    "{} changed={} ahead={} behind={}",
                    git.branch, git.changed, git.ahead, git.behind
                )
            });
        let last_run = project
            .last_run
            .as_ref()
            .map(format_run)
            .unwrap_or_else(|| "no runs yet".to_string());
        let processes = if project.processes.is_empty() {
            "none".to_string()
        } else {
            project
                .processes
                .iter()
                .map(|process| {
                    let port = process
                        .port
                        .map(|port| format!(" :{port}"))
                        .unwrap_or_default();
                    format!("{} pid={}{}", process.command_name, process.pid, port)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let tools = ["git", "docker", "gh", "rg", "ssh", "journalctl", "tmux"]
            .into_iter()
            .map(|tool| {
                let marker = project
                    .tools
                    .get(tool)
                    .is_some_and(|availability| availability.available);
                format!("{tool}:{}", if marker { "ok" } else { "missing" })
            })
            .collect::<Vec<_>>()
            .join(" ");
        let workflows = if project.workflows.is_empty() {
            "none".to_string()
        } else {
            project
                .workflows
                .iter()
                .map(|workflow| format!("{}: {}", workflow.name, workflow.steps.join(" -> ")))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let plugins = if project.plugins.is_empty() {
            "none".to_string()
        } else {
            project
                .plugins
                .iter()
                .map(|plugin| {
                    format!(
                        "{} [{}]: {}",
                        plugin.name,
                        plugin.source.label(),
                        plugin.cmd
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "{}\n{}\n\nkinds: {}\ngit: {}\ntools: {}\nlast: {}\nprocesses:\n{}\nworkflows:\n{}\nplugins:\n{}\n\n{}",
            project.name,
            project.root.display(),
            kinds,
            git,
            tools,
            last_run,
            processes,
            workflows,
            plugins,
            self.output
        )
    }

    fn selected_project(&self) -> Option<&Project> {
        self.project_state
            .selected()
            .and_then(|index| self.projects.get(index))
    }

    fn next_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let next = self
            .project_state
            .selected()
            .map_or(0, |index| (index + 1).min(self.projects.len() - 1));
        self.project_state.select(Some(next));
        self.sync_command_selection();
    }

    fn previous_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let previous = self.project_state.selected().unwrap_or(0).saturating_sub(1);
        self.project_state.select(Some(previous));
        self.sync_command_selection();
    }

    fn next_command(&mut self) {
        let Some(project) = self.selected_project() else {
            return;
        };
        if project.commands.is_empty() {
            return;
        }
        let next = self
            .command_state
            .selected()
            .map_or(0, |index| (index + 1).min(project.commands.len() - 1));
        self.command_state.select(Some(next));
    }

    fn previous_command(&mut self) {
        let previous = self.command_state.selected().unwrap_or(0).saturating_sub(1);
        self.command_state.select(Some(previous));
    }

    fn sync_command_selection(&mut self) {
        let Some(project) = self.selected_project() else {
            self.command_state.select(None);
            return;
        };
        if project.commands.is_empty() {
            self.command_state.select(None);
        } else {
            self.command_state.select(Some(0));
        }
    }

    fn run_selected(&mut self) -> Result<()> {
        let Some(project_index) = self.project_state.selected() else {
            return Ok(());
        };
        let Some(command_index) = self.command_state.selected() else {
            return Ok(());
        };
        let Some(project) = self.projects.get(project_index).cloned() else {
            return Ok(());
        };
        let Some(command) = project.commands.get(command_index).cloned() else {
            return Ok(());
        };

        if command.kind == CommandKind::Server {
            return self.toggle_server(project, command);
        }

        match run_command(&project, &command, &mut self.state, &self.paths) {
            Ok(result) => {
                self.output = result.output;
                self.reload()?;
            }
            Err(error) => {
                self.output = format!("error: {error:#}");
            }
        }
        Ok(())
    }

    fn toggle_server(
        &mut self,
        project: Project,
        command: crate::model::CommandSpec,
    ) -> Result<()> {
        if let Some(process) = self.state.running_process_for(&project.id, &command.name) {
            match stop_process(&process) {
                Ok(()) => {
                    self.state.mark_process_stopped(&project.id, &command.name);
                    self.output = format!(
                        "stopped {} {} pid {}",
                        project.name, command.name, process.pid
                    );
                    self.state.save(&self.paths)?;
                    self.reload()?;
                }
                Err(error) => {
                    self.output = format!("error: {error:#}");
                }
            }
            return Ok(());
        }

        match start_process(&project, &command, &self.state, &self.paths) {
            Ok(process) => {
                self.output = format!(
                    "started {} {} pid {} log: {}",
                    project.name,
                    command.name,
                    process.pid,
                    shorten_path(&process.log_path).display()
                );
                self.state.record_process(process);
                self.state.save(&self.paths)?;
                self.reload()?;
            }
            Err(error) => {
                self.output = format!("error: {error:#}");
            }
        }
        Ok(())
    }

    fn run_first_workflow(&mut self) -> Result<()> {
        let Some(project) = self.selected_project().cloned() else {
            return Ok(());
        };
        let Some(workflow) = project.workflows.first().cloned() else {
            self.output = format!("{} has no workflows", project.name);
            return Ok(());
        };

        let mut output = String::new();
        match crate::workflow::run_workflow_stream(
            &project,
            &workflow,
            &mut self.state,
            &self.paths,
            |line| {
                output.push_str(line);
                Ok(())
            },
        ) {
            Ok(result) => {
                if let Some(step) = result.failed_step {
                    output.push_str(&format!(
                        "workflow {} failed at step {step}\n",
                        result.workflow_name
                    ));
                } else {
                    output.push_str(&format!(
                        "workflow {} completed {} steps\n",
                        result.workflow_name,
                        result.completed_steps.len()
                    ));
                }
                self.output = output;
                self.state.save(&self.paths)?;
                self.reload()?;
            }
            Err(error) => {
                self.output = format!("error: {error:#}");
            }
        }
        Ok(())
    }
}

fn format_run(run: &RunSummary) -> String {
    let log = shorten_path(&run.log_path);
    format!(
        "{} exit={:?} log={}",
        run.command_name,
        run.exit_code,
        log.display()
    )
}

fn shorten_path(path: &Path) -> PathBuf {
    let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) else {
        return path.to_path_buf();
    };
    path.strip_prefix(&home)
        .map(|tail| PathBuf::from("~").join(tail))
        .unwrap_or_else(|_| path.to_path_buf())
}
