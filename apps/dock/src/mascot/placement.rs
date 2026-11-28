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
    /// In the menu bar, the exact size its poses are drawn at (see
    /// `super::shrink`): every character as tall, whatever its grid.
    pub sprite: Option<(u32, u32)>,
}

impl Geometry {
    pub fn new(character: Character, place: Place, scale: f32) -> Self {
        let (columns, rows) = cells(character);
        let cell_px = (scale * 1.6).round().max(2.0) as u32;
        // In the menu bar every character stands as tall, about three
        // quarters of the bar, whatever its grid's rows (the crab and the
        // mochi have 15, the girl 26): a fraction of a cell each.
        let sprite = match place {
            Place::Bar { top, bottom, .. } => {
                let tall = ((bottom - top) as f32 * 0.76).round().max(8.0);
                let wide = (tall * columns as f32 / rows as f32).round();
                Some((wide as u32, tall as u32))
            }
            _ => None,
        };
        let cell = sprite.map_or(cell_px as f32, |(_, tall)| tall as f32 / rows as f32);
        let crab = sprite.map_or(
            (
                (columns as u32 * cell_px) as f32,
                (rows as u32 * cell_px) as f32,
            ),
            |(wide, tall)| (wide as f32, tall as f32),
        );
        // Room above the crab for hops and the emote.
        let headroom = 14.0 * cell;
        let height = crab.1 + headroom;
        let (rect, crab_top) = match place {
            // Feet two pixels above the bar's bottom edge.
            Place::Bar {
                left,
                right,
                top,
                bottom,
            } => (
                RECT {
                    left,
                    top,
                    right,
                    bottom,
                },
                ((bottom - top) as f32 - crab.1 - 2.0).max(0.0),
            ),
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
            sprite,
        }
    }
}
