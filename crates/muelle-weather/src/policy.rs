use crate::{Coordinates, Error, Forecast};
use std::time::{Duration, Instant};

const NORMAL: Duration = Duration::from_secs(30 * 60);
const SAVER: Duration = Duration::from_secs(60 * 60);
const MAX_AGE: Duration = Duration::from_secs(2 * 60 * 60);

/// Supply existing application visibility/network/power state; do not poll
/// Windows or construct another monitor just for this scheduler.
#[derive(Clone, Copy, Default)]
pub struct Demand {
    pub visible: bool,
    pub online: bool,
    pub energy_saver: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    generation: u64,
    serial: u64,
    coordinates: Coordinates,
}
impl Ticket {
    pub fn coordinates(self) -> Coordinates {
        self.coordinates
    }
}

pub struct Cached<'a> {
    pub forecast: &'a Forecast,
    pub age: Duration,
    pub stale: bool,
}

/// Single-city memory cache and scheduling only: no timer, worker, DNS, disk
/// or HTTP client. Configure off by default; a future controller owns actual I/O.
#[derive(Default)]
pub struct Refresh {
    enabled: bool,
    coordinates: Option<Coordinates>,
    generation: u64,
    serial: u64,
    flight: Option<Ticket>,
    cached: Option<(Forecast, Instant)>,
    retry_at: Option<Instant>,
    rate_limited_until: Option<Instant>,
    failures: u8,
}

impl Refresh {
    pub fn configure(&mut self, enabled: bool, coordinates: Option<Coordinates>) {
        if self.enabled == enabled && self.coordinates == coordinates {
            return;
        }
        self.enabled = enabled;
        self.coordinates = coordinates;
        self.generation = self.generation.wrapping_add(1);
        self.cached = None;
        self.retry_at = None;
        self.failures = 0;
        // Keep a stale flight until its bounded worker completes/cancels:
        // toggling or switching location must not spawn overlapping requests.
    }

    pub fn next_deadline(&self, now: Instant, demand: Demand) -> Option<Instant> {
        if !self.enabled
            || self.coordinates.is_none()
            || self.flight.is_some()
            || !demand.visible
            || !demand.online
        {
            return None;
        }
        let interval = if demand.energy_saver { SAVER } else { NORMAL };
        let freshness = self
            .cached
            .as_ref()
            .map_or(now, |(_, fetched)| *fetched + interval);
        Some(
            freshness
                .max(self.retry_at.unwrap_or(now))
                .max(self.rate_limited_until.unwrap_or(now))
                .max(now),
        )
    }

    pub fn begin(&mut self, now: Instant, demand: Demand) -> Option<Ticket> {
        if self.next_deadline(now, demand)? > now {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let ticket = Ticket {
            generation: self.generation,
            serial: self.serial,
            coordinates: self.coordinates?,
        };
        self.flight = Some(ticket);
        Some(ticket)
    }

    /// False means an outdated/duplicate result was rejected. Failed requests
    /// keep a bounded stale snapshot; they never replace real data with zeros.
    pub fn finish(
        &mut self,
        ticket: Ticket,
        result: Result<Forecast, Error>,
        now: Instant,
    ) -> bool {
        if self.flight != Some(ticket) {
            return false;
        }
        self.flight = None;
        // A provider-wide limit survives a city change or off/on toggle, even
        // when the old request's data no longer belongs to the current city.
        if let Err(Error::RateLimited { retry_after }) = &result {
            let until = now
                + (*retry_after).clamp(Duration::from_secs(300), Duration::from_secs(24 * 3600));
            self.rate_limited_until =
                Some(self.rate_limited_until.map_or(until, |old| old.max(until)));
        }
        if !self.enabled || ticket.generation != self.generation {
            return false;
        }
        match result {
            Ok(forecast) => {
                self.cached = Some((forecast, now));
                self.failures = 0;
                self.retry_at = None;
            }
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                let exponential = Duration::from_secs(
                    (300_u64 << self.failures.min(5).saturating_sub(1)).min(3600),
                );
                let delay = match error {
                    Error::RateLimited { retry_after } => {
                        exponential.max(retry_after.min(Duration::from_secs(24 * 3600)))
                    }
                    _ => exponential,
                };
                self.retry_at = Some(now + delay);
            }
        }
        true
    }

    pub fn cached(&self, now: Instant) -> Option<Cached<'_>> {
        if !self.enabled {
            return None;
        }
        let (forecast, fetched) = self.cached.as_ref()?;
        let age = now.saturating_duration_since(*fetched);
        (age <= MAX_AGE).then_some(Cached {
            forecast,
            age,
            stale: age >= NORMAL,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demand() -> Demand {
        Demand {
            visible: true,
            online: true,
            energy_saver: false,
        }
    }
    fn coordinates() -> Coordinates {
        Coordinates::new(38.72, -9.14).unwrap_or_else(|_| unreachable!())
    }
    fn forecast() -> Forecast {
        crate::decode(include_bytes!("../tests/forecast.json"))
            .unwrap_or_else(|e| panic!("fixture: {e}"))
    }

    #[test]
    fn disabled_unlocated_hidden_and_offline_have_no_deadline_or_requests() {
        let now = Instant::now();
        let mut state = Refresh::default();
        assert!(state.begin(now, demand()).is_none());
        state.configure(true, None);
        assert!(state.next_deadline(now, demand()).is_none());
        state.configure(true, Some(coordinates()));
        assert!(
            state
                .begin(
                    now,
                    Demand {
                        visible: false,
                        ..demand()
                    }
                )
                .is_none()
        );
        assert!(
            state
                .begin(
                    now,
                    Demand {
                        online: false,
                        ..demand()
                    }
                )
                .is_none()
        );
        assert!(state.begin(now, demand()).is_some());
    }

    #[test]
    fn repeated_demand_coalesces_and_visibility_does_not_force_another_fetch() {
        let now = Instant::now();
        let mut state = Refresh::default();
        state.configure(true, Some(coordinates()));
        let ticket = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
        for _ in 0..100 {
            assert!(state.begin(now, demand()).is_none());
        }
        assert!(state.finish(ticket, Ok(forecast()), now));
        assert!(
            state
                .begin(now + Duration::from_secs(1700), demand())
                .is_none()
        );
        assert_eq!(state.next_deadline(now, demand()), Some(now + NORMAL));
        assert_eq!(
            state.next_deadline(
                now,
                Demand {
                    energy_saver: true,
                    ..demand()
                }
            ),
            Some(now + SAVER)
        );
        assert!(
            state
                .begin(
                    now + NORMAL,
                    Demand {
                        energy_saver: true,
                        ..demand()
                    }
                )
                .is_none()
        );
    }

    #[test]
    fn disabling_and_reenabling_rejects_old_work_without_parallel_requests() {
        let now = Instant::now();
        let mut state = Refresh::default();
        state.configure(true, Some(coordinates()));
        let old = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
        state.configure(false, Some(coordinates()));
        state.configure(true, Some(coordinates()));
        assert!(state.begin(now, demand()).is_none());
        assert!(!state.finish(old, Ok(forecast()), now));
        assert!(state.cached(now).is_none());
        let new = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
        assert!(!state.finish(old, Ok(forecast()), now));
        assert!(
            state.begin(now, demand()).is_none(),
            "duplicate result must not clear new work"
        );
        assert!(state.finish(new, Ok(forecast()), now));
    }

    #[test]
    fn changing_location_discards_both_cached_data_and_delayed_old_results() {
        let now = Instant::now();
        let mut state = Refresh::default();
        state.configure(true, Some(coordinates()));
        let old = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
        state.configure(
            true,
            Some(Coordinates::new(-12.05, -77.04).unwrap_or_else(|_| unreachable!())),
        );
        assert!(!state.finish(old, Ok(forecast()), now));
        assert!(state.cached(now).is_none());
        assert!(state.begin(now, demand()).is_some());
    }

    #[test]
    fn failure_preserves_real_data_but_respects_backoff_and_expiry() {
        let now = Instant::now();
        let mut state = Refresh::default();
        state.configure(true, Some(coordinates()));
        let first = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
        state.finish(first, Ok(forecast()), now);
        let later = now + NORMAL;
        let next = state
            .begin(later, demand())
            .unwrap_or_else(|| unreachable!());
        state.finish(next, Err(Error::Http(503)), later);
        assert!(
            state
                .cached(later)
                .is_some_and(|view| view.stale && view.forecast.current.temperature_c == 17.0)
        );
        assert_eq!(
            state.next_deadline(later, demand()),
            Some(later + Duration::from_secs(300))
        );
        assert!(
            state
                .begin(later + Duration::from_secs(299), demand())
                .is_none()
        );
        assert!(
            state
                .cached(now + MAX_AGE + Duration::from_secs(1))
                .is_none()
        );
    }

    #[test]
    fn rate_limits_and_repeated_failures_cannot_turn_into_tight_retry_loops() {
        let mut now = Instant::now();
        let mut state = Refresh::default();
        state.configure(true, Some(coordinates()));
        for delay in [300, 600, 1200, 2400, 3600, 3600] {
            let ticket = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
            state.finish(ticket, Err(Error::Http(500)), now);
            assert_eq!(
                state.next_deadline(now, demand()),
                Some(now + Duration::from_secs(delay))
            );
            now += Duration::from_secs(delay);
        }
        let ticket = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
        state.finish(
            ticket,
            Err(Error::RateLimited {
                retry_after: Duration::from_secs(7200),
            }),
            now,
        );
        assert_eq!(
            state.next_deadline(now, demand()),
            Some(now + Duration::from_secs(7200))
        );
    }

    #[test]
    fn changing_city_or_toggling_does_not_bypass_a_provider_rate_limit() {
        let now = Instant::now();
        let mut state = Refresh::default();
        state.configure(true, Some(coordinates()));
        let old = state.begin(now, demand()).unwrap_or_else(|| unreachable!());
        state.configure(false, None);
        state.configure(true, Some(coordinates()));
        assert!(!state.finish(
            old,
            Err(Error::RateLimited {
                retry_after: Duration::from_secs(7200)
            }),
            now
        ));
        assert!(
            state
                .begin(now + Duration::from_secs(7199), demand())
                .is_none()
        );
        assert!(
            state
                .begin(now + Duration::from_secs(7200), demand())
                .is_some()
        );
    }
}
