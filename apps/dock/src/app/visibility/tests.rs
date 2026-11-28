//! Regression harness: hidden test-owned HWNDs, no taskbar takeover or settings writes.
use super::super::{ASSETS_READY, App, HOVER_OMEGA, Press, PreviewOrder};
use crate::{assets::Visuals, gfx::Gfx, thumbnails::Thumbnails};
use anyhow::Result;
use muelle_config::Config;
use muelle_core::{attention::Attention, roster::Roster, spring::Spring};
use muelle_shell::{power, tracker::WindowTracker};
use std::{collections::HashMap, time::Instant};
use windows::{
    Win32::{
        Foundation::{HWND, POINT},
        System::WinRT::{
            Composition::ICompositorDesktopInterop, CreateDispatcherQueueController, DQTAT_COM_STA,
            DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
            WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
    core::{Interface, w},
};

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
pub(in crate::app) struct Fixture {
    pub(in crate::app) app: App,
    _windows: [Window; 2],
}
impl Fixture {
    pub(in crate::app) fn new() -> Result<Self> {
        let create = || unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("Muelle hiding regression"),
                WS_POPUP,
                -32000,
                -32000,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .map(Window)
        };
        let windows = [create()?, create()?];
        let visual = windows[0].0;
        let input = windows[1].0;
        let gfx = Gfx::new()?;
        let target = unsafe {
            gfx.compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(visual, true)?
        };
        let config = Config::default();
        let app = App {
            visual,
            input,
            gfx,
            target,
            appbar: None,
            taskbar_worker: None,
            taskbar_retry: false,
            taskbar_attempts: 0,
            fullscreen: false,
            smart_hidden: false,
            moving_since: None,
            roster: Roster::default(),
            tracker: WindowTracker::default(),
            windows: Vec::new(),
            preview_order: PreviewOrder::default(),
            last_rects: HashMap::new(),
            entries: Vec::new(),
            icons: Visuals::new(visual, ASSETS_READY)?,
            downloads: None,
            bin_full: false,
            _bin_watch: None,
            _saver_watch: power::SaverWatch::new(input.0 as isize),
            pin_watch: None,
            badge_watch: None,
            attention: Attention::default(),
            attention_enabled: false,
            assets: HashMap::new(),
            assets_for: None,
            placeholder: None,
            remove_label: None,
            config,
            slide: Spring::at(0.0, 24.0),
            press: None,
            drag: None,
            incoming: Vec::new(),
            incoming_pin: None,
            drag_over: false,
            spring: None,
            spring_opened_group: false,
            switching: false,
            switch_started: Instant::now(),
            switch_ended: None,
            launched: Instant::now(),
            update_tried: None,
            follow: None,
            pointed: false,
            pending_drop: None,
            bounces: HashMap::new(),
            minimized: Vec::new(),
            thumbnails: Thumbnails::new(visual),
            flights: Vec::new(),
            snapshots: Vec::new(),
            menubar: None,
            edge_watch: None,
            dock_display: None,
            names: HashMap::new(),
            calmed: Vec::new(),
            started: false,
            stage: None,
            hovering: false,
            hovered: None,
            preview_candidate: None,
            preview: None,
            group_view: None,
            group_was_open: None,
            ghost: None,
            menu_open: false,
            mascot: None,
            companion: super::super::companion::State::default(),
            weather: super::super::weather::State::default(),
            // Initial registration notifications establish the baseline after
            // the queue starts, without playing a startup charging/saver act.
            power_seen: None,
            charging_shown: None,
            moments: crate::moments::Moments::default(),
            welcome_tries: 0,
            cursor: 0.0,
            amount: Spring::at(0.0, HOVER_OMEGA),
            last_frame: Instant::now(),
        };
        Ok(Self {
            app,
            _windows: windows,
        })
    }
}
pub(in crate::app) fn queue() -> windows::core::Result<windows::System::DispatcherQueueController> {
    unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
    }
}

#[test]
#[ignore = "Native hiding regression; only hidden test-owned windows, no user settings writes"]
fn native_hiding_reconciles_all_modes_when_the_desktop_returns() -> Result<()> {
    let _queue = queue()?;
    let mut fixture = Fixture::new()?;
    let app = &mut fixture.app;
    for auto in [false, true] {
        for smart in [false, true] {
            for overlap in [false, true] {
                app.config.auto_hide = auto;
                app.config.smart_hide = smart;
                app.smart_hidden = !auto && smart && overlap;
                let hidden = auto || (smart && overlap);
                app.slide = Spring::at(if hidden { 0.0 } else { 1.0 }, 24.0);
                app.hide_if_unused()?;
                assert_eq!(
                    app.slide.target,
                    if hidden { 1.0 } else { 0.0 },
                    "auto={auto}, smart={smart}, overlap={overlap}"
                );
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "Native hiding regression; only hidden test-owned windows, no user settings writes"]
fn native_hiding_does_not_interrupt_a_pending_press_or_open_menu() -> Result<()> {
    let _queue = queue()?;
    let mut fixture = Fixture::new()?;
    let app = &mut fixture.app;
    for auto in [false, true] {
        app.config.auto_hide = auto;
        app.config.smart_hide = true;
        app.smart_hidden = !auto;
        app.slide = Spring::at(0.0, 24.0);
        app.press = Some(Press {
            id: "test-app".into(),
            at: POINT::default(),
            draggable: true,
        });
        app.hide_if_unused()?;
        assert_eq!(
            app.slide.target, 0.0,
            "a pending icon press must keep the dock visible"
        );
        app.press = None;
        app.menu_open = true;
        app.hide_if_unused()?;
        assert_eq!(
            app.slide.target, 0.0,
            "an open menu must keep the dock visible"
        );
        app.menu_open = false;
        app.hide_if_unused()?;
        assert_eq!(
            app.slide.target, 1.0,
            "hiding resumes once the interaction ends"
        );
        // A hide timeout can fire during the press. Releasing or cancelling
        // must rearm that same one-shot delay instead of leaving the dock up.
        app.slide = Spring::at(0.0, 24.0);
        app.press = Some(Press {
            id: "test-app".into(),
            at: POINT::default(),
            draggable: false,
        });
        assert!(app.on_release()?.is_none());
        assert!(unsafe {
            windows::Win32::UI::WindowsAndMessaging::KillTimer(
                Some(app.input),
                super::super::HIDE_TIMER,
            )
            .is_ok()
        });
        app.press = Some(Press {
            id: "test-app".into(),
            at: POINT::default(),
            draggable: false,
        });
        app.cancel_drag()?;
        assert!(unsafe {
            windows::Win32::UI::WindowsAndMessaging::KillTimer(
                Some(app.input),
                super::super::HIDE_TIMER,
            )
            .is_ok()
        });
    }
    Ok(())
}

#[test]
#[ignore = "Native hiding regression; only hidden test-owned windows, no user settings writes"]
fn native_hiding_clears_overlap_when_automatic_hiding_takes_precedence() -> Result<()> {
    let _queue = queue()?;
    let mut fixture = Fixture::new()?;
    let app = &mut fixture.app;
    app.config.auto_hide = true;
    app.config.smart_hide = true;
    app.smart_hidden = true; // Previous smart-only mode.
    app.slide = Spring::at(1.0, 24.0);
    app.update_smart_hiding()?;
    assert!(
        !app.smart_hidden,
        "automatic mode must not retain intelligent overlap state"
    );
    assert_eq!(
        app.slide.target, 1.0,
        "the desktop does not override automatic hiding"
    );
    app.config.auto_hide = false;
    app.hide_if_unused()?;
    assert_eq!(
        app.slide.target, 0.0,
        "smart-only mode shows an unobstructed desktop"
    );
    Ok(())
}

#[test]
fn flyouts_keep_the_dock_as_it_was_and_the_desktop_shows_it() {
    use super::smart_covered;
    for was_hidden in [false, true] {
        // An app the dock shows decides by itself.
        assert!(smart_covered(Some(true), false, was_hidden));
        assert!(!smart_covered(Some(false), false, was_hidden));
        // The desktop in front shows the dock.
        assert!(!smart_covered(None, true, was_hidden));
        // A flyout or a launcher's popup (from the menu bar) changes nothing.
        assert_eq!(smart_covered(None, false, was_hidden), was_hidden);
    }
}

#[test]
#[ignore = "Offscreen weather input only; no network, location or live Dock changes"]
fn native_weather_hover_keeps_the_dock_open_and_disabling_releases_it() -> Result<()> {
    use crate::{
        appearance::{Look, Material},
        gfx::WeatherGlyph,
        weather::{Card, Key, Model, Placement},
    };
    use windows::Win32::Foundation::RECT;
    let _queue = queue()?;
    let mut fixture = Fixture::new()?;
    let app = &mut fixture.app;
    app.config.weather = true;
    app.config.auto_hide = true;
    app.config.smart_hide = false;
    app.slide = Spring::at(0.0, 24.0);
    let key = Key {
        placement: Placement {
            bounds: RECT {
                left: -32000,
                top: -32000,
                right: -31760,
                bottom: -31942,
            },
            compact: false,
        },
        screen: RECT {
            left: -33000,
            top: -33000,
            right: -30000,
            bottom: -30000,
        },
        look: Look {
            material: Material::Standard,
            light: false,
            glass: false,
        },
        scale: 1.0,
        revision: 0,
        language: muelle_config::Language::En,
    };
    let make_card = |gfx: &Gfx| {
        Card::new(
            gfx,
            key,
            &Model {
                degrees: "17°".into(),
                title: "Partly cloudy".into(),
                subtitle: "Current location".into(),
                glyph: WeatherGlyph::PartlyCloudy,
            },
        )
    };
    app.weather.card = Some(make_card(&app.gfx)?);
    let source = app
        .weather
        .card
        .as_ref()
        .map(Card::hwnd)
        .unwrap_or_default();
    app.weather_event(app.input, 1)?;
    assert!(
        !app.weather_holds_open(),
        "ignore an event from a stale/foreign input window"
    );
    app.hovering = true;
    app.amount.target = 1.0;
    app.weather_event(source, 1)?;
    assert!(!app.hovering);
    assert_eq!(
        app.amount.target, 0.0,
        "weather stops app magnification without losing its reveal hold"
    );
    // This fixture deliberately owns no Stage. Rendering the hover exit must
    // discard its orphaned card rather than retain an invisible reveal hold.
    assert!(app.weather.card.is_none());
    assert!(!app.weather_holds_open());
    app.weather.card = Some(make_card(&app.gfx)?);
    let source = app
        .weather
        .card
        .as_ref()
        .map(Card::hwnd)
        .unwrap_or_default();
    app.weather_event(source, 1)?;
    app.hide_if_unused()?;
    assert!(app.weather_holds_open());
    assert_eq!(app.slide.target, 0.0);
    app.weather_after_menu(source);
    assert!(
        app.weather_holds_open(),
        "restore tracking when a menu releases capture"
    );
    app.weather_after_menu(app.input);
    assert!(
        !app.weather_holds_open(),
        "moving to another window must release the weather hold"
    );
    app.weather_event(source, 0)?;
    app.hide_if_unused()?;
    assert!(!app.weather_holds_open());
    assert_eq!(app.slide.target, 1.0);
    app.config.weather = false;
    app.sync_weather();
    assert!(app.weather.card.is_none());
    assert!(!app.weather_holds_open());
    Ok(())
}

#[test]
#[ignore = "Hidden owned forecast popup: no focus, capture, network request or live Dock changes"]
fn native_forecast_popup_holds_autohide_ignores_foreign_events_and_closes() -> Result<()> {
    use crate::{
        appearance::{Look, Material},
        weather::popup::{CLOSE, Context, Popup, data::Data},
    };
    use muelle_core::frame::DockEdge;
    use windows::Win32::Foundation::RECT;
    let _queue = queue()?;
    let mut fixture = Fixture::new()?;
    let app = &mut fixture.app;
    app.config.auto_hide = true;
    app.config.smart_hide = false;
    let context = Context {
        anchor: RECT {
            left: -32000,
            top: -31000,
            right: -31780,
            bottom: -30952,
        },
        screen: RECT {
            left: -33000,
            top: -33000,
            right: -30000,
            bottom: -30000,
        },
        edge: DockEdge::Bottom,
        scale: 1.0,
        look: Look {
            material: Material::Frosted,
            light: false,
            glass: false,
        },
        language: muelle_config::Language::Es,
        revision: 0,
        hour: 0,
        has_data: false,
    };
    let data = Data::new(None, context.language, 0, Default::default());
    let popup = Popup::new(&app.gfx, context, &data)?;
    let source = popup.hwnd();
    app.weather.popup = Some(popup);
    app.hide_if_unused()?;
    assert_eq!(
        app.slide.target, 0.0,
        "an open forecast must keep the Dock visible"
    );
    app.weather_popup_event(app.input, CLOSE)?;
    assert!(
        app.weather.popup.is_some(),
        "a foreign or obsolete popup event is ignored"
    );
    app.weather_popup_event(source, CLOSE)?;
    assert!(app.weather.popup.is_none());
    app.hide_if_unused()?;
    assert_eq!(
        app.slide.target, 1.0,
        "closing the forecast releases the autohide hold"
    );
    Ok(())
}
