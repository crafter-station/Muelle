//! Choices about what hangs from the menu bar.

use serde::{Deserialize, Serialize};

/// The day the calendar's weeks start on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeekStart {
    Monday,
    Sunday,
    Saturday,
    /// As Windows' regional settings say. Also what a day added by a newer
    /// version reads as (serde needs this fallback last).
    #[default]
    #[serde(other)]
    Auto,
}

/// What the icon before the app's name looks like.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuBarIcon {
    /// Windows' four squares.
    Windows,
    /// The user's own picture (`Config::menu_bar_icon_file`); the ring while
    /// there is none that can be read.
    Custom,
    /// A hollow ring. Also what a look added by a newer version reads as
    /// (serde needs this fallback last).
    #[default]
    #[serde(other)]
    Ring,
}

/// Which tiles the control center shows. The rest close up the space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ControlTiles {
    pub network: bool,
    pub media: bool,
    pub bluetooth: bool,
    pub nearby_sharing: bool,
    pub focus: bool,
    pub task_view: bool,
    pub cast: bool,
    pub display: bool,
    pub sound: bool,
    pub dark_mode: bool,
    pub calculator: bool,
    pub timer: bool,
    pub screenshot: bool,
}

impl Default for ControlTiles {
    fn default() -> Self {
        Self {
            network: true,
            media: true,
            bluetooth: true,
            nearby_sharing: true,
            focus: true,
            task_view: true,
            cast: true,
            display: true,
            sound: true,
            dark_mode: true,
            calculator: true,
            timer: true,
            screenshot: true,
        }
    }
}
