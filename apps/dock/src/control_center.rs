//! The control center: glass tiles under the right end of the menu bar, laid
//! out and moving as macOS's own, for the network, Bluetooth, what is playing,
//! the screen's brightness, the volume and a few shortcuts. A click switches
//! what a tile stands for; holding it (or right-clicking it) grows it into a
//! page with more to choose from, and a click anywhere else goes back.
//!
//! The menu bar's network icon opens the Wi-Fi page by itself, without the
//! tiles: there a click anywhere else closes it.
//!
//! It exists only while open. Opening it starts the watches it needs and one
//! worker thread for the slow reads (radios, the monitor, the lists); closing
//! it drops them all. It never takes the focus from the app in front.

pub(crate) mod motion;
mod page;
mod worker;

use std::time::{Duration, Instant};

use anyhow::Result;
use muelle_config::ControlTiles;
use muelle_shell::{
    controls::{self, RadioSwitch},
    status::{AudioWatch, Network, NetworkWatch, Volume},
    system, tracker,
};
use windows::{
    UI::{
        Color,
        Composition::{
            CompositionRoundedRectangleGeometry, ContainerVisual, Desktop::DesktopWindowTarget,
            ShapeVisual, VisualCollection,
        },
    },
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute},
            Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH},
        },
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, GetCapture, ReleaseCapture, SetCapture, VK_ESCAPE, VK_LBUTTON,
                VK_MBUTTON, VK_RBUTTON,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics,
                HWND_TOPMOST, IDC_ARROW, KillTimer, LWA_ALPHA, LoadCursorW, MA_NOACTIVATE,
                RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes,
                SetTimer, SetWindowPos, WM_APP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
                WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
                WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{BOOL, Interface, w},
};
use windows_numerics::{Vector2, Vector3};

pub use page::Projection;

use crate::{
    app, appearance,
    gfx::{Gfx, Ink},
    media,
    menubar::{add_visual, retained_rounded_box, rgba},
    scene::Theme,
    strings,
};
use motion::Motion;
use page::{Action, Content, Hit, Page, Switch};
use worker::{Command, Lists, Readings, Worker};

const TIMER: usize = 1;
/// How often the pointer and keyboard are checked, only while open.
const TICK_MS: u32 = 30;
/// Pressing a tile this long opens its page.
const HOLD: Duration = Duration::from_millis(380);

/// Posted to the window by the watches and the worker.
pub const AUDIO_CHANGED: u32 = WM_APP + 1;
pub const MEDIA_CHANGED: u32 = WM_APP + 2;
pub const NETWORK_CHANGED: u32 = WM_APP + 3;
pub const READINGS_CHANGED: u32 = WM_APP + 4;

// Logical pixels. Four columns of `CELL`, `GAP` apart.
const CELL: f32 = 64.0;
const GAP: f32 = 12.0;
const GRID: f32 = 4.0 * CELL + 3.0 * GAP;
const TWO: f32 = 2.0 * CELL + GAP;
/// Room around the tiles inside the window.
const MARGIN: f32 = 12.0;
const MORE_HEIGHT: f32 = 28.0;
const HEIGHT: f32 = 6.0 * (CELL + GAP) + MORE_HEIGHT;
/// Distance from the screen's right edge and from the menu bar.
const EDGE: f32 = 12.0;
const DROP: f32 = 8.0;
const CORNER: f32 = 24.0;
const WHITE: Color = rgba(255, 255, 255, 255);

mod glyph {
    pub const WIFI: char = '\u{E701}';
    pub const ETHERNET: char = '\u{E839}';
    pub const OFFLINE: char = '\u{F384}';
    pub const BLUETOOTH: char = '\u{E702}';
    pub const SHARE: char = '\u{E72D}';
    pub const FOCUS: char = '\u{E708}';
    pub const NIGHT: char = '\u{E708}';
    pub const TASK_VIEW: char = '\u{E7C4}';
    pub const CAST: char = '\u{E7F4}';
    pub const BRIGHTNESS: char = '\u{E706}';
    pub const VOLUME_LOW: char = '\u{E992}';
    pub const VOLUME_HIGH: char = '\u{E995}';
    pub const MUTE: char = '\u{E74F}';
    pub const OUTPUT: char = '\u{E7F5}';
    pub const DARK_MODE: char = '\u{E793}';
    pub const CALCULATOR: char = '\u{E8EF}';
    pub const TIMER: char = '\u{E916}';
    pub const SCREENSHOT: char = '\u{E722}';
    pub const PREVIOUS: char = '\u{E892}';
    pub const PLAY: char = '\u{E768}';
    pub const PAUSE: char = '\u{E769}';
    pub const NEXT: char = '\u{E893}';
    pub const NOTIFICATIONS: char = '\u{EA8F}';
    pub const ADD: char = '\u{E710}';
    pub const BACK: char = '\u{E72B}';
    pub const FORWARD: char = '\u{E72A}';
    pub const DESKTOP: char = '\u{E977}';
    pub const EXTEND: char = '\u{E8A0}';
    pub const REGION: char = '\u{E7A8}';
    pub const RECORD: char = '\u{E7C8}';

    /// The Wi-Fi fan for a signal from 0 to 100.
    pub const fn signal(strength: u8) -> char {
        match strength {
            0..25 => '\u{E872}',
            25..50 => '\u{E873}',
            50..75 => '\u{E874}',
            _ => WIFI,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tile {
    Network,
    Media,
    Bluetooth,
    Share,
    Focus,
    TaskView,
    Cast,
    Display,
    Sound,
    DarkMode,
    Calculator,
    Timer,
    Screenshot,
    More,
}

/// Every tile but `More`, in the order they are placed, with its width and
/// height in cells.
const TILES: [(Tile, usize, usize); 13] = [
    (Tile::Network, 2, 1),
    (Tile::Media, 2, 2),
    (Tile::Bluetooth, 1, 1),
    (Tile::Share, 1, 1),
    (Tile::Focus, 2, 1),
    (Tile::TaskView, 1, 1),
    (Tile::Cast, 1, 1),
    (Tile::Display, 4, 1),
    (Tile::Sound, 4, 1),
    (Tile::DarkMode, 1, 1),
    (Tile::Calculator, 1, 1),
    (Tile::Timer, 1, 1),
    (Tile::Screenshot, 1, 1),
];
const COLUMNS: usize = 4;

/// Whether Settings has `tile` shown.
const fn wanted(tile: Tile, shown: ControlTiles) -> bool {
    match tile {
        Tile::Network => shown.network,
        Tile::Media => shown.media,
        Tile::Bluetooth => shown.bluetooth,
        Tile::Share => shown.nearby_sharing,
        Tile::Focus => shown.focus,
        Tile::TaskView => shown.task_view,
        Tile::Cast => shown.cast,
        Tile::Display => shown.display,
        Tile::Sound => shown.sound,
        Tile::DarkMode => shown.dark_mode,
        Tile::Calculator => shown.calculator,
        Tile::Timer => shown.timer,
        Tile::Screenshot => shown.screenshot,
        Tile::More => true,
    }
}

/// Where each tile shown goes within the grid, in logical pixels: left, top,
/// width, height.
struct Grid(Vec<(Tile, [f32; 4])>);

impl Grid {
    /// Each tile takes the first place with room for it, row by row, so
    /// those hidden leave no hole where a later one fits. `More` ends it.
    fn new(shown: ControlTiles) -> Self {
        let step = CELL + GAP;
        // The columns taken in each row, one bit apiece.
        let mut rows: Vec<u8> = Vec::new();
        let mut places = Vec::with_capacity(TILES.len() + 1);
        for (tile, columns, height) in TILES {
            if !wanted(tile, shown) {
                continue;
            }
            let free = |rows: &[u8], row: usize, mask: u8| {
                (row..row + height).all(|r| rows.get(r).is_none_or(|taken| taken & mask == 0))
            };
            let mut row = 0;
            let (column, mask) = loop {
                let found = (0..=COLUMNS - columns)
                    .map(|column| (column, ((1u8 << columns) - 1) << column))
                    .find(|&(_, mask)| free(&rows, row, mask));
                if let Some(found) = found {
                    break found;
                }
                row += 1;
            };
            rows.resize(rows.len().max(row + height), 0);
            for taken in &mut rows[row..row + height] {
                *taken |= mask;
            }
            let span = |cells: usize| cells as f32 * step - GAP;
            places.push((
                tile,
                [
                    column as f32 * step,
                    row as f32 * step,
                    span(columns),
                    span(height),
                ],
            ));
        }
        places.push((
            Tile::More,
            [
                (GRID - 140.0) / 2.0,
                rows.len() as f32 * step,
                140.0,
                MORE_HEIGHT,
            ],
        ));
        Self(places)
    }

    /// The place of `wanted`; empty if it is not shown.
    fn place(&self, wanted: Tile) -> [f32; 4] {
        self.0
            .iter()
            .find(|(tile, _)| *tile == wanted)
            .map_or([0.0; 4], |(_, place)| *place)
    }
}

/// What a click on the grid does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The tile's page.
    Open,
    /// Nothing to switch: only holding does something.
    Nothing,
    /// Turns what the tile stands for on or off.
    Switch(Switch),
    Share,
    TaskView,
    Cast,
    Previous,
    PlayPause,
    Next,
    BrightnessSlider,
    VolumeSlider,
    Mute,
    DarkMode,
    Calculator,
    Timer,
    Screenshot,
    More,
}

/// A slider's track within its tile: left edge, width, and the centre's height.
const BRIGHTNESS_TRACK: [f32; 3] = [38.0, GRID - 76.0, 43.0];
const VOLUME_TRACK: [f32; 3] = [38.0, GRID - 112.0, 43.0];
const MEDIA_CONTROLS: [(Target, f32); 3] = [
    (Target::Previous, 36.0),
    (Target::PlayPause, 70.0),
    (Target::Next, 104.0),
];
const MEDIA_CONTROLS_Y: f32 = 116.0;

/// Something outside the control center to open; it closes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    NetworkList,
    WifiSettings,
    BluetoothSettings,
    NearbySharing,
    Focus,
    Notifications,
    TaskView,
    NewDesktop,
    PreviousDesktop,
    NextDesktop,
    ShowDesktop,
    Cast,
    Project(Projection),
    DisplaySettings,
    NightLight,
    Personalize,
    SoundSettings,
    Calculator,
    Timer,
    Screenshot,
    CaptureScreen,
    RecordScreen,
    SnippingTool,
    QuickSettings,
}

impl Launch {
    /// Whether it takes a picture of the screen, which must not include the
    /// control center.
    pub fn captures(self) -> bool {
        matches!(
            self,
            Self::Screenshot | Self::CaptureScreen | Self::RecordScreen
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Launch(Launch),
}

/// What the periodic check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Keep,
    /// Escape, a press elsewhere, or another app came to the front.
    Close,
    /// The closing animation has ended: the window can go.
    Closed,
}

/// A rectangle in window pixels from one in grid logical pixels.
fn pixels(scale: f32, [left, top, width, height]: [f32; 4]) -> RECT {
    let px = |v: f32| (v * scale).round() as i32;
    RECT {
        left: px(MARGIN + left),
        top: px(MARGIN + top),
        right: px(MARGIN + left + width),
        bottom: px(MARGIN + top + height),
    }
}

/// The same as left, top, width and height.
fn spread(area: RECT) -> [f32; 4] {
    [
        area.left as f32,
        area.top as f32,
        (area.right - area.left) as f32,
        (area.bottom - area.top) as f32,
    ]
}

fn middle([left, top, width, height]: [f32; 4]) -> Vector2 {
    Vector2 {
        X: left + width / 2.0,
        Y: top + height / 2.0,
    }
}

fn contains(rect: RECT, point: POINT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

/// A square of side `side` centred on a point of the tile at `place`.
fn around(place: [f32; 4], x: f32, y: f32, side: f32) -> [f32; 4] {
    [
        place[0] + x - side / 2.0,
        place[1] + y - side / 2.0,
        side,
        side,
    ]
}

/// The tile under `point` (window pixels) and what a click there does.
/// `None` outside every tile, which closes the control center.
fn target_at(grid: &Grid, scale: f32, point: POINT) -> Option<(Tile, Target)> {
    let (tile, place) = grid
        .0
        .iter()
        .find(|(_, place)| contains(pixels(scale, *place), point))?;
    let on = |area: [f32; 4]| contains(pixels(scale, area), point);
    let slider = |[left, width, y]: [f32; 3]| {
        on([
            place[0] + left - 8.0,
            place[1] + y - 14.0,
            width + 16.0,
            28.0,
        ])
    };
    let target = match tile {
        // A click switches; holding opens the page.
        Tile::Network => Target::Switch(Switch::Wifi),
        Tile::Focus => Target::Switch(Switch::Focus),
        Tile::Media => MEDIA_CONTROLS
            .iter()
            .find(|(_, x)| on(around(*place, *x, MEDIA_CONTROLS_Y, 32.0)))
            .map_or(Target::PlayPause, |(target, _)| *target),
        Tile::Bluetooth => Target::Switch(Switch::Bluetooth),
        Tile::Share => Target::Share,
        Tile::TaskView => Target::TaskView,
        Tile::Cast => Target::Cast,
        Tile::Display if slider(BRIGHTNESS_TRACK) => Target::BrightnessSlider,
        Tile::Display => Target::Nothing,
        Tile::Sound if on(around(*place, GRID - 28.0, 43.0, 32.0)) => Target::Open,
        Tile::Sound if on(around(*place, 22.0, 43.0, 24.0)) => Target::Mute,
        Tile::Sound if slider(VOLUME_TRACK) => Target::VolumeSlider,
        Tile::Sound => Target::Mute,
        Tile::DarkMode => Target::DarkMode,
        Tile::Calculator => Target::Calculator,
        Tile::Timer => Target::Timer,
        Tile::Screenshot => Target::Screenshot,
        Tile::More => Target::More,
    };
    Some((*tile, target))
}

/// A slider track's left edge and width in window pixels, for a track within
/// the area at `place`.
fn track_span(scale: f32, place: [f32; 4], [left, width, _]: [f32; 3]) -> (f32, f32) {
    ((MARGIN + place[0] + left) * scale, width * scale)
}

/// The slider value under a window x coordinate.
fn slider_percent((start, length): (f32, f32), x: i32) -> u8 {
    (((x as f32 - start) / length.max(1.0)).clamp(0.0, 1.0) * 100.0).round() as u8
}

/// The scale to draw at, and the window's place under a menu bar whose bottom
/// edge is `top`: against the right of the screen, shrunk if it would not fit.
fn placement(screen: RECT, top: i32, scale: f32) -> (f32, RECT) {
    let available = (screen.bottom - top) as f32;
    let scale = scale.min(available / (HEIGHT + MARGIN + DROP + EDGE));
    let px = |v: f32| (v * scale).round() as i32;
    let (width, height) = (px(GRID + 2.0 * MARGIN), px(HEIGHT + 2.0 * MARGIN));
    let left = (screen.right - px(EDGE - MARGIN) - width).max(screen.left);
    let top = top + px(DROP) - px(MARGIN).min(px(DROP));
    (
        scale,
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        },
    )
}

/// A media button's symbol, and whether the player accepts it.
fn transport(target: Target, track: Option<&media::Track>) -> (char, bool) {
    match target {
        Target::Previous => (glyph::PREVIOUS, track.is_some_and(|t| t.previous)),
        Target::Next => (glyph::NEXT, track.is_some_and(|t| t.next)),
        _ => (
            if track.is_some_and(|t| t.playing) {
                glyph::PAUSE
            } else {
                glyph::PLAY
            },
            track.is_some_and(|t| t.toggle),
        ),
    }
}

/// The parts of a slider that move.
struct Slider {
    fill: CompositionRoundedRectangleGeometry,
    knob: ShapeVisual,
    /// In window pixels.
    left: f32,
    width: f32,
    knob_top: f32,
    knob_width: f32,
}

impl Slider {
    fn move_to(&self, percent: u8) -> Result<()> {
        let filled = self.width * f32::from(percent.min(100)) / 100.0;
        self.fill.SetSize(Vector2::new(
            filled,
            self.fill.Size().map_or(0.0, |size| size.Y),
        ))?;
        let knob = (self.left + filled - self.knob_width / 2.0)
            .clamp(self.left, self.left + self.width - self.knob_width);
        self.knob
            .SetOffset(Vector3::new(knob.round(), self.knob_top, 0.0))?;
        Ok(())
    }
}

/// Everything drawn, to tell whether a change needs a redraw.
#[derive(Clone, PartialEq)]
struct Look {
    appearance: appearance::Look,
    network: Network,
    network_name: Option<String>,
    readings: Readings,
    lists: Lists,
    volume: Option<Volume>,
    track: Option<media::Track>,
    accent: [u8; 3],
    page: Option<Page>,
    /// Seconds played and in all, while the player's page is open.
    position: Option<(u64, u64)>,
}

#[derive(Clone, Copy)]
struct Drag {
    /// Which slider: `BrightnessSlider` or `VolumeSlider`.
    target: Target,
    span: (f32, f32),
    percent: u8,
}

/// The primary button is down on a tile.
#[derive(Clone, Copy)]
struct Held {
    tile: Tile,
    target: Target,
    since: Instant,
}

/// Which of the menu bar's items it hangs from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// The control center's button: the tiles.
    Tiles,
    /// The network icon: the Wi-Fi page alone.
    Network,
}

impl Opening {
    /// The tile whose page opens by itself.
    fn alone(self) -> Option<Tile> {
        match self {
            Self::Tiles => None,
            Self::Network => Some(Tile::Network),
        }
    }
}

/// `bounds` moved left to start under `button`, if the button is further from
/// the screen's edge than the window reaches.
fn under(bounds: RECT, screen: RECT, button: RECT, scale: f32) -> RECT {
    let left = (button.left - (MARGIN * scale).round() as i32)
        .max(screen.left)
        .min(bounds.left);
    RECT {
        left,
        right: left + (bounds.right - bounds.left),
        ..bounds
    }
}

/// An all but invisible window under the control center, over the rest of the
/// screen below the menu bar.
struct Scrim {
    hwnd: HWND,
    pressed: bool,
}

impl Scrim {
    /// Below `above`, from `top` down.
    fn show(above: HWND, top: i32) -> Result<Self> {
        // SAFETY: Class and window belong to this UI thread; the returned
        // value owns the HWND as soon as it exists.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("MuelleControlCenterScrim");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(scrim_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                lpszClassName: class,
                ..Default::default()
            });
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Muelle control center backdrop"),
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
            let scrim = Self {
                hwnd,
                pressed: false,
            };
            // One step from fully clear, so that it still receives clicks.
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA)?;
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let bottom = GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN);
            SetWindowPos(
                hwnd,
                Some(above),
                left,
                top,
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                (bottom - top).max(0),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            Ok(scrim)
        }
    }
}

impl Drop for Scrim {
    fn drop(&mut self) {
        // SAFETY: Our own window.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn scrim_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => app::control_center_scrim(true),
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => app::control_center_scrim(false),
        // SAFETY: Standard forwarding of unhandled messages.
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    LRESULT(0)
}

/// The page that is open, or folding away.
struct Sheet {
    page: Page,
    from: Tile,
    content: Content,
    layout: page::Layout,
    pressed: Option<Hit>,
    leaving: Option<Instant>,
}

pub struct ControlCenter {
    hwnd: HWND,
    target: Option<DesktopWindowTarget>,
    /// Where each tile goes.
    places: Grid,
    /// The grid of tiles, one container each, kept across redraws so that a
    /// redraw does not interrupt their movement.
    grid: ContainerVisual,
    tiles: Vec<ContainerVisual>,
    /// The open page.
    page_layer: ContainerVisual,
    /// While a page is open, catches clicks anywhere else on the screen.
    scrim: Option<Scrim>,
    motion: Motion,
    scale: f32,
    bounds: RECT,
    /// The menu bar's button, on screen: pressing it is the dock's business.
    button: RECT,
    opening: Opening,
    audio: Option<AudioWatch>,
    media: Option<media::Watch>,
    _network: Option<NetworkWatch>,
    worker: Worker,
    network: Network,
    network_name: Option<String>,
    volume: Option<Volume>,
    readings: Readings,
    lists: Lists,
    drawn: Option<Look>,
    brightness_slider: Option<Slider>,
    volume_slider: Option<Slider>,
    page_slider: Option<Slider>,
    drag: Option<Drag>,
    held: Option<Held>,
    sheet: Option<Sheet>,
    /// When the closing animation ends.
    closing: Option<Instant>,
    mouse_was_down: bool,
    escape_was_down: bool,
    foreground: isize,
}

impl ControlCenter {
    /// Opens under the menu bar on `screen`, from the item at `button`.
    pub fn show(
        gfx: &Gfx,
        screen: RECT,
        button: RECT,
        scale: f32,
        opening: Opening,
        shown: ControlTiles,
    ) -> Result<Self> {
        let (scale, mut bounds) = placement(screen, button.bottom, scale);
        if opening.alone().is_some() {
            bounds = under(bounds, screen, button, scale);
        }
        let c = &gfx.compositor;
        let size = Vector2::new(
            (bounds.right - bounds.left) as f32,
            (bounds.bottom - bounds.top) as f32,
        );
        let root = c.CreateContainerVisual()?;
        root.SetSize(size)?;
        let grid = c.CreateContainerVisual()?;
        grid.SetSize(size)?;
        grid.SetCenterPoint(Vector3::new(size.X / 2.0, size.Y / 2.0, 0.0))?;
        let places = Grid::new(shown);
        let mut tiles = Vec::with_capacity(places.0.len());
        for &(_, place) in &places.0 {
            let tile = c.CreateContainerVisual()?;
            tile.SetSize(size)?;
            let center = middle(spread(pixels(scale, place)));
            tile.SetCenterPoint(Vector3::new(center.X, center.Y, 0.0))?;
            grid.Children()?.InsertAtTop(&tile)?;
            tiles.push(tile);
        }
        let page_layer = c.CreateContainerVisual()?;
        page_layer.SetSize(size)?;
        let corner = (MARGIN * scale).round();
        page_layer.SetCenterPoint(Vector3::new(corner, corner, 0.0))?;
        root.Children()?.InsertAtTop(&grid)?;
        root.Children()?.InsertAtTop(&page_layer)?;
        // SAFETY: Class and window belong to this UI thread; `center` owns the
        // HWND as soon as it exists.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("MuelleControlCenter");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(control_center_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            });
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Muelle control center"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };
        let window = hwnd.0 as isize;
        let mut center = Self {
            hwnd,
            target: None,
            places,
            grid,
            tiles,
            page_layer,
            scrim: None,
            motion: Motion::new(c, system::animations_enabled()),
            scale,
            bounds,
            button,
            opening,
            audio: AudioWatch::new(window, AUDIO_CHANGED),
            media: media::Watch::start(hwnd, MEDIA_CHANGED),
            _network: NetworkWatch::new(window, NETWORK_CHANGED),
            worker: Worker::start(window, READINGS_CHANGED),
            network: muelle_shell::status::network(),
            network_name: controls::network_name(),
            volume: None,
            readings: Readings::default(),
            lists: Lists::default(),
            drawn: None,
            brightness_slider: None,
            volume_slider: None,
            page_slider: None,
            drag: None,
            held: None,
            sheet: None,
            closing: None,
            mouse_was_down: true,
            escape_was_down: false,
            foreground: tracker::foreground_window(),
        };
        center.volume = center.audio.as_mut().and_then(AudioWatch::volume);
        center.readings = center.worker.readings();
        let enable = BOOL::from(true);
        // SAFETY: Our own window and a live value of the declared size. Older
        // Windows versions reject it; their theme is opaque anyway.
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                (&raw const enable).cast(),
                size_of::<BOOL>() as u32,
            );
        }
        // SAFETY: This process owns the top-level destination window.
        let target = unsafe {
            c.cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)?
        };
        target.SetRoot(&root)?;
        center.target = Some(target);
        center.refresh(gfx)?;
        match opening.alone() {
            None => center
                .motion
                .open(&center.moving_tiles(), center.origin())?,
            Some(tile) => center.show_alone(gfx, tile)?,
        }
        // SAFETY: Shows our window without taking the focus from the app in front.
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            SetTimer(Some(hwnd), TIMER, TICK_MS, None);
        }
        Ok(center)
    }

    pub fn refresh_backdrops(&self, gfx: &Gfx) -> Result<()> {
        if let Some(target) = &self.target {
            gfx.refresh_backdrops(&target.Root()?.cast()?)?;
        }
        Ok(())
    }

    /// The middle of the menu bar's button, in window pixels: where the tiles
    /// come from.
    fn origin(&self) -> Vector2 {
        Vector2 {
            X: ((self.button.left + self.button.right) / 2 - self.bounds.left) as f32,
            Y: ((self.button.top + self.button.bottom) / 2 - self.bounds.top) as f32,
        }
    }

    fn moving_tiles(&self) -> Vec<(ContainerVisual, Vector2)> {
        self.tiles
            .iter()
            .zip(&self.places.0)
            .map(|(visual, &(_, place))| {
                (visual.clone(), middle(spread(pixels(self.scale, place))))
            })
            .collect()
    }

    pub fn opening(&self) -> Opening {
        self.opening
    }

    /// Whether a page is all there is, with no tiles to go back to.
    fn alone(&self) -> bool {
        self.opening.alone().is_some()
    }

    /// The page of `tile` springs out of the button by itself.
    fn show_alone(&mut self, gfx: &Gfx, tile: Tile) -> Result<()> {
        self.grid.SetIsVisible(false)?;
        self.prepare_page(gfx, tile)?;
        if let Some(sheet) = &self.sheet {
            let center = middle(spread(pixels(self.scale, sheet.layout.area())));
            self.motion
                .appear(&self.page_layer, center, self.origin())?;
        }
        Ok(())
    }

    pub fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Starts folding back into the button. `false` if there is nothing to
    /// wait for, so the window can go at once.
    pub fn begin_close(&mut self) -> Result<bool> {
        if !self.motion.enabled() {
            return Ok(false);
        }
        self.held = None;
        self.drag = None;
        self.scrim = None;
        self.release_capture();
        self.closing = Some(Instant::now() + Duration::from_millis(motion::CLOSE_MS));
        match &self.sheet {
            // The grid is out of sight behind a page.
            Some(sheet) if sheet.leaving.is_none() => {
                let center = middle(spread(pixels(self.scale, sheet.layout.area())));
                self.motion
                    .close(&[(self.page_layer.clone(), center)], self.origin())?;
            }
            _ => self.motion.close(&self.moving_tiles(), self.origin())?,
        }
        Ok(true)
    }

    /// A watch or the worker has news.
    pub fn changed(&mut self, gfx: &Gfx, message: u32) -> Result<()> {
        if self.closing.is_some() {
            return Ok(());
        }
        match message {
            AUDIO_CHANGED => {
                self.volume = self.audio.as_mut().and_then(AudioWatch::volume);
                // The output may be another one now.
                if self.page() == Some(Page::Sound)
                    && self
                        .audio
                        .as_ref()
                        .is_some_and(AudioWatch::take_default_changed)
                {
                    self.worker.send(Command::ListOutputs);
                }
            }
            NETWORK_CHANGED => {
                self.network = muelle_shell::status::network();
                self.network_name = controls::network_name();
                if self.page() == Some(Page::Network) {
                    self.worker.send(Command::ListWifi);
                }
            }
            READINGS_CHANGED => {
                self.readings = self.worker.readings();
                self.lists = self.worker.lists();
            }
            // The track is read while drawing.
            _ => {}
        }
        self.refresh(gfx)
    }

    /// The page open now, not one folding away.
    fn page(&self) -> Option<Page> {
        self.sheet
            .as_ref()
            .filter(|sheet| sheet.leaving.is_none())
            .map(|sheet| sheet.page)
    }

    fn position(&self, track: Option<&media::Track>) -> Option<(u64, u64)> {
        if self.page() != Some(Page::Media) {
            return None;
        }
        let timeline = track?.timeline?;
        Some((
            timeline.position_at(Instant::now()).as_secs(),
            timeline.duration.as_secs(),
        ))
    }

    /// Redraws if anything shown has changed. A slider being dragged keeps
    /// the redraw for later: only its own parts move until it is released.
    pub fn refresh(&mut self, gfx: &Gfx) -> Result<()> {
        if let Some(drag) = self.drag {
            return self.paint_drag(drag);
        }
        let track = self.media.as_ref().and_then(media::Watch::snapshot);
        let look = Look {
            appearance: appearance::menu_bar(),
            network: self.network,
            network_name: self.network_name.clone(),
            readings: self.readings,
            lists: self.lists.clone(),
            volume: self.volume,
            position: self.position(track.as_ref()),
            track,
            accent: system::accent_color().unwrap_or([10, 132, 255]),
            page: self.page(),
        };
        if self.drawn.as_ref() == Some(&look) {
            return Ok(());
        }
        self.build(gfx, &look)?;
        self.drawn = Some(look);
        Ok(())
    }

    /// The periodic check while open.
    pub fn tick(&mut self, gfx: &Gfx) -> Result<Tick> {
        if let Some(until) = self.closing {
            return Ok(if Instant::now() >= until {
                Tick::Closed
            } else {
                Tick::Keep
            });
        }
        // SAFETY: Read-only keyboard, button and pointer queries.
        let (escape, primary, down, cursor) = unsafe {
            let held = |key: u16| GetAsyncKeyState(i32::from(key)) < 0;
            let primary = app::primary_button_down();
            let down = held(VK_LBUTTON.0) || held(VK_RBUTTON.0) || held(VK_MBUTTON.0);
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            (held(VK_ESCAPE.0), primary, down, cursor)
        };
        let was_down = std::mem::replace(&mut self.mouse_was_down, down);
        let escape_was_down = std::mem::replace(&mut self.escape_was_down, escape);
        let here = POINT {
            x: cursor.x - self.bounds.left,
            y: cursor.y - self.bounds.top,
        };
        if escape && !escape_was_down {
            // From a page back to the grid, then away.
            if self.page().is_none() || self.alone() {
                return Ok(Tick::Close);
            }
            self.close_page()?;
        }
        if let Some(drag) = self.drag {
            // A window that never takes focus may not hear the pointer once
            // it leaves, so the drag follows it from here too.
            self.drag_to(here.x)?;
            if !primary {
                self.finish_drag(gfx, drag.target)?;
            }
            return Ok(Tick::Keep);
        }
        if self
            .sheet
            .as_ref()
            .and_then(|sheet| sheet.leaving)
            .is_some_and(|until| Instant::now() >= until)
        {
            self.page_layer.Children()?.RemoveAll()?;
            self.sheet = None;
            self.page_slider = None;
        }
        // With a page open, a press elsewhere lands on the scrim (or the menu
        // bar, which closes it all itself).
        if down
            && !was_down
            && (self.page().is_none() || self.alone())
            && !contains(self.bounds, cursor)
            && !contains(self.button, cursor)
        {
            return Ok(Tick::Close);
        }
        if tracker::foreground_window() != self.foreground {
            return Ok(Tick::Close);
        }
        if let Some(held) = self.held {
            let over = contains(pixels(self.scale, self.places.place(held.tile)), here);
            if !over && !primary {
                // Released elsewhere: no message comes here for that.
                self.let_go()?;
            } else if over && primary && held.since.elapsed() >= HOLD {
                self.open_page(gfx, held.tile)?;
            }
        }
        // The player moves on, and the theme can change under it, from its own
        // dark mode button too.
        let stale = self.drawn.as_ref().is_some_and(|look| {
            look.appearance != appearance::menu_bar()
                || look.position != self.position(look.track.as_ref())
        });
        if stale {
            self.refresh(gfx)?;
        }
        Ok(Tick::Keep)
    }

    fn tile_visual(&self, wanted: Tile) -> Option<&ContainerVisual> {
        self.places
            .0
            .iter()
            .position(|(tile, _)| *tile == wanted)
            .and_then(|index| self.tiles.get(index))
    }

    /// The held tile springs back.
    fn let_go(&mut self) -> Result<Option<Held>> {
        let held = self.held.take();
        if let Some(held) = held
            && let Some(visual) = self.tile_visual(held.tile)
        {
            self.motion.press(visual, false)?;
        }
        Ok(held)
    }

    /// Grows `tile` into its page, if it has one.
    fn open_page(&mut self, gfx: &Gfx, tile: Tile) -> Result<()> {
        if !self.prepare_page(gfx, tile)? {
            return Ok(());
        }
        self.scrim = Some(Scrim::show(self.hwnd, self.button.bottom)?);
        if let Some(sheet) = &self.sheet {
            self.motion.expand(
                &self.page_layer,
                &self.grid,
                spread(pixels(self.scale, self.places.place(tile))),
                spread(pixels(self.scale, sheet.layout.area())),
            )?;
        }
        Ok(())
    }

    /// Reads and draws the page of `tile`; `false` if none is to be opened.
    fn prepare_page(&mut self, gfx: &Gfx, tile: Tile) -> Result<bool> {
        let Some(page) = page::page_of(tile) else {
            return Ok(false);
        };
        if self.page().is_some() || self.closing.is_some() {
            return Ok(false);
        }
        self.let_go()?;
        match page {
            Page::Network => self.worker.send(Command::ListWifi),
            Page::Bluetooth => self.worker.send(Command::ListBluetooth),
            Page::Sound => self.worker.send(Command::ListOutputs),
            Page::Media => {
                if let Some(media) = &self.media {
                    media.show_progress(true);
                }
            }
            _ => {}
        }
        let empty = Content {
            title: "",
            switch: None,
            slider: None,
            player: false,
            rows: Vec::new(),
            footer: None,
        };
        self.sheet = Some(Sheet {
            page,
            from: tile,
            layout: page::Layout::new(&empty),
            content: empty,
            pressed: None,
            leaving: None,
        });
        self.drawn = None;
        self.refresh(gfx)?;
        Ok(true)
    }

    /// Folds the page back into its tile.
    fn close_page(&mut self) -> Result<()> {
        self.drag = None;
        self.scrim = None;
        self.release_capture();
        let Some(sheet) = &mut self.sheet else {
            return Ok(());
        };
        if sheet.leaving.is_some() {
            return Ok(());
        }
        sheet.pressed = None;
        sheet.leaving = Some(Instant::now() + Duration::from_millis(motion::COLLAPSE_MS));
        self.motion.collapse(
            &self.page_layer,
            &self.grid,
            spread(pixels(self.scale, self.places.place(sheet.from))),
            spread(pixels(self.scale, sheet.layout.area())),
        )?;
        if let Some(media) = &self.media {
            media.show_progress(false);
        }
        // The grid shows what was chosen on the page.
        self.drawn = None;
        Ok(())
    }

    /// A button went down or up beside the window, with a page open. The
    /// whole click is kept from what lies beneath, and goes back to the grid.
    pub fn scrim_clicked(&mut self, down: bool) -> Result<()> {
        let Some(scrim) = &mut self.scrim else {
            return Ok(());
        };
        // A release alone ends a press that began on the page.
        if std::mem::replace(&mut scrim.pressed, down) && !down {
            self.close_page()?;
        }
        Ok(())
    }

    fn start_drag(&mut self, target: Target, span: (f32, f32), x: i32) -> Result<()> {
        self.drag = Some(Drag {
            target,
            span,
            percent: 0,
        });
        // SAFETY: Capture on our own window for the drag only.
        unsafe {
            SetCapture(self.hwnd);
        }
        self.drag_to(x)
    }

    fn has_value(&self, slider: Target) -> bool {
        match slider {
            Target::BrightnessSlider => self.readings.brightness.is_some(),
            _ => self.volume.is_some(),
        }
    }

    /// The primary button went down at `point`, in window pixels.
    pub fn press(&mut self, point: POINT) -> Result<Outcome> {
        if self.closing.is_some() {
            return Ok(Outcome::Stay);
        }
        let alone = self.alone();
        if let Some(sheet) = &mut self.sheet {
            if sheet.leaving.is_some() {
                return Ok(Outcome::Stay);
            }
            let hit = page::hit(&sheet.content, &sheet.layout, self.scale, point);
            let slider = sheet.content.slider.map(|(target, _)| target);
            let track = sheet.layout.slider;
            let area = sheet.layout.area();
            sheet.pressed = hit;
            match hit {
                // Beside the page: back to the grid, if there is one.
                None if alone => return Ok(Outcome::Close),
                None => self.close_page()?,
                Some(Hit::Slider) => {
                    if let (Some(target), Some(track)) = (slider, track)
                        && self.has_value(target)
                    {
                        let span = track_span(self.scale, area, track);
                        self.start_drag(target, span, point.x)?;
                    }
                }
                Some(_) => {}
            }
            return Ok(Outcome::Stay);
        }
        let Some((tile, mut target)) = target_at(&self.places, self.scale, point) else {
            return Ok(Outcome::Close);
        };
        if matches!(target, Target::BrightnessSlider | Target::VolumeSlider) {
            if self.has_value(target) {
                let track = if target == Target::BrightnessSlider {
                    BRIGHTNESS_TRACK
                } else {
                    VOLUME_TRACK
                };
                let span = track_span(self.scale, self.places.place(tile), track);
                self.start_drag(target, span, point.x)?;
                return Ok(Outcome::Stay);
            }
            // The page says where Windows sets it.
            target = Target::Open;
        }
        self.held = Some(Held {
            tile,
            target,
            since: Instant::now(),
        });
        if let Some(visual) = self.tile_visual(tile) {
            self.motion.press(visual, true)?;
        }
        Ok(Outcome::Stay)
    }

    pub fn moved(&mut self, point: POINT) -> Result<()> {
        if self.drag.is_some() {
            self.drag_to(point.x)?;
        }
        Ok(())
    }

    /// The primary button went up at `point`, in window pixels.
    pub fn release(&mut self, gfx: &Gfx, point: POINT) -> Result<Outcome> {
        if self.closing.is_some() {
            return Ok(Outcome::Stay);
        }
        if let Some(drag) = self.drag {
            self.drag_to(point.x)?;
            self.finish_drag(gfx, drag.target)?;
            return Ok(Outcome::Stay);
        }
        if let Some(sheet) = &mut self.sheet {
            let pressed = sheet.pressed.take();
            let released = page::hit(&sheet.content, &sheet.layout, self.scale, point);
            return match pressed {
                // Releasing somewhere else cancels, as any button does.
                Some(hit) if sheet.leaving.is_none() && released == Some(hit) => {
                    self.activate_page(gfx, hit)
                }
                _ => Ok(Outcome::Stay),
            };
        }
        // Nothing held: the press opened a page, or was on another tile.
        let Some(held) = self.let_go()? else {
            return Ok(Outcome::Stay);
        };
        let released = target_at(&self.places, self.scale, point);
        let same = released.is_some_and(|(tile, target)| {
            tile == held.tile && (target == held.target || held.target == Target::Open)
        });
        if !same {
            return Ok(Outcome::Stay);
        }
        self.activate(gfx, held.tile, held.target)
    }

    /// A right click opens a tile's page at once.
    pub fn secondary(&mut self, gfx: &Gfx, point: POINT) -> Result<()> {
        if self.sheet.is_none()
            && self.drag.is_none()
            && let Some((tile, _)) = target_at(&self.places, self.scale, point)
        {
            self.open_page(gfx, tile)?;
        }
        Ok(())
    }

    /// Turns `switch` over; `false` if there is nothing to switch.
    fn flip(&mut self, gfx: &Gfx, switch: Switch) -> Result<bool> {
        let reading = match switch {
            Switch::Wifi => &mut self.readings.wifi,
            Switch::Bluetooth => &mut self.readings.bluetooth,
            Switch::Focus => &mut self.readings.focus,
        };
        let Some(on) = *reading else {
            return Ok(false);
        };
        // Shown at once; the worker reads back what Windows did.
        *reading = Some(!on);
        self.worker.send(match switch {
            Switch::Wifi => Command::Radio(RadioSwitch::WiFi, !on),
            Switch::Bluetooth => Command::Radio(RadioSwitch::Bluetooth, !on),
            Switch::Focus => Command::Focus(!on),
        });
        self.refresh(gfx)?;
        Ok(true)
    }

    fn set_dark(&mut self, gfx: &Gfx, dark: bool) -> Result<()> {
        self.readings.dark = dark;
        self.worker.send(Command::Dark(dark));
        self.refresh(gfx)
    }

    fn play(&self, target: Target) {
        if let Some(media) = &self.media
            && let Some(track) = media.snapshot()
        {
            let control = match target {
                Target::Previous => media::Control::Previous,
                Target::Next => media::Control::Next,
                _ => media::Control::PlayPause,
            };
            media.send(&track, control);
        }
    }

    fn activate(&mut self, gfx: &Gfx, tile: Tile, target: Target) -> Result<Outcome> {
        let launch = match target {
            Target::Open => {
                self.open_page(gfx, tile)?;
                return Ok(Outcome::Stay);
            }
            Target::Switch(switch) => {
                // Without anything to switch, the page explains.
                if !self.flip(gfx, switch)? {
                    self.open_page(gfx, tile)?;
                }
                return Ok(Outcome::Stay);
            }
            Target::Nothing => return Ok(Outcome::Stay),
            Target::DarkMode => {
                self.set_dark(gfx, !self.readings.dark)?;
                return Ok(Outcome::Stay);
            }
            Target::Previous | Target::PlayPause | Target::Next => {
                self.play(target);
                return Ok(Outcome::Stay);
            }
            Target::Mute => {
                if let Some(audio) = &mut self.audio {
                    audio.toggle_mute()?;
                    self.volume = audio.volume();
                    self.refresh(gfx)?;
                }
                return Ok(Outcome::Stay);
            }
            Target::BrightnessSlider | Target::VolumeSlider => return Ok(Outcome::Stay),
            Target::Share => Launch::NearbySharing,
            Target::TaskView => Launch::TaskView,
            Target::Cast => Launch::Cast,
            Target::Calculator => Launch::Calculator,
            Target::Timer => Launch::Timer,
            Target::Screenshot => Launch::Screenshot,
            Target::More => Launch::QuickSettings,
        };
        Ok(Outcome::Launch(launch))
    }

    fn activate_page(&mut self, gfx: &Gfx, hit: Hit) -> Result<Outcome> {
        let Some(sheet) = &self.sheet else {
            return Ok(Outcome::Stay);
        };
        let row = match hit {
            Hit::Row(index) => sheet.content.rows.get(index),
            Hit::Footer => sheet.content.footer.as_ref(),
            Hit::Switch => {
                if let Some((switch, _)) = sheet.content.switch {
                    self.flip(gfx, switch)?;
                }
                None
            }
            Hit::Transport(target) => {
                self.play(target);
                None
            }
            Hit::Slider | Hit::Nothing => None,
        };
        // Only rows and the footer carry an action.
        match row.and_then(|row| row.action.clone()) {
            Some(Action::Launch(launch)) => return Ok(Outcome::Launch(launch)),
            Some(Action::JoinWifi(profile)) => self.worker.send(Command::JoinWifi(profile)),
            Some(Action::SelectOutput(id)) => {
                // Marked at once; the worker reads back what Windows did.
                for output in self.lists.outputs.iter_mut().flatten() {
                    output.selected = output.id == id;
                }
                self.worker.send(Command::SelectOutput(id));
                self.refresh(gfx)?;
            }
            Some(Action::Dark(dark)) => self.set_dark(gfx, dark)?,
            Some(Action::Focus) => {
                self.flip(gfx, Switch::Focus)?;
            }
            None => {}
        }
        Ok(Outcome::Stay)
    }

    fn drag_to(&mut self, x: i32) -> Result<()> {
        let Some(mut drag) = self.drag else {
            return Ok(());
        };
        let percent = slider_percent(drag.span, x);
        drag.percent = percent;
        self.drag = Some(drag);
        match drag.target {
            Target::BrightnessSlider => {
                if self.readings.brightness != Some(percent) {
                    self.readings.brightness = Some(percent);
                    self.worker.send(Command::Brightness(percent));
                }
            }
            _ => {
                let differs = self
                    .volume
                    .is_some_and(|volume| volume.percent != percent || volume.muted);
                if differs && let Some(audio) = &mut self.audio {
                    audio.set_percent(percent)?;
                    self.volume = audio.volume();
                }
            }
        }
        self.paint_drag(drag)
    }

    fn paint_drag(&self, drag: Drag) -> Result<()> {
        let slider = if self.page().is_some() {
            &self.page_slider
        } else if drag.target == Target::BrightnessSlider {
            &self.brightness_slider
        } else {
            &self.volume_slider
        };
        if let Some(slider) = slider {
            slider.move_to(drag.percent)?;
        }
        Ok(())
    }

    fn release_capture(&self) {
        // SAFETY: Releases only a capture this window holds.
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
        }
    }

    fn finish_drag(&mut self, gfx: &Gfx, target: Target) -> Result<()> {
        self.drag = None;
        if let Some(sheet) = &mut self.sheet {
            sheet.pressed = None;
        }
        self.release_capture();
        if target == Target::VolumeSlider {
            self.volume = self.audio.as_mut().and_then(AudioWatch::volume);
        }
        // The icons may have changed with the value.
        self.drawn = None;
        self.refresh(gfx)
    }

    fn painter<'a>(&self, gfx: &'a Gfx, layers: VisualCollection, look: &Look) -> Painter<'a> {
        Painter {
            gfx,
            layers,
            scale: self.scale,
            light: look.appearance.light,
            accent: rgba(look.accent[0], look.accent[1], look.accent[2], 255),
        }
    }

    /// Redraws the contents of each tile, and of the open page. The
    /// containers stay, with whatever movement they are in.
    fn build(&mut self, gfx: &Gfx, look: &Look) -> Result<()> {
        let theme = Theme::for_controls(look.appearance);
        // A hidden slider has nothing to move.
        for index in 0..self.places.0.len() {
            let (tile, place) = self.places.0[index];
            let Some(visual) = self.tiles.get(index) else {
                continue;
            };
            let layers = visual.Children()?;
            layers.RemoveAll()?;
            let painter = self.painter(gfx, layers, look);
            let radius = match tile {
                Tile::Media | Tile::Display | Tile::Sound => CORNER,
                _ => place[3] / 2.0,
            };
            painter.panel(theme, place, radius)?;
            match tile {
                Tile::Network => draw_network(&painter, place, look)?,
                Tile::Media => draw_media(&painter, place, look)?,
                Tile::Focus => draw_focus(&painter, place, look)?,
                Tile::More => {
                    painter.centered_text(place, strings::text().control_more, 12.0)?;
                }
                Tile::Display => self.brightness_slider = draw_display(&painter, place, look)?,
                Tile::Sound => self.volume_slider = draw_sound(&painter, place, look)?,
                _ => draw_round(&painter, tile, place, look)?,
            }
        }
        let Some(page) = look.page else {
            // A page folding away keeps what it showed.
            return Ok(());
        };
        let content = page::content(page, look, strings::text());
        let layout = page::Layout::new(&content);
        let layers = self.page_layer.Children()?;
        layers.RemoveAll()?;
        let painter = self.painter(gfx, layers, look);
        painter.panel(theme, layout.area(), CORNER)?;
        self.page_slider = page::draw(&painter, &content, &layout, look)?;
        if let Some(sheet) = &mut self.sheet {
            sheet.content = content;
            sheet.layout = layout;
        }
        Ok(())
    }
}

fn draw_network(painter: &Painter<'_>, place: [f32; 4], look: &Look) -> Result<()> {
    let text = strings::text();
    // With a Wi-Fi radio the tile is Wi-Fi's; without, the connection's.
    let wifi = look.readings.wifi;
    let connected = !matches!(look.network, Network::Offline);
    let (symbol, title) = match (wifi, look.network) {
        (None, Network::Ethernet) => (glyph::ETHERNET, text.control_ethernet),
        (None, Network::Offline) => (glyph::OFFLINE, text.control_network),
        _ => (glyph::WIFI, "Wi-Fi"),
    };
    let on = wifi.unwrap_or(connected);
    // A wired connection's name is not the Wi-Fi network's.
    let shows_name = wifi.is_none() || matches!(look.network, Network::Wifi { .. });
    let status = match &look.network_name {
        _ if wifi == Some(false) => text.control_off,
        Some(name) if connected && shows_name => name.as_str(),
        _ => text.control_not_connected,
    };
    painter.round_button(around(place, 32.0, 32.0, 44.0), symbol, on)?;
    painter.text(place, 62.0, 13.0, title, 13.0, true, 1.0, TWO - 72.0)?;
    painter.text(place, 62.0, 33.0, status, 12.0, false, 0.75, TWO - 72.0)
}

fn draw_media(painter: &Painter<'_>, place: [f32; 4], look: &Look) -> Result<()> {
    let text = strings::text();
    let width = TWO - 28.0;
    let track = look.track.as_ref();
    if let Some(track) = track
        && let Some(image) = media::artwork(track)
    {
        painter.image(image, [place[0] + 14.0, place[1] + 14.0, 44.0, 44.0])?;
    }
    let title = track.map_or(text.control_not_playing, |track| track.title.as_str());
    painter.text(place, 14.0, 64.0, title, 13.0, true, 1.0, width)?;
    if let Some(track) = track {
        painter.text(place, 14.0, 83.0, &track.artist, 12.0, false, 0.75, width)?;
    }
    for (target, x) in MEDIA_CONTROLS {
        let (symbol, enabled) = transport(target, track);
        painter.symbol(
            place,
            x,
            MEDIA_CONTROLS_Y,
            symbol,
            22.0,
            if enabled { 1.0 } else { 0.35 },
        )?;
    }
    Ok(())
}

fn draw_focus(painter: &Painter<'_>, place: [f32; 4], look: &Look) -> Result<()> {
    let text = strings::text();
    let on = look.readings.focus == Some(true);
    painter.round_button(around(place, 32.0, 32.0, 44.0), glyph::FOCUS, on)?;
    let top = if on { 13.0 } else { 23.0 };
    painter.text(
        place,
        62.0,
        top,
        text.control_focus,
        13.0,
        true,
        1.0,
        TWO - 72.0,
    )?;
    if on {
        painter.text(
            place,
            62.0,
            33.0,
            text.control_on,
            12.0,
            false,
            0.75,
            TWO - 72.0,
        )?;
    }
    Ok(())
}

/// The round tiles.
fn draw_round(painter: &Painter<'_>, tile: Tile, place: [f32; 4], look: &Look) -> Result<()> {
    let bluetooth = look.readings.bluetooth;
    let (symbol, lit, opacity) = match tile {
        // Switches: lit while on; Bluetooth is dim without a radio.
        Tile::Bluetooth => (
            glyph::BLUETOOTH,
            bluetooth == Some(true),
            if bluetooth.is_some() { 1.0 } else { 0.4 },
        ),
        Tile::DarkMode => (glyph::DARK_MODE, look.readings.dark, 1.0),
        Tile::Share => (glyph::SHARE, false, 1.0),
        Tile::TaskView => (glyph::TASK_VIEW, false, 1.0),
        Tile::Cast => (glyph::CAST, false, 1.0),
        Tile::Calculator => (glyph::CALCULATOR, false, 1.0),
        Tile::Timer => (glyph::TIMER, false, 1.0),
        _ => (glyph::SCREENSHOT, false, 1.0),
    };
    if lit {
        painter.round_button(place, symbol, true)
    } else {
        painter.symbol(place, CELL / 2.0, CELL / 2.0, symbol, 26.0, opacity)
    }
}

fn draw_display(painter: &Painter<'_>, place: [f32; 4], look: &Look) -> Result<Option<Slider>> {
    let text = strings::text();
    painter.text(
        place,
        16.0,
        9.0,
        text.control_display,
        13.0,
        true,
        1.0,
        GRID - 32.0,
    )?;
    let level = look.readings.brightness;
    let opacity = if level.is_some() { 1.0 } else { 0.35 };
    let y = BRIGHTNESS_TRACK[2];
    painter.symbol(place, 22.0, y, glyph::BRIGHTNESS, 14.0, opacity)?;
    painter.symbol(place, GRID - 22.0, y, glyph::BRIGHTNESS, 20.0, opacity)?;
    painter.slider(place, BRIGHTNESS_TRACK, level)
}

fn draw_sound(painter: &Painter<'_>, place: [f32; 4], look: &Look) -> Result<Option<Slider>> {
    let text = strings::text();
    painter.text(
        place,
        16.0,
        9.0,
        text.control_sound,
        13.0,
        true,
        1.0,
        GRID - 32.0,
    )?;
    let volume = look.volume;
    let opacity = if volume.is_some() { 1.0 } else { 0.35 };
    let y = VOLUME_TRACK[2];
    let low = if volume.is_some_and(|v| v.muted) {
        glyph::MUTE
    } else {
        glyph::VOLUME_LOW
    };
    painter.symbol(place, 22.0, y, low, 16.0, opacity)?;
    painter.symbol(place, GRID - 58.0, y, glyph::VOLUME_HIGH, 18.0, opacity)?;
    painter.circle(around(place, GRID - 28.0, y, 28.0), painter.wash())?;
    painter.symbol(place, GRID - 28.0, y, glyph::OUTPUT, 16.0, 1.0)?;
    painter.slider(place, VOLUME_TRACK, volume.map(|v| v.percent))
}

/// Draws into one tile or page, placing things by grid coordinates.
struct Painter<'a> {
    gfx: &'a Gfx,
    layers: VisualCollection,
    scale: f32,
    light: bool,
    accent: Color,
}

impl Painter<'_> {
    fn ink(&self) -> Ink {
        if self.light { Ink::Dark } else { Ink::Light }
    }

    fn ink_color(&self) -> Color {
        if self.light {
            rgba(0, 0, 0, 220)
        } else {
            rgba(255, 255, 255, 240)
        }
    }

    /// A faint disc or track behind content.
    fn wash(&self) -> Color {
        if self.light {
            rgba(0, 0, 0, 26)
        } else {
            rgba(255, 255, 255, 46)
        }
    }

    /// The glass behind a tile or page.
    fn panel(&self, theme: Theme, place: [f32; 4], radius: f32) -> Result<()> {
        let [left, top, width, height] = spread(pixels(self.scale, place));
        let panel = theme.panel(
            self.gfx,
            Vector2::new(width, height),
            (radius * self.scale).round(),
            self.scale,
        )?;
        panel.SetOffset(Vector3::new(left, top, 0.0))?;
        self.layers.InsertAtTop(&panel)?;
        Ok(())
    }

    fn rounded(&self, area: RECT, radius: f32, fill: Color) -> Result<()> {
        let [left, top, width, height] = spread(area);
        retained_rounded_box(
            &self.gfx.compositor,
            &self.layers,
            left,
            top,
            width,
            height,
            radius,
            fill,
            None,
        )?;
        Ok(())
    }

    fn circle(&self, place: [f32; 4], fill: Color) -> Result<()> {
        let area = pixels(self.scale, place);
        self.rounded(area, (area.right - area.left) as f32 / 2.0, fill)
    }

    /// A dividing line.
    fn line(&self, place: [f32; 4]) -> Result<()> {
        let mut area = pixels(self.scale, place);
        area.bottom = area.top + self.scale.round().max(1.0) as i32;
        self.rounded(area, 0.0, self.wash())
    }

    fn image(&self, image: &muelle_shell::icons::IconImage, place: [f32; 4]) -> Result<()> {
        let surface = self.gfx.image_surface(image)?;
        let brush = self
            .gfx
            .compositor
            .CreateSurfaceBrushWithSurface(&surface)?;
        let [left, top, width, height] = spread(pixels(self.scale, place));
        add_visual(
            &self.gfx.compositor,
            &self.layers,
            &brush.cast()?,
            left,
            top,
            width,
            height,
        )?;
        Ok(())
    }

    /// A glyph of logical size `size` centred on a point of a tile.
    fn symbol_in(
        &self,
        place: [f32; 4],
        x: f32,
        y: f32,
        symbol: char,
        size: f32,
        color: Color,
    ) -> Result<()> {
        let side = (size * self.scale * 1.25).round();
        let brush = self
            .gfx
            .symbol_brush(symbol, side, size * self.scale, color)?;
        add_visual(
            &self.gfx.compositor,
            &self.layers,
            &brush.brush.cast()?,
            (MARGIN + place[0] + x) * self.scale - side / 2.0,
            (MARGIN + place[1] + y) * self.scale - side / 2.0,
            brush.width,
            brush.height,
        )?;
        Ok(())
    }

    fn symbol(
        &self,
        place: [f32; 4],
        x: f32,
        y: f32,
        symbol: char,
        size: f32,
        opacity: f32,
    ) -> Result<()> {
        let mut color = self.ink_color();
        color.A = (f32::from(color.A) * opacity).round() as u8;
        self.symbol_in(place, x, y, symbol, size, color)
    }

    /// A switch filling `place`: white with an accent glyph while on, as in
    /// macOS, otherwise a faint disc.
    fn round_button(&self, place: [f32; 4], symbol: char, on: bool) -> Result<()> {
        let fill = if on {
            rgba(255, 255, 255, 242)
        } else {
            self.wash()
        };
        self.circle(place, fill)?;
        let color = if on { self.accent } else { self.ink_color() };
        let size = place[2] * if place[2] < CELL { 0.48 } else { 0.42 };
        self.symbol_in(
            [place[0], place[1], 0.0, 0.0],
            place[2] / 2.0,
            place[3] / 2.0,
            symbol,
            size,
            color,
        )
    }

    /// A macOS-style on/off switch.
    fn switch(&self, place: [f32; 4], on: bool) -> Result<()> {
        let radius = place[3] / 2.0;
        self.rounded(
            pixels(self.scale, place),
            radius * self.scale,
            if on { self.accent } else { self.wash() },
        )?;
        let knob = place[3] - 4.0;
        let x = if on {
            place[0] + place[2] - knob - 2.0
        } else {
            place[0] + 2.0
        };
        self.circle([x, place[1] + 2.0, knob, knob], WHITE)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Typography and placement are chosen together for each label"
    )]
    fn text(
        &self,
        place: [f32; 4],
        x: f32,
        y: f32,
        value: &str,
        size: f32,
        bold: bool,
        opacity: f32,
        width: f32,
    ) -> Result<()> {
        if value.is_empty() {
            return Ok(());
        }
        let label = self.gfx.text_brush_fitted(
            value,
            self.scale,
            size,
            bold,
            self.ink(),
            width * self.scale,
        )?;
        add_visual(
            &self.gfx.compositor,
            &self.layers,
            &label.brush.cast()?,
            (MARGIN + place[0] + x) * self.scale,
            (MARGIN + place[1] + y) * self.scale,
            label.width,
            label.height,
        )?
        .SetOpacity(opacity)?;
        Ok(())
    }

    /// Text ending at `right`.
    fn text_right(
        &self,
        place: [f32; 4],
        right: f32,
        y: f32,
        value: &str,
        size: f32,
        opacity: f32,
    ) -> Result<()> {
        let label = self
            .gfx
            .text_brush_sized(value, self.scale, size, false, self.ink())?;
        add_visual(
            &self.gfx.compositor,
            &self.layers,
            &label.brush.cast()?,
            (MARGIN + place[0] + right) * self.scale - label.width,
            (MARGIN + place[1] + y) * self.scale,
            label.width,
            label.height,
        )?
        .SetOpacity(opacity)?;
        Ok(())
    }

    fn centered_text(&self, place: [f32; 4], value: &str, size: f32) -> Result<()> {
        let label = self.gfx.text_brush_fitted(
            value,
            self.scale,
            size,
            true,
            self.ink(),
            (place[2] - 16.0) * self.scale,
        )?;
        let area = pixels(self.scale, place);
        add_visual(
            &self.gfx.compositor,
            &self.layers,
            &label.brush.cast()?,
            (area.left + area.right) as f32 / 2.0 - label.width / 2.0,
            (area.top + area.bottom) as f32 / 2.0 - label.height / 2.0,
            label.width,
            label.height,
        )?;
        Ok(())
    }

    /// A horizontal slider. Without a value only its track is drawn.
    fn slider(
        &self,
        place: [f32; 4],
        [left, width, y]: [f32; 3],
        percent: Option<u8>,
    ) -> Result<Option<Slider>> {
        let s = self.scale;
        let track = pixels(s, [place[0] + left, place[1] + y - 3.0, width, 6.0]);
        let height = (track.bottom - track.top) as f32;
        self.rounded(track, height / 2.0, self.wash())?;
        let Some(percent) = percent else {
            return Ok(None);
        };
        let white = rgba(255, 255, 255, 250);
        let (_, fill) = retained_rounded_box(
            &self.gfx.compositor,
            &self.layers,
            track.left as f32,
            track.top as f32,
            (track.right - track.left) as f32,
            height,
            height / 2.0,
            white,
            None,
        )?;
        let (knob_width, knob_height) = ((20.0 * s).round(), (15.0 * s).round());
        let knob_top = ((track.top + track.bottom) as f32 / 2.0 - knob_height / 2.0).round();
        let (knob, _) = retained_rounded_box(
            &self.gfx.compositor,
            &self.layers,
            track.left as f32,
            knob_top,
            knob_width,
            knob_height,
            knob_height / 2.0,
            white,
            Some(rgba(0, 0, 0, 38)),
        )?;
        let slider = Slider {
            fill,
            knob,
            left: track.left as f32,
            width: (track.right - track.left) as f32,
            knob_top,
            knob_width,
        };
        slider.move_to(percent)?;
        Ok(Some(slider))
    }
}

impl Drop for ControlCenter {
    fn drop(&mut self) {
        // Release the composition target before the HWND.
        self.target = None;
        self.release_capture();
        // SAFETY: Our own timer and window.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn control_center_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
) -> LRESULT {
    let point = || POINT {
        x: i32::from((l.0 & 0xFFFF) as i16),
        y: i32::from((l.0 >> 16) as i16),
    };
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if w.0 == TIMER => app::control_center_tick(),
        WM_LBUTTONDOWN => app::control_center_press(point()),
        WM_MOUSEMOVE => app::control_center_moved(point()),
        WM_LBUTTONUP => app::control_center_release(point()),
        WM_RBUTTONUP => app::control_center_secondary(point()),
        AUDIO_CHANGED | MEDIA_CHANGED | NETWORK_CHANGED | READINGS_CHANGED => {
            app::control_center_changed(msg);
        }
        // SAFETY: Standard forwarding of unhandled messages.
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muelle_shell::control_lists::{AudioOutput, WifiList, WifiNetwork};

    const SCREEN: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    fn look() -> Look {
        Look {
            appearance: appearance::resolve(muelle_config::Appearance::Dark, false, true),
            network: Network::Ethernet,
            network_name: Some("Home".into()),
            readings: Readings {
                wifi: Some(true),
                bluetooth: None,
                focus: Some(false),
                brightness: Some(50),
                dark: true,
            },
            lists: Lists::default(),
            volume: None,
            track: None,
            accent: [10, 132, 255],
            page: None,
            position: None,
        }
    }

    const PAGES: [Page; 11] = [
        Page::Network,
        Page::Media,
        Page::Bluetooth,
        Page::Sharing,
        Page::Focus,
        Page::Desktops,
        Page::Screens,
        Page::Display,
        Page::Sound,
        Page::Appearance,
        Page::Screenshot,
    ];

    /// All the tiles, as Settings has them to start with.
    fn all() -> Grid {
        Grid::new(ControlTiles::default())
    }

    fn assert_fits(grid: &Grid) {
        for (index, (tile, a)) in grid.0.iter().enumerate() {
            assert!(a[0] >= 0.0 && a[1] >= 0.0, "{tile:?}");
            assert!(a[0] + a[2] <= GRID && a[1] + a[3] <= HEIGHT, "{tile:?}");
            for (other, b) in &grid.0[index + 1..] {
                let apart = a[0] + a[2] < b[0]
                    || b[0] + b[2] < a[0]
                    || a[1] + a[3] < b[1]
                    || b[1] + b[3] < a[1];
                assert!(apart, "{tile:?} and {other:?}");
            }
        }
    }

    #[test]
    fn tiles_stay_inside_the_grid_without_touching() {
        let grid = all();
        assert_eq!(grid.0.len(), TILES.len() + 1);
        assert_fits(&grid);
    }

    #[test]
    fn with_every_tile_the_layout_is_that_of_macos() {
        let grid = all();
        let cell = |tile| {
            let [left, top, ..] = grid.place(tile);
            (left / (CELL + GAP), top / (CELL + GAP))
        };
        assert_eq!(cell(Tile::Media), (2.0, 0.0));
        assert_eq!(cell(Tile::Share), (1.0, 1.0));
        assert_eq!(cell(Tile::Cast), (3.0, 2.0));
        assert_eq!(cell(Tile::Sound), (0.0, 4.0));
        assert_eq!(cell(Tile::Screenshot), (3.0, 5.0));
        assert_eq!(grid.place(Tile::More)[1], 6.0 * (CELL + GAP));
    }

    #[test]
    fn hidden_tiles_leave_no_room_and_more_stays_last() {
        // Every choice of tiles.
        for bits in 0..1u16 << TILES.len() {
            let on = |index: u16| bits & (1 << index) != 0;
            let shown = ControlTiles {
                network: on(0),
                media: on(1),
                bluetooth: on(2),
                nearby_sharing: on(3),
                focus: on(4),
                task_view: on(5),
                cast: on(6),
                display: on(7),
                sound: on(8),
                dark_mode: on(9),
                calculator: on(10),
                timer: on(11),
                screenshot: on(12),
            };
            let grid = Grid::new(shown);
            assert_eq!(grid.0.len(), bits.count_ones() as usize + 1);
            assert_fits(&grid);
            let more = grid.place(Tile::More)[1];
            assert!(grid.0.iter().all(|(_, place)| place[1] <= more));
        }
        // With only the sliders, they move to the top.
        let none = ControlTiles {
            network: false,
            media: false,
            bluetooth: false,
            nearby_sharing: false,
            focus: false,
            task_view: false,
            cast: false,
            dark_mode: false,
            calculator: false,
            timer: false,
            screenshot: false,
            ..ControlTiles::default()
        };
        let grid = Grid::new(none);
        assert_eq!(grid.place(Tile::Display)[1], 0.0);
        assert_eq!(grid.place(Tile::More)[1], 2.0 * (CELL + GAP));
        assert_eq!(grid.place(Tile::Network), [0.0; 4]);
    }

    #[test]
    fn presses_find_their_control_at_every_scale() {
        let grid = all();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let at = |tile: Tile, x: f32, y: f32| {
                let place = grid.place(tile);
                target_at(
                    &grid,
                    scale,
                    POINT {
                        x: ((MARGIN + place[0] + x) * scale) as i32,
                        y: ((MARGIN + place[1] + y) * scale) as i32,
                    },
                )
            };
            let finds = |tile, x, y, target| assert_eq!(at(tile, x, y), Some((tile, target)));
            // A click anywhere on a switch's tile switches it.
            let wifi = Target::Switch(Switch::Wifi);
            finds(Tile::Network, 32.0, 32.0, wifi);
            finds(Tile::Network, 100.0, 32.0, wifi);
            finds(Tile::Focus, 100.0, 32.0, Target::Switch(Switch::Focus));
            finds(
                Tile::Bluetooth,
                32.0,
                32.0,
                Target::Switch(Switch::Bluetooth),
            );
            finds(Tile::Media, 70.0, 116.0, Target::PlayPause);
            finds(Tile::Media, 104.0, 116.0, Target::Next);
            finds(Tile::Media, 70.0, 30.0, Target::PlayPause);
            finds(Tile::Display, 120.0, 43.0, Target::BrightnessSlider);
            finds(Tile::Display, 40.0, 12.0, Target::Nothing);
            finds(Tile::Sound, 80.0, 12.0, Target::Mute);
            finds(Tile::Sound, 120.0, 43.0, Target::VolumeSlider);
            finds(Tile::Sound, 22.0, 43.0, Target::Mute);
            finds(Tile::Sound, GRID - 28.0, 43.0, Target::Open);
            finds(Tile::Screenshot, 32.0, 32.0, Target::Screenshot);
            // The gap between two tiles is outside.
            assert_eq!(at(Tile::Network, 20.0, CELL + GAP / 2.0), None);
        }
    }

    #[test]
    fn sliders_map_their_track_from_zero_to_a_hundred() {
        for scale in [1.0, 1.5] {
            let span = track_span(scale, all().place(Tile::Sound), VOLUME_TRACK);
            let x = |along: f32| (span.0 + span.1 * along).round() as i32;
            assert_eq!(slider_percent(span, x(0.0)), 0);
            assert_eq!(slider_percent(span, x(0.5)), 50);
            assert_eq!(slider_percent(span, x(1.0)), 100);
            assert_eq!(slider_percent(span, -500), 0);
            assert_eq!(slider_percent(span, 5000), 100);
        }
    }

    #[test]
    fn every_tile_with_more_to_offer_has_a_page() {
        for (tile, _) in all().0 {
            let single = matches!(tile, Tile::Calculator | Tile::Timer | Tile::More);
            assert_eq!(page::page_of(tile).is_none(), single, "{tile:?}");
        }
    }

    #[test]
    fn every_page_fits_the_window_and_its_parts_answer_where_drawn() {
        let mut crowded = look();
        crowded.lists.wifi = Some(WifiList::Networks(
            (0..30)
                .map(|i| WifiNetwork {
                    name: format!("Network {i}"),
                    signal: Some(50),
                    connected: i == 0,
                    profile: None,
                    secured: true,
                })
                .collect(),
        ));
        for state in [look(), crowded] {
            for page in PAGES {
                let content = page::content(page, &state, strings::text());
                let layout = page::Layout::new(&content);
                assert!(!content.title.is_empty(), "{page:?}");
                assert!(layout.height <= HEIGHT, "{page:?}: {}", layout.height);
                assert_eq!(layout.footer.is_some(), content.footer.is_some());
                for scale in [1.0, 1.5] {
                    let middle_of = |area: [f32; 4]| {
                        let at = pixels(scale, area);
                        POINT {
                            x: (at.left + at.right) / 2,
                            y: (at.top + at.bottom) / 2,
                        }
                    };
                    for (index, row) in layout.rows.iter().enumerate() {
                        let hit = page::hit(&content, &layout, scale, middle_of(*row));
                        assert_eq!(hit, Some(Hit::Row(index)), "{page:?}");
                    }
                    if let Some(footer) = layout.footer {
                        let hit = page::hit(&content, &layout, scale, middle_of(footer));
                        assert_eq!(hit, Some(Hit::Footer), "{page:?}");
                    }
                    if let Some(switch) = layout.switch {
                        let hit = page::hit(&content, &layout, scale, middle_of(switch));
                        assert_eq!(hit, Some(Hit::Switch), "{page:?}");
                    }
                    let below = POINT {
                        x: (GRID / 2.0 * scale) as i32,
                        y: ((MARGIN + layout.height + 4.0) * scale) as i32,
                    };
                    assert_eq!(page::hit(&content, &layout, scale, below), None);
                }
            }
        }
    }

    #[test]
    fn pages_offer_what_the_pc_has() {
        let mut state = look();
        // No Bluetooth radio: no switch, and an explanation once listed.
        state.lists.bluetooth = Some(Vec::new());
        let bluetooth = page::content(Page::Bluetooth, &state, strings::text());
        assert_eq!(bluetooth.switch, None);
        assert_eq!(bluetooth.rows.len(), 1);
        assert_eq!(bluetooth.rows[0].action, None);
        // The output in use is marked, and each can be chosen.
        state.lists.outputs = Some(vec![
            AudioOutput {
                id: "a".into(),
                name: "Speakers".into(),
                selected: false,
            },
            AudioOutput {
                id: "b".into(),
                name: "Headset".into(),
                selected: true,
            },
        ]);
        let sound = page::content(Page::Sound, &state, strings::text());
        let marked: Vec<bool> = sound.rows.iter().map(|row| row.checked).collect();
        assert_eq!(marked, [false, true]);
        assert_eq!(sound.rows[0].action, Some(Action::SelectOutput("a".into())));
        // Wi-Fi off lists nothing to join.
        state.readings.wifi = Some(false);
        let wifi = page::content(Page::Network, &state, strings::text());
        assert!(wifi.rows.iter().all(|row| row.action.is_none()));
        // The theme in use is the marked one.
        let theme = page::content(Page::Appearance, &state, strings::text());
        assert_eq!(theme.rows[1].action, Some(Action::Dark(true)));
        assert!(theme.rows[1].checked && !theme.rows[0].checked);
    }

    #[test]
    fn hidden_networks_leave_the_saved_ones_and_a_way_to_the_rest() {
        let mut state = look();
        state.lists.wifi = Some(WifiList::SavedOnly(
            (0..20)
                .map(|i| WifiNetwork {
                    name: format!("Saved {i}"),
                    signal: None,
                    connected: i == 0,
                    profile: Some(format!("Saved {i}")),
                    secured: true,
                })
                .collect(),
        ));
        let content = page::content(Page::Network, &state, strings::text());
        let layout = page::Layout::new(&content);
        // Every row fits, however many networks are saved.
        assert_eq!(layout.rows.len(), content.rows.len());
        assert_eq!(content.rows[0].action, None);
        assert_eq!(
            content.rows[1].action,
            Some(Action::JoinWifi("Saved 1".into()))
        );
        assert_eq!(
            content.rows.last().and_then(|row| row.action.clone()),
            Some(Action::Launch(Launch::NetworkList))
        );
    }

    #[test]
    fn a_page_alone_stays_under_its_icon() {
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let (scale, bounds) = placement(screen, 28, 1.0);
        let icon = |left| RECT {
            left,
            top: 0,
            right: left + 30,
            bottom: 28,
        };
        // Near the edge it keeps the control center's place.
        assert_eq!(under(bounds, screen, icon(1800), scale), bounds);
        // Further in, it follows the icon.
        let moved = under(bounds, screen, icon(1000), scale);
        assert_eq!(moved.left, 988);
        assert_eq!(moved.right - moved.left, bounds.right - bounds.left);
    }

    #[test]
    fn it_opens_at_the_right_under_the_bar_and_shrinks_to_fit() {
        let (scale, bounds) = placement(SCREEN, 28, 1.0);
        assert_eq!(scale, 1.0);
        assert_eq!(bounds.right, SCREEN.right);
        assert_eq!(bounds.top, 28);
        assert!(bounds.bottom <= SCREEN.bottom);
        let small = RECT {
            bottom: 600,
            ..SCREEN
        };
        let (scale, bounds) = placement(small, 56, 2.0);
        assert!(scale < 2.0);
        assert!(bounds.bottom <= small.bottom && bounds.left >= small.left);
    }

    /// Shows the real window, then the page of each tile in turn, to look at
    /// them. It reads this PC's state and changes nothing.
    #[test]
    #[ignore = "Briefly opens the control center on the desktop"]
    fn native_preview() -> Result<()> {
        use std::thread;
        use windows::Win32::{
            System::{
                Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
                WinRT::{
                    CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                    DispatcherQueueOptions,
                },
            },
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
                },
                WindowsAndMessaging::{
                    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, WindowFromPoint,
                },
            },
        };
        // SAFETY: A private apartment, dispatcher queue and compositor here.
        let _queue = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };
        let gfx = Gfx::new()?;
        let monitor = system::primary_monitor().ok_or_else(|| anyhow::anyhow!("no monitor"))?;
        let bar = (28.0 * monitor.scale).round() as i32;
        let button = RECT {
            left: monitor.screen.right - 200,
            top: monitor.screen.top,
            right: monitor.screen.right - 160,
            bottom: monitor.screen.top + bar,
        };
        let mut center = ControlCenter::show(
            &gfx,
            monitor.screen,
            button,
            monitor.scale,
            Opening::Tiles,
            ControlTiles::default(),
        )?;
        println!("bounds {:?}", center.bounds);
        // No dock here to forward messages, so read what the watches have.
        let wait = |center: &mut ControlCenter, milliseconds: u64| -> Result<()> {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(milliseconds) {
                let mut message = MSG::default();
                // SAFETY: Pumps this test thread's own queue.
                unsafe {
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&message);
                    }
                }
                center.changed(&gfx, AUDIO_CHANGED)?;
                center.changed(&gfx, READINGS_CHANGED)?;
                center.tick(&gfx)?;
                thread::sleep(Duration::from_millis(30));
            }
            Ok(())
        };
        println!("stage grid");
        wait(&mut center, 2500)?;
        for tile in [
            Tile::Network,
            Tile::Sound,
            Tile::Display,
            Tile::Media,
            Tile::Screenshot,
        ] {
            center.open_page(&gfx, tile)?;
            wait(&mut center, 900)?;
            println!("stage {tile:?}");
            wait(&mut center, 1500)?;
            // Windows itself says what a click would land on: the scrim all
            // around the window, the window within it.
            let scrim = center.scrim.as_ref().map(|scrim| scrim.hwnd);
            let at = |x, y| unsafe { WindowFromPoint(POINT { x, y }) };
            let bounds = center.bounds;
            for (x, y) in [
                (bounds.left - 200, bounds.top + 100),
                (monitor.screen.left + 20, monitor.screen.bottom - 5),
            ] {
                assert_eq!(Some(at(x, y)), scrim, "{tile:?} at {x},{y}");
            }
            assert_eq!(at(bounds.left + 40, bounds.top + 40), center.hwnd);
            // A release without a press there changes nothing; a click goes back.
            center.scrim_clicked(false)?;
            assert!(center.page().is_some());
            center.scrim_clicked(true)?;
            center.scrim_clicked(false)?;
            assert!(center.page().is_none() && center.scrim.is_none());
            wait(&mut center, 500)?;
            // SAFETY: Read-only hit test.
            assert_ne!(Some(at(bounds.left - 200, bounds.top + 100)), scrim);
        }
        if center.begin_close()? {
            wait(&mut center, 400)?;
        }
        Ok(())
    }
}
