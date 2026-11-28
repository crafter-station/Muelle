//! Optional lively mascot between events. One one-shot deadline per act;
//! none while hidden, disabled or resting in reduced motion. It lives on the
//! dock and now and then visits the menu bar (see `mascot::companion`).

use std::time::{Duration, Instant};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{App, COMPANION_TIMER};
use crate::mascot::{
    Place,
    companion::{Companion, Entrance, Next, Poke},
};

/// The pointer came over the companion or clicked it (see
/// `mascot::companion::create_hit_window`).
pub(crate) fn companion_poke(poke: Poke) {
    super::with_app("mascota", |app| {
        app.poke_companion(poke);
        Ok(())
    });
}

#[derive(Default)]
pub(super) struct State {
    view: Option<Companion>,
    due: Option<Instant>,
    retry_after: Option<Instant>,
    locked: bool,
    /// It is visiting the menu bar.
    on_bar: bool,
    /// How it comes on screen next time it is built.
    entrance: Entrance,
    /// When it may next visit the menu bar. Kept here, not in the view: the
    /// view is rebuilt whenever the dock hides or is used.
    next_trip: Option<Instant>,
    /// The wake-up armed for that visit while nothing shows (the dock hidden).
    wake: Option<Instant>,
}

/// Between two visits to the menu bar.
const TRIP_EVERY: Duration = Duration::from_secs(90);

impl App {
    pub(super) fn suspend_companion(&mut self) {
        if self.companion.view.take().is_some() || self.companion.due.take().is_some() {
            self.companion.due = None;
            // SAFETY: This timer belongs to our UI window/thread.
            unsafe {
                let _ = KillTimer(Some(self.input), COMPANION_TIMER);
            }
        }
    }

    /// Just turned on in Settings: it says hello on the dock.
    pub(super) fn greet_companion(&mut self) {
        self.companion.entrance = Entrance::Greet;
        self.companion.on_bar = false;
        // Its first round shows the menu bar walk.
        self.companion.next_trip = Some(Instant::now());
        self.companion.retry_after = None;
        self.suspend_companion();
    }

    /// No system reads or visual mutations for an unchanged, visible companion.
    /// The existing render path gives immediate hide/show and geometry updates.
    pub(super) fn sync_companion(&mut self) {
        let wanted = self.config.mascot
            && self.config.mascot_always_visible
            && !self.companion.locked
            && !self.fullscreen
            && self.mascot.is_none()
            && !self.menu_open
            && self.weather.popup.is_none()
            && !self.switching;
        // On the dock it needs the dock at rest and left alone.
        let dock_ready = self.moving_since.is_none()
            && self.slide.target == 0.0
            && self.slide.value == 0.0
            && self.slide.is_settled()
            && !self.hovering
            && self.amount.value == 0.0
            && self.press.is_none()
            && self.drag.is_none()
            && !self.drag_over
            && self.preview.is_none()
            && self.group_view.is_none();
        let walkway = self.menubar.as_ref().and_then(|bar| {
            bar.walkway()
                .map(|(rect, scale)| (rect, scale, bar.window()))
        });
        if self.companion.on_bar && walkway.is_none() {
            // The bar hid (automatic or intelligent hiding, full screen) or
            // filled up: back to the dock at once, with a hop.
            self.companion.on_bar = false;
            self.companion.entrance = Entrance::Arrive;
        }
        let motion = companion_motion();
        let trip_due = self
            .companion
            .next_trip
            .is_some_and(|at| Instant::now() >= at);
        if wanted
            && !self.companion.on_bar
            && !dock_ready
            && motion
            && trip_due
            && walkway.is_some()
        {
            // The dock is hidden (a window covers it, say) but the bar shows:
            // the mascot visits the bar on its own now and then.
            self.companion.on_bar = true;
            self.companion.entrance = Entrance::Arrive;
        }
        let context = if !wanted {
            None
        } else if self.companion.on_bar {
            walkway.map(|(rect, scale, bar)| {
                let place = Place::Bar {
                    left: rect.left,
                    right: rect.right,
                    top: rect.top,
                    bottom: rect.bottom,
                };
                (place, scale, Some(bar))
            })
        } else if dock_ready {
            self.dock_edge()
                .zip(self.stage.as_ref().map(|stage| stage.scale))
                // Owned by the dock's input window: always above the dock.
                .map(|(place, scale)| (place, scale, Some(self.input)))
        } else {
            None
        };
        let character = self.config.mascot_character;
        let Some((place, scale, owner)) = context else {
            self.suspend_companion();
            if wanted && motion {
                self.wake_companion_for_trip();
            }
            return;
        };
        self.companion.wake = None;
        if self
            .companion
            .view
            .as_ref()
            .is_some_and(|view| view.matches(character, place, scale))
        {
            return;
        }
        self.suspend_companion();
        if self
            .companion
            .retry_after
            .is_some_and(|after| Instant::now() < after)
        {
            return;
        }
        let entrance = std::mem::take(&mut self.companion.entrance);
        let built = (|| -> anyhow::Result<_> {
            let mut view = Companion::new(&self.gfx, character, place, scale, entrance, owner)?;
            let delay = view.set_motion(companion_motion())?;
            Ok((view, delay))
        })();
        match built {
            Ok((view, delay)) => {
                self.companion.view = Some(view);
                self.companion.retry_after = None;
                self.arm_companion(delay);
            }
            Err(error) => {
                // Not on the bar after all (too little room there): the dock.
                if self.companion.on_bar {
                    self.companion.on_bar = false;
                    self.companion.entrance = Entrance::Arrive;
                    muelle_shell::log::line(&format!("mascota en la barra superior: {error:#}"));
                    return self.sync_companion();
                }
                self.companion_failed(&error);
            }
        }
    }

    /// Nothing shows (the dock is hidden): one timer, at the next visit to
    /// the menu bar, and none again for the same visit.
    fn wake_companion_for_trip(&mut self) {
        let now = Instant::now();
        let mut at = *self
            .companion
            .next_trip
            .get_or_insert_with(|| now + TRIP_EVERY);
        if at <= now {
            // Due, but the bar does not show or has no room: a bit later.
            at = now + Duration::from_secs(30);
            self.companion.next_trip = Some(at);
        }
        if self.companion.wake == Some(at) {
            return;
        }
        self.companion.wake = Some(at);
        let wait = at.saturating_duration_since(Instant::now());
        // SAFETY: Only our UI thread arms this window's timer; companion_tick
        // kills it and runs `sync_companion`, which starts the visit.
        unsafe {
            SetTimer(
                Some(self.input),
                COMPANION_TIMER,
                wait.as_millis().clamp(10, u128::from(u32::MAX)) as u32,
                None,
            );
        }
    }

    fn arm_companion(&mut self, delay: Option<Duration>) {
        self.companion.due = delay.map(|delay| Instant::now() + delay);
        // SAFETY: Only our UI thread arms/removes this window's timer. WM_TIMER
        // is repeating at the API level; companion_tick kills it before work.
        unsafe {
            let _ = KillTimer(Some(self.input), COMPANION_TIMER);
            if let Some(delay) = delay
                && SetTimer(
                    Some(self.input),
                    COMPANION_TIMER,
                    delay.as_millis().clamp(10, u128::from(u32::MAX)) as u32,
                    None,
                ) == 0
            {
                self.companion_failed(&anyhow::anyhow!("no se pudo programar la mascota"));
            }
        }
    }

    fn companion_failed(&mut self, error: &anyhow::Error) {
        self.suspend_companion();
        // Never retry a broken graphics/window resource on each render frame.
        self.companion.retry_after = Some(Instant::now() + Duration::from_secs(60));
        muelle_shell::log::line(&format!("mascota permanente: {error:#}"));
    }

    pub(super) fn companion_tick(&mut self) -> anyhow::Result<()> {
        // SAFETY: Our own timer; stale queued messages do not advance an act.
        unsafe {
            let _ = KillTimer(Some(self.input), COMPANION_TIMER);
        }
        // Whatever this tick was for, the timer is gone now.
        self.companion.wake = None;
        self.sync_companion();
        let Some(due) = self.companion.due else {
            return Ok(());
        };
        if let Some(remaining) = due.checked_duration_since(Instant::now()) {
            self.arm_companion(Some(remaining));
            return Ok(());
        }
        let next_trip = *self
            .companion
            .next_trip
            .get_or_insert_with(|| Instant::now() + TRIP_EVERY);
        let result = self.companion.view.as_mut().map(|view| {
            view.set_trip_due(Instant::now() >= next_trip);
            view.advance(&self.gfx)
        });
        match result {
            Some(Ok(Next::Wait(delay))) => {
                self.arm_companion((delay != Duration::MAX).then_some(delay));
            }
            Some(Ok(Next::Travel)) => self.travel_companion(),
            Some(Err(error)) => self.companion_failed(&error),
            None => {}
        }
        Ok(())
    }

    fn poke_companion(&mut self, poke: Poke) {
        let Some(view) = &mut self.companion.view else {
            return;
        };
        match view.react(&self.gfx, poke) {
            // Its next act waits until the reaction is over.
            Ok(Some(time)) => {
                let until = Instant::now() + time;
                if self.companion.due.is_none_or(|due| due < until) {
                    self.arm_companion(Some(time));
                }
            }
            Ok(None) => {}
            Err(error) => self.companion_failed(&error),
        }
    }

    /// It left the dock for the menu bar, or the bar for the dock: it shows
    /// up there. With no room in the bar now, it comes back to the dock.
    fn travel_companion(&mut self) {
        let to_bar = !self.companion.on_bar
            && self
                .menubar
                .as_ref()
                .is_some_and(|bar| bar.walkway().is_some());
        if !to_bar {
            // Back from the bar, or no room there: the next visit later.
            self.companion.next_trip = Some(Instant::now() + TRIP_EVERY);
        }
        self.companion.on_bar = to_bar;
        self.companion.entrance = Entrance::Arrive;
        self.suspend_companion();
        self.sync_companion();
    }

    /// Called by the existing power/settings notification path, never per
    /// animation frame. Saver and reduced-motion changes cancel the patrol.
    pub(super) fn refresh_companion_motion(&mut self) {
        if let Some(view) = &mut self.companion.view {
            let enabled = companion_motion();
            if view.motion() != enabled {
                if !enabled && view.on_bar() {
                    // Still, it waits on the dock rather than in the bar.
                    self.companion.on_bar = false;
                    self.suspend_companion();
                    self.sync_companion();
                    return;
                }
                match view.set_motion(enabled) {
                    Ok(delay) => self.arm_companion(delay),
                    Err(error) => self.companion_failed(&error),
                }
            }
        }
    }

    pub(super) fn companion_locked(&mut self, locked: bool) {
        self.companion.locked = locked;
        self.suspend_companion();
        if !locked {
            self.sync_companion();
        }
    }
}

fn companion_motion() -> bool {
    muelle_shell::system::animations_enabled() && !muelle_shell::system::energy_saver_on()
}
