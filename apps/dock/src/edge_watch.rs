//! Observes screen edges on a dedicated timer thread while auto-hide is active.

use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::Result;
use muelle_core::frame::DockEdge;
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::{
        GetCursorPos, GetMessageW, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW,
        PostThreadMessageW, SetTimer, WM_APP, WM_QUIT, WM_TIMER,
    },
};

pub const CHANGED: u32 = WM_APP + 26;
/// How long the pointer rests on another display's edge before the dock
/// moves there: long enough that passing by does not move it.
const DWELL: Duration = Duration::from_millis(180);
const MENU_DWELL: Duration = Duration::from_millis(150);
const EDGE_DEPTH: i32 = 2;
/// Sampling away from the edges, where nothing can be revealed yet.
const SAMPLE_MS: u32 = 80;
/// Sampling once the pointer is this close to a watched edge: every display
/// frame, so arriving there is seen at once instead of up to 80 ms late.
const NEAR_SAMPLE_MS: u32 = 16;
const NEAR: i32 = 96;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// A menu bar's top strip, by its window (one bar per display).
    MenuBar(isize),
    Dock(RECT),
}

impl Target {
    fn delay(self) -> Duration {
        match self {
            Self::MenuBar(_) => MENU_DWELL,
            Self::Dock(_) => DWELL,
        }
    }
}

pub enum Reveal {
    Idle,
    Wait(Duration),
    Ready(Target),
}

#[derive(Default)]
struct Edges {
    window: isize,
    /// The top strip of each hidden menu bar, by its window.
    tops: Vec<(isize, RECT)>,
    /// The displays a hidden dock can be revealed on, and the edge it sits on.
    screens: Vec<RECT>,
    dock_edge: DockEdge,
    pointer_screen: Option<RECT>,
    target: Option<Target>,
    entered: Option<Instant>,
}

static EDGES: Mutex<Edges> = Mutex::new(Edges {
    window: 0,
    tops: Vec::new(),
    screens: Vec::new(),
    dock_edge: DockEdge::Bottom,
    pointer_screen: None,
    target: None,
    entered: None,
});

impl Edges {
    fn at(&self, point: POINT, screen: Option<RECT>) -> Option<Target> {
        if let Some(&(bar, _)) = self.tops.iter().find(|(_, top)| {
            (top.left..top.right).contains(&point.x)
                && (top.top..top.top + EDGE_DEPTH).contains(&point.y)
        }) {
            return Some(Target::MenuBar(bar));
        }
        // The menu bar, checked first, keeps the top corners.
        screen
            .filter(|screen| match self.dock_edge {
                DockEdge::Bottom => point.y >= screen.bottom - EDGE_DEPTH,
                DockEdge::Left => point.x < screen.left + EDGE_DEPTH,
                DockEdge::Right => point.x >= screen.right - EDGE_DEPTH,
            })
            .map(Target::Dock)
    }

    /// Whether `point` is close to an edge that reveals something.
    fn near(&self, point: POINT) -> bool {
        let close = |from: i32, to: i32| (from - to).abs() < NEAR;
        self.tops
            .iter()
            .any(|(_, top)| (top.left..top.right).contains(&point.x) && close(point.y, top.top))
            || self.screens.iter().any(|screen| {
                (screen.left..screen.right).contains(&point.x)
                    && (screen.top..screen.bottom).contains(&point.y)
                    && match self.dock_edge {
                        DockEdge::Bottom => close(point.y, screen.bottom - 1),
                        DockEdge::Left => close(point.x, screen.left),
                        DockEdge::Right => close(point.x, screen.right - 1),
                    }
            })
    }

    fn observe(&mut self, point: POINT, now: Instant) -> bool {
        let screen = self.screens.iter().copied().find(|screen| {
            (screen.left..screen.right).contains(&point.x)
                && (screen.top..screen.bottom).contains(&point.y)
        });
        let changed_screen = self.pointer_screen != screen;
        self.pointer_screen = screen;
        let target = self.at(point, screen);
        if target == self.target {
            return changed_screen;
        }
        self.target = target;
        self.entered = target.map(|_| now);
        true
    }

    fn set_top(&mut self, bar: isize, top: Option<RECT>) {
        let old = self
            .tops
            .iter()
            .position(|(id, _)| *id == bar)
            .map(|index| self.tops.remove(index).1);
        if let Some(top) = top {
            self.tops.push((bar, top));
        }
        if old != top && self.target == Some(Target::MenuBar(bar)) {
            self.target = None;
            self.entered = None;
        }
    }

    fn set_screens(&mut self, screens: Vec<RECT>, edge: DockEdge) {
        if let Some(Target::Dock(screen)) = self.target
            && (!screens.contains(&screen) || edge != self.dock_edge)
        {
            self.target = None;
            self.entered = None;
        }
        self.screens = screens;
        self.dock_edge = edge;
    }

    fn dwell(&mut self, point: POINT, now: Instant) -> Reveal {
        self.observe(point, now);
        let (Some(entered), Some(target)) = (self.entered, self.target) else {
            return Reveal::Idle;
        };
        let elapsed = now.saturating_duration_since(entered);
        let delay = target.delay();
        if elapsed < delay {
            return Reveal::Wait(delay - elapsed);
        }
        self.entered = None;
        Reveal::Ready(target)
    }
}

fn lock() -> MutexGuard<'static, Edges> {
    EDGES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The top strip that reveals the hidden menu bar `bar`, or none while it
/// shows or is gone.
pub fn set_menu_bar(bar: HWND, top: Option<RECT>) {
    lock().set_top(bar.0 as isize, top);
}

/// The displays a hidden dock can come out on, from `edge` of each.
pub fn set_dock_monitors(screens: Vec<RECT>, edge: DockEdge) {
    lock().set_screens(screens, edge);
}

pub fn poll(point: POINT) -> Reveal {
    lock().dwell(point, Instant::now())
}

/// Dwell is owned by the UI thread while observations arrive from the watcher.
pub fn pending() -> (Option<Target>, Option<Duration>) {
    let edges = lock();
    (
        edges.target,
        edges
            .entered
            .zip(edges.target)
            .map(|(entered, target)| target.delay().saturating_sub(entered.elapsed())),
    )
}

pub fn retry_on_move(target: Target) {
    let mut edges = lock();
    if edges.target == Some(target) {
        edges.target = None;
        edges.entered = None;
    }
}

pub struct EdgeWatch {
    thread: u32,
    join: Option<JoinHandle<()>>,
}

impl EdgeWatch {
    pub fn start(window: HWND) -> Result<Self> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let join = thread::Builder::new()
            .name("muelle-edge".into())
            .spawn(move || {
                // SAFETY: This thread owns the message queue and timer.
                unsafe {
                    let mut msg = MSG::default();
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    let timer = SetTimer(None, 0, SAMPLE_MS, None);
                    if timer == 0 {
                        let _ = sender.send(Err(windows::core::Error::from_thread()));
                        return;
                    }
                    let _ = sender.send(Ok(GetCurrentThreadId()));
                    let mut every = SAMPLE_MS;
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                        if msg.message == WM_TIMER {
                            let wanted = if sample_cursor() {
                                NEAR_SAMPLE_MS
                            } else {
                                SAMPLE_MS
                            };
                            if wanted != every {
                                every = wanted;
                                // The same id replaces this thread's timer.
                                SetTimer(None, timer, every, None);
                            }
                        }
                    }
                    let _ = KillTimer(None, timer);
                }
            })?;
        match receiver.recv() {
            Ok(Ok(thread)) => {
                lock().window = window.0 as isize;
                Ok(Self {
                    thread,
                    join: Some(join),
                })
            }
            result => {
                let _ = join.join();
                Err(anyhow::anyhow!("no se pudo observar el borde: {result:?}"))
            }
        }
    }
}

impl Drop for EdgeWatch {
    fn drop(&mut self) {
        {
            let mut edges = lock();
            edges.window = 0;
            edges.pointer_screen = None;
            edges.target = None;
            edges.entered = None;
        }
        // SAFETY: Posting to the queue created before the thread reported ready.
        unsafe {
            let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Reads the pointer; returns whether it is near a watched edge.
fn sample_cursor() -> bool {
    let mut point = POINT::default();
    // SAFETY: The output is live and this thread only reads cursor position.
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return false;
    }
    let mut near = false;
    let window = EDGES.try_lock().ok().and_then(|mut edges| {
        near = edges.near(point);
        (edges.window != 0 && edges.observe(point, Instant::now())).then_some(edges.window)
    });
    if let Some(window) = window {
        // SAFETY: Post a scalar notification without waiting for the UI.
        unsafe {
            let _ = PostMessageW(Some(HWND(window as *mut _)), CHANGED, WPARAM(0), LPARAM(0));
        }
    }
    near
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires a Windows desktop and a stationary pointer"]
    fn watcher_detects_a_pointer_already_at_the_edge() -> Result<()> {
        use windows::{
            Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, PM_REMOVE, WS_POPUP},
            core::w,
        };
        // SAFETY: A test-owned hidden receiving window. The test reads the
        // cursor and pumps only its own messages without injecting input.
        unsafe {
            let window = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("Muelle edge test"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )?;
            let result = (|| -> Result<()> {
                let mut point = POINT::default();
                GetCursorPos(&mut point)?;
                set_menu_bar(
                    window,
                    Some(RECT {
                        left: point.x - 200,
                        right: point.x + 200,
                        top: point.y,
                        bottom: point.y + 30,
                    }),
                );
                let watch = EdgeWatch::start(window)?;
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut msg = MSG::default();
                let mut received = false;
                while Instant::now() < deadline {
                    if PeekMessageW(&mut msg, Some(window), CHANGED, CHANGED, PM_REMOVE).as_bool() {
                        received = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(
                    received,
                    "edge notification must arrive without a new mouse event"
                );
                drop(watch);
                set_menu_bar(window, None);
                Ok(())
            })();
            let _ = DestroyWindow(window);
            result
        }
    }

    #[test]
    fn a_side_dock_comes_out_of_its_side_and_the_menu_bar_keeps_the_corner() {
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let mut edges = Edges::default();
        edges.set_top(
            1,
            Some(RECT {
                bottom: 35,
                ..screen
            }),
        );
        edges.set_screens(vec![screen], DockEdge::Left);
        assert_eq!(
            edges.at(POINT { x: 0, y: 500 }, Some(screen)),
            Some(Target::Dock(screen))
        );
        assert_eq!(edges.at(POINT { x: 900, y: 1079 }, Some(screen)), None);
        assert_eq!(
            edges.at(POINT { x: 0, y: 0 }, Some(screen)),
            Some(Target::MenuBar(1))
        );
        edges.set_screens(vec![screen], DockEdge::Right);
        assert_eq!(
            edges.at(POINT { x: 1919, y: 500 }, Some(screen)),
            Some(Target::Dock(screen))
        );
        assert_eq!(edges.at(POINT { x: 0, y: 500 }, Some(screen)), None);
    }

    #[test]
    fn sampling_quickens_only_near_a_watched_edge() {
        let laptop = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let monitor = RECT {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1080,
        };
        let mut edges = Edges::default();
        edges.set_screens(vec![laptop, monitor], DockEdge::Bottom);
        edges.set_top(
            1,
            Some(RECT {
                bottom: 35,
                ..laptop
            }),
        );
        let at = |x, y| POINT { x, y };
        assert!(!edges.near(at(960, 540)));
        assert!(edges.near(at(2880, 1079)));
        assert!(edges.near(at(2880, 1000)));
        assert!(edges.near(at(960, 10)));
        // No bar on the monitor: its top edge reveals nothing.
        assert!(!edges.near(at(2880, 10)));
        edges.set_screens(vec![laptop, monitor], DockEdge::Left);
        assert!(!edges.near(at(2880, 1079)));
        assert!(edges.near(at(1930, 540)));
    }

    #[test]
    fn each_display_reveals_its_own_menu_bar() {
        let strip = |left, right| RECT {
            left,
            top: 0,
            right,
            bottom: 30,
        };
        let mut edges = Edges::default();
        edges.set_top(1, Some(strip(0, 1920)));
        edges.set_top(2, Some(strip(1920, 3840)));
        let top = |x| POINT { x, y: 0 };
        assert_eq!(edges.at(top(500), None), Some(Target::MenuBar(1)));
        assert_eq!(edges.at(top(2500), None), Some(Target::MenuBar(2)));
        // One bar showing again stops watching its strip, not the other's.
        edges.set_top(1, None);
        assert_eq!(edges.at(top(500), None), None);
        assert_eq!(edges.at(top(2500), None), Some(Target::MenuBar(2)));
    }

    #[test]
    fn menu_bar_reveals_sooner_without_changing_dock_switching_delay() {
        let start = Instant::now();
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let top = POINT { x: 700, y: 0 };
        let bottom = POINT { x: 700, y: 1079 };
        let mut edges = Edges::default();
        edges.set_top(1, Some(screen));
        edges.set_screens(vec![screen], DockEdge::Bottom);
        edges.observe(top, start);
        assert!(matches!(
            edges.dwell(top, start + MENU_DWELL),
            Reveal::Ready(Target::MenuBar(1))
        ));
        edges.observe(bottom, start);
        assert!(matches!(
            edges.dwell(bottom, start + MENU_DWELL),
            Reveal::Wait(_)
        ));
        assert!(matches!(
            edges.dwell(bottom, start + DWELL),
            Reveal::Ready(Target::Dock(_))
        ));
    }

    #[test]
    fn an_unchanged_bar_layout_does_not_cancel_hover_dwell() {
        let mut edges = Edges::default();
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let point = POINT { x: 700, y: 1 };
        let start = Instant::now();
        edges.set_top(1, Some(screen));
        assert!(edges.observe(point, start));
        edges.set_top(1, Some(screen));
        assert!(matches!(
            edges.dwell(point, start + DWELL),
            Reveal::Ready(Target::MenuBar(1))
        ));
        assert!(matches!(edges.dwell(point, start + DWELL), Reveal::Idle));
    }

    #[test]
    fn dwell_follows_the_bottom_of_the_display_with_the_pointer() {
        let primary = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let secondary = RECT {
            left: -1280,
            top: -200,
            right: 0,
            bottom: 824,
        };
        let mut edges = Edges::default();
        edges.set_screens(vec![primary, secondary], DockEdge::Bottom);
        let start = Instant::now();
        let left = POINT { x: -700, y: 823 };
        let right = POINT { x: 700, y: 1079 };
        assert!(edges.observe(right, start));
        assert!(edges.observe(left, start + Duration::from_millis(100)));
        assert!(matches!(edges.dwell(left, start + DWELL), Reveal::Wait(_)));
        assert!(
            matches!(edges.dwell(left, start + DWELL * 2), Reveal::Ready(Target::Dock(screen)) if screen == secondary)
        );
        edges.set_screens(vec![primary], DockEdge::Bottom);
        assert!(matches!(edges.dwell(left, start + DWELL * 3), Reveal::Idle));
    }

    #[test]
    fn monitor_entry_notifies_before_the_pointer_reaches_the_edge() {
        let primary = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let secondary = RECT {
            left: 1920,
            right: 3840,
            ..primary
        };
        let mut edges = Edges::default();
        edges.set_screens(vec![primary, secondary], DockEdge::Bottom);
        let now = Instant::now();
        assert!(edges.observe(POINT { x: 700, y: 500 }, now));
        assert!(edges.observe(POINT { x: 2500, y: 500 }, now));
        assert_eq!(edges.target, None);
        assert!(!edges.observe(POINT { x: 2600, y: 500 }, now));
    }

    #[test]
    fn pending_reveal_cancels_when_the_pointer_crosses_the_edge() {
        let mut edges = Edges::default();
        edges.set_screens(
            vec![RECT {
                left: 0,
                top: -900,
                right: 1600,
                bottom: 0,
            }],
            DockEdge::Bottom,
        );
        let start = Instant::now();
        assert!(edges.observe(POINT { x: 800, y: -1 }, start));
        assert!(matches!(
            edges.dwell(POINT { x: 800, y: 0 }, start + DWELL),
            Reveal::Idle
        ));
    }
}
