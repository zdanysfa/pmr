//! `pmr monit` — live terminal dashboard: process list, cpu/mem gauges,
//! streaming logs for the selected process.

use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph};

use crate::client::Pmr;
use crate::ipc::{Event, ProcessSnapshot, Status};

const LOG_BUFFER: usize = 200;

pub fn run() -> Result<()> {
    let mut pmr = Pmr::connect()?;
    let procs = pmr.list()?;
    if procs.is_empty() {
        println!("[pmr] no processes to monitor");
        return Ok(());
    }

    // Background thread: subscribe to log events, feed the UI via a channel.
    let (log_tx, log_rx) = std_mpsc::channel::<Event>();
    std::thread::spawn(move || {
        let Ok(sub) = Pmr::connect() else { return };
        let Ok(stream) = sub.subscribe(&["log:out", "log:err"], None) else {
            return;
        };
        for ev in stream.flatten() {
            if log_tx.send(ev).is_err() {
                return;
            }
        }
    });

    let mut terminal = ratatui::init();
    let result = ui_loop(&mut terminal, &mut pmr, log_rx);
    ratatui::restore();
    result
}

fn ui_loop(
    terminal: &mut ratatui::DefaultTerminal,
    pmr: &mut Pmr,
    log_rx: std_mpsc::Receiver<Event>,
) -> Result<()> {
    let mut procs: Vec<ProcessSnapshot> = pmr.list()?;
    let mut selected = ListState::default();
    selected.select(Some(0));
    let mut logs: VecDeque<(u32, String, String)> = VecDeque::with_capacity(LOG_BUFFER);
    let mut last_refresh = Instant::now();

    loop {
        // Drain pending log events.
        while let Ok(ev) = log_rx.try_recv() {
            if let Event::Log {
                pm_id,
                stream,
                line,
                ..
            } = ev
            {
                if logs.len() == LOG_BUFFER {
                    logs.pop_front();
                }
                logs.push_back((pm_id, stream, line));
            }
        }
        // Refresh the process list once a second.
        if last_refresh.elapsed() > Duration::from_secs(1) {
            if let Ok(p) = pmr.list() {
                procs = p;
            }
            last_refresh = Instant::now();
        }
        if !procs.is_empty() {
            let i = selected.selected().unwrap_or(0).min(procs.len() - 1);
            selected.select(Some(i));
        }

        terminal.draw(|f| draw(f, &procs, &mut selected, &logs))?;

        if event::poll(Duration::from_millis(100))?
            && let TermEvent::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = selected.selected().unwrap_or(0);
                    if i + 1 < procs.len() {
                        selected.select(Some(i + 1));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = selected.selected().unwrap_or(0);
                    selected.select(Some(i.saturating_sub(1)));
                }
                _ => {}
            }
        }
    }
}

fn draw(
    f: &mut ratatui::Frame,
    procs: &[ProcessSnapshot],
    selected: &mut ListState,
    logs: &VecDeque<(u32, String, String)>,
) {
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(f.area());

    // Left: process list.
    let items: Vec<ListItem> = procs
        .iter()
        .map(|p| {
            let color = match p.status {
                Status::Online => Color::Green,
                Status::Errored => Color::Red,
                Status::Launching | Status::WaitingRestart => Color::Yellow,
                _ => Color::DarkGray,
            };
            ListItem::new(Line::from(format!(
                "[{}] {} — {} (↺{})",
                p.pm_id, p.name, p.status, p.restarts
            )))
            .style(Style::default().fg(color))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" processes (q quit, ↑↓ select) "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, outer[0], selected);

    // Right: gauges + logs of the selected process.
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(3),
        ])
        .split(outer[1]);

    let current = selected.selected().and_then(|i| procs.get(i));
    let (cpu, mem, mem_label, pm_id) = match current {
        Some(p) => (
            (p.monit.cpu as f64 / 100.0).clamp(0.0, 1.0),
            memory_ratio(p.monit.memory),
            crate::cli::table::format_bytes(p.monit.memory),
            Some(p.pm_id),
        ),
        None => (0.0, 0.0, "-".into(), None),
    };
    f.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title(" cpu "))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(cpu)
            .label(format!("{:.1}%", cpu * 100.0)),
        right[0],
    );
    f.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title(" memory "))
            .gauge_style(Style::default().fg(Color::Magenta))
            .ratio(mem)
            .label(mem_label),
        right[1],
    );

    let visible = (right[2].height as usize).saturating_sub(2);
    let lines: Vec<Line> = logs
        .iter()
        .filter(|(id, _, _)| pm_id.is_none_or(|want| *id == want))
        .rev()
        .take(visible)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|(_, stream, line)| {
            let style = if stream == "err" {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Line::styled(line.clone(), style)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" logs ")),
        right[2],
    );
}

/// Memory gauge scale: fraction of total system memory.
fn memory_ratio(bytes: u64) -> f64 {
    use sysinfo::System;
    let total = System::new_all().total_memory().max(1);
    (bytes as f64 / total as f64).clamp(0.0, 1.0)
}
