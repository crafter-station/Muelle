//! A retained, click-through companion. No frame or pointer polling: quiet
//! poses are static; only a short walk/blink runs on the compositor.

use super::{Character, Face, Gfx, Place, create_window, placement::Geometry, pose_image, sprite};
use anyhow::Result;
use std::time::Duration;
use windows::{
    Foundation::TimeSpan,
    UI::Composition::{ContainerVisual, Desktop::DesktopWindowTarget, SpriteVisual},
    Win32::{
        Foundation::HWND,
        System::WinRT::Composition::ICompositorDesktopInterop,
        UI::WindowsAndMessaging::{DestroyWindow, SW_SHOWNOACTIVATE, ShowWindow},
    },
    core::{Interface, h},
};
use windows_numerics::{Vector2, Vector3};

const INITIAL_REST: Duration = Duration::from_secs(6);
const WALK: Duration = Duration::from_secs(3);
const BLINK: Duration = Duration::from_millis(180);
const SMILE: Duration = Duration::from_millis(1200);

/// Short, varied pauses keep the companion visibly alive without continuous
/// animation, pretend status changes or idle-input reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Beat {
    Rest(u8),
    Blink,
    Walk(bool),
    Smile,
}

impl Beat {
    fn next(self) -> (Self, Duration) {
        match self {
            Self::Rest(0) => (Self::Walk(true), WALK),
            Self::Walk(true) => (Self::Rest(1), Duration::from_secs(8)),
            Self::Rest(1) => (Self::Blink, BLINK),
            Self::Blink => (Self::Rest(2), Duration::from_secs(14)),
            Self::Rest(2) => (Self::Smile, SMILE),
            Self::Smile => (Self::Rest(3), Duration::from_secs(10)),
            Self::Rest(_) => (Self::Walk(false), WALK),
            Self::Walk(false) => (Self::Rest(0), Duration::from_secs(12)),
        }
    }
}

/// The small walking range remains inside the available rim, even on a tiny
/// Dock. At most 96 logical pixels, around its right-hand resting area.
fn stops(width: f32, body: f32, scale: f32) -> (f32, f32) {
    let room = (width - body).max(0.0);
    let home = room * 0.72;
    ((home - 96.0 * scale).max(0.0), home)
}

pub struct Companion {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    path: ContainerVisual,
    neutral: SpriteVisual,
    step: SpriteVisual,
    blink: SpriteVisual,
    happy: SpriteVisual,
    character: Character,
    place: Place,
    scale: f32,
    left: f32,
    home: f32,
    x: f32,
    beat: Beat,
    motion: bool,
}

impl Companion {
    /// Hidden construction, released if any compositor operation fails.
    pub fn new(gfx: &Gfx, character: Character, place: Place, scale: f32) -> Result<Self> {
        let Geometry {
            rect,
            turn,
            crab,
            crab_top,
            cell,
        } = Geometry::new(character, place, scale);
        anyhow::ensure!(
            turn.width >= crab.0,
            "el borde del Dock no tiene espacio para la mascota"
        );
        let hwnd = create_window(rect)?;
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
            let (left, home) = stops(turn.width, crab.0, scale);
            path.SetOffset(Vector3::new(home, crab_top, 0.0))?;
            let pose = |face, walking, visible| -> Result<SpriteVisual> {
                let pose = sprite(gfx, &pose_image(character, face, walking, cell as u32))?;
                pose.SetOpacity(if visible { 1.0 } else { 0.0 })?;
                path.Children()?.InsertAtTop(&pose)?;
                Ok(pose)
            };
            let neutral = pose(Face::Neutral, false, true)?;
            let step = pose(Face::Neutral, true, false)?;
            let blink = pose(Face::Sleep, false, false)?;
            let happy = pose(Face::Happy, false, false)?;
            target.SetRoot(&root)?;
            Ok(Self {
                hwnd,
                _target: target,
                path,
                neutral,
                step,
                blink,
                happy,
                character,
                place,
                scale,
                left,
                home,
                x: home,
                beat: Beat::Rest(0),
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
                }
                Err(error)
            }
        }
    }

    pub fn matches(&self, character: Character, place: Place, scale: f32) -> bool {
        self.character == character && self.place == place && self.scale == scale
    }

    /// None means a static surface with no timer. Restart from a neutral rest
    /// after saver/reduced-motion changes; never catch up missed choreography.
    pub fn set_motion(&mut self, enabled: bool) -> Result<Option<Duration>> {
        self.reset_pose()?;
        self.beat = Beat::Rest(0);
        self.motion = enabled;
        Ok(enabled.then_some(INITIAL_REST))
    }

    pub fn motion(&self) -> bool {
        self.motion
    }

    fn reset_pose(&self) -> Result<()> {
        self.path.StopAnimation(h!("Offset"))?;
        let mut position = self.path.Offset()?;
        position.X = self.x;
        self.path.SetOffset(position)?;
        for pose in [&self.neutral, &self.step, &self.blink, &self.happy] {
            pose.StopAnimation(h!("Opacity"))?;
            pose.SetOpacity(if pose == &self.neutral { 1.0 } else { 0.0 })?;
        }
        Ok(())
    }

    /// Only called at a beat boundary, not for animation frames.
    pub fn advance(&mut self, gfx: &Gfx) -> Result<Option<Duration>> {
        if !self.motion {
            return Ok(None);
        }
        self.reset_pose()?;
        let (beat, delay) = self.beat.next();
        self.beat = beat;
        match beat {
            Beat::Blink => {
                self.neutral.SetOpacity(0.0)?;
                self.blink.SetOpacity(1.0)?;
            }
            Beat::Smile => {
                self.neutral.SetOpacity(0.0)?;
                self.happy.SetOpacity(1.0)?;
            }
            Beat::Walk(out) if self.left != self.home => {
                let destination = if out { self.left } else { self.home };
                let offset = self.path.Offset()?;
                let animation = gfx.compositor.CreateVector3KeyFrameAnimation()?;
                animation.SetDuration(TimeSpan {
                    Duration: WALK.as_micros() as i64 * 10,
                })?;
                let linear = gfx.compositor.CreateLinearEasingFunction()?;
                animation.InsertKeyFrameWithEasingFunction(0.0, offset, &linear)?;
                animation.InsertKeyFrameWithEasingFunction(
                    1.0,
                    Vector3::new(destination, offset.Y, 0.0),
                    &linear,
                )?;
                self.path.StartAnimation(h!("Offset"), &animation)?;
                for (pose, phase) in [(&self.neutral, false), (&self.step, true)] {
                    let steps = gfx.compositor.CreateScalarKeyFrameAnimation()?;
                    steps.SetDuration(TimeSpan {
                        Duration: WALK.as_micros() as i64 * 10,
                    })?;
                    for frame in 0..=24 {
                        let at = frame as f32 / 24.0;
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
                self.x = destination;
            }
            _ => {}
        }
        Ok(Some(delay))
    }
}

impl Drop for Companion {
    fn drop(&mut self) {
        // SAFETY: This type uniquely owns the window, on its UI thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_cycle_is_mostly_static_and_returns_home() {
        let mut beat = Beat::Rest(0);
        let (mut rest, mut active, mut walks) = (0, 0, Vec::new());
        for _ in 0..8 {
            let (next, time) = beat.next();
            if matches!(next, Beat::Rest(_)) {
                rest += time.as_millis();
            } else {
                active += time.as_millis();
            }
            if let Beat::Walk(out) = next {
                walks.push(out);
            }
            beat = next;
        }
        assert_eq!(beat, Beat::Rest(0));
        assert_eq!(walks, [true, false]);
        assert!(rest > active * 5);
    }

    #[test]
    fn the_first_action_is_a_walk_within_ten_seconds() {
        let (beat, _) = Beat::Rest(0).next();
        assert!(
            INITIAL_REST <= Duration::from_secs(10) && beat == Beat::Walk(true),
            "the first visible activity must not be a blink that is easy to miss"
        );
    }

    #[test]
    fn idle_actions_never_leave_more_than_fifteen_seconds_of_rest() {
        let mut beat = Beat::Rest(0);
        for _ in 0..16 {
            let (next, delay) = beat.next();
            assert!(delay <= Duration::from_secs(15));
            beat = next;
        }
    }

    #[test]
    fn walking_stays_inside_tiny_and_large_docks_at_every_scale() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            for character in [
                Character::Crab,
                Character::Cat,
                Character::Fox,
                Character::Capybara,
                Character::Mochi,
                Character::Girl,
            ] {
                let (columns, _) = super::super::cells(character);
                let body = columns as f32 * (scale * 1.6_f32).round().max(2.0);
                for width in [0.0, 20.0, 140.0, 1600.0] {
                    let (left, home) = stops(width, body, scale);
                    assert!(left >= 0.0 && left <= home);
                    assert!(home <= (width - body).max(0.0));
                    assert!(home - left <= 96.0 * scale);
                }
            }
        }
    }

    #[test]
    #[ignore = "Offscreen test-owned companion windows only; no input or system setting changes"]
    fn native_all_companions_reuse_four_poses_and_release_their_windows() -> Result<()> {
        use muelle_core::frame::DockEdge;
        use windows::Win32::{
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow},
        };
        // SAFETY: A dispatcher on this isolated test thread for Composition.
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
            for edge in [DockEdge::Bottom, DockEdge::Left, DockEdge::Right] {
                for scale in [1.0, 1.5, 2.0] {
                    let place = Place::Along {
                        left: -32000,
                        right: -31200,
                        top: -32000,
                        edge,
                    };
                    let mut view = Companion::new(&gfx, character, place, scale)?;
                    let hwnd = view.hwnd;
                    let poses = view.path.Children()?;
                    assert_eq!(poses.Count()?, 4);
                    let retained = poses.First()?.Current()?;
                    assert_eq!(view.set_motion(false)?, None);
                    assert_eq!(view.advance(&gfx)?, None);
                    assert_eq!(view.set_motion(true)?, Some(INITIAL_REST));
                    assert_eq!(view.advance(&gfx)?, Some(WALK));
                    assert_eq!(view.beat, Beat::Walk(true));
                    assert_eq!(view.x, view.left);
                    for _ in 1..8 {
                        assert!(view.advance(&gfx)?.is_some());
                    }
                    assert_eq!(view.beat, Beat::Rest(0));
                    assert_eq!(view.x, view.home);
                    assert_eq!(
                        poses.First()?.Current()?,
                        retained,
                        "walking must not regenerate sprite textures"
                    );
                    assert_eq!(view.set_motion(false)?, None);
                    assert_eq!(view.neutral.Opacity()?, 1.0);
                    assert_eq!(view.step.Opacity()?, 0.0);
                    assert_eq!(view.blink.Opacity()?, 0.0);
                    assert_eq!(view.happy.Opacity()?, 0.0);
                    drop(view);
                    assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
                }
            }
        }
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        Ok(())
    }
}
