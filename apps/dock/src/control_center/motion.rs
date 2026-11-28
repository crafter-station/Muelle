//! The control center's movement, as macOS moves its own: tiles spring out of
//! the menu bar button and fold back into it, a held tile grows into its page,
//! and the page folds back into its tile.
//!
//! Everything runs in the compositor; nothing here ticks per frame.

use anyhow::Result;
use windows::{
    Foundation::TimeSpan,
    UI::Composition::{
        AnimationDelayBehavior, CompositionEasingFunction, Compositor, ContainerVisual,
    },
    core::{Interface, h},
};
use windows_numerics::{Vector2, Vector3};

pub const OPEN_MS: u64 = 360;
/// The longest a tile waits before setting off.
const STAGGER_MS: f32 = 70.0;
pub const CLOSE_MS: u64 = 190;
pub const EXPAND_MS: u64 = 340;
pub const COLLAPSE_MS: u64 = 240;
const PRESS_MS: u64 = 120;

/// How far a tile travels towards the button, and how small it starts.
const REACH: f32 = 0.32;
const SMALL: f32 = 0.55;
/// The slight overshoot before settling.
const BOUNCE: f32 = 1.035;
const PRESSED: f32 = 0.93;
/// The grid steps back while a page is open.
const BEHIND: f32 = 0.9;

fn time(milliseconds: u64) -> TimeSpan {
    TimeSpan {
        Duration: (milliseconds * 10_000) as i64,
    }
}

fn v3(x: f32, y: f32) -> Vector3 {
    Vector3 { X: x, Y: y, Z: 0.0 }
}

fn uniform(scale: f32) -> Vector3 {
    Vector3 {
        X: scale,
        Y: scale,
        Z: 1.0,
    }
}

/// Where a visual is, and how it looks.
#[derive(Clone, Copy)]
struct Pose {
    offset: Vector3,
    scale: Vector3,
    opacity: f32,
}

impl Pose {
    fn rest() -> Self {
        Self {
            offset: v3(0.0, 0.0),
            scale: uniform(1.0),
            opacity: 1.0,
        }
    }
}

pub struct Motion {
    compositor: Compositor,
    /// Off when Windows' animation effects are: poses are then set at once.
    enabled: bool,
}

impl Motion {
    pub fn new(compositor: &Compositor, enabled: bool) -> Self {
        Self {
            compositor: compositor.clone(),
            enabled,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn ease(&self, from: (f32, f32), to: (f32, f32)) -> Result<CompositionEasingFunction> {
        Ok(self
            .compositor
            .CreateCubicBezierEasingFunction(
                Vector2::new(from.0, from.1),
                Vector2::new(to.0, to.1),
            )?
            .cast()?)
    }

    /// Decelerating: quick to start, gentle to arrive.
    fn settle(&self) -> Result<CompositionEasingFunction> {
        self.ease((0.16, 0.84), (0.28, 1.0))
    }

    /// Accelerating: for things leaving.
    fn leave(&self) -> Result<CompositionEasingFunction> {
        self.ease((0.4, 0.0), (0.9, 0.6))
    }

    fn set(visual: &ContainerVisual, pose: Pose) -> Result<()> {
        visual.StopAnimation(h!("Offset"))?;
        visual.StopAnimation(h!("Scale"))?;
        visual.StopAnimation(h!("Opacity"))?;
        visual.SetOffset(pose.offset)?;
        visual.SetScale(pose.scale)?;
        visual.SetOpacity(pose.opacity)?;
        Ok(())
    }

    /// Moves `visual` to `to`, from `from` or from wherever it is now.
    #[expect(
        clippy::too_many_arguments,
        reason = "One animation is its ends, timing and feel"
    )]
    fn go(
        &self,
        visual: &ContainerVisual,
        from: Option<Pose>,
        to: Pose,
        duration: u64,
        delay: u64,
        fade: u64,
        easing: &CompositionEasingFunction,
        bounce: bool,
    ) -> Result<()> {
        if !self.enabled {
            return Self::set(visual, to);
        }
        let c = &self.compositor;
        let offset = c.CreateVector3KeyFrameAnimation()?;
        let scale = c.CreateVector3KeyFrameAnimation()?;
        let opacity = c.CreateScalarKeyFrameAnimation()?;
        if let Some(from) = from {
            offset.InsertKeyFrame(0.0, from.offset)?;
            scale.InsertKeyFrame(0.0, from.scale)?;
            opacity.InsertKeyFrame(0.0, from.opacity)?;
        } else {
            offset.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            scale.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            opacity.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        }
        if bounce {
            let past = Vector3 {
                X: to.scale.X * BOUNCE,
                Y: to.scale.Y * BOUNCE,
                Z: 1.0,
            };
            scale.InsertKeyFrameWithEasingFunction(0.66, past, easing)?;
        }
        offset.InsertKeyFrameWithEasingFunction(1.0, to.offset, easing)?;
        scale.InsertKeyFrameWithEasingFunction(1.0, to.scale, easing)?;
        opacity.InsertKeyFrameWithEasingFunction(1.0, to.opacity, easing)?;
        offset.SetDuration(time(duration))?;
        scale.SetDuration(time(duration))?;
        opacity.SetDuration(time(fade.max(1)))?;
        if delay > 0 {
            // Held at its starting pose until its turn, not shown at rest.
            let held = AnimationDelayBehavior::SetInitialValueBeforeDelay;
            offset.SetDelayBehavior(held)?;
            offset.SetDelayTime(time(delay))?;
            scale.SetDelayBehavior(held)?;
            scale.SetDelayTime(time(delay))?;
            opacity.SetDelayBehavior(held)?;
            opacity.SetDelayTime(time(delay))?;
        }
        visual.StartAnimation(h!("Offset"), &offset)?;
        visual.StartAnimation(h!("Scale"), &scale)?;
        visual.StartAnimation(h!("Opacity"), &opacity)?;
        Ok(())
    }

    /// A tile tucked towards the button at `origin`, from its own `center`.
    fn tucked(center: Vector2, origin: Vector2) -> Pose {
        Pose {
            offset: v3((origin.X - center.X) * REACH, (origin.Y - center.Y) * REACH),
            scale: uniform(SMALL),
            opacity: 0.0,
        }
    }

    /// Tiles spring out of the button, the nearest first. Each tile scales
    /// about its `CenterPoint`, which is its `center`.
    pub fn open(&self, tiles: &[(ContainerVisual, Vector2)], origin: Vector2) -> Result<()> {
        let far = tiles
            .iter()
            .map(|(_, center)| distance(*center, origin))
            .fold(1.0_f32, f32::max);
        let settle = self.settle()?;
        for (tile, center) in tiles {
            let delay = (distance(*center, origin) / far * STAGGER_MS) as u64;
            self.go(
                tile,
                Some(Self::tucked(*center, origin)),
                Pose::rest(),
                OPEN_MS,
                delay,
                OPEN_MS / 2,
                &settle,
                true,
            )?;
        }
        Ok(())
    }

    /// One menu springs out of the button, at once.
    pub fn appear(&self, menu: &ContainerVisual, center: Vector2, origin: Vector2) -> Result<()> {
        let settle = self.settle()?;
        self.go(
            menu,
            Some(Self::tucked(center, origin)),
            Pose::rest(),
            OPEN_MS,
            0,
            OPEN_MS / 2,
            &settle,
            true,
        )
    }

    /// Everything folds back into the button.
    pub fn close(&self, tiles: &[(ContainerVisual, Vector2)], origin: Vector2) -> Result<()> {
        let leave = self.leave()?;
        for (tile, center) in tiles {
            self.go(
                tile,
                None,
                Self::tucked(*center, origin),
                CLOSE_MS,
                0,
                CLOSE_MS,
                &leave,
                false,
            )?;
        }
        Ok(())
    }

    /// The pose that lays the page (`page`: left, top, width, height) over
    /// the tile it comes from. The page scales about its top-left corner.
    fn over(tile: [f32; 4], page: [f32; 4]) -> Pose {
        Pose {
            offset: v3(tile[0] - page[0], tile[1] - page[1]),
            scale: Vector3 {
                X: tile[2] / page[2].max(1.0),
                Y: tile[3] / page[3].max(1.0),
                Z: 1.0,
            },
            opacity: 0.0,
        }
    }

    /// `tile` grows into the page while the grid steps back.
    pub fn expand(
        &self,
        page: &ContainerVisual,
        grid: &ContainerVisual,
        tile: [f32; 4],
        area: [f32; 4],
    ) -> Result<()> {
        let settle = self.settle()?;
        self.go(
            page,
            Some(Self::over(tile, area)),
            Pose::rest(),
            EXPAND_MS,
            0,
            EXPAND_MS / 3,
            &settle,
            false,
        )?;
        self.go(
            grid,
            None,
            Pose {
                scale: uniform(BEHIND),
                opacity: 0.0,
                ..Pose::rest()
            },
            EXPAND_MS,
            0,
            EXPAND_MS / 2,
            &settle,
            false,
        )
    }

    /// The page folds back into its tile and the grid returns.
    pub fn collapse(
        &self,
        page: &ContainerVisual,
        grid: &ContainerVisual,
        tile: [f32; 4],
        area: [f32; 4],
    ) -> Result<()> {
        let settle = self.settle()?;
        self.go(
            page,
            None,
            Self::over(tile, area),
            COLLAPSE_MS,
            0,
            COLLAPSE_MS,
            &settle,
            false,
        )?;
        self.go(
            grid,
            None,
            Pose::rest(),
            COLLAPSE_MS,
            0,
            COLLAPSE_MS,
            &settle,
            false,
        )
    }

    /// A tile gives way under the pointer, and springs back.
    pub fn press(&self, tile: &ContainerVisual, down: bool) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let settle = self.settle()?;
        let pose = Pose {
            scale: uniform(if down { PRESSED } else { 1.0 }),
            ..Pose::rest()
        };
        self.go(tile, None, pose, PRESS_MS * 2, 0, 1, &settle, !down)
    }
}

fn distance(a: Vector2, b: Vector2) -> f32 {
    (a.X - b.X).hypot(a.Y - b.Y)
}
