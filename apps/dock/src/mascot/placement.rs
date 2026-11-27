//! Shared placement for event acts and the quiet companion.
use super::{Character, Place, Turn, cells};
use muelle_core::frame::DockEdge;
use windows::Win32::Foundation::RECT;

pub(super) struct Geometry {
    pub rect: RECT,
    pub turn: Turn,
    pub crab: (f32, f32),
    pub crab_top: f32,
    pub cell: f32,
}

impl Geometry {
    pub fn new(character: Character, place: Place, scale: f32) -> Self {
        let cell_px = (scale * 1.6).round().max(2.0) as u32;
        let cell = cell_px as f32;
        let (columns, rows) = cells(character);
        let crab = (
            (columns as u32 * cell_px) as f32,
            (rows as u32 * cell_px) as f32,
        );
        // Room above the crab for hops and the emote.
        let headroom = 14.0 * cell;
        let height = crab.1 + headroom;
        let (rect, crab_top) = match place {
            Place::Under { x, top } => {
                let width = crab.0 + 8.0 * cell;
                let left = x - (width / 2.0) as i32;
                // Hangs a little below the bar.
                let top = top + cell as i32;
                (
                    RECT {
                        left,
                        top,
                        right: left + width as i32,
                        bottom: top + height as i32,
                    },
                    headroom,
                )
            }
            // Feet one cell into the rim, as if standing on it.
            Place::Along {
                left,
                right,
                top,
                edge: DockEdge::Bottom,
            } => {
                let bottom = top + cell as i32;
                (
                    RECT {
                        left,
                        top: bottom - height as i32,
                        right,
                        bottom,
                    },
                    headroom,
                )
            }
            Place::Along {
                left: start,
                right: end,
                top: rim,
                edge,
            } => {
                let feet = if edge == DockEdge::Left {
                    rim - cell as i32
                } else {
                    rim + cell as i32 - height as i32
                };
                (
                    RECT {
                        left: feet,
                        top: start,
                        right: feet + height as i32,
                        bottom: end,
                    },
                    headroom,
                )
            }
        };
        let turn = match place {
            Place::Along { edge, .. } if edge.is_vertical() => Turn {
                edge,
                width: (rect.bottom - rect.top) as f32,
                height,
            },
            _ => Turn {
                edge: DockEdge::Bottom,
                width: (rect.right - rect.left) as f32,
                height: (rect.bottom - rect.top) as f32,
            },
        };
        Self {
            rect,
            turn,
            crab,
            crab_top,
            cell,
        }
    }
}
