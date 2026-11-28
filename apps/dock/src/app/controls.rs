//! Opening and closing the control center and the tray menu, and what they
//! ask to open.

use std::{thread, time::Duration};

use anyhow::Result;
use muelle_shell::{launcher, system::primary_monitor, tray_host::TrayHost};
use windows::Win32::Foundation::RECT;

use crate::{
    calendar::{self, Calendar},
    control_center::{ControlCenter, Launch, Opening, Outcome, Projection, Tick},
    tray_menu::{self, TrayMenu},
};

use super::{App, Outside};

/// Long enough for a closed window to leave the screen before a capture.
const CAPTURE_DELAY: Duration = Duration::from_millis(250);

impl App {
    /// The menu bar's control center button or network icon, at `button` on
    /// screen, was pressed.
    pub(super) fn toggle_control_center(&mut self, button: RECT, opening: Opening) -> Result<()> {
        // The other item's replaces what is open.
        if self
            .control_center
            .as_ref()
            .is_some_and(|center| !center.is_closing() && center.opening() == opening)
        {
            self.close_control_center();
            return Ok(());
        }
        let Some(monitor) = primary_monitor() else {
            return Ok(());
        };
        // One still folding away gives way to the new one.
        self.control_center = None;
        self.close_calendar();
        self.close_preview();
        self.close_group_view();
        if let Some(bar) = &mut self.menubar {
            bar.dismiss_island(&self.gfx)?;
        }
        self.control_center = Some(ControlCenter::show(
            &self.gfx,
            monitor.screen,
            button,
            monitor.scale,
            opening,
            self.config.control_center_tiles,
        )?);
        if let Some(bar) = &mut self.menubar {
            match opening {
                Opening::Tiles => bar.set_control_center(&self.gfx, true)?,
                Opening::Network => bar.set_network_menu(&self.gfx, true)?,
            }
        }
        Ok(())
    }

    /// Closes it with its animation.
    pub(super) fn close_control_center(&mut self) {
        let Some(center) = &mut self.control_center else {
            return;
        };
        if center.is_closing() {
            return;
        }
        match center.begin_close() {
            Ok(true) => self.release_menu_bar(),
            Ok(false) => self.drop_control_center(),
            Err(error) => {
                muelle_shell::log::line(&format!("centro de control: {error:#}"));
                self.drop_control_center();
            }
        }
    }

    /// Removes it at once: when what it hangs from is changing.
    pub(super) fn drop_control_center(&mut self) {
        if self.control_center.take().is_some() {
            self.release_menu_bar();
        }
    }

    fn release_menu_bar(&mut self) {
        if let Some(bar) = &mut self.menubar
            && let Err(error) = bar
                .set_control_center(&self.gfx, false)
                .and_then(|()| bar.set_network_menu(&self.gfx, false))
        {
            muelle_shell::log::line(&format!("centro de control: {error:#}"));
        }
    }

    /// The notification area is listened to while Muelle's tray menu, on the
    /// menu bar, offers its icons.
    pub(super) fn sync_tray_host(&mut self) {
        if !self.config.menu_bar || !self.config.tray_menu {
            self.tray_host = None;
        } else if self.tray_host.is_none() {
            self.tray_host = TrayHost::start();
        }
    }

    /// The menu bar's arrow, at `button` on screen, was pressed. `false` if
    /// there is nothing to show, so Windows' own menu should open instead.
    pub(super) fn toggle_tray_menu(&mut self, button: RECT) -> Result<bool> {
        if self
            .tray_menu
            .as_ref()
            .is_some_and(|menu| !menu.is_closing())
        {
            self.close_tray_menu();
            return Ok(true);
        }
        // One still folding away gives way to the new one.
        self.tray_menu = None;
        self.close_calendar();
        let Some(monitor) = primary_monitor() else {
            return Ok(false);
        };
        if self
            .tray_host
            .as_ref()
            .is_none_or(|host| host.icons().is_empty())
        {
            return Ok(false);
        }
        self.close_preview();
        self.close_group_view();
        if let Some(bar) = &mut self.menubar {
            bar.dismiss_island(&self.gfx)?;
        }
        let Some(host) = &self.tray_host else {
            return Ok(false);
        };
        self.tray_menu = Some(TrayMenu::show(
            &self.gfx,
            monitor.screen,
            button,
            monitor.scale,
            host,
        )?);
        if let Some(bar) = &mut self.menubar {
            bar.set_tray_menu(&self.gfx, true)?;
        }
        Ok(true)
    }

    /// Closes it with its animation.
    pub(super) fn close_tray_menu(&mut self) {
        let Some(menu) = &mut self.tray_menu else {
            return;
        };
        if menu.is_closing() {
            return;
        }
        match menu.begin_close() {
            Ok(true) => self.release_tray_button(),
            Ok(false) => self.drop_tray_menu(),
            Err(error) => {
                muelle_shell::log::line(&format!("bandeja: {error:#}"));
                self.drop_tray_menu();
            }
        }
    }

    /// Removes it at once: when what it hangs from is changing.
    pub(super) fn drop_tray_menu(&mut self) {
        if self.tray_menu.take().is_some() {
            self.release_tray_button();
        }
    }

    fn release_tray_button(&mut self) {
        if let Some(bar) = &mut self.menubar
            && let Err(error) = bar.set_tray_menu(&self.gfx, false)
        {
            muelle_shell::log::line(&format!("bandeja: {error:#}"));
        }
    }

    pub(super) fn tray_menu_checked(&mut self, tick: Tick) {
        match tick {
            Tick::Keep => {}
            Tick::Close => self.close_tray_menu(),
            Tick::Closed => self.drop_tray_menu(),
        }
    }

    pub(super) fn follow_tray_menu(&mut self, outcome: tray_menu::Outcome) {
        match outcome {
            tray_menu::Outcome::Stay => {}
            tray_menu::Outcome::Close => self.close_tray_menu(),
            tray_menu::Outcome::Windows(anchor) => {
                self.close_tray_menu();
                launcher::toggle_hidden_icons(anchor);
                if let Some(bar) = &mut self.menubar
                    && let Err(error) = bar.follow_windows_tray(&self.gfx)
                {
                    muelle_shell::log::line(&format!("bandeja: {error:#}"));
                }
            }
        }
    }

    /// The menu bar's clock, at `button` on screen, was pressed.
    pub(super) fn toggle_calendar(&mut self, button: RECT) -> Result<()> {
        if self
            .calendar
            .as_ref()
            .is_some_and(|calendar| !calendar.is_closing())
        {
            self.close_calendar();
            return Ok(());
        }
        // One still folding away gives way to the new one.
        self.calendar = None;
        let Some(monitor) = primary_monitor() else {
            return Ok(());
        };
        self.close_control_center();
        self.close_tray_menu();
        self.close_preview();
        self.close_group_view();
        if let Some(bar) = &mut self.menubar {
            bar.dismiss_island(&self.gfx)?;
        }
        self.calendar = Some(Calendar::show(
            &self.gfx,
            monitor.screen,
            button,
            monitor.scale,
            self.config.calendar_week_start,
        )?);
        if let Some(bar) = &mut self.menubar {
            bar.set_calendar(&self.gfx, true)?;
        }
        Ok(())
    }

    /// Closes it with its animation.
    pub(super) fn close_calendar(&mut self) {
        let Some(calendar) = &mut self.calendar else {
            return;
        };
        if calendar.is_closing() {
            return;
        }
        match calendar.begin_close() {
            Ok(true) => self.release_clock(),
            Ok(false) => self.drop_calendar(),
            Err(error) => {
                muelle_shell::log::line(&format!("calendario: {error:#}"));
                self.drop_calendar();
            }
        }
    }

    /// Removes it at once: when what it hangs from is changing.
    pub(super) fn drop_calendar(&mut self) {
        if self.calendar.take().is_some() {
            self.release_clock();
        }
    }

    fn release_clock(&mut self) {
        if let Some(bar) = &mut self.menubar
            && let Err(error) = bar.set_calendar(&self.gfx, false)
        {
            muelle_shell::log::line(&format!("calendario: {error:#}"));
        }
    }

    pub(super) fn calendar_checked(&mut self, tick: Tick) {
        match tick {
            Tick::Keep => {}
            Tick::Close => self.close_calendar(),
            Tick::Closed => self.drop_calendar(),
        }
    }

    /// What a click on the calendar asks for, if it takes more than the
    /// calendar itself.
    pub(super) fn follow_calendar(&self, outcome: calendar::Outcome) -> Option<Outside> {
        let calendar::Outcome::Menu { anchor, origin } = outcome else {
            return None;
        };
        Some(Outside::CalendarMenu {
            gfx: self.gfx.clone(),
            anchor,
            dark: !crate::appearance::menu_bar_light(),
            spring: muelle_shell::system::animations_enabled().then_some(origin),
        })
    }

    pub(super) fn control_center_checked(&mut self, tick: Tick) {
        match tick {
            Tick::Keep => {}
            Tick::Close => self.close_control_center(),
            Tick::Closed => self.drop_control_center(),
        }
    }

    pub(super) fn follow_control_center(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Stay => {}
            Outcome::Close => self.close_control_center(),
            Outcome::Launch(launch) => {
                // A picture of the screen must not include it.
                if launch.captures() {
                    self.drop_control_center();
                } else {
                    self.close_control_center();
                }
                open(launch);
            }
        }
    }
}

fn open(launch: Launch) {
    let page = match launch {
        Launch::NetworkList => return launcher::open_network_list(),
        Launch::BluetoothSettings => return launcher::open_bluetooth(),
        Launch::TaskView => return launcher::open_task_view(),
        Launch::NewDesktop => return launcher::new_desktop(),
        Launch::PreviousDesktop => return launcher::switch_desktop(false),
        Launch::NextDesktop => return launcher::switch_desktop(true),
        Launch::ShowDesktop => return launcher::show_desktop(),
        Launch::Cast => return launcher::open_cast(),
        Launch::QuickSettings => return launcher::open_quick_settings(),
        Launch::Project(projection) => return project(projection),
        Launch::Screenshot => {
            return after_closing(|| {
                if let Err(error) = launcher::open("ms-screenclip:") {
                    muelle_shell::log::line(&format!("centro de control, captura: {error}"));
                }
            });
        }
        Launch::CaptureScreen => return after_closing(launcher::capture_screen),
        Launch::RecordScreen => return after_closing(launcher::record_screen),
        Launch::WifiSettings => "ms-settings:network-wifi",
        Launch::NearbySharing => "ms-settings:crossdevice",
        Launch::Focus => "ms-settings:quiethours",
        Launch::Notifications => "ms-settings:notifications",
        Launch::DisplaySettings => "ms-settings:display",
        Launch::NightLight => "ms-settings:nightlight",
        Launch::Personalize => "ms-settings:personalization-colors",
        Launch::SoundSettings => "ms-settings:sound",
        Launch::SnippingTool => "ms-screensketch:",
        Launch::Calculator => "calc.exe",
        Launch::Timer => "ms-clock:",
    };
    if let Err(error) = launcher::open(page) {
        muelle_shell::log::line(&format!("centro de control, abrir {page}: {error}"));
    }
}

/// Runs `capture` once the closed window is off the screen, without keeping
/// the dock waiting.
fn after_closing(capture: impl FnOnce() + Send + 'static) {
    thread::spawn(move || {
        thread::sleep(CAPTURE_DELAY);
        capture();
    });
}

/// Windows' own projection switch, as Win+P offers it.
fn project(projection: Projection) {
    let mode = match projection {
        Projection::ThisScreen => "/internal",
        Projection::Duplicate => "/clone",
        Projection::Extend => "/extend",
        Projection::SecondScreen => "/external",
    };
    if let Err(error) = std::process::Command::new("DisplaySwitch.exe")
        .arg(mode)
        .spawn()
    {
        muelle_shell::log::line(&format!("centro de control, pantallas: {error}"));
    }
}
