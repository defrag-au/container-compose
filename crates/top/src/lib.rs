//! `container-compose top`: a read-only terminal dashboard for a stack.
//!
//! It shows what `ps` shows, kept current — state, health, uptime, restarts
//! and ports per service — plus what changed between polls and the selected
//! service's logs. It reads the same two sources `ps` does: the runtime's
//! container list and the supervisor's health cache. It takes no lock and
//! starts or stops nothing, so it runs beside a supervisor, not instead of one.
//!
//! The split, so the logic is testable without a terminal:
//!
//! - [`model`] is pure: a poll and the clock in, rows and events out.
//! - [`poll`] is the worker thread that owns every runtime call.
//! - [`app`] is the state a poll or a key changes.
//! - [`ui`] draws it.

pub mod app;
pub mod model;
pub mod poll;
pub mod ui;

use std::collections::BTreeMap;
use std::io::{self, IsTerminal};
use std::time::Duration;

use ratatui::crossterm::event;
use ratatui::DefaultTerminal;

use container_compose_stack::resolve::ResolvedStack;

use crate::app::App;
use crate::model::Declared;
use crate::poll::{Request, Source};

/// How long a key wait lasts before the screen redraws with whatever the
/// poller sent meanwhile.
const FRAME: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct Options {
    pub runtime: String,
    /// The project to show; from the stack file, or the directory as `ps` does.
    pub project: String,
    /// The stack, when its file exists. Without it, rows come from the
    /// project's containers alone and there is no on-demand probe.
    pub stack: Option<ResolvedStack>,
    pub interval: Duration,
    pub log_lines: u32,
}

pub fn run(options: Options) -> io::Result<()> {
    if !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "`top` needs a terminal; use `ps --json` from a script",
        ));
    }

    let declared = options.stack.as_ref().map_or_else(
        || Declared::bare(options.project.clone()),
        Declared::from_stack,
    );
    let healthchecks: BTreeMap<String, (Vec<String>, Duration)> = options
        .stack
        .iter()
        .flat_map(|stack| stack.services.iter())
        .filter_map(|(name, service)| {
            let check = service.healthcheck.as_ref()?;
            Some((
                name.clone(),
                (check.exec.clone(), check.timeout.as_duration()),
            ))
        })
        .collect();

    let (requests, updates) = poll::spawn(Source {
        runtime: options.runtime,
        project: declared.project.clone(),
        config_hash: declared.config_hash.clone(),
        healthchecks,
        interval: options.interval,
        log_lines: options.log_lines,
    });
    let mut app = App::new(declared);

    ratatui::run(|terminal| event_loop(terminal, &mut app, &requests, &updates))
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    requests: &std::sync::mpsc::Sender<Request>,
    updates: &std::sync::mpsc::Receiver<poll::Update>,
) -> io::Result<()> {
    while !app.quit {
        while let Ok(update) = updates.try_recv() {
            if let Some(request) = app.apply(update) {
                send(requests, request)?;
            }
        }
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(FRAME)? {
            if let Some(key) = event::read()?.as_key_press_event() {
                if let Some(request) = app.on_key(key) {
                    send(requests, request)?;
                }
            }
        }
    }
    Ok(())
}

fn send(requests: &std::sync::mpsc::Sender<Request>, request: Request) -> io::Result<()> {
    requests
        .send(request)
        .map_err(|_| io::Error::other("the poller stopped"))
}
