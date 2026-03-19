//! TUI dashboard rendering.

use abox_core::sandbox::SandboxStatus;
use abox_core::workspace::DivergenceEntry;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Tabs};
use std::io::stdout;
use std::time::Duration;

/// State for the TUI dashboard.
pub struct DashboardState {
    pub sandboxes: Vec<SandboxStatus>,
    pub divergence: Vec<DivergenceEntry>,
    pub audit_lines: Vec<String>,
    pub selected_tab: usize,
    pub should_quit: bool,
}

impl DashboardState {
    pub fn new() -> Self {
        Self {
            sandboxes: Vec::new(),
            divergence: Vec::new(),
            audit_lines: Vec::new(),
            selected_tab: 0,
            should_quit: false,
        }
    }
}

/// Run the TUI dashboard. This takes over the terminal.
pub fn run_dashboard(state: &mut DashboardState) -> anyhow::Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    loop {
        terminal.draw(|frame| render(frame, state))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => state.should_quit = true,
                        KeyCode::Tab => {
                            state.selected_tab = (state.selected_tab + 1) % 3;
                        }
                        KeyCode::BackTab => {
                            state.selected_tab =
                                if state.selected_tab == 0 { 2 } else { state.selected_tab - 1 };
                        }
                        _ => {}
                    }
                }
            }
        }

        if state.should_quit {
            break;
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn render(frame: &mut Frame, state: &DashboardState) {
    let area = frame.area();

    // Layout: header + main content + footer
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header with tabs
            Constraint::Min(10),   // main content
            Constraint::Length(1), // footer
        ])
        .split(area);

    // Header with tabs
    let tabs = Tabs::new(vec!["Sandboxes", "Divergence", "Audit Log"])
        .block(Block::default().borders(Borders::ALL).title(" abox "))
        .select(state.selected_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(tabs, layout[0]);

    // Main content based on selected tab
    match state.selected_tab {
        0 => render_sandboxes(frame, layout[1], state),
        1 => render_divergence(frame, layout[1], state),
        2 => render_audit(frame, layout[1], state),
        _ => {}
    }

    // Footer
    let footer = Paragraph::new(" Tab: switch | q: quit | r: refresh")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, layout[2]);
}

fn render_sandboxes(frame: &mut Frame, area: Rect, state: &DashboardState) {
    let header = Row::new(vec!["ID", "Branch", "State", "PID", "Ahead"])
        .style(Style::default().bold().fg(Color::Cyan));

    let rows: Vec<Row> = state
        .sandboxes
        .iter()
        .map(|s| {
            let state_style = match s.vm_state.as_str() {
                "running" => Style::default().fg(Color::Green),
                "paused" => Style::default().fg(Color::Yellow),
                "stopped" => Style::default().fg(Color::Red),
                _ => Style::default(),
            };
            Row::new(vec![
                Cell::from(s.id.clone()),
                Cell::from(s.branch.clone()),
                Cell::from(s.vm_state.clone()).style(state_style),
                Cell::from(s.vm_pid.to_string()),
                Cell::from(s.commits_ahead.to_string()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(" Sandboxes "));

    frame.render_widget(table, area);
}

fn render_divergence(frame: &mut Frame, area: Rect, state: &DashboardState) {
    let header =
        Row::new(vec!["File", "Sandbox", "Status"]).style(Style::default().bold().fg(Color::Cyan));

    let rows: Vec<Row> = state
        .divergence
        .iter()
        .map(|d| {
            Row::new(vec![
                Cell::from(d.file_path.clone()),
                Cell::from(d.sandbox_id.clone()),
                Cell::from(d.status.to_string()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Percentage(50), Constraint::Length(16), Constraint::Length(12)],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(" Divergence Matrix "));

    frame.render_widget(table, area);
}

fn render_audit(frame: &mut Frame, area: Rect, state: &DashboardState) {
    let items: Vec<ListItem> = state
        .audit_lines
        .iter()
        .rev()
        .take(area.height as usize)
        .map(|line| ListItem::new(line.as_str()))
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Audit Log (newest first) "));

    frame.render_widget(list, area);
}
