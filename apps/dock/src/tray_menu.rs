//! The tray menu: the notification area's icons in a glass menu under the menu
//! bar's arrow, with the control center's look and movement. A click on a row
//! tells its program what a click on its icon in the taskbar would, so each
//! opens its own window or menu. The last row opens Windows' own flyout, for
//! an icon the host has not heard of. A program's menu opens beside its row,
//! with the tray menu still behind it.
//!
//! It exists only while open, and never takes the focus from the app in front.

mod submenu;

use std::time::{Duration, Instant};

use anyhow::Result;
use muelle_shell::{
    launcher, names, system, tracker,
    tray_host::{self, Click, Shown, TrayHost, TrayIcon},
};
use submenu::{Seen, Step, Submenu, contains};
use windows::{
    UI::{
        Color,
        Composition::{
            ContainerVisual, Desktop::DesktopWindowTarget, ShapeVisual, VisualCollection,
        },
    },
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute},
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, GetDoubleClickTime, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos,
                GetWindowThreadProcessId, HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW,
                MA_NOACTIVATE, RegisterClassW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetTimer,
                SetWindowPos, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
                WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
                WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{BOOL, Interface, w},
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    app, appearance,
    control_center::{Tick, motion, motion::Motion},
    gfx::{Gfx, Ink},
    menubar::{add_visual, retained_rounded_box, rgba},
    scene::Theme,
    strings,
};

const TIMER: usize = 1;
/// How often the pointer, keyboard and icons are checked, only while open.
const TICK_MS: u32 = 30;
/// While a program's menu is awaited or open: it is moved as soon as it shows.
const MENU_TICK_MS: u32 = 10;
/// How long a program may take to show a menu after a right click, and after
/// the time a left click waits for a second one.
const MENU_WAIT: Duration = Duration::from_millis(1500);
const AFTER_DOUBLE: Duration = Duration::from_millis(400);
/// Between the glass and a program's menu.
const GAP: f32 = 4.0;

// Logical pixels.
const WIDTH: f32 = 292.0;
const ROW: f32 = 32.0;
/// Between the glass's edge and its rows.
const PADDING: f32 = 6.0;
/// The band holding the line above the last row.
const DIVIDER: f32 = 11.0;
const CORNER: f32 = 16.0;
/// Room around the glass inside the window, for its spring.
const MARGIN: f32 = 12.0;
const DROP: f32 = 6.0;
/// Distance kept from the screen's right edge.
const EDGE: f32 = 8.0;
const ICON: f32 = 16.0;
const TEXT_LEFT: f32 = 38.0;
const TEXT: f32 = 13.0;
/// Shown for an icon whose picture could not be read.
const NO_PICTURE: char = '\u{ECAA}';
/// Windows' "^".
const ARROW: char = '\u{E70E}';

/// What a click on the menu asks of the dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// Windows' own hidden icons, hung under this point.
    Windows((i32, i32)),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    /// The program's icon at this index.
    Icon(usize),
    /// Windows' own menu.
    Windows,
}

/// The glass's height for `icons` rows, the line and the last row.
fn glass_height(icons: usize) -> f32 {
    icons as f32 * ROW + DIVIDER + ROW + 2.0 * PADDING
}

/// A row's top, from the top of the glass.
fn row_top(spot: Spot, icons: usize) -> f32 {
    match spot {
        Spot::Icon(index) => PADDING + index as f32 * ROW,
        Spot::Windows => PADDING + icons as f32 * ROW + DIVIDER,
    }
}

/// The row under a point of the window, in its pixels.
fn spot_at(scale: f32, icons: usize, point: POINT) -> Option<Spot> {
    let x = point.x as f32 / scale - MARGIN;
    let y = point.y as f32 / scale - MARGIN;
    if x < PADDING || x >= WIDTH - PADDING {
        return None;
    }
    (0..icons)
        .map(Spot::Icon)
        .chain([Spot::Windows])
        .find(|&spot| {
            let top = row_top(spot, icons);
            y >= top && y < top + ROW
        })
}

/// The scale to draw at, the window's place, and how many icons fit: under the
/// button at `button`, kept on `screen`.
fn placement(screen: RECT, button: RECT, scale: f32, icons: usize) -> (f32, RECT, usize) {
    let px = |v: f32| (v * scale).round() as i32;
    let room = (screen.bottom - button.bottom) as f32 / scale - 2.0 * MARGIN - DROP;
    let fits = ((room - glass_height(0)) / ROW).max(0.0) as usize;
    let icons = icons.min(fits);
    let (width, height) = (
        px(WIDTH + 2.0 * MARGIN),
        px(glass_height(icons) + 2.0 * MARGIN),
    );
    let furthest = screen.right - px(EDGE) + px(MARGIN) - width;
    let left = (button.left - px(MARGIN)).min(furthest).max(screen.left);
    // The glass hangs `DROP` below the bar; the window starts at the bar.
    let top = button.bottom;
    (
        scale,
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        },
        icons,
    )
}

/// The first line of the tooltip, which names the program as the taskbar does.
fn first_line(tip: &str) -> Option<&str> {
    tip.lines().map(str::trim).find(|line| !line.is_empty())
}

fn label(icon: &TrayIcon) -> String {
    if let Some(line) = first_line(&icon.tip) {
        return line.to_owned();
    }
    tracker::process_image(icon.process())
        .map_or_else(|| "—".to_owned(), |path| names::display_name(&path))
}

fn process_of(window: isize) -> u32 {
    let mut process = 0;
    // SAFETY: Read-only query; a window that is gone leaves zero.
    unsafe { GetWindowThreadProcessId(HWND(window as *mut _), Some(&mut process)) };
    process
}

pub struct TrayMenu {
    hwnd: HWND,
    target: Option<DesktopWindowTarget>,
    /// Everything shown, moved as one.
    sheet: ContainerVisual,
    /// Behind the row under the pointer.
    highlight: Option<ShapeVisual>,
    motion: Motion,
    scale: f32,
    screen: RECT,
    /// The menu bar's button, on screen.
    button: RECT,
    bounds: RECT,
    icons: Vec<TrayIcon>,
    /// The host's count when `icons` was read.
    generation: u64,
    look: appearance::Look,
    hover: Option<Spot>,
    /// Where the left or right button went down.
    pressed: Option<(bool, Spot)>,
    /// A row clicked once, kept a moment for a second click.
    lingering: Option<(usize, Instant)>,
    /// The menu of the program on this row, awaited or open.
    submenu: Option<(usize, Submenu)>,
    closing: Option<Instant>,
    mouse_was_down: bool,
    escape_was_down: bool,
    foreground: isize,
    /// The window in front when the menu opened.
    front: isize,
}

impl TrayMenu {
    /// Opens under the menu bar on `screen`, from its button at `button`.
    pub fn show(
        gfx: &Gfx,
        screen: RECT,
        button: RECT,
        scale: f32,
        host: &TrayHost,
    ) -> Result<Self> {
        let generation = host.generation();
        let icons = host.icons();
        let sheet = gfx.compositor.CreateContainerVisual()?;
        // SAFETY: Class and window belong to this UI thread; the menu owns the
        // HWND as soon as it exists.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("MuelleTrayMenu");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(tray_menu_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            });
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Muelle tray menu"),
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
        let mut menu = Self {
            hwnd,
            target: None,
            sheet,
            highlight: None,
            motion: Motion::new(&gfx.compositor, system::animations_enabled()),
            scale,
            screen,
            button,
            bounds: RECT::default(),
            icons,
            generation,
            look: appearance::menu_bar(),
            hover: None,
            pressed: None,
            lingering: None,
            submenu: None,
            closing: None,
            mouse_was_down: true,
            escape_was_down: false,
            foreground: tracker::foreground_window(),
            front: tracker::foreground_window(),
        };
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
            gfx.compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)?
        };
        target.SetRoot(&menu.sheet)?;
        menu.target = Some(target);
        menu.build(gfx, scale)?;
        menu.motion
            .appear(&menu.sheet, menu.middle(), menu.origin())?;
        menu.place();
        // SAFETY: A timer on our own window.
        unsafe { SetTimer(Some(hwnd), TIMER, TICK_MS, None) };
        Ok(menu)
    }

    /// Shows the window where it belongs, without taking the focus.
    fn place(&self) {
        let bounds = self.bounds;
        // SAFETY: Our own window.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// The middle of the glass, in window pixels.
    fn middle(&self) -> Vector2 {
        Vector2 {
            X: (self.bounds.right - self.bounds.left) as f32 / 2.0,
            Y: (self.bounds.bottom - self.bounds.top) as f32 / 2.0,
        }
    }

    /// The middle of the menu bar's button, in window pixels.
    fn origin(&self) -> Vector2 {
        Vector2 {
            X: ((self.button.left + self.button.right) / 2 - self.bounds.left) as f32,
            Y: ((self.button.top + self.button.bottom) / 2 - self.bounds.top) as f32,
        }
    }

    pub fn refresh_backdrops(&self, gfx: &Gfx) -> Result<()> {
        gfx.refresh_backdrops(&self.sheet)?;
        Ok(())
    }

    /// Sizes the window for the icons and draws every row.
    fn build(&mut self, gfx: &Gfx, wanted: f32) -> Result<()> {
        let (scale, bounds, fits) = placement(self.screen, self.button, wanted, self.icons.len());
        self.icons.truncate(fits);
        self.scale = scale;
        self.bounds = bounds;
        let size = Vector2::new(
            (bounds.right - bounds.left) as f32,
            (bounds.bottom - bounds.top) as f32,
        );
        self.sheet.SetSize(size)?;
        self.sheet
            .SetCenterPoint(Vector3::new(size.X / 2.0, size.Y / 2.0, 0.0))?;
        let layers = self.sheet.Children()?;
        layers.RemoveAll()?;
        let px = |v: f32| (v * scale).round();
        let icons = self.icons.len();
        let panel = Theme::for_controls(self.look).panel(
            gfx,
            Vector2::new(px(WIDTH), px(glass_height(icons))),
            px(CORNER),
            scale,
        )?;
        panel.SetOffset(Vector3::new(px(MARGIN), px(MARGIN), 0.0))?;
        layers.InsertAtTop(&panel)?;
        let light = self.look.light;
        let (ink, wash): (Ink, Color) = if light {
            (Ink::Dark, rgba(0, 0, 0, 26))
        } else {
            (Ink::Light, rgba(255, 255, 255, 40))
        };
        let (highlight, _) = retained_rounded_box(
            &gfx.compositor,
            &layers,
            px(MARGIN + PADDING),
            0.0,
            px(WIDTH - 2.0 * PADDING),
            px(ROW),
            px(CORNER - PADDING),
            wash,
            None,
        )?;
        self.highlight = Some(highlight);
        let ink_color = if light {
            rgba(0, 0, 0, 220)
        } else {
            rgba(255, 255, 255, 240)
        };
        for (index, icon) in self.icons.iter().enumerate() {
            let top = MARGIN + row_top(Spot::Icon(index), icons);
            match &icon.image {
                Some(image) => {
                    let surface = gfx.image_surface(image)?;
                    let brush = gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
                    add_visual(
                        &gfx.compositor,
                        &layers,
                        &brush.cast()?,
                        px(MARGIN + PADDING + 10.0),
                        px(top + (ROW - ICON) / 2.0),
                        px(ICON),
                        px(ICON),
                    )?;
                }
                None => self.symbol(gfx, &layers, NO_PICTURE, top, ink_color)?,
            }
            self.text(gfx, &layers, &label(icon), top, ink)?;
        }
        let top = MARGIN + row_top(Spot::Windows, icons);
        retained_rounded_box(
            &gfx.compositor,
            &layers,
            px(MARGIN + PADDING + 10.0),
            px(top - DIVIDER / 2.0),
            px(WIDTH - 2.0 * PADDING - 20.0),
            scale.round().max(1.0),
            0.0,
            wash,
            None,
        )?;
        self.symbol(gfx, &layers, ARROW, top, ink_color)?;
        self.text(gfx, &layers, strings::text().tray_windows, top, ink)?;
        self.point_at(self.hover)
    }

    /// A glyph where a row's picture goes; `top` is the row's, in the window.
    fn symbol(
        &self,
        gfx: &Gfx,
        layers: &VisualCollection,
        symbol: char,
        top: f32,
        color: Color,
    ) -> Result<()> {
        let side = (ICON * self.scale * 1.25).round();
        let brush = gfx.symbol_brush(symbol, side, 13.0 * self.scale, color)?;
        add_visual(
            &gfx.compositor,
            layers,
            &brush.brush.cast()?,
            (MARGIN + PADDING + 10.0 + ICON / 2.0) * self.scale - side / 2.0,
            (top + ROW / 2.0) * self.scale - side / 2.0,
            brush.width,
            brush.height,
        )?;
        Ok(())
    }

    fn text(
        &self,
        gfx: &Gfx,
        layers: &VisualCollection,
        value: &str,
        top: f32,
        ink: Ink,
    ) -> Result<()> {
        let width = (WIDTH - PADDING - TEXT_LEFT - 10.0) * self.scale;
        let label = gfx.text_brush_fitted(value, self.scale, TEXT, false, ink, width)?;
        add_visual(
            &gfx.compositor,
            layers,
            &label.brush.cast()?,
            ((MARGIN + PADDING + TEXT_LEFT) * self.scale).round(),
            ((top + ROW / 2.0) * self.scale - label.height / 2.0).round(),
            label.width,
            label.height,
        )?;
        Ok(())
    }

    /// Moves the highlight behind `spot`, or hides it.
    fn point_at(&mut self, spot: Option<Spot>) -> Result<()> {
        self.hover = spot;
        let Some(highlight) = &self.highlight else {
            return Ok(());
        };
        highlight.SetIsVisible(spot.is_some())?;
        if let Some(spot) = spot {
            let top = MARGIN + row_top(spot, self.icons.len());
            highlight.SetOffset(Vector3::new(
                ((MARGIN + PADDING) * self.scale).round(),
                (top * self.scale).round(),
                0.0,
            ))?;
        }
        Ok(())
    }

    /// The menu bar's button it hangs from, on screen.
    pub fn button(&self) -> RECT {
        self.button
    }

    pub fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Starts folding back into the button. `false` if there is nothing to
    /// wait for, so the window can go at once.
    pub fn begin_close(&mut self) -> Result<bool> {
        self.dismiss_submenu();
        if !self.motion.enabled() {
            return Ok(false);
        }
        self.closing = Some(Instant::now() + Duration::from_millis(motion::CLOSE_MS));
        self.motion
            .close(&[(self.sheet.clone(), self.middle())], self.origin())?;
        Ok(true)
    }

    /// A menu a program draws itself closes when it loses the focus, which
    /// a click on the menu bar does not take from it.
    fn dismiss_submenu(&mut self) {
        let Some((_, submenu)) = self.submenu.take() else {
            return;
        };
        let front = tracker::foreground_window();
        if submenu.menu() == Some(front) {
            // The window that was in front, or the desktop.
            let before: &[isize] = if self.front == front {
                &[]
            } else {
                std::slice::from_ref(&self.front)
            };
            launcher::focus_front_or_desktop(before);
        }
    }

    /// The periodic check while open.
    pub fn tick(&mut self, gfx: &Gfx, host: Option<&TrayHost>) -> Result<Tick> {
        if let Some(until) = self.closing {
            return Ok(if Instant::now() >= until {
                Tick::Closed
            } else {
                Tick::Keep
            });
        }
        // SAFETY: Read-only keyboard, button and pointer queries.
        let (escape, down, cursor) = unsafe {
            let held = |key: u16| GetAsyncKeyState(i32::from(key)) < 0;
            let down = held(VK_LBUTTON.0) || held(VK_RBUTTON.0) || held(VK_MBUTTON.0);
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            (held(VK_ESCAPE.0), down, cursor)
        };
        let was_down = std::mem::replace(&mut self.mouse_was_down, down);
        let escape_was_down = std::mem::replace(&mut self.escape_was_down, escape);
        let inside = |area: RECT| contains(area, cursor);
        let escape_pressed = escape && !escape_was_down;
        let pressed_outside = down && !was_down && !inside(self.bounds) && !inside(self.button);
        let panel = self.panel();
        if let Some((row, submenu)) = &mut self.submenu {
            let row = *row;
            let windows = tray_host::windows_of(submenu.process);
            let foreground = tracker::foreground_window();
            let step = submenu.look(&Seen {
                windows: &windows,
                foreground,
                foreground_process: process_of(foreground),
                panel,
                cursor,
                pressed_outside,
                escape: escape_pressed,
                now: Instant::now(),
            });
            match step {
                Step::Wait => {}
                Step::Hold(shown) => self.hold(row, shown)?,
                Step::Stay => {
                    self.submenu = None;
                    self.foreground = foreground;
                    self.every(TICK_MS);
                }
                Step::Close => return Ok(Tick::Close),
            }
            return Ok(Tick::Keep);
        }
        if escape_pressed || pressed_outside || tracker::foreground_window() != self.foreground {
            return Ok(Tick::Close);
        }
        // The pointer may leave without the window hearing of it.
        if self.hover.is_some() && !inside(self.bounds) {
            self.point_at(None)?;
        }
        let look = appearance::menu_bar();
        let changed = host.filter(|host| host.generation() != self.generation);
        if changed.is_some() || look != self.look {
            if let Some(host) = changed {
                self.generation = host.generation();
                self.icons = host.icons();
                self.pressed = None;
                self.lingering = None;
                self.hover = None;
            }
            self.look = look;
            self.build(gfx, self.scale)?;
            self.place();
        }
        Ok(Tick::Keep)
    }

    /// A mouse message at `point`, in window pixels.
    pub fn input(&mut self, message: u32, point: POINT) -> Result<Outcome> {
        if self.closing.is_some() {
            return Ok(Outcome::Stay);
        }
        let spot = spot_at(self.scale, self.icons.len(), point);
        if spot != self.hover {
            self.point_at(spot)?;
        }
        let right = matches!(message, WM_RBUTTONDOWN | WM_RBUTTONUP);
        match message {
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                self.pressed = spot.map(|spot| (right, spot));
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let pressed = self.pressed.take();
                // A click: down and up on the same row.
                if let Some(spot) = spot
                    && pressed == Some((right, spot))
                {
                    return Ok(self.activate(spot, right));
                }
            }
            _ => {}
        }
        Ok(Outcome::Stay)
    }

    fn activate(&mut self, spot: Spot, right: bool) -> Outcome {
        let index = match spot {
            Spot::Windows if right => return Outcome::Stay,
            Spot::Windows => {
                let middle = (self.button.left + self.button.right) / 2;
                return Outcome::Windows((middle, self.button.bottom));
            }
            Spot::Icon(index) => index,
        };
        let Some(icon) = self.icons.get(index) else {
            return Outcome::Stay;
        };
        let mut cursor = POINT::default();
        // SAFETY: A read-only query into a live value.
        let _ = unsafe { GetCursorPos(&mut cursor) };
        let again = !right
            && self
                .lingering
                .is_some_and(|(row, until)| row == index && Instant::now() < until);
        let click = if right {
            Click::Right
        } else if again {
            Click::Double
        } else {
            Click::Left
        };
        // What it has on screen now, to tell its menu from it.
        let process = icon.process();
        let before = tray_host::windows_of(process);
        let foreground = tracker::foreground_window();
        if !tray_host::click(icon, click, (cursor.x, cursor.y)) {
            return Outcome::Stay;
        }
        let now = Instant::now();
        let wait = match click {
            Click::Double => return Outcome::Close,
            Click::Right => {
                self.lingering = None;
                MENU_WAIT
            }
            Click::Left => {
                // Some programs open only on a double click.
                // SAFETY: Read-only system setting.
                let wait = Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
                self.lingering = Some((index, now + wait));
                wait + AFTER_DOUBLE
            }
        };
        let submenu = Submenu::new(process, &before, foreground, now, wait);
        self.submenu = Some((index, submenu));
        self.every(MENU_TICK_MS);
        Outcome::Stay
    }

    /// The glass, on screen.
    fn panel(&self) -> RECT {
        let px = |v: f32| (v * self.scale).round() as i32;
        let (left, top) = (self.bounds.left + px(MARGIN), self.bounds.top + px(MARGIN));
        RECT {
            left,
            top,
            right: left + px(WIDTH),
            bottom: top + px(glass_height(self.icons.len())),
        }
    }

    /// Keeps a program's menu beside its row, which stays highlighted.
    fn hold(&mut self, row: usize, menu: Shown) -> Result<()> {
        self.lingering = None;
        if self.hover != Some(Spot::Icon(row)) {
            self.point_at(Some(Spot::Icon(row)))?;
        }
        let top = MARGIN + row_top(Spot::Icon(row), self.icons.len());
        let size = (
            menu.bounds.right - menu.bounds.left,
            menu.bounds.bottom - menu.bounds.top,
        );
        let (x, y) = submenu::beside(
            self.screen,
            self.panel(),
            self.bounds.top + (top * self.scale).round() as i32,
            self.button.bottom,
            size,
            (GAP * self.scale).round() as i32,
        );
        if (x, y) != (menu.bounds.left, menu.bounds.top) {
            tray_host::move_window(menu.window, x, y);
        }
        Ok(())
    }

    /// How often to look from now on.
    fn every(&self, milliseconds: u32) {
        // SAFETY: Our own timer; setting it again replaces it.
        unsafe { SetTimer(Some(self.hwnd), TIMER, milliseconds, None) };
    }
}

impl Drop for TrayMenu {
    fn drop(&mut self) {
        self.dismiss_submenu();
        // Release the composition target before the HWND.
        self.target = None;
        // SAFETY: Our own timer and window.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn tray_menu_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if w.0 == TIMER => app::tray_menu_tick(),
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            app::tray_menu_input(
                msg,
                POINT {
                    x: i32::from((l.0 & 0xFFFF) as i16),
                    y: i32::from((l.0 >> 16) as i16),
                },
            );
        }
        // SAFETY: Standard forwarding of unhandled messages.
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    LRESULT(0)
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

    fn button(left: i32) -> RECT {
        RECT {
            left,
            top: 0,
            right: left + 32,
            bottom: 28,
        }
    }

    #[test]
    fn rows_answer_where_they_are_drawn_at_every_scale() {
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            for spot in [Spot::Icon(0), Spot::Icon(2), Spot::Windows] {
                let middle = POINT {
                    x: ((MARGIN + WIDTH / 2.0) * scale) as i32,
                    y: ((MARGIN + row_top(spot, 3) + ROW / 2.0) * scale) as i32,
                };
                assert_eq!(spot_at(scale, 3, middle), Some(spot), "{scale}");
            }
        }
        // The margin, the glass's rim and the line answer nothing.
        let at = |x: f32, y: f32| {
            spot_at(
                1.0,
                3,
                POINT {
                    x: x as i32,
                    y: y as i32,
                },
            )
        };
        assert_eq!(at(2.0, 60.0), None);
        assert_eq!(at(MARGIN + 2.0, MARGIN + 20.0), None);
        let line = row_top(Spot::Windows, 3) - DIVIDER / 2.0;
        assert_eq!(at(100.0, MARGIN + line), None);
    }

    #[test]
    fn it_hangs_under_its_button_and_stays_on_screen() {
        let (_, bounds, fits) = placement(SCREEN, button(600), 1.0, 6);
        assert_eq!((bounds.left, bounds.top, fits), (588, 28, 6));
        assert_eq!(bounds.bottom - bounds.top, glass_height(6) as i32 + 24);
        // Near the right edge the glass keeps its distance from it.
        let (_, bounds, _) = placement(SCREEN, button(1850), 1.0, 6);
        assert_eq!(bounds.right - MARGIN as i32, 1920 - EDGE as i32);
        // More icons than fit: as many as do.
        let (_, bounds, fits) = placement(SCREEN, button(600), 1.0, 200);
        assert!(fits < 200 && bounds.bottom <= SCREEN.bottom);
    }

    #[test]
    fn a_row_is_named_by_the_first_line_of_its_tooltip() {
        assert_eq!(first_line("\n OneDrive \nUp to date"), Some("OneDrive"));
        assert_eq!(first_line("  \n"), None);
    }
}
