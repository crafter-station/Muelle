use muelle_core::frame::DockEdge;
use windows::Win32::Foundation::{POINT, RECT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Day(usize),
    Hour(usize),
    PreviousHours,
    NextHours,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub bounds: RECT,
    pub scale: f32,
    pub width: f32,
    pub height: f32,
}
impl Layout {
    pub fn hit(self, point: POINT) -> Option<Hit> {
        let (x, y) = (point.x as f32 / self.scale, point.y as f32 / self.scale);
        if (20.0..380.0).contains(&x) && (338.0..488.0).contains(&y) {
            Some(Hit::Day(((y - 338.0) / 30.0) as usize))
        } else if (20.0..380.0).contains(&x) && (194.0..290.0).contains(&y) {
            let offset = x - 20.0;
            ((offset % 60.0) < 56.0).then_some(Hit::Hour((offset / 60.0) as usize))
        } else if (165.0..190.0).contains(&y) {
            if (221.0..245.0).contains(&x) {
                Some(Hit::PreviousHours)
            } else if (249.0..273.0).contains(&x) {
                Some(Hit::NextHours)
            } else {
                None
            }
        } else {
            None
        }
    }
    pub fn new(
        anchor: RECT,
        screen: RECT,
        edge: DockEdge,
        requested: f32,
        has_data: bool,
    ) -> Option<Self> {
        if !requested.is_finite() || requested <= 0.0 {
            return None;
        }
        let (width, height) = (400.0_f32, if has_data { 536.0_f32 } else { 188.0 });
        // Fit into the space away from the Dock, not the whole screen.
        // Otherwise a short/high-DPI display clamps the panel over its card.
        let (room_width, room_height) = match edge {
            DockEdge::Bottom => (screen.right - screen.left, anchor.top - screen.top),
            DockEdge::Left => (screen.right - anchor.right, screen.bottom - screen.top),
            DockEdge::Right => (anchor.left - screen.left, screen.bottom - screen.top),
        };
        let scale = requested
            .min(room_width as f32 / (width + 24.0))
            .min(room_height as f32 / (height + 24.0));
        if scale < 0.5 {
            return None;
        }
        let (w, h, gap) = (
            (width * scale).round() as i32,
            (height * scale).round() as i32,
            (10.0 * scale).round() as i32,
        );
        let (x, y) = match edge {
            DockEdge::Bottom => ((anchor.left + anchor.right - w) / 2, anchor.top - gap - h),
            DockEdge::Left => (anchor.right + gap, (anchor.top + anchor.bottom - h) / 2),
            DockEdge::Right => (anchor.left - gap - w, (anchor.top + anchor.bottom - h) / 2),
        };
        let x = x.clamp(screen.left + gap, screen.right - gap - w);
        let y = y.clamp(screen.top + gap, screen.bottom - gap - h);
        Some(Self {
            bounds: RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
            scale,
            width,
            height,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forecast_panel_fits_negative_monitors_side_docks_and_scaling() -> Result<(), &'static str> {
        for (w, h) in [(800, 600), (1920, 1080), (3840, 2160)] {
            for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
                for edge in [DockEdge::Bottom, DockEdge::Left, DockEdge::Right] {
                    let screen = RECT {
                        left: -w,
                        top: -h,
                        right: 0,
                        bottom: 0,
                    };
                    let anchor = match edge {
                        DockEdge::Bottom => RECT {
                            left: -250,
                            top: -80,
                            right: -20,
                            bottom: -20,
                        },
                        DockEdge::Left => RECT {
                            left: -w + 10,
                            top: -200,
                            right: -w + 70,
                            bottom: -70,
                        },
                        DockEdge::Right => RECT {
                            left: -70,
                            top: -200,
                            right: -10,
                            bottom: -70,
                        },
                    };
                    for has_data in [false, true] {
                        let l = Layout::new(anchor, screen, edge, scale, has_data)
                            .ok_or("missing popup layout")?;
                        assert!(l.bounds.left >= screen.left && l.bounds.top >= screen.top);
                        assert!(l.bounds.right <= screen.right && l.bounds.bottom <= screen.bottom);
                        match edge {
                            DockEdge::Bottom => assert!(l.bounds.bottom < anchor.top),
                            DockEdge::Left => assert!(l.bounds.left > anchor.right),
                            DockEdge::Right => assert!(l.bounds.right < anchor.left),
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
