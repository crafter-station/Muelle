//! Weather lifecycle: event-driven OS state, one deadline, one bounded worker.
//! Render only keys retained visuals/arms work; it never performs HTTP or parses.
use super::{
    App, HIDE_DELAY_MS, HIDE_TIMER, MAIN_WINDOW, MenuItem, MenuRequest, MenuTarget, Outside,
};
use crate::{
    gfx::WeatherGlyph,
    menu,
    weather::{self, Card, Key, Model, glyph},
};
use anyhow::Result;
use muelle_config::{WeatherCity, WeatherLocation, WeatherLocationMode};
use muelle_shell::{power, status};
use muelle_weather::{Coordinates, Demand, Forecast, Refresh, Ticket};
use std::{
    cell::Cell,
    sync::{Arc, Mutex, atomic::Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, WPARAM},
    UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer, WM_APP},
};

pub(super) const READY: u32 = WM_APP + 110;
pub(super) const EVENT: u32 = WM_APP + 111;
pub(super) const NETWORK: u32 = WM_APP + 112;
pub(super) const POPUP_EVENT: u32 = WM_APP + 113;
pub(super) const TIMER: usize = 21;
const SOURCE: u32 = 1;
const SETTINGS: u32 = 2;
type Mailbox = Arc<Mutex<Option<(Ticket, std::result::Result<Forecast, muelle_weather::Error>)>>>;
struct Pending {
    ticket: Ticket,
    deadline: Option<Instant>,
}

#[derive(Default)]
pub(super) struct State {
    service: Refresh,
    configured: Option<(
        bool,
        WeatherLocationMode,
        Option<WeatherLocation>,
        Option<WeatherCity>,
    )>,
    mailbox: Mailbox,
    pub(super) card: Option<Card>,
    pub(super) popup: Option<weather::popup::Popup>,
    network: Option<status::NetworkWatch>,
    online: bool,
    network_known: bool,
    hovering: bool,
    locked: bool,
    sleeping: bool,
    failed: bool,
    revision: u64,
    due: Option<Instant>,
    view_retry: Option<Instant>,
    pending: Option<Pending>,
    network_retry: Option<Instant>,
    popup_hour: Option<(u64, Instant)>,
    drawings_dirty: Cell<bool>,
}

pub(crate) fn signal(source: HWND, event: usize) {
    // Post rather than re-enter App from CreateWindow/SetWindowPos callbacks.
    unsafe {
        let _ = PostMessageW(
            Some(HWND(MAIN_WINDOW.load(Ordering::Relaxed) as *mut _)),
            EVENT,
            WPARAM(event),
            LPARAM(source.0 as isize),
        );
    }
}

pub(crate) fn popup_signal(source: HWND, action: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(MAIN_WINDOW.load(Ordering::Relaxed) as *mut _)),
            POPUP_EVENT,
            WPARAM(source.0 as usize),
            LPARAM(action),
        );
    }
}

impl App {
    pub(super) fn weather_holds_open(&self) -> bool {
        self.weather.hovering || self.weather.popup.is_some()
    }
    pub(super) fn weather_locked(&mut self, locked: bool) {
        self.weather.locked = locked;
        self.sync_weather();
    }
    pub(super) fn suspend_weather_view(&mut self) {
        let released = self.weather.card.is_some() || self.weather.popup.is_some();
        self.weather.popup = None;
        self.weather.popup_hour = None;
        let hovered = std::mem::take(&mut self.weather.hovering);
        self.weather.card = None;
        if released {
            self.queue_weather_graphics_trim();
        }
        self.arm_weather(None);
        if hovered && self.config.hides_automatically() {
            unsafe {
                SetTimer(Some(self.input), HIDE_TIMER, HIDE_DELAY_MS, None);
            }
        }
    }
    fn weather_visible(&self) -> bool {
        self.config.weather
            && !self.weather.locked
            && !self.weather.sleeping
            && !self.fullscreen
            && self.moving_since.is_none()
            && self.slide.value < 1.0
            && !self.switching
    }
    fn weather_demand(&self) -> Demand {
        Demand {
            // Transient Dock interactions retain the permanent card. They pause
            // network demand, not the visual, avoiding empty slots and rebuilds.
            visible: self.weather_visible()
                && self.weather.card.is_some()
                && !self.menu_open
                && self.preview.is_none()
                && self.group_view.is_none()
                && self.drag.is_none()
                && !self.drag_over,
            online: self.weather.online,
            energy_saver: self.power_seen.is_some_and(|(_, saver)| saver),
        }
    }

    pub(super) fn sync_weather(&mut self) {
        // Compare borrowed labels on the render path; clone only on a setting change.
        let changed = self
            .weather
            .configured
            .as_ref()
            .is_none_or(|(enabled, mode, fix, city)| {
                *enabled != self.config.weather
                    || *mode != self.config.weather_location_mode
                    || *fix != self.config.weather_location
                    || city.as_ref() != self.config.display_weather_city()
            });
        if changed {
            let coordinates = self.config.weather_coordinates().and_then(|(lat, lon)| {
                Coordinates::new(f64::from(lat) / 100.0, f64::from(lon) / 100.0).ok()
            });
            self.weather
                .service
                .configure(self.config.weather, coordinates);
            self.weather.configured = Some((
                self.config.weather,
                self.config.weather_location_mode,
                self.config.weather_location,
                self.config.display_weather_city().cloned(),
            ));
            self.weather.failed = false;
            self.weather.view_retry = None;
            self.weather.revision = self.weather.revision.wrapping_add(1);
            if self.config.weather && self.weather.network.is_none() {
                self.subscribe_weather_network();
                unsafe {
                    let _ = PostMessageW(Some(self.input), NETWORK, WPARAM(0), LPARAM(0));
                }
            } else if !self.config.weather {
                self.weather.network = None;
                self.weather.online = false;
                self.weather.network_known = false;
                self.weather.network_retry = None;
            }
        }
        if !self.weather_visible() || self.stage.is_none() {
            self.suspend_weather_view();
            return;
        }
        let Some(stage) = self.stage.as_ref() else {
            self.suspend_weather_view();
            return;
        };
        let Some(placement) = stage.weather_place else {
            self.suspend_weather_view();
            return;
        };
        let now = Instant::now();
        let cached = self.weather.service.cached(now);
        let cache_kind = cached.as_ref().map_or(0, |c| if c.stale { 2 } else { 1 });
        let key = Key {
            placement,
            screen: stage.key.screen,
            look: stage.key.look,
            scale: stage.scale,
            revision: self
                .weather
                .revision
                .wrapping_mul(4)
                .wrapping_add(cache_kind),
            language: crate::strings::language(),
        };
        if key.visible_bounds().is_none() {
            self.suspend_weather_view();
            return;
        }
        // Position-only spring/magnification frames do not format strings,
        // rasterize art or replace the Composition root.
        if let Some(view) = &mut self.weather.card
            && view.key.same_surface(key)
            && let Err(error) = view.reposition(key)
        {
            self.suspend_weather_view();
            self.weather.view_retry = Some(now + Duration::from_secs(60));
            crate::recovery::graphics_error(&error);
            muelle_shell::log::line(&format!("posición del clima: {error:#}"));
            self.arm_weather(self.weather.view_retry);
            return;
        }
        let redraw = self
            .weather
            .card
            .as_ref()
            .is_none_or(|view| view.key != key);
        if redraw && self.weather.view_retry.is_none_or(|after| now >= after) {
            let text = weather::text::text(key.language);
            let saved = self.config.weather_location_mode == WeatherLocationMode::Auto
                && self.config.weather_location.is_some_and(|loc| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .is_ok_and(|now| now.as_secs().saturating_sub(loc.updated_at) > 6 * 3600)
                });
            let (degrees, title, glyph) = match cached.as_ref() {
                Some(c) => (
                    format!("{}°", c.forecast.current.temperature_c.round() as i16),
                    text.condition(c.forecast.current.condition),
                    glyph(c.forecast.current.condition, c.forecast.current.is_day),
                ),
                None => (
                    ("—").into(),
                    if self.config.weather_coordinates().is_none() {
                        text.location_needed
                    } else if self.weather.failed
                        || (self.weather.network_known && !self.weather.online)
                    {
                        text.unavailable
                    } else {
                        text.loading
                    },
                    WeatherGlyph::Unknown,
                ),
            };
            let subtitle = if let Some(city) = self.config.display_weather_city() {
                city.label.as_str()
            } else if self.config.weather_coordinates().is_none() {
                text.settings
            } else if cached.as_ref().is_some_and(|c| c.stale) {
                text.stale
            } else if saved {
                text.saved_location
            } else {
                text.current_location
            };
            let model = Model {
                degrees,
                title: title.to_owned(),
                subtitle: subtitle.to_owned(),
                glyph,
            };
            let result = if let Some(view) = &mut self.weather.card {
                view.update(&self.gfx, key, &model)
            } else {
                Card::new(&self.gfx, key, &model).map(|view| self.weather.card = Some(view))
            };
            if let Err(error) = result {
                self.suspend_weather_view();
                self.weather.view_retry = Some(now + Duration::from_secs(60));
                crate::recovery::graphics_error(&error);
                muelle_shell::log::line(&format!("tarjeta del clima: {error:#}"));
                self.arm_weather(self.weather.view_retry);
                return;
            }
            self.weather.view_retry = None;
            self.queue_weather_graphics_trim();
        }
        let mut due = self
            .weather
            .service
            .next_deadline(now, self.weather_demand());
        if let Some(work) = &self.weather.pending
            && let Some(deadline) = work.deadline
        {
            due = Some(due.map_or(deadline, |d| d.min(deadline)));
        }
        // Local freshness/expiry also advances while offline, without a request
        // or a permanent UI clock. Hidden cards have no deadline at all.
        if let Some(cache) = self.weather.service.cached(now) {
            due = Some(due.map_or(cache.next_change, |d| d.min(cache.next_change)));
        }
        for deadline in [self.weather.view_retry, self.weather.network_retry]
            .into_iter()
            .flatten()
        {
            due = Some(due.map_or(deadline, |d| d.min(deadline)));
        }
        // One local wake at the next hour only while the forecast is open:
        // remove elapsed columns/date labels without a minute clock or HTTP.
        if self.weather.popup.is_some() {
            let unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |t| t.as_secs());
            let deadline = match self.weather.popup_hour {
                Some((hour, deadline)) if hour == unix / 3600 => deadline,
                _ => {
                    let deadline = now + Duration::from_secs(3600 - unix % 3600);
                    self.weather.popup_hour = Some((unix / 3600, deadline));
                    deadline
                }
            };
            due = Some(due.map_or(deadline, |d| d.min(deadline)));
        } else {
            self.weather.popup_hour = None;
        }
        self.arm_weather(due);
        self.sync_weather_popup();
    }

    fn arm_weather(&mut self, due: Option<Instant>) {
        let now = Instant::now();
        if self.weather.due == due
            || (due.is_some_and(|d| d <= now) && self.weather.due.is_some_and(|old| old <= now))
        {
            return;
        }
        self.weather.due = due;
        unsafe {
            let _ = KillTimer(Some(self.input), TIMER);
            if let Some(due) = due {
                let delay = due
                    .saturating_duration_since(now)
                    .as_millis()
                    .clamp(10, u32::MAX as u128) as u32;
                if SetTimer(Some(self.input), TIMER, delay, None) == 0 {
                    self.weather.due = None;
                }
            }
        }
    }

    pub(super) fn weather_tick(&mut self) -> Result<()> {
        self.arm_weather(None);
        self.take_weather_result();
        if self.weather_visible()
            && self
                .weather
                .network_retry
                .is_some_and(|due| Instant::now() >= due)
        {
            self.subscribe_weather_network();
            self.weather_network()?;
        }
        if let Some(work) = &mut self.weather.pending
            && work
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            // One local watchdog, not polling: recover a completed result even
            // if PostMessage failed, or show unavailable without spawning a
            // second worker while the original still owns its request.
            work.deadline = None;
            muelle_shell::log::line(
                "clima: respuesta pendiente tras 15 s; se conserva un único trabajador",
            );
            self.weather.failed = true;
            self.weather.revision = self.weather.revision.wrapping_add(1);
        }
        if let Some(ticket) = self
            .weather
            .service
            .begin(Instant::now(), self.weather_demand())
        {
            self.weather.pending = Some(Pending {
                ticket,
                deadline: Some(Instant::now() + Duration::from_secs(15)),
            });
            let mailbox = self.weather.mailbox.clone();
            let target = self.input.0 as isize;
            let spawned = thread::Builder::new()
                .name("muelle-weather".into())
                .spawn(move || {
                    power::set_thread_efficiency(true);
                    // Client, DNS, TLS, body validation and parsing all stay here.
                    let started = Instant::now();
                    let result = muelle_weather::Client::new()
                        .and_then(|client| client.fetch(ticket.coordinates()));
                    if let Err(error) = &result {
                        muelle_shell::log::line(&format!(
                            "clima: consulta fallida ({}, {} ms)",
                            error.diagnostic_code(),
                            started.elapsed().as_millis()
                        ));
                    }
                    *mailbox
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some((ticket, result));
                    unsafe {
                        let _ =
                            PostMessageW(Some(HWND(target as *mut _)), READY, WPARAM(0), LPARAM(0));
                    }
                });
            if spawned.is_err() {
                muelle_shell::log::line("clima: no se pudo iniciar el trabajador");
                self.weather.pending = None;
                self.weather.service.finish(
                    ticket,
                    Err(muelle_weather::Error::Payload("worker unavailable")),
                    Instant::now(),
                );
                self.weather.failed = true;
                self.weather.revision = self.weather.revision.wrapping_add(1);
            }
        }
        self.sync_weather();
        Ok(())
    }
    pub(super) fn weather_ready(&mut self) -> Result<()> {
        self.take_weather_result();
        self.sync_weather();
        Ok(())
    }
    fn take_weather_result(&mut self) {
        let result = self
            .weather
            .mailbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((ticket, result)) = result {
            if self
                .weather
                .pending
                .as_ref()
                .is_some_and(|work| work.ticket == ticket)
            {
                self.weather.pending = None;
            }
            let failed = result.is_err();
            if self.weather.service.finish(ticket, result, Instant::now()) {
                self.weather.failed = failed;
                self.weather.revision = self.weather.revision.wrapping_add(1);
            }
        }
    }
    pub(super) fn weather_network(&mut self) -> Result<()> {
        if self.config.weather {
            let online = matches!(
                status::network(),
                status::Network::Ethernet
                    | status::Network::Wifi {
                        has_internet: true,
                        ..
                    }
            );
            if !self.weather.network_known || self.weather.online != online {
                self.weather.online = online;
                self.weather.network_known = true;
                self.weather.revision = self.weather.revision.wrapping_add(1);
                self.sync_weather();
            }
        }
        Ok(())
    }
    pub(super) fn weather_event(&mut self, source: HWND, event: usize) -> Result<Option<Outside>> {
        let Some(card) = &self.weather.card else {
            return Ok(None);
        };
        if card.hwnd() != source {
            return Ok(None);
        }
        if event < 2 {
            self.weather.hovering = event == 1;
            // A captured reorder or OLE drag keeps its own hover controller.
            if self.drag.is_some() || self.drag_over {
                return Ok(None);
            }
            if event == 1
                && (self.hovering
                    || self.pointed
                    || self.amount.target != 0.0
                    || self.slide.target != 0.0
                    || self.preview.is_some())
            {
                // The card can intercept input while the Dock is still sliding.
                // Finish revealing it and transfer the hold without a hide pulse.
                self.hovering = false;
                self.pointed = false;
                self.hovered = None;
                self.amount.target = 0.0;
                self.slide.target = 0.0;
                self.last_frame = Instant::now();
                self.close_preview();
                self.place_windows()?;
                self.render()?;
            }
            if event == 1 {
                // Closing an app preview can arm this timer: cancel it after
                // the transfer, with the weather hold already established.
                unsafe {
                    let _ = KillTimer(Some(self.input), HIDE_TIMER);
                }
            }
            if event == 0 && self.config.hides_automatically() {
                unsafe {
                    SetTimer(Some(self.input), HIDE_TIMER, HIDE_DELAY_MS, None);
                }
            }
            return Ok(None);
        }
        // A captured reorder or OLE drop owns this interaction. Keep the tile
        // visible, but ignore queued releases instead of opening a second UI.
        if self.drag.is_some() || self.drag_over {
            return Ok(None);
        }
        if event == 2 && menu::needs_native_menu() {
            return self.accessible_weather_menu();
        }
        if event == 2 {
            self.toggle_weather_popup()?;
            return Ok(None);
        }
        let r = card.key.placement.bounds;
        let text = weather::text::text(card.key.language);
        let mut items = vec![MenuItem::Header(text.title.to_owned())];
        if let Some(cache) = self.weather.service.cached(Instant::now()) {
            items.push(MenuItem::Header(format!(
                "{:.0}° · {}",
                cache.forecast.current.temperature_c,
                text.condition(cache.forecast.current.condition)
            )));
        }
        items.extend([
            MenuItem::Separator,
            MenuItem::action(SETTINGS, text.settings),
            MenuItem::action(SOURCE, muelle_weather::ATTRIBUTION),
        ]);
        Ok(Some(Outside::Menu(MenuRequest {
            gfx: self.gfx.clone(),
            target: MenuTarget::Weather,
            items,
            anchor: POINT {
                x: (r.left + r.right) / 2,
                y: r.top,
            },
            placement: menu::Placement::Above,
            look: card.key.look,
        })))
    }
    pub(super) fn weather_command(&mut self, command: u32) -> Result<Option<Outside>> {
        Ok(match command {
            SOURCE => Some(Outside::Open(muelle_weather::ATTRIBUTION_URL.into())),
            SETTINGS => Some(Outside::OpenSettings),
            _ => None,
        })
    }
    pub(super) fn weather_after_menu(&mut self, pointer: HWND) {
        self.weather.hovering = self
            .weather
            .card
            .as_ref()
            .is_some_and(|card| card.track_after_menu(pointer == card.hwnd()));
    }
}

#[path = "weather/popup.rs"]
mod popup;

#[path = "weather/recovery.rs"]
mod recovery;

#[path = "weather/accessibility.rs"]
mod accessibility;

#[cfg(test)]
#[path = "weather/input_tests.rs"]
mod input_tests;
