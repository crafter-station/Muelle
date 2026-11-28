//! A fixed, non-magnifying item at the end of the Dock, inside its glass.
use muelle_core::{
    frame::{DockEdge, Frame},
    layout::{Layout, Metrics, Rect},
};
use windows::Win32::Foundation::RECT;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Placement {
    pub bounds: RECT,
    pub compact: bool,
}

fn length(icon: f32, edge: DockEdge) -> f32 {
    icon * if edge.is_vertical() { 2.2 } else { 4.6 }
}

/// Reserve space before fitting/centering the Dock. No artificial app slot is
/// inserted, so previews, pinning, keyboard selection and drag IDs stay intact.
pub fn reserve(layout: &mut Layout, metrics: &Metrics, edge: DockEdge) {
    layout.width += length(metrics.icon, edge)
        + if layout.slots.is_empty() {
            0.0
        } else {
            metrics.gap
        };
}

/// Follow the same trailing edge and slide as the retained Dock glass. This
/// leaves the whole card inside its reserved space even when nearby icons grow.
pub fn place(
    frame: Frame,
    layout: &Layout,
    icon: f32,
    right: f32,
    slide: f32,
) -> Option<Placement> {
    if !icon.is_finite() || icon < 1.0 || !right.is_finite() || !slide.is_finite() {
        return None;
    }
    let padding = (layout.height - icon) / 2.0;
    let rect = frame.rect(Rect {
        x: right - padding - length(icon, frame.edge),
        y: padding,
        width: length(icon, frame.edge),
        height: icon,
    });
    let (dx, dy) = frame.outward(slide);
    Some(Placement {
        bounds: RECT {
            left: (rect.x + dx).round() as i32,
            top: (rect.y + dy).round() as i32,
            // Stable size through fractional spring positions: rounding each
            // end separately can cause a one-pixel rerasterization every frame.
            right: (rect.x + dx).round() as i32 + rect.width.round() as i32,
            bottom: (rect.y + dy).round() as i32 + rect.height.round() as i32,
        },
        compact: frame.edge.is_vertical(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use muelle_core::{
        layout::{ItemKind, layout},
        magnify::{Magnifier, magnify},
    };

    #[test]
    fn weather_only_dock_has_no_phantom_magnification() {
        let metrics = Metrics::DEFAULT;
        let mut dock = layout(&[], &metrics);
        reserve(&mut dock, &metrics, DockEdge::Bottom);
        for cursor in [0.0, dock.width / 2.0, dock.width] {
            let state = magnify(
                &dock,
                cursor,
                1.0,
                &Magnifier {
                    range: 210.0,
                    max_scale: 1.6,
                },
            );
            assert_eq!((state.left, state.right), (0.0, dock.width));
        }
    }
    #[test]
    fn weather_stays_inside_glass_without_overlapping_magnified_icons() -> Result<(), &'static str>
    {
        for scale in [0.6, 1.0, 1.25, 1.5, 2.0, 3.0] {
            for edge in [DockEdge::Bottom, DockEdge::Left, DockEdge::Right] {
                let metrics = Metrics::DEFAULT.scaled(scale);
                let mut dock = layout(&[ItemKind::Icon; 12], &metrics);
                let apps = dock.clone();
                reserve(&mut dock, &metrics, edge);
                assert_eq!(dock.elastic_width, apps.width);
                let frame = Frame::new(edge, (-2400.0, -1000.0), dock.height);
                let mag = Magnifier {
                    range: 210.0 * scale,
                    max_scale: 1.6,
                };
                for cursor in (0..dock.width as usize).step_by(11) {
                    let state = magnify(&dock, cursor as f32, 1.0, &mag);
                    let app_state = magnify(&apps, cursor as f32, 1.0, &mag);
                    assert_eq!(
                        state.rects, app_state.rects,
                        "a fixed card cannot stretch empty space or change app geometry"
                    );
                    assert_eq!(state.left, app_state.left);
                    assert!(
                        (state.right - app_state.right - (dock.width - apps.width)).abs() < 0.01
                    );
                    let tile_start = state.right - metrics.padding - length(metrics.icon, edge);
                    for rect in &state.rects {
                        assert!(rect.x + rect.width <= tile_start + 0.1);
                    }
                    let card = place(frame, &dock, metrics.icon, state.right, 0.0)
                        .ok_or("weather placement missing")?;
                    let resting = place(frame, &dock, metrics.icon, dock.width, 0.0)
                        .ok_or("resting weather placement missing")?;
                    assert_eq!(
                        card.bounds.right - card.bounds.left,
                        resting.bounds.right - resting.bounds.left
                    );
                    assert_eq!(
                        card.bounds.bottom - card.bounds.top,
                        resting.bounds.bottom - resting.bounds.top
                    );
                    let glass = frame.rect(Rect {
                        x: state.left,
                        y: 0.0,
                        width: state.right - state.left,
                        height: dock.height,
                    });
                    assert!(card.bounds.left as f32 >= glass.x - 0.5);
                    assert!(card.bounds.top as f32 >= glass.y - 0.5);
                    assert!(card.bounds.right as f32 <= glass.x + glass.width + 1.0);
                    assert!(card.bounds.bottom as f32 <= glass.y + glass.height + 1.0);
                    let moved = place(frame, &dock, metrics.icon, state.right, 40.0 * scale)
                        .ok_or("sliding weather placement missing")?;
                    let (dx, dy) = frame.outward(40.0 * scale);
                    assert!(((moved.bounds.left - card.bounds.left) as f32 - dx).abs() <= 1.0);
                    assert!(((moved.bounds.top - card.bounds.top) as f32 - dy).abs() <= 1.0);
                }
            }
        }
        Ok(())
    }
}
