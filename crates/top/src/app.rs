//! The dashboard's state, and how a poll or a key changes it.
//!
//! No terminal here: [`App::apply`] takes an [`Update`] and [`App::on_key`]
//! a key, and each returns the [`Request`] the poller should receive, if any.

use std::collections::VecDeque;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use crate::model::{self, Declared, Event, Row};
use crate::poll::{Logs, Request, Snapshot, Update};

/// Events kept for the events pane; older ones fall off.
const EVENT_HISTORY: usize = 200;

#[derive(Debug)]
pub struct App {
    pub declared: Declared,
    pub rows: Vec<Row>,
    pub table: TableState,
    /// Newest first.
    pub events: VecDeque<Event>,
    pub logs: Option<Logs>,
    /// Lines scrolled up from the end of the log; 0 follows the tail.
    pub log_scroll: usize,
    pub runtime_error: Option<String>,
    pub supervisor_pid: Option<u32>,
    pub updated_at: Option<String>,
    /// The last on-demand probe, shown in the footer.
    pub note: Option<String>,
    pub quit: bool,
    polled: bool,
}

impl App {
    pub fn new(declared: Declared) -> Self {
        Self {
            declared,
            rows: Vec::new(),
            table: TableState::default(),
            events: VecDeque::new(),
            logs: None,
            log_scroll: 0,
            runtime_error: None,
            supervisor_pid: None,
            updated_at: None,
            note: None,
            quit: false,
            polled: false,
        }
    }

    pub fn selected_service(&self) -> Option<&str> {
        self.table
            .selected()
            .and_then(|index| self.rows.get(index))
            .map(|row| row.service.as_str())
    }

    pub fn apply(&mut self, update: Update) -> Option<Request> {
        match update {
            Update::Snapshot(snapshot) => self.apply_snapshot(snapshot),
            Update::Probed { service, outcome } => {
                self.note = Some(format!("probe {service}: {outcome}"));
                None
            }
        }
    }

    fn apply_snapshot(&mut self, snapshot: Snapshot) -> Option<Request> {
        let before = self.selected_service().map(str::to_owned);
        self.updated_at = Some(model::clock_of_day(snapshot.at_secs));
        self.supervisor_pid = snapshot.cache.as_ref().map(|cache| cache.pid);

        match snapshot.containers {
            Ok(containers) => {
                self.runtime_error = None;
                let rows = model::rows(
                    &self.declared,
                    &containers,
                    snapshot.cache.as_ref(),
                    snapshot.at_secs,
                );
                // The first poll is the baseline, not a burst of "appeared".
                if self.polled {
                    for event in model::events(&self.rows, &rows, snapshot.at_secs) {
                        self.events.push_front(event);
                    }
                    self.events.truncate(EVENT_HISTORY);
                }
                self.polled = true;
                self.rows = rows;
            }
            // Keep the last rows on screen; the header says they are stale.
            Err(error) => self.runtime_error = Some(error),
        }

        self.keep_selection(before.as_deref());
        let after = self.selected_service().map(str::to_owned);

        // A poll that raced a selection change carries the previous service's
        // lines; only the selected service's logs are shown.
        self.logs = snapshot
            .logs
            .filter(|logs| after.as_deref() == Some(logs.service.as_str()));

        (after != before).then_some(Request::Select(after))
    }

    /// Stay on the same service when rows move; fall back to the first row.
    fn keep_selection(&mut self, service: Option<&str>) {
        let index = service
            .and_then(|service| self.rows.iter().position(|row| row.service == service))
            .or_else(|| (!self.rows.is_empty()).then_some(0));
        self.table.select(index);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Request> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit = true;
                None
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageUp => {
                self.log_scroll = self.log_scroll.saturating_add(10);
                None
            }
            KeyCode::PageDown => {
                self.log_scroll = self.log_scroll.saturating_sub(10);
                None
            }
            KeyCode::End => {
                self.log_scroll = 0;
                None
            }
            KeyCode::Char('r') => Some(Request::Refresh),
            KeyCode::Char('p') => {
                let service = self.selected_service()?.to_owned();
                self.note = Some(format!("probing {service}…"));
                Some(Request::Probe(service))
            }
            _ => None,
        }
    }

    fn move_selection(&mut self, step: isize) -> Option<Request> {
        if self.rows.is_empty() {
            return None;
        }
        let last = self.rows.len() - 1;
        let current = self.table.selected().unwrap_or(0);
        let next = current.saturating_add_signed(step).min(last);
        if next == current {
            return None;
        }
        self.table.select(Some(next));
        self.log_scroll = 0;
        // The old service's lines would sit under the new service's title.
        self.logs = None;
        Some(Request::Select(self.selected_service().map(str::to_owned)))
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::model::Declared;
    use crate::poll::{Logs, Request, Snapshot, Update};
    use container_compose_runtime::ObservedContainer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    fn captured() -> Vec<ObservedContainer> {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/real/container-list.json"
        ))
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn app() -> App {
        let project = captured()[0].project().unwrap().to_owned();
        App::new(Declared::bare(project))
    }

    fn snapshot(containers: Vec<ObservedContainer>, logs: Option<Logs>) -> Update {
        Update::Snapshot(Snapshot {
            at_secs: 0,
            containers: Ok(containers),
            cache: None,
            logs,
        })
    }

    #[test]
    fn the_first_poll_selects_the_first_row_and_asks_for_its_logs() {
        let mut app = app();
        let request = app.apply(snapshot(captured(), None));
        assert_eq!(request, Some(Request::Select(Some("api".to_owned()))));
        assert!(app.events.is_empty(), "the first poll is the baseline");
    }

    #[test]
    fn moving_down_selects_the_next_service_and_clears_its_logs() {
        let mut app = app();
        app.apply(snapshot(
            captured(),
            Some(Logs {
                service: "api".to_owned(),
                lines: vec!["capture-hello".to_owned()],
                error: None,
            }),
        ));
        let request = app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(request, Some(Request::Select(Some("worker".to_owned()))));
        assert!(app.logs.is_none());

        // Already on the last row: nothing to ask for.
        assert_eq!(app.on_key(KeyEvent::from(KeyCode::Down)), None);
    }

    #[test]
    fn a_runtime_error_keeps_the_last_rows() {
        let mut app = app();
        app.apply(snapshot(captured(), None));
        app.apply(Update::Snapshot(Snapshot {
            at_secs: 1,
            containers: Err("`container list` exited with 1".to_owned()),
            cache: None,
            logs: None,
        }));
        assert_eq!(app.rows.len(), 2);
        assert!(app.runtime_error.is_some());
    }

    #[test]
    fn a_container_that_disappears_is_an_event() {
        let mut app = app();
        app.apply(snapshot(captured(), None));
        let only_api: Vec<ObservedContainer> = captured()
            .into_iter()
            .filter(|container| container.service() == Some("api"))
            .collect();
        app.apply(snapshot(only_api, None));
        let texts: Vec<String> = app
            .events
            .iter()
            .map(|event| format!("{} {}", event.service, event.text))
            .collect();
        assert_eq!(texts, ["worker removed"]);
    }

    #[test]
    fn q_quits() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(app.quit);
    }
}
