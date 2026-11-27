//! Optional quiet mascot between events. One sparse, one-shot deadline per
//! visible beat; none while hidden, disabled or resting in reduced motion.

use std::time::{Duration, Instant};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{App, COMPANION_TIMER};
use crate::mascot::companion::Companion;

#[derive(Default)]
pub(super) struct State {
    view: Option<Companion>,
    due: Option<Instant>,
    retry_after: Option<Instant>,
    locked: bool,
}

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

    /// No system reads or visual mutations for an unchanged, visible companion.
    /// The existing render path gives immediate hide/show and geometry updates.
    pub(super) fn sync_companion(&mut self) {
        let allowed = self.config.mascot
            && self.config.mascot_always_visible
            && !self.companion.locked
            && !self.fullscreen
            && self.moving_since.is_none()
            && self.mascot.is_none()
            && self.slide.target == 0.0
            && self.slide.value == 0.0
            && self.slide.is_settled()
            && !self.hovering
            && self.amount.value == 0.0
            && !self.menu_open
            && self.weather.popup.is_none()
            && self.press.is_none()
            && self.drag.is_none()
            && !self.drag_over
            && !self.switching
            && self.preview.is_none()
            && self.group_view.is_none();
        let context = allowed
            .then(|| {
                Some((
                    self.config.mascot_character,
                    self.dock_edge()?,
                    self.stage.as_ref()?.scale,
                ))
            })
            .flatten();
        let Some((character, place, scale)) = context else {
            self.suspend_companion();
            return;
        };
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
        let built = (|| -> anyhow::Result<_> {
            let mut view = Companion::new(&self.gfx, character, place, scale)?;
            let delay = view.set_motion(companion_motion())?;
            Ok((view, delay))
        })();
        match built {
            Ok((view, delay)) => {
                self.companion.view = Some(view);
                self.companion.retry_after = None;
                self.arm_companion(delay);
            }
            Err(error) => self.companion_failed(&error),
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
                    delay.as_millis().clamp(10, u32::MAX as u128) as u32,
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
        // SAFETY: Our own timer; stale queued messages do not advance a beat.
        unsafe {
            let _ = KillTimer(Some(self.input), COMPANION_TIMER);
        }
        self.sync_companion();
        let Some(due) = self.companion.due else {
            return Ok(());
        };
        if let Some(remaining) = due.checked_duration_since(Instant::now()) {
            self.arm_companion(Some(remaining));
            return Ok(());
        }
        let result = self
            .companion
            .view
            .as_mut()
            .map(|view| view.advance(&self.gfx));
        match result {
            Some(Ok(delay)) => self.arm_companion(delay),
            Some(Err(error)) => self.companion_failed(&error),
            None => {}
        }
        Ok(())
    }

    /// Called by the existing power/settings notification path, never per
    /// animation frame. Saver and reduced-motion changes cancel the patrol.
    pub(super) fn refresh_companion_motion(&mut self) {
        if let Some(view) = &mut self.companion.view {
            let enabled = companion_motion();
            if view.motion() != enabled {
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
