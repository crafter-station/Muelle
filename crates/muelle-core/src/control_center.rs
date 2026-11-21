//! macOS-style Control Center panel models and layout.
//!
//! Organizes connectivity tiles, media playback, focus/DND, display and
//! sound sliders, quick system shortcuts, and battery status into a
//! structured glass panel.

/// Width of the Control Center panel in logical pixels.
pub const PANEL_WIDTH: f32 = 320.0;
/// Corner radius of the glass panel.
pub const PANEL_RADIUS: f32 = 18.0;
/// Padding inside the panel.
pub const PADDING: f32 = 14.0;
/// Gap between tiles.
pub const TILE_GAP: f32 = 10.0;
/// Standard tile corner radius.
pub const TILE_RADIUS: f32 = 14.0;
/// Height of a slider control track in logical pixels.
pub const SLIDER_HEIGHT: f32 = 42.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[must_use]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.width && py >= self.y && py <= self.y + self.height
    }
}

/// State of a continuous slider control (brightness or volume).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliderState {
    /// Normalized value from 0.0 to 1.0.
    pub value: f32,
    /// Whether the output is muted (volume only).
    pub muted: bool,
    /// Whether the hardware or monitor supports software adjustments.
    pub supported: bool,
    /// Visual drag in progress.
    pub dragging: bool,
}

impl Default for SliderState {
    fn default() -> Self {
        Self {
            value: 0.5,
            muted: false,
            supported: true,
            dragging: false,
        }
    }
}

impl SliderState {
    #[must_use]
    pub fn new(value: f32, muted: bool, supported: bool) -> Self {
        Self {
            value: value.clamp(0.0, 1.0),
            muted,
            supported,
            dragging: false,
        }
    }

    /// Sets the slider value from an x-coordinate relative to the track bounds.
    pub fn set_from_coordinate(&mut self, relative_x: f32, track_width: f32) {
        if track_width > 0.0 && self.supported {
            self.value = (relative_x / track_width).clamp(0.0, 1.0);
        }
    }
}

/// Connectivity tile statuses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectivityState {
    pub wifi_enabled: bool,
    pub wifi_ssid: Option<String>,
    pub bluetooth_enabled: bool,
    pub bluetooth_device_name: Option<String>,
    pub airplane_mode: bool,
    pub hotspot_enabled: bool,
    pub hotspot_supported: bool,
}

/// Now Playing tile status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaState {
    pub title: String,
    pub artist: String,
    pub is_playing: bool,
    pub has_media: bool,
}

/// Battery footer status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: bool,
    pub saver: bool,
}

/// Layout computation for the Control Center panel.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlCenterLayout {
    pub panel_rect: Rect,
    pub connectivity_rect: Rect,
    pub now_playing_rect: Rect,
    pub focus_rect: Rect,
    pub display_slider_rect: Rect,
    pub sound_slider_rect: Rect,
    pub shortcuts_rect: Rect,
    pub footer_rect: Rect,
}

impl ControlCenterLayout {
    /// Computes the layout given the top-right anchor position of the menu bar button.
    #[must_use]
    pub fn compute(anchor_x: f32, anchor_y: f32, scale: f32) -> Self {
        let width = PANEL_WIDTH * scale;
        let pad = PADDING * scale;
        let gap = TILE_GAP * scale;
        let inner_width = width - pad * 2.0;

        let left = (anchor_x - width + 20.0 * scale).max(10.0 * scale);
        let mut top = anchor_y + 6.0 * scale;

        let panel_top = top;

        // 1. Connectivity block & Focus/Now Playing
        // Left column: Wi-Fi, Bluetooth, Airplane mode (width ~ inner_width * 0.5 - gap * 0.5)
        let col_width = (inner_width - gap) / 2.0;
        let connectivity_rect = Rect::new(left + pad, top, col_width, 130.0 * scale);

        // Right column: Focus tile (top) + Hotspot (bottom)
        let focus_rect = Rect::new(left + pad + col_width + gap, top, col_width, 60.0 * scale);
        top += 130.0 * scale + gap;

        // 2. Now Playing tile
        let now_playing_rect = Rect::new(left + pad, top, inner_width, 64.0 * scale);
        top += 64.0 * scale + gap;

        // 3. Display Slider
        let slider_h = SLIDER_HEIGHT * scale;
        let display_slider_rect = Rect::new(left + pad, top, inner_width, slider_h);
        top += slider_h + gap;

        // 4. Sound Slider
        let sound_slider_rect = Rect::new(left + pad, top, inner_width, slider_h);
        top += slider_h + gap;

        // 5. Shortcuts row
        let shortcuts_rect = Rect::new(left + pad, top, inner_width, 44.0 * scale);
        top += 44.0 * scale + gap;

        // 6. Footer (Battery + Settings)
        let footer_rect = Rect::new(left + pad, top, inner_width, 32.0 * scale);
        top += 32.0 * scale + pad;

        let total_height = top - panel_top;
        let panel_rect = Rect::new(left, panel_top, width, total_height);

        Self {
            panel_rect,
            connectivity_rect,
            now_playing_rect,
            focus_rect,
            display_slider_rect,
            sound_slider_rect,
            shortcuts_rect,
            footer_rect,
        }
    }
}

/// Identifies an interactive hit element in the Control Center panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlCenterAction {
    ToggleWifi,
    ToggleBluetooth,
    ToggleAirplaneMode,
    ToggleHotspot,
    ToggleFocus,
    TogglePlayPause,
    MediaPrevious,
    MediaNext,
    DisplaySlider,
    SoundSlider,
    ToggleMute,
    ToggleDarkMode,
    OpenNightLight,
    OpenBatterySaver,
    OpenScreenshot,
    OpenCalculator,
    OpenClock,
    OpenSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_state_clamping_and_relative_coordinates() {
        let mut slider = SliderState::new(0.5, false, true);
        assert_eq!(slider.value, 0.5);

        // Midpoint
        slider.set_from_coordinate(100.0, 200.0);
        assert!((slider.value - 0.5).abs() < 1e-4);

        // Below 0
        slider.set_from_coordinate(-10.0, 200.0);
        assert_eq!(slider.value, 0.0);

        // Above max
        slider.set_from_coordinate(250.0, 200.0);
        assert_eq!(slider.value, 1.0);
    }

    #[test]
    fn unsupported_slider_ignores_drag() {
        let mut slider = SliderState::new(0.75, false, false);
        assert_eq!(slider.value, 0.75);
        slider.set_from_coordinate(50.0, 200.0);
        // Stays unchanged because supported is false
        assert_eq!(slider.value, 0.75);
    }

    #[test]
    fn layout_computes_monotonic_non_overlapping_vertical_bounds() {
        let layout = ControlCenterLayout::compute(800.0, 28.0, 1.0);
        assert!(layout.panel_rect.width >= PANEL_WIDTH);
        assert!(layout.display_slider_rect.y > layout.now_playing_rect.y);
        assert!(layout.sound_slider_rect.y > layout.display_slider_rect.y);
        assert!(layout.shortcuts_rect.y > layout.sound_slider_rect.y);
        assert!(layout.footer_rect.y > layout.shortcuts_rect.y);
        assert!(
            layout
                .panel_rect
                .contains(layout.display_slider_rect.x, layout.display_slider_rect.y)
        );
    }
}
