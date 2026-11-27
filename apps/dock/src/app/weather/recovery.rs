//! Exceptional recovery shares the weather deadline; healthy readers never poll.
use super::*;
impl App {
    pub(super) fn queue_weather_graphics_trim(&self) {
        if !self.weather.drawings_dirty.replace(true) {
            self.schedule_trim();
        }
    }
    pub(in crate::app) fn flush_weather_drawings(&self) -> Result<()> {
        if self.weather.drawings_dirty.replace(false) {
            self.gfx.flush_released_drawings()?;
        }
        Ok(())
    }
    pub(super) fn subscribe_weather_network(&mut self) {
        let first_failure = self.weather.network_retry.is_none();
        self.weather.network = status::NetworkWatch::new(self.input.0 as isize, NETWORK);
        self.weather.network_retry = self
            .weather
            .network
            .is_none()
            .then(|| Instant::now() + Duration::from_secs(300));
        if first_failure && self.weather.network.is_none() {
            muelle_shell::log::line(
                "clima: suscripción de red no disponible; reintento visible en 5 min",
            );
        }
    }
    pub(in crate::app) fn weather_environment_changed(&mut self, display_changed: bool) {
        if self.weather.popup.is_some() && (display_changed || menu::needs_native_menu()) {
            self.close_weather_popup();
        }
    }
    pub(in crate::app) fn weather_suspended(&mut self, sleeping: bool) {
        self.weather.sleeping = sleeping;
        if sleeping {
            self.suspend_weather_view();
        }
    }
    pub(in crate::app) fn weather_resumed(&mut self) -> Result<()> {
        if !self.config.weather {
            return Ok(());
        }
        self.close_weather_popup();
        self.take_weather_result();
        self.arm_weather(None);
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        self.weather.service.revalidate(Instant::now(), unix);
        self.weather.revision = self.weather.revision.wrapping_add(1);
        if self.weather.network.is_none() {
            self.subscribe_weather_network();
        }
        // Reuse the reader once for this actual system event, not each frame.
        self.weather_network()?;
        self.sync_weather();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::visibility::tests::{Fixture, queue};
    #[test]
    #[ignore = "Owned fixture only; no running apps, live location, network request or taskbar change"]
    fn recovery_is_opt_in_and_uses_one_deadline_without_spawning_work() -> Result<()> {
        let _queue = queue()?;
        let mut fixture = Fixture::new()?;
        let app = &mut fixture.app;
        assert!(!app.config.weather);
        app.weather_resumed()?;
        assert!(app.weather.network.is_none() && app.weather.due.is_none());
        assert!(!app.weather.drawings_dirty.get());
        app.queue_weather_graphics_trim();
        app.queue_weather_graphics_trim();
        assert!(app.weather.drawings_dirty.get());
        app.flush_weather_drawings()?;
        assert!(!app.weather.drawings_dirty.get());
        app.flush_weather_drawings()?; // unchanged/off path performs no work
        app.weather_suspended(true);
        assert!(app.weather.sleeping);
        app.weather_locked(true);
        app.weather_suspended(false);
        assert!(
            app.weather.locked,
            "resume must not unlock a locked session"
        );
        app.weather_locked(false);
        app.config.weather = true;
        app.weather.network_retry = Some(Instant::now() - Duration::from_secs(1));
        app.weather.view_retry = Some(Instant::now() - Duration::from_secs(1));
        app.weather_tick()?; // no stage/visible card: never query network or spawn HTTP
        assert!(app.weather.pending.is_none());
        assert!(
            app.weather.due.is_none(),
            "hidden/offscreen weather does not schedule retries"
        );
        app.config.weather = false;
        app.sync_weather();
        assert!(app.weather.network.is_none() && app.weather.network_retry.is_none());
        Ok(())
    }
}
