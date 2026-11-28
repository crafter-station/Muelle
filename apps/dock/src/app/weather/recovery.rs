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
    #[ignore = "Offscreen owned preview/card only; no live app, focus, location or network changes"]
    fn window_preview_retains_weather_visibility_but_never_requests_a_forecast() -> Result<()> {
        use crate::{
            appearance::{Look, Material},
            gfx::WeatherGlyph,
            multi_window_view::{Anchor, MultiWindowView},
            scene::Theme,
            weather::{Key, Model, Placement},
        };
        use muelle_core::frame::DockEdge;
        use windows::Win32::{
            Foundation::RECT,
            UI::{
                Input::KeyboardAndMouse::GetCapture,
                WindowsAndMessaging::{GetForegroundWindow, IsWindow},
            },
        };
        let _queue = queue()?;
        let mut fixture = Fixture::new()?;
        let app = &mut fixture.app;
        app.config.weather = true;
        let screen = RECT {
            left: -33000,
            top: -33000,
            right: -30000,
            bottom: -30000,
        };
        let anchor = RECT {
            left: -32000,
            top: -31000,
            right: -31780,
            bottom: -30952,
        };
        let look = Look {
            material: Material::Frosted,
            light: false,
            glass: false,
        };
        app.weather.card = Some(Card::new(
            &app.gfx,
            Key {
                placement: Placement {
                    bounds: anchor,
                    compact: false,
                },
                screen,
                look,
                scale: 1.0,
                revision: 0,
                language: muelle_config::Language::Es,
            },
            &Model {
                degrees: "19°".into(),
                title: "Llovizna".into(),
                subtitle: "Lima".into(),
                glyph: WeatherGlyph::Rain,
            },
        )?);
        let card = app
            .weather
            .card
            .as_ref()
            .map(Card::hwnd)
            .unwrap_or_default();
        let foreground = unsafe { GetForegroundWindow() };
        let capture = unsafe { GetCapture() };
        assert!(app.weather_visible());
        for _ in 0..3 {
            app.preview = Some(MultiWindowView::show(
                &app.gfx,
                "owned-source".into(),
                vec![app.visual.0 as isize],
                &Anchor {
                    icon: anchor,
                    screen,
                    edge: DockEdge::Bottom,
                },
                1.0,
                Theme::new(look),
            )?);
            assert!(
                app.weather_visible(),
                "opening a preview must not leave an empty weather slot"
            );
            assert!(
                !app.weather_demand().visible,
                "preview rendering must not add weather fetches"
            );
            assert_eq!(app.weather.card.as_ref().map(Card::hwnd), Some(card));
            assert!(unsafe { IsWindow(Some(card)) }.as_bool());
            app.close_preview();
            assert!(app.weather_visible());
        }
        assert!(app.weather.pending.is_none() && app.weather.network.is_none());
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        assert_eq!(unsafe { GetCapture() }, capture);
        // Actual hiding policies still win, unlike a temporary preview.
        app.fullscreen = true;
        assert!(!app.weather_visible());
        app.fullscreen = false;
        app.weather.sleeping = true;
        assert!(!app.weather_visible());
        app.weather.sleeping = false;
        app.weather.locked = true;
        assert!(!app.weather_visible());
        Ok(())
    }
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
