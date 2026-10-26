//! Drawing. Everything shown is in [`App`]; nothing here calls the runtime.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row as TableRow, Table};
use ratatui::Frame;

use crate::app::App;
use crate::model::{Row, NOT_CREATED, NO_CHECK, NO_HEALTH};

/// Rows of the events pane, inside its border.
const EVENT_LINES: u16 = 5;

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Header, services (sized to fit), events, logs (the rest), footer.
    let table_height = u16::try_from(app.rows.len()).unwrap_or(u16::MAX).max(1) + 3;
    let [header, services, events, logs, footer] = frame.area().layout(&Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(table_height),
        Constraint::Length(EVENT_LINES + 2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ]));

    draw_header(frame, header, app);
    draw_services(frame, services, app);
    draw_events(frame, events, app);
    draw_logs(frame, logs, app);
    draw_footer(frame, footer, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::from(format!(" {} ", app.declared.project))
            .bold()
            .reversed(),
        Span::from("  "),
    ];
    match app.supervisor_pid {
        Some(pid) => spans.push(Span::from(format!("supervisor pid {pid}")).green()),
        None => spans.push(Span::from("no supervisor: health unknown").yellow()),
    }
    if let Some(updated) = &app.updated_at {
        spans.push(Span::from(format!("  ·  updated {updated}")).dark_gray());
    }
    if let Some(error) = &app.runtime_error {
        spans.push(Span::from("  ·  runtime: ").red());
        spans.push(Span::from(error.clone()).red());
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_services(frame: &mut Frame, area: Rect, app: &mut App) {
    let header = TableRow::new([
        "SERVICE", "STATE", "HEALTH", "UPTIME", "RESTARTS", "POLICY", "PORTS", "IMAGE",
    ])
    .style(Style::new().bold());

    let rows: Vec<TableRow> = app.rows.iter().map(service_row).collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(11),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(24),
            Constraint::Fill(1),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ")
    .block(Block::bordered().title(" services "));

    if app.rows.is_empty() {
        let message = if app.updated_at.is_some() {
            "no containers and no declared services for this project"
        } else {
            "waiting for the first poll…"
        };
        frame.render_widget(
            Paragraph::new(message)
                .dark_gray()
                .block(Block::bordered().title(" services ")),
            area,
        );
        return;
    }
    frame.render_stateful_widget(table, area, &mut app.table);
}

fn service_row(row: &Row) -> TableRow<'static> {
    let state_style = match row.state.as_str() {
        "running" => Style::new().green(),
        NOT_CREATED => Style::new().dark_gray(),
        _ => Style::new().red(),
    };
    let health_style = match row.health.as_str() {
        "healthy" => Style::new().green(),
        "unhealthy" => Style::new().red(),
        "starting" => Style::new().yellow(),
        NO_HEALTH | NO_CHECK => Style::new().dark_gray(),
        _ => Style::new().yellow(),
    };
    let restarts = match (row.restarts, row.gave_up) {
        (count, true) => Cell::from(format!("{count} gave up")).red(),
        (0, false) => Cell::from("0").dark_gray(),
        (count, false) => Cell::from(count.to_string()).yellow(),
    };
    TableRow::new([
        Cell::from(row.service.clone()).bold(),
        Cell::from(row.state.clone()).style(state_style),
        Cell::from(row.health.clone()).style(health_style),
        Cell::from(row.uptime.clone().unwrap_or_else(|| "—".to_owned())),
        restarts,
        Cell::from(row.policy.clone().unwrap_or_else(|| "—".to_owned())),
        Cell::from(row.ports.join(", ")),
        Cell::from(row.image.clone().unwrap_or_default()).dark_gray(),
    ])
}

fn draw_events(frame: &mut Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = if app.events.is_empty() {
        vec![Line::from("no changes since top started").dark_gray()]
    } else {
        app.events
            .iter()
            .take(usize::from(EVENT_LINES))
            .map(|event| {
                Line::from(vec![
                    Span::from(format!("{} ", event.at)).dark_gray(),
                    Span::from(format!("{} ", event.service)).bold(),
                    Span::from(event.text.clone()),
                ])
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" events ")),
        area,
    );
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let service = app.selected_service().unwrap_or("");
    let scrolled = if app.log_scroll > 0 {
        format!(" · {} lines up", app.log_scroll)
    } else {
        String::new()
    };
    let block = Block::bordered().title(format!(" logs · {service}{scrolled} "));

    let Some(logs) = &app.logs else {
        let row = app.rows.iter().find(|row| row.service == service);
        let message = match row {
            Some(row) if row.container.is_none() => "the container has not been created",
            Some(_) => "fetching…",
            None => "",
        };
        frame.render_widget(Paragraph::new(message).dark_gray().block(block), area);
        return;
    };
    if let Some(error) = &logs.error {
        frame.render_widget(Paragraph::new(error.clone()).red().block(block), area);
        return;
    }

    // The tail that fits, moved up by the scroll. Lines are clipped rather than
    // wrapped, so one line of log is one line of screen and the arithmetic holds.
    let height = usize::from(area.height.saturating_sub(2));
    let end = logs.lines.len().saturating_sub(app.log_scroll);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = logs.lines[start..end]
        .iter()
        .map(|line| Line::from(line.clone()))
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let keys = Span::from(" q quit · ↑↓ select · p probe · r refresh · PgUp/PgDn/End logs ")
        .style(Style::new().fg(Color::Black).bg(Color::Gray));
    let mut spans = vec![keys];
    if let Some(note) = &app.note {
        spans.push(Span::from(format!("  {note}")));
    }
    frame.render_widget(Line::from(spans), area);
}

#[cfg(test)]
mod tests {
    use super::draw;
    use crate::app::App;
    use crate::model::Declared;
    use crate::poll::{Logs, Snapshot, Update};
    use container_compose_runtime::ObservedContainer;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn captured() -> Vec<ObservedContainer> {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/real/container-list.json"
        ))
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// The screen as text, one line per terminal row, for assertions and for
    /// reading what was drawn.
    pub(crate) fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with_logs() -> App {
        let project = captured()[0].project().unwrap().to_owned();
        let mut app = App::new(Declared::bare(project));
        app.apply(Update::Snapshot(Snapshot {
            at_secs: 1_792_830_352,
            containers: Ok(captured()),
            cache: None,
            logs: None,
        }));
        app.apply(Update::Snapshot(Snapshot {
            at_secs: 1_792_830_353,
            containers: Ok(captured()),
            cache: None,
            logs: Some(Logs {
                service: "api".to_owned(),
                lines: vec!["capture-hello".to_owned()],
                error: None,
            }),
        }));
        app
    }

    #[test]
    fn the_screen_shows_services_events_and_the_selected_services_logs() {
        let mut app = app_with_logs();
        let text = screen(&mut app, 120, 24);
        println!("{text}");

        assert!(text.contains("no supervisor: health unknown"));
        assert!(text.contains("SERVICE"));
        assert!(text.contains("▶ api"));
        assert!(text.contains("worker"));
        assert!(text.contains("running"));
        assert!(text.contains("stopped"));
        assert!(text.contains("logs · api"));
        assert!(text.contains("capture-hello"));
        assert!(text.contains("q quit"));
    }

    #[test]
    fn a_narrow_terminal_still_draws() {
        let mut app = app_with_logs();
        let text = screen(&mut app, 40, 12);
        assert!(text.contains("api"));
    }
}
