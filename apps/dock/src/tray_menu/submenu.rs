//! A program's own menu, opened by a click on its row. It is the program's
//! window, drawn and placed by it, often far from the row: the tray menu moves
//! it beside the row, as macOS opens a submenu, and stays open behind it until
//! it is gone.

use std::time::{Duration, Instant};

use muelle_shell::tray_host::Shown;
use windows::Win32::Foundation::{POINT, RECT};

/// Escape this shortly before the menu went means it was what closed it.
const ESCAPED: Duration = Duration::from_millis(500);
/// A program with its menu already up may close it on the click and open it
/// again, leaving the foreground to another program meanwhile.
const SETTLING: Duration = Duration::from_millis(300);

/// What one look at the screen finds.
pub struct Seen<'a> {
    /// What the program has on screen.
    pub windows: &'a [Shown],
    pub foreground: isize,
    pub foreground_process: u32,
    /// The tray menu's glass, and the pointer, on screen.
    pub panel: RECT,
    pub cursor: POINT,
    /// A button went down away from the tray menu and its bar button.
    pub pressed_outside: bool,
    pub escape: bool,
    pub now: Instant,
}

/// What the tray menu does next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Nothing has shown yet.
    Wait,
    /// The program's menu is open: it belongs beside the row.
    Hold(Shown),
    /// Over, and the tray menu stays.
    Stay,
    /// Over, and the tray menu goes too.
    Close,
}

pub struct Submenu {
    pub process: u32,
    /// What the program already had on screen when clicked.
    before: Vec<isize>,
    /// The foreground window when clicked.
    foreground: isize,
    /// Its menu, once it shows.
    menu: Option<isize>,
    clicked: Instant,
    /// When to give up waiting for one.
    deadline: Instant,
    pointer_on_it: bool,
    escaped: Option<Instant>,
}

pub fn contains(area: RECT, point: POINT) -> bool {
    point.x >= area.left && point.x < area.right && point.y >= area.top && point.y < area.bottom
}

impl Submenu {
    pub fn new(
        process: u32,
        before: &[Shown],
        foreground: isize,
        clicked: Instant,
        wait: Duration,
    ) -> Self {
        Self {
            process,
            before: before.iter().map(|shown| shown.window).collect(),
            foreground,
            menu: None,
            clicked,
            deadline: clicked + wait,
            pointer_on_it: false,
            escaped: None,
        }
    }

    /// The program's menu, once it has shown.
    pub fn menu(&self) -> Option<isize> {
        self.menu
    }

    pub fn look(&mut self, seen: &Seen<'_>) -> Step {
        let before = &self.before;
        let fresh = || {
            seen.windows
                .iter()
                .filter(|shown| !before.contains(&shown.window))
        };
        if let Some(menu) = self.menu {
            if seen.escape {
                self.escaped = Some(seen.now);
            }
            if let Some(&shown) = seen.windows.iter().find(|shown| shown.window == menu) {
                // The menu and the menus opened from it.
                self.pointer_on_it = fresh().any(|shown| contains(shown.bounds, seen.cursor));
                // A menu the program draws itself may not close on Escape:
                // closing ours takes the focus from it, which does.
                return if seen.escape && !shown.standard {
                    Step::Close
                } else {
                    Step::Hold(shown)
                };
            }
            // Gone. Dismissed from here, the tray menu remains; after a choice,
            // or a click elsewhere, both are done.
            let escaped = self
                .escaped
                .is_some_and(|at| seen.now.duration_since(at) < ESCAPED);
            let back = contains(seen.panel, seen.cursor) && !self.pointer_on_it;
            return if escaped || back {
                Step::Stay
            } else {
                Step::Close
            };
        }
        // A new menu, or one that was already up and has taken the foreground.
        let menu = fresh().find(|shown| shown.menu).or_else(|| {
            seen.windows
                .iter()
                .find(|shown| shown.menu && shown.window == seen.foreground)
        });
        if let Some(&shown) = menu {
            self.menu = Some(shown.window);
            return Step::Hold(shown);
        }
        // A window instead of a menu, either new or brought to the front.
        let moved = seen.foreground != self.foreground;
        let opened = fresh().next().is_some()
            || (moved
                && seen
                    .windows
                    .iter()
                    .any(|shown| shown.window == seen.foreground && !shown.menu));
        let settled = seen.now.duration_since(self.clicked) >= SETTLING;
        let left = settled && moved && seen.foreground_process != self.process;
        if opened || left || seen.pressed_outside || seen.escape || seen.now >= self.deadline {
            Step::Close
        } else {
            Step::Wait
        }
    }
}

/// Where a menu of `size` goes: left of the glass `panel`, or right of it if
/// there is no room, level with the row at `row`, and below the menu bar.
pub fn beside(
    screen: RECT,
    panel: RECT,
    row: i32,
    bar_bottom: i32,
    (width, height): (i32, i32),
    gap: i32,
) -> (i32, i32) {
    let left = panel.left - gap - width;
    let x = if left >= screen.left {
        left
    } else {
        (panel.right + gap).min(screen.right - width)
    };
    let y = row.min(screen.bottom - height).max(bar_bottom);
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    const PANEL: RECT = RECT {
        left: 1600,
        top: 40,
        right: 1892,
        bottom: 280,
    };
    const ON_PANEL: POINT = POINT { x: 1700, y: 100 };
    const ELSEWHERE: POINT = POINT { x: 400, y: 600 };
    const OURS: u32 = 7;

    fn shown(window: isize, left: i32, menu: bool) -> Shown {
        Shown {
            window,
            bounds: RECT {
                left,
                top: 100,
                right: left + 200,
                bottom: 300,
            },
            menu,
            standard: menu,
        }
    }

    fn seen<'a>(windows: &'a [Shown], cursor: POINT, now: Instant) -> Seen<'a> {
        Seen {
            windows,
            foreground: 1,
            foreground_process: 99,
            panel: PANEL,
            cursor,
            pressed_outside: false,
            escape: false,
            now,
        }
    }

    /// Clicked with the foreground on window 1, the program having window 2.
    fn clicked(now: Instant) -> Submenu {
        let wait = Duration::from_secs(1);
        Submenu::new(OURS, &[shown(2, 0, false)], 1, now, wait)
    }

    #[test]
    fn the_programs_menu_is_held_and_dismissing_it_from_here_keeps_ours() {
        let now = Instant::now();
        let mut menu = clicked(now);
        // The program takes the foreground before its menu shows.
        let hidden = [shown(2, 0, false)];
        let waiting = Seen {
            foreground: 3,
            foreground_process: OURS,
            ..seen(&hidden, ON_PANEL, now)
        };
        assert_eq!(menu.look(&waiting), Step::Wait);
        let open = [shown(2, 0, false), shown(4, 1390, true)];
        assert_eq!(menu.look(&seen(&open, ON_PANEL, now)), Step::Hold(open[1]));
        // It stays held past the deadline, wherever the pointer goes.
        let later = now + Duration::from_secs(60);
        assert_eq!(
            menu.look(&seen(&open, ELSEWHERE, later)),
            Step::Hold(open[1])
        );
        assert_eq!(
            menu.look(&seen(&open, ON_PANEL, later)),
            Step::Hold(open[1])
        );
        assert_eq!(menu.look(&seen(&open[..1], ON_PANEL, later)), Step::Stay);
    }

    #[test]
    fn a_choice_or_a_click_elsewhere_closes_both() {
        let now = Instant::now();
        let open = [shown(4, 1390, true)];
        for pointer in [POINT { x: 1400, y: 150 }, ELSEWHERE] {
            let mut menu = clicked(now);
            menu.look(&seen(&open, pointer, now));
            menu.look(&seen(&open, pointer, now));
            assert_eq!(menu.look(&seen(&[], pointer, now)), Step::Close);
        }
        // A menu opened from it may overlap the tray menu.
        let mut menu = clicked(now);
        let nested = [shown(4, 1390, true), shown(5, 1580, true)];
        let over = POINT { x: 1700, y: 150 };
        menu.look(&seen(&nested[..1], over, now));
        menu.look(&seen(&nested, over, now));
        assert_eq!(menu.look(&seen(&[], over, now)), Step::Close);
    }

    #[test]
    fn escape_keeps_ours_wherever_the_pointer_is() {
        let now = Instant::now();
        let open = [shown(4, 1390, true)];
        let mut menu = clicked(now);
        menu.look(&seen(&open, ELSEWHERE, now));
        let escape = Seen {
            escape: true,
            ..seen(&open, ELSEWHERE, now)
        };
        menu.look(&escape);
        assert_eq!(menu.look(&seen(&[], ELSEWHERE, now)), Step::Stay);
    }

    #[test]
    fn escape_closes_both_on_a_menu_the_program_draws() {
        let now = Instant::now();
        let own = [Shown {
            standard: false,
            ..shown(4, 1390, true)
        }];
        let mut menu = clicked(now);
        menu.look(&seen(&own, ELSEWHERE, now));
        let escape = Seen {
            escape: true,
            ..seen(&own, ELSEWHERE, now)
        };
        assert_eq!(menu.look(&escape), Step::Close);
    }

    #[test]
    fn without_a_menu_ours_closes() {
        let now = Instant::now();
        let old = [shown(2, 0, false)];
        // A window opens, or one already open comes to the front.
        let new = [shown(6, 0, false)];
        assert_eq!(clicked(now).look(&seen(&new, ON_PANEL, now)), Step::Close);
        let raised = Seen {
            foreground: 2,
            foreground_process: OURS,
            ..seen(&old, ON_PANEL, now)
        };
        assert_eq!(clicked(now).look(&raised), Step::Close);
        // Another program comes to the front, or nothing happens in time.
        let other = Seen {
            foreground: 8,
            ..seen(&old, ON_PANEL, now + SETTLING)
        };
        assert_eq!(clicked(now).look(&other), Step::Close);
        // Not while the program may still be putting its menu back.
        let passing = Seen {
            foreground: 8,
            ..seen(&old, ON_PANEL, now)
        };
        assert_eq!(clicked(now).look(&passing), Step::Wait);
        assert_eq!(clicked(now).look(&seen(&old, ON_PANEL, now)), Step::Wait);
        let late = now + Duration::from_secs(2);
        assert_eq!(clicked(now).look(&seen(&old, ON_PANEL, late)), Step::Close);
    }

    #[test]
    fn a_menu_already_up_is_held_once_it_takes_the_foreground() {
        let now = Instant::now();
        let up = [shown(4, 1390, true)];
        let mut menu = Submenu::new(OURS, &up, 4, now, Duration::from_secs(1));
        // It gives way to the click, then comes back.
        let away = Seen {
            foreground: 8,
            ..seen(&[], ON_PANEL, now)
        };
        assert_eq!(menu.look(&away), Step::Wait);
        let back = Seen {
            foreground: 4,
            foreground_process: OURS,
            ..seen(&up, ON_PANEL, now)
        };
        assert_eq!(menu.look(&back), Step::Hold(up[0]));
    }

    #[test]
    fn a_menu_goes_beside_the_row_and_stays_on_screen() {
        assert_eq!(beside(SCREEN, PANEL, 120, 28, (200, 300), 4), (1396, 120));
        // Too low: raised to fit. Too tall: from under the bar.
        assert_eq!(beside(SCREEN, PANEL, 900, 28, (200, 300), 4), (1396, 780));
        assert_eq!(beside(SCREEN, PANEL, 120, 28, (200, 2000), 4), (1396, 28));
        // No room to the left.
        let near = RECT {
            left: 10,
            right: 302,
            ..PANEL
        };
        assert_eq!(beside(SCREEN, near, 120, 28, (200, 300), 4), (306, 120));
    }
}
