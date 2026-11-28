//! Weather popup ownership, data invalidation and actions. No new polling.
use super::*;
use crate::weather::popup::{
    Action, Context, Popup,
    data::{Data, Selection, Status},
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, WindowFromPoint};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_secs() as i64)
}
impl App {
    pub(super) fn weather_popup_context(&self) -> Option<Context> {
        let card = self.weather.card.as_ref()?;
        let stage = self.stage.as_ref()?;
        Some(Context {
            anchor: card.key.placement.bounds,
            screen: stage.key.screen,
            edge: stage.key.edge,
            scale: stage.scale,
            look: stage.key.look,
            language: card.key.language,
            revision: card.key.revision,
            hour: now() / 3600,
            has_data: self.weather.service.cached(Instant::now()).is_some(),
        })
    }
    pub(super) fn weather_popup_data(&self, context: Context) -> Data {
        let selection = self
            .weather
            .popup
            .as_ref()
            .map_or_else(Selection::default, Popup::selection);
        self.weather_popup_data_for(context, selection)
    }
    fn weather_popup_data_for(&self, context: Context, selection: Selection) -> Data {
        let cache = self.weather.service.cached(Instant::now());
        let mut data = Data::for_selection(
            cache.as_ref().map(|c| c.forecast),
            context.language,
            now(),
            Status {
                stale: cache.as_ref().is_some_and(|c| c.stale),
                has_location: self.config.weather_coordinates().is_some(),
                failed: self.weather.failed,
                offline: self.weather.network_known && !self.weather.online,
                saved: self.config.weather_location_mode == WeatherLocationMode::Auto
                    && self
                        .config
                        .weather_location
                        .is_some_and(|loc| now().saturating_sub(loc.updated_at as i64) > 6 * 3600),
            },
            selection,
        );
        if let Some(city) = self.config.display_weather_city() {
            data.location.clone_from(&city.label);
        }
        data
    }
    pub(super) fn toggle_weather_popup(&mut self) -> Result<()> {
        if self.weather.popup.is_some() {
            self.close_weather_popup();
            return self.render();
        }
        let Some(context) = self.weather_popup_context() else {
            return Ok(());
        };
        self.close_preview();
        self.close_group_view();
        self.suspend_companion();
        self.on_mouse_leave()?;
        self.hovering = false;
        self.hovered = None;
        self.amount.target = 0.0;
        let data = self.weather_popup_data(context);
        let mut popup = Popup::new(&self.gfx, context, &data)?;
        if popup.show() {
            self.weather.popup = Some(popup);
        }
        self.render()
    }
    pub(super) fn close_weather_popup(&mut self) {
        let released = self.weather.popup.is_some();
        self.weather.popup = None;
        if released {
            self.queue_weather_graphics_trim();
        }
        self.weather.popup_hour = None;
        let mut point = POINT::default();
        let pointer = unsafe {
            if GetCursorPos(&mut point).is_ok() {
                WindowFromPoint(point)
            } else {
                HWND::default()
            }
        };
        self.weather_after_menu(pointer);
        if self.config.hides_automatically() {
            unsafe {
                SetTimer(Some(self.input), HIDE_TIMER, HIDE_DELAY_MS, None);
            }
        }
    }
    pub(super) fn sync_weather_popup(&mut self) {
        if self.weather.popup.is_none() {
            return;
        }
        if self.menu_open || self.drag.is_some() || self.drag_over {
            self.close_weather_popup();
            return;
        }
        let Some(context) = self.weather_popup_context() else {
            self.close_weather_popup();
            return;
        };
        if self
            .weather
            .popup
            .as_ref()
            .is_some_and(|p| !p.same_display(context))
        {
            self.close_weather_popup();
            return;
        }
        let data = self
            .weather
            .popup
            .as_ref()
            .filter(|p| p.needs_data(context))
            .map(|_| self.weather_popup_data(context));
        if data.is_some() {
            self.queue_weather_graphics_trim();
        }
        if let Some(popup) = &mut self.weather.popup
            && let Err(error) = popup.update(&self.gfx, context, data.as_ref())
        {
            crate::recovery::graphics_error(&error);
            muelle_shell::log::line(&format!("popup del clima: {error:#}"));
            self.close_weather_popup();
        }
    }
    pub(in crate::app) fn weather_popup_event(
        &mut self,
        source: HWND,
        event: isize,
    ) -> Result<Option<Outside>> {
        let Some(popup) = &mut self.weather.popup else {
            return Ok(None);
        };
        if popup.hwnd() != source {
            return Ok(None);
        }
        let Some(action) = popup.action(event) else {
            return Ok(None);
        };
        match action {
            Action::Close => self.close_weather_popup(),
            Action::Select(selection) => {
                let Some(context) = self.weather_popup_context() else {
                    return Ok(None);
                };
                let data = self.weather_popup_data_for(context, selection);
                self.queue_weather_graphics_trim();
                if let Some(popup) = &mut self.weather.popup
                    && let Err(error) = popup.update(&self.gfx, context, Some(&data))
                {
                    crate::recovery::graphics_error(&error);
                    muelle_shell::log::line(&format!("popup del clima: {error:#}"));
                    self.close_weather_popup();
                }
            }
        }
        self.render()?;
        Ok(None)
    }
}
