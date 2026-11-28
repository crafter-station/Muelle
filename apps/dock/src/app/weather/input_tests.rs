//! Replay the pointer path against owned offscreen Dock windows, without
//! moving the user's pointer, querying location or starting forecast work.
use super::super::visibility::tests::{Fixture, queue};
use super::super::{App, Stage, StageKey};
use crate::{
    appearance::{Look, Material},
    scene::{Scene, SceneInput, SlotContent, Theme},
};
use anyhow::Result;
use muelle_core::{
    frame::{DockEdge, Frame},
    layout::{ItemKind, Metrics, layout},
    magnify::{Magnifier, magnify},
    spring::Spring,
};
use windows::Win32::{
    Foundation::{POINT, RECT},
    UI::WindowsAndMessaging::GetForegroundWindow,
};

fn mount(app: &mut App, edge: DockEdge) -> Result<(POINT, POINT)> {
    let metrics = Metrics::DEFAULT;
    let mut layout = layout(&[ItemKind::Icon; 2], &metrics);
    crate::weather::reserve(&mut layout, &metrics, edge);
    let frame = Frame::new(edge, (-32000.0, -32000.0), layout.height);
    let local = Frame::new(edge, (80.0, 80.0), layout.height);
    let look = Look {
        material: Material::Frosted,
        light: false,
        glass: false,
    };
    let scene = Scene::build(
        &app.gfx,
        &SceneInput {
            layout: &layout,
            contents: &[SlotContent::default(); 2],
            theme: Theme::new(look),
            scale: 1.0,
            frame: local,
            remove_label: None,
        },
    )?;
    app.target.SetRoot(&scene.root)?;
    let screen = RECT {
        left: -33000,
        top: -33000,
        right: -30000,
        bottom: -30000,
    };
    let zone = RECT {
        left: -32080,
        top: -32080,
        right: -31480,
        bottom: -31480,
    };
    let magnifier = Magnifier {
        range: 210.0,
        max_scale: 1.6,
    };
    let state = magnify(&layout, 0.0, 0.0, &magnifier);
    let weather = crate::weather::place(frame, &layout, metrics.icon, state.right, 0.0)
        .ok_or_else(|| anyhow::anyhow!("missing weather placement"))?;
    let along = (layout.elastic_width + layout.width) / 2.0;
    let point = |u, v| {
        let (x, y) = frame.point(u, v);
        POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        }
    };
    let weather_point = point(along, layout.height + 19.0);
    let first_icon = layout.slots[0].rect;
    let icon_point = point(first_icon.x + first_icon.width / 2.0, layout.height / 2.0);
    app.stage = Some(Stage {
        key: StageKey {
            screen,
            edge,
            top_inset: 0,
            scale: 1.0,
            look,
            bin_full: false,
            auto_hide: app.config.auto_hide,
            smart_hide: app.config.smart_hide,
            icon_size: 48,
            density: app.config.dock_density,
            max_scale: 1.6,
            indicators: false,
            weather: true,
            entries: Vec::new(),
        },
        window_zone: zone,
        scene,
        reorder: None,
        weather_place: Some(weather),
        magnifier,
        icon: metrics.icon,
        gap: metrics.gap,
        scale: 1.0,
        frame,
        window_frame: local,
        rest_zone: zone,
        hover_zone: zone,
        edge_zone: zone,
        slide_distance: layout.height + 40.0,
        layout,
    });
    // No native network subscription or location: this replay only needs input.
    app.weather.configured = Some((true, app.config.weather_location_mode, None, None));
    Ok((weather_point, icon_point))
}

#[test]
#[ignore = "Offscreen pointer replay only; no live Dock, focus, mouse, location or HTTP changes"]
fn native_weather_tail_reveals_both_hiding_modes_on_every_dock_edge() -> Result<()> {
    let _queue = queue()?;
    let mut fixture = Fixture::new()?;
    let app = &mut fixture.app;
    app.config.weather = true;
    let foreground = unsafe { GetForegroundWindow() };
    for edge in [DockEdge::Bottom, DockEdge::Left, DockEdge::Right] {
        for auto in [false, true] {
            app.config.auto_hide = auto;
            app.config.smart_hide = !auto;
            app.smart_hidden = !auto;
            let (weather_point, icon_point) = mount(app, edge)?;
            for hidden in [1.0, 0.55] {
                app.hovering = false;
                app.weather.hovering = false;
                app.amount = Spring::at(0.0, 24.0);
                app.slide = Spring::at(hidden, 24.0);
                app.slide.target = 1.0;
                app.move_pointer(weather_point)?;
                assert!(
                    app.hovering,
                    "weather strip must reveal: {edge:?}, auto={auto}"
                );
                assert_eq!(app.slide.target, 0.0);
                assert_eq!(app.amount.target, 0.0);
                assert!(app.hovered.is_none());
                app.hide_if_unused()?;
                assert_eq!(app.slide.target, 0.0, "input strip keeps the reveal open");

                // The native card receives input before the reveal has finished.
                app.slide = Spring::at(0.55, 24.0);
                app.render()?;
                let card = app
                    .weather
                    .card
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing offscreen card"))?
                    .hwnd();
                app.slide.target = 1.0;
                app.weather_event(card, 1)?;
                assert!(app.weather_holds_open());
                assert!(!app.hovering);
                assert_eq!(app.slide.target, 0.0, "card entry reverses a pending hide");
                app.hide_if_unused()?;
                assert_eq!(app.slide.target, 0.0);

                app.weather_event(card, 0)?;
                app.move_pointer(icon_point)?;
                assert_eq!(
                    app.amount.target, 1.0,
                    "apps still magnify after weather entry"
                );
                assert_eq!(app.slide.target, 0.0);
                app.on_mouse_leave()?;
                app.hide_if_unused()?;
                assert_eq!(app.slide.target, 1.0, "leaving releases both hover holds");
            }
        }
    }
    assert!(app.weather.pending.is_none() && app.weather.network.is_none());
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
    Ok(())
}
