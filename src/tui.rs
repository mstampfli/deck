//! Terminal UI: the interactive cockpit over the same surfaces as the CLI.
//!
//! Views reuse the CLI's `Render` implementations (the Summary tab is exactly
//! `deck summary`), and every action runs the deck binary itself as a
//! subprocess with its output streamed live into the Output tab. That keeps
//! the TUI on par with the CLI by construction: the `:` command bar accepts
//! any deck command line, and `!` runs a shell command in the project root.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, IsTerminal};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::contracts::render_to_string;
use crate::model::{CommandKind, Project};
use crate::safety::command_safety;
use crate::selection::{filtered_processes, load_projects};
use crate::state::{ProcessView, State};

const OUTPUT_CAP: usize = 5000;
const LOG_TAIL_LINES: usize = 400;

pub fn run_tui() -> Result<()> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        anyhow::bail!("the TUI needs an interactive terminal; use the CLI or --json instead");
    }
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Restore the terminal even if drawing panics, so a bug never leaves the
    // shell in raw mode.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        original_hook(panic);
    }));
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_app(&mut terminal);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    let _ = std::panic::take_hook();
    result
}

fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    let mut app = App::load()?;
    loop {
        app.tick()?;
        terminal.draw(|frame| app.draw(frame))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.handle_key(key)? {
                    break;
                }
            }
            Event::Mouse(mouse) => app.handle_mouse(mouse)?,
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Projects,
    Content,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Summary,
    Commands,
    Workflows,
    Processes,
    Recent,
    Output,
}

impl Tab {
    const ALL: [Tab; 6] = [
        Tab::Summary,
        Tab::Commands,
        Tab::Workflows,
        Tab::Processes,
        Tab::Recent,
        Tab::Output,
    ];

    fn title(self) -> &'static str {
        match self {
            Tab::Summary => "1 Summary",
            Tab::Commands => "2 Commands",
            Tab::Workflows => "3 Workflows",
            Tab::Processes => "4 Processes",
            Tab::Recent => "5 Recent",
            Tab::Output => "6 Output",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BarMode {
    Filter,
    DeckCommand,
    Shell,
}

/// The command palette: a filter query over every deck command plus the
/// curated setup templates.
struct Palette {
    query: String,
    list: ListState,
}

/// A running subprocess whose merged output streams into the Output tab.
struct RunningAction {
    label: String,
    child: Child,
    rx: Receiver<String>,
}

struct App {
    projects: Vec<Project>,
    state: State,
    /// Indices into `projects` that match the current filter.
    visible: Vec<usize>,
    filter: String,
    zone: Zone,
    tab: Tab,
    project_list: ListState,
    command_list: ListState,
    workflow_list: ListState,
    process_list: ListState,
    recent_list: ListState,
    /// Cached processes for the selected project, rebuilt per selection.
    processes: Vec<ProcessView>,
    recents: Vec<crate::model::RunSummary>,
    summary_cache: Option<(String, String)>,
    summary_scroll: u16,
    output: VecDeque<String>,
    output_scroll: usize,
    output_follow: bool,
    content_height: u16,
    /// Active input bar: mode, buffer, and cursor position in chars.
    bar: Option<(BarMode, String, usize)>,
    palette: Option<Palette>,
    show_help: bool,
    projects_area: Rect,
    tabs_y: u16,
    tab_hits: Vec<(u16, u16, Tab)>,
    content_area: Rect,
    palette_area: Rect,
    last_click: Option<(std::time::Instant, u16, u16)>,
    status: String,
    action: Option<RunningAction>,
}

impl App {
    fn load() -> Result<Self> {
        let (projects, state, _) = load_projects(&[])?;
        let mut app = Self {
            projects,
            state,
            visible: Vec::new(),
            filter: String::new(),
            zone: Zone::Projects,
            tab: Tab::Summary,
            project_list: ListState::default(),
            command_list: ListState::default(),
            workflow_list: ListState::default(),
            process_list: ListState::default(),
            recent_list: ListState::default(),
            processes: Vec::new(),
            recents: Vec::new(),
            summary_cache: None,
            summary_scroll: 0,
            output: VecDeque::new(),
            output_scroll: 0,
            output_follow: true,
            content_height: 0,
            bar: None,
            palette: None,
            show_help: false,
            projects_area: Rect::default(),
            tabs_y: 0,
            tab_hits: Vec::new(),
            content_area: Rect::default(),
            palette_area: Rect::default(),
            last_click: None,
            status: "Tab focus  1-6 tabs  Enter act  : deck cmd  ! shell  / filter  ? help"
                .to_string(),
            action: None,
        };
        app.apply_filter();
        Ok(app)
    }

    fn reload(&mut self) -> Result<()> {
        let selected_id = self.selected_project().map(|project| project.id.clone());
        let (projects, state, _) = load_projects(&[])?;
        self.projects = projects;
        self.state = state;
        self.summary_cache = None;
        self.apply_filter();
        if let Some(id) = selected_id
            && let Some(position) = self
                .visible
                .iter()
                .position(|index| self.projects[*index].id == id)
        {
            self.project_list.select(Some(position));
        }
        self.refresh_project_views();
        Ok(())
    }

    /// Recompute the visible project list from the filter, keeping a valid
    /// selection.
    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible = self
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                needle.is_empty() || project.name.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();
        let selection = match self.project_list.selected() {
            Some(selected) if selected < self.visible.len() => Some(selected),
            _ if self.visible.is_empty() => None,
            _ => Some(0),
        };
        self.project_list.select(selection);
        self.refresh_project_views();
    }

    fn selected_project(&self) -> Option<&Project> {
        self.project_list
            .selected()
            .and_then(|position| self.visible.get(position))
            .and_then(|index| self.projects.get(*index))
    }

    /// Rebuild per-project caches (processes, recent runs, list selections).
    fn refresh_project_views(&mut self) {
        let (processes, recents) = match self.selected_project() {
            Some(project) => (
                filtered_processes(&self.state, Some(project)),
                crate::history::recent_runs(&self.state.runs, Some(&project.id), 50),
            ),
            None => (Vec::new(), Vec::new()),
        };
        self.processes = processes;
        self.recents = recents;
        self.summary_scroll = 0;
        let command_len = self
            .selected_project()
            .map_or(0, |project| project.commands.len());
        select_first(&mut self.command_list, command_len);
        let workflow_len = self
            .selected_project()
            .map_or(0, |project| project.workflows.len());
        select_first(&mut self.workflow_list, workflow_len);
        select_first(&mut self.process_list, self.processes.len());
        select_first(&mut self.recent_list, self.recents.len());
    }

    fn summary_text(&mut self) -> String {
        let Some(project) = self.selected_project() else {
            return "no project selected; press R to rescan".to_string();
        };
        if let Some((id, text)) = &self.summary_cache
            && *id == project.id
        {
            return text.clone();
        }
        let text = match crate::summary::build(project, &self.state) {
            Ok(summary) => render_to_string(&summary),
            Err(error) => format!("summary failed: {error:#}"),
        };
        self.summary_cache = Some((project.id.clone(), text.clone()));
        text
    }

    // ----- background actions -----

    /// Spawn the deck binary itself so TUI actions share every code path with
    /// the CLI (records, process groups, signal forwarding, output shapes).
    fn spawn_deck(&mut self, args: Vec<String>) -> Result<()> {
        let exe = std::env::current_exe().context("locating the deck binary")?;
        let label = format!("deck {}", args.join(" "));
        let mut command = Command::new(exe);
        command.args(&args);
        // Run in the selected project's root so cwd-dependent commands like
        // `:init` act on the project instead of wherever the TUI started.
        if let Some(project) = self.selected_project() {
            command.current_dir(&project.root);
        }
        self.spawn_action(command, label)
    }

    fn spawn_shell(&mut self, line: String) -> Result<()> {
        let Some(project) = self.selected_project() else {
            self.status = "no project selected".to_string();
            return Ok(());
        };
        let label = format!("$ {line}");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(&line).current_dir(&project.root);
        self.spawn_action(command, label)
    }

    fn spawn_action(&mut self, mut command: Command, label: String) -> Result<()> {
        if self.action.is_some() {
            self.status = "an action is already running (c cancels it)".to_string();
            return Ok(());
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.status = format!("spawn failed: {error}");
                return Ok(());
            }
        };
        let (tx, rx) = channel::<String>();
        if let Some(stdout) = child.stdout.take() {
            spawn_line_reader(stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_line_reader(stderr, tx);
        }
        self.push_output(format!("-- {label} --"));
        self.status = format!("running: {label}");
        self.action = Some(RunningAction { label, child, rx });
        self.tab = Tab::Output;
        self.output_follow = true;
        Ok(())
    }

    /// Drain streamed output; when the action's pipes close, reap it and
    /// refresh the registry so records and processes are current.
    fn tick(&mut self) -> Result<()> {
        let Some(action) = &mut self.action else {
            return Ok(());
        };
        let mut finished = false;
        loop {
            match action.rx.try_recv() {
                Ok(line) => {
                    let line = line.trim_end_matches('\n').to_string();
                    self.output.push_back(line);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        while self.output.len() > OUTPUT_CAP {
            self.output.pop_front();
        }
        if finished {
            let mut action = self.action.take().expect("action present");
            let status = action.child.wait()?;
            let label = match status.code() {
                Some(0) => format!("done: {}", action.label),
                Some(code) => format!("failed ({code}): {}", action.label),
                None => format!("terminated: {}", action.label),
            };
            self.push_output(format!("-- {label} --"));
            self.status = label;
            self.reload()?;
        }
        Ok(())
    }

    fn cancel_action(&mut self) {
        let Some(action) = &mut self.action else {
            self.status = "nothing is running".to_string();
            return;
        };
        #[cfg(unix)]
        unsafe {
            libc::kill(-(action.child.id() as i32), libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = action.child.kill();
        self.status = format!("cancelling: {}", action.label);
    }

    fn push_output(&mut self, line: String) {
        self.output.push_back(line);
        while self.output.len() > OUTPUT_CAP {
            self.output.pop_front();
        }
    }

    fn show_log_tail(&mut self, path: &std::path::Path) {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let lines: Vec<&str> = raw.lines().collect();
                let start = lines.len().saturating_sub(LOG_TAIL_LINES);
                self.push_output(format!("-- log: {} --", path.display()));
                if start > 0 {
                    self.push_output(format!("... ({start} earlier lines)"));
                }
                for line in &lines[start..] {
                    self.push_output((*line).to_string());
                }
                self.tab = Tab::Output;
                self.output_follow = true;
            }
            Err(error) => self.status = format!("reading {}: {error}", path.display()),
        }
    }

    /// Setup actions with curated defaults, ahead of the generated entries.
    fn setup_templates(&self) -> Vec<(String, String)> {
        let project = self
            .selected_project()
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "PROJECT".to_string());
        vec![
            (
                "init this project (writes deck.toml)".to_string(),
                "init --sandbox test".to_string(),
            ),
            (
                "add a command".to_string(),
                format!("config add-command {project} NAME --cmd \"CMD\""),
            ),
            (
                "add an argv command (sandbox-safe)".to_string(),
                format!("config add-argv-command {project} NAME --arg PROG --arg ARG"),
            ),
            (
                "add a server command".to_string(),
                format!(
                    "config add-command {project} NAME --cmd \"CMD\" --kind server --port 3000"
                ),
            ),
            (
                "add a workflow".to_string(),
                format!("config add-workflow {project} NAME --step fmt --step test"),
            ),
            (
                "add a sandbox profile (locked preset)".to_string(),
                format!("config add-sandbox {project} default --preset locked"),
            ),
            (
                "add a sandbox profile (custom)".to_string(),
                format!(
                    "config add-sandbox {project} NAME --network false --readonly-project true --writable ./target --env PATH"
                ),
            ),
            (
                "add a plugin".to_string(),
                format!("config add-plugin {project} NAME --cmd \"CMD\""),
            ),
            (
                "add a task".to_string(),
                format!("tasks add {project} NAME --title \"TITLE\""),
            ),
            (
                "apply a whole config document".to_string(),
                format!("config apply {project} --file setup.toml"),
            ),
            (
                "forget this project (files untouched)".to_string(),
                format!("forget {project}"),
            ),
        ]
    }

    /// Everything the palette can offer: curated setup entries first, then
    /// every deck command from the clap graph, filtered by the query.
    /// Each item is (label, detail, template).
    fn palette_items(&self) -> Vec<(String, String, String)> {
        let query = self
            .palette
            .as_ref()
            .map(|palette| palette.query.to_lowercase())
            .unwrap_or_default();
        let mut items: Vec<(String, String, String)> = self
            .setup_templates()
            .into_iter()
            .map(|(label, line)| (format!("setup: {label}"), line.clone(), line))
            .collect();
        items.extend(
            crate::manifest::command_templates()
                .into_iter()
                .map(|template| (template.line.clone(), template.about, template.line)),
        );
        items.retain(|(label, detail, _)| {
            query.is_empty()
                || label.to_lowercase().contains(&query)
                || detail.to_lowercase().contains(&query)
        });
        items
    }

    /// Run the chosen palette entry directly when every placeholder resolved,
    /// otherwise pre-fill the bar with the cursor on the first placeholder.
    fn choose_palette_entry(&mut self) -> Result<()> {
        let items = self.palette_items();
        let selected = self
            .palette
            .as_ref()
            .and_then(|palette| palette.list.selected())
            .unwrap_or(0);
        let Some((_, _, template)) = items.into_iter().nth(selected) else {
            self.palette = None;
            return Ok(());
        };
        self.palette = None;
        let project = self.selected_project().map(|project| project.name.clone());
        let (line, ready) = resolve_template(&template, project.as_deref());
        if ready {
            let args = split_command_line(&line);
            self.spawn_deck(args)
        } else {
            let cursor = first_placeholder_end(&line);
            self.bar = Some((BarMode::DeckCommand, line, cursor));
            self.status =
                "edit the placeholders (arrows move the cursor), Enter runs it".to_string();
            Ok(())
        }
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> Result<()> {
        let count = self.palette_items().len();
        let Some(palette) = &mut self.palette else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Up => move_list(&mut palette.list, count, -1),
            KeyCode::Down => move_list(&mut palette.list, count, 1),
            KeyCode::Enter => return self.choose_palette_entry(),
            KeyCode::Backspace => {
                palette.query.pop();
                palette.list.select(Some(0));
            }
            KeyCode::Char(ch) => {
                palette.query.push(ch);
                palette.list.select(Some(0));
            }
            _ => {}
        }
        Ok(())
    }

    // ----- key handling -----

    /// Returns true when the app should quit.
    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.show_help {
            self.show_help = false;
            return Ok(false);
        }
        if self.palette.is_some() {
            self.handle_palette_key(key)?;
            return Ok(false);
        }
        if self.bar.is_some() {
            self.handle_bar_key(key)?;
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.apply_filter();
                } else {
                    return Ok(true);
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.zone = match self.zone {
                    Zone::Projects => Zone::Content,
                    Zone::Content => Zone::Projects,
                };
            }
            KeyCode::Char(digit @ '1'..='6') => {
                self.tab = Tab::ALL[digit as usize - '1' as usize];
                self.zone = Zone::Content;
            }
            KeyCode::Left | KeyCode::Char('[') => self.cycle_tab(-1),
            KeyCode::Right | KeyCode::Char(']') => self.cycle_tab(1),
            KeyCode::Char('a') => {
                let mut list = ListState::default();
                list.select(Some(0));
                self.palette = Some(Palette {
                    query: String::new(),
                    list,
                });
            }
            KeyCode::Char('/') => {
                let filter = self.filter.clone();
                let cursor = filter.chars().count();
                self.bar = Some((BarMode::Filter, filter, cursor));
            }
            KeyCode::Char(':') => self.bar = Some((BarMode::DeckCommand, String::new(), 0)),
            KeyCode::Char('!') => self.bar = Some((BarMode::Shell, String::new(), 0)),
            KeyCode::Char('R') => {
                self.reload()?;
                self.status = "reloaded".to_string();
            }
            KeyCode::Char('c') => self.cancel_action(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.scroll_page(-1),
            KeyCode::PageDown => self.scroll_page(1),
            KeyCode::End => {
                self.output_follow = true;
            }
            KeyCode::Enter => self.activate()?,
            KeyCode::Char('d') => self.dry_run_selected()?,
            KeyCode::Char('s') => self.toggle_selected_server()?,
            KeyCode::Char('l') => self.open_selected_log(),
            KeyCode::Char('r') => self.rerun_selected()?,
            _ => {}
        }
        Ok(false)
    }

    fn handle_bar_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some((mode, buffer, cursor)) = &mut self.bar else {
            return Ok(());
        };
        let mut changed = false;
        match key.code {
            KeyCode::Esc => {
                if *mode == BarMode::Filter {
                    self.filter.clear();
                    self.apply_filter();
                }
                self.bar = None;
                return Ok(());
            }
            KeyCode::Enter => {
                let mode = *mode;
                let line = buffer.clone();
                self.bar = None;
                match mode {
                    BarMode::Filter => {}
                    BarMode::DeckCommand => {
                        let args = split_command_line(&line);
                        if args.is_empty() {
                            self.status = "empty command".to_string();
                        } else {
                            self.spawn_deck(args)?;
                        }
                    }
                    BarMode::Shell => {
                        if line.trim().is_empty() {
                            self.status = "empty command".to_string();
                        } else {
                            self.spawn_shell(line)?;
                        }
                    }
                }
                return Ok(());
            }
            KeyCode::Left => *cursor = cursor.saturating_sub(1),
            KeyCode::Right => *cursor = (*cursor + 1).min(buffer.chars().count()),
            KeyCode::Home => *cursor = 0,
            KeyCode::End => *cursor = buffer.chars().count(),
            KeyCode::Backspace if *cursor > 0 => {
                let byte = char_to_byte(buffer, *cursor - 1);
                buffer.remove(byte);
                *cursor -= 1;
                changed = true;
            }
            KeyCode::Delete if *cursor < buffer.chars().count() => {
                let byte = char_to_byte(buffer, *cursor);
                buffer.remove(byte);
                changed = true;
            }
            KeyCode::Char(ch) => {
                let byte = char_to_byte(buffer, *cursor);
                buffer.insert(byte, ch);
                *cursor += 1;
                changed = true;
            }
            _ => {}
        }
        if changed && *mode == BarMode::Filter {
            self.filter = buffer.clone();
            self.apply_filter();
        }
        Ok(())
    }

    fn cycle_tab(&mut self, direction: isize) {
        let count = Tab::ALL.len() as isize;
        let next = (self.tab.index() as isize + direction).rem_euclid(count);
        self.tab = Tab::ALL[next as usize];
        self.zone = Zone::Content;
    }

    fn move_selection(&mut self, direction: isize) {
        match self.zone {
            Zone::Projects => {
                move_list(&mut self.project_list, self.visible.len(), direction);
                self.refresh_project_views();
            }
            Zone::Content => match self.tab {
                Tab::Commands => {
                    let len = self
                        .selected_project()
                        .map_or(0, |project| project.commands.len());
                    move_list(&mut self.command_list, len, direction);
                }
                Tab::Workflows => {
                    let len = self
                        .selected_project()
                        .map_or(0, |project| project.workflows.len());
                    move_list(&mut self.workflow_list, len, direction);
                }
                Tab::Processes => {
                    move_list(&mut self.process_list, self.processes.len(), direction)
                }
                Tab::Recent => move_list(&mut self.recent_list, self.recents.len(), direction),
                Tab::Summary => {
                    self.summary_scroll = scroll_by(self.summary_scroll, direction);
                }
                Tab::Output => self.scroll_output(direction),
            },
        }
    }

    fn scroll_page(&mut self, direction: isize) {
        let page = self.content_height.max(1) as isize - 1;
        match self.tab {
            Tab::Summary => {
                self.summary_scroll = scroll_by(self.summary_scroll, direction * page);
            }
            Tab::Output => self.scroll_output(direction * page),
            _ => {}
        }
    }

    fn scroll_output(&mut self, delta: isize) {
        let max = self
            .output
            .len()
            .saturating_sub(self.content_height.max(1) as usize);
        let current = if self.output_follow {
            max
        } else {
            self.output_scroll
        };
        let next = (current as isize + delta).clamp(0, max as isize) as usize;
        self.output_scroll = next;
        self.output_follow = next >= max;
    }

    // ----- actions on the selected row -----

    fn activate(&mut self) -> Result<()> {
        if self.zone == Zone::Projects {
            self.zone = Zone::Content;
            return Ok(());
        }
        match self.tab {
            Tab::Commands => {
                let Some((project_id, command)) = self.selected_command() else {
                    return Ok(());
                };
                if command.kind == CommandKind::Server {
                    return self.toggle_server(&project_id, &command.name);
                }
                self.spawn_deck(vec!["run".into(), project_id, command.name.clone()])
            }
            Tab::Workflows => {
                let Some(project) = self.selected_project() else {
                    return Ok(());
                };
                let Some(workflow) = self
                    .workflow_list
                    .selected()
                    .and_then(|index| project.workflows.get(index))
                else {
                    return Ok(());
                };
                let args = vec![
                    "workflow".into(),
                    "run".into(),
                    project.id.clone(),
                    workflow.name.clone(),
                ];
                self.spawn_deck(args)
            }
            Tab::Processes => self.toggle_selected_server(),
            Tab::Recent => {
                self.open_selected_log();
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn selected_command(&self) -> Option<(String, crate::model::CommandSpec)> {
        let project = self.selected_project()?;
        let command = project.commands.get(self.command_list.selected()?)?;
        Some((project.id.clone(), command.clone()))
    }

    fn dry_run_selected(&mut self) -> Result<()> {
        if self.tab != Tab::Commands {
            return Ok(());
        }
        let Some((project_id, command)) = self.selected_command() else {
            return Ok(());
        };
        self.spawn_deck(vec![
            "run".into(),
            project_id,
            command.name.clone(),
            "--dry-run".into(),
        ])
    }

    fn toggle_selected_server(&mut self) -> Result<()> {
        match self.tab {
            Tab::Commands => {
                let Some((project_id, command)) = self.selected_command() else {
                    return Ok(());
                };
                if command.kind != CommandKind::Server {
                    self.status = format!("{} is not a server command", command.name);
                    return Ok(());
                }
                self.toggle_server(&project_id, &command.name)
            }
            Tab::Processes => {
                let Some(view) = self
                    .process_list
                    .selected()
                    .and_then(|index| self.processes.get(index))
                else {
                    return Ok(());
                };
                if !view.alive {
                    self.status = "process is not running".to_string();
                    return Ok(());
                }
                let args = vec![
                    "stop".into(),
                    view.process.project_id.clone(),
                    view.process.command_name.clone(),
                ];
                self.spawn_deck(args)
            }
            _ => Ok(()),
        }
    }

    fn toggle_server(&mut self, project_id: &str, command_name: &str) -> Result<()> {
        let running = self
            .state
            .running_process_for(project_id, command_name)
            .is_some();
        let verb = if running { "stop" } else { "start" };
        self.spawn_deck(vec![
            verb.into(),
            project_id.to_string(),
            command_name.to_string(),
        ])
    }

    fn open_selected_log(&mut self) {
        let path = match self.tab {
            Tab::Processes => self
                .process_list
                .selected()
                .and_then(|index| self.processes.get(index))
                .map(|view| view.process.log_path.clone()),
            Tab::Recent => self
                .recent_list
                .selected()
                .and_then(|index| self.recents.get(index))
                .map(|run| run.log_path.clone()),
            _ => None,
        };
        match path {
            Some(path) => self.show_log_tail(&path),
            None => self.status = "no log selected (use the Processes or Recent tab)".to_string(),
        }
    }

    fn rerun_selected(&mut self) -> Result<()> {
        if self.tab != Tab::Recent {
            return Ok(());
        }
        let Some(run) = self
            .recent_list
            .selected()
            .and_then(|index| self.recents.get(index))
        else {
            return Ok(());
        };
        let args = vec![
            "run".into(),
            run.project_id.clone(),
            run.command_name.clone(),
        ];
        self.spawn_deck(args)
    }

    // ----- mouse -----

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.register_click(mouse.column, mouse.row);
                self.handle_click(mouse.column, mouse.row, double)
            }
            MouseEventKind::ScrollUp => {
                self.handle_wheel(mouse.column, mouse.row, -1);
                Ok(())
            }
            MouseEventKind::ScrollDown => {
                self.handle_wheel(mouse.column, mouse.row, 1);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Track click position and timing; a second click on the same cell
    /// within the double-click window activates instead of selecting.
    fn register_click(&mut self, column: u16, row: u16) -> bool {
        let now = std::time::Instant::now();
        let double = self.last_click.is_some_and(|(at, last_column, last_row)| {
            last_column == column
                && last_row == row
                && now.duration_since(at) < Duration::from_millis(450)
        });
        self.last_click = if double {
            None
        } else {
            Some((now, column, row))
        };
        double
    }

    fn handle_click(&mut self, column: u16, row: u16, double: bool) -> Result<()> {
        if self.show_help {
            self.show_help = false;
            return Ok(());
        }
        if self.palette.is_some() {
            let inner = inner_rect(self.palette_area);
            if let Some(index) = row_at(inner, column, row) {
                let count = self.palette_items().len();
                if let Some(palette) = &mut self.palette {
                    let index = palette.list.offset() + index;
                    if index < count {
                        palette.list.select(Some(index));
                        if double {
                            return self.choose_palette_entry();
                        }
                    }
                }
            } else {
                self.palette = None;
            }
            return Ok(());
        }
        if let Some(index) = row_at(inner_rect(self.projects_area), column, row) {
            let index = self.project_list.offset() + index;
            if index < self.visible.len() {
                self.project_list.select(Some(index));
                self.refresh_project_views();
            }
            self.zone = Zone::Projects;
            if double {
                self.zone = Zone::Content;
            }
            return Ok(());
        }
        if row == self.tabs_y {
            if let Some((_, _, tab)) = self
                .tab_hits
                .iter()
                .find(|(start, end, _)| column >= *start && column < *end)
            {
                self.tab = *tab;
                self.zone = Zone::Content;
            }
            return Ok(());
        }
        if let Some(index) = row_at(inner_rect(self.content_area), column, row) {
            self.zone = Zone::Content;
            let command_len = self
                .selected_project()
                .map_or(0, |project| project.commands.len());
            let workflow_len = self
                .selected_project()
                .map_or(0, |project| project.workflows.len());
            let (list, len) = match self.tab {
                Tab::Commands => (&mut self.command_list, command_len),
                Tab::Workflows => (&mut self.workflow_list, workflow_len),
                Tab::Processes => (&mut self.process_list, self.processes.len()),
                Tab::Recent => (&mut self.recent_list, self.recents.len()),
                Tab::Summary | Tab::Output => return Ok(()),
            };
            let index = list.offset() + index;
            if index < len {
                list.select(Some(index));
                if double {
                    return self.activate();
                }
            }
        }
        Ok(())
    }

    fn handle_wheel(&mut self, column: u16, row: u16, direction: isize) {
        if self.palette.is_some() {
            let count = self.palette_items().len();
            if let Some(palette) = &mut self.palette {
                move_list(&mut palette.list, count, direction);
            }
            return;
        }
        if rect_contains(self.projects_area, column, row) {
            move_list(&mut self.project_list, self.visible.len(), direction);
            self.refresh_project_views();
            return;
        }
        if rect_contains(self.content_area, column, row) || row == self.tabs_y {
            let zone = self.zone;
            self.zone = Zone::Content;
            self.move_selection(direction);
            self.zone = zone;
        }
    }

    // ----- drawing -----

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(frame.area());
        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Min(20)])
            .split(outer[0]);

        self.projects_area = main[0];
        self.draw_projects(frame, main[0]);
        self.draw_content(frame, main[1]);
        self.draw_bottom(frame, outer[1]);
        if self.palette.is_some() {
            self.draw_palette(frame);
        } else {
            self.palette_area = Rect::default();
        }
        if self.show_help {
            self.draw_help(frame);
        }
    }

    fn draw_palette(&mut self, frame: &mut Frame<'_>) {
        let items = self.palette_items();
        let query = self
            .palette
            .as_ref()
            .map(|palette| palette.query.clone())
            .unwrap_or_default();
        let height = (items.len() as u16 + 2).clamp(3, frame.area().height);
        let area = centered_rect(frame.area(), 86, height);
        self.palette_area = area;
        let rows = items
            .into_iter()
            .map(|(label, detail, _)| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{label:<42}")),
                    Span::styled(detail, Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect::<Vec<_>>();
        let title = format!("Commands: type to filter [{query}] (Enter or double-click runs)");
        let list = List::new(rows)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_widget(Clear, area);
        if let Some(palette) = &mut self.palette {
            frame.render_stateful_widget(list, area, &mut palette.list);
        }
    }

    fn draw_projects(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .visible
            .iter()
            .map(|index| {
                let project = &self.projects[*index];
                let running = project.processes.iter().any(|process| {
                    self.state
                        .running_process_for(&project.id, &process.command_name)
                        .is_some()
                });
                let mut spans = vec![Span::styled(
                    project.name.clone(),
                    Style::default().fg(Color::Cyan),
                )];
                if let Some(git) = &project.git
                    && git.changed > 0
                {
                    spans.push(Span::styled(" *", Style::default().fg(Color::Yellow)));
                }
                if running {
                    spans.push(Span::styled(" >", Style::default().fg(Color::Green)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect::<Vec<_>>();
        let title = if self.filter.is_empty() {
            format!("Projects ({})", self.visible.len())
        } else {
            format!("Projects /{} ({})", self.filter, self.visible.len())
        };
        let list = List::new(items)
            .block(focus_block(title, self.zone == Zone::Projects))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.project_list);
    }

    fn draw_content(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(2)])
            .split(area);
        // Render the tab titles by hand so each one has an exact hit box for
        // mouse clicks.
        self.tabs_y = chunks[0].y;
        self.tab_hits.clear();
        let mut spans = Vec::new();
        let mut x = chunks[0].x;
        for (position, tab) in Tab::ALL.iter().enumerate() {
            if position > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
                x += 3;
            }
            let title = tab.title();
            let width = title.len() as u16;
            let style = if *tab == self.tab {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            spans.push(Span::styled(title, style));
            self.tab_hits.push((x, x + width, *tab));
            x += width;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);

        let pane = chunks[1];
        self.content_area = pane;
        self.content_height = pane.height.saturating_sub(2);
        match self.tab {
            Tab::Summary => {
                let text = self.summary_text();
                let paragraph = Paragraph::new(text)
                    .block(focus_block(
                        "Summary".to_string(),
                        self.zone == Zone::Content,
                    ))
                    .wrap(Wrap { trim: false })
                    .scroll((self.summary_scroll, 0));
                frame.render_widget(paragraph, pane);
            }
            Tab::Commands => self.draw_commands(frame, pane),
            Tab::Workflows => self.draw_workflows(frame, pane),
            Tab::Processes => self.draw_processes(frame, pane),
            Tab::Recent => self.draw_recent(frame, pane),
            Tab::Output => self.draw_output(frame, pane),
        }
    }

    fn draw_commands(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .selected_project()
            .map(|project| {
                project
                    .commands
                    .iter()
                    .map(|command| {
                        let safety = command_safety(command);
                        let running = command.kind == CommandKind::Server
                            && self
                                .state
                                .running_process_for(&project.id, &command.name)
                                .is_some();
                        let marker = if !command.available {
                            Span::styled("!", Style::default().fg(Color::Red))
                        } else if running {
                            Span::styled(">", Style::default().fg(Color::Green))
                        } else {
                            Span::raw(" ")
                        };
                        let mut traits = Vec::new();
                        if command.kind == CommandKind::Server {
                            traits.push("server");
                        }
                        traits.push(if safety.direct_argv { "argv" } else { "shell" });
                        ListItem::new(Line::from(vec![
                            marker,
                            Span::raw(format!(
                                " {:<18} [{}] {}",
                                command.name,
                                traits.join(","),
                                command.command
                            )),
                        ]))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let list = List::new(items)
            .block(focus_block(
                "Commands (Enter run/toggle, d dry-run, s server)".to_string(),
                self.zone == Zone::Content,
            ))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.command_list);
    }

    fn draw_workflows(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .selected_project()
            .map(|project| {
                project
                    .workflows
                    .iter()
                    .map(|workflow| {
                        ListItem::new(format!(
                            "{:<18} {}",
                            workflow.name,
                            workflow.steps.join(" -> ")
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let list = List::new(items)
            .block(focus_block(
                "Workflows (Enter runs)".to_string(),
                self.zone == Zone::Content,
            ))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.workflow_list);
    }

    fn draw_processes(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .processes
            .iter()
            .map(|view| {
                let status = if view.alive {
                    Span::styled("running", Style::default().fg(Color::Green))
                } else {
                    Span::styled("stopped", Style::default().fg(Color::DarkGray))
                };
                let port = view
                    .process
                    .port
                    .map(|port| format!(" :{port}"))
                    .unwrap_or_default();
                ListItem::new(Line::from(vec![
                    status,
                    Span::raw(format!(
                        " pid={:<8} {:<18}{}",
                        view.process.pid, view.process.command_name, port
                    )),
                ]))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(focus_block(
                "Processes (Enter/s stop, l log)".to_string(),
                self.zone == Zone::Content,
            ))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.process_list);
    }

    fn draw_recent(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .recents
            .iter()
            .map(|run| {
                let exit = if !run.finished && crate::state::is_run_alive(run) {
                    "running".to_string()
                } else {
                    run.exit_label()
                };
                let style = match exit.as_str() {
                    "0" | "running" => Style::default(),
                    _ => Style::default().fg(Color::Red),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{exit:<12}"), style),
                    Span::raw(format!(
                        "{:<18} {}",
                        run.command_name,
                        run.started_at.format("%m-%d %H:%M:%S")
                    )),
                ]))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(focus_block(
                "Recent runs (Enter/l log, r rerun)".to_string(),
                self.zone == Zone::Content,
            ))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.recent_list);
    }

    fn draw_output(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let height = area.height.saturating_sub(2) as usize;
        let max = self.output.len().saturating_sub(height.max(1));
        let scroll = if self.output_follow {
            max
        } else {
            self.output_scroll.min(max)
        };
        let text = self
            .output
            .iter()
            .skip(scroll)
            .take(height.max(1))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let title = if self.action.is_some() {
            "Output (running... c cancels)".to_string()
        } else {
            "Output".to_string()
        };
        let paragraph = Paragraph::new(text).block(focus_block(title, self.zone == Zone::Content));
        frame.render_widget(paragraph, area);
    }

    fn draw_bottom(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let line = match &self.bar {
            Some((mode, buffer, cursor)) => {
                let prefix = match mode {
                    BarMode::Filter => "/",
                    BarMode::DeckCommand => ":deck ",
                    BarMode::Shell => "!",
                };
                let byte = char_to_byte(buffer, *cursor);
                format!("{prefix}{}\u{2588}{}", &buffer[..byte], &buffer[byte..])
            }
            None => self.status.clone(),
        };
        let style = if self.bar.is_some() {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        frame.render_widget(Paragraph::new(line).style(style), area);
    }

    fn draw_help(&self, frame: &mut Frame<'_>) {
        let area = centered_rect(frame.area(), 62, 20);
        let help = "\
deck TUI

  Tab            switch focus between projects and content
  1-6 [ ] arrows switch content tab (left/right)
  up/down j/k    move selection / scroll
  PgUp/PgDn    page scroll (Summary, Output)   End: follow output
  Enter        run command / toggle server / open log
  d            dry-run the selected command
  s            start/stop the selected server or process
  l            open the selected log (Processes, Recent)
  r            rerun the selected recent run
  a            command palette: every deck command; type to filter,
               Enter/double-click runs (placeholders open the : bar)
  mouse        click selects, double-click runs, wheel scrolls,
               click a tab title to switch
  /            filter projects (Esc clears)
  :            run any deck command line (e.g. :summary deck)
  !            run a shell command in the project root
  c            cancel the running action
  R            reload the registry
  q / Esc      quit (Esc closes bars and the filter first)

any key closes this help";
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(help).block(Block::default().title("Help").borders(Borders::ALL)),
            area,
        );
    }
}

fn focus_block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(style)
}

fn select_first(list: &mut ListState, len: usize) {
    list.select(if len == 0 { None } else { Some(0) });
}

fn move_list(list: &mut ListState, len: usize, direction: isize) {
    if len == 0 {
        list.select(None);
        return;
    }
    let current = list.selected().unwrap_or(0) as isize;
    let next = (current + direction).clamp(0, len as isize - 1) as usize;
    list.select(Some(next));
}

fn scroll_by(current: u16, delta: isize) -> u16 {
    (current as isize + delta).max(0).min(u16::MAX as isize) as u16
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Substitute the selected project into a command template and strip
/// unfilled optional tokens. Returns the resolved line and whether every
/// placeholder resolved (ready to run without editing).
fn resolve_template(template: &str, project: Option<&str>) -> (String, bool) {
    let mut parts = Vec::new();
    let mut ready = true;
    for token in template.split_whitespace() {
        let optional = token.starts_with('[');
        let core = token
            .trim_matches(|ch| ch == '[' || ch == ']')
            .trim_end_matches("..");
        let placeholder = is_placeholder(core);
        if placeholder && core.contains("PROJECT") {
            match project {
                Some(project) => parts.push(project.to_string()),
                None if optional => {}
                None => {
                    parts.push(core.to_string());
                    ready = false;
                }
            }
        } else if placeholder {
            if !optional {
                parts.push(core.to_string());
                ready = false;
            }
        } else {
            parts.push(token.to_string());
        }
    }
    (parts.join(" "), ready)
}

/// A token is a placeholder when it is shouting (NAME, COMMAND) or offers a
/// fixed choice set (diff|branches|commits).
fn is_placeholder(token: &str) -> bool {
    if token.contains('|') {
        return true;
    }
    token.chars().any(|ch| ch.is_ascii_uppercase())
        && token
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch == '_' || ch == '.')
}

/// Cursor position for editing: the end of the first unresolved placeholder,
/// so Backspace immediately replaces it.
fn first_placeholder_end(line: &str) -> usize {
    let mut chars = 0;
    for token in line.split(' ') {
        let core = token
            .trim_matches(|ch| ch == '[' || ch == ']')
            .trim_end_matches("..");
        if is_placeholder(core) || token.contains('|') {
            return chars + token.chars().count();
        }
        chars += token.chars().count() + 1;
    }
    line.chars().count()
}

/// The area inside a bordered block.
fn inner_rect(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Which visible row (0-based) of a bordered list a click landed on.
fn row_at(inner: Rect, column: u16, row: u16) -> Option<usize> {
    rect_contains(inner, column, row).then(|| (row - inner.y) as usize)
}

/// Byte offset of the `cursor`-th character in `text`.
fn char_to_byte(text: &str, cursor: usize) -> usize {
    text.char_indices()
        .nth(cursor)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn spawn_line_reader<R: io::Read + Send + 'static>(reader: R, tx: Sender<String>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        while let Ok(read) = reader.read_line(&mut line) {
            if read == 0 {
                break;
            }
            if tx.send(std::mem::take(&mut line)).is_err() {
                break;
            }
        }
    });
}

/// Split a `:` command line into arguments, honoring single and double quotes
/// so titles and shell-ish values survive (`tasks add deck x --title "Ship"`).
fn split_command_line(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut pending = false;
    for ch in line.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => current.push(ch),
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    pending = true;
                }
                ch if ch.is_whitespace() => {
                    if pending || !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                        pending = false;
                    }
                }
                ch => current.push(ch),
            },
        }
    }
    if pending || !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_command_lines_with_quotes() {
        assert_eq!(
            split_command_line(r#"tasks add deck ship --title "Ship it now""#),
            vec!["tasks", "add", "deck", "ship", "--title", "Ship it now"]
        );
        assert_eq!(split_command_line("summary deck"), vec!["summary", "deck"]);
        assert_eq!(split_command_line("  "), Vec::<String>::new());
        assert_eq!(
            split_command_line(r#"run deck ''"#),
            vec!["run", "deck", ""]
        );
    }

    #[test]
    fn resolves_templates_against_the_selected_project() {
        assert_eq!(resolve_template("list", Some("x")), ("list".into(), true));
        assert_eq!(
            resolve_template("run PROJECT COMMAND", Some("deck")),
            ("run deck COMMAND".into(), false)
        );
        assert_eq!(
            resolve_template("ps [PROJECT]", Some("deck")),
            ("ps deck".into(), true)
        );
        assert_eq!(
            resolve_template("scan [ROOTS..]", None),
            ("scan".into(), true)
        );
        assert_eq!(
            resolve_template("git PROJECT diff|branches|commits", Some("p")),
            ("git p diff|branches|commits".into(), false)
        );
        assert_eq!(
            resolve_template("forget PROJECT", None),
            ("forget PROJECT".into(), false)
        );
    }

    #[test]
    fn cursor_lands_on_the_first_placeholder() {
        assert_eq!(first_placeholder_end("run deck COMMAND"), 16);
        assert_eq!(first_placeholder_end("tasks add deck NAME --title T"), 19);
        assert_eq!(first_placeholder_end("list"), 4);
    }

    #[test]
    fn char_to_byte_handles_multibyte_text() {
        assert_eq!(char_to_byte("abc", 0), 0);
        assert_eq!(char_to_byte("abc", 2), 2);
        assert_eq!(char_to_byte("abc", 9), 3);
        assert_eq!(char_to_byte("a\u{e4}c", 2), 3);
    }

    #[test]
    fn list_movement_clamps_to_bounds() {
        let mut list = ListState::default();
        move_list(&mut list, 3, 1);
        assert_eq!(list.selected(), Some(1));
        move_list(&mut list, 3, 10);
        assert_eq!(list.selected(), Some(2));
        move_list(&mut list, 3, -10);
        assert_eq!(list.selected(), Some(0));
        move_list(&mut list, 0, 1);
        assert_eq!(list.selected(), None);
    }
}
