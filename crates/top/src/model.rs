//! The view model: what one poll of the runtime and the health cache means,
//! as rows a table can draw and events a reader can scan.
//!
//! Pure. Containers, the cache and the clock are parameters, so every rule
//! here is a unit test rather than a terminal session.

use std::collections::BTreeMap;

use container_compose_runtime::ObservedContainer;
use container_compose_stack::resolve::ResolvedStack;
use container_compose_supervisor::cache::HealthCache;
use container_compose_supervisor::clock;

/// What the stack file declares. `top` runs without one, as `ps` does, so an
/// interrupted session stays inspectable; then only the project is known.
#[derive(Debug, Clone, Default)]
pub struct Declared {
    pub project: String,
    /// Start order. Rows follow it; anything undeclared sorts after, by name.
    pub order: Vec<String>,
    pub services: BTreeMap<String, DeclaredService>,
    /// The stack's config hash, so a cache from a different config is ignored.
    pub config_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredService {
    pub ports: Vec<String>,
    pub restart: String,
    pub has_healthcheck: bool,
}

impl Declared {
    pub fn from_stack(stack: &ResolvedStack) -> Self {
        Self {
            project: stack.name.clone(),
            order: stack.order.clone(),
            services: stack
                .services
                .iter()
                .map(|(name, service)| {
                    (
                        name.clone(),
                        DeclaredService {
                            ports: service.ports.iter().map(|port| port.display()).collect(),
                            restart: service.restart.as_str().to_owned(),
                            has_healthcheck: service.healthcheck.is_some(),
                        },
                    )
                })
                .collect(),
            config_hash: Some(stack.config_hash.clone()),
        }
    }

    pub fn bare(project: String) -> Self {
        Self {
            project,
            ..Self::default()
        }
    }
}

/// The state shown for a declared service that has no container.
pub const NOT_CREATED: &str = "not created";
/// Health of a service that is not running: there is nothing to be healthy.
pub const NO_HEALTH: &str = "—";
/// Health of a running service that declares no healthcheck.
pub const NO_CHECK: &str = "no check";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub service: String,
    pub container: Option<String>,
    /// The runtime's word (`running`, `stopped`), or [`NOT_CREATED`].
    pub state: String,
    pub health: String,
    pub uptime: Option<String>,
    pub restarts: u32,
    pub gave_up: bool,
    pub policy: Option<String>,
    pub ports: Vec<String>,
    pub image: Option<String>,
}

impl Row {
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }
}

/// One row per declared service, in start order, then one per undeclared
/// container of the project. Only a cache that [`cache::live`] trusted
/// should be passed: health and restarts come from it alone.
///
/// [`cache::live`]: container_compose_supervisor::cache::live
pub fn rows(
    declared: &Declared,
    containers: &[ObservedContainer],
    cache: Option<&HealthCache>,
    now_secs: u64,
) -> Vec<Row> {
    let mine: Vec<&ObservedContainer> = containers
        .iter()
        .filter(|container| container.project() == Some(declared.project.as_str()))
        .collect();

    let mut rows: Vec<Row> = declared
        .order
        .iter()
        .map(|service| {
            let container = mine
                .iter()
                .copied()
                .find(|container| container.service() == Some(service.as_str()));
            row(
                service,
                container,
                declared.services.get(service),
                cache,
                now_secs,
            )
        })
        .collect();

    let mut undeclared: Vec<Row> = mine
        .iter()
        .copied()
        .filter(|container| {
            container
                .service()
                .is_none_or(|service| !declared.order.iter().any(|known| known == service))
        })
        .map(|container| {
            let service = container.service().unwrap_or(&container.id).to_owned();
            row(&service, Some(container), None, cache, now_secs)
        })
        .collect();
    undeclared.sort_by(|a, b| a.service.cmp(&b.service));
    rows.extend(undeclared);
    rows
}

fn row(
    service: &str,
    container: Option<&ObservedContainer>,
    declared: Option<&DeclaredService>,
    cache: Option<&HealthCache>,
    now_secs: u64,
) -> Row {
    let cached = cache.and_then(|cache| cache.services.get(service));
    let running = container.is_some_and(ObservedContainer::is_running);

    let health = if !running {
        NO_HEALTH.to_owned()
    } else if declared.is_some_and(|service| !service.has_healthcheck) {
        NO_CHECK.to_owned()
    } else {
        cached.map_or_else(|| "unknown".to_owned(), |cached| cached.state.clone())
    };

    let uptime = container
        .filter(|container| container.is_running())
        .and_then(|container| container.status.started_date.as_deref())
        .and_then(clock::parse_rfc3339_seconds)
        .map(|started| format_span(now_secs.saturating_sub(started)));

    Row {
        service: service.to_owned(),
        container: container.map(|container| container.id.clone()),
        state: container.map_or_else(
            || NOT_CREATED.to_owned(),
            |container| container.status.state.clone(),
        ),
        health,
        uptime,
        restarts: cached.map_or(0, |cached| cached.restarts),
        gave_up: cached.is_some_and(|cached| cached.gave_up),
        policy: declared.map(|service| service.restart.clone()),
        ports: declared
            .map(|service| service.ports.clone())
            .unwrap_or_default(),
        image: container.map(|container| container.configuration.image.reference.clone()),
    }
}

/// `45s`, `12m 03s`, `5h 07m`, `3d 04h`: two units at most, which is all a
/// glance at uptime needs.
pub fn format_span(seconds: u64) -> String {
    let (days, hours, minutes, secs) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
        seconds % 60,
    );
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// `HH:MM:SSZ`, UTC, the clock the cache and the runtime both use.
    pub at: String,
    pub service: String,
    pub text: String,
}

/// What changed between two polls, matched by service. Uptime ticking is not
/// an event; a state, a health, a restart or a give-up is.
pub fn events(previous: &[Row], next: &[Row], now_secs: u64) -> Vec<Event> {
    let at = clock_of_day(now_secs);
    let mut events = Vec::new();
    for row in next {
        let event = |text: String| Event {
            at: at.clone(),
            service: row.service.clone(),
            text,
        };
        let Some(before) = previous.iter().find(|before| before.service == row.service) else {
            events.push(event(format!("appeared, {}", row.state)));
            continue;
        };
        if before.state != row.state {
            events.push(event(format!("{} → {}", before.state, row.state)));
        } else if before.health != row.health && row.is_running() {
            events.push(event(format!("health {} → {}", before.health, row.health)));
        }
        if row.restarts > before.restarts {
            events.push(event(format!("restarted, {} so far", row.restarts)));
        }
        if row.gave_up && !before.gave_up {
            events.push(event("supervisor gave up restarting it".to_owned()));
        }
    }
    for before in previous {
        if !next.iter().any(|row| row.service == before.service) {
            events.push(Event {
                at: at.clone(),
                service: before.service.clone(),
                text: "removed".to_owned(),
            });
        }
    }
    events
}

/// `HH:MM:SSZ` from epoch seconds.
pub fn clock_of_day(seconds: u64) -> String {
    let stamp = clock::format_rfc3339_seconds(seconds);
    // `YYYY-MM-DDTHH:MM:SSZ`; the time of day is everything after the `T`.
    stamp
        .split_once('T')
        .map_or(stamp.clone(), |(_, time)| time.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        events, format_span, rows, Declared, DeclaredService, Row, NOT_CREATED, NO_CHECK, NO_HEALTH,
    };
    use container_compose_runtime::ObservedContainer;
    use container_compose_supervisor::cache::{CachedHealth, HealthCache};
    use container_compose_supervisor::clock;
    use std::collections::BTreeMap;

    /// The `container list --all --format json` response captured from the
    /// real runtime for the test suite's fixtures.
    fn captured() -> Vec<ObservedContainer> {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/real/container-list.json"
        ))
        .expect("the captured container list");
        serde_json::from_str(&text).expect("it parses as the observed shape")
    }

    /// The captured project and its two services: `api` running, `worker`
    /// stopped. `api` declares a healthcheck; `worker` does not.
    fn declared(with: &[&str]) -> Declared {
        let project = captured()[0].project().unwrap().to_owned();
        let mut order: Vec<String> = vec!["api".to_owned(), "worker".to_owned()];
        order.extend(with.iter().map(|name| (*name).to_owned()));
        Declared {
            project,
            services: order
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        DeclaredService {
                            ports: Vec::new(),
                            restart: "no".to_owned(),
                            has_healthcheck: name == "api",
                        },
                    )
                })
                .collect(),
            order,
            config_hash: None,
        }
    }

    fn started_at_of_api() -> u64 {
        let api = captured()
            .into_iter()
            .find(|container| container.service() == Some("api"))
            .unwrap();
        clock::parse_rfc3339_seconds(api.status.started_date.as_deref().unwrap()).unwrap()
    }

    fn cache(project: &str, api: CachedHealth) -> HealthCache {
        HealthCache {
            pid: std::process::id(),
            project: project.to_owned(),
            config: "unused".to_owned(),
            updated_at: "2026-10-24T04:26:00Z".to_owned(),
            services: BTreeMap::from([("api".to_owned(), api)]),
        }
    }

    fn healthy(restarts: u32, gave_up: bool) -> CachedHealth {
        CachedHealth {
            state: "healthy".to_owned(),
            checked_at: None,
            restarts,
            last_restart_at: None,
            gave_up,
        }
    }

    #[test]
    fn declared_services_come_first_in_start_order() {
        let rows = rows(&declared(&["db"]), &captured(), None, started_at_of_api());
        let services: Vec<&str> = rows.iter().map(|row| row.service.as_str()).collect();
        assert_eq!(services, ["api", "worker", "db"]);
        assert_eq!(rows[2].state, NOT_CREATED);
        assert_eq!(rows[2].container, None);
    }

    #[test]
    fn health_comes_from_the_cache_only_while_running() {
        let declared = declared(&[]);
        let now = started_at_of_api() + 125;
        let cache = cache(&declared.project, healthy(2, false));
        let rows = rows(&declared, &captured(), Some(&cache), now);

        assert_eq!(rows[0].state, "running");
        assert_eq!(rows[0].health, "healthy");
        assert_eq!(rows[0].uptime.as_deref(), Some("2m 05s"));
        assert_eq!(rows[0].restarts, 2);

        assert_eq!(rows[1].state, "stopped");
        assert_eq!(rows[1].health, NO_HEALTH);
        assert_eq!(rows[1].uptime, None);
    }

    #[test]
    fn a_running_service_without_a_cache_is_unknown_unless_it_has_no_check() {
        let declared = declared(&[]);
        let rows = rows(&declared, &captured(), None, started_at_of_api());
        assert_eq!(rows[0].health, "unknown");

        let mut unchecked = declared.clone();
        unchecked.services.get_mut("api").unwrap().has_healthcheck = false;
        let rows = super::rows(&unchecked, &captured(), None, started_at_of_api());
        assert_eq!(rows[0].health, NO_CHECK);
    }

    #[test]
    fn without_a_stack_file_every_project_container_is_a_row() {
        let project = captured()[0].project().unwrap().to_owned();
        let rows = rows(
            &Declared::bare(project),
            &captured(),
            None,
            started_at_of_api(),
        );
        let services: Vec<&str> = rows.iter().map(|row| row.service.as_str()).collect();
        assert_eq!(services, ["api", "worker"]);
        assert_eq!(rows[0].policy, None);
    }

    #[test]
    fn another_projects_containers_are_not_rows() {
        let rows = rows(
            &Declared::bare("someone-else".to_owned()),
            &captured(),
            None,
            0,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn spans_use_two_units_at_most() {
        assert_eq!(format_span(45), "45s");
        assert_eq!(format_span(723), "12m 03s");
        assert_eq!(format_span(18_420), "5h 07m");
        assert_eq!(format_span(273_600), "3d 04h");
    }

    fn base() -> Row {
        Row {
            service: "indexer".to_owned(),
            container: Some("lab-indexer".to_owned()),
            state: "running".to_owned(),
            health: "starting".to_owned(),
            uptime: Some("5s".to_owned()),
            restarts: 0,
            gave_up: false,
            policy: Some("on-stop".to_owned()),
            ports: Vec::new(),
            image: None,
        }
    }

    #[test]
    fn events_name_state_health_restart_and_give_up_changes() {
        let before = base();
        let now = clock::parse_rfc3339_seconds("2026-10-26T01:18:08Z").unwrap();

        let healthy = Row {
            health: "healthy".to_owned(),
            uptime: Some("10s".to_owned()),
            ..before.clone()
        };
        let texts = |next: &Row| -> Vec<String> {
            events(
                std::slice::from_ref(&before),
                std::slice::from_ref(next),
                now,
            )
            .into_iter()
            .map(|event| event.text)
            .collect()
        };
        assert_eq!(texts(&healthy), ["health starting → healthy"]);

        let stopped = Row {
            state: "stopped".to_owned(),
            health: NO_HEALTH.to_owned(),
            ..before.clone()
        };
        assert_eq!(texts(&stopped), ["running → stopped"]);

        let restarted = Row {
            restarts: 1,
            ..before.clone()
        };
        assert_eq!(texts(&restarted), ["restarted, 1 so far"]);

        let abandoned = Row {
            state: "stopped".to_owned(),
            health: NO_HEALTH.to_owned(),
            gave_up: true,
            ..before.clone()
        };
        assert_eq!(
            texts(&abandoned),
            ["running → stopped", "supervisor gave up restarting it"]
        );

        let event = &events(std::slice::from_ref(&before), &[healthy], now)[0];
        assert_eq!(event.at, "01:18:08Z");
        assert_eq!(event.service, "indexer");
    }

    #[test]
    fn uptime_ticking_is_not_an_event() {
        let before = base();
        let later = Row {
            uptime: Some("6s".to_owned()),
            ..before.clone()
        };
        assert!(events(&[before], &[later], 0).is_empty());
    }
}
