use muelle_core::frame::DockEdge;
use windows::Win32::Foundation::RECT;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Placement { pub bounds: RECT, pub compact: bool }

pub fn place(dock: RECT, screen: RECT, edge: DockEdge, scale: f32) -> Option<Placement> {
    if !scale.is_finite() || scale <= 0.0 { return None; }
    let px = |value: f32| (value * scale).round() as i32;
    let gap = px(8.0).max(1);
    let height = px(58.0).max(1);
    let room = screen.right.checked_sub(screen.left)? - 2*gap;
    if room < px(104.0) || screen.bottom - screen.top < height + 2*gap { return None; }
    let right_room = screen.right - gap - dock.right - gap;
    let (width, compact, x, y) = if edge == DockEdge::Bottom && right_room >= px(240.0) {
        (px(240.0), false, dock.right + gap, dock.bottom-height)
    } else if edge == DockEdge::Bottom && right_room >= px(104.0) {
        (px(104.0), true, dock.right + gap, dock.bottom-height)
    } else if edge == DockEdge::Bottom {
        // Too little room at the side: remain above the last icons, never over
        // their hit boxes. The controller yields during magnification/previews.
        (px(160.0).min(room), true, dock.right-px(160.0).min(room), dock.top-gap-height)
    } else {
        let side_room = if edge == DockEdge::Left { screen.right-gap-dock.right-gap }
            else { dock.left-gap-screen.left-gap };
        let width = px(240.0).min(side_room).min(room);
        if width < px(104.0) { return None; }
        let x = if edge == DockEdge::Left { dock.right+gap } else { dock.left-gap-width };
        (width, width < px(240.0), x, dock.bottom-height)
    };
    let x = x.clamp(screen.left+gap, screen.right-gap-width);
    let y = y.clamp(screen.top+gap, screen.bottom-gap-height);
    Some(Placement { bounds: RECT { left:x, top:y, right:x+width,bottom:y+height }, compact })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cards_stay_in_their_own_monitor_at_every_edge_and_scale() {
        for scale in [1.0,1.25,1.5,2.0,3.0] {
            for (left,top) in [(0,0),(-2560,-1440),(1920,0)] {
                let screen = RECT { left,top,right:left+1920,bottom:top+1080 };
                for edge in [DockEdge::Bottom,DockEdge::Left,DockEdge::Right] {
                    for length in [300,1400,1870] {
                        let dock = match edge {
                            DockEdge::Bottom => RECT { left:left+20,top:top+1000,right:left+length,bottom:top+1060 },
                            DockEdge::Left => RECT { left:left+10,top:top+40,right:left+70,bottom:top+1000 },
                            DockEdge::Right => RECT { left:left+1850,top:top+40,right:left+1910,bottom:top+1000 },
                        };
                        let card = place(dock,screen,edge,scale).unwrap_or_else(|| panic!("{edge:?}, {scale}"));
                        let r=card.bounds;
                        assert!(r.left>=screen.left && r.right<=screen.right && r.top>=screen.top && r.bottom<=screen.bottom);
                        assert!(r.right<=dock.left || r.left>=dock.right || r.bottom<=dock.top || r.top>=dock.bottom);
                    }
                }
            }
        }
    }
}
