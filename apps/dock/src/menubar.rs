//! Minimal macOS-style menu bar across the top of the screen.
//!
//! Hiding the Windows taskbar also hides its clock and status area; this bar
//! brings them back: the name of the app in front, the battery, a control
//! center button (opens the Windows quick settings: network, volume,
//! brightness) and the date and time (opens the notification center).
//!
//! Cost while idle: one timer that fires once a minute, on the minute, to
//! redraw the clock. Everything else is event-driven.
//!
//! With auto-hide on, the bar reserves no space and stays off screen. Resting
//! the pointer briefly at the top edge reveals it.
//!
//! Status items, right to left as on a Mac: clock, control center, search,
//! network, volume, battery, then Task View and Widgets. Search, Task View and
//! Widgets appear only if the user has them on the Windows taskbar. Network
//! and volume changes arrive through separate notifications; nothing polls.

use anyhow::Result;
use muelle_config::{IslandStyle, Language, MenuBarItem};
use muelle_shell::{
    accessories::{Kind as AccessoryKind, Snapshot as AccessorySnapshot},
    appbar::{AppBar, Edge},
    brightness, launcher,
    radio::{self, RadioKind},
    status::{self, AudioWatch, Network, NetworkWatch, Privacy, TaskbarButtons, Volume},
    system::{self, Battery},
    tray::{TrayButton, TrayHost, TrayIconInfo, TrayKey},
    watch::RegistryWatch,
};
use std::{
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    Foundation::{TimeSpan, TypedEventHandler},
    UI::{
        Color,
        Composition::{
            CompositionBatchTypes, CompositionBrush, CompositionRoundedRectangleGeometry,
            CompositionScopedBatch, CompositionStretch, CompositionSurfaceBrush, Compositor,
            ContainerVisual, Desktop::DesktopWindowTarget, ShapeVisual, SpriteVisual,
            VisualCollection,
        },
    },
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM},
        Graphics::Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute},
        System::{
            LibraryLoader::GetModuleHandleW, SystemInformation::GetLocalTime,
            WinRT::Composition::ICompositorDesktopInterop,
        },
        UI::Input::KeyboardAndMouse::{
            GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
        },
        UI::WindowsAndMessaging::{
            CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, HHOOK,
            HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW, MSLLHOOKSTRUCT,
            PBT_POWERSETTINGCHANGE, PostMessageW, RegisterClassW, SWP_HIDEWINDOW, SWP_NOACTIVATE,
            SWP_SHOWWINDOW, SetCursor, SetTimer, SetWindowPos, SetWindowsHookExW,
            UnhookWindowsHookEx, WH_MOUSE_LL, WM_APP, WM_CAPTURECHANGED, WM_LBUTTONDOWN,
            WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST,
            WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETTINGCHANGE, WM_TIMECHANGE, WM_TIMER,
            WM_XBUTTONDOWN, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
    core::{BOOL, Interface, h, w},
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    accessories, app, edge_watch,
    gfx::{Gfx, Ink, LabelBrush},
    mascot,
    media::{self, Control as MediaControl, Track},
    scene::Theme,
    strings,
    usage::{
        self, ActiveProviders, Monitor as UsageMonitor, Provider, ProviderUsage, Snapshot, State,
    },
};

// Logical pixels.
const HEIGHT: f32 = 28.0;
const SIDE_MARGIN: f32 = 14.0;
const ITEM_GAP: f32 = 16.0;
/// Extra clickable room around an item.
const HIT_SLOP: f32 = 7.0;

const MEDIA_CHANGED: u32 = WM_APP + 28;
const VOLUME_CAPTURE_LOST: u32 = WM_APP + 31;
const AUDIO_CHANGED: u32 = WM_APP + 32;
const AUDIO_TICK: u32 = WM_APP + 33;
/// A worker thread left new Control Center readings in `PANEL_INBOX`.
const PANEL_DATA_CHANGED: u32 = WM_APP + 34;
/// The tray relay saw an icon appear, change or go (coalesced).
pub(crate) const TRAY_CHANGED: u32 = WM_APP + 35;
const VOLUME_TIMER: usize = 4;
const ACCESSORIES_CHANGED: u32 = WM_APP + 30;

const CLOCK_TIMER: usize = 1;
const POINTER_TIMER: usize = 2;
/// Reads the tray icons once a burst of changes has settled.
const TRAY_TIMER: usize = 5;
const TRAY_SETTLE_MS: u32 = 100;
/// The apps are asked to announce their icons again at most this often.
const TRAY_REFRESH_MIN: Duration = Duration::from_secs(30);
/// A clock showing seconds redraws the whole bar at least this often.
const CLOCK_FULL_EVERY: Duration = Duration::from_secs(59);
const POINTER_CHECK_MS: u32 = 100;
const HIDE_ANIMATION_TIMER: usize = 3;
const REVEAL_MS: u32 = 180;
const HIDE_MS: u32 = 130;
// Recovery only; normal completion comes from the compositor, not this timer.
const HIDE_FALLBACK_MS: u32 = HIDE_MS + 250;
const HIDE_ANIMATION_DONE: u32 = WM_APP + 29;
const ISLAND_SINGLE_WIDTH: f32 = 145.0;
const ISLAND_EXPANDED_WIDTH: f32 = 440.0;
const ISLAND_ROW_HEIGHT: f32 = 118.0;
const BUBBLE_VERTICAL_INSET: f32 = 2.5;
const BUBBLE_MUSIC_WIDTH: f32 = 164.0;
const BUBBLE_PROVIDER_WIDTH: f32 = 62.0;
const BUBBLE_USAGE_PADDING: f32 = 16.0;
const PANEL_GAP: f32 = 5.0;
const PANEL_RADIUS: f32 = 18.0;
const PANEL_SHADOW_MARGIN: f32 = 20.0;
const WM_POWERBROADCAST: u32 = 0x0218;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// AppBar notifications need a message id; the bar ignores them.
const APPBAR_CALLBACK: u32 = WM_APP + 20;
/// Posted by the network watch; audio uses its own coalesced notification.
const STATUS_CHANGED: u32 = WM_APP + 21;
/// Posted by the bar to itself to update from the message loop.
const TICK: u32 = WM_APP + 22;
const USAGE_CHANGED: u32 = WM_APP + 23;
const DISMISS_ISLAND: u32 = WM_APP + 24;
/// Privacy activity or the user's taskbar preferences changed.
const REGISTRY_CHANGED: u32 = WM_APP + 25;
/// `wParam` of WM_SETTINGCHANGE when the work area changes, which every
/// AppBar reservation (the dock's, this bar's) causes.
const SPI_SETWORKAREA: usize = 0x002F;
/// A `TICK` is already posted.
static TICK_PENDING: AtomicBool = AtomicBool::new(false);
static AUDIO_TICK_PENDING: AtomicBool = AtomicBool::new(false);
static TRIGGERS_LEFT: AtomicI32 = AtomicI32::new(0);
static TRIGGERS_RIGHT: AtomicI32 = AtomicI32::new(0);
static PANEL_LEFT: AtomicI32 = AtomicI32::new(0);
static PANEL_RIGHT: AtomicI32 = AtomicI32::new(0);
static BAR_TOP: AtomicI32 = AtomicI32::new(0);
static BAR_BOTTOM: AtomicI32 = AtomicI32::new(0);
static PANEL_BOTTOM: AtomicI32 = AtomicI32::new(0);
/// The bar zone a Control Center or Sound popup hangs from.
static ZONE_LEFT: AtomicI32 = AtomicI32::new(0);
static ZONE_RIGHT: AtomicI32 = AtomicI32::new(0);
/// Readings from worker threads, drained on the UI thread.
static PANEL_INBOX: Mutex<Vec<PanelUpdate>> = Mutex::new(Vec::new());
/// The hook exists only while the island is expanded.
static ISLAND_HOOK_WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Segoe Fluent Icons code points.
pub(crate) mod glyph {
    pub const ETHERNET: char = '\u{E839}';
    pub const OFFLINE: char = '\u{F384}';
    pub const VOLUME: [char; 4] = ['\u{E992}', '\u{E993}', '\u{E994}', '\u{E995}'];
    pub const MUTE: char = '\u{E74F}';
    pub const SEARCH: char = '\u{E721}';
    pub const TASK_VIEW: char = '\u{E7C4}';
    pub const WIDGETS: char = '\u{F246}';
    /// ChevronDown: the arrow that drops the hidden-icons popup, as on a Mac.
    pub const HIDDEN_ICONS: char = '\u{E70D}';
    pub const MICROPHONE: char = '\u{E720}';
    pub const CAMERA: char = '\u{E722}';
    pub const FOCUS: char = '\u{E708}';
    pub const MOUSE: char = '\u{E962}';
    pub const HEADSET: char = '\u{E95B}';
    pub const BLUETOOTH: char = '\u{E702}';
    pub const WIFI: char = '\u{E701}';
    pub const BRIGHTNESS: char = '\u{E706}';
    pub const CHEVRON_RIGHT: char = '\u{E76C}';
}

/// The menu-bar items that open a Mac-style dropdown instead of a Windows flyout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropdown {
    Input,
    Network,
    Battery,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    AccessoryIndicator,
    /// Escape hatch to the Windows quick settings flyout.
    #[allow(dead_code)]
    QuickSettings,
    ControlCenter,
    Notifications,
    ToggleDnd,
    Sound,
    Network,
    Battery,
    Search,
    TaskView,
    Widgets,
    HiddenIcons,
    InputLanguage,
    Microphone,
    Camera,
    Bluetooth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuBarWidgets {
    pub search: Option<bool>,
    pub task_view: Option<bool>,
    pub widgets: Option<bool>,
    pub bluetooth: bool,
    pub battery_percent: Option<bool>,
    pub island_style: IslandStyle,
    /// Left to right.
    pub order: Vec<MenuBarItem>,
    pub hidden: Vec<MenuBarItem>,
    pub clock: system::ClockFormat,
    /// 0..=100, see `glass_alpha`.
    pub glass_strength: u8,
}

/// Bar items from the right edge inward (the order they are laid out in),
/// for the `order` given left to right and only those `visible` accepts.
fn bar_items(order: &[MenuBarItem], visible: impl Fn(MenuBarItem) -> bool) -> Vec<MenuBarItem> {
    order
        .iter()
        .rev()
        .copied()
        .filter(|&item| visible(item))
        .collect()
}

/// Alpha of the glass tint for a strength of 0..=100.
fn glass_alpha(light: bool, strength: u8) -> u8 {
    let s = f32::from(strength.min(100));
    let alpha = if light {
        40.0 + s * 1.2
    } else {
        50.0 + s * 1.3
    };
    alpha.round().clamp(0.0, 255.0) as u8
}

impl Default for MenuBarWidgets {
    fn default() -> Self {
        Self {
            search: None,
            task_view: None,
            widgets: None,
            bluetooth: true,
            battery_percent: None,
            island_style: IslandStyle::Notch,
            order: MenuBarItem::DEFAULT_ORDER.to_vec(),
            hidden: Vec::new(),
            clock: system::ClockFormat {
                date: true,
                weekday: true,
                seconds: false,
                h24: None,
            },
            glass_strength: 50,
        }
    }
}

/// Network, volume and taskbar buttons, read only when Windows reports a change.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Status {
    network: Network,
    volume: Option<Volume>,
    buttons: TaskbarButtons,
    privacy: Privacy,
    battery_percentage: bool,
}

/// The clock label's visual and where it sits, kept to redraw it alone.
struct ClockSlot {
    visual: SpriteVisual,
    /// Right edge of the label, in window pixels.
    right: f32,
    /// Width and bar height as drawn.
    width: f32,
    bar_height: f32,
    /// Extra clickable room around the label.
    slop: f32,
}

/// What a tick of the clock timer redraws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockTick {
    /// The whole bar: status, battery, everything read on each draw.
    Full,
    /// Only the clock label.
    LabelOnly,
    /// Nothing: the bar is not on screen.
    Skip,
}

/// Decides what a clock tick redraws. Without seconds, ticks are the minute
/// and redraw everything. With seconds, only the minute boundary (or a bar
/// that went `since_full` without one) does; the other ticks only change the
/// label, and nothing while the bar is not shown.
fn clock_tick_plan(seconds: bool, second: u16, shown: bool, since_full: Duration) -> ClockTick {
    if !seconds || second == 0 || since_full >= CLOCK_FULL_EVERY {
        ClockTick::Full
    } else if shown {
        ClockTick::LabelOnly
    } else {
        ClockTick::Skip
    }
}

/// Whether a label of `new` width can replace one of `old` without moving
/// its neighbours (a pixel of drift is not worth a new layout).
fn label_fits_slot(old: f32, new: f32) -> bool {
    (new - old).abs() <= 1.0
}

/// What the bar was last drawn for.
#[derive(Clone, PartialEq)]
struct Look {
    accessories: [Option<AccessoryReading>; 2],
    language: Language,
    screen: RECT,
    scale: f32,
    light: bool,
    /// See `system::transparency_enabled`.
    glass: bool,
    app_name: String,
    clock: String,
    /// The input language indicator, when more than one is installed.
    input: Option<String>,
    battery: Option<Battery>,
    status: Status,
    widgets: MenuBarWidgets,
    usage: Snapshot,
    usage_enabled: bool,
    active_providers: ActiveProviders,
    panel: Option<Island>,
    usage_refreshing: bool,
    track: Option<Track>,
    control: ControlData,
}

/// What the Control Center and Sound popups show beyond the bar's own status.
/// Radios and brightness are read on worker threads; `None` is "not known yet"
/// (or, for brightness, "this display has no control").
#[derive(Clone, Default, PartialEq, Eq)]
struct ControlData {
    wifi: Option<bool>,
    bluetooth: Option<bool>,
    /// The connected Wi-Fi network.
    ssid: Option<String>,
    dnd: bool,
    brightness: Option<u8>,
    outputs: Vec<status::AudioOutput>,
    /// The tray icons the Hidden icons popup lists.
    tray: Vec<TrayEntry>,
    /// The tile under the pointer; its name shows under the grid.
    tray_hover: Option<usize>,
}

/// One tray icon as the popup shows it.
#[derive(Clone)]
struct TrayEntry {
    key: TrayKey,
    name: String,
    width: u32,
    height: u32,
    /// Premultiplied BGRA, top-down.
    bgra: Arc<Vec<u8>>,
}

impl PartialEq for TrayEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.name == other.name
            && self.width == other.width
            && self.height == other.height
            && (Arc::ptr_eq(&self.bgra, &other.bgra) || self.bgra == other.bgra)
    }
}

impl Eq for TrayEntry {}

impl TrayEntry {
    /// The label under the icon: the first line of the tooltip, else the
    /// name of the program that owns the icon.
    fn from_info(info: TrayIconInfo, names: &mut HashMap<u32, String>) -> Self {
        let tip = info.tip.lines().next().unwrap_or("").trim();
        let name = if tip.is_empty() {
            names
                .entry(info.pid)
                .or_insert_with(|| {
                    muelle_shell::tray::process_exe_name(info.pid).unwrap_or_default()
                })
                .clone()
        } else {
            tip.to_owned()
        };
        Self {
            key: info.key,
            name,
            width: info.width,
            height: info.height,
            bgra: info.bgra,
        }
    }
}

// Hidden icons popup layout, in logical pixels.
const TRAY_MAX_COLUMNS: usize = 4;
const TRAY_MIN_COLUMNS: usize = 4;
const TRAY_CELL: f32 = 36.0;
const TRAY_PAD: f32 = 6.0;
const TRAY_ICON_SIZE: f32 = 20.0;
/// Room under the grid for the hovered icon's name, kept even when nothing
/// is hovered so the popup never resizes.
const TRAY_TIP_HEIGHT: f32 = 24.0;
const TRAY_FOOTER_HEIGHT: f32 = 28.0;
/// The "No icons" line, in place of the grid.
const TRAY_EMPTY_HEIGHT: f32 = 24.0;
/// More icons than this would not fit a short screen.
const TRAY_MAX_ICONS: usize = 48;

fn tray_rows(icons: usize) -> usize {
    icons.min(TRAY_MAX_ICONS).div_ceil(TRAY_MAX_COLUMNS)
}

fn tray_columns(icons: usize) -> usize {
    icons.clamp(TRAY_MIN_COLUMNS, TRAY_MAX_COLUMNS)
}

fn tray_width(icons: usize) -> f32 {
    tray_columns(icons) as f32 * TRAY_CELL + 2.0 * TRAY_PAD
}

/// Top-left of cell `index` inside the popup.
fn tray_cell(index: usize) -> (f32, f32) {
    (
        TRAY_PAD + (index % TRAY_MAX_COLUMNS) as f32 * TRAY_CELL,
        TRAY_PAD + (index / TRAY_MAX_COLUMNS) as f32 * TRAY_CELL,
    )
}

/// Where the grid (or the "No icons" line) ends, which is where the name
/// strip starts.
fn tray_grid_bottom(icons: usize) -> f32 {
    if icons == 0 {
        TRAY_PAD + TRAY_EMPTY_HEIGHT
    } else {
        TRAY_PAD + tray_rows(icons) as f32 * TRAY_CELL
    }
}

fn tray_popup_height(icons: usize) -> f32 {
    let strip = if icons == 0 { 0.0 } else { TRAY_TIP_HEIGHT };
    tray_grid_bottom(icons) + strip + TRAY_FOOTER_HEIGHT
}

/// A reading a worker thread hands to the UI thread.
enum PanelUpdate {
    Radios {
        wifi: Option<bool>,
        bluetooth: Option<bool>,
        ssid: Option<String>,
    },
    Brightness(Option<u8>),
    Outputs(Vec<status::AudioOutput>),
}

// Control Center layout, in logical pixels.
const CC_WIDTH: f32 = 320.0;
const CC_PAD: f32 = 12.0;
const CC_GAP: f32 = 12.0;
const CC_MODULE_HEIGHT: f32 = 144.0;
const CC_SLIDER_MODULE_HEIGHT: f32 = 60.0;
const CC_SOUND_MODULE_HEIGHT: f32 = 90.0;
const CC_MARGIN: f32 = 8.0;
// Sound popup layout.
const SOUND_WIDTH: f32 = 280.0;
const SOUND_HEADER_HEIGHT: f32 = 106.0;
const SOUND_ROW_HEIGHT: f32 = 28.0;
const SOUND_FOOTER_HEIGHT: f32 = 48.0;

fn control_center_height(brightness: bool) -> f32 {
    let sliders = if brightness {
        CC_SLIDER_MODULE_HEIGHT + CC_GAP
    } else {
        0.0
    };
    CC_PAD + CC_MODULE_HEIGHT + CC_GAP + sliders + CC_SOUND_MODULE_HEIGHT + CC_PAD
}

/// More output devices than this would not fit a short screen.
const SOUND_MAX_OUTPUTS: usize = 8;

fn sound_popup_height(outputs: usize) -> f32 {
    SOUND_HEADER_HEIGHT
        + SOUND_ROW_HEIGHT * outputs.min(SOUND_MAX_OUTPUTS) as f32
        + SOUND_FOOTER_HEIGHT
}

/// The system accent color, or the macOS blue.
fn accent_color() -> Color {
    system::accent_color().map_or(rgba(10, 132, 255, 255), |[r, g, b]| rgba(r, g, b, 255))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccessoryReading {
    kind: AccessoryKind,
    percent: Option<u8>,
}

// Compare only visible readings: unrelated names/devices do not redraw the bar.
fn accessory_readings(snapshot: &AccessorySnapshot) -> [Option<AccessoryReading>; 2] {
    snapshot.compact().map(|device| {
        device.map(|device| AccessoryReading {
            kind: device.kind,
            percent: device.percent,
        })
    })
}

impl Look {
    fn panel_content_height(&self) -> f32 {
        match self.panel {
            Some(Island::Music) => 184.0,
            Some(Island::ControlCenter) => control_center_height(self.control.brightness.is_some()),
            Some(Island::Sound) => sound_popup_height(self.control.outputs.len()),
            Some(Island::Tray) => tray_popup_height(self.control.tray.len()),
            Some(Island::Usage) => {
                74.0 + Provider::ALL
                    .into_iter()
                    .filter(|provider| self.active_providers.has(*provider))
                    .map(|provider| usage_row_height(self.usage.get(provider)))
                    .sum::<f32>()
            }
            None => 0.0,
        }
    }

    fn panel_height(&self) -> f32 {
        let natural = self.panel_content_height();
        if self.panel == Some(Island::Usage) {
            let available = (self.screen.bottom - self.screen.top) as f32 / self.scale
                - HEIGHT
                - PANEL_GAP
                - PANEL_SHADOW_MARGIN;
            natural.min(available.max(0.0))
        } else {
            natural
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Island {
    Music,
    Usage,
    /// Opened from its bar zone, not from an island trigger.
    ControlCenter,
    Sound,
    /// The hidden tray icons, from the arrow's zone.
    Tray,
}

impl Island {
    /// The bar item a zone popup hangs from.
    fn zone_action(self) -> Option<Action> {
        match self {
            Self::ControlCenter => Some(Action::ControlCenter),
            Self::Sound => Some(Action::Sound),
            Self::Tray => Some(Action::HiddenIcons),
            Self::Music | Self::Usage => None,
        }
    }

    /// Whether the popup holds sliders.
    fn has_sliders(self) -> bool {
        !matches!(self, Self::Usage | Self::Tray)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slider {
    Volume,
    Brightness,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Span {
    left: f32,
    right: f32,
}

impl Span {
    fn width(self) -> f32 {
        self.right - self.left
    }
}

#[derive(Clone, Copy)]
enum IslandAction {
    Toggle(Island),
    Media(MediaControl),
    Slider(Slider, Span),
    Mute,
    Refresh,
    ScrollUsage(i8),
    ToggleWifi,
    ToggleBluetooth,
    ToggleFocus,
    OpenNetwork,
    OpenBluetooth,
    QuickSettings,
    SetOutput(usize),
    SoundSettings,
    /// A tile of the hidden-icons popup, by index.
    TrayIcon(usize),
    TraySettings,
}

/// Work for the app once the bar's click handler has returned: it needs the
/// app itself or a menu that must open outside the app borrow.
#[derive(Clone, Debug, PartialEq)]
pub enum Deferred {
    ToggleDnd,
    OpenNetwork(POINT),
    /// Click a tray icon's app, at the icon's screen centre.
    TrayClick(TrayKey, TrayButton, POINT),
}

#[derive(Clone, Copy)]
struct IslandHit {
    bounds: (f32, f32, f32, f32),
    action: IslandAction,
}

/// A retained, clipped body. Wheel/arrow input only changes its transform and
/// arrow opacity; no quota queries or redraws.
struct UsageViewport {
    content: ContainerVisual,
    maximum: f32,
    scale: f32,
    bounds: (f32, f32, f32, f32),
    up: SpriteVisual,
    down: SpriteVisual,
}

impl IslandHit {
    fn at(&self, x: f32, y: f32) -> bool {
        let (left, top, right, bottom) = self.bounds;
        x >= left && x < right && y >= top && y < bottom
    }
}

struct ExitAnimation {
    batch: CompositionScopedBatch,
    completed: i64,
}

/// Only these retained visuals move during a slider gesture. Album artwork,
/// track labels and the rest of the menu bar keep their existing visual tree.
struct SliderControls {
    fill: CompositionRoundedRectangleGeometry,
    knob: ShapeVisual,
    /// The percentage label, when the slider has one.
    value: Option<SpriteVisual>,
    geometry: IslandGeometry,
    /// Logical y of the track's centre line.
    y: f32,
    left: f32,
    width: f32,
    position_percent: Option<u8>,
    label_percent: u8,
}

impl SliderControls {
    fn move_to(&mut self, percent: u8) -> Result<()> {
        if self.position_percent == Some(percent) {
            return Ok(());
        }
        let px = |v| self.geometry.px(v);
        let filled = self.width * f32::from(percent) / 100.0;
        self.fill.SetSize(Vector2::new(filled, px(4.0)))?;
        self.fill
            .SetCornerRadius(Vector2::new(px(2.0).min(filled / 2.0), px(2.0)))?;
        self.knob.SetOffset(Vector3::new(
            self.left + filled - px(5.0),
            px(self.y) - px(5.0),
            0.0,
        ))?;
        self.position_percent = Some(percent);
        Ok(())
    }

    fn paint(&mut self, gfx: &Gfx, percent: u8) -> Result<()> {
        self.move_to(percent)?;
        if self.label_percent != percent {
            if let Some(value) = &self.value {
                let label = gfx.text_brush_sized(
                    &format!("{percent}%"),
                    self.geometry.scale,
                    10.5,
                    false,
                    self.geometry.ink(),
                )?;
                value.SetBrush(&label.brush.cast::<CompositionBrush>()?)?;
                value.SetSize(Vector2::new(label.width, label.height))?;
            }
            self.label_percent = percent;
        }
        Ok(())
    }
}

impl Drop for ExitAnimation {
    fn drop(&mut self) {
        let _ = self.batch.RemoveCompleted(self.completed);
    }
}

pub struct MenuBar {
    accessories: Option<accessories::Monitor>,
    accessories_enabled: bool,
    accessory_retry_at: Instant,
    hwnd: HWND,
    _target: DesktopWindowTarget,
    /// Stable parent: content refreshes must not restart reveal animations.
    surface: ContainerVisual,
    appbar: Option<AppBar>,
    app_name: String,
    look: Option<Look>,
    /// Clickable spans, in window pixels.
    zones: Vec<(f32, f32, Action)>,
    visible: bool,
    auto_hide: bool,
    /// With auto-hide on: the pointer is at the top edge, so the bar shows.
    revealed: bool,
    hide_until: Option<Instant>,
    animation_epoch: usize,
    exit_animation: Option<ExitAnimation>,
    /// Where the full bar goes on screen.
    bounds: RECT,
    status: Status,
    widgets: MenuBarWidgets,
    usage: Option<UsageMonitor>,
    /// Where each provider sits in the closed island, as screen x.
    usage_anchors: Vec<(Provider, i32)>,
    /// A moment for the crab, waiting for the dock (see `take_cue`).
    cue: Option<(mascot::Act, i32)>,
    media: Option<media::Watch>,
    /// "Show what is playing": off, nothing listens to media sessions.
    media_enabled: bool,
    cover: Option<(
        Option<Arc<muelle_shell::icons::IconImage>>,
        CompositionSurfaceBrush,
    )>,
    slider_drag: Option<(Slider, Span)>,
    pending_volume: Option<u8>,
    volume_target: Option<u8>,
    volume_controls: Option<SliderControls>,
    pending_brightness: Option<u8>,
    brightness_target: Option<u8>,
    brightness_controls: Option<SliderControls>,
    /// Readings for the Control Center and Sound popups.
    control: ControlData,
    deferred: Option<Deferred>,
    /// The tray relay, when the hidden-icons popup is on.
    tray: Option<Rc<TrayHost>>,
    /// Icon images as composition brushes, by icon; see `tray_brush`.
    tray_brushes: HashMap<TrayKey, (Arc<Vec<u8>>, CompositionSurfaceBrush)>,
    /// Program names by process id, for icons without a tooltip.
    tray_names: HashMap<u32, String>,
    /// A tray change is waiting for `TRAY_TIMER`.
    tray_flush_armed: bool,
    /// The clock label as drawn, for redrawing it alone.
    clock_slot: Option<ClockSlot>,
    /// The last redraw of the whole bar from a clock tick.
    last_full_tick: Instant,
    /// The bar zone the open Control Center or Sound popup hangs from.
    zone_span: Span,
    usage_scroll: f32,
    usage_viewport: Option<UsageViewport>,
    suppress_volume_release: bool,
    panel: Option<Island>,
    visual_height: i32,
    triggers_span: Span,
    panel_span: Span,
    island_hits: Vec<IslandHit>,
    outside_click_hook: Option<HHOOK>,
    /// Held for their callbacks; dropping them unsubscribes.
    _network: Option<NetworkWatch>,
    /// Hears when apps start or stop using the microphone or camera.
    privacy_watch: Option<RegistryWatch>,
    /// Windows' taskbar preferences, including the battery percentage.
    settings_watch: Option<RegistryWatch>,
    /// Energy saver on or off arrives as `WM_POWERBROADCAST`; glass follows.
    _saver_watch: muelle_shell::power::SaverWatch,
    audio: Option<AudioWatch>,
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            launcher::note_menu_bar_press();
            app::menu_bar_volume_press((l.0 & 0xFFFF) as i16 as f32, (l.0 >> 16) as i16 as f32);
        }
        WM_LBUTTONUP => {
            let x = (l.0 & 0xFFFF) as i16;
            let y = (l.0 >> 16) as i16;
            if !app::menu_bar_volume_release(Some(f32::from(x))) {
                app::menu_bar_clicked(f32::from(x), f32::from(y));
            }
        }
        WM_MOUSEMOVE => {
            let x = (l.0 & 0xFFFF) as i16;
            let y = (l.0 >> 16) as i16;
            app::menu_bar_pointer_moved(f32::from(x), f32::from(y));
        }
        WM_MOUSELEAVE => app::menu_bar_check_pointer(),
        WM_MOUSEWHEEL => {
            let delta = (w.0 >> 16) as u16 as i16;
            let point = POINT {
                x: (l.0 & 0xFFFF) as i16 as i32,
                y: (l.0 >> 16) as i16 as i32,
            };
            if app::menu_bar_scrolled(point, delta) {
                return LRESULT(0);
            }
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
        WM_TIMER if w.0 == POINTER_TIMER => app::menu_bar_check_pointer(),
        WM_TIMER if w.0 == VOLUME_TIMER => app::menu_bar_volume_flush(),
        WM_TIMER if w.0 == CLOCK_TIMER => app::menu_bar_clock_tick(),
        WM_TIMER if w.0 == TRAY_TIMER => app::menu_bar_tray_flush(),
        WM_CAPTURECHANGED => {
            // Avoid reentering the app borrow inside Set/ReleaseCapture.
            unsafe {
                let _ = PostMessageW(Some(hwnd), VOLUME_CAPTURE_LOST, WPARAM(0), LPARAM(0));
            }
        }
        VOLUME_CAPTURE_LOST => {
            // A delayed notification must not cancel a newer drag.
            if unsafe { GetCapture() } != hwnd {
                app::menu_bar_volume_release(None);
            }
        }
        WM_TIMER if w.0 == HIDE_ANIMATION_TIMER => app::menu_bar_finish_hide(None),
        HIDE_ANIMATION_DONE => app::menu_bar_finish_hide(Some(w.0)),
        DISMISS_ISLAND => app::menu_bar_dismiss_island(),
        REGISTRY_CHANGED => app::menu_bar_registry_changed(),
        ACCESSORIES_CHANGED => app::menu_bar_accessories_changed(),
        MEDIA_CHANGED => app::menu_bar_media_changed(),
        PANEL_DATA_CHANGED => app::menu_bar_panel_data_changed(),
        TRAY_CHANGED => app::menu_bar_tray_changed(),
        WM_RBUTTONUP => {
            let x = (l.0 & 0xFFFF) as i16;
            let y = (l.0 >> 16) as i16;
            app::menu_bar_right_clicked(f32::from(x), f32::from(y));
        }
        AUDIO_CHANGED => {
            if !AUDIO_TICK_PENDING.swap(true, Ordering::Relaxed) {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), AUDIO_TICK, WPARAM(0), LPARAM(0));
                }
            }
        }
        AUDIO_TICK => {
            AUDIO_TICK_PENDING.store(false, Ordering::Relaxed);
            app::menu_bar_audio_changed();
        }
        WM_SETCURSOR => {
            // SAFETY: Keep the standard arrow throughout the bar and popups.
            if let Ok(arrow) = unsafe { LoadCursorW(None, IDC_ARROW) } {
                unsafe { SetCursor(Some(arrow)) };
                return LRESULT(1);
            }
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
        WM_NCHITTEST => {
            let x = (l.0 & 0xFFFF) as i16 as i32;
            let y = (l.0 >> 16) as i16 as i32;
            if y >= BAR_BOTTOM.load(Ordering::Relaxed)
                && (y >= PANEL_BOTTOM.load(Ordering::Relaxed)
                    || x < PANEL_LEFT.load(Ordering::Relaxed)
                    || x >= PANEL_RIGHT.load(Ordering::Relaxed))
            {
                return LRESULT(-1);
            }
            // SAFETY: Default hit testing for the bar and the island.
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
        // The minute changed, the charger was plugged or unplugged, the
        // network or volume changed, or a Windows setting (taskbar buttons).
        // Nothing the bar shows depends on the work area.
        WM_SETTINGCHANGE if w.0 == SPI_SETWORKAREA => {}
        // Some of these are sent while the dock is busy reserving space, and
        // updating then was dropped. From the message loop the dock is never
        // busy, and a burst of them makes a single update.
        // WM_TIMECHANGE: the clock or time zone was changed.
        WM_TIMER | WM_POWERBROADCAST | WM_TIMECHANGE | STATUS_CHANGED | USAGE_CHANGED
        | WM_SETTINGCHANGE => {
            if msg == WM_POWERBROADCAST && matches!(w.0, 7 | 18) {
                app::menu_bar_accessories_reset();
            }
            if msg == WM_POWERBROADCAST && w.0 == PBT_POWERSETTINGCHANGE as usize {
                // SAFETY: Windows owns the setting payload for this message.
                unsafe {
                    muelle_shell::power::power_setting_changed(l);
                }
            }
            if !TICK_PENDING.swap(true, Ordering::Relaxed) {
                // SAFETY: Posting to our own live window.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), TICK, WPARAM(0), LPARAM(0));
                }
            }
        }
        TICK => {
            TICK_PENDING.store(false, Ordering::Relaxed);
            app::menu_bar_tick();
        }
        // SAFETY: Standard forwarding of unhandled messages.
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    LRESULT(0)
}

unsafe extern "system" fn outside_click_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            w.0 as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        // SAFETY: Windows provides an MSLLHOOKSTRUCT for mouse hook actions.
        let point = unsafe { (*(l.0 as *const MSLLHOOKSTRUCT)).pt };
        let in_triggers = point.y >= BAR_TOP.load(Ordering::Relaxed)
            && point.y < BAR_BOTTOM.load(Ordering::Relaxed)
            && ((point.x >= TRIGGERS_LEFT.load(Ordering::Relaxed)
                && point.x < TRIGGERS_RIGHT.load(Ordering::Relaxed))
                || (point.x >= ZONE_LEFT.load(Ordering::Relaxed)
                    && point.x < ZONE_RIGHT.load(Ordering::Relaxed)));
        let in_panel = point.y >= BAR_BOTTOM.load(Ordering::Relaxed)
            && point.y < PANEL_BOTTOM.load(Ordering::Relaxed)
            && point.x >= PANEL_LEFT.load(Ordering::Relaxed)
            && point.x < PANEL_RIGHT.load(Ordering::Relaxed);
        if !in_triggers && !in_panel {
            let window = ISLAND_HOOK_WINDOW.load(Ordering::Relaxed);
            if window != 0 {
                // SAFETY: The window remains live until the hook is removed in Drop.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(window as *mut _)),
                        DISMISS_ISLAND,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        }
    }
    // SAFETY: Never consume the click; the target application must receive it.
    unsafe { CallNextHookEx(None, code, w, l) }
}

pub(crate) fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color {
        A: a,
        R: r,
        G: g,
        B: b,
    }
}

pub(crate) fn add_visual(
    compositor: &Compositor,
    layers: &VisualCollection,
    brush: &CompositionBrush,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Result<SpriteVisual> {
    let visual = compositor.CreateSpriteVisual()?;
    visual.SetSize(Vector2 {
        X: width,
        Y: height,
    })?;
    visual.SetOffset(Vector3 {
        X: x.round(),
        Y: y.round(),
        Z: 0.0,
    })?;
    visual.SetBrush(brush)?;
    layers.InsertAtTop(&visual)?;
    Ok(visual)
}

#[expect(
    clippy::too_many_arguments,
    reason = "A rounded shape needs its rectangle, radius and two colors"
)]
pub(crate) fn add_rounded_box(
    compositor: &Compositor,
    layers: &VisualCollection,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    fill: Color,
    border: Option<Color>,
) -> Result<()> {
    retained_rounded_box(
        compositor, layers, x, y, width, height, radius, fill, border,
    )
    .map(|_| ())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Same rectangle/color inputs as add_rounded_box; retain geometry for small UI updates"
)]
fn retained_rounded_box(
    compositor: &Compositor,
    layers: &VisualCollection,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    fill: Color,
    border: Option<Color>,
) -> Result<(ShapeVisual, CompositionRoundedRectangleGeometry)> {
    let geometry = compositor.CreateRoundedRectangleGeometry()?;
    let inset = if border.is_some() { 0.5 } else { 0.0 };
    geometry.SetOffset(Vector2 { X: inset, Y: inset })?;
    geometry.SetSize(Vector2 {
        X: (width - inset * 2.0).max(0.0),
        Y: (height - inset * 2.0).max(0.0),
    })?;
    geometry.SetCornerRadius(Vector2 {
        X: radius.min(width / 2.0),
        Y: radius.min(height / 2.0),
    })?;
    let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
    shape.SetFillBrush(&compositor.CreateColorBrushWithColor(fill)?)?;
    if let Some(border) = border {
        shape.SetStrokeBrush(&compositor.CreateColorBrushWithColor(border)?)?;
        shape.SetStrokeThickness(1.0)?;
    }
    let visual = compositor.CreateShapeVisual()?;
    visual.SetSize(Vector2 {
        X: width,
        Y: height,
    })?;
    visual.SetOffset(Vector3 {
        X: x.round(),
        Y: y.round(),
        Z: 0.0,
    })?;
    visual.Shapes()?.Append(&shape)?;
    layers.InsertAtTop(&visual)?;
    Ok((visual, geometry))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Typography and placement are chosen together for panel labels"
)]
fn add_panel_text(
    gfx: &Gfx,
    layers: &VisualCollection,
    value: &str,
    x: f32,
    y: f32,
    scale: f32,
    font_size: f32,
    bold: bool,
    ink: Ink,
    opacity: f32,
) -> Result<LabelBrush> {
    let label = gfx.text_brush_sized(value, scale, font_size, bold, ink)?;
    add_visual(
        &gfx.compositor,
        layers,
        &label.brush.cast()?,
        x,
        y,
        label.width,
        label.height,
    )?
    .SetOpacity(opacity)?;
    Ok(label)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Right-aligned panel labels choose typography and placement together"
)]
fn add_panel_text_right(
    gfx: &Gfx,
    layers: &VisualCollection,
    value: &str,
    right: f32,
    y: f32,
    scale: f32,
    font_size: f32,
    bold: bool,
    ink: Ink,
    opacity: f32,
) -> Result<LabelBrush> {
    let label = gfx.text_brush_sized(value, scale, font_size, bold, ink)?;
    add_visual(
        &gfx.compositor,
        layers,
        &label.brush.cast()?,
        right - label.width,
        y,
        label.width,
        label.height,
    )?
    .SetOpacity(opacity)?;
    Ok(label)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Each provider row draws its own colors, data and measured rectangle"
)]
fn draw_usage_row(
    gfx: &Gfx,
    layers: &VisualCollection,
    provider: Provider,
    usage: &ProviderUsage,
    accent: Color,
    scale: f32,
    light: bool,
    island_width: f32,
    y: f32,
) -> Result<()> {
    let c = &gfx.compositor;
    let px = |logical: f32| (logical * scale).round();
    let ink = if light { Ink::Dark } else { Ink::Light };
    let text = strings::text();
    let left = px(23.0);
    let width = island_width - px(46.0);
    let right = left + width;
    add_rounded_box(
        c,
        layers,
        left + px(12.0),
        y + px(11.0),
        px(32.0),
        px(32.0),
        px(10.0),
        rgba(accent.R, accent.G, accent.B, if light { 31 } else { 41 }),
        None,
    )?;
    if let Some(image) = usage::logo(provider) {
        let icon_size = px(20.0) as u32;
        let image_brush = gfx.icon_brush(image, icon_size)?;
        let brush: CompositionBrush =
            if !matches!(provider, Provider::Claude | Provider::Gemini) && light {
                let tinted = c.CreateMaskBrush()?;
                tinted.SetMask(&image_brush)?;
                tinted.SetSource(&c.CreateColorBrushWithColor(rgba(27, 34, 45, 255))?)?;
                tinted.cast()?
            } else {
                image_brush.cast()?
            };
        add_visual(
            c,
            layers,
            &brush,
            left + px(18.0),
            y + px(17.0),
            icon_size as f32,
            icon_size as f32,
        )?;
    }
    add_panel_text(
        gfx,
        layers,
        provider.name(),
        left + px(53.0),
        y + px(10.0),
        scale,
        14.0,
        true,
        ink,
        1.0,
    )?;
    if let Some(main) = usage.main_window() {
        add_panel_text(
            gfx,
            layers,
            &main.title(),
            left + px(53.0),
            y + px(30.0),
            scale,
            11.5,
            false,
            ink,
            0.72,
        )?;
        add_panel_text_right(
            gfx,
            layers,
            &format!("{}%", main.used_percent),
            right - px(14.0),
            y + px(6.0),
            scale,
            20.0,
            true,
            ink,
            1.0,
        )?;
        add_panel_text_right(
            gfx,
            layers,
            text.usage_used,
            right - px(15.0),
            y + px(31.0),
            scale,
            11.0,
            false,
            ink,
            0.7,
        )?;
        let track_left = left + px(14.0);
        let track_width = width - px(28.0);
        let track_y = y + px(53.0);
        add_rounded_box(
            c,
            layers,
            track_left,
            track_y,
            track_width,
            px(6.0),
            px(3.0),
            if light {
                rgba(54, 73, 95, 43)
            } else {
                rgba(224, 235, 248, 48)
            },
            None,
        )?;
        let fill = track_width * f32::from(main.used_percent) / 100.0;
        if fill > 0.0 {
            add_rounded_box(
                c,
                layers,
                track_left,
                track_y,
                fill,
                px(6.0),
                px(3.0),
                accent,
                None,
            )?;
        }
        if let Some(reset) = main.reset_text() {
            add_panel_text_right(
                gfx,
                layers,
                &format!("{} {reset}", text.usage_resets),
                right - px(14.0),
                y + px(64.0),
                scale,
                11.0,
                false,
                ink,
                0.78,
            )?;
        }
        if usage.state == State::Unavailable
            || usage.age_minutes().is_some_and(|minutes| minutes > 5)
        {
            add_panel_text(
                gfx,
                layers,
                text.usage_stale,
                left + px(14.0),
                y + px(64.0),
                scale,
                11.0,
                false,
                ink,
                0.7,
            )?;
        }
        for (index, secondary) in usage.windows.iter().skip(1).enumerate() {
            add_visual(
                c,
                layers,
                &c.CreateColorBrushWithColor(if light {
                    rgba(50, 68, 88, 24)
                } else {
                    rgba(228, 236, 247, 29)
                })?
                .cast()?,
                left + px(14.0),
                y + px(83.0 + index as f32 * 26.0),
                width - px(28.0),
                px(1.0),
            )?;
            add_panel_text(
                gfx,
                layers,
                &format!("{} · {}%", secondary.title(), secondary.used_percent),
                left + px(14.0),
                y + px(87.0 + index as f32 * 26.0),
                scale,
                11.5,
                true,
                ink,
                0.94,
            )?;
            if let Some(reset) = secondary.reset_text() {
                add_panel_text_right(
                    gfx,
                    layers,
                    &format!("{} {reset}", text.usage_resets),
                    right - px(14.0),
                    y + px(87.0 + index as f32 * 26.0),
                    scale,
                    11.5,
                    false,
                    ink,
                    0.76,
                )?;
            }
        }
    } else {
        let message = match usage.state {
            State::Loading => text.usage_loading,
            State::SignIn => text.usage_sign_in,
            State::Ready | State::Unavailable => text.usage_unavailable,
        };
        add_panel_text(
            gfx,
            layers,
            message,
            left + px(53.0),
            y + px(43.0),
            scale,
            12.0,
            false,
            ink,
            0.72,
        )?;
    }
    Ok(())
}

fn usage_row_height(usage: &ProviderUsage) -> f32 {
    ISLAND_ROW_HEIGHT + usage.windows.len().saturating_sub(2) as f32 * 26.0
}

impl MenuBar {
    pub fn create(
        gfx: &Gfx,
        usage_enabled: bool,
        media_enabled: bool,
        auto_hide: bool,
        widgets: MenuBarWidgets,
    ) -> Result<Self> {
        let class = w!("MuelleMenuBar");
        // SAFETY: Standard class registration and window creation on the UI
        // thread; the window is destroyed in `Drop`.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            // Registering an existing class fails harmlessly.
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(bar_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            });
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Muelle"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            // Without this, the system backdrop brush appears black.
            let enable = BOOL::from(true);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                (&raw const enable).cast(),
                size_of::<BOOL>() as u32,
            );
            hwnd
        };
        let target = unsafe {
            // SAFETY: `hwnd` is the live window created above.
            gfx.compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)?
        };
        // Our compositor owns the transition; do not combine it with DWM's
        // native show/hide effect, which can present a cached opaque frame.
        system::set_window_transitions(hwnd.0 as isize, false);
        let surface = gfx.compositor.CreateContainerVisual()?;
        target.SetRoot(&surface)?;
        let mut bar = Self {
            accessories: None,
            accessories_enabled: false,
            accessory_retry_at: Instant::now() + Duration::from_secs(60),
            hwnd,
            _target: target,
            surface,
            appbar: AppBar::register(hwnd, APPBAR_CALLBACK),
            app_name: String::new(),
            look: None,
            zones: Vec::new(),
            visible: true,
            auto_hide,
            revealed: false,
            hide_until: None,
            animation_epoch: 0,
            exit_animation: None,
            bounds: RECT::default(),
            status: Status {
                network: Network::Offline,
                volume: None,
                buttons: TaskbarButtons::default(),
                privacy: Privacy::default(),
                battery_percentage: false,
            },
            widgets,
            usage: usage_enabled.then(|| UsageMonitor::new(hwnd.0 as isize, USAGE_CHANGED)),
            usage_anchors: Vec::new(),
            cue: None,
            media: media_enabled
                .then(|| media::Watch::start(hwnd, MEDIA_CHANGED))
                .flatten(),
            media_enabled,
            cover: None,
            slider_drag: None,
            pending_volume: None,
            volume_target: None,
            volume_controls: None,
            pending_brightness: None,
            brightness_target: None,
            brightness_controls: None,
            control: ControlData::default(),
            deferred: None,
            tray: None,
            tray_brushes: HashMap::new(),
            tray_names: HashMap::new(),
            tray_flush_armed: false,
            clock_slot: None,
            last_full_tick: Instant::now(),
            zone_span: Span::default(),
            usage_scroll: 0.0,
            usage_viewport: None,
            suppress_volume_release: false,
            panel: None,
            visual_height: 0,
            triggers_span: Span::default(),
            panel_span: Span::default(),
            island_hits: Vec::new(),
            outside_click_hook: None,
            _network: NetworkWatch::new(hwnd.0 as isize, STATUS_CHANGED),
            privacy_watch: status::watch_privacy(hwnd.0 as isize, REGISTRY_CHANGED),
            settings_watch: status::watch_taskbar_settings(hwnd.0 as isize, REGISTRY_CHANGED),
            _saver_watch: muelle_shell::power::SaverWatch::new(hwnd.0 as isize),
            audio: AudioWatch::new(hwnd.0 as isize, AUDIO_CHANGED),
        };
        AUDIO_TICK_PENDING.store(false, Ordering::Relaxed);
        bar.update_status();
        bar.refresh(gfx)?;
        bar.schedule_next_minute();
        Ok(bar)
    }

    pub fn set_app_name(&mut self, gfx: &Gfx, name: &str) -> Result<()> {
        if self.app_name == name {
            return Ok(());
        }
        name.clone_into(&mut self.app_name);
        self.refresh(gfx)
    }

    /// Hidden while an app is full screen.
    /// How tall the bar is at `scale`: the strip it keeps at the top of the
    /// primary display unless it hides itself. A side dock starts below it.
    pub fn strip_height(scale: f32) -> i32 {
        (HEIGHT * scale).round() as i32
    }

    pub fn set_visible(&mut self, visible: bool) -> Result<()> {
        if !visible {
            self.release_volume(None)?;
        }
        if self.visible == visible {
            return Ok(());
        }
        self.visible = visible;
        if !visible && self.auto_hide {
            self.revealed = false;
        }
        self.place()
    }

    pub fn set_auto_hide(&mut self, auto_hide: bool) -> Result<()> {
        if self.auto_hide == auto_hide {
            return Ok(());
        }
        self.auto_hide = auto_hide;
        self.revealed = false;
        self.hide_until = None;
        self.place()
    }

    pub fn set_usage_enabled(&mut self, gfx: &Gfx, enabled: bool) -> Result<()> {
        if self.usage.is_some() == enabled {
            return Ok(());
        }
        self.usage = enabled.then(|| UsageMonitor::new(self.hwnd.0 as isize, USAGE_CHANGED));
        self.refresh(gfx)
    }

    /// Shows or hides what is playing. Off, the media watch stops too, so
    /// nothing follows playback changes.
    pub fn set_media_enabled(&mut self, gfx: &Gfx, enabled: bool) -> Result<()> {
        if self.media_enabled == enabled {
            return Ok(());
        }
        self.media_enabled = enabled;
        self.media = enabled
            .then(|| media::Watch::start(self.hwnd, MEDIA_CHANGED))
            .flatten();
        self.refresh(gfx)
    }

    pub fn set_accessories_enabled(&mut self, gfx: &Gfx, enabled: bool) -> Result<()> {
        if self.accessories_enabled == enabled {
            return Ok(());
        }
        self.accessories_enabled = enabled;
        if enabled {
            self.restart_accessories(gfx)
        } else {
            self.accessories = None;
            self.refresh(gfx)
        }
    }

    pub fn accessories_changed(&mut self, gfx: &Gfx) -> Result<()> {
        if self.accessories_enabled && self.visible && (!self.auto_hide || self.revealed) {
            self.refresh(gfx)?;
        }
        Ok(())
    }

    pub fn restart_accessories(&mut self, gfx: &Gfx) -> Result<()> {
        // Resume/audio notifications and the minute tick must never start a
        // disabled reader, including after a previously enabled one failed.
        if !self.accessories_enabled {
            return Ok(());
        }
        self.accessory_retry_at = Instant::now() + Duration::from_secs(60);
        self.accessories = None;
        self.accessories = accessories::Monitor::new(
            self.hwnd.0 as isize,
            ACCESSORIES_CHANGED,
            self.visible && (!self.auto_hide || self.revealed),
        )
        .inspect_err(|error| eprintln!("muelle: accesorios: {error:#}"))
        .ok();
        self.refresh(gfx)
    }

    pub fn set_widgets(&mut self, gfx: &Gfx, widgets: MenuBarWidgets) -> Result<()> {
        if self.widgets == widgets {
            return Ok(());
        }
        self.widgets = widgets;
        self.schedule_next_minute();
        self.refresh(gfx)
    }

    /// The pointer reached the top edge or left the bar.
    pub fn hover(&mut self, gfx: &Gfx, inside: bool) -> Result<()> {
        if self.slider_drag.is_some() && !inside {
            return Ok(());
        }
        if !inside && self.panel.take().is_some() {
            self.refresh(gfx)?;
        }
        if !self.auto_hide || self.revealed == inside {
            return Ok(());
        }
        let was_hiding = self.hide_until.is_some();
        let animated = self.visible && system::animations_enabled();
        self.animation_epoch = self.animation_epoch.wrapping_add(1);
        self.exit_animation = None;
        self.revealed = inside;
        self.hide_until = (!inside && animated)
            .then(|| Instant::now() + Duration::from_millis(u64::from(HIDE_FALLBACK_MS)));
        if !animated {
            self.reset_surface()?;
        } else if inside && !was_hiding {
            self.surface
                .SetOffset(Vector3::new(0.0, -self.reveal_lift(), 0.0))?;
            self.surface.SetOpacity(0.0)?;
        }
        self.place()?;
        if animated && let Err(error) = self.animate_visibility(gfx, inside) {
            muelle_shell::log::line(&format!("animación de barra superior: {error:#}"));
            self.hide_until = None;
            self.reset_surface()?;
            self.place()?;
        }
        if inside {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: `track` is a valid structure with its size initialized.
            unsafe { TrackMouseEvent(&mut track)? };
        }
        self.refresh(gfx)
    }

    fn reveal_lift(&self) -> f32 {
        (self.bounds.bottom - self.bounds.top) as f32 * 0.3
    }

    fn animate_visibility(&mut self, gfx: &Gfx, shown: bool) -> Result<()> {
        let c = &gfx.compositor;
        let ease = if shown {
            c.CreateCubicBezierEasingFunction(Vector2::new(0.2, 0.8), Vector2::new(0.2, 1.0))?
        } else {
            c.CreateCubicBezierEasingFunction(Vector2::new(0.4, 0.0), Vector2::new(1.0, 1.0))?
        };
        let duration = TimeSpan {
            Duration: i64::from(if shown { REVEAL_MS } else { HIDE_MS }) * 10_000,
        };
        let slide = c.CreateScalarKeyFrameAnimation()?;
        slide.SetDuration(duration)?;
        slide.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        slide.InsertKeyFrameWithEasingFunction(
            1.0,
            if shown { 0.0 } else { -self.reveal_lift() },
            &ease,
        )?;
        let fade = c.CreateScalarKeyFrameAnimation()?;
        fade.SetDuration(duration)?;
        fade.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        fade.InsertKeyFrameWithEasingFunction(1.0, if shown { 1.0 } else { 0.0 }, &ease)?;
        let batch = (!shown)
            .then(|| c.CreateScopedBatch(CompositionBatchTypes::Animation))
            .transpose()?;
        let started = (|| -> Result<()> {
            self.surface.StartAnimation(h!("Offset.Y"), &slide)?;
            self.surface.StartAnimation(h!("Opacity"), &fade)?;
            Ok(())
        })();
        if let Some(batch) = batch {
            let window = self.hwnd.0 as isize;
            let epoch = self.animation_epoch;
            let completed = batch.Completed(&TypedEventHandler::new(move |_, _| {
                // SAFETY: Only scalar state is posted back to the UI thread.
                // A cancelled transition's already queued reply is ignored.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(window as *mut _)),
                        HIDE_ANIMATION_DONE,
                        WPARAM(epoch),
                        LPARAM(0),
                    );
                }
                Ok(())
            }));
            // Register before ending the scope. End it even on failure so
            // unrelated later animations cannot become part of this exit.
            let ended = batch.End();
            started?;
            ended?;
            let completed = completed?;
            self.exit_animation = Some(ExitAnimation { batch, completed });
        } else {
            started?;
        }
        Ok(())
    }

    fn park_surface(&self) -> Result<()> {
        self.surface.StopAnimation(h!("Offset.Y"))?;
        self.surface.StopAnimation(h!("Opacity"))?;
        self.surface
            .SetOffset(Vector3::new(0.0, -self.reveal_lift(), 0.0))?;
        self.surface.SetOpacity(0.0)?;
        Ok(())
    }

    fn reset_surface(&self) -> Result<()> {
        self.surface.StopAnimation(h!("Offset.Y"))?;
        self.surface.StopAnimation(h!("Opacity"))?;
        self.surface.SetOffset(Vector3::new(0.0, 0.0, 0.0))?;
        self.surface.SetOpacity(1.0)?;
        Ok(())
    }

    pub fn finish_hide(&mut self, gfx: &Gfx, completed_epoch: Option<usize>) -> Result<()> {
        let mut point = POINT::default();
        // SAFETY: Read-only cursor query before completing an exit transition.
        unsafe { GetCursorPos(&mut point)? };
        self.finish_hide_at(gfx, Instant::now(), point, completed_epoch)
    }

    fn finish_hide_at(
        &mut self,
        gfx: &Gfx,
        now: Instant,
        point: POINT,
        completed_epoch: Option<usize>,
    ) -> Result<()> {
        // A queued timer from a cancelled transition cannot hide a reopened bar.
        if completed_epoch.is_some_and(|epoch| epoch != self.animation_epoch) {
            return Ok(());
        }
        let Some(deadline) = self.hide_until else {
            return Ok(());
        };
        self.pointer_left_at(gfx, point)?;
        if self.hide_until.is_none() {
            return Ok(());
        }
        if completed_epoch.is_some() || now >= deadline {
            self.hide_until = None;
            self.exit_animation = None;
        }
        self.place()
    }

    /// A leave notification can already be queued when the hidden window is
    /// shown. Check the pointer rather than hiding a newly revealed bar on it.
    pub fn check_pointer(&mut self, gfx: &Gfx) -> Result<()> {
        if self.panel.is_some() {
            // SAFETY: Read-only Escape state; checked only while a popup is open.
            if unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x1B) } < 0 {
                return self.dismiss_island(gfx);
            }
        }
        let mut point = POINT::default();
        // SAFETY: Read-only cursor query in the same DPI context as `bounds`.
        unsafe { GetCursorPos(&mut point)? };
        self.pointer_left_at(gfx, point)
    }

    fn pointer_left_at(&mut self, gfx: &Gfx, point: POINT) -> Result<()> {
        let x = (point.x - self.bounds.left) as f32;
        if let Some((slider, span)) = self.slider_drag {
            self.queue_slider(slider, span, x);
            // A nonactivating background window may not receive mouse-up
            // outside its visible bounds. Reuse the existing popup pointer
            // check to finish the gesture without ever stealing app focus.
            let pressed = unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
                let primary = if GetSystemMetrics(SM_SWAPBUTTON) == 0 {
                    0x01
                } else {
                    0x02
                };
                windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(primary) < 0
            };
            if pressed {
                return self.paint_volume_preview(gfx);
            }
            self.release_volume(Some(x))?;
        }
        let within_bar = point.x >= self.bounds.left
            && point.x < self.bounds.right
            && point.y >= self.bounds.top
            && point.y < self.bounds.bottom;
        // Include the gap between the island and its popup so crossing it
        // does not close the popup before the pointer reaches its controls.
        let within_panel = self.panel.is_some()
            && x >= self.panel_span.left
            && x < self.panel_span.right
            && point.y >= self.bounds.bottom
            && point.y < PANEL_BOTTOM.load(Ordering::Relaxed);
        if (within_bar || within_panel) && self.hide_until.is_some() {
            self.hover(gfx, true)?;
        } else if !within_bar && !within_panel {
            self.hover(gfx, false)?;
        }
        Ok(())
    }

    /// Leaving the island sideways still stays inside the full-width bar window.
    pub fn pointer_moved(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        if let Some((slider, span)) = self.slider_drag {
            self.queue_slider(slider, span, x);
            return self.paint_volume_preview(gfx);
        }
        let span = if y < (self.bounds.bottom - self.bounds.top) as f32 {
            // A zone popup hangs from its small bar item. Use the popup's own
            // width on the bar row too, so a diagonal move from the item into
            // the popup does not close it on the way.
            if self.zone_span.width() > 0.0 {
                Span {
                    left: self.zone_span.left.min(self.panel_span.left),
                    right: self.zone_span.right.max(self.panel_span.right),
                }
            } else {
                self.triggers_span
            }
        } else {
            self.panel_span
        };
        if self.panel.is_some() && (x < span.left || x >= span.right) {
            self.dismiss_island(gfx)?;
        }
        self.track_tray_hover(gfx, x, y)?;
        self.hover(gfx, true)
    }

    pub fn dismiss_island(&mut self, gfx: &Gfx) -> Result<()> {
        self.release_volume(None)?;
        self.control.tray_hover = None;
        if self.panel.take().is_some() {
            self.refresh(gfx)?;
        }
        Ok(())
    }

    /// Follows the pointer over the hidden-icons tiles; a change redraws.
    fn track_tray_hover(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        let hover = if self.panel == Some(Island::Tray) {
            self.island_hits.iter().find_map(|hit| match hit.action {
                IslandAction::TrayIcon(index) if hit.at(x, y) => Some(index),
                _ => None,
            })
        } else {
            None
        };
        if self.control.tray_hover == hover {
            return Ok(());
        }
        self.control.tray_hover = hover;
        self.control_changed(gfx)
    }

    /// Positions the window and reserves (or frees) its strip of the screen.
    fn place(&mut self) -> Result<()> {
        let bounds = self.bounds;
        let bar_height = bounds.bottom - bounds.top;
        if bar_height <= 0 {
            return Ok(());
        }
        let hidden = self.auto_hide && !self.revealed;
        if !self.visible || !self.auto_hide {
            self.hide_until = None;
            self.exit_animation = None;
        }
        let on_screen = self.visible && (!hidden || self.hide_until.is_some());
        if let Some(monitor) = &self.accessories {
            monitor.set_visible(on_screen && !hidden);
        }
        if !on_screen {
            // Keep the hidden tree transparent. Restoring opacity here races
            // HWND hiding and flashes its last opaque frame on the next show.
            self.park_surface()?;
        } else if !self.auto_hide {
            self.reset_surface()?;
        }
        if let Some(appbar) = &mut self.appbar {
            // An auto-hiding bar leaves the whole screen to the windows.
            let strip = if self.auto_hide { 0 } else { bar_height };
            appbar.reserve(Edge::Top, bounds, strip);
        }
        // Hidden, the bar keeps no window on screen, not even a thin strip:
        // a strip along the top edge took the clicks meant for the close
        // button of maximized windows and for browser tabs. The edge watcher
        // watches the pointer instead.
        let show = if on_screen {
            SWP_SHOWWINDOW
        } else {
            SWP_HIDEWINDOW
        };
        // SAFETY: Our own live window.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                self.visual_height,
                SWP_NOACTIVATE | show,
            )?;
            if on_screen && (self.auto_hide || self.panel.is_some()) {
                // TrackMouseEvent may report a leave immediately after showing
                // a window. Check real geometry while revealed, including if
                // no further WM_MOUSEMOVE reaches this window.
                SetTimer(Some(self.hwnd), POINTER_TIMER, POINTER_CHECK_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), POINTER_TIMER);
            }
            let _ = KillTimer(Some(self.hwnd), HIDE_ANIMATION_TIMER);
            if let Some(deadline) = self.hide_until {
                let wait = deadline.saturating_duration_since(Instant::now());
                SetTimer(
                    Some(self.hwnd),
                    HIDE_ANIMATION_TIMER,
                    (wait.as_millis() as u32).max(1),
                    None,
                );
            }
        }
        edge_watch::set_menu_bar((self.visible && hidden).then_some(bounds));
        self.sync_outside_click_hook()
    }

    /// The edge watcher has verified the pointer's dwell on this bar.
    pub fn edge_rested(&mut self, gfx: &Gfx, point: POINT) -> Result<()> {
        if self.visible && self.auto_hide && !self.revealed {
            if muelle_shell::tracker::caption_button_at(point, self.bounds) {
                edge_watch::retry_on_move(edge_watch::Target::MenuBar);
            } else {
                self.hover(gfx, true)?;
            }
        }
        Ok(())
    }

    fn sync_outside_click_hook(&mut self) -> Result<()> {
        let needed =
            self.panel_span.width() > 0.0 && self.visible && (!self.auto_hide || self.revealed);
        if needed && self.outside_click_hook.is_none() {
            // SAFETY: This process owns the hook procedure and installs the hook
            // only while the panel is visible.
            let module = unsafe { GetModuleHandleW(None)? };
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_MOUSE_LL,
                    Some(outside_click_proc),
                    Some(module.into()),
                    0,
                )?
            };
            ISLAND_HOOK_WINDOW.store(self.hwnd.0 as isize, Ordering::Relaxed);
            self.outside_click_hook = Some(hook);
        } else if !needed && let Some(hook) = self.outside_click_hook {
            // SAFETY: This hook was installed by this MenuBar.
            unsafe { UnhookWindowsHookEx(hook)? };
            self.outside_click_hook = None;
            ISLAND_HOOK_WINDOW.store(0, Ordering::Relaxed);
        }
        Ok(())
    }

    /// The minute changed, or something the bar shows did: redraw what differs.
    /// Privacy activity or the user's taskbar preferences changed.
    pub fn registry_changed(&mut self, gfx: &Gfx) -> Result<()> {
        if let Some(watch) = &mut self.privacy_watch {
            watch.arm();
        }
        if let Some(watch) = &mut self.settings_watch {
            watch.arm();
        }
        self.tick(gfx)
    }

    /// The clock timer fired. With seconds shown, only the label changes on
    /// most ticks, so only the label is redrawn.
    pub fn clock_tick(&mut self, gfx: &Gfx) -> Result<()> {
        let shown = self.visible && (!self.auto_hide || self.revealed);
        // SAFETY: Read-only query.
        let second = unsafe { GetLocalTime() }.wSecond;
        match clock_tick_plan(
            self.widgets.clock.seconds,
            second,
            shown,
            self.last_full_tick.elapsed(),
        ) {
            ClockTick::Full => self.tick(gfx),
            ClockTick::Skip => {
                self.schedule_next_minute();
                Ok(())
            }
            ClockTick::LabelOnly => {
                if self.redraw_clock_label(gfx)? {
                    self.schedule_next_minute();
                    Ok(())
                } else {
                    self.tick(gfx)
                }
            }
        }
    }

    /// Swaps the clock label's brush for the current time. `false` when that
    /// is not possible (nothing drawn yet, or the new label would shift its
    /// neighbours): redraw everything then.
    fn redraw_clock_label(&mut self, gfx: &Gfx) -> Result<bool> {
        let (Some(look), Some(slot)) = (&mut self.look, &mut self.clock_slot) else {
            return Ok(false);
        };
        // Theme, display and settings changes arrive as their own events and
        // redraw everything; this only follows the time.
        let clock = system::clock_text_with(&self.widgets.clock);
        if clock == look.clock {
            return Ok(true);
        }
        let ink = if look.light { Ink::Dark } else { Ink::Light };
        let label = gfx.text_brush(&clock, look.scale, false, ink)?;
        if !label_fits_slot(slot.width, label.width) {
            return Ok(false);
        }
        let left = (slot.right - label.width).round();
        slot.visual.SetBrush(&label.brush)?;
        slot.visual.SetSize(Vector2 {
            X: label.width,
            Y: label.height,
        })?;
        slot.visual.SetOffset(Vector3 {
            X: left,
            Y: ((slot.bar_height - label.height) / 2.0).round(),
            Z: 0.0,
        })?;
        slot.width = label.width;
        if let Some(zone) = self
            .zones
            .iter_mut()
            .find(|(_, _, action)| *action == Action::Notifications)
        {
            zone.0 = left - slot.slop;
        }
        look.clock = clock;
        Ok(true)
    }

    pub fn tick(&mut self, gfx: &Gfx) -> Result<()> {
        self.last_full_tick = Instant::now();
        self.update_status();
        let audio_changed = self
            .audio
            .as_ref()
            .is_some_and(AudioWatch::take_default_changed);
        let retry_due = Instant::now() >= self.accessory_retry_at
            && self.accessories.as_ref().is_none_or(|monitor| {
                monitor.snapshot().state == muelle_shell::accessories::State::Unavailable
            });
        if self.accessories_enabled && (audio_changed || retry_due) {
            self.restart_accessories(gfx)?;
        }
        self.schedule_next_minute();
        self.refresh(gfx)
    }

    /// Ordinary audio notifications do not requery network, registry, providers
    /// or clock. A default-device change still restarts enabled accessories.
    pub fn audio_changed(&mut self, gfx: &Gfx) -> Result<()> {
        let volume = self.audio.as_mut().and_then(AudioWatch::volume);
        if self
            .audio
            .as_ref()
            .is_some_and(AudioWatch::take_default_changed)
            && self.accessories_enabled
        {
            self.status.volume = volume;
            self.restart_accessories(gfx)?;
        }
        self.apply_volume_reading(gfx, volume)
    }

    fn apply_volume_reading(&mut self, gfx: &Gfx, volume: Option<Volume>) -> Result<()> {
        self.status.volume = volume;
        if let Some(look) = &self.look
            && same_volume_indicator(look.status.volume, volume)
        {
            if let Some(volume) = volume
                && let Some(controls) = &mut self.volume_controls
            {
                controls.paint(gfx, self.volume_target.unwrap_or(volume.percent))?;
            }
            if let Some(look) = &mut self.look {
                look.status.volume = volume;
            }
            return Ok(());
        }
        // Availability, mute and speaker-glyph changes affect layout, but do
        // not justify a new provider/process or system-status scan either.
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        look.status.volume = volume;
        look.control = self.control.clone();
        self.apply_look(gfx, look)
    }

    fn update_status(&mut self) {
        self.status = Status {
            network: status::network(),
            volume: self.audio.as_mut().and_then(AudioWatch::volume),
            buttons: status::taskbar_buttons(),
            privacy: status::privacy(),
            battery_percentage: status::battery_percentage_enabled(),
        };
    }

    /// One wake-up per minute, aligned to the minute, instead of polling; one
    /// per second while the clock shows seconds.
    fn schedule_next_minute(&self) {
        // SAFETY: Read-only query, then a timer on our own window (setting an
        // existing timer id replaces it).
        unsafe {
            let now: SYSTEMTIME = GetLocalTime();
            let (period, elapsed) = if self.widgets.clock.seconds {
                (1_000_u32, u32::from(now.wMilliseconds))
            } else {
                (
                    60_000_u32,
                    u32::from(now.wSecond) * 1000 + u32::from(now.wMilliseconds),
                )
            };
            SetTimer(
                Some(self.hwnd),
                CLOCK_TIMER,
                period - elapsed.min(period - 100) + 50,
                None,
            );
        }
    }

    pub fn click(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        if let Some(hit) = self.island_hits.iter().find(|hit| hit.at(x, y)) {
            let bounds = hit.bounds;
            match hit.action {
                IslandAction::TrayIcon(index) => {
                    // Close first, so the app's own menu is not covered.
                    self.queue_tray_click(index, TrayButton::Left, bounds);
                    return self.dismiss_island(gfx);
                }
                IslandAction::TraySettings => {
                    open_settings("ms-settings:taskbar");
                    return self.dismiss_island(gfx);
                }
                IslandAction::Toggle(island) => {
                    self.panel = toggle_panel(self.panel, island);
                    if let Some(open) = self.panel {
                        self.opened(open)?;
                    }
                }
                IslandAction::Refresh => {
                    if let Some(usage) = &mut self.usage {
                        usage.refresh(true);
                    }
                }
                IslandAction::Media(control) => {
                    if let Some(media) = &self.media
                        && let Some(track) = self.look.as_ref().and_then(|look| look.track.as_ref())
                    {
                        media.send(track, control);
                    }
                    return Ok(());
                }
                IslandAction::Slider(..) => return Ok(()), // Handled on press/drag.
                IslandAction::ToggleWifi => {
                    self.toggle_radio(RadioKind::WiFi);
                }
                IslandAction::ToggleBluetooth => {
                    self.toggle_radio(RadioKind::Bluetooth);
                }
                IslandAction::ToggleFocus => {
                    self.deferred = Some(Deferred::ToggleDnd);
                    return Ok(());
                }
                IslandAction::OpenNetwork => {
                    let anchor = self
                        .zone_anchor(Action::Network)
                        .or_else(|| self.zone_anchor(Action::ControlCenter))
                        .unwrap_or_default();
                    self.deferred = Some(Deferred::OpenNetwork(anchor));
                    return self.dismiss_island(gfx);
                }
                IslandAction::OpenBluetooth => {
                    open_settings("ms-settings:bluetooth");
                    return self.dismiss_island(gfx);
                }
                IslandAction::QuickSettings => {
                    launcher::open_quick_settings();
                    return self.dismiss_island(gfx);
                }
                IslandAction::SoundSettings => {
                    open_settings("ms-settings:sound");
                    return self.dismiss_island(gfx);
                }
                IslandAction::SetOutput(index) => {
                    let Some(output) = self.control.outputs.get(index) else {
                        return Ok(());
                    };
                    if let Err(error) = status::set_default_output(&output.id) {
                        muelle_shell::log::line(&format!("cambiar salida de audio: {error:#}"));
                        open_settings("ms-settings:sound");
                        return self.dismiss_island(gfx);
                    }
                    self.control.outputs = status::output_devices();
                    self.audio_changed(gfx)?;
                }
                IslandAction::Mute => {
                    if let Some(audio) = &mut self.audio {
                        audio.toggle_mute()?;
                    }
                    return self.audio_changed(gfx);
                }
                IslandAction::ScrollUsage(direction) => {
                    self.scroll_usage(f32::from(direction) * 72.0)?;
                    return Ok(());
                }
            }
            return self.refresh(gfx);
        }
        if y >= (self.bounds.bottom - self.bounds.top) as f32 {
            return Ok(());
        }
        let open = self.panel;
        let hit = self
            .zones
            .iter()
            .find(|(left, right, _)| x >= *left && x < *right)
            .copied();
        // The Control Center and Sound items toggle their own popup.
        let zone_popup = match hit.map(|(_, _, action)| action) {
            Some(Action::ControlCenter) => Some(Island::ControlCenter),
            Some(Action::Sound) => Some(Island::Sound),
            // Without the tray relay the arrow opens Windows' own flyout.
            Some(Action::HiddenIcons) if self.tray.is_some() => Some(Island::Tray),
            _ => None,
        };
        if let Some(island) = zone_popup {
            self.panel = toggle_panel(open, island);
            if let Some(open) = self.panel {
                self.opened(open)?;
            }
            return self.refresh(gfx);
        }
        self.dismiss_island(gfx)?;
        // Where a flyout opened from this item should hang: under its centre.
        let anchor = hit.map_or((0, 0), |(left, right, _)| {
            (
                self.bounds.left + ((left + right) / 2.0) as i32,
                self.bounds.bottom,
            )
        });
        match hit.map(|(_, _, action)| action) {
            Some(Action::AccessoryIndicator) => {}
            Some(Action::QuickSettings) => launcher::open_quick_settings(),
            Some(Action::Notifications) => launcher::open_notifications(),
            // Needs the app, which is borrowed while the bar handles a click.
            Some(Action::ToggleDnd) => self.deferred = Some(Deferred::ToggleDnd),
            // Dropdowns open outside the app borrow; the popups opened above.
            Some(Action::ControlCenter | Action::Sound | Action::Network | Action::Battery) => {}
            Some(Action::Search) => launcher::open_search(),
            Some(Action::TaskView) => launcher::open_task_view(),
            Some(Action::Widgets) => launcher::open_widgets(),
            Some(Action::Bluetooth) => launcher::open_bluetooth(),
            Some(Action::HiddenIcons) => launcher::toggle_hidden_icons(anchor),
            Some(Action::InputLanguage) => {} // Opened outside the app borrow.
            // Windows' own pages list which apps use each device.
            Some(Action::Microphone) => {
                let _ = launcher::open("ms-settings:privacy-microphone");
            }
            Some(Action::Camera) => {
                let _ = launcher::open("ms-settings:privacy-webcam");
            }
            None => {}
        }
        Ok(())
    }

    /// The dropdown a click at `(x, y)` opens, with the point it hangs from.
    pub fn dropdown_at(&self, x: f32, y: f32) -> Option<(Dropdown, POINT)> {
        if y < 0.0 || y >= (self.bounds.bottom - self.bounds.top) as f32 {
            return None;
        }
        self.zones.iter().find_map(|(left, right, action)| {
            let kind = match action {
                Action::InputLanguage => Dropdown::Input,
                Action::Network => Dropdown::Network,
                Action::Battery => Dropdown::Battery,
                _ => return None,
            };
            (x >= *left && x < *right).then(|| {
                (
                    kind,
                    POINT {
                        x: self.bounds.left + ((left + right) * 0.5) as i32,
                        y: self.bounds.bottom + 6,
                    },
                )
            })
        })
    }

    /// Work the last click left for the app to do outside the bar.
    pub fn take_deferred(&mut self) -> Option<Deferred> {
        self.deferred.take()
    }

    /// Hands the bar the tray relay (or takes it away). Without one, the
    /// hidden-icons arrow opens Windows' flyout.
    pub fn set_tray(&mut self, tray: Option<Rc<TrayHost>>) {
        if tray.is_none() {
            self.control.tray.clear();
            self.tray_brushes.clear();
            if self.panel == Some(Island::Tray) {
                self.panel = None;
            }
        }
        self.tray = tray;
    }

    /// Asks the app to click the app behind tile `index`, at the tile's centre.
    fn queue_tray_click(&mut self, index: usize, button: TrayButton, bounds: (f32, f32, f32, f32)) {
        let Some(entry) = self.control.tray.get(index) else {
            return;
        };
        let (left, top, right, bottom) = bounds;
        let at = POINT {
            x: self.bounds.left + ((left + right) * 0.5) as i32,
            y: self.bounds.top + ((top + bottom) * 0.5) as i32,
        };
        self.deferred = Some(Deferred::TrayClick(entry.key.clone(), button, at));
    }

    /// A right click: only the tiles of the hidden-icons popup answer to it.
    pub fn right_click(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        let Some((index, bounds)) = self.island_hits.iter().find_map(|hit| match hit.action {
            IslandAction::TrayIcon(index) if hit.at(x, y) => Some((index, hit.bounds)),
            _ => None,
        }) else {
            return Ok(());
        };
        self.queue_tray_click(index, TrayButton::Right, bounds);
        self.dismiss_island(gfx)
    }

    /// The relay reported a change: reread the icons while the popup shows them.
    ///
    /// Apps re-add their icons in a burst, so the read waits for the burst to
    /// settle. The relay sends no further notification until icons are read,
    /// so a burst costs one read per settling period.
    pub fn tray_changed(&mut self, _gfx: &Gfx) -> Result<()> {
        if self.panel != Some(Island::Tray) {
            return Ok(());
        }
        if arm_tray_flush(&mut self.tray_flush_armed) {
            // SAFETY: A timer on our own window.
            unsafe { SetTimer(Some(self.hwnd), TRAY_TIMER, TRAY_SETTLE_MS, None) };
        }
        Ok(())
    }

    /// The burst of tray changes settled: reads the icons and redraws.
    pub fn tray_flush(&mut self, gfx: &Gfx) -> Result<()> {
        // SAFETY: Our own timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TRAY_TIMER);
        }
        self.tray_flush_armed = false;
        if self.panel != Some(Island::Tray) {
            return Ok(());
        }
        self.read_tray();
        self.control_changed(gfx)
    }

    /// Takes the relay's current icons (which also re-arms its notification).
    fn read_tray(&mut self) {
        let icons = self
            .tray
            .as_ref()
            .map(|tray| tray.icons())
            .unwrap_or_default();
        let pids: Vec<u32> = icons.iter().map(|info| info.pid).collect();
        let entries: Vec<TrayEntry> = icons
            .into_iter()
            .take(TRAY_MAX_ICONS)
            .map(|info| TrayEntry::from_info(info, &mut self.tray_names))
            .collect();
        // Names of programs whose icons are gone are looked up again if they
        // return (a process id may be reused by another program).
        self.tray_names.retain(|pid, _| pids.contains(pid));
        self.control.tray = entries;
    }

    /// Where a flyout hangs from the bar item `action`: under its centre.
    fn zone_anchor(&self, action: Action) -> Option<POINT> {
        self.zones
            .iter()
            .find(|(_, _, found)| *found == action)
            .map(|(left, right, _)| POINT {
                x: self.bounds.left + ((left + right) * 0.5) as i32,
                y: self.bounds.bottom + 6,
            })
    }

    /// The bar's window, as the relay's notification target.
    pub fn window(&self) -> isize {
        self.hwnd.0 as isize
    }

    /// Focus (do not disturb) changed in the app.
    pub fn set_dnd(&mut self, gfx: &Gfx, on: bool) -> Result<()> {
        if self.control.dnd == on {
            return Ok(());
        }
        self.control.dnd = on;
        self.control_changed(gfx)
    }

    /// A popup opened: follow the pointer out of it, and read what it shows.
    fn opened(&mut self, island: Island) -> Result<()> {
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: `track` describes our live window.
        unsafe { TrackMouseEvent(&mut track)? };
        match island {
            Island::ControlCenter => {
                // The popup opens with the devices it knew; the fresh list
                // arrives from a worker thread.
                read_outputs(self.hwnd.0 as isize);
                read_panel_data(self.hwnd.0 as isize, true);
            }
            Island::Sound => read_outputs(self.hwnd.0 as isize),
            Island::Tray => {
                self.control.tray_hover = None;
                self.read_tray();
                // Ask the apps to announce their icons again (not more than
                // every so often); they arrive through TRAY_CHANGED while the
                // popup is open.
                if let Some(tray) = &self.tray {
                    tray.refresh_if_older_than(TRAY_REFRESH_MIN);
                }
            }
            Island::Music | Island::Usage => {}
        }
        Ok(())
    }

    /// Switches a radio on a worker thread (Windows blocks while it does) and
    /// reads the result back.
    fn toggle_radio(&mut self, kind: RadioKind) {
        let current = match kind {
            RadioKind::WiFi => &mut self.control.wifi,
            RadioKind::Bluetooth => &mut self.control.bluetooth,
        };
        let Some(on) = *current else {
            return;
        };
        *current = Some(!on);
        let window = self.hwnd.0 as isize;
        let started = std::thread::Builder::new()
            .name("muelle-radio".into())
            .spawn(move || {
                if let Err(error) = radio::set_radio(kind, !on) {
                    muelle_shell::log::line(&format!("cambiar radio {kind:?}: {error}"));
                }
                read_panel_data(window, false);
            });
        if let Err(error) = started {
            muelle_shell::log::line(&format!("cambiar radio: {error}"));
        }
    }

    /// Takes in what the worker threads read; redraws if the popup shows it.
    pub fn panel_data_changed(&mut self, gfx: &Gfx) -> Result<()> {
        let updates = std::mem::take(&mut *PANEL_INBOX.lock().unwrap_or_else(|e| e.into_inner()));
        for update in updates {
            match update {
                PanelUpdate::Radios {
                    wifi,
                    bluetooth,
                    ssid,
                } => {
                    self.control.wifi = wifi;
                    self.control.bluetooth = bluetooth;
                    self.control.ssid = ssid;
                }
                PanelUpdate::Outputs(outputs) => self.control.outputs = outputs,
                PanelUpdate::Brightness(percent) => {
                    // A drag in progress owns the slider.
                    if self.brightness_target.is_none() {
                        self.control.brightness = percent;
                    }
                }
            }
        }
        self.control_changed(gfx)
    }

    /// Redraws the bar if anything it shows has changed.
    pub fn refresh(&mut self, gfx: &Gfx) -> Result<()> {
        let Some(monitor) = system::primary_monitor() else {
            return Ok(());
        };
        let active_providers = if self.usage.is_some() {
            usage::active_providers()
        } else {
            ActiveProviders::default()
        };
        let track = self.media.as_ref().and_then(media::Watch::snapshot);
        self.panel = available_panel(self.panel, track.is_some(), active_providers.any());
        if !active_providers.any() {
            if let Some(usage) = &mut self.usage {
                usage.mark_inactive();
            }
        } else if self.visible
            && (!self.auto_hide || self.revealed)
            && let Some(usage) = &mut self.usage
        {
            usage.refresh(false);
        }
        let look = Look {
            accessories: self.accessories.as_ref().map_or([None, None], |monitor| {
                accessory_readings(&monitor.snapshot())
            }),
            language: strings::language(),
            screen: monitor.screen,
            scale: monitor.scale,
            light: system::uses_light_theme(),
            glass: system::menu_glass(),
            app_name: self.app_name.clone(),
            clock: system::clock_text_with(&self.widgets.clock),
            input: system::input_language(),
            battery: system::battery(),
            status: self.status,
            widgets: self.widgets.clone(),
            usage: self
                .usage
                .as_ref()
                .map_or_else(Snapshot::default, UsageMonitor::snapshot),
            usage_enabled: self.usage.is_some() && active_providers.any(),
            active_providers,
            panel: self.panel,
            usage_refreshing: self.usage.as_ref().is_some_and(UsageMonitor::is_refreshing),
            track,
            control: self.control.clone(),
        };
        self.apply_look(gfx, look)
    }

    /// Only what the popups show (`control`) changed: redraws from the last
    /// look without reading the system status, the process list, the battery
    /// or the glass again.
    fn control_changed(&mut self, gfx: &Gfx) -> Result<()> {
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        look.control = self.control.clone();
        look.panel = self.panel;
        self.apply_look(gfx, look)
    }

    /// Track changes only need to update the contents of the island.
    pub fn media_changed(&mut self, gfx: &Gfx) -> Result<()> {
        let Some(mut look) = self.look.clone() else {
            return self.refresh(gfx);
        };
        look.track = self.media.as_ref().and_then(media::Watch::snapshot);
        look.control = self.control.clone();
        self.panel = available_panel(self.panel, look.track.is_some(), look.usage_enabled);
        look.panel = self.panel;
        self.apply_look(gfx, look)
    }

    fn apply_look(&mut self, gfx: &Gfx, mut look: Look) -> Result<()> {
        look.active_providers = look.usage.visible_providers(look.active_providers);
        look.usage_enabled &= look.active_providers.any();
        self.panel = available_panel(look.panel, look.track.is_some(), look.usage_enabled);
        look.panel = self.panel;
        if look.panel != Some(Island::Usage) {
            self.usage_scroll = 0.0;
        }
        let moved = self
            .look
            .as_ref()
            .is_some_and(|old| old.screen != look.screen || old.scale != look.scale);
        if !look.panel.is_some_and(Island::has_sliders) || moved {
            self.release_volume(None)?;
        }
        if self.look.as_ref() == Some(&look) {
            return Ok(());
        }
        let moment = self
            .look
            .as_ref()
            .and_then(|old| usage_moment(&old.usage, &look.usage, &look.active_providers));
        self.build(gfx, &look)?;
        // A crowded bar can have no room even for a compact trigger. Do not
        // keep a ghost popup that reappears by itself when space returns.
        if look.panel.is_some() && self.panel_span.width() <= 0.0 {
            self.panel = None;
            look.panel = None;
            self.build(gfx, &look)?;
        }
        self.look = Some(look);
        if let Some((provider, act)) = moment
            && let Some(&(_, x)) = self.usage_anchors.iter().find(|(p, _)| *p == provider)
        {
            self.cue = Some((act, x));
        }
        Ok(())
    }

    /// A moment worth the crab, found by the last update: the act and the
    /// screen x of the provider in the island. Taken once.
    pub fn take_cue(&mut self) -> Option<(mascot::Act, POINT)> {
        let (act, x) = self.cue.take()?;
        let shown = self.visible && (!self.auto_hide || self.revealed);
        shown.then_some((
            act,
            POINT {
                x,
                y: self.bounds.bottom,
            },
        ))
    }

    fn build(&mut self, gfx: &Gfx, look: &Look) -> Result<()> {
        let c = &gfx.compositor;
        let scale = look.scale;
        let px = |logical: f32| (logical * scale).round();
        let screen = look.screen;
        let (width, height) = ((screen.right - screen.left) as f32, px(HEIGHT));
        let panel_bottom = height + px(PANEL_GAP) + px(look.panel_height());
        let visual_height = if look.panel.is_some() {
            panel_bottom + px(PANEL_SHADOW_MARGIN)
        } else {
            height
        };
        let ink = if look.light { Ink::Dark } else { Ink::Light };

        let root = c.CreateContainerVisual()?;
        root.SetSize(Vector2 {
            X: width,
            Y: visual_height,
        })?;
        let layers = root.Children()?;
        // Without transparency the bar is solid, as macOS draws it with
        // "Reduce transparency".
        let alpha = glass_alpha(look.light, look.widgets.glass_strength);
        let tint = match (look.light, look.glass) {
            (true, true) => rgba(255, 255, 255, alpha),
            (false, true) => rgba(24, 24, 26, alpha),
            (true, false) => rgba(236, 236, 238, 255),
            (false, false) => rgba(36, 36, 38, 255),
        };
        let backdrop = if look.glass {
            Some(c.CreateHostBackdropBrush()?.cast()?)
        } else {
            None
        };
        let fills: [windows::UI::Composition::CompositionBrush; 1] =
            [c.CreateColorBrushWithColor(tint)?.cast()?];
        for brush in backdrop.iter().chain(&fills) {
            let fill = c.CreateSpriteVisual()?;
            fill.SetSize(Vector2 {
                X: width,
                Y: height,
            })?;
            fill.SetBrush(brush)?;
            layers.InsertAtTop(&fill)?;
        }

        let place = |layers: &windows::UI::Composition::VisualCollection,
                     item: &LabelBrush,
                     left: f32|
         -> Result<SpriteVisual> {
            let visual = c.CreateSpriteVisual()?;
            visual.SetSize(Vector2 {
                X: item.width,
                Y: item.height,
            })?;
            visual.SetOffset(Vector3 {
                X: left.round(),
                Y: ((height - item.height) / 2.0).round(),
                Z: 0.0,
            })?;
            visual.SetBrush(&item.brush)?;
            layers.InsertAtTop(&visual)?;
            Ok(visual)
        };

        self.zones.clear();
        self.clock_slot = None;
        let mut left_end = px(SIDE_MARGIN);
        if !look.app_name.is_empty() {
            let name = gfx.text_brush_fitted(
                &look.app_name,
                scale,
                13.0,
                true,
                ink,
                px(220.0).min(width * 0.22),
            )?;
            place(&layers, &name, px(SIDE_MARGIN))?;
            left_end += name.width;
        }

        // Right side, from the edge inward.
        let slop = px(HIT_SLOP);
        let mut right = width - px(SIDE_MARGIN);
        let status = look.status;
        let symbol = |layers: &[(char, f32)]| gfx.glyph_brush(layers, scale, ink);
        let mut items: Vec<(LabelBrush, Action)> = vec![(
            gfx.text_brush(&look.clock, scale, false, ink)?,
            Action::Notifications,
        )];
        // Privacy indicators, as the Windows tray shows them: something is
        // using the microphone or camera right now.
        let privacy = status.privacy;
        let show_search = look.widgets.search.unwrap_or(status.buttons.search);
        let show_task_view = look.widgets.task_view.unwrap_or(status.buttons.task_view);
        let show_widgets = look.widgets.widgets.unwrap_or(status.buttons.widgets);
        let show_battery_percentage = look
            .widgets
            .battery_percent
            .unwrap_or(status.battery_percentage);
        let hidden = &look.widgets.hidden;
        let battery_shown = look.battery.is_some() && !hidden.contains(&MenuBarItem::Battery);
        let has_accessories = look.accessories.iter().any(Option::is_some);
        let mut battery_label = None;
        let mut accessory_labels = Vec::new();
        let slots = bar_items(&look.widgets.order, |item| match item {
            MenuBarItem::Bluetooth => look.widgets.bluetooth,
            MenuBarItem::Search => show_search,
            MenuBarItem::TaskView => show_task_view,
            MenuBarItem::Widgets => show_widgets,
            MenuBarItem::Sound => status.volume.is_some() && !hidden.contains(&item),
            MenuBarItem::InputLanguage => look.input.is_some() && !hidden.contains(&item),
            // Accessory readings follow the battery slot even when the
            // battery itself is hidden.
            MenuBarItem::Battery => battery_shown || has_accessories,
            MenuBarItem::Privacy => {
                (privacy.microphone || privacy.camera) && !hidden.contains(&item)
            }
            _ => !hidden.contains(&item),
        });
        for slot in slots {
            match slot {
                MenuBarItem::Privacy => {
                    // Pushed right to left: the camera ends up left of the
                    // microphone, as before.
                    for (on, glyph, action) in [
                        (privacy.microphone, glyph::MICROPHONE, Action::Microphone),
                        (privacy.camera, glyph::CAMERA, Action::Camera),
                    ] {
                        if on {
                            items.push((symbol(&[(glyph, 1.0)])?, action));
                        }
                    }
                }
                MenuBarItem::ControlCenter => {
                    items.push((gfx.control_center_brush(scale, ink)?, Action::ControlCenter));
                }
                MenuBarItem::Focus => {
                    items.push((symbol(&[(glyph::FOCUS, 1.0)])?, Action::ToggleDnd));
                }
                MenuBarItem::Search => {
                    items.push((symbol(&[(glyph::SEARCH, 1.0)])?, Action::Search));
                }
                MenuBarItem::Bluetooth => {
                    items.push((symbol(&[(glyph::BLUETOOTH, 1.0)])?, Action::Bluetooth));
                }
                MenuBarItem::Wifi => match status.network {
                    Network::Wifi { bars, has_internet } => {
                        items.push((
                            gfx.wifi_brush(bars, has_internet, scale, ink)?,
                            Action::Network,
                        ));
                    }
                    Network::Ethernet => {
                        items.push((symbol(&[(glyph::ETHERNET, 1.0)])?, Action::Network));
                    }
                    Network::Offline => {
                        items.push((symbol(&[(glyph::OFFLINE, 1.0)])?, Action::Network));
                    }
                },
                MenuBarItem::Sound => {
                    if let Some(volume) = status.volume {
                        let speaker = if volume.muted {
                            glyph::MUTE
                        } else {
                            glyph::VOLUME[usize::from(volume.level.min(3))]
                        };
                        items.push((symbol(&[(speaker, 1.0)])?, Action::Sound));
                    }
                }
                MenuBarItem::Battery => {
                    if let Some(battery) = look.battery
                        && battery_shown
                    {
                        if show_battery_percentage {
                            battery_label = Some((
                                items.len(),
                                gfx.text_brush(
                                    &format!("{}%", battery.percent),
                                    scale,
                                    false,
                                    ink,
                                )?,
                            ));
                        }
                        items.push((
                            gfx.battery_brush(
                                battery.percent,
                                battery.charging,
                                battery.saver,
                                scale,
                                ink,
                            )?,
                            Action::Battery,
                        ));
                    }
                    for device in look.accessories.into_iter().flatten().rev() {
                        let value = device
                            .percent
                            .map_or_else(|| "—".to_owned(), |percent| format!("{percent}%"));
                        accessory_labels
                            .push((items.len(), gfx.text_brush(&value, scale, false, ink)?));
                        let icon = match device.kind {
                            AccessoryKind::Mouse => glyph::MOUSE,
                            AccessoryKind::Headset => glyph::HEADSET,
                        };
                        items.push((symbol(&[(icon, 1.0)])?, Action::AccessoryIndicator));
                    }
                }
                MenuBarItem::InputLanguage => {
                    if let Some(language) = &look.input {
                        items.push((
                            gfx.text_brush(language, scale, false, ink)?,
                            Action::InputLanguage,
                        ));
                    }
                }
                // The notification area (antivirus, OneDrive, apps in the
                // background) lives only in the hidden taskbar; this arrow
                // opens its flyout.
                MenuBarItem::HiddenIcons => {
                    items.push((symbol(&[(glyph::HIDDEN_ICONS, 1.0)])?, Action::HiddenIcons));
                }
                MenuBarItem::TaskView => {
                    items.push((symbol(&[(glyph::TASK_VIEW, 1.0)])?, Action::TaskView));
                }
                MenuBarItem::Widgets => {
                    items.push((symbol(&[(glyph::WIDGETS, 1.0)])?, Action::Widgets));
                }
            }
        }
        for (index, (item, action)) in items.iter().enumerate() {
            let zone_right = right;
            if let Some((_, label)) = battery_label
                .iter()
                .chain(accessory_labels.iter())
                .find(|(item_index, _)| *item_index == index)
            {
                right -= label.width;
                place(&layers, label, right)?;
                right -= px(6.0);
            }
            let left = right - item.width;
            let visual = place(&layers, item, left)?;
            if index == 0 {
                // The clock is the rightmost item, with nothing before it.
                self.clock_slot = Some(ClockSlot {
                    visual,
                    right,
                    width: item.width,
                    bar_height: height,
                    slop,
                });
            }
            // The battery icon and its percentage form one control.
            // Accessory readings are passive status indicators, not controls.
            if *action != Action::AccessoryIndicator {
                self.zones.push((left - slop, zone_right + slop, *action));
            }
            right = left - px(ITEM_GAP);
        }

        self.island_hits.clear();
        self.volume_controls = None;
        self.brightness_controls = None;
        self.usage_viewport = None;
        self.triggers_span = Span::default();
        self.panel_span = Span::default();
        self.zone_span = Span::default();
        self.draw_island(gfx, look, &layers, width, height, left_end, right)?;
        if let Some(island) = look.panel
            && let Some(action) = island.zone_action()
            && let Some(&(zone_left, zone_right, _)) =
                self.zones.iter().find(|(_, _, found)| *found == action)
        {
            self.zone_span = Span {
                left: zone_left,
                right: zone_right,
            };
            self.panel_span = zone_panel_span(
                self.zone_span,
                width,
                px(match island {
                    Island::Sound => SOUND_WIDTH,
                    Island::Tray => tray_width(look.control.tray.len()),
                    _ => CC_WIDTH,
                }),
                px(CC_MARGIN),
            );
            let geometry = IslandGeometry::new(self.panel_span, height + px(PANEL_GAP), look);
            let popup_height = px(look.panel_height());
            let theme = Theme::for_popup_with(look.light, look.glass, look.widgets.glass_strength);
            let content = popup_surface(gfx, theme, &layers, geometry, popup_height)?;
            match island {
                Island::Sound => self.draw_sound_popup(gfx, &content, geometry, look)?,
                Island::Tray => self.draw_tray_popup(gfx, &content, geometry, look)?,
                _ => self.draw_control_center(gfx, &content, geometry, look)?,
            }
        }
        if let Some((slider, span)) = self.slider_drag {
            let found = self.island_hits.iter().any(|hit| {
                matches!(hit.action, IslandAction::Slider(kind, found)
                    if kind == slider && found == span)
            });
            if !found {
                self.release_volume(None)?;
            }
        }
        self.surface.SetSize(Vector2 {
            X: width,
            Y: visual_height,
        })?;
        let contents = self.surface.Children()?;
        contents.RemoveAll()?;
        contents.InsertAtTop(&root)?;
        self.visual_height = if self.panel_span.width() > 0.0 {
            visual_height
        } else {
            height
        } as i32;
        self.bounds = RECT {
            bottom: screen.top + height as i32,
            ..screen
        };
        TRIGGERS_LEFT.store(
            screen.left + self.triggers_span.left as i32,
            Ordering::Relaxed,
        );
        TRIGGERS_RIGHT.store(
            screen.left + self.triggers_span.right as i32,
            Ordering::Relaxed,
        );
        PANEL_LEFT.store(screen.left + self.panel_span.left as i32, Ordering::Relaxed);
        PANEL_RIGHT.store(
            screen.left + self.panel_span.right as i32,
            Ordering::Relaxed,
        );
        let (zone_left, zone_right) = if self.zone_span.width() > 0.0 {
            (
                screen.left + self.zone_span.left as i32,
                screen.left + self.zone_span.right as i32,
            )
        } else {
            (0, 0)
        };
        ZONE_LEFT.store(zone_left, Ordering::Relaxed);
        ZONE_RIGHT.store(zone_right, Ordering::Relaxed);
        BAR_TOP.store(screen.top, Ordering::Relaxed);
        BAR_BOTTOM.store(screen.top + height as i32, Ordering::Relaxed);
        let clickable_height = if self.panel_span.width() > 0.0 {
            panel_bottom
        } else {
            height
        };
        PANEL_BOTTOM.store(screen.top + clickable_height as i32, Ordering::Relaxed);
        self.place()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "The island is laid out between the measured bar items"
    )]
    fn draw_island(
        &mut self,
        gfx: &Gfx,
        look: &Look,
        layers: &VisualCollection,
        width: f32,
        bar_height: f32,
        left_end: f32,
        right_start: f32,
    ) -> Result<()> {
        let px = |logical: f32| (logical * look.scale).round();
        let style = look.widgets.island_style;
        let cover = if let Some(track) = &look.track {
            let cached =
                self.cover
                    .as_ref()
                    .is_some_and(|(source, _)| match (source, &track.artwork) {
                        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
                        (None, None) => true,
                        _ => false,
                    });
            if !cached {
                self.cover = media::artwork(track)
                    .map(|image| {
                        let surface = gfx.image_surface(image)?;
                        Ok::<_, anyhow::Error>((
                            track.artwork.clone(),
                            gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?,
                        ))
                    })
                    .transpose()?;
            }
            self.cover.as_ref().map(|(_, brush)| brush.clone())
        } else {
            self.cover = None;
            None
        };
        let prepared_music = if style == IslandStyle::Bubble
            && let Some(track) = &look.track
        {
            Some(prepare_bubble_music(gfx, look, track)?)
        } else {
            None
        };
        let triggers = match style {
            IslandStyle::Notch => trigger_layout(
                Span {
                    left: left_end + px(8.0),
                    right: right_start - px(8.0),
                },
                width / 2.0,
                look.scale,
                look.track.is_some(),
                look.active_providers.count(),
            ),
            IslandStyle::Bubble => bubble_trigger_layout(
                Span {
                    left: left_end + px(8.0),
                    right: right_start,
                },
                look.scale,
                prepared_music.as_ref().map(|music| music.width),
                look.active_providers.count(),
            ),
        };
        let [(_, music), (_, usage)] = triggers;
        let (Some(first), Some(last)) = (music.or(usage), usage.or(music)) else {
            return Ok(());
        };
        self.triggers_span = Span {
            left: first.left,
            right: last.right,
        };
        let geometry = IslandGeometry::new(self.triggers_span, 0.0, look);
        let (compact, compact_height, divider_inset) = match style {
            IslandStyle::Notch => (
                island_surface(gfx, look, layers, geometry, bar_height)?,
                bar_height,
                px(7.0),
            ),
            IslandStyle::Bubble => {
                let (surface, pill_height) =
                    bubble_surface(gfx, look, layers, geometry, bar_height)?;
                (surface, pill_height, px(5.0))
            }
        };
        let divider = music
            .zip(usage)
            .map(|(music, usage)| ((music.right + usage.left) / 2.0).round());
        let c = &gfx.compositor;
        if let Some(x) = divider {
            add_visual(
                c,
                &compact,
                &c.CreateColorBrushWithColor(geometry.border())?.cast()?,
                x - self.triggers_span.left,
                divider_inset,
                px(1.0).max(1.0),
                (compact_height - 2.0 * divider_inset).max(1.0),
            )?;
        }
        let providers = island_providers(look);
        for (kind, span) in triggers {
            let Some(span) = span else {
                continue;
            };
            // Zone popups (Control Center, Sound) have no island trigger.
            if kind.zone_action().is_some() {
                continue;
            }
            let geometry = IslandGeometry::new(span, 0.0, look);
            let segment = c.CreateContainerVisual()?;
            segment.SetSize(Vector2::new(geometry.width, compact_height))?;
            segment.SetOffset(Vector3::new(span.left - self.triggers_span.left, 0.0, 0.0))?;
            let content = segment.Children()?;
            compact.InsertAtTop(&segment)?;
            match kind {
                Island::Music => {
                    if let Some(track) = &look.track {
                        match style {
                            IslandStyle::Notch => draw_compact_player(
                                gfx,
                                &content,
                                geometry,
                                geometry.width,
                                compact_height,
                                track,
                                cover.as_ref(),
                            )?,
                            IslandStyle::Bubble => {
                                if let Some(prepared) = &prepared_music {
                                    draw_bubble_player(
                                        gfx,
                                        &content,
                                        geometry,
                                        compact_height,
                                        track,
                                        prepared,
                                        cover.as_ref(),
                                    )?;
                                }
                            }
                        }
                    }
                }
                Island::Usage | Island::ControlCenter | Island::Sound | Island::Tray => {
                    let anchors = match style {
                        IslandStyle::Notch => {
                            draw_compact_usage(gfx, &content, geometry, compact_height, &providers)?
                        }
                        IslandStyle::Bubble => {
                            draw_bubble_usage(gfx, &content, geometry, compact_height, &providers)?
                        }
                    };
                    self.usage_anchors = anchors
                        .into_iter()
                        .map(|(provider, x)| (provider, look.screen.left + (span.left + x) as i32))
                        .collect();
                }
            }
            let (left, right) = match kind {
                Island::Music => (span.left, divider.unwrap_or(span.right)),
                Island::Usage | Island::ControlCenter | Island::Sound | Island::Tray => {
                    (divider.unwrap_or(span.left), span.right)
                }
            };
            self.island_hits.push(IslandHit {
                bounds: (left, 0.0, right, bar_height),
                action: IslandAction::Toggle(kind),
            });
            if look.panel != Some(kind) {
                continue;
            }
            let desired = px(match kind {
                Island::Music => 360.0,
                Island::Usage | Island::ControlCenter | Island::Sound | Island::Tray => {
                    ISLAND_EXPANDED_WIDTH
                }
            });
            self.panel_span = match style {
                IslandStyle::Notch => panel_span(span, width, desired),
                IslandStyle::Bubble => bubble_panel_span(self.triggers_span, width, desired),
            };
            let geometry = IslandGeometry::new(self.panel_span, bar_height + px(PANEL_GAP), look);
            let height = px(look.panel_height());
            let theme = Theme::for_popup_with(look.light, look.glass, look.widgets.glass_strength);
            let content = popup_surface(gfx, theme, layers, geometry, height)?;
            match kind {
                Island::Music => {
                    if let Some(track) = &look.track {
                        self.draw_player(gfx, &content, geometry, track, cover.as_ref())?;
                    }
                }
                Island::Usage | Island::ControlCenter | Island::Sound | Island::Tray => {
                    self.draw_usage_panel(gfx, &content, geometry, height, look, &providers)?
                }
            }
        }
        Ok(())
    }

    fn draw_usage_panel(
        &mut self,
        gfx: &Gfx,
        content: &VisualCollection,
        geometry: IslandGeometry,
        island_height: f32,
        look: &Look,
        providers: &[IslandProvider<'_>],
    ) -> Result<()> {
        let c = &gfx.compositor;
        let scale = geometry.scale;
        let px = |v| geometry.px(v);
        let island_width = geometry.width;
        let ink = geometry.ink();
        let title = strings::text();
        add_panel_text(
            gfx,
            content,
            title.usage_title,
            px(28.0),
            px(15.0),
            scale,
            13.0,
            true,
            ink,
            0.94,
        )?;
        let button_text = if look.usage_refreshing {
            title.usage_refreshing
        } else {
            title.usage_refresh
        };
        let button_label = gfx.text_brush_sized(button_text, scale, 11.5, true, ink)?;
        let button_width = button_label.width + px(20.0);
        let button_height = px(27.0);
        let button_left = island_width - px(23.0) - button_width;
        let button_top = px(8.0);
        add_rounded_box(
            c,
            content,
            button_left,
            button_top,
            button_width,
            button_height,
            px(13.5),
            geometry.control_fill(),
            Some(geometry.border()),
        )?;
        add_visual(
            c,
            content,
            &button_label.brush.cast()?,
            button_left + px(10.0),
            button_top + (button_height - button_label.height) / 2.0,
            button_label.width,
            button_label.height,
        )?;
        if !look.usage_refreshing {
            self.island_hits.push(geometry.hit(
                button_left,
                button_top,
                button_width,
                button_height,
                IslandAction::Refresh,
            ));
        }
        let body_top = px(42.0);
        let body_height = (island_height - body_top - px(26.0)).max(0.0);
        let content_height = px((look.panel_content_height() - 68.0).max(0.0));
        let maximum = ((content_height - body_height) / scale).max(0.0);
        self.usage_scroll = self.usage_scroll.clamp(0.0, maximum);
        let viewport = c.CreateContainerVisual()?;
        viewport.SetOffset(Vector3::new(0.0, body_top, 0.0))?;
        viewport.SetSize(Vector2::new(island_width, body_height))?;
        viewport.SetClip(&c.CreateInsetClip()?)?;
        let body = c.CreateContainerVisual()?;
        body.SetSize(Vector2::new(island_width, content_height))?;
        body.SetOffset(Vector3::new(0.0, -px(self.usage_scroll), 0.0))?;
        viewport.Children()?.InsertAtTop(&body)?;
        content.InsertAtTop(&viewport)?;
        let rows = body.Children()?;
        let mut row_top = 6.0;
        for (provider, usage, accent) in providers {
            let y = px(row_top);
            row_top += usage_row_height(usage);
            let inset = px(23.0);
            add_visual(
                c,
                &rows,
                &c.CreateColorBrushWithColor(geometry.border())?.cast()?,
                inset,
                y - px(6.0),
                island_width - 2.0 * inset,
                1.0,
            )?;
            draw_usage_row(
                gfx,
                &rows,
                *provider,
                usage,
                *accent,
                scale,
                look.light,
                island_width,
                y,
            )?;
        }
        let bounds = (
            geometry.left,
            geometry.top + body_top,
            geometry.left + island_width,
            geometry.top + body_top + body_height,
        );
        if maximum > 0.0 {
            let [up, down] = [
                ('\u{E70E}', body_top + px(2.0), -1),
                ('\u{E70D}', body_top + body_height - px(22.0), 1),
            ]
            .map(|(symbol, top, direction)| -> Result<SpriteVisual> {
                let label = gfx.glyph_brush(&[(symbol, 1.0)], scale * 0.7, ink)?;
                let visual = add_visual(
                    c,
                    content,
                    &label.brush.cast()?,
                    island_width - px(22.0) + (px(18.0) - label.width) / 2.0,
                    top + (px(20.0) - label.height) / 2.0,
                    label.width,
                    label.height,
                )?;
                self.island_hits.push(geometry.hit(
                    island_width - px(22.0),
                    top,
                    px(18.0),
                    px(20.0),
                    IslandAction::ScrollUsage(direction),
                ));
                Ok(visual)
            });
            self.usage_viewport = Some(UsageViewport {
                content: body,
                maximum,
                scale,
                bounds,
                up: up?,
                down: down?,
            });
            self.position_usage_body()?;
        }
        let newest_age = providers
            .iter()
            .filter_map(|provider| provider.1.age_minutes())
            .min();
        if let Some(minutes) = newest_age {
            let status = format!("{} {minutes} min", title.usage_updated);
            let label = gfx.text_brush_sized(&status, scale, 11.0, false, ink)?;
            add_visual(
                c,
                content,
                &label.brush.cast()?,
                (island_width - label.width) / 2.0,
                island_height - px(21.0),
                label.width,
                label.height,
            )?
            .SetOpacity(0.53)?;
        }
        Ok(())
    }

    fn draw_player(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        track: &Track,
        cover: Option<&CompositionSurfaceBrush>,
    ) -> Result<()> {
        let px = |v| geometry.px(v);
        let c = &gfx.compositor;
        let ink = geometry.ink();
        add_panel_text(
            gfx,
            layers,
            strings::text().media_title,
            px(26.0),
            px(15.0),
            geometry.scale,
            12.5,
            true,
            ink,
            0.88,
        )?;
        let chevron = gfx.glyph_brush(&[('', 0.72)], geometry.scale, ink)?;
        let x = geometry.width - px(22.0) - chevron.width;
        add_visual(
            c,
            layers,
            &chevron.brush.cast()?,
            x,
            (px(46.0) - chevron.height) / 2.0,
            chevron.width,
            chevron.height,
        )?;
        self.island_hits.push(geometry.hit(
            x - px(6.0),
            px(7.0),
            chevron.width + px(12.0),
            px(32.0),
            IslandAction::Toggle(Island::Music),
        ));
        let tile = px(34.0);
        if let Some(cover) = cover {
            add_visual(c, layers, &cover.cast()?, px(25.0), px(46.0), tile, tile)?;
        }
        let available = geometry.width - px(91.0);
        let title = gfx.text_brush_fitted(
            track_title(track),
            geometry.scale,
            13.0,
            true,
            ink,
            available,
        )?;
        let title_y = if track.artist.trim().is_empty() {
            px(63.0) - title.height / 2.0
        } else {
            px(47.0)
        };
        add_visual(
            c,
            layers,
            &title.brush.cast()?,
            px(69.0),
            title_y,
            title.width,
            title.height,
        )?;
        if !track.artist.trim().is_empty() {
            let artist =
                gfx.text_brush_fitted(&track.artist, geometry.scale, 11.0, false, ink, available)?;
            add_visual(
                c,
                layers,
                &artist.brush.cast()?,
                px(69.0),
                px(65.0),
                artist.width,
                artist.height,
            )?
            .SetOpacity(0.69)?;
        }
        let center = geometry.width / 2.0;
        for (control, glyph, enabled, offset) in [
            (MediaControl::Previous, '\u{E892}', track.previous, -44.0),
            (
                MediaControl::PlayPause,
                if track.playing {
                    '\u{E769}'
                } else {
                    '\u{E768}'
                },
                track.toggle,
                0.0,
            ),
            (MediaControl::Next, '\u{E893}', track.next, 44.0),
        ] {
            let size = px(34.0);
            let x = center + px(offset) - size / 2.0;
            let y = px(89.0);
            let main = matches!(control, MediaControl::PlayPause);
            if main {
                add_rounded_box(
                    c,
                    layers,
                    x,
                    y,
                    size,
                    size,
                    size / 2.0,
                    geometry.control_fill(),
                    Some(geometry.border()),
                )?;
            }
            let symbol = gfx.glyph_brush(&[(glyph, 1.0)], geometry.scale, ink)?;
            add_visual(
                c,
                layers,
                &symbol.brush.cast()?,
                x + (size - symbol.width) / 2.0,
                y + (size - symbol.height) / 2.0,
                symbol.width,
                symbol.height,
            )?
            .SetOpacity(if enabled { 1.0 } else { 0.30 })?;
            if enabled {
                self.island_hits
                    .push(geometry.hit(x, y, size, size, IslandAction::Media(control)));
            }
        }
        add_panel_text(
            gfx,
            layers,
            strings::text().media_volume,
            px(25.0),
            px(135.0),
            geometry.scale,
            10.5,
            false,
            ink,
            0.65,
        )?;
        if let Some(volume) = self.status.volume {
            let symbol = if volume.muted {
                glyph::MUTE
            } else {
                glyph::VOLUME[volume.level.min(3) as usize]
            };
            let label = gfx.glyph_brush(&[(symbol, 1.0)], geometry.scale, ink)?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                px(25.0),
                px(160.0) - label.height / 2.0,
                label.width,
                label.height,
            )?;
            self.island_hits.push(geometry.hit(
                px(18.0),
                px(148.0),
                px(32.0),
                px(27.0),
                IslandAction::Mute,
            ));
            let left = px(59.0);
            let width = (geometry.width - left - px(66.0)).max(px(1.0));
            let percent = self.volume_target.unwrap_or(volume.percent);
            self.add_slider(
                gfx,
                layers,
                geometry,
                Slider::Volume,
                160.0,
                (left, width),
                percent,
                Some(geometry.width - px(51.0)),
            )?;
        } else {
            add_panel_text(
                gfx,
                layers,
                strings::text().media_no_output,
                px(25.0),
                px(155.0),
                geometry.scale,
                11.0,
                false,
                ink,
                0.5,
            )?;
        }
        Ok(())
    }

    fn draw_control_center(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        look: &Look,
    ) -> Result<()> {
        let c = &gfx.compositor;
        let px = |v| geometry.px(v);
        let ink = geometry.ink();
        let scale = geometry.scale;
        let text = strings::text();
        let data = &look.control;
        let accent = accent_color();
        let module = |x: f32, y: f32, width: f32, height: f32| {
            add_rounded_box(
                c,
                layers,
                px(x),
                px(y),
                px(width),
                px(height),
                px(14.0),
                geometry.control_fill(),
                Some(geometry.border()),
            )
        };
        let column = (CC_WIDTH - 2.0 * CC_PAD - CC_GAP) / 2.0;
        let (left_x, right_x) = (CC_PAD, CC_PAD + column + CC_GAP);
        let top = CC_PAD;

        // Network, Bluetooth and Focus.
        module(left_x, top, column, CC_MODULE_HEIGHT)?;
        let unknown = "…";
        let on_off = |on: Option<bool>| match on {
            None => unknown,
            Some(true) => text.on,
            Some(false) => text.off,
        };
        let wifi_subtitle = match data.wifi {
            None => unknown.to_owned(),
            Some(false) => text.off.to_owned(),
            Some(true) => data
                .ssid
                .clone()
                .unwrap_or_else(|| text.not_connected.to_owned()),
        };
        let rows = [
            (
                glyph::WIFI,
                text.wifi,
                wifi_subtitle,
                data.wifi == Some(true),
                IslandAction::ToggleWifi,
                IslandAction::OpenNetwork,
            ),
            (
                glyph::BLUETOOTH,
                text.bluetooth,
                on_off(data.bluetooth).to_owned(),
                data.bluetooth == Some(true),
                IslandAction::ToggleBluetooth,
                IslandAction::OpenBluetooth,
            ),
            (
                glyph::FOCUS,
                text.focus,
                on_off(Some(data.dnd)).to_owned(),
                data.dnd,
                IslandAction::ToggleFocus,
                IslandAction::ToggleFocus,
            ),
        ];
        for (index, (symbol, title, subtitle, on, toggle, open)) in rows.into_iter().enumerate() {
            let row = top + 48.0 * index as f32;
            let circle = px(26.0);
            let (circle_x, circle_y) = (px(left_x + 10.0), px(row + 11.0));
            add_rounded_box(
                c,
                layers,
                circle_x,
                circle_y,
                circle,
                circle,
                circle / 2.0,
                if on { accent } else { geometry.control_fill() },
                (!on).then(|| geometry.border()),
            )?;
            let icon =
                gfx.glyph_brush(&[(symbol, 1.0)], scale, if on { Ink::Light } else { ink })?;
            add_visual(
                c,
                layers,
                &icon.brush.cast()?,
                circle_x + (circle - icon.width) / 2.0,
                circle_y + (circle - icon.height) / 2.0,
                icon.width,
                icon.height,
            )?;
            let text_width = px(column - 44.0 - 6.0);
            let label = gfx.text_brush_fitted(title, scale, 13.0, true, ink, text_width)?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                px(left_x + 44.0),
                px(row + 7.0),
                label.width,
                label.height,
            )?;
            let label = gfx.text_brush_fitted(&subtitle, scale, 11.0, false, ink, text_width)?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                px(left_x + 44.0),
                px(row + 25.0),
                label.width,
                label.height,
            )?
            .SetOpacity(0.6)?;
            self.island_hits.push(geometry.hit(
                px(left_x + 4.0),
                px(row + 5.0),
                px(38.0),
                px(38.0),
                toggle,
            ));
            self.island_hits.push(geometry.hit(
                px(left_x + 44.0),
                px(row + 4.0),
                px(column - 44.0),
                px(40.0),
                open,
            ));
        }

        // Now Playing, or the way out to the Windows flyout.
        module(right_x, top, column, CC_MODULE_HEIGHT)?;
        if let Some(track) = &look.track {
            if let Some((_, cover)) = &self.cover {
                add_visual(
                    c,
                    layers,
                    &cover.cast()?,
                    px(right_x + 10.0),
                    px(top + 10.0),
                    px(36.0),
                    px(36.0),
                )?;
            }
            let available = px(column - 20.0);
            let title =
                gfx.text_brush_fitted(track_title(track), scale, 12.5, true, ink, available)?;
            add_visual(
                c,
                layers,
                &title.brush.cast()?,
                px(right_x + 10.0),
                px(top + 54.0),
                title.width,
                title.height,
            )?;
            if !track.artist.trim().is_empty() {
                let artist =
                    gfx.text_brush_fitted(&track.artist, scale, 11.0, false, ink, available)?;
                add_visual(
                    c,
                    layers,
                    &artist.brush.cast()?,
                    px(right_x + 10.0),
                    px(top + 72.0),
                    artist.width,
                    artist.height,
                )?
                .SetOpacity(0.69)?;
            }
            let center = px(right_x + column / 2.0);
            for (control, symbol, enabled, offset) in [
                (MediaControl::Previous, '\u{E892}', track.previous, -42.0),
                (
                    MediaControl::PlayPause,
                    if track.playing {
                        '\u{E769}'
                    } else {
                        '\u{E768}'
                    },
                    track.toggle,
                    0.0,
                ),
                (MediaControl::Next, '\u{E893}', track.next, 42.0),
            ] {
                let size = px(34.0);
                let x = center + px(offset) - size / 2.0;
                let y = px(top + 100.0);
                let icon = gfx.glyph_brush(&[(symbol, 1.0)], scale, ink)?;
                add_visual(
                    c,
                    layers,
                    &icon.brush.cast()?,
                    x + (size - icon.width) / 2.0,
                    y + (size - icon.height) / 2.0,
                    icon.width,
                    icon.height,
                )?
                .SetOpacity(if enabled { 1.0 } else { 0.30 })?;
                if enabled {
                    self.island_hits.push(geometry.hit(
                        x,
                        y,
                        size,
                        size,
                        IslandAction::Media(control),
                    ));
                }
            }
        } else {
            let icon = gfx.control_center_brush(scale, ink)?;
            let center = px(right_x + column / 2.0);
            add_visual(
                c,
                layers,
                &icon.brush.cast()?,
                center - icon.width / 2.0,
                px(top + 48.0) - icon.height / 2.0,
                icon.width,
                icon.height,
            )?;
            let label = gfx.text_brush_fitted(
                text.quick_settings,
                scale,
                11.0,
                false,
                ink,
                px(column - 16.0),
            )?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                center - label.width / 2.0,
                px(top + 80.0),
                label.width,
                label.height,
            )?
            .SetOpacity(0.8)?;
            self.island_hits.push(geometry.hit(
                px(right_x),
                px(top),
                px(column),
                px(CC_MODULE_HEIGHT),
                IslandAction::QuickSettings,
            ));
        }

        // Display and Sound.
        let full = CC_WIDTH - 2.0 * CC_PAD;
        let track_left = 52.0;
        let track_width = CC_PAD + full - 16.0 - track_left;
        let mut y = top + CC_MODULE_HEIGHT + CC_GAP;
        if let Some(stored) = data.brightness {
            module(CC_PAD, y, full, CC_SLIDER_MODULE_HEIGHT)?;
            add_panel_text(
                gfx,
                layers,
                text.display,
                px(CC_PAD + 14.0),
                px(y + 9.0),
                scale,
                12.0,
                true,
                ink,
                0.9,
            )?;
            let sun = gfx.glyph_brush(&[(glyph::BRIGHTNESS, 1.0)], scale, ink)?;
            add_visual(
                c,
                layers,
                &sun.brush.cast()?,
                px(CC_PAD + 14.0),
                px(y + 42.0) - sun.height / 2.0,
                sun.width,
                sun.height,
            )?;
            self.add_slider(
                gfx,
                layers,
                geometry,
                Slider::Brightness,
                y + 42.0,
                (px(track_left), px(track_width)),
                self.brightness_target.unwrap_or(stored),
                None,
            )?;
            y += CC_SLIDER_MODULE_HEIGHT + CC_GAP;
        }
        module(CC_PAD, y, full, CC_SOUND_MODULE_HEIGHT)?;
        add_panel_text(
            gfx,
            layers,
            text.sound,
            px(CC_PAD + 14.0),
            px(y + 9.0),
            scale,
            12.0,
            true,
            ink,
            0.9,
        )?;
        if let Some(volume) = self.status.volume {
            self.draw_speaker(gfx, layers, geometry, volume, CC_PAD + 14.0, y + 42.0)?;
            self.add_slider(
                gfx,
                layers,
                geometry,
                Slider::Volume,
                y + 42.0,
                (px(track_left), px(track_width)),
                self.volume_target.unwrap_or(volume.percent),
                None,
            )?;
        } else {
            add_panel_text(
                gfx,
                layers,
                text.media_no_output,
                px(CC_PAD + 14.0),
                px(y + 36.0),
                scale,
                11.0,
                false,
                ink,
                0.5,
            )?;
        }
        let device = data
            .outputs
            .iter()
            .find(|output| output.default)
            .map_or(text.media_no_output, |output| output.name.as_str());
        let name =
            gfx.text_brush_fitted(device, scale, 11.5, false, ink, px(full - 28.0 - 22.0))?;
        add_visual(
            c,
            layers,
            &name.brush.cast()?,
            px(CC_PAD + 14.0),
            px(y + 66.0),
            name.width,
            name.height,
        )?
        .SetOpacity(0.7)?;
        let chevron = gfx.glyph_brush(&[(glyph::CHEVRON_RIGHT, 0.72)], scale * 0.8, ink)?;
        add_visual(
            c,
            layers,
            &chevron.brush.cast()?,
            px(CC_PAD + full - 14.0) - chevron.width,
            px(y + 66.0) + (name.height - chevron.height) / 2.0,
            chevron.width,
            chevron.height,
        )?
        .SetOpacity(0.7)?;
        self.island_hits.push(geometry.hit(
            px(CC_PAD),
            px(y + 58.0),
            px(full),
            px(32.0),
            IslandAction::Toggle(Island::Sound),
        ));
        Ok(())
    }

    /// The speaker glyph, which mutes and unmutes, centred on logical `y`.
    fn draw_speaker(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        volume: Volume,
        x: f32,
        y: f32,
    ) -> Result<()> {
        let px = |v| geometry.px(v);
        let symbol = if volume.muted {
            glyph::MUTE
        } else {
            glyph::VOLUME[usize::from(volume.level.min(3))]
        };
        let label = gfx.glyph_brush(&[(symbol, 1.0)], geometry.scale, geometry.ink())?;
        add_visual(
            &gfx.compositor,
            layers,
            &label.brush.cast()?,
            px(x),
            px(y) - label.height / 2.0,
            label.width,
            label.height,
        )?;
        self.island_hits.push(geometry.hit(
            px(x - 8.0),
            px(y - 13.5),
            px(32.0),
            px(27.0),
            IslandAction::Mute,
        ));
        Ok(())
    }

    fn draw_sound_popup(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        look: &Look,
    ) -> Result<()> {
        let c = &gfx.compositor;
        let px = |v| geometry.px(v);
        let ink = geometry.ink();
        let scale = geometry.scale;
        let text = strings::text();
        let accent = accent_color();
        let inset = 12.0;
        add_panel_text(
            gfx,
            layers,
            text.sound,
            px(16.0),
            px(12.0),
            scale,
            13.0,
            true,
            ink,
            1.0,
        )?;
        if let Some(volume) = self.status.volume {
            self.draw_speaker(gfx, layers, geometry, volume, 16.0, 52.0)?;
            self.add_slider(
                gfx,
                layers,
                geometry,
                Slider::Volume,
                52.0,
                (px(46.0), px(SOUND_WIDTH - 46.0 - 54.0)),
                self.volume_target.unwrap_or(volume.percent),
                Some(px(SOUND_WIDTH - 44.0)),
            )?;
        } else {
            add_panel_text(
                gfx,
                layers,
                text.media_no_output,
                px(16.0),
                px(45.0),
                scale,
                11.0,
                false,
                ink,
                0.5,
            )?;
        }
        let separator = |y: f32| -> Result<()> {
            add_visual(
                c,
                layers,
                &c.CreateColorBrushWithColor(geometry.border())?.cast()?,
                px(inset),
                px(y),
                px(SOUND_WIDTH - 2.0 * inset),
                px(1.0).max(1.0),
            )
            .map(|_| ())
        };
        separator(76.0)?;
        add_panel_text(
            gfx,
            layers,
            text.output,
            px(16.0),
            px(86.0),
            scale,
            11.0,
            true,
            ink,
            0.6,
        )?;
        let shown = look.control.outputs.iter().take(SOUND_MAX_OUTPUTS);
        for (index, output) in shown.enumerate() {
            let row = SOUND_HEADER_HEIGHT + SOUND_ROW_HEIGHT * index as f32;
            if output.default {
                add_rounded_box(
                    c,
                    layers,
                    px(18.0),
                    px(row + 10.0),
                    px(8.0),
                    px(8.0),
                    px(4.0),
                    accent,
                    None,
                )?;
            }
            let label = gfx.text_brush_fitted(
                &output.name,
                scale,
                12.0,
                false,
                ink,
                px(SOUND_WIDTH - 40.0 - 14.0),
            )?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                px(40.0),
                px(row + 5.0),
                label.width,
                label.height,
            )?;
            self.island_hits.push(geometry.hit(
                px(8.0),
                px(row),
                px(SOUND_WIDTH - 16.0),
                px(SOUND_ROW_HEIGHT),
                IslandAction::SetOutput(index),
            ));
        }
        let end = sound_popup_height(look.control.outputs.len()) - SOUND_FOOTER_HEIGHT;
        separator(end + 4.0)?;
        add_panel_text(
            gfx,
            layers,
            text.sound_settings,
            px(16.0),
            px(end + 15.0),
            scale,
            12.0,
            false,
            ink,
            1.0,
        )?;
        self.island_hits.push(geometry.hit(
            px(8.0),
            px(end + 10.0),
            px(SOUND_WIDTH - 16.0),
            px(SOUND_ROW_HEIGHT),
            IslandAction::SoundSettings,
        ));
        Ok(())
    }

    /// The brush for a tray icon's image, kept while the icon's pixels do not
    /// change so that redraws only re-parent it.
    fn tray_brush(&mut self, gfx: &Gfx, entry: &TrayEntry) -> Result<CompositionSurfaceBrush> {
        if let Some((bgra, brush)) = self.tray_brushes.get(&entry.key)
            && (Arc::ptr_eq(bgra, &entry.bgra) || **bgra == *entry.bgra)
        {
            return Ok(brush.clone());
        }
        let image = muelle_shell::icons::IconImage {
            width: entry.width,
            height: entry.height,
            bgra: (*entry.bgra).clone(),
        };
        let surface = gfx.image_surface(&image)?;
        let brush = gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        // Icons are not always square.
        brush.SetStretch(CompositionStretch::Uniform)?;
        self.tray_brushes
            .insert(entry.key.clone(), (Arc::clone(&entry.bgra), brush.clone()));
        Ok(brush)
    }

    fn draw_tray_popup(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        look: &Look,
    ) -> Result<()> {
        let c = &gfx.compositor;
        let px = |v| geometry.px(v);
        let ink = geometry.ink();
        let scale = geometry.scale;
        let text = strings::text();
        let icons = &look.control.tray;
        let width = tray_width(icons.len());
        self.tray_brushes
            .retain(|key, _| icons.iter().any(|entry| &entry.key == key));
        if icons.is_empty() {
            add_panel_text(
                gfx,
                layers,
                text.no_icons,
                px(TRAY_PAD + 6.0),
                px(TRAY_PAD + 2.0),
                scale,
                12.0,
                false,
                ink,
                0.5,
            )?;
        }
        let hover_fill = if geometry.light {
            rgba(0, 0, 0, 30)
        } else {
            rgba(255, 255, 255, 40)
        };
        for (index, entry) in icons.iter().enumerate() {
            let (cell_x, cell_y) = tray_cell(index);
            if look.control.tray_hover == Some(index) {
                add_rounded_box(
                    c,
                    layers,
                    px(cell_x),
                    px(cell_y),
                    px(TRAY_CELL),
                    px(TRAY_CELL),
                    px(6.0),
                    hover_fill,
                    None,
                )?;
            }
            let brush = self.tray_brush(gfx, entry)?;
            add_visual(
                c,
                layers,
                &brush.cast()?,
                px(cell_x + (TRAY_CELL - TRAY_ICON_SIZE) / 2.0),
                px(cell_y + (TRAY_CELL - TRAY_ICON_SIZE) / 2.0),
                px(TRAY_ICON_SIZE),
                px(TRAY_ICON_SIZE),
            )?;
            self.island_hits.push(geometry.hit(
                px(cell_x),
                px(cell_y),
                px(TRAY_CELL),
                px(TRAY_CELL),
                IslandAction::TrayIcon(index),
            ));
        }
        // The hovered icon's name, in the strip under the grid.
        if let Some(index) = look.control.tray_hover
            && let Some(entry) = icons.get(index)
            && !entry.name.is_empty()
        {
            let (cell_x, _) = tray_cell(index);
            let label = gfx.text_brush_fitted(
                &entry.name,
                scale,
                11.0,
                false,
                ink,
                px(width - 2.0 * TRAY_PAD) - px(12.0),
            )?;
            let bubble_width = label.width + px(12.0);
            let bubble_height = label.height + px(6.0);
            let left = (px(cell_x + TRAY_CELL / 2.0) - bubble_width / 2.0)
                .min(px(width - 4.0) - bubble_width)
                .max(px(4.0));
            let top = px(tray_grid_bottom(icons.len()) + 1.0);
            let fill = if geometry.light {
                rgba(250, 250, 252, 255)
            } else {
                rgba(58, 58, 62, 255)
            };
            add_rounded_box(
                c,
                layers,
                left,
                top,
                bubble_width,
                bubble_height,
                px(5.0),
                fill,
                Some(geometry.border()),
            )?;
            add_visual(
                c,
                layers,
                &label.brush.cast()?,
                left + px(6.0),
                top + px(3.0),
                label.width,
                label.height,
            )?;
        }
        let end = tray_popup_height(icons.len()) - TRAY_FOOTER_HEIGHT;
        add_visual(
            c,
            layers,
            &c.CreateColorBrushWithColor(geometry.border())?.cast()?,
            px(TRAY_PAD),
            px(end),
            px(width - 2.0 * TRAY_PAD),
            px(1.0).max(1.0),
        )?;
        add_panel_text(
            gfx,
            layers,
            text.tray_settings,
            px(TRAY_PAD + 6.0),
            px(end + 7.0),
            scale,
            11.0,
            false,
            ink,
            0.6,
        )?;
        self.island_hits.push(geometry.hit(
            px(TRAY_PAD / 2.0),
            px(end + 2.0),
            px(width - TRAY_PAD),
            px(TRAY_FOOTER_HEIGHT - 4.0),
            IslandAction::TraySettings,
        ));
        Ok(())
    }

    pub fn press_volume(&mut self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        self.suppress_volume_release = false;
        let Some((slider, span)) = self.island_hits.iter().find_map(|hit| match hit.action {
            IslandAction::Slider(slider, span) if hit.at(x, y) => Some((slider, span)),
            _ => None,
        }) else {
            return Ok(());
        };
        self.slider_drag = Some((slider, span));
        // SAFETY: Capture only for a live slider drag, never to activate the bar.
        unsafe {
            SetCapture(self.hwnd);
        }
        self.queue_slider(slider, span, x);
        self.paint_volume_preview(gfx)
    }

    /// One slider of a popup: a track, its fill, a knob, and the hit that
    /// starts a drag. `track` is its logical `(left, width)` in the popup and
    /// `y` the logical centre line.
    #[expect(
        clippy::too_many_arguments,
        reason = "A slider is placed by its popup, kind, line, track, value and label together"
    )]
    fn add_slider(
        &mut self,
        gfx: &Gfx,
        layers: &VisualCollection,
        geometry: IslandGeometry,
        kind: Slider,
        y: f32,
        (left, width): (f32, f32),
        percent: u8,
        label_x: Option<f32>,
    ) -> Result<()> {
        let c = &gfx.compositor;
        let px = |v| geometry.px(v);
        add_rounded_box(
            c,
            layers,
            left,
            px(y - 2.0),
            width,
            px(4.0),
            px(2.0),
            geometry.control_fill(),
            None,
        )?;
        let (_, fill) = retained_rounded_box(
            c,
            layers,
            left,
            px(y - 2.0),
            width,
            px(4.0),
            px(2.0),
            if geometry.light {
                rgba(40, 48, 58, 220)
            } else {
                rgba(240, 244, 250, 235)
            },
            None,
        )?;
        let dot = px(10.0);
        let (knob, _) = retained_rounded_box(
            c,
            layers,
            left - dot / 2.0,
            px(y) - dot / 2.0,
            dot,
            dot,
            dot / 2.0,
            rgba(245, 247, 250, 255),
            Some(geometry.border()),
        )?;
        let value = match label_x {
            Some(x) => {
                let label = gfx.text_brush_sized(
                    &format!("{percent}%"),
                    geometry.scale,
                    10.5,
                    false,
                    geometry.ink(),
                )?;
                let value = add_visual(
                    c,
                    layers,
                    &label.brush.cast()?,
                    x,
                    px(y - 7.0),
                    label.width,
                    label.height,
                )?;
                value.SetOpacity(0.8)?;
                Some(value)
            }
            None => None,
        };
        let mut controls = SliderControls {
            fill,
            knob,
            value,
            geometry,
            y,
            left,
            width,
            position_percent: None,
            label_percent: percent,
        };
        controls.move_to(percent)?;
        match kind {
            Slider::Volume => self.volume_controls = Some(controls),
            Slider::Brightness => self.brightness_controls = Some(controls),
        }
        self.island_hits.push(geometry.hit(
            left - px(6.0),
            px(y - 13.0),
            width + px(12.0),
            px(29.0),
            IslandAction::Slider(
                kind,
                Span {
                    left: geometry.left + left,
                    right: geometry.left + left + width,
                },
            ),
        ));
        Ok(())
    }

    pub fn scroll_usage_at(&mut self, point: POINT, delta: i16) -> Result<bool> {
        if !self.visible || (self.auto_hide && !self.revealed) || delta == 0 {
            return Ok(false);
        }
        let Some(viewport) = &self.usage_viewport else {
            return Ok(false);
        };
        let (x, y) = (
            (point.x - self.bounds.left) as f32,
            (point.y - self.bounds.top) as f32,
        );
        let (left, top, right, bottom) = viewport.bounds;
        if x < left || x >= right || y < top || y >= bottom {
            return Ok(false);
        }
        self.scroll_usage(-f32::from(delta) / 120.0 * 48.0)
    }

    fn scroll_usage(&mut self, delta: f32) -> Result<bool> {
        let Some(viewport) = &self.usage_viewport else {
            return Ok(false);
        };
        let next = (self.usage_scroll + delta).clamp(0.0, viewport.maximum);
        if next != self.usage_scroll {
            self.usage_scroll = next;
            self.position_usage_body()?;
        }
        Ok(true)
    }

    fn position_usage_body(&mut self) -> Result<()> {
        let Some(viewport) = &self.usage_viewport else {
            return Ok(());
        };
        let offset = (self.usage_scroll * viewport.scale).round();
        viewport
            .content
            .SetOffset(Vector3::new(0.0, -offset, 0.0))?;
        viewport
            .up
            .SetOpacity(if self.usage_scroll > 0.0 { 0.8 } else { 0.22 })?;
        viewport
            .down
            .SetOpacity(if self.usage_scroll < viewport.maximum {
                0.8
            } else {
                0.22
            })?;
        Ok(())
    }

    fn paint_volume_preview(&mut self, gfx: &Gfx) -> Result<()> {
        if let Some(percent) = self.volume_target
            && let Some(controls) = &mut self.volume_controls
        {
            controls.paint(gfx, percent)?;
        }
        if let Some(percent) = self.brightness_target
            && let Some(controls) = &mut self.brightness_controls
        {
            controls.paint(gfx, percent)?;
        }
        Ok(())
    }

    fn queue_slider(&mut self, slider: Slider, span: Span, x: f32) {
        let percent = volume_percent(span, x);
        let arm = self.pending_volume.is_none() && self.pending_brightness.is_none();
        match slider {
            Slider::Volume => {
                self.volume_target = Some(percent);
                if let Some(controls) = &mut self.volume_controls {
                    // Optimistic GPU-only feedback is independent of endpoint latency.
                    // Audio writes still proceed if device loss interrupts this paint;
                    // the existing graphics heartbeat owns recovery.
                    let _ = controls.move_to(percent);
                }
                if self.pending_volume == Some(percent)
                    || (self.pending_volume.is_none()
                        && self
                            .status
                            .volume
                            .is_some_and(|v| v.percent == percent && !v.muted))
                {
                    return;
                }
                self.pending_volume = Some(percent);
            }
            Slider::Brightness => {
                self.brightness_target = Some(percent);
                if let Some(controls) = &mut self.brightness_controls {
                    let _ = controls.move_to(percent);
                }
                if self.pending_brightness == Some(percent)
                    || (self.pending_brightness.is_none()
                        && self.control.brightness == Some(percent))
                {
                    return;
                }
                self.pending_brightness = Some(percent);
            }
        }
        if arm {
            // At most 30 endpoint writes/second during dragging; no idle timer.
            unsafe {
                SetTimer(Some(self.hwnd), VOLUME_TIMER, 33, None);
            }
        }
    }

    pub fn flush_volume(&mut self) -> Result<()> {
        // SAFETY: Our own short-lived drag timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), VOLUME_TIMER);
        }
        if let Some(percent) = self.pending_brightness.take() {
            write_brightness(percent);
        }
        if let Some(percent) = self.pending_volume.take()
            && let Some(audio) = &mut self.audio
        {
            audio.set_percent(percent)?;
        }
        Ok(())
    }

    pub fn release_volume(&mut self, x: Option<f32>) -> Result<bool> {
        let Some((slider, span)) = self.slider_drag.take() else {
            return Ok(x.is_some() && std::mem::take(&mut self.suppress_volume_release));
        };
        self.suppress_volume_release = true;
        if let Some(x) = x {
            self.queue_slider(slider, span, x);
        }
        // Clear state first: ReleaseCapture posts a capture-lost notification.
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
        }
        if let Err(error) = self.flush_volume() {
            muelle_shell::log::line(&format!("ajustar volumen: {error:#}"));
        }
        self.volume_target = None;
        // The display keeps what the drag set.
        if let Some(percent) = self.brightness_target.take() {
            self.control.brightness = Some(percent);
        }
        Ok(true)
    }
}

fn open_settings(page: &str) {
    if let Err(error) = launcher::open(page) {
        muelle_shell::log::line(&format!("abrir {page}: {error}"));
    }
}

fn post_panel_updates(window: isize, updates: Vec<PanelUpdate>) {
    PANEL_INBOX
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(updates);
    // SAFETY: Posting to the bar window; a destroyed window just fails.
    unsafe {
        let _ = PostMessageW(
            Some(HWND(window as *mut _)),
            PANEL_DATA_CHANGED,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// Lists the audio output devices on a worker thread. The result comes back
/// as `PANEL_DATA_CHANGED`.
fn read_outputs(window: isize) {
    let started = std::thread::Builder::new()
        .name("muelle-outputs".into())
        .spawn(move || {
            post_panel_updates(
                window,
                vec![PanelUpdate::Outputs(status::output_devices_on_worker())],
            );
        });
    if let Err(error) = started {
        muelle_shell::log::line(&format!("leer salidas de audio: {error}"));
    }
}

/// Whether the caller must start the timer that reads a burst of tray
/// changes: only the first change of a burst does.
fn arm_tray_flush(armed: &mut bool) -> bool {
    !std::mem::replace(armed, true)
}

/// Reads the radios (and the brightness) on a worker thread: Windows blocks
/// while it lists them. The result comes back as `PANEL_DATA_CHANGED`.
fn read_panel_data(window: isize, with_brightness: bool) {
    let started = std::thread::Builder::new()
        .name("muelle-control-center".into())
        .spawn(move || {
            let (wifi, bluetooth) = radio::wifi_and_bluetooth_on();
            let ssid = if wifi == Some(true) {
                muelle_shell::wifi::networks()
                    .into_iter()
                    .find(|network| network.connected)
                    .map(|network| network.ssid)
            } else {
                None
            };
            let mut updates = vec![PanelUpdate::Radios {
                wifi,
                bluetooth,
                ssid,
            }];
            if with_brightness {
                updates.push(PanelUpdate::Brightness(brightness::get()));
            }
            post_panel_updates(window, updates);
        });
    if let Err(error) = started {
        muelle_shell::log::line(&format!("leer centro de control: {error}"));
    }
}

/// Writes the brightness on one worker thread (WMI blocks). Writes that pile
/// up while it works are dropped in favour of the latest.
fn write_brightness(percent: u8) {
    static WRITER: OnceLock<Option<std::sync::mpsc::Sender<u8>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<u8>();
        std::thread::Builder::new()
            .name("muelle-brightness".into())
            .spawn(move || {
                // One connection serves every write of the drag.
                let mut writer = brightness::Writer::new();
                while let Ok(mut percent) = receiver.recv() {
                    while let Ok(newer) = receiver.try_recv() {
                        percent = newer;
                    }
                    if let Err(error) = writer.set(percent) {
                        muelle_shell::log::line(&format!("ajustar brillo: {error}"));
                    }
                }
            })
            .ok()
            .map(|_| sender)
    });
    if let Some(writer) = writer {
        let _ = writer.send(percent);
    }
}

fn volume_percent(span: Span, x: f32) -> u8 {
    if span.width() <= 0.0 || !x.is_finite() {
        return 0;
    }
    (((x - span.left) / span.width()).clamp(0.0, 1.0) * 100.0).round() as u8
}

fn same_volume_indicator(old: Option<Volume>, new: Option<Volume>) -> bool {
    old.map(|v| (v.level, v.muted)) == new.map(|v| (v.level, v.muted))
}

#[derive(Clone, Copy)]
struct IslandGeometry {
    left: f32,
    top: f32,
    width: f32,
    scale: f32,
    light: bool,
}

impl IslandGeometry {
    fn new(span: Span, top: f32, look: &Look) -> Self {
        Self {
            left: span.left,
            top,
            width: span.width(),
            scale: look.scale,
            light: look.light,
        }
    }

    fn hit(self, x: f32, y: f32, width: f32, height: f32, action: IslandAction) -> IslandHit {
        let left = self.left + x;
        let top = self.top + y;
        IslandHit {
            bounds: (left, top, left + width, top + height),
            action,
        }
    }

    fn px(self, logical: f32) -> f32 {
        (logical * self.scale).round()
    }
    fn ink(self) -> Ink {
        if self.light { Ink::Dark } else { Ink::Light }
    }
    fn control_fill(self) -> Color {
        if self.light {
            rgba(0, 0, 0, 18)
        } else {
            rgba(255, 255, 255, 27)
        }
    }
    fn border(self) -> Color {
        if self.light {
            rgba(35, 57, 83, 31)
        } else {
            rgba(223, 237, 255, 37)
        }
    }
}

fn toggle_panel(open: Option<Island>, clicked: Island) -> Option<Island> {
    (open != Some(clicked)).then_some(clicked)
}

fn available_panel(open: Option<Island>, music: bool, usage: bool) -> Option<Island> {
    open.filter(|island| match island {
        Island::Music => music,
        Island::Usage => usage,
        Island::ControlCenter | Island::Sound | Island::Tray => true,
    })
}

fn trigger_layout(
    bounds: Span,
    center: f32,
    scale: f32,
    music: bool,
    providers: usize,
) -> [(Island, Option<Span>); 2] {
    let px = |v: f32| (v * scale).round();
    let mut result = [(Island::Music, None), (Island::Usage, None)];
    let gap = if music && providers > 0 { px(8.0) } else { 0.0 };
    let music_width = if music { px(214.0) } else { 0.0 };
    let usage_width = match providers {
        0 => 0.0,
        1 => px(ISLAND_SINGLE_WIDTH),
        count => px(134.0 * count as f32),
    };
    let total = music_width + usage_width;
    let available = (bounds.width() - gap).min(total);
    if total == 0.0 || available < px(if gap > 0.0 { 85.0 } else { 42.0 }) {
        return result;
    }
    let music_width = if gap > 0.0 {
        (available * music_width / total)
            .round()
            .clamp(px(42.0), available - px(42.0))
    } else if music {
        available
    } else {
        0.0
    };
    let left = (center - (available + gap) / 2.0)
        .clamp(
            bounds.left,
            (bounds.right - available - gap).max(bounds.left),
        )
        .round();
    if music {
        result[0].1 = Some(Span {
            left,
            right: left + music_width,
        });
    }
    if providers > 0 {
        result[1].1 = Some(Span {
            left: left + music_width + gap,
            right: left + available + gap,
        });
    }
    result
}

fn bubble_trigger_layout(
    bounds: Span,
    scale: f32,
    music_width: Option<f32>,
    providers: usize,
) -> [(Island, Option<Span>); 2] {
    let px = |v: f32| (v * scale).round();
    let mut result = [(Island::Music, None), (Island::Usage, None)];
    let music = music_width.is_some();
    let gap = if music && providers > 0 { px(8.0) } else { 0.0 };
    let music_width = music_width
        .map(|width| width.clamp(px(36.0), px(BUBBLE_MUSIC_WIDTH)))
        .unwrap_or(0.0);
    let usage_width = if providers > 0 {
        px(BUBBLE_USAGE_PADDING + BUBBLE_PROVIDER_WIDTH * providers as f32)
    } else {
        0.0
    };
    let total = music_width + usage_width;
    let available = (bounds.width() - gap).min(total);
    if total == 0.0 || available < px(if gap > 0.0 { 72.0 } else { 36.0 }) {
        return result;
    }
    let music_width = if gap > 0.0 {
        (available * music_width / total)
            .round()
            .clamp(px(36.0), available - px(36.0))
    } else if music {
        available
    } else {
        0.0
    };
    let left = (bounds.right - available - gap).max(bounds.left).round();
    if music {
        result[0].1 = Some(Span {
            left,
            right: left + music_width,
        });
    }
    if providers > 0 {
        result[1].1 = Some(Span {
            left: left + music_width + gap,
            right: left + available + gap,
        });
    }
    result
}

fn panel_span(trigger: Span, screen_width: f32, desired: f32) -> Span {
    let width = desired.max(trigger.width()).min(screen_width);
    let left = ((trigger.left + trigger.right - width) / 2.0)
        .clamp(0.0, screen_width - width)
        .round();
    Span {
        left,
        right: left + width,
    }
}

/// A popup of up to `desired` width centred under its bar `zone`, kept
/// `margin` away from both screen edges.
fn zone_panel_span(zone: Span, screen_width: f32, desired: f32, margin: f32) -> Span {
    let width = desired.min((screen_width - 2.0 * margin).max(0.0));
    let left = ((zone.left + zone.right - width) / 2.0)
        .clamp(margin, (screen_width - margin - width).max(margin))
        .round();
    Span {
        left,
        right: left + width,
    }
}

fn bubble_panel_span(anchor: Span, screen_width: f32, desired: f32) -> Span {
    let width = desired.max(anchor.width()).min(screen_width);
    let left = (anchor.right - width)
        .clamp(0.0, screen_width - width)
        .round();
    Span {
        left,
        right: left + width,
    }
}

type IslandProvider<'a> = (Provider, &'a ProviderUsage, Color);

fn island_providers(look: &Look) -> Vec<IslandProvider<'_>> {
    Provider::ALL
        .into_iter()
        .filter(|provider| look.active_providers.has(*provider))
        .map(|provider| {
            let accent = match provider {
                Provider::Claude => rgba(217, 119, 87, 255),
                Provider::Codex => rgba(135, 190, 245, 255),
                Provider::OpenCode => rgba(132, 196, 166, 255),
                Provider::Gemini => rgba(111, 146, 255, 255),
            };
            (provider, look.usage.get(provider), accent)
        })
        .collect()
}

fn popup_surface(
    gfx: &Gfx,
    theme: Theme,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    height: f32,
) -> Result<VisualCollection> {
    let c = &gfx.compositor;
    let size = Vector2::new(geometry.width, height);
    let radius = geometry.px(PANEL_RADIUS);
    let panel = theme.panel(gfx, size, radius, geometry.scale)?;
    let popup = c.CreateContainerVisual()?;
    popup.SetSize(size)?;
    popup.SetOffset(Vector3::new(geometry.left.round(), geometry.top, 0.0))?;
    let children = popup.Children()?;
    let mask = c.CreateNineGridBrush()?;
    mask.SetSource(&gfx.rounded_mask(radius)?)?;
    mask.SetInsets(radius.ceil() + 1.0)?;
    children.InsertAtTop(&island_shadow(c, &mask.cast()?, size, geometry)?)?;
    children.InsertAtTop(&panel)?;
    layers.InsertAtTop(&popup)?;
    Ok(children)
}

fn island_shadow(
    c: &Compositor,
    mask: &CompositionBrush,
    size: Vector2,
    geometry: IslandGeometry,
) -> Result<SpriteVisual> {
    let shadow = c.CreateDropShadow()?;
    shadow.SetMask(mask)?;
    shadow.SetBlurRadius(geometry.px(12.0))?;
    shadow.SetOffset(Vector3::new(0.0, geometry.px(5.0), 0.0))?;
    shadow.SetColor(rgba(0, 0, 0, 92))?;
    let host = c.CreateSpriteVisual()?;
    host.SetSize(size)?;
    host.SetShadow(&shadow)?;
    Ok(host)
}

fn island_surface(
    gfx: &Gfx,
    look: &Look,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    island_height: f32,
) -> Result<VisualCollection> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let island_left = geometry.left;
    let island_width = geometry.width;
    let scale = geometry.scale;
    let size = Vector2::new(island_width, island_height);
    let island = c.CreateContainerVisual()?;
    island.SetSize(size)?;
    island.SetOffset(Vector3::new(island_left.round(), 0.0, 0.0))?;
    let content = island.Children()?;
    let silhouette = gfx.island_mask(island_width, island_height, scale)?;
    content.InsertAtTop(&island_shadow(c, &silhouette.cast()?, size, geometry)?)?;

    let tint = match (look.light, look.glass) {
        (true, true) => rgba(247, 249, 252, 243),
        (false, true) => rgba(7, 9, 13, 242),
        (true, false) => rgba(247, 249, 252, 255),
        (false, false) => rgba(10, 12, 17, 255),
    };
    if look.glass {
        // The backdrop cannot be used as a mask-brush source. Inset its
        // rounded clip so it stays entirely inside the curved silhouette.
        let inset = (12.0 * scale)
            .min(island_width / 5.0)
            .min(island_height / 2.0);
        let glass_width = island_width - inset * 2.0;
        let glass = c.CreateContainerVisual()?;
        glass.SetSize(Vector2 {
            X: glass_width,
            Y: island_height,
        })?;
        glass.SetOffset(Vector3 {
            X: inset,
            Y: 0.0,
            Z: 0.0,
        })?;
        let outline = c.CreateRoundedRectangleGeometry()?;
        let lift = px(22.0);
        outline.SetSize(Vector2 {
            X: glass_width,
            Y: island_height + lift,
        })?;
        outline.SetOffset(Vector2 { X: 0.0, Y: -lift })?;
        outline.SetCornerRadius(Vector2 { X: lift, Y: lift })?;
        glass.SetClip(&c.CreateGeometricClipWithGeometry(&outline)?)?;
        add_visual(
            c,
            &glass.Children()?,
            &c.CreateHostBackdropBrush()?.cast()?,
            0.0,
            0.0,
            glass_width,
            island_height,
        )?;
        content.InsertAtTop(&glass)?;
    }
    let masked = c.CreateMaskBrush()?;
    masked.SetSource(&c.CreateColorBrushWithColor(tint)?)?;
    masked.SetMask(&silhouette)?;
    add_visual(
        c,
        &content,
        &masked.cast()?,
        0.0,
        0.0,
        island_width,
        island_height,
    )?;

    layers.InsertAtTop(&island)?;
    Ok(content)
}

fn bubble_surface(
    gfx: &Gfx,
    look: &Look,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    bar_height: f32,
) -> Result<(VisualCollection, f32)> {
    let c = &gfx.compositor;
    let inset_y = geometry.px(BUBBLE_VERTICAL_INSET);
    let pill_height = (bar_height - 2.0 * inset_y).max(1.0);
    let radius = pill_height / 2.0;
    let size = Vector2::new(geometry.width, pill_height);
    let bubble = c.CreateContainerVisual()?;
    bubble.SetSize(size)?;
    bubble.SetOffset(Vector3::new(geometry.left.round(), inset_y, 0.0))?;
    let content = bubble.Children()?;

    let tint = match (look.light, look.glass) {
        (true, true) => rgba(247, 249, 252, 243),
        (false, true) => rgba(7, 9, 13, 242),
        (true, false) => rgba(247, 249, 252, 255),
        (false, false) => rgba(10, 12, 17, 255),
    };
    if look.glass {
        let glass = c.CreateContainerVisual()?;
        glass.SetSize(size)?;
        let outline = c.CreateRoundedRectangleGeometry()?;
        outline.SetSize(size)?;
        outline.SetCornerRadius(Vector2 {
            X: radius,
            Y: radius,
        })?;
        glass.SetClip(&c.CreateGeometricClipWithGeometry(&outline)?)?;
        add_visual(
            c,
            &glass.Children()?,
            &c.CreateHostBackdropBrush()?.cast()?,
            0.0,
            0.0,
            geometry.width,
            pill_height,
        )?;
        content.InsertAtTop(&glass)?;
    }
    add_rounded_box(
        c,
        &content,
        0.0,
        0.0,
        geometry.width,
        pill_height,
        radius,
        tint,
        None,
    )?;

    layers.InsertAtTop(&bubble)?;
    Ok((content, pill_height))
}

/// Draws the providers in the closed island and returns where each one's
/// figure sits (its centre, from the segment's left edge), for the crab.
/// The moment an AI plan's figure crosses a line worth showing, comparing
/// the previous update with this one: a limit reset to 0 % (after being in
/// use), past 90 %, or at 100 %. Nothing on the first figures after starting,
/// which have nothing to compare with.
fn usage_moment(
    old: &usage::Snapshot,
    new: &usage::Snapshot,
    active: &ActiveProviders,
) -> Option<(Provider, mascot::Act)> {
    Provider::ALL
        .into_iter()
        .filter(|p| active.has(*p))
        .find_map(|provider| {
            let previous = old.get(provider);
            let current = new.get(provider);
            if previous.state != State::Ready || current.state != State::Ready {
                return None;
            }
            let before_window = previous.main_window()?;
            let now_window = current.main_window()?;
            if before_window.minutes != now_window.minutes
                || before_window.label != now_window.label
            {
                return None;
            }
            if let (Some(before), Some(now)) = (previous.fetched_at, current.fetched_at)
                && now <= before
            {
                return None;
            }
            let before = before_window.used_percent;
            let now = now_window.used_percent;
            let act = if now >= 100 && before < 100 {
                mascot::Act::Exhausted
            } else if now >= 90 && before < 90 {
                mascot::Act::Nervous
            } else if now <= 1 && before >= 20 {
                mascot::Act::Fresh
            } else {
                return None;
            };
            Some((provider, act))
        })
}

fn draw_compact_usage(
    gfx: &Gfx,
    content: &VisualCollection,
    geometry: IslandGeometry,
    bar_height: f32,
    providers: &[IslandProvider<'_>],
) -> Result<Vec<(Provider, f32)>> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let scale = geometry.scale;
    let inset = px(13.0);
    let half = (geometry.width - 2.0 * inset) / providers.len().max(1) as f32;
    if half < px(20.0) {
        let icon = gfx.glyph_brush(&[('\u{E9D9}', 1.0)], scale, ink)?;
        add_visual(
            c,
            content,
            &icon.brush.cast()?,
            (geometry.width - icon.width) / 2.0,
            (bar_height - icon.height) / 2.0,
            icon.width,
            icon.height,
        )?;
        let centre = geometry.width / 2.0;
        return Ok(providers
            .iter()
            .map(|(provider, ..)| (*provider, centre))
            .collect());
    }
    let mut anchors = Vec::with_capacity(providers.len());
    for (index, (provider, usage, accent)) in providers.iter().enumerate() {
        anchors.push((*provider, inset + (index as f32 + 0.5) * half));
        // When space is tight the full list is still available in the panel.
        if half < px(20.0) {
            break;
        }
        let icon_size = px(18.0) as u32;
        let start = inset
            + index as f32 * half
            + if half < px(84.0) {
                (half - icon_size as f32) / 2.0
            } else {
                px(21.0)
            };
        if let Some(image) = usage::logo(*provider) {
            let image_brush = gfx.icon_brush(image, icon_size)?;
            let brush: CompositionBrush =
                if !matches!(provider, Provider::Claude | Provider::Gemini) && geometry.light {
                    let tinted = c.CreateMaskBrush()?;
                    tinted.SetMask(&image_brush)?;
                    tinted.SetSource(&c.CreateColorBrushWithColor(rgba(27, 34, 45, 255))?)?;
                    tinted.cast()?
                } else {
                    image_brush.cast()?
                };
            add_visual(
                c,
                content,
                &brush,
                start,
                (bar_height - icon_size as f32) / 2.0,
                icon_size as f32,
                icon_size as f32,
            )?;
        }
        let percent = usage.compact_text();
        if half > px(106.0) {
            let font = if usage.main_window().is_some() {
                14.0
            } else {
                12.0
            };
            let label = gfx.text_brush_sized(&percent, scale, font, true, ink)?;
            let reserve = if usage.main_window().is_some() {
                74.0
            } else {
                50.0
            };
            if label.width <= half - px(reserve) {
                add_visual(
                    c,
                    content,
                    &label.brush.cast()?,
                    start + px(25.0),
                    (bar_height - label.height) / 2.0,
                    label.width,
                    label.height,
                )?;
            }
        }
        if half >= px(84.0) {
            let ring = gfx.usage_ring(
                usage.main_window().map(|window| window.used_percent),
                scale,
                *accent,
            )?;
            add_visual(
                c,
                content,
                &ring.brush.cast()?,
                inset + (index + 1) as f32 * half - px(16.0) - ring.width,
                (bar_height - ring.height) / 2.0,
                ring.width,
                ring.height,
            )?;
        }
    }
    Ok(anchors)
}

fn draw_bubble_usage(
    gfx: &Gfx,
    content: &VisualCollection,
    geometry: IslandGeometry,
    pill_height: f32,
    providers: &[IslandProvider<'_>],
) -> Result<Vec<(Provider, f32)>> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let scale = geometry.scale;
    let inset = px(8.0);
    let slot = (geometry.width - 2.0 * inset) / providers.len().max(1) as f32;
    if slot < px(20.0) {
        let icon = gfx.glyph_brush(&[('\u{E9D9}', 1.0)], scale, ink)?;
        add_visual(
            c,
            content,
            &icon.brush.cast()?,
            (geometry.width - icon.width) / 2.0,
            (pill_height - icon.height) / 2.0,
            icon.width,
            icon.height,
        )?;
        let centre = geometry.width / 2.0;
        return Ok(providers
            .iter()
            .map(|(provider, ..)| (*provider, centre))
            .collect());
    }
    let icon_size = px(15.0) as u32;
    let gap = px(4.0);
    let mut anchors = Vec::with_capacity(providers.len());
    for (index, (provider, usage, _accent)) in providers.iter().enumerate() {
        let slot_left = inset + index as f32 * slot;
        anchors.push((*provider, slot_left + slot / 2.0));
        let percent = usage.compact_text();
        let label = if slot >= px(44.0) {
            let label = gfx.text_brush_sized(&percent, scale, 11.5, true, ink)?;
            (label.width + icon_size as f32 + gap <= slot).then_some(label)
        } else {
            None
        };
        let group_width = icon_size as f32 + label.as_ref().map_or(0.0, |text| gap + text.width);
        let start = slot_left + ((slot - group_width) / 2.0).max(0.0);
        if let Some(image) = usage::logo(*provider) {
            let image_brush = gfx.icon_brush(image, icon_size)?;
            let brush: CompositionBrush =
                if !matches!(provider, Provider::Claude | Provider::Gemini) && geometry.light {
                    let tinted = c.CreateMaskBrush()?;
                    tinted.SetMask(&image_brush)?;
                    tinted.SetSource(&c.CreateColorBrushWithColor(rgba(27, 34, 45, 255))?)?;
                    tinted.cast()?
                } else {
                    image_brush.cast()?
                };
            add_visual(
                c,
                content,
                &brush,
                start,
                (pill_height - icon_size as f32) / 2.0,
                icon_size as f32,
                icon_size as f32,
            )?;
        }
        if let Some(label) = label {
            add_visual(
                c,
                content,
                &label.brush.cast()?,
                start + icon_size as f32 + gap,
                (pill_height - label.height) / 2.0,
                label.width,
                label.height,
            )?;
        }
    }
    Ok(anchors)
}

fn track_title(track: &Track) -> &str {
    if track.title.trim().is_empty() {
        strings::text().media_title
    } else {
        &track.title
    }
}

fn draw_compact_player(
    gfx: &Gfx,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    width: f32,
    height: f32,
    track: &Track,
    cover: Option<&CompositionSurfaceBrush>,
) -> Result<()> {
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let side = px(18.0).min(height).min(width);
    let x = px(23.0) + side;
    let show_title = width > x + px(18.0);
    let note_x = if show_title {
        px(18.0)
    } else {
        (width - side) / 2.0
    };
    if let Some(cover) = cover {
        add_visual(
            c,
            layers,
            &cover.cast()?,
            note_x,
            (height - side) / 2.0,
            side,
            side,
        )?
        .SetOpacity(if track.playing { 1.0 } else { 0.7 })?;
    }
    if show_title {
        let label = gfx.text_brush_fitted(
            track_title(track),
            geometry.scale,
            11.5,
            true,
            ink,
            width - x - px(13.0),
        )?;
        add_visual(
            c,
            layers,
            &label.brush.cast()?,
            x,
            (height - label.height) / 2.0,
            label.width,
            label.height,
        )?;
    }
    Ok(())
}

struct BubbleMusic {
    artwork_size: f32,
    label: LabelBrush,
    width: f32,
}

fn prepare_bubble_music(gfx: &Gfx, look: &Look, track: &Track) -> Result<BubbleMusic> {
    let px = |v: f32| (v * look.scale).round();
    let ink = if look.light { Ink::Dark } else { Ink::Light };
    let artwork_size = px(18.0);
    let left_pad = px(10.0);
    let gap = px(6.0);
    let right_pad = px(8.0);
    let max_text =
        (px(BUBBLE_MUSIC_WIDTH) - left_pad - artwork_size - gap - right_pad).max(px(18.0));
    let label = gfx.text_brush_fitted(track_title(track), look.scale, 11.5, true, ink, max_text)?;
    let width = (left_pad + artwork_size + gap + label.width + right_pad)
        .min(px(BUBBLE_MUSIC_WIDTH))
        .ceil();
    Ok(BubbleMusic {
        artwork_size,
        label,
        width,
    })
}

fn draw_bubble_player(
    gfx: &Gfx,
    layers: &VisualCollection,
    geometry: IslandGeometry,
    height: f32,
    track: &Track,
    prepared: &BubbleMusic,
    cover: Option<&CompositionSurfaceBrush>,
) -> Result<()> {
    let width = geometry.width;
    let c = &gfx.compositor;
    let px = |v| geometry.px(v);
    let ink = geometry.ink();
    let side = prepared.artwork_size.min(height).min(width);
    let left_pad = px(10.0);
    let gap = px(6.0);
    let right_pad = px(8.0);
    let text_x = left_pad + side + gap;
    let max_label_width = width - text_x - right_pad;
    let show_title = max_label_width >= px(18.0);
    let note_x = if show_title {
        left_pad
    } else {
        (width - side) / 2.0
    };
    if let Some(cover) = cover {
        add_visual(
            c,
            layers,
            &cover.cast()?,
            note_x,
            (height - side) / 2.0,
            side,
            side,
        )?
        .SetOpacity(if track.playing { 1.0 } else { 0.7 })?;
    }
    if show_title {
        let fitted;
        let label = if prepared.label.width <= max_label_width {
            &prepared.label
        } else {
            fitted = gfx.text_brush_fitted(
                track_title(track),
                geometry.scale,
                11.5,
                true,
                ink,
                max_label_width,
            )?;
            &fitted
        };
        add_visual(
            c,
            layers,
            &label.brush.cast()?,
            text_x,
            (height - label.height) / 2.0,
            label.width,
            label.height,
        )?;
    }
    Ok(())
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        self.accessories = None;
        self.exit_animation = None;
        self.media = None;
        ISLAND_HOOK_WINDOW.store(0, Ordering::Relaxed);
        edge_watch::set_menu_bar(None);
        if let Some(hook) = self.outside_click_hook.take() {
            // SAFETY: Do not leave a callback pointing at a destroyed window.
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        // Give the reserved strip back before the window goes away.
        self.appbar = None;
        // SAFETY: Our own window, destroyed once.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod island_tests {
    use super::*;

    #[test]
    #[ignore = "Native popup layout/input regression; hidden window, no audio writes"]
    fn native_popup_overflow_and_layout_changes_keep_input_in_sync() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        // SAFETY: All composition objects stay on this test's STA thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.audio = None;
        let mut look = bar.look.clone().context("initial look")?;
        look.screen = RECT {
            left: -1920,
            top: -768,
            right: 0,
            bottom: 0,
        };
        look.track = None;
        look.panel = Some(Island::Usage);
        look.usage_enabled = true;
        look.active_providers.0.fill(true);
        for provider in Provider::ALL {
            look.usage.providers[provider as usize] = ProviderUsage {
                state: State::Ready,
                windows: [300, 10_080]
                    .into_iter()
                    .map(|minutes| usage::Window {
                        label: None,
                        minutes,
                        used_percent: 30,
                        resets_at: None,
                    })
                    .collect(),
                fetched_at: Some(std::time::SystemTime::now()),
            };
        }
        let opencode = &mut look.usage.providers[Provider::OpenCode as usize];
        opencode.windows.push(usage::Window {
            label: None,
            minutes: 43_200,
            used_percent: 10,
            resets_at: None,
        });
        for style in [IslandStyle::Notch, IslandStyle::Bubble] {
            look.widgets.island_style = style;
            for scale in [1.0, 1.5, 2.0] {
                look.scale = scale;
                for light in [false, true] {
                    look.light = light;
                    bar.usage_scroll = 0.0;
                    bar.build(&gfx, &look)?;
                    assert!(bar.visual_height <= 769, "popup must fit a 768 px display");
                    if scale == 1.0 {
                        assert!(bar.usage_viewport.is_none());
                        continue;
                    }
                    let viewport = bar
                        .usage_viewport
                        .as_ref()
                        .context("overflow needs a viewport")?;
                    let bounds = viewport.bounds;
                    let maximum = viewport.maximum;
                    assert!(maximum > 0.0);
                    let body = viewport.content.clone();
                    let down = viewport.down.clone();
                    let root = bar.surface.Children()?.First()?.Current()?;
                    for _ in 0..20 {
                        assert!(bar.scroll_usage(48.0)?);
                    }
                    assert_eq!(bar.surface.Children()?.First()?.Current()?, root);
                    assert_eq!(bar.usage_scroll, maximum);
                    assert_eq!(body.Offset()?.Y, -(maximum * scale).round());
                    assert_eq!(down.Opacity()?, 0.22);
                    let up = bar
                        .island_hits
                        .iter()
                        .find(|hit| matches!(hit.action, IslandAction::ScrollUsage(-1)))
                        .context("scroll up button")?;
                    let (left, top, right, bottom) = up.bounds;
                    assert!(
                        left >= bounds.0
                            && top >= bounds.1
                            && right <= bounds.2
                            && bottom <= bounds.3
                    );
                    bar.click(&gfx, (left + right) / 2.0, (top + bottom) / 2.0)?;
                    assert!(bar.usage_scroll < maximum);
                    assert_eq!(bar.surface.Children()?.First()?.Current()?, root);
                    assert!(bar.scroll_usage(-10_000.0)?);
                    assert_eq!(bar.usage_scroll, 0.0);
                }
            }
        }
        // A busy refresh is not another active input target.
        look.usage_refreshing = true;
        bar.build(&gfx, &look)?;
        assert!(
            !bar.island_hits
                .iter()
                .any(|hit| matches!(hit.action, IslandAction::Refresh))
        );
        look.usage_refreshing = false;
        bar.scroll_usage(10_000.0)?;
        look.active_providers.0.fill(false);
        look.active_providers.0[Provider::Codex as usize] = true;
        bar.build(&gfx, &look)?;
        assert_eq!(bar.usage_scroll, 0.0); // A refresh that removes rows clamps the offset.
        assert!(bar.usage_viewport.is_none());

        look.panel = Some(Island::Music);
        look.track = Some(Track::test_fixture());
        bar.panel = look.panel;
        bar.status.volume = Some(Volume {
            level: 2,
            muted: false,
            percent: 50,
        });
        bar.apply_look(&gfx, look.clone())?;
        bar.apply_volume_reading(
            &gfx,
            Some(Volume {
                level: 0,
                muted: true,
                percent: 0,
            }),
        )?;
        // Mute/glyph changes render the current snapshot; a full refresh would
        // replace this monitor and scan live processes/system status again.
        assert!(
            bar.look
                .as_ref()
                .is_some_and(|current| current.screen == look.screen
                    && current.status.volume.is_some_and(|volume| volume.muted))
        );
        let span = bar
            .island_hits
            .iter()
            .find_map(|hit| match hit.action {
                IslandAction::Slider(Slider::Volume, span) => Some(span),
                _ => None,
            })
            .context("slider")?;
        bar.slider_drag = Some((Slider::Volume, span)); // Never capture the user's mouse in a test.
        bar.queue_slider(Slider::Volume, span, span.left);
        look.scale = 1.5;
        bar.apply_look(&gfx, look.clone())?;
        assert!(
            bar.slider_drag.is_none()
                && bar.pending_volume.is_none()
                && bar.volume_target.is_none()
        );
        assert!(bar.release_volume(Some(span.right))?); // Late up must not activate another button.
        let span = bar
            .island_hits
            .iter()
            .find_map(|hit| match hit.action {
                IslandAction::Slider(Slider::Volume, span) => Some(span),
                _ => None,
            })
            .context("new slider")?;
        bar.slider_drag = Some((Slider::Volume, span));
        bar.queue_slider(Slider::Volume, span, span.right);
        look.active_providers.0.fill(true); // Changes popup placement without changing monitor/DPI.
        bar.apply_look(&gfx, look.clone())?;
        assert!(bar.slider_drag.is_none() && bar.pending_volume.is_none());

        // A trigger that cannot fit must not leave a latent open popup.
        look.screen.right = look.screen.left + 160;
        bar.apply_look(&gfx, look.clone())?;
        assert!(bar.panel.is_none() && bar.look.as_ref().is_some_and(|look| look.panel.is_none()));
        let mut wide = bar.look.clone().context("crowded look")?;
        wide.screen.right = wide.screen.left + 1920;
        bar.apply_look(&gfx, wide)?;
        assert!(bar.panel_span.width() <= 0.0 && bar.usage_viewport.is_none());
        Ok(())
    }

    #[test]
    fn zone_popups_center_under_their_item_and_stay_on_screen() {
        let zone = Span {
            left: 1000.0,
            right: 1030.0,
        };
        let centered = zone_panel_span(zone, 1920.0, 320.0, 8.0);
        assert_eq!(centered.width(), 320.0);
        assert_eq!((centered.left + centered.right) / 2.0, 1015.0);
        let edge = zone_panel_span(
            Span {
                left: 1890.0,
                right: 1910.0,
            },
            1920.0,
            320.0,
            8.0,
        );
        assert_eq!(edge.right, 1912.0);
        let start = zone_panel_span(Span::default(), 1920.0, 320.0, 8.0);
        assert_eq!(start.left, 8.0);
        let narrow = zone_panel_span(zone, 200.0, 320.0, 8.0);
        assert!(narrow.left >= 8.0 && narrow.right <= 192.0);
    }

    #[test]
    fn zone_popups_are_always_available_and_size_to_their_content() {
        assert_eq!(
            available_panel(Some(Island::ControlCenter), false, false),
            Some(Island::ControlCenter)
        );
        assert_eq!(
            available_panel(Some(Island::Sound), false, false),
            Some(Island::Sound)
        );
        assert_eq!(
            toggle_panel(Some(Island::Sound), Island::ControlCenter),
            Some(Island::ControlCenter)
        );
        assert!(control_center_height(true) > control_center_height(false));
        assert!(sound_popup_height(3) > sound_popup_height(1));
        assert_eq!(
            sound_popup_height(100),
            sound_popup_height(SOUND_MAX_OUTPUTS)
        );
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; hidden window, no audio or display writes"]
    fn native_control_center_and_sound_popups_build_in_both_themes() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        // SAFETY: All composition objects stay on this test's STA thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.audio = None;
        bar.status.volume = Some(Volume {
            level: 2,
            muted: false,
            percent: 35,
        });
        bar.control = ControlData {
            wifi: Some(true),
            bluetooth: Some(false),
            ssid: Some("Home".into()),
            dnd: true,
            brightness: Some(60),
            outputs: vec![
                status::AudioOutput {
                    id: "a".into(),
                    name: "Speakers".into(),
                    default: true,
                },
                status::AudioOutput {
                    id: "b".into(),
                    name: "Headphones".into(),
                    default: false,
                },
            ],
            ..ControlData::default()
        };
        // The Sound zone only exists with a volume reading, and both zones
        // need the default bar items.
        let mut look = bar.look.clone().context("initial look")?;
        look.status = bar.status;
        look.control = bar.control.clone();
        bar.apply_look(&gfx, look.clone())?;
        assert!(bar.zones.iter().any(|zone| zone.2 == Action::ControlCenter));
        for island in [Island::ControlCenter, Island::Sound] {
            for track in [None, Some(Track::test_fixture())] {
                for (light, glass) in [(true, true), (false, true), (true, false), (false, false)] {
                    for scale in [1.0, 1.5] {
                        look.panel = Some(island);
                        look.track = track.clone();
                        look.light = light;
                        look.glass = glass;
                        look.scale = scale;
                        bar.panel = look.panel;
                        bar.build(&gfx, &look)?;
                        if !bar
                            .zones
                            .iter()
                            .any(|zone| Some(zone.2) == island.zone_action())
                        {
                            continue; // Hidden item, hence no popup.
                        }
                        assert!(bar.panel_span.width() > 0.0, "{island:?} must open");
                        assert!(bar.zone_span.width() > 0.0);
                        assert!(
                            bar.panel_span.right <= (look.screen.right - look.screen.left) as f32
                        );
                        assert!(bar.island_hits.iter().any(|hit| matches!(
                            hit.action,
                            IslandAction::Slider(Slider::Volume, _)
                        )));
                        assert_eq!(
                            bar.island_hits.iter().any(|hit| matches!(
                                hit.action,
                                IslandAction::Slider(Slider::Brightness, _)
                            )),
                            island == Island::ControlCenter
                        );
                        let wanted = if island == Island::Sound {
                            IslandAction::SoundSettings
                        } else {
                            IslandAction::OpenNetwork
                        };
                        assert!(bar.island_hits.iter().any(|hit| std::mem::discriminant(
                            &hit.action
                        ) == std::mem::discriminant(
                            &wanted
                        )));
                    }
                }
            }
        }
        // Without brightness there is no display slider and the popup shrinks.
        look.panel = Some(Island::ControlCenter);
        look.control.brightness = None;
        bar.build(&gfx, &look)?;
        assert!(
            !bar.island_hits
                .iter()
                .any(|hit| matches!(hit.action, IslandAction::Slider(Slider::Brightness, _)))
        );
        // Closing leaves no stale zone for the outside-click hook.
        look.panel = None;
        bar.panel = None;
        bar.build(&gfx, &look)?;
        assert!(bar.zone_span.width() <= 0.0 && bar.panel_span.width() <= 0.0);
        Ok(())
    }

    #[test]
    fn tray_grid_has_four_columns_and_grows_by_rows() {
        assert_eq!(tray_rows(0), 0);
        assert_eq!(tray_rows(1), 1);
        assert_eq!(tray_rows(4), 1);
        assert_eq!(tray_rows(5), 2);
        assert_eq!(tray_rows(1000), TRAY_MAX_ICONS / TRAY_MAX_COLUMNS);
        assert_eq!(tray_cell(0), (TRAY_PAD, TRAY_PAD));
        assert_eq!(tray_cell(3), (TRAY_PAD + 3.0 * TRAY_CELL, TRAY_PAD));
        assert_eq!(tray_cell(4), (TRAY_PAD, TRAY_PAD + TRAY_CELL));
        assert_eq!(
            tray_cell(9),
            (TRAY_PAD + TRAY_CELL, TRAY_PAD + 2.0 * TRAY_CELL)
        );
        assert_eq!(tray_columns(0), 4);
        assert_eq!(tray_columns(1), 4);
        assert_eq!(tray_columns(100), 4);
        assert_eq!(tray_width(0), 4.0 * TRAY_CELL + 2.0 * TRAY_PAD);
        assert_eq!(tray_width(8), 4.0 * TRAY_CELL + 2.0 * TRAY_PAD);
        assert_eq!(
            tray_popup_height(5),
            TRAY_PAD + 2.0 * TRAY_CELL + TRAY_TIP_HEIGHT + TRAY_FOOTER_HEIGHT
        );
        assert!(tray_popup_height(0) < tray_popup_height(1));
        assert_eq!(tray_popup_height(4), tray_popup_height(1));
        assert_eq!(tray_popup_height(100), tray_popup_height(TRAY_MAX_ICONS));
        // The last cell of a full row stays inside the popup.
        assert!(tray_cell(3).0 + TRAY_CELL + TRAY_PAD <= tray_width(4));
    }

    #[test]
    fn the_tray_popup_is_a_zone_popup_on_the_hidden_icons_arrow() {
        assert!(Island::Tray.zone_action() == Some(Action::HiddenIcons));
        assert_eq!(
            available_panel(Some(Island::Tray), false, false),
            Some(Island::Tray)
        );
        assert!(!Island::Tray.has_sliders());
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; hidden window, no tray relay"]
    fn native_tray_popup_builds_with_no_icons_and_with_many() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        // SAFETY: All composition objects stay on this test's STA thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.audio = None;
        let fake = |index: u32| {
            let side = 16 + index % 3 * 8;
            TrayEntry {
                key: TrayKey::for_test(index),
                name: if index % 4 == 3 {
                    String::new()
                } else {
                    format!("Application number {index}")
                },
                width: side,
                height: side,
                bgra: Arc::new(vec![
                    (index as u8).wrapping_mul(30);
                    (side * side * 4) as usize
                ]),
            }
        };
        let mut look = bar.look.clone().context("initial look")?;
        for count in [0_u32, 8, 20] {
            for (light, glass, hover) in [
                (true, true, Some(0)),
                (false, true, None),
                (true, false, Some(0)),
                (false, false, None),
            ] {
                for scale in [1.0, 1.5] {
                    look.control.tray = (0..count).map(fake).collect();
                    look.control.tray_hover = hover.filter(|_| count > 0);
                    look.panel = Some(Island::Tray);
                    look.light = light;
                    look.glass = glass;
                    look.scale = scale;
                    bar.control = look.control.clone();
                    bar.panel = look.panel;
                    bar.build(&gfx, &look)?;
                    if !bar.zones.iter().any(|zone| zone.2 == Action::HiddenIcons) {
                        continue; // The item is hidden, hence no popup.
                    }
                    assert!(bar.panel_span.width() > 0.0, "the tray popup must open");
                    assert!(bar.panel_span.right <= (look.screen.right - look.screen.left) as f32);
                    let tiles: Vec<usize> = bar
                        .island_hits
                        .iter()
                        .filter_map(|hit| match hit.action {
                            IslandAction::TrayIcon(index) => Some(index),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(tiles, (0..count as usize).collect::<Vec<_>>());
                    assert!(
                        bar.island_hits
                            .iter()
                            .any(|hit| matches!(hit.action, IslandAction::TraySettings))
                    );
                    assert_eq!(bar.tray_brushes.len(), count as usize);
                }
            }
        }
        // Closing leaves no stale zone for the outside-click hook.
        look.panel = None;
        bar.panel = None;
        bar.build(&gfx, &look)?;
        assert!(bar.zone_span.width() <= 0.0 && bar.panel_span.width() <= 0.0);
        Ok(())
    }

    #[test]
    fn volume_coordinates_clamp_and_round_at_both_ends() {
        let span = Span {
            left: 100.0,
            right: 300.0,
        };
        for (x, expected) in [
            (-100.0, 0),
            (100.0, 0),
            (200.0, 50),
            (299.0, 100),
            (500.0, 100),
            (f32::NAN, 0),
        ] {
            assert_eq!(volume_percent(span, x), expected);
        }
        assert_eq!(volume_percent(Span::default(), 1.0), 0);
    }

    #[test]
    fn percentage_changes_keep_the_same_indicator_but_mute_and_device_changes_do_not() {
        let volume = Volume {
            level: 2,
            muted: false,
            percent: 40,
        };
        assert!(same_volume_indicator(
            Some(volume),
            Some(Volume {
                percent: 60,
                ..volume
            })
        ));
        assert!(!same_volume_indicator(
            Some(volume),
            Some(Volume {
                muted: true,
                ..volume
            })
        ));
        assert!(!same_volume_indicator(
            Some(volume),
            Some(Volume { level: 3, ..volume })
        ));
        assert!(!same_volume_indicator(Some(volume), None));
        assert!(!same_volume_indicator(None, Some(volume)));
    }

    #[test]
    #[ignore = "Native compositor coverage; hidden test window, no audio writes"]
    fn native_music_artwork_and_volume_fit_both_styles_and_release_drag_state() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.audio = None; // This fixture can never change the user's audio.
        bar.status.volume = Some(Volume {
            level: 2,
            muted: false,
            percent: 42,
        });
        let mut look = bar.look.clone().context("initial look")?;
        look.status = bar.status;
        look.panel = Some(Island::Music);
        look.track = Some(Track::test_fixture());
        for style in [IslandStyle::Notch, IslandStyle::Bubble] {
            look.widgets.island_style = style;
            for scale in [1.0, 1.25, 1.5, 2.0] {
                look.scale = scale;
                for (light, glass) in [(true, true), (false, true), (true, false), (false, false)] {
                    look.light = light;
                    look.glass = glass;
                    bar.build(&gfx, &look)?;
                    assert!(bar.cover.is_some());
                    let brush = bar.cover.as_ref().context("missing cover")?.1.clone();
                    bar.build(&gfx, &look)?;
                    assert_eq!(bar.cover.as_ref().context("cached cover")?.1, brush);
                    let volume = bar
                        .island_hits
                        .iter()
                        .find(|hit| matches!(hit.action, IslandAction::Slider(Slider::Volume, _)))
                        .context("missing volume hit")?;
                    let (left, top, right, bottom) = volume.bounds;
                    assert!(left >= bar.panel_span.left && right <= bar.panel_span.right);
                    assert!(
                        top >= HEIGHT * scale
                            && bottom <= (HEIGHT + PANEL_GAP + look.panel_height()) * scale + 1.0
                    );
                    assert!(
                        bar.island_hits
                            .iter()
                            .any(|hit| matches!(hit.action, IslandAction::Mute))
                    );
                }
            }
        }
        let image = Arc::new(muelle_shell::icons::IconImage {
            width: 8,
            height: 8,
            bgra: [20, 40, 80, 255].repeat(64),
        });
        look.track.as_mut().context("track")?.artwork = Some(image.clone());
        bar.build(&gfx, &look)?;
        assert!(
            bar.cover
                .as_ref()
                .and_then(|(source, _)| source.as_ref())
                .is_some_and(|source| Arc::ptr_eq(source, &image))
        );
        // Compare the old full redraw path with retained updates without ever
        // writing the real audio endpoint. Tree identity is the regression
        // assertion; timings are informational, not a flaky speed threshold.
        let started = Instant::now();
        for step in 0..64 {
            bar.status.volume = Some(Volume {
                level: 2,
                muted: false,
                percent: 40 + step % 20,
            });
            look.status = bar.status;
            bar.build(&gfx, &look)?;
        }
        let full_redraw = started.elapsed();
        bar.look = Some(look.clone());
        let root = bar.surface.Children()?.First()?.Current()?;
        let started = Instant::now();
        for step in 0..64 {
            bar.apply_volume_reading(
                &gfx,
                Some(Volume {
                    level: 2,
                    muted: false,
                    percent: 40 + step % 20,
                }),
            )?;
            assert_eq!(bar.surface.Children()?.First()?.Current()?, root);
            assert_eq!(
                bar.volume_controls
                    .as_ref()
                    .context("retained volume")?
                    .label_percent,
                40 + step % 20
            );
        }
        eprintln!(
            "64 volume updates: full redraw {:?}, retained {:?} (no audio writes)",
            full_redraw,
            started.elapsed()
        );
        let span = Span {
            left: 100.0,
            right: 300.0,
        };
        bar.slider_drag = Some((Slider::Volume, span)); // No SetCapture in a test.
        bar.queue_slider(Slider::Volume, span, 140.0);
        bar.pointer_moved(&gfx, 260.0, 0.0)?;
        assert_eq!(bar.pending_volume, Some(80));
        assert_eq!(
            bar.volume_controls
                .as_ref()
                .context("immediate percentage")?
                .label_percent,
            80
        );
        assert_eq!(
            bar.volume_controls
                .as_ref()
                .context("immediate thumb")?
                .position_percent,
            Some(80)
        );
        // An endpoint acknowledgement must not drag the thumb backwards while
        // a newer pointer target is still waiting for its coalesced write.
        bar.apply_volume_reading(
            &gfx,
            Some(Volume {
                level: 2,
                muted: false,
                percent: 50,
            }),
        )?;
        assert_eq!(
            bar.volume_controls
                .as_ref()
                .context("optimistic thumb")?
                .position_percent,
            Some(80)
        );
        assert_eq!(bar.surface.Children()?.First()?.Current()?, root);
        assert!(bar.release_volume(Some(300.0))?);
        assert!(bar.pending_volume.is_none() && bar.slider_drag.is_none());
        assert!(!bar.release_volume(None)?);
        assert!(bar.release_volume(Some(200.0))?); // Late up after capture loss/timer.
        assert!(!bar.release_volume(Some(200.0))?);
        bar.status.volume = None;
        bar.build(&gfx, &look)?;
        assert!(!bar.island_hits.iter().any(|hit| matches!(
            hit.action,
            IslandAction::Slider(Slider::Volume, _) | IslandAction::Mute
        )));
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows composition; hidden window and no provider requests"]
    fn opencode_only_measured_go_appears_in_both_island_styles() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        // SAFETY: The dispatcher and hidden composition objects belong to this thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, false, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        let mut look = bar.look.clone().context("initial look")?;
        look.track = None;
        look.active_providers = ActiveProviders::default();
        look.active_providers.0[Provider::OpenCode as usize] = true;
        // Yesterday's saved quota is not evidence that Codex is running now.
        look.usage.providers[Provider::Codex as usize] = ProviderUsage {
            state: State::Ready,
            windows: vec![usage::Window {
                label: None,
                minutes: 300,
                used_percent: 42,
                resets_at: None,
            }],
            fetched_at: Some(std::time::SystemTime::now() - Duration::from_secs(24 * 60 * 60)),
        };
        let quota: Vec<_> = [300, 10_080, 43_200]
            .into_iter()
            .map(|minutes| usage::Window {
                label: None,
                minutes,
                used_percent: 15,
                resets_at: Some(chrono::Utc::now() + chrono::Duration::hours(2)),
            })
            .collect();
        for style in [IslandStyle::Notch, IslandStyle::Bubble] {
            look.widgets.island_style = style;
            for scale in [1.0, 1.25, 1.5, 2.0] {
                look.scale = scale;
                for (light, glass) in [(true, true), (false, true), (true, false), (false, false)] {
                    look.light = light;
                    look.glass = glass;
                    look.panel = Some(Island::Usage);
                    look.usage_enabled = true;
                    look.usage.providers[Provider::OpenCode as usize] = ProviderUsage::default();
                    bar.apply_look(&gfx, look.clone())?;
                    assert!(bar.island_hits.is_empty() && bar.panel.is_none());
                    assert_eq!(bar.triggers_span.width(), 0.0);
                    look.usage.providers[Provider::OpenCode as usize] = ProviderUsage {
                        state: State::Ready,
                        windows: quota.clone(),
                        fetched_at: Some(std::time::SystemTime::now()),
                    };
                    bar.apply_look(&gfx, look.clone())?;
                    let current = bar.look.as_ref().context("rendered Go")?;
                    let providers = island_providers(current);
                    assert_eq!(providers.len(), 1);
                    assert!(providers.iter().all(|entry| entry.0 == Provider::OpenCode));
                    assert_eq!(
                        current.panel_content_height(),
                        74.0 + ISLAND_ROW_HEIGHT + 26.0
                    );
                    assert!(bar.panel_span.width() > 0.0);
                    assert!(
                        bar.island_hits
                            .iter()
                            .any(|hit| matches!(hit.action, IslandAction::Toggle(Island::Usage)))
                    );
                    assert!(
                        bar.island_hits
                            .iter()
                            .any(|hit| matches!(hit.action, IslandAction::Refresh))
                    );
                }
            }
        }
        // Removing Go credentials/quota closes its open popup and removes the trigger.
        look.usage.providers[Provider::OpenCode as usize] = ProviderUsage {
            state: State::SignIn,
            ..ProviderUsage::default()
        };
        bar.apply_look(&gfx, look)?;
        assert!(bar.panel.is_none() && bar.panel_span.width() == 0.0);
        assert!(bar.island_hits.is_empty() && bar.usage_viewport.is_none());
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; keeps the bar hidden"]
    fn accessory_opt_out_blocks_startup_resume_and_retry() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        assert!(!bar.accessories_enabled && bar.accessories.is_none());
        bar.restart_accessories(&gfx)?;
        bar.tick(&gfx)?;
        assert!(bar.accessories.is_none());
        bar.set_accessories_enabled(&gfx, true)?;
        assert!(bar.accessories.is_some());
        bar.set_accessories_enabled(&gfx, false)?;
        bar.accessory_retry_at = Instant::now() - Duration::from_secs(1);
        bar.restart_accessories(&gfx)?;
        bar.tick(&gfx)?;
        assert!(!bar.accessories_enabled && bar.accessories.is_none());
        assert!(
            bar.look
                .as_ref()
                .is_some_and(|look| look.accessories == [None, None])
        );
        Ok(())
    }

    #[test]
    fn accessory_status_ignores_hidden_metadata_and_hides_incomplete_readings() {
        use muelle_shell::accessories::{Device, State};
        let mut snapshot = AccessorySnapshot {
            state: State::Ready,
            ..Default::default()
        };
        assert_eq!(accessory_readings(&snapshot), [None, None]);
        snapshot.devices = vec![
            Device {
                id: "mouse".into(),
                name: "Mouse".into(),
                kind: AccessoryKind::Mouse,
                percent: Some(0),
                conflicting: false,
                active_audio: false,
            },
            Device {
                id: "headset".into(),
                name: "Headset".into(),
                kind: AccessoryKind::Headset,
                percent: None,
                conflicting: false,
                active_audio: true,
            },
        ];
        let before = accessory_readings(&snapshot);
        assert!(before[0].is_some_and(|reading| reading.percent == Some(0)));
        assert!(before[1].is_some_and(|reading| reading.percent.is_none()));
        snapshot.devices[0].name = "New name".into();
        assert!(accessory_readings(&snapshot) == before);
        for state in [State::Loading, State::Unavailable] {
            snapshot.state = state;
            assert_eq!(accessory_readings(&snapshot), [None, None]);
        }
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; keeps the test bar hidden"]
    fn accessory_indicators_render_without_controls_or_popups() -> Result<()> {
        use anyhow::Context;
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.set_visible(false)?;
        bar.accessories = None;
        let mut look = bar.look.clone().context("missing initial menu bar")?;
        look.track = None;
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for light in [true, false] {
                for glass in [true, false] {
                    look.scale = scale;
                    look.light = light;
                    look.glass = glass;
                    look.accessories = [None, None];
                    bar.build(&gfx, &look)?;
                    let root: ContainerVisual =
                        bar.surface.Children()?.First()?.Current()?.cast()?;
                    let empty_visuals = root.Children()?.Count()?;
                    let controls = bar.zones.len();
                    look.accessories = [
                        Some(AccessoryReading {
                            kind: AccessoryKind::Mouse,
                            percent: Some(76),
                        }),
                        Some(AccessoryReading {
                            kind: AccessoryKind::Headset,
                            percent: Some(58),
                        }),
                    ];
                    bar.build(&gfx, &look)?;
                    let root: ContainerVisual =
                        bar.surface.Children()?.First()?.Current()?.cast()?;
                    assert_eq!(root.Children()?.Count()?, empty_visuals + 4);
                    assert_eq!(bar.zones.len(), controls);
                    assert!(bar.island_hits.is_empty());
                    assert!(bar.outside_click_hook.is_none());
                    assert_eq!(bar.panel, None);
                }
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn changing_ui_language_redraws_unchanged_menu_bar_content() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };
        struct RestoreLanguage(Language);
        impl Drop for RestoreLanguage {
            fn drop(&mut self) {
                strings::set_language(self.0);
            }
        }
        let _restore = RestoreLanguage(strings::language());
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        strings::set_language(Language::Es);
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        assert!(
            bar.look
                .as_ref()
                .is_some_and(|look| look.language == Language::Es)
        );
        let previous = bar.surface.Children()?.First()?.Current()?;
        strings::set_language(Language::En);
        bar.refresh(&gfx)?;
        assert_ne!(
            previous,
            bar.surface.Children()?.First()?.Current()?,
            "translation-only changes must rebuild the content"
        );
        assert!(
            bar.look
                .as_ref()
                .is_some_and(|look| look.language == Language::En)
        );
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn auto_hidden_bar_can_reveal_and_hide_repeatedly() -> Result<()> {
        use windows::Win32::{
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::IsWindowVisible,
        };
        // SAFETY: The dispatcher and bar belong to this test thread.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        assert!(
            !unsafe { IsWindowVisible(bar.hwnd) }.as_bool(),
            "auto-hide must apply before the first frame"
        );
        bar.set_auto_hide(true)?;
        for _ in 0..3 {
            assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            assert_eq!(
                bar.surface.Opacity()?,
                0.0,
                "hidden content must stay transparent"
            );
            let point = POINT {
                x: (bar.bounds.left + bar.bounds.right) / 2,
                y: bar.bounds.top,
            };
            bar.edge_rested(&gfx, point)?;
            assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            // A queued leave from before the reveal must not hide it while
            // the pointer is still at the edge, including its first pixel.
            bar.pointer_left_at(&gfx, point)?;
            assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            let surface = bar.surface.clone();
            bar.set_app_name(&gfx, "Muelle regression test")?;
            assert_eq!(
                bar.surface, surface,
                "redraws must preserve the animation parent"
            );
            let outside = POINT {
                y: bar.bounds.bottom,
                ..point
            };
            bar.pointer_left_at(&gfx, outside)?;
            if bar.hide_until.is_some() {
                // Returning during the exit cancels it. Its already queued
                // completion must not close the bar after that reversal.
                bar.pointer_left_at(&gfx, point)?;
                let old_epoch = bar.animation_epoch;
                bar.finish_hide_at(
                    &gfx,
                    Instant::now() + Duration::from_secs(1),
                    outside,
                    Some(old_epoch.wrapping_sub(1)),
                )?;
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
                bar.pointer_left_at(&gfx, outside)?;
                // An old completion must also be ignored during a newer exit.
                bar.finish_hide_at(
                    &gfx,
                    Instant::now() + Duration::from_secs(1),
                    outside,
                    Some(old_epoch),
                )?;
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
                let epoch = bar.animation_epoch;
                bar.finish_hide_at(&gfx, Instant::now(), outside, None)?;
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
                bar.finish_hide_at(&gfx, Instant::now(), outside, Some(epoch))?;
            }
        }
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        bar.set_auto_hide(false)?;
        assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        bar.set_auto_hide(true)?;
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        bar.edge_rested(
            &gfx,
            POINT {
                x: bar.bounds.right / 2,
                y: bar.bounds.top,
            },
        )?;
        bar.set_visible(false)?;
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        assert!(bar.hide_until.is_none());
        bar.set_visible(true)?;
        assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
        Ok(())
    }

    #[test]
    #[ignore = "Requires Windows desktop composition; runs visible transitions"]
    fn compositor_completion_hides_only_the_current_exit() -> Result<()> {
        use windows::Win32::{
            System::WinRT::{
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
            UI::WindowsAndMessaging::{
                DispatchMessageW, IsWindowVisible, MSG, PM_REMOVE, PeekMessageW,
            },
        };
        // SAFETY: The dispatcher, queue, window and compositor are test-owned.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let mut bar = MenuBar::create(&gfx, false, true, true, MenuBarWidgets::default())?;
        bar.set_app_name(&gfx, "Muelle animation test")?;
        bar.set_auto_hide(true)?;
        let inside = POINT {
            x: bar.bounds.left + 100,
            y: bar.bounds.top,
        };
        let outside = POINT {
            y: bar.bounds.bottom + 50,
            ..inside
        };
        let pump = |bar: &mut MenuBar, duration: Duration, point: POINT| -> Result<usize> {
            let until = Instant::now() + duration;
            let mut completions = 0;
            let mut msg = MSG::default();
            while Instant::now() < until {
                // Pump the dispatcher so real compositor completion events run.
                // Pointer state is deterministic; no mouse input is injected.
                while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                    if msg.hwnd == bar.hwnd && msg.message == HIDE_ANIMATION_DONE {
                        completions += 1;
                        bar.finish_hide_at(&gfx, Instant::now(), point, Some(msg.wParam.0))?;
                    } else if msg.hwnd == bar.hwnd
                        && msg.message == WM_TIMER
                        && msg.wParam.0 == HIDE_ANIMATION_TIMER
                    {
                        bar.finish_hide_at(&gfx, Instant::now(), point, None)?;
                    } else {
                        unsafe { DispatchMessageW(&msg) };
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(completions)
        };
        pump(&mut bar, Duration::from_millis(300), outside)?;
        for _ in 0..3 {
            bar.hover(&gfx, true)?;
            pump(&mut bar, Duration::from_millis(500), inside)?;
            assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            bar.hover(&gfx, false)?;
            if system::animations_enabled() {
                assert!(unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            }
            let completions = pump(&mut bar, Duration::from_millis(500), outside)?;
            if system::animations_enabled() {
                assert_eq!(completions, 1, "exit must complete through the compositor");
            }
            assert!(!unsafe { IsWindowVisible(bar.hwnd) }.as_bool());
            assert_eq!(bar.surface.Opacity()?, 0.0);
        }
        Ok(())
    }

    #[test]
    fn clicking_usage_replaces_music_and_clicking_again_closes_it() {
        let open = toggle_panel(Some(Island::Music), Island::Usage);
        assert_eq!(open, Some(Island::Usage));
        assert_eq!(toggle_panel(open, Island::Usage), None);
    }

    #[test]
    fn usage_panel_covers_the_last_provider_even_when_the_trigger_is_wide() {
        let screen = 1920.0;
        let triggers = trigger_layout(
            Span {
                left: 210.0,
                right: 1540.0,
            },
            screen / 2.0,
            1.25,
            true,
            5,
        );
        let Some(usage) = triggers[1].1 else {
            panic!("Usage trigger should fit");
        };
        let panel = panel_span(usage, screen, 550.0);
        assert!(panel.left <= usage.left && panel.right >= usage.right);
    }

    #[test]
    fn narrow_bar_still_gives_each_trigger_its_own_clickable_span() {
        let bounds = Span {
            left: 200.0,
            right: 390.0,
        };
        let [(_, Some(music)), (_, Some(usage))] = trigger_layout(bounds, 300.0, 1.5, true, 3)
        else {
            panic!("Both triggers should fit");
        };
        assert!(
            music.left >= bounds.left && music.right < usage.left && usage.right <= bounds.right
        );
    }

    #[test]
    fn bubble_layout_aligns_to_right_status_edge_and_stays_compact() {
        let bounds = Span {
            left: 210.0,
            right: 1420.0,
        };
        let [(_, Some(short_music)), (_, Some(short_usage))] =
            bubble_trigger_layout(bounds, 1.0, Some(92.0), 2)
        else {
            panic!("Both bubble segments should fit");
        };
        let [(_, Some(long_music)), (_, Some(long_usage))] =
            bubble_trigger_layout(bounds, 1.0, Some(240.0), 2)
        else {
            panic!("Both bubble segments should fit");
        };
        assert_eq!(short_usage.right, bounds.right);
        assert_eq!(long_usage.right, bounds.right);
        assert!(short_music.width() < long_music.width());
        assert_eq!(long_music.width(), BUBBLE_MUSIC_WIDTH);
        assert!(short_music.left >= bounds.left && short_music.right < short_usage.left);

        let short_bubble = Span {
            left: short_music.left,
            right: short_usage.right,
        };
        let long_bubble = Span {
            left: long_music.left,
            right: long_usage.right,
        };
        let short_panel = bubble_panel_span(short_bubble, 1920.0, 360.0);
        let long_panel = bubble_panel_span(long_bubble, 1920.0, 360.0);
        assert_eq!(short_panel.left, long_panel.left);
        assert_eq!(short_panel.right, long_panel.right);
    }

    #[test]
    fn losing_a_session_closes_its_panel_without_opening_the_other() {
        assert_eq!(available_panel(Some(Island::Music), false, true), None);
        assert_eq!(available_panel(Some(Island::Usage), true, false), None);
    }

    #[test]
    #[ignore = "Requires Windows desktop composition"]
    fn popup_surface_builds_in_both_themes_with_and_without_transparency() -> Result<()> {
        use windows::Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        };

        // SAFETY: The controller initializes the test thread's own queue.
        let _queue = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        for (light, glass) in [(true, true), (false, true), (true, false), (false, false)] {
            let root = gfx.compositor.CreateContainerVisual()?;
            let theme = Theme::for_popup_with(light, glass, crate::menu::DEFAULT_GLASS_STRENGTH);
            let geometry = IslandGeometry {
                left: 90.0,
                top: HEIGHT + PANEL_GAP,
                width: 360.0,
                scale: 1.0,
                light,
            };
            popup_surface(&gfx, theme, &root.Children()?, geometry, 136.0)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    const SOON: Duration = Duration::from_secs(10);

    #[test]
    fn without_seconds_every_tick_is_a_full_redraw() {
        for shown in [true, false] {
            assert_eq!(clock_tick_plan(false, 17, shown, SOON), ClockTick::Full);
        }
    }

    #[test]
    fn seconds_redraw_only_the_label_while_the_bar_is_shown() {
        assert_eq!(clock_tick_plan(true, 17, true, SOON), ClockTick::LabelOnly);
        assert_eq!(clock_tick_plan(true, 17, false, SOON), ClockTick::Skip);
    }

    #[test]
    fn the_minute_boundary_and_a_missed_one_redraw_everything() {
        for shown in [true, false] {
            assert_eq!(clock_tick_plan(true, 0, shown, SOON), ClockTick::Full);
            assert_eq!(
                clock_tick_plan(true, 33, shown, CLOCK_FULL_EVERY),
                ClockTick::Full
            );
        }
    }

    #[test]
    fn a_label_a_pixel_off_still_fits() {
        assert!(label_fits_slot(50.0, 50.0));
        assert!(label_fits_slot(50.0, 51.0));
        assert!(label_fits_slot(50.0, 49.0));
        assert!(!label_fits_slot(50.0, 52.0));
        assert!(!label_fits_slot(50.0, 48.5));
    }

    #[test]
    fn a_burst_of_tray_changes_arms_the_timer_once() {
        let mut armed = false;
        assert!(arm_tray_flush(&mut armed));
        assert!(!arm_tray_flush(&mut armed));
        assert!(!arm_tray_flush(&mut armed));
        armed = false;
        assert!(arm_tray_flush(&mut armed));
    }
}

#[cfg(test)]
mod mascot_tests {
    use super::*;
    use crate::usage::{ProviderUsage, Snapshot, State, Window};

    fn at(percent: Option<u8>) -> Snapshot {
        let mut snapshot = Snapshot::default();
        if let Some(percent) = percent {
            snapshot.providers[Provider::Claude as usize] = ProviderUsage {
                state: State::Ready,
                windows: vec![Window {
                    label: None,
                    minutes: 300,
                    used_percent: percent,
                    resets_at: None,
                }],
                fetched_at: None,
            };
        }
        snapshot
    }

    fn moment(before: Option<u8>, now: Option<u8>) -> Option<mascot::Act> {
        let mut active = ActiveProviders::default();
        active.0[Provider::Claude as usize] = true;
        usage_moment(&at(before), &at(now), &active).map(|(_, act)| act)
    }

    #[test]
    fn usage_moment_should_fire_only_when_a_line_is_crossed() {
        assert_eq!(moment(Some(60), Some(0)), Some(mascot::Act::Fresh));
        assert_eq!(moment(Some(85), Some(91)), Some(mascot::Act::Nervous));
        assert_eq!(moment(Some(95), Some(100)), Some(mascot::Act::Exhausted));
        // Straight to 100 is "exhausted", not "nervous".
        assert_eq!(moment(Some(50), Some(100)), Some(mascot::Act::Exhausted));
        // Staying above a line, a small reset, or the first figure: nothing.
        assert_eq!(moment(Some(92), Some(95)), None);
        assert_eq!(moment(Some(10), Some(0)), None);
        assert_eq!(moment(None, Some(0)), None);
        assert_eq!(moment(None, Some(100)), None);
    }

    #[test]
    fn usage_moment_ignores_failed_reads_and_different_quota_windows() {
        let before = at(Some(95));
        let mut after = at(Some(0));
        let mut active = ActiveProviders::default();
        active.0[Provider::Claude as usize] = true;
        after.providers[Provider::Claude as usize].windows[0].minutes = 10080;
        assert!(usage_moment(&before, &after, &active).is_none());
        after.providers[Provider::Claude as usize].windows[0].minutes = 300;
        after.providers[Provider::Claude as usize].state = State::Unavailable;
        assert!(usage_moment(&before, &after, &active).is_none());
    }

    #[test]
    fn usage_moment_ignores_out_of_order_measurements() {
        let mut before = at(Some(95));
        let mut after = at(Some(0));
        let mut active = ActiveProviders::default();
        active.0[Provider::Claude as usize] = true;
        let now = std::time::SystemTime::now();
        before.providers[Provider::Claude as usize].fetched_at = Some(now);
        after.providers[Provider::Claude as usize].fetched_at =
            now.checked_sub(std::time::Duration::from_secs(60));
        assert!(usage_moment(&before, &after, &active).is_none());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn bar_items_run_right_to_left_and_skip_invisible() {
        let order = [
            MenuBarItem::Widgets,
            MenuBarItem::Battery,
            MenuBarItem::Wifi,
            MenuBarItem::ControlCenter,
        ];
        assert_eq!(
            bar_items(&order, |item| item != MenuBarItem::Wifi),
            [
                MenuBarItem::ControlCenter,
                MenuBarItem::Battery,
                MenuBarItem::Widgets
            ]
        );
    }

    #[test]
    fn glass_alpha_follows_strength() {
        assert_eq!(glass_alpha(true, 50), 100);
        assert_eq!(glass_alpha(false, 50), 115);
        assert_eq!(glass_alpha(false, 100), 180);
        assert_eq!(glass_alpha(true, 0), 40);
    }
}
