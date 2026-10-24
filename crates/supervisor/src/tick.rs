//! The supervision state machine.
//!
//! Pure by construction: `tick` takes the monotonic clock, the wall clock and
//! the last observation, and returns the actions to perform. A crash loop is
//! tested by advancing a number, not by sleeping. The loop in [`crate::run`] is
//! the only place that starts a container or runs a probe.

use std::collections::BTreeMap;

use container_compose_stack::model::RestartPolicy;
use container_compose_stack::resolve::{ResolvedHealthcheck, ResolvedService};

/// A container that stays up this long resets its restart count.
const STABLE_AFTER_MS: u64 = 60_000;

/// How long a restart may take to show `running` before it is a failed attempt.
/// The runtime's own start is asynchronous and took two seconds in the probe.
const RESTART_GRACE_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Starting,
    Healthy,
    Unhealthy,
    Unknown,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Starting => "starting",
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
            Health::Unknown => "unknown",
        }
    }
}

/// What the supervisor needs to know about one service.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub name: String,
    pub restart: RestartPolicy,
    pub max_retries: u32,
    pub healthcheck: Option<ResolvedHealthcheck>,
}

impl ServiceSpec {
    pub fn from_service(service: &ResolvedService) -> Self {
        Self {
            name: service.name.clone(),
            restart: service.restart,
            max_retries: service.max_retries,
            healthcheck: service.healthcheck.clone(),
        }
    }
}

/// The runtime's answer for one container, reduced to what the policy uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct View {
    pub running: bool,
    /// Wall-clock milliseconds from `status.startedDate`, when present.
    pub started_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The policy decided to restart, and when.
    Schedule {
        service: String,
        attempt: u32,
        delay_ms: u128,
    },
    /// The backoff elapsed; run `start` now.
    Restart { service: String, attempt: u32 },
    /// `on-stop` reached `max_retries`.
    GiveUp { service: String, attempts: u32 },
    /// A healthcheck is due.
    Probe { service: String, timeout_ms: u128 },
}

/// The result of one probe, with the runtime's three outcomes kept distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    Healthy,
    Failed,
    /// The container was not running; it counts for nothing.
    Unknown,
}

#[derive(Debug, Default)]
pub struct Update {
    pub actions: Vec<Action>,
    /// Services whose reported health changed this tick.
    pub changed: Vec<(String, Health)>,
}

pub struct Supervisor {
    services: BTreeMap<String, State>,
    /// The last health reported to the outside world, for change detection.
    reported: BTreeMap<String, Health>,
}

struct State {
    spec: ServiceSpec,
    running: bool,
    /// Monotonic time the container is deemed to have started. Seeded from
    /// `startedDate`, so an adopted container's age is not lost.
    running_since: Option<u64>,
    /// The supervisor asked for this stop; do not restart on the transition.
    intent: bool,
    attempts: u32,
    retry_at: Option<u64>,
    /// A restart was issued and has not been seen running yet; holds the
    /// monotonic deadline after which the attempt counts as failed.
    awaiting_start: Option<u64>,
    gave_up: bool,
    health: Health,
    failures: u32,
    next_probe: u64,
    checked_at: Option<u64>,
}

impl Supervisor {
    pub fn new(specs: Vec<ServiceSpec>) -> Self {
        let services: BTreeMap<String, State> = specs
            .into_iter()
            .map(|spec| {
                let name = spec.name.clone();
                (name, State::new(spec))
            })
            .collect();
        let reported = services
            .keys()
            .map(|name| (name.clone(), Health::Unknown))
            .collect();
        Self { services, reported }
    }

    pub fn health(&self, service: &str) -> Health {
        self.services
            .get(service)
            .map(|state| state.health)
            .unwrap_or(Health::Unknown)
    }

    pub fn checked_at(&self, service: &str) -> Option<u64> {
        self.services
            .get(service)
            .and_then(|state| state.checked_at)
    }

    pub fn health_snapshot(&self) -> BTreeMap<String, Health> {
        self.services
            .iter()
            .map(|(name, state)| (name.clone(), state.health))
            .collect()
    }

    /// Record a stop the supervisor is about to issue. The next observation of
    /// `stopped` is then expected and never triggers a restart.
    pub fn note_stop(&mut self, service: &str) {
        if let Some(state) = self.services.get_mut(service) {
            state.intent = true;
        }
    }

    pub fn tick(&mut self, now_ms: u64, wall_ms: u64, views: &BTreeMap<String, View>) -> Update {
        let mut update = Update::default();
        for (name, state) in &mut self.services {
            let view = views.get(name).copied().unwrap_or_default();
            state.observe(now_ms, wall_ms, view, &mut update.actions);
        }
        self.collect_health(&mut update.changed);
        update
    }

    /// Feed a probe result back. Returns the new health when it changed, so the
    /// loop writes the cache only on a transition.
    pub fn record_probe(
        &mut self,
        now_ms: u64,
        wall_ms: u64,
        service: &str,
        outcome: ProbeOutcome,
    ) -> Option<Health> {
        let state = self.services.get_mut(service)?;
        state.record_probe(now_ms, wall_ms, outcome);
        let health = state.health;
        let previous = self.reported.insert(service.to_owned(), health);
        if previous == Some(health) {
            None
        } else {
            Some(health)
        }
    }

    fn collect_health(&mut self, changed: &mut Vec<(String, Health)>) {
        let transitions: Vec<(String, Health)> = self
            .services
            .iter()
            .filter(|(name, state)| self.reported.get(*name) != Some(&state.health))
            .map(|(name, state)| (name.clone(), state.health))
            .collect();
        for (name, health) in transitions {
            self.reported.insert(name.clone(), health);
            changed.push((name, health));
        }
    }
}

impl State {
    fn new(spec: ServiceSpec) -> Self {
        Self {
            spec,
            running: false,
            running_since: None,
            intent: false,
            attempts: 0,
            retry_at: None,
            awaiting_start: None,
            gave_up: false,
            health: Health::Unknown,
            failures: 0,
            next_probe: 0,
            checked_at: None,
        }
    }

    fn observe(&mut self, now: u64, wall: u64, view: View, actions: &mut Vec<Action>) {
        if view.running && !self.running {
            self.started(now, wall, view.started_at_ms);
        } else if !view.running && self.running {
            self.stopped(now, actions);
        }

        // A container that has stayed up long enough forgets its crash history,
        // including a give-up. This is the 60 s reset.
        if view.running {
            if let Some(since) = self.running_since {
                if now.saturating_sub(since) >= STABLE_AFTER_MS {
                    self.attempts = 0;
                    self.gave_up = false;
                }
            }
        }

        if !self.running {
            if let Some(deadline) = self.awaiting_start {
                if now < deadline {
                    return;
                }
                // The start never produced a running container. It counts as
                // the next failed attempt, so a crash loop still backs off.
                self.awaiting_start = None;
                self.stopped(now, actions);
                return;
            }
            if let Some(at) = self.retry_at {
                if now >= at {
                    self.retry_at = None;
                    self.awaiting_start = Some(now.saturating_add(RESTART_GRACE_MS));
                    actions.push(Action::Restart {
                        service: self.spec.name.clone(),
                        attempt: self.attempts,
                    });
                }
            }
            return;
        }

        if let Some(check) = &self.spec.healthcheck {
            if now >= self.next_probe {
                self.next_probe = now.saturating_add(millis(check.interval.millis()));
                actions.push(Action::Probe {
                    service: self.spec.name.clone(),
                    timeout_ms: check.timeout.millis(),
                });
            }
            if self.in_start_period(now) && self.health != Health::Healthy {
                self.health = Health::Starting;
            }
        }
    }

    fn started(&mut self, now: u64, wall: u64, started_at_ms: Option<u64>) {
        self.running = true;
        self.intent = false;
        self.retry_at = None;
        self.awaiting_start = None;
        self.failures = 0;
        self.health = if self.spec.healthcheck.is_some() {
            Health::Starting
        } else {
            Health::Unknown
        };
        let age = started_at_ms
            .map(|started| wall.saturating_sub(started))
            .unwrap_or(0);
        self.running_since = Some(now.saturating_sub(age));
        self.next_probe = now;
    }

    fn stopped(&mut self, now: u64, actions: &mut Vec<Action>) {
        self.running = false;
        self.running_since = None;
        self.health = Health::Unknown;
        self.failures = 0;

        if self.intent {
            self.intent = false;
            return;
        }
        match self.spec.restart {
            RestartPolicy::No => {}
            RestartPolicy::OnStop if self.attempts >= self.spec.max_retries => {
                self.gave_up = true;
                actions.push(Action::GiveUp {
                    service: self.spec.name.clone(),
                    attempts: self.attempts,
                });
            }
            RestartPolicy::OnStop | RestartPolicy::Always => {
                self.attempts += 1;
                let delay_ms = backoff_ms(self.attempts);
                self.retry_at = Some(now.saturating_add(millis(delay_ms)));
                actions.push(Action::Schedule {
                    service: self.spec.name.clone(),
                    attempt: self.attempts,
                    delay_ms,
                });
            }
        }
    }

    fn in_start_period(&self, now: u64) -> bool {
        let Some(check) = &self.spec.healthcheck else {
            return false;
        };
        let Some(since) = self.running_since else {
            return false;
        };
        now < since.saturating_add(millis(check.start_period.millis()))
    }

    fn record_probe(&mut self, now: u64, wall: u64, outcome: ProbeOutcome) {
        self.checked_at = Some(wall);
        match outcome {
            ProbeOutcome::Healthy => {
                self.failures = 0;
                self.health = Health::Healthy;
            }
            ProbeOutcome::Unknown => {
                self.health = Health::Unknown;
            }
            ProbeOutcome::Failed => {
                if self.in_start_period(now) {
                    self.health = Health::Starting;
                } else {
                    self.failures += 1;
                    let retries = self
                        .spec
                        .healthcheck
                        .as_ref()
                        .map(|check| check.retries)
                        .unwrap_or(1);
                    if self.failures >= retries {
                        self.health = Health::Unhealthy;
                    }
                }
            }
        }
    }
}

/// `1s, 2s, 4s, 8s, 16s, 30s, 30s, …`.
pub fn backoff_ms(attempt: u32) -> u128 {
    let shift = attempt.saturating_sub(1).min(5);
    let seconds = 1u64.checked_shl(shift).unwrap_or(30).min(30);
    u128::from(seconds) * 1_000
}

fn millis(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Action, Health, ProbeOutcome, ServiceSpec, Supervisor, Update, View};
    use container_compose_stack::duration::Span;
    use container_compose_stack::model::RestartPolicy;
    use container_compose_stack::resolve::ResolvedHealthcheck;
    use std::collections::BTreeMap;

    fn spec(name: &str, restart: RestartPolicy, max_retries: u32) -> ServiceSpec {
        ServiceSpec {
            name: name.to_owned(),
            restart,
            max_retries,
            healthcheck: None,
        }
    }

    fn healthcheck(interval: &str, start_period: &str, retries: u32) -> ResolvedHealthcheck {
        ResolvedHealthcheck {
            exec: vec!["true".to_owned()],
            interval: Span::parse(interval).unwrap(),
            timeout: Span::parse("1s").unwrap(),
            start_period: Span::parse(start_period).unwrap(),
            retries,
        }
    }

    fn views(running: bool) -> BTreeMap<String, View> {
        let mut views = BTreeMap::new();
        views.insert(
            "api".to_owned(),
            View {
                running,
                started_at_ms: None,
            },
        );
        views
    }

    fn schedules(update: &Update) -> Vec<(u32, u128)> {
        update
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::Schedule {
                    attempt, delay_ms, ..
                } => Some((*attempt, *delay_ms)),
                _ => None,
            })
            .collect()
    }

    fn restarts(update: &Update) -> usize {
        update
            .actions
            .iter()
            .filter(|action| matches!(action, Action::Restart { .. }))
            .count()
    }

    #[test]
    fn backoff_is_one_two_four_and_caps_at_thirty() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::Always, 5)]);
        let mut now = 0;
        supervisor.tick(now, now, &views(true));

        let mut delays = Vec::new();
        for _ in 0..7 {
            now += 1_000;
            let update = supervisor.tick(now, now, &views(false));
            let scheduled = schedules(&update);
            assert_eq!(scheduled.len(), 1, "{update:?}");
            delays.push(scheduled[0].1);

            now += scheduled[0].1 as u64;
            let update = supervisor.tick(now, now, &views(false));
            assert_eq!(
                restarts(&update),
                1,
                "the scheduled restart is due: {update:?}"
            );

            let update = supervisor.tick(now, now, &views(true));
            assert!(schedules(&update).is_empty(), "a start is not another stop");
        }

        assert_eq!(delays, [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]);
    }

    #[test]
    fn on_stop_gives_up_after_max_retries() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::OnStop, 2)]);
        let mut now = 0;
        supervisor.tick(now, now, &views(true));

        for expected in [1, 2] {
            now += 1_000;
            let update = supervisor.tick(now, now, &views(false));
            assert_eq!(
                schedules(&update),
                vec![(expected, 1_000 * u128::from(expected))]
            );
            now += 1_000 * u64::from(expected);
            supervisor.tick(now, now, &views(false));
            supervisor.tick(now, now, &views(true));
        }

        now += 1_000;
        let update = supervisor.tick(now, now, &views(false));
        assert!(
            update
                .actions
                .iter()
                .any(|action| matches!(action, Action::GiveUp { attempts: 2, .. })),
            "{update:?}"
        );
        assert!(schedules(&update).is_empty());
    }

    #[test]
    fn a_restart_that_never_leads_to_running_is_retried() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::Always, 5)]);
        let mut now = 0;
        supervisor.tick(now, now, &views(true));

        now += 1_000;
        assert_eq!(
            schedules(&supervisor.tick(now, now, &views(false))),
            [(1, 1_000)]
        );
        now += 1_000;
        assert_eq!(restarts(&supervisor.tick(now, now, &views(false))), 1);

        // Still stopped inside the grace: the start may simply be slow.
        now += 1_000;
        assert!(supervisor.tick(now, now, &views(false)).actions.is_empty());

        // The grace expired, so the attempt failed and the next one backs off.
        now += 5_000;
        assert_eq!(
            schedules(&supervisor.tick(now, now, &views(false))),
            [(2, 2_000)]
        );
    }

    #[test]
    fn a_stop_the_supervisor_issued_is_not_restarted() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::Always, 5)]);
        supervisor.tick(0, 0, &views(true));
        supervisor.note_stop("api");

        let update = supervisor.tick(1_000, 1_000, &views(false));
        assert!(update.actions.is_empty(), "{update:?}");
        assert_eq!(supervisor.health("api"), Health::Unknown);
    }

    #[test]
    fn sixty_seconds_up_resets_the_backoff() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::OnStop, 5)]);
        let mut now = 0;
        supervisor.tick(now, now, &views(true));

        now += 1_000;
        assert_eq!(
            schedules(&supervisor.tick(now, now, &views(false))),
            [(1, 1_000)]
        );
        now += 1_000;
        supervisor.tick(now, now, &views(false));
        supervisor.tick(now, now, &views(true));

        now += 60_001;
        supervisor.tick(now, now, &views(true));
        now += 1_000;
        assert_eq!(
            schedules(&supervisor.tick(now, now, &views(false))),
            [(1, 1_000)],
            "the attempt count was reset"
        );
    }

    #[test]
    fn a_started_container_is_probed_immediately_and_then_on_interval() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::No, 5)]);
        // Give the service a healthcheck. `spec` above is healthcheck-free.
        supervisor.services.get_mut("api").unwrap().spec.healthcheck =
            Some(healthcheck("10s", "0s", 3));

        let update = supervisor.tick(0, 0, &views(true));
        assert!(update
            .actions
            .iter()
            .any(|action| matches!(action, Action::Probe { .. })));

        let update = supervisor.tick(5_000, 5_000, &views(true));
        assert!(!update
            .actions
            .iter()
            .any(|action| matches!(action, Action::Probe { .. })));

        let update = supervisor.tick(10_000, 10_000, &views(true));
        assert!(update
            .actions
            .iter()
            .any(|action| matches!(action, Action::Probe { .. })));
    }

    #[test]
    fn failures_in_the_start_period_are_starting_not_unhealthy() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::No, 5)]);
        supervisor.services.get_mut("api").unwrap().spec.healthcheck =
            Some(healthcheck("1s", "10s", 2));
        supervisor.tick(0, 0, &views(true));

        for at in [1_000, 2_000, 9_000] {
            supervisor.tick(at, at, &views(true));
            assert_eq!(
                supervisor.record_probe(at, at, "api", ProbeOutcome::Failed),
                None,
                "the state did not change from starting"
            );
            assert_eq!(supervisor.health("api"), Health::Starting);
        }

        supervisor.tick(10_000, 10_000, &views(true));
        assert_eq!(
            supervisor.record_probe(10_000, 10_000, "api", ProbeOutcome::Failed),
            None,
            "the first failure after the period is not yet the retry limit"
        );
        assert_eq!(supervisor.health("api"), Health::Starting);
        assert_eq!(
            supervisor.record_probe(11_000, 11_000, "api", ProbeOutcome::Failed),
            Some(Health::Unhealthy)
        );
    }

    #[test]
    fn a_healthy_probe_clears_the_failure_count() {
        let mut supervisor = Supervisor::new(vec![spec("api", RestartPolicy::No, 5)]);
        supervisor.services.get_mut("api").unwrap().spec.healthcheck =
            Some(healthcheck("1s", "0s", 3));
        supervisor.tick(0, 0, &views(true));
        supervisor.record_probe(1_000, 1_000, "api", ProbeOutcome::Failed);
        supervisor.record_probe(2_000, 2_000, "api", ProbeOutcome::Failed);
        assert_eq!(
            supervisor.record_probe(3_000, 3_000, "api", ProbeOutcome::Healthy),
            Some(Health::Healthy)
        );
        supervisor.record_probe(4_000, 4_000, "api", ProbeOutcome::Failed);
        assert_eq!(supervisor.health("api"), Health::Healthy);
    }
}
