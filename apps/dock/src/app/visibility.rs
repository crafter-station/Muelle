//! Monitor edges, intelligent hiding, fullscreen and reveal behavior.

use std::time::Instant;

use anyhow::Result;
use muelle_config::DockPosition;
use muelle_core::{frame::DockEdge, layout::Rect, spring::Spring};
use muelle_shell::{
    appbar::ABN_FULLSCREENAPP,
    system::{Monitor, monitor_at, monitor_rects, primary_monitor},
    taskbar, tracker,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
        Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute},
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON},
            WindowsAndMessaging::{GetCursorPos, KillTimer, PostMessageW, SetTimer},
        },
    },
    core::BOOL,
};
use windows_future::AsyncActionCompletedHandler;

use crate::{
    edge_watch::{self, EdgeWatch, Reveal, Target as EdgeTarget},
    menubar::MenuBar,
};

use super::{
    App, DOCK_MOVED, EDGE_TIMER, FULLSCREEN_RECHECK_MS, FULLSCREEN_TIMER, HIDE_DELAY_MS,
    HIDE_TIMER, HOVER_OMEGA, millis,
};

/// Whether intelligent hiding keeps the dock away, from what is in front:
/// `Some(overlaps)` for an app the dock shows, which hides it when it reaches
/// into the dock. The desktop shows it. Anything else (Windows Search, the
/// notification center, quick settings, the notification area flyout, a
/// launcher such as Command Palette, mostly opened from the menu bar) comes
/// and goes over the app that stays, so it keeps the dock as it was instead
/// of bringing it up over that app.
fn smart_covered(app_overlap: Option<bool>, desktop: bool, was_hidden: bool) -> bool {
    match app_overlap {
        Some(overlaps) => overlaps,
        None if desktop => false,
        None => was_hidden,
    }
}

/// The display the dock was brought to, if it is still connected exactly as
/// it was. Unplugged, turned off by Windows+P ("PC screen only") or moved in
/// the arrangement, it is forgotten: the dock goes back to the primary display
/// instead of jumping to it again, or to whatever display now covers its old
/// place, when displays change.
pub(super) fn remembered_display(remembered: Option<RECT>, monitors: &[RECT]) -> Option<RECT> {
    remembered.filter(|screen| monitors.contains(screen))
}

pub(super) fn rects_overlap(a: RECT, b: RECT) -> bool {
    a.left < a.right
        && a.top < a.bottom
        && b.left < b.right
        && b.top < b.bottom
        && a.left < b.right
        && b.left < a.right
        && a.top < b.bottom
        && b.top < a.bottom
}

impl App {
    pub(super) fn hide_if_unused(&mut self) -> Result<()> {
        // One decision for timer, settings and overlap changes. Automatic
        // hiding wins; intelligent hiding matters only in its absence.
        let hidden = self.config.auto_hide || (self.config.smart_hide && self.smart_hidden);
        if hidden
            && (self.mascot.is_some()
                || self.weather_holds_open()
                || self.menu_open
                || self.press.is_some()
                || self.drag_over
                || self.hovering
                || self.drag.is_some()
                || self.switching
                || self.preview.is_some()
                || self.group_view.is_some())
        {
            return Ok(());
        }
        if !hidden {
            // A queued hide must not survive returning to the desktop or
            // disabling a policy. Other interactions keep their own timers.
            unsafe {
                let _ = KillTimer(Some(self.input), HIDE_TIMER);
            }
        }
        let target = if hidden { 1.0 } else { 0.0 };
        if self.slide.target == target {
            return Ok(());
        }
        self.slide.target = target;
        self.last_frame = Instant::now();
        self.place_windows()
    }

    /// The screen edge chosen in Settings.
    pub(super) fn position(&self) -> DockEdge {
        match self.config.dock_position {
            DockPosition::Bottom => DockEdge::Bottom,
            DockPosition::Left => DockEdge::Left,
            DockPosition::Right => DockEdge::Right,
        }
    }

    /// How much of `screen`'s top the menu bar keeps, for a dock on a side
    /// edge to start below it. Zero on the bottom edge (not needed there),
    /// when the bar is off or hides itself, and on other displays (the bar
    /// is on the primary one).
    pub(super) fn menu_bar_inset(&self, screen: RECT, edge: DockEdge) -> i32 {
        if !edge.is_vertical() {
            return 0;
        }
        // From the settings, not the live bar: at startup the dock is placed
        // before the bar exists, and it must not move once the bar appears.
        if !self.config.menu_bar || self.config.menu_bar_auto_hide {
            return 0;
        }
        // With a copy of the bar on every display, each display has one.
        let display = if self.config.menu_bar_all_displays {
            monitor_at(POINT {
                x: screen.left + (screen.right - screen.left) / 2,
                y: screen.top + (screen.bottom - screen.top) / 2,
            })
        } else {
            primary_monitor()
        };
        display
            .filter(|display| display.screen == screen)
            .map_or(0, |display| MenuBar::strip_height(display.scale))
    }

    pub(super) fn dock_monitor(&self) -> Option<Monitor> {
        self.dock_display
            .filter(|_| self.config.hides_automatically())
            .and_then(|screen| {
                monitor_at(POINT {
                    x: screen.left + (screen.right - screen.left) / 2,
                    y: screen.top + (screen.bottom - screen.top) / 2,
                })
            })
            // The same display, not another one now at its place.
            .filter(|monitor| Some(monitor.screen) == self.dock_display)
            .or_else(primary_monitor)
    }

    /// Runs after a settings change and after the displays change (Windows+P,
    /// a monitor plugged in or out, a new arrangement).
    pub(super) fn apply_edge_settings(&mut self) {
        self.configure_edge_watcher(monitor_rects());
    }

    fn configure_edge_watcher(&mut self, monitors: Vec<RECT>) {
        let remembered = remembered_display(self.dock_display, &monitors);
        if remembered != self.dock_display {
            muelle_shell::log::line(&format!(
                "pantallas: {}; el dock vuelve a la pantalla principal",
                monitors.len()
            ));
        }
        self.dock_display = remembered.filter(|_| self.config.hides_automatically());
        let screens = if self.config.hides_automatically() {
            monitors
        } else {
            Vec::new()
        };
        // Even one display needs a reveal path independent of WM_MOUSEMOVE:
        // the desktop can cover the input strip. Reuse the shared edge observer
        // to raise our own windows on dwell, without activating another window.
        let watch_dock = !screens.is_empty();
        edge_watch::set_dock_monitors(screens, self.position());
        // Both hiding modes of the menu bar reveal it at the top edge; the
        // intelligent one while a window covers it (issue #61).
        let enabled = watch_dock
            || (self.menubar.is_some()
                && (self.config.menu_bar_auto_hide || self.config.menu_bar_smart_hide));
        if enabled && self.edge_watch.is_none() {
            self.edge_watch = EdgeWatch::start(self.visual)
                .inspect_err(|error| {
                    muelle_shell::log::line(&format!("bordes de pantalla: {error:#}"))
                })
                .ok();
        } else if !enabled && self.edge_watch.take().is_some() {
            // SAFETY: This timer belongs to our UI thread.
            unsafe {
                let _ = KillTimer(Some(self.visual), EDGE_TIMER);
            }
        }
    }

    pub(super) fn edge_changed(&mut self) -> Result<()> {
        // Explorer can recreate a secondary taskbar while the dock is running.
        // Check on monitor entry so it is transparent before the edge dwell.
        if self.config.hides_automatically()
            && self.taskbar_worker.is_none()
            && taskbar::needs_visibility_repair()
        {
            self.request_taskbar(false)?;
        }
        let (target, wait) = edge_watch::pending();
        let on_dock_edge = matches!(target, Some(EdgeTarget::Dock(screen))
            if self.stage.as_ref().is_some_and(|stage| stage.key.screen == screen));
        self.arm_edge_timer(wait);
        if !on_dock_edge
            && self.config.hides_automatically()
            && !self.hovering
            && self.slide.target == 0.0
        {
            // SAFETY: Existing hide timer on the dock input window.
            unsafe {
                SetTimer(Some(self.input), HIDE_TIMER, HIDE_DELAY_MS, None);
            }
        }
        Ok(())
    }

    pub(super) fn arm_edge_timer(&self, wait: Option<std::time::Duration>) {
        // SAFETY: One-shot timer on the window receiving edge notifications.
        unsafe {
            let _ = KillTimer(Some(self.visual), EDGE_TIMER);
            if let Some(wait) = wait {
                SetTimer(Some(self.visual), EDGE_TIMER, millis(wait).max(1), None);
            }
        }
    }

    pub(super) fn edge_rested(&mut self) -> Result<()> {
        self.arm_edge_timer(None);
        if self.edge_watch.is_none() {
            return Ok(());
        }
        let mut point = POINT::default();
        // SAFETY: Valid output for this call.
        unsafe { GetCursorPos(&mut point)? };
        match edge_watch::poll(point) {
            Reveal::Wait(wait) => self.arm_edge_timer(Some(wait)),
            Reveal::Ready(EdgeTarget::MenuBar(id)) => {
                if let Some(bar) = self.menubar.as_mut().and_then(|bar| bar.bar_mut(id)) {
                    bar.edge_rested(&self.gfx, point)?;
                }
            }
            Reveal::Ready(EdgeTarget::Dock(screen)) => self.reveal_dock(point, screen)?,
            Reveal::Idle => self.edge_changed()?,
        }
        Ok(())
    }

    pub(super) fn reveal_dock(&mut self, point: POINT, screen: RECT) -> Result<()> {
        // SAFETY: Read-only mouse button state.
        let mouse_down = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
            .into_iter()
            .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) < 0 });
        // Why the dock stays away, if it does.
        let refused = if !self.config.hides_automatically() {
            Some("no se oculta")
        } else if self.hovering {
            Some("puntero sobre el dock")
        } else if self.menu_open || self.press.is_some() || self.drag_over || self.drag.is_some() {
            Some("menú o arrastre en curso")
        } else if self.switching {
            Some("Alt+Tab")
        } else if !self.flights.is_empty() {
            Some("animación de ventana")
        } else if mouse_down {
            Some("botón del ratón pulsado")
        } else if monitor_at(point).is_none_or(|monitor| monitor.screen != screen) {
            Some("el puntero ya no está en esa pantalla")
        } else if tracker::foreground_is_fullscreen(screen) {
            Some("pantalla completa")
        } else {
            None
        };
        if let Some(reason) = refused {
            // Only for another display, where nothing else shows why the
            // dock did not come: one line per rest of the pointer.
            if self
                .stage
                .as_ref()
                .is_some_and(|stage| stage.key.screen != screen)
            {
                muelle_shell::log::line(&format!(
                    "el dock no pasa a la pantalla {},{}: {reason} (al frente: {})",
                    screen.left,
                    screen.top,
                    tracker::foreground_class()
                ));
            }
            return Ok(());
        }
        let switched = self
            .stage
            .as_ref()
            .is_none_or(|stage| stage.key.screen != screen);
        if switched {
            self.close_preview();
            self.stop_mascot();
            self.dock_display = Some(screen);
            muelle_shell::log::line(&format!(
                "el dock pasa a la pantalla {},{} {}x{}",
                screen.left,
                screen.top,
                screen.right - screen.left,
                screen.bottom - screen.top
            ));
            self.amount = Spring::at(0.0, HOVER_OMEGA);
            self.slide = Spring::at(1.0, self.config.dock_auto_hide_speed.omega());
            self.fullscreen = false;
            // Windows moves the window at once but shows its new content a
            // few frames later: meanwhile the old dock, at the other
            // display's scale, flashed at the new place before sliding in.
            // Keep the window out of view until the new content is there.
            self.set_moving(true);
            if let Err(error) = self
                .refresh_dock()
                .and_then(|()| self.on_commit(DOCK_MOVED))
            {
                self.set_moving(false);
                return Err(error);
            }
        }
        if self
            .stage
            .as_ref()
            .is_some_and(|stage| stage.spans_reveal(point))
        {
            return self.on_mouse_move();
        }
        self.slide.target = 0.0;
        self.last_frame = Instant::now();
        // SAFETY: Leaving the edge or entering the dock re-arms the hide
        // delay. Cancel any delay left over from the previous monitor.
        unsafe {
            let _ = KillTimer(Some(self.input), HIDE_TIMER);
        }
        self.place_windows()?;
        self.render()
    }

    /// Keeps the dock's window out of view, or shows it again. Cloaking
    /// leaves the window where it is and still taking the pointer.
    pub(super) fn set_moving(&mut self, moving: bool) {
        if self.moving_since.is_some() == moving {
            return;
        }
        self.moving_since = moving.then(Instant::now);
        let cloak = BOOL::from(moving);
        // SAFETY: Our own window; the value outlives the call.
        unsafe {
            let _ = DwmSetWindowAttribute(
                self.visual,
                DWMWA_CLOAK,
                (&raw const cloak).cast(),
                size_of::<BOOL>() as u32,
            );
        }
    }

    /// Posts `message` to the dock once the compositor has taken everything
    /// changed so far.
    fn on_commit(&self, message: u32) -> Result<()> {
        let input = self.input.0 as isize;
        self.gfx.compositor.RequestCommitAsync()?.SetCompleted(
            &AsyncActionCompletedHandler::new(move |_, _| {
                // SAFETY: Only a message with no data is posted to our window.
                unsafe {
                    let _ =
                        PostMessageW(Some(HWND(input as *mut _)), message, WPARAM(0), LPARAM(0));
                }
                Ok(())
            }),
        )?;
        Ok(())
    }

    pub(super) fn update_smart_hiding(&mut self) -> Result<()> {
        // Paused under automatic hiding: no competing overlap controller and
        // no native geometry query for a policy that cannot affect visibility.
        let covered = !self.config.auto_hide
            && self.config.smart_hide
            && self.stage.as_ref().is_some_and(|stage| {
                let front = tracker::foreground_window();
                let app_overlap =
                    self.windows
                        .iter()
                        .any(|window| window.hwnd == front)
                        .then(|| {
                            // What shows of it: its invisible resize borders
                            // reaching into the dock must not hide it.
                            tracker::visible_rect(front).is_some_and(|rect| {
                                rects_overlap(
                                    stage.screen_rect(Rect {
                                        x: 0.0,
                                        y: 0.0,
                                        width: stage.layout.width,
                                        height: stage.layout.height,
                                    }),
                                    rect,
                                )
                            })
                        });
                smart_covered(
                    app_overlap,
                    tracker::is_wallpaper(HWND(front as *mut _)),
                    self.smart_hidden,
                )
            });
        if covered != self.smart_hidden {
            self.smart_hidden = covered;
            self.hide_if_unused()?;
            self.render()?;
        }
        Ok(())
    }

    pub(super) fn update_fullscreen(&mut self) -> Result<()> {
        // The window in front is where the user works: note where it is now.
        let front = tracker::foreground_window();
        if self.windows.iter().any(|window| window.hwnd == front) {
            self.remember_rect(front);
        }
        let Some(monitor) = primary_monitor() else {
            return Ok(());
        };
        let bar_fullscreen = tracker::foreground_is_fullscreen(monitor.screen);
        let mut mirror_fullscreen = false;
        let fullscreen = self.dock_monitor().map_or(bar_fullscreen, |dock| {
            if dock.screen == monitor.screen {
                bar_fullscreen
            } else {
                tracker::foreground_is_fullscreen(dock.screen)
            }
        });
        if let Some(bar) = &mut self.menubar {
            // An auxiliary bar failure must not strand a hidden dock.
            if let Err(error) = bar.set_visible(!bar_fullscreen) {
                crate::recovery::graphics_error(&error);
                muelle_shell::log::line(&format!("visibilidad de la barra superior: {error:#}"));
            }
            // Runs when a window comes to the front and once it stops moving:
            // its title bar must not open, or be dropped, under the bar.
            for strip in bar.reserved_strips() {
                tracker::keep_below(front, strip);
            }
            // Intelligent hiding (issue #61): the same test as the dock's,
            // against the bar's strip.
            let covered = self.config.menu_bar_smart_hide
                && self.windows.iter().any(|window| window.hwnd == front)
                && tracker::visible_rect(front)
                    .is_some_and(|rect| rects_overlap(bar.bounds(), rect));
            if let Err(error) = bar.set_covered(&self.gfx, covered) {
                muelle_shell::log::line(&format!("ocultación inteligente de la barra: {error:#}"));
            }
            // The copies on the other displays, each for its own display.
            let front_rect = (self.config.menu_bar_smart_hide
                && self.windows.iter().any(|window| window.hwnd == front))
            .then(|| tracker::visible_rect(front))
            .flatten();
            for (display, mirror) in bar.mirrors_mut() {
                let full = tracker::foreground_is_fullscreen(display);
                mirror_fullscreen |= full;
                let result = mirror.set_visible(!full).and_then(|()| {
                    mirror.set_covered(
                        &self.gfx,
                        front_rect.is_some_and(|rect| rects_overlap(mirror.bounds(), rect)),
                    )
                });
                if let Err(error) = result {
                    muelle_shell::log::line(&format!("barra superior en otra pantalla: {error:#}"));
                }
            }
        }
        // Windows sometimes omits the shell event when a full-screen app
        // closes or switches desktops. Poll only while our UI is hidden.
        // SAFETY: `self.input` is a live window on this UI thread.
        unsafe {
            if fullscreen || ((bar_fullscreen || mirror_fullscreen) && self.menubar.is_some()) {
                SetTimer(
                    Some(self.input),
                    FULLSCREEN_TIMER,
                    FULLSCREEN_RECHECK_MS,
                    None,
                );
            }
        }
        if fullscreen != self.fullscreen {
            muelle_shell::log::line(&format!(
                "{} por pantalla completa (al frente: {})",
                if fullscreen { "se oculta" } else { "vuelve" },
                tracker::foreground_class()
            ));
            let previous = self.fullscreen;
            self.fullscreen = fullscreen;
            if fullscreen {
                self.stop_mascot();
                self.close_preview();
                self.close_group_view();
                self.hovering = false;
                self.amount = Spring::at(0.0, HOVER_OMEGA);
                self.press = None;
                self.drag = None;
                self.ghost = None;
                self.stop_drag_input();
            }
            if let Err(error) = self.place_windows() {
                self.fullscreen = previous;
                return Err(error);
            }
        }
        if !fullscreen && (!bar_fullscreen || self.menubar.is_none()) {
            // SAFETY: This UI thread owns the timer.
            unsafe {
                let _ = KillTimer(Some(self.input), FULLSCREEN_TIMER);
            }
        }
        self.update_smart_hiding()?;
        // Updating an app name can fail transiently; it must never prevent
        // the dock or bar from returning after full-screen mode.
        if let Err(error) = self.update_menu_bar_app() {
            eprintln!("muelle: nombre de la barra superior: {error:#}");
        }
        self.sync_companion();
        self.sync_weather();
        Ok(())
    }

    pub(super) fn on_appbar_notice(&mut self, notice: usize) -> Result<()> {
        match notice {
            // Windows notifications may be missing or unreliable, so use them
            // only as a signal to recheck the state.
            ABN_FULLSCREENAPP => self.update_fullscreen(),
            // The dock's own reservation can trigger this. The strip changes
            // only with the screen or dock size, already handled by `refresh`.
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;

#[cfg(test)]
mod display_tests {
    use super::*;

    #[test]
    fn a_display_that_is_gone_or_moved_is_forgotten() {
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        let laptop = rect(0, 0, 1920, 1080);
        let external = rect(1920, 0, 4480, 1440);
        // Still connected: the dock stays on it.
        assert_eq!(
            remembered_display(Some(external), &[laptop, external]),
            Some(external)
        );
        // Windows+P "PC screen only", or unplugged: back to the primary.
        assert_eq!(remembered_display(Some(external), &[laptop]), None);
        // Extended again later: the dock does not jump back by itself.
        // Rearranged to the left of the laptop: another place, forgotten.
        assert_eq!(
            remembered_display(Some(external), &[laptop, rect(-2560, 0, 0, 1440)]),
            None
        );
        assert_eq!(remembered_display(None, &[laptop, external]), None);
    }
}
