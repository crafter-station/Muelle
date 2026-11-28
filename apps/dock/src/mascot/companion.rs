//! A retained, click-through companion. No frame or pointer polling: each act
//! is one compositor animation, scheduled by a one-shot timer between acts.
//!
//! On the dock it rests, blinks, smiles, hops and walks along the rim. Now
//! and then it hops away and walks inside the menu bar for a while, smaller,
//! then comes back (see `App::travel_companion`). Turned on, it says hello
//! with a double hop.

use super::{
    Character, Face, Gfx, Place, create_window, placement::Geometry, pose_image, pose_image_sized,
    sprite,
};
use anyhow::Result;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicIsize, Ordering},
    time::{Duration, Instant},
};
use windows::{
    Foundation::TimeSpan,
    UI::Composition::{
        AnimationIterationBehavior, ContainerVisual, Desktop::DesktopWindowTarget, SpriteVisual,
    },
    Win32::{
        Foundation::HWND,
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        System::WinRT::Composition::ICompositorDesktopInterop,
        UI::{
            Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, IDC_HAND, LoadCursorW,
                MA_NOACTIVATE, RegisterClassW, SW_SHOWNOACTIVATE, SWP_HIDEWINDOW, SWP_NOACTIVATE,
                SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetCursor, SetWindowPos,
                ShowWindow, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_SETCURSOR, WNDCLASSW,
                WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_POPUP,
            },
        },
    },
    core::{Interface, h, w},
};
use windows_numerics::{Vector2, Vector3};

/// The first act of a quiet start (after a hover or a menu, say).
const INITIAL_REST: Duration = Duration::from_secs(3);
/// The first act of a greeting or an arrival: at once.
const AT_ONCE: Duration = Duration::from_millis(60);
const BLINK: Duration = Duration::from_millis(180);
const SMILE: Duration = Duration::from_millis(1200);
const HOP: Duration = Duration::from_millis(420);
const APPEAR: Duration = Duration::from_millis(650);
const LEAVE: Duration = Duration::from_millis(600);
/// Walking speed, in logical pixels a second.
const SPEED: f32 = 55.0;
/// `WM_MOUSELEAVE`, which the bindings keep with the common controls.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// A hover reacts at most this often.
const HOVER_AGAIN: Duration = Duration::from_millis(1500);

/// What the pointer did to the companion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poke {
    /// It came over it: a smile and a hop.
    Hover,
    /// A click: love, and two hops.
    Click,
}

/// The pointer-catching window over the companion, while the pointer is on it.
static TRACKING: AtomicIsize = AtomicIsize::new(0);

/// The companion's window lets every click through. This small, invisible
/// window sits exactly over it while it stands still and takes the pointer:
/// hover and click make it react. While it walks or hops away it is hidden.
fn create_hit_window(owner: Option<HWND>) -> Result<HWND> {
    static CLASS: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let class = w!("MuelleMascotHit");
    // SAFETY: The class is registered once; the window is destroyed with
    // its companion.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        CLASS.get_or_init(|| {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(hit_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
        });
        Ok(CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            owner,
            None,
            Some(instance.into()),
            None,
        )?)
    }
}

unsafe extern "system" fn hit_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        // Never takes the focus from the app in front.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            // SAFETY: A shared system cursor.
            if let Ok(hand) = unsafe { LoadCursorW(None, IDC_HAND) } {
                unsafe { SetCursor(Some(hand)) };
            }
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            if TRACKING.swap(hwnd.0 as isize, Ordering::Relaxed) != hwnd.0 as isize {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: `track` describes our live window.
                let _ = unsafe { TrackMouseEvent(&mut track) };
                crate::app::companion_poke(Poke::Hover);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            TRACKING.store(0, Ordering::Relaxed);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            crate::app::companion_poke(Poke::Click);
            LRESULT(0)
        }
        // SAFETY: Default handling for our own window.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

/// How the companion comes on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Entrance {
    /// Simply there, resting (after a hover, a menu, an event act).
    #[default]
    Quiet,
    /// Just turned on: hello, with a double hop.
    Greet,
    /// Back from (or arriving at) the menu bar: fades in with a hop.
    Arrive,
}

/// What the app does after an act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Run the next act after this long.
    Wait(Duration),
    /// The companion left: show it at the other place (dock or menu bar).
    Travel,
}

/// Where a walk ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    /// Its resting place: right of the dock's middle; the bar's start.
    Home,
    /// The far end of its walk.
    Away,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Rest(Duration),
    Blink,
    Smile,
    Hop(u8),
    Walk(Spot),
    Appear,
    Leave,
    Travel,
}

/// The dock's round, about 40 seconds. With `trip`, it ends by leaving for
/// the menu bar.
fn dock_cycle(trip: bool) -> Vec<Act> {
    let secs = Duration::from_secs_f32;
    let mut acts = vec![
        Act::Rest(secs(3.5)),
        Act::Walk(Spot::Away),
        Act::Rest(secs(2.5)),
        Act::Blink,
        Act::Rest(secs(4.5)),
        Act::Hop(1),
        Act::Rest(secs(3.5)),
        Act::Walk(Spot::Home),
        Act::Rest(secs(4.0)),
        Act::Smile,
        Act::Rest(secs(5.0)),
    ];
    if trip {
        acts.extend([Act::Leave, Act::Travel]);
    }
    acts
}

/// A visit to the menu bar: across it and back, then home to the dock.
fn bar_cycle() -> Vec<Act> {
    let secs = Duration::from_secs_f32;
    vec![
        Act::Rest(secs(0.5)),
        Act::Walk(Spot::Away),
        Act::Smile,
        Act::Rest(secs(1.2)),
        Act::Blink,
        Act::Rest(secs(0.8)),
        Act::Walk(Spot::Home),
        Act::Rest(secs(0.5)),
        Act::Leave,
        Act::Travel,
    ]
}

/// The dock's walking range: from its resting place, right of the middle,
/// up to 200 logical pixels left, inside the rim even on a tiny dock.
fn stops(width: f32, body: f32, scale: f32) -> (f32, f32) {
    let room = (width - body).max(0.0);
    let home = room * 0.72;
    ((home - 200.0 * scale).max(0.0), home)
}

pub struct Companion {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    path: ContainerVisual,
    neutral: SpriteVisual,
    step: SpriteVisual,
    blink: SpriteVisual,
    happy: SpriteVisual,
    love: SpriteVisual,
    /// Takes the pointer over it while it stands still (see
    /// `create_hit_window`); none on a side dock, where it is turned.
    hit: Option<HWND>,
    /// The window's corner on screen and the companion's size in it.
    origin: (i32, i32),
    size: (f32, f32),
    /// Whether it stands still now (its hit window is over it).
    still: bool,
    last_poke: Option<Instant>,
    character: Character,
    place: Place,
    scale: f32,
    /// Where it rests, where its walk ends, and where it is.
    home: f32,
    away: f32,
    x: f32,
    /// Its height in the window at rest, and how high it hops.
    ground: f32,
    hop: f32,
    entrance: Entrance,
    script: VecDeque<Act>,
    /// The app's say: the next dock round ends with a visit to the menu bar.
    trip_due: bool,
    motion: bool,
}

impl Companion {
    /// Hidden construction, released if any compositor operation fails. In
    /// the menu bar, `owner` (the bar) keeps it above the bar.
    pub fn new(
        gfx: &Gfx,
        character: Character,
        place: Place,
        scale: f32,
        entrance: Entrance,
        owner: Option<HWND>,
    ) -> Result<Self> {
        let Geometry {
            rect,
            turn,
            crab,
            crab_top,
            cell,
            sprite: size,
        } = Geometry::new(character, place, scale);
        anyhow::ensure!(
            turn.width >= crab.0,
            "no hay espacio para la mascota en ese borde"
        );
        let on_bar = matches!(place, Place::Bar { .. });
        let (home, away) = if on_bar {
            // In from the stretch's start, across to its end.
            let room = (turn.width - crab.0).max(0.0);
            (0.0, room)
        } else {
            let (left, home) = stops(turn.width, crab.0, scale);
            (home, left)
        };
        let hop = if on_bar {
            // All the room above its head in the bar.
            crab_top
        } else {
            6.0 * cell
        };
        let hwnd = create_window(rect, owner)?;
        // Upright only: a side dock turns the scene, and it takes no pointer.
        let hit = (turn.matrix().is_none())
            .then(|| create_hit_window(owner))
            .transpose()
            .inspect_err(|_| {
                // SAFETY: Our own window, created above.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            })?;
        let built = || -> Result<Self> {
            // SAFETY: This is our own live, still-hidden window.
            let target = unsafe {
                gfx.compositor
                    .cast::<ICompositorDesktopInterop>()?
                    .CreateDesktopWindowTarget(hwnd, true)?
            };
            let root = gfx.compositor.CreateContainerVisual()?;
            root.SetRelativeSizeAdjustment(Vector2::new(1.0, 1.0))?;
            if let Some(matrix) = turn.matrix() {
                root.SetTransformMatrix(matrix)?;
            }
            let path = gfx.compositor.CreateContainerVisual()?;
            path.SetSize(Vector2::new(crab.0, crab.1))?;
            root.Children()?.InsertAtTop(&path)?;
            path.SetOffset(Vector3::new(home, crab_top, 0.0))?;
            let pose = |face, walking, visible| -> Result<SpriteVisual> {
                let image = match size {
                    Some(size) => pose_image_sized(character, face, walking, size),
                    None => pose_image(character, face, walking, cell as u32),
                };
                let pose = sprite(gfx, &image)?;
                pose.SetOpacity(if visible { 1.0 } else { 0.0 })?;
                path.Children()?.InsertAtTop(&pose)?;
                Ok(pose)
            };
            let neutral = pose(Face::Neutral, false, true)?;
            let step = pose(Face::Neutral, true, false)?;
            let blink = pose(Face::Sleep, false, false)?;
            let happy = pose(Face::Happy, false, false)?;
            let love = pose(Face::Love, false, false)?;
            target.SetRoot(&root)?;
            Ok(Self {
                hwnd,
                _target: target,
                path,
                neutral,
                step,
                blink,
                happy,
                love,
                hit,
                origin: (rect.left, rect.top),
                size: crab,
                still: false,
                last_poke: None,
                character,
                place,
                scale,
                home,
                away,
                x: home,
                ground: crab_top,
                hop,
                entrance,
                script: VecDeque::new(),
                trip_due: false,
                motion: false,
            })
        };
        match built() {
            Ok(view) => {
                // SAFETY: Show our own click-through window without taking focus.
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
                Ok(view)
            }
            Err(error) => {
                // SAFETY: Construction failed before ownership was transferred.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                    if let Some(hit) = hit {
                        let _ = DestroyWindow(hit);
                    }
                }
                Err(error)
            }
        }
    }

    /// Whether this companion already shows `character` at `place`. In the
    /// menu bar its exact stretch may change as the bar redraws (a new app
    /// name): the walk goes on rather than starting over.
    pub fn matches(&self, character: Character, place: Place, scale: f32) -> bool {
        self.character == character
            && self.scale == scale
            && match (self.place, place) {
                (Place::Bar { .. }, Place::Bar { .. }) => true,
                (mine, other) => mine == other,
            }
    }

    /// Whether its next dock round ends with a visit to the menu bar.
    pub fn set_trip_due(&mut self, due: bool) {
        self.trip_due = due;
    }

    /// Whether it is in the menu bar.
    pub fn on_bar(&self) -> bool {
        matches!(self.place, Place::Bar { .. })
    }

    /// Starts (or stops, without motion) its acts: none means a static
    /// companion with no timer. Restarts from its entrance after saver or
    /// reduced-motion changes; never catches up missed acts.
    pub fn set_motion(&mut self, enabled: bool) -> Result<Option<Duration>> {
        self.reset_pose()?;
        self.motion = enabled;
        self.script.clear();
        if !enabled {
            self.path.SetOpacity(1.0)?;
            self.place_hit(true);
            return Ok(None);
        }
        match self.entrance {
            Entrance::Quiet => {
                self.place_hit(true);
                Ok(Some(INITIAL_REST))
            }
            Entrance::Greet => {
                self.script.push_back(Act::Hop(2));
                Ok(Some(AT_ONCE))
            }
            Entrance::Arrive => {
                // Unseen until it appears, a moment from now.
                self.path.SetOpacity(0.0)?;
                self.script.push_back(Act::Appear);
                Ok(Some(AT_ONCE))
            }
        }
    }

    pub fn motion(&self) -> bool {
        self.motion
    }

    fn reset_pose(&self) -> Result<()> {
        self.path.StopAnimation(h!("Offset"))?;
        self.path.StopAnimation(h!("Opacity"))?;
        // A stopped fade keeps the opacity it had reached: back to whole.
        self.path.SetOpacity(1.0)?;
        self.path
            .SetOffset(Vector3::new(self.x, self.ground, 0.0))?;
        for pose in [
            &self.neutral,
            &self.step,
            &self.blink,
            &self.happy,
            &self.love,
        ] {
            pose.StopAnimation(h!("Opacity"))?;
            pose.SetOpacity(if pose == &self.neutral { 1.0 } else { 0.0 })?;
        }
        Ok(())
    }

    fn show(&self, face: &SpriteVisual) -> Result<()> {
        self.neutral.SetOpacity(0.0)?;
        face.SetOpacity(1.0)?;
        Ok(())
    }

    /// Runs the next act, only at an act's boundary, never per frame.
    pub fn advance(&mut self, gfx: &Gfx) -> Result<Next> {
        if !self.motion {
            // Only a reaction asked for this: back to its usual face.
            self.reset_pose()?;
            return Ok(Next::Wait(Duration::MAX));
        }
        if self.script.is_empty() {
            let acts = if self.on_bar() {
                bar_cycle()
            } else {
                dock_cycle(self.trip_due)
            };
            self.script.extend(acts);
        }
        let Some(act) = self.script.pop_front() else {
            return Ok(Next::Wait(INITIAL_REST));
        };
        // Gone, it stays as its last act left it until the app moves it.
        if act != Act::Travel {
            self.reset_pose()?;
        }
        let delay = match act {
            Act::Rest(time) => time,
            Act::Blink => {
                self.show(&self.blink)?;
                BLINK
            }
            Act::Smile => {
                self.show(&self.happy)?;
                SMILE
            }
            Act::Hop(times) => {
                self.show(&self.happy)?;
                self.hops(gfx, times)?;
                HOP * u32::from(times)
            }
            Act::Walk(spot) => {
                let to = match spot {
                    Spot::Home => self.home,
                    Spot::Away => self.away,
                };
                self.walk(gfx, to)?
            }
            Act::Appear => {
                self.show(&self.happy)?;
                self.fade(gfx, &[(0.0, 0.0), (0.35, 1.0), (1.0, 1.0)], APPEAR)?;
                self.hops(gfx, 1)?;
                APPEAR
            }
            Act::Leave => {
                self.show(&self.happy)?;
                self.hops(gfx, 1)?;
                self.fade(gfx, &[(0.0, 1.0), (0.6, 1.0), (1.0, 0.0)], LEAVE)?;
                LEAVE
            }
            Act::Travel => {
                self.place_hit(false);
                return Ok(Next::Travel);
            }
        };
        // It takes the pointer only while it stands where it is.
        self.place_hit(!matches!(act, Act::Walk(_) | Act::Leave));
        Ok(Next::Wait(delay))
    }

    /// Puts its hit window over it, or out of the way.
    fn place_hit(&mut self, still: bool) {
        self.still = still;
        let Some(hit) = self.hit else {
            return;
        };
        // SAFETY: Our own window; never activated, its z-order kept (it is
        // owned, so above the companion and the dock or bar).
        unsafe {
            let _ = if still {
                SetWindowPos(
                    hit,
                    None,
                    self.origin.0 + self.x.round() as i32,
                    self.origin.1 + self.ground.round() as i32,
                    self.size.0.round() as i32,
                    self.size.1.round() as i32,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
                )
            } else {
                SetWindowPos(
                    hit,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE | SWP_NOSIZE | SWP_HIDEWINDOW,
                )
            };
        }
    }

    /// The pointer came over it or clicked it: a smile and a hop, or love
    /// and two hops. How long it takes, or none when it is busy (walking)
    /// or was just hovered.
    pub fn react(&mut self, gfx: &Gfx, poke: Poke) -> Result<Option<Duration>> {
        if !self.still {
            return Ok(None);
        }
        let now = Instant::now();
        if poke == Poke::Hover
            && self
                .last_poke
                .is_some_and(|at| now.duration_since(at) < HOVER_AGAIN)
        {
            return Ok(None);
        }
        self.last_poke = Some(now);
        self.reset_pose()?;
        let (face, hops) = match poke {
            Poke::Hover => (&self.happy, 1),
            Poke::Click => (&self.love, 2),
        };
        self.show(face)?;
        if self.motion {
            self.hops(gfx, hops)?;
        }
        Ok(Some(HOP * u32::from(hops) + Duration::from_millis(500)))
    }

    /// Hops `times`, up and down, in `HOP` each.
    fn hops(&self, gfx: &Gfx, times: u8) -> Result<()> {
        if self.hop <= 0.0 {
            return Ok(());
        }
        let c = &gfx.compositor;
        let animation = c.CreateVector3KeyFrameAnimation()?;
        animation.SetDuration(span(HOP))?;
        let up =
            c.CreateCubicBezierEasingFunction(Vector2::new(0.2, 0.0), Vector2::new(0.4, 1.0))?;
        let down =
            c.CreateCubicBezierEasingFunction(Vector2::new(0.6, 0.0), Vector2::new(0.8, 1.0))?;
        let ground = Vector3::new(self.x, self.ground, 0.0);
        animation.InsertKeyFrame(0.0, ground)?;
        animation.InsertKeyFrameWithEasingFunction(
            0.45,
            Vector3::new(self.x, self.ground - self.hop, 0.0),
            &up,
        )?;
        animation.InsertKeyFrameWithEasingFunction(1.0, ground, &down)?;
        animation.SetIterationBehavior(AnimationIterationBehavior::Count)?;
        animation.SetIterationCount(i32::from(times.max(1)))?;
        self.path.StartAnimation(h!("Offset"), &animation)?;
        Ok(())
    }

    /// Fades the whole companion through `(progress, opacity)` keys.
    fn fade(&self, gfx: &Gfx, keys: &[(f32, f32)], time: Duration) -> Result<()> {
        let animation = gfx.compositor.CreateScalarKeyFrameAnimation()?;
        animation.SetDuration(span(time))?;
        for &(at, opacity) in keys {
            animation.InsertKeyFrame(at, opacity)?;
        }
        self.path.StartAnimation(h!("Opacity"), &animation)?;
        Ok(())
    }

    /// Walks to `to`, stepping all the way; returns how long it takes.
    fn walk(&mut self, gfx: &Gfx, to: f32) -> Result<Duration> {
        let distance = (to - self.x).abs();
        if distance < 1.0 {
            return Ok(Duration::from_millis(300));
        }
        let time = Duration::from_secs_f32((distance / (SPEED * self.scale)).clamp(1.2, 8.0));
        let c = &gfx.compositor;
        let linear = c.CreateLinearEasingFunction()?;
        let animation = c.CreateVector3KeyFrameAnimation()?;
        animation.SetDuration(span(time))?;
        animation.InsertKeyFrameWithEasingFunction(
            0.0,
            Vector3::new(self.x, self.ground, 0.0),
            &linear,
        )?;
        animation.InsertKeyFrameWithEasingFunction(
            1.0,
            Vector3::new(to, self.ground, 0.0),
            &linear,
        )?;
        self.path.StartAnimation(h!("Offset"), &animation)?;
        // Eight steps a second, alternating the standing and stepping poses.
        let frames = ((time.as_secs_f32() * 8.0).round() as u32).max(2);
        for (pose, phase) in [(&self.neutral, false), (&self.step, true)] {
            let steps = c.CreateScalarKeyFrameAnimation()?;
            steps.SetDuration(span(time))?;
            for frame in 0..=frames {
                let at = frame as f32 / frames as f32;
                let value = if (frame % 2 == 0) == phase { 1.0 } else { 0.0 };
                if frame > 0 {
                    steps.InsertKeyFrameWithEasingFunction(
                        (at - 0.0005).max(0.0),
                        1.0 - value,
                        &linear,
                    )?;
                }
                steps.InsertKeyFrameWithEasingFunction(at, value, &linear)?;
            }
            pose.StartAnimation(h!("Opacity"), &steps)?;
        }
        self.x = to;
        Ok(time)
    }
}

fn span(time: Duration) -> TimeSpan {
    TimeSpan {
        Duration: time.as_micros() as i64 * 10,
    }
}

impl Drop for Companion {
    fn drop(&mut self) {
        // SAFETY: This type uniquely owns its windows, on its UI thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            if let Some(hit) = self.hit {
                let _ = DestroyWindow(hit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dock_round_is_mostly_rest_and_returns_home() {
        for trip in [false, true] {
            let acts = dock_cycle(trip);
            let walks: Vec<_> = acts
                .iter()
                .filter_map(|act| match act {
                    Act::Walk(spot) => Some(*spot),
                    _ => None,
                })
                .collect();
            assert_eq!(walks, [Spot::Away, Spot::Home]);
            let rest: f32 = acts
                .iter()
                .map(|act| match act {
                    Act::Rest(time) => time.as_secs_f32(),
                    _ => 0.0,
                })
                .sum();
            assert!(rest >= 20.0, "{rest}");
            assert_eq!(acts.last() == Some(&Act::Travel), trip);
        }
    }

    #[test]
    fn a_bar_visit_crosses_and_comes_back_then_travels() {
        let acts = bar_cycle();
        let walks: Vec<_> = acts
            .iter()
            .filter(|act| matches!(act, Act::Walk(_)))
            .collect();
        assert_eq!(walks, [&Act::Walk(Spot::Away), &Act::Walk(Spot::Home)]);
        assert_eq!(&acts[acts.len() - 2..], [Act::Leave, Act::Travel]);
    }

    #[test]
    fn no_rest_leaves_it_still_more_than_five_seconds() {
        for act in dock_cycle(true).into_iter().chain(bar_cycle()) {
            if let Act::Rest(time) = act {
                assert!(time <= Duration::from_secs(5), "{time:?}");
            }
        }
    }

    #[test]
    fn walking_stays_inside_tiny_and_large_docks_at_every_scale() {
        for width in [40.0, 120.0, 400.0, 1600.0] {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let body = 22.0 * 1.6 * scale;
                let (left, home) = stops(width, body, scale);
                assert!(left >= 0.0 && left <= home, "{width} {scale}");
                assert!(home + body <= width.max(body), "{width} {scale}");
                assert!(home - left <= 200.0 * scale + 0.5, "{width} {scale}");
            }
        }
    }

    #[test]
    #[ignore = "Offscreen test-owned companion windows only; no input or system setting changes"]
    fn native_companions_greet_walk_visit_the_bar_and_release_their_windows() -> Result<()> {
        use muelle_core::frame::DockEdge;
        use windows::Win32::{
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow},
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let foreground = unsafe { GetForegroundWindow() };
        for character in [
            Character::Crab,
            Character::Cat,
            Character::Fox,
            Character::Capybara,
            Character::Mochi,
            Character::Girl,
        ] {
            for scale in [1.0, 1.25, 2.0] {
                let places = [
                    Place::Along {
                        left: -32000,
                        right: -31200,
                        top: -32000,
                        edge: DockEdge::Bottom,
                    },
                    Place::Along {
                        left: -32000,
                        right: -31200,
                        top: -32000,
                        edge: DockEdge::Left,
                    },
                    Place::Bar {
                        left: -32000,
                        right: -31500,
                        top: -32000,
                        bottom: -32000 + (28.0 * scale) as i32,
                    },
                ];
                for place in places {
                    let mut view =
                        Companion::new(&gfx, character, place, scale, Entrance::Greet, None)?;
                    view.set_trip_due(true);
                    let hwnd = view.hwnd;
                    let poses = view.path.Children()?;
                    assert_eq!(poses.Count()?, 5);
                    let retained = poses.First()?.Current()?;
                    assert_eq!(view.set_motion(false)?, None);
                    assert_eq!(view.advance(&gfx)?, Next::Wait(Duration::MAX));
                    // Turned on: hello at once, two hops.
                    assert_eq!(view.set_motion(true)?, Some(AT_ONCE));
                    assert_eq!(view.advance(&gfx)?, Next::Wait(HOP * 2));
                    // Then its round, out and back home, ending in a trip
                    // (the first round of a greeted one).
                    let mut traveled = false;
                    for _ in 0..20 {
                        if view.advance(&gfx)? == Next::Travel {
                            traveled = true;
                            break;
                        }
                    }
                    assert!(traveled, "{character:?} {place:?}");
                    assert_eq!(view.x, view.home);
                    assert_eq!(
                        poses.First()?.Current()?,
                        retained,
                        "acts must not regenerate sprite textures"
                    );
                    drop(view);
                    assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
                }
                // An arrival starts unseen and appears at once.
                let mut view =
                    Companion::new(&gfx, character, places[2], scale, Entrance::Arrive, None)?;
                assert_eq!(view.set_motion(true)?, Some(AT_ONCE));
                assert_eq!(view.path.Opacity()?, 0.0);
                assert_eq!(view.advance(&gfx)?, Next::Wait(APPEAR));
                assert!(view.on_bar());
            }
        }
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        Ok(())
    }
}
