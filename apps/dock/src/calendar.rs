//! The calendar under the menu bar's clock: a month on glass, with the control
//! center's look and movement. The arrows and the wheel change the month, its
//! name returns to today, and the round button offers what else the clock used
//! to lead to.
//!
//! It exists only while open, and never takes the focus from the app in front.

use std::time::{Duration, Instant};

use anyhow::Result;
use muelle_config::{Language, WeekStart};
use muelle_shell::{dates, system, tracker};
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
                GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, HWND_TOPMOST,
                IDC_ARROW, KillTimer, LoadCursorW, MA_NOACTIVATE, RegisterClassW, SWP_NOACTIVATE,
                SWP_SHOWWINDOW, SetTimer, SetWindowPos, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_TIMER, WNDCLASSW,
                WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_POPUP,
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
/// How often the pointer, keyboard and date are checked, only while open.
const TICK_MS: u32 = 30;
/// How long a newly shown month takes to arrive.
const TURN_MS: i64 = 200;

// Logical pixels.
const WIDTH: f32 = 300.0;
/// Between the glass's sides and the columns.
const SIDE: f32 = 10.0;
const COLUMN: f32 = (WIDTH - 2.0 * SIDE) / 7.0;
/// The middle of the header, and of the weekday letters.
const HEADER_Y: f32 = 28.0;
const LETTERS_Y: f32 = 64.0;
/// The top of the first week.
const GRID_TOP: f32 = 78.0;
const ROW: f32 = 36.0;
const WEEKS: usize = 6;
const HEIGHT: f32 = GRID_TOP + WEEKS as f32 * ROW + 10.0;
/// The circle behind today, and behind what the pointer is on.
const DISC: f32 = 32.0;
const CORNER: f32 = 16.0;
/// Room around the glass inside the window, for its spring.
const MARGIN: f32 = 12.0;
const DROP: f32 = 6.0;
/// Distance kept from the screen's right edge.
const EDGE: f32 = 8.0;
const TITLE_TEXT: f32 = 15.0;
const DAY_TEXT: f32 = 14.0;
const LETTER_TEXT: f32 = 11.0;
const LETTER_OPACITY: f32 = 0.6;
/// Days of the months before and after.
const OUTSIDE_OPACITY: f32 = 0.35;
const PREVIOUS: char = '\u{E76B}';
const NEXT: char = '\u{E76C}';
const MORE: char = '\u{E712}';

/// A day: year, month from 1, day from 1.
pub type Date = (i32, u32, u32);

/// Days from 1 March of year 0 to `date`, in the Gregorian calendar.
fn day_number((year, month, day): Date) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let month = i64::from(month);
    // Years that start in March end with February, so the leap day is last.
    let from_march = if month > 2 { month - 3 } else { month + 9 };
    let before_year = 365 * year + year.div_euclid(4) - year.div_euclid(100) + year.div_euclid(400);
    before_year + (153 * from_march + 2) / 5 + i64::from(day) - 1
}

/// 0 for Monday to 6 for Sunday.
fn weekday(date: Date) -> u8 {
    // 1 March of year 0 was a Wednesday.
    (day_number(date) + 2).rem_euclid(7) as u8
}

fn days_in(year: i32, month: u32) -> u32 {
    let next = if month == 12 {
        (year + 1, 1, 1)
    } else {
        (year, month + 1, 1)
    };
    (day_number(next) - day_number((year, month, 1))) as u32
}

/// The month `by` months after `month` of `year`.
fn moved(year: i32, month: u32, by: i32) -> (i32, u32) {
    let months = year * 12 + month as i32 - 1 + by;
    (months.div_euclid(12), months.rem_euclid(12) as u32 + 1)
}

/// The six weeks shown for a month, starting each on `first` (0 is Monday):
/// the month's days with the end of the one before and the start of the next.
fn weeks(year: i32, month: u32, first: u8) -> [Date; WEEKS * 7] {
    let lead = u32::from((weekday((year, month, 1)) + 7 - first) % 7);
    let (before_year, before) = moved(year, month, -1);
    let (after_year, after) = moved(year, month, 1);
    let (length, before_length) = (days_in(year, month), days_in(before_year, before));
    std::array::from_fn(|cell| {
        let cell = cell as u32;
        if cell < lead {
            (before_year, before, before_length - lead + cell + 1)
        } else if cell < lead + length {
            (year, month, cell - lead + 1)
        } else {
            (after_year, after, cell - lead - length + 1)
        }
    })
}

/// What the pointer can be on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    Previous,
    Next,
    /// The month's name: back to today.
    Title,
    More,
    /// A cell of the six weeks.
    Day(usize),
}

/// The centre of a spot, from the glass's corner.
fn centre(spot: Spot) -> (f32, f32) {
    match spot {
        Spot::Previous => (SIDE + 18.0, HEADER_Y),
        Spot::Next => (SIDE + 52.0, HEADER_Y),
        Spot::Title => (WIDTH / 2.0, HEADER_Y),
        Spot::More => (WIDTH - SIDE - 20.0, HEADER_Y),
        Spot::Day(cell) => (
            SIDE + (cell % 7) as f32 * COLUMN + COLUMN / 2.0,
            GRID_TOP + (cell / 7) as f32 * ROW + ROW / 2.0,
        ),
    }
}

/// The spot under a point of the window, in its pixels.
fn spot_at(scale: f32, point: POINT) -> Option<Spot> {
    let x = point.x as f32 / scale - MARGIN;
    let y = point.y as f32 / scale - MARGIN;
    [Spot::Previous, Spot::Next, Spot::Title, Spot::More]
        .into_iter()
        .chain((0..WEEKS * 7).map(Spot::Day))
        .find(|&spot| {
            let (cx, cy) = centre(spot);
            let (wide, tall) = match spot {
                Spot::Title => (130.0, DISC),
                Spot::Day(_) => (COLUMN, ROW),
                _ => (DISC, DISC),
            };
            x >= cx - wide / 2.0
                && x < cx + wide / 2.0
                && y >= cy - tall / 2.0
                && y < cy + tall / 2.0
        })
}

/// The scale to draw at and the window's place: centred under the clock at
/// `button`, kept on `screen`, and shrunk if it would not fit.
fn placement(screen: RECT, button: RECT, scale: f32) -> (f32, RECT) {
    let room = (screen.bottom - button.bottom) as f32;
    let scale = scale.min(room / (HEIGHT + 2.0 * MARGIN + DROP));
    let px = |v: f32| (v * scale).round() as i32;
    let (width, height) = (px(WIDTH + 2.0 * MARGIN), px(HEIGHT + 2.0 * MARGIN));
    let furthest = screen.right - px(EDGE) + px(MARGIN) - width;
    let left = ((button.left + button.right) / 2 - width / 2)
        .min(furthest)
        .max(screen.left);
    let top = button.bottom;
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

/// The locale whose words the calendar uses, following Muelle's language.
fn locale(language: Language) -> String {
    let (short, fallback) = match language {
        Language::Es => ("es", "es-ES"),
        Language::Fr => ("fr", "fr-FR"),
        Language::Zh => ("zh", "zh-CN"),
        Language::Hi => ("hi", "hi-IN"),
        // Not ar-SA, whose own calendar is not the Gregorian one drawn here.
        Language::Ar => ("ar", "ar-EG"),
        Language::Bn => ("bn", "bn-BD"),
        Language::Pt => ("pt", "pt-BR"),
        Language::En | Language::Auto => ("en", "en-US"),
    };
    dates::locale_for(short, fallback)
}

/// What heads a column: a single capital where the script has them, the
/// locale's shortest name otherwise.
fn letter(language: Language, name: &str) -> String {
    match language {
        Language::Zh | Language::Hi | Language::Ar | Language::Bn => name.to_owned(),
        _ => name
            .chars()
            .next()
            .map(|first| first.to_uppercase().collect())
            .unwrap_or_default(),
    }
}

/// `title` with its first letter a capital, as a heading.
fn heading(title: &str) -> String {
    let mut letters = title.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

fn week_start(chosen: WeekStart) -> u8 {
    match chosen {
        WeekStart::Monday => 0,
        WeekStart::Saturday => 5,
        WeekStart::Sunday => 6,
        WeekStart::Auto => dates::first_weekday(),
    }
}

/// What a click asks of the dock.
#[derive(Clone, Copy, Debug)]
pub enum Outcome {
    Stay,
    /// The round button's menu, below `anchor` and out of `origin`, on screen.
    Menu {
        anchor: POINT,
        origin: POINT,
    },
}

pub struct Calendar {
    hwnd: HWND,
    target: Option<DesktopWindowTarget>,
    /// Everything shown, moved as one.
    sheet: ContainerVisual,
    /// Behind what the pointer is on.
    highlight: Option<ShapeVisual>,
    motion: Motion,
    scale: f32,
    bounds: RECT,
    /// The menu bar's clock, on screen.
    button: RECT,
    /// The day the week starts on, 0 being Monday.
    first: u8,
    today: Date,
    /// The month shown.
    month: (i32, u32),
    look: appearance::Look,
    hover: Option<Spot>,
    pressed: Option<Spot>,
    /// Its menu is open: it stays whatever the menu does.
    held: bool,
    closing: Option<Instant>,
    mouse_was_down: bool,
    escape_was_down: bool,
    foreground: isize,
}

impl Calendar {
    /// Opens under the menu bar on `screen`, from its clock at `button`.
    pub fn show(
        gfx: &Gfx,
        screen: RECT,
        button: RECT,
        scale: f32,
        start: WeekStart,
    ) -> Result<Self> {
        let (scale, bounds) = placement(screen, button, scale);
        let size = Vector2::new(
            (bounds.right - bounds.left) as f32,
            (bounds.bottom - bounds.top) as f32,
        );
        let sheet = gfx.compositor.CreateContainerVisual()?;
        sheet.SetSize(size)?;
        sheet.SetCenterPoint(Vector3::new(size.X / 2.0, size.Y / 2.0, 0.0))?;
        // SAFETY: Class and window belong to this UI thread; the calendar owns
        // the HWND as soon as it exists.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("MuelleCalendar");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(calendar_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: class,
                ..Default::default()
            });
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Muelle calendar"),
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
        let today = dates::today();
        let mut calendar = Self {
            hwnd,
            target: None,
            sheet,
            highlight: None,
            motion: Motion::new(&gfx.compositor, system::animations_enabled()),
            scale,
            bounds,
            button,
            first: week_start(start),
            today,
            month: (today.0, today.1),
            look: appearance::menu_bar(),
            hover: None,
            pressed: None,
            held: false,
            closing: None,
            mouse_was_down: true,
            escape_was_down: false,
            foreground: tracker::foreground_window(),
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
        target.SetRoot(&calendar.sheet)?;
        calendar.target = Some(target);
        calendar.build(gfx, false)?;
        calendar
            .motion
            .appear(&calendar.sheet, calendar.middle(), calendar.origin())?;
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
        Ok(calendar)
    }

    /// The middle of the glass, in window pixels.
    fn middle(&self) -> Vector2 {
        Vector2 {
            X: (self.bounds.right - self.bounds.left) as f32 / 2.0,
            Y: (self.bounds.bottom - self.bounds.top) as f32 / 2.0,
        }
    }

    /// The middle of the clock, in window pixels.
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

    /// Draws the month shown; with `turned`, its days arrive gently.
    fn build(&mut self, gfx: &Gfx, turned: bool) -> Result<()> {
        let c = &gfx.compositor;
        let scale = self.scale;
        let px = |v: f32| (v * scale).round();
        let layers = self.sheet.Children()?;
        layers.RemoveAll()?;
        let panel = Theme::for_controls(self.look).panel(
            gfx,
            Vector2::new(px(WIDTH), px(HEIGHT)),
            px(CORNER),
            scale,
        )?;
        panel.SetOffset(Vector3::new(px(MARGIN), px(MARGIN), 0.0))?;
        layers.InsertAtTop(&panel)?;
        let light = self.look.light;
        let (ink, ink_color, wash) = if light {
            (Ink::Dark, rgba(0, 0, 0, 220), rgba(0, 0, 0, 24))
        } else {
            (
                Ink::Light,
                rgba(255, 255, 255, 240),
                rgba(255, 255, 255, 36),
            )
        };
        let (highlight, _) = retained_rounded_box(
            c,
            &layers,
            0.0,
            0.0,
            px(DISC),
            px(DISC),
            px(DISC) / 2.0,
            wash,
            None,
        )?;
        self.highlight = Some(highlight);

        let language = strings::language();
        let locale = locale(language);
        let (year, month) = self.month;
        let title = dates::month_title(&locale, year, month)
            .map_or_else(|| format!("{month:02}/{year}"), |title| heading(&title));
        self.text(
            gfx,
            &layers,
            &title,
            centre(Spot::Title),
            TITLE_TEXT,
            true,
            ink,
            1.0,
        )?;
        for (spot, symbol) in [
            (Spot::Previous, PREVIOUS),
            (Spot::Next, NEXT),
            (Spot::More, MORE),
        ] {
            let (x, y) = centre(spot);
            let side = px(22.0);
            let brush = gfx.symbol_brush(symbol, side, 12.0 * scale, ink_color)?;
            add_visual(
                c,
                &layers,
                &brush.brush.cast()?,
                px(MARGIN + x) - brush.width / 2.0,
                px(MARGIN + y) - brush.height / 2.0,
                brush.width,
                brush.height,
            )?;
        }
        // The ring around the round button.
        let (x, y) = centre(Spot::More);
        retained_rounded_box(
            c,
            &layers,
            px(MARGIN + x - 10.0),
            px(MARGIN + y - 10.0),
            px(20.0),
            px(20.0),
            px(10.0),
            rgba(0, 0, 0, 0),
            Some(ink_color),
        )?;
        for column in 0..7u8 {
            let day = (self.first + column) % 7;
            let name = dates::shortest_day_name(&locale, day).unwrap_or_default();
            let (x, _) = centre(Spot::Day(usize::from(column)));
            self.text(
                gfx,
                &layers,
                &letter(language, &name),
                (x, LETTERS_Y),
                LETTER_TEXT,
                true,
                ink,
                LETTER_OPACITY,
            )?;
        }

        let grid = c.CreateContainerVisual()?;
        grid.SetSize(self.sheet.Size()?)?;
        let days = grid.Children()?;
        let [r, g, b] = system::accent_color().unwrap_or([10, 132, 255]);
        let accent: Color = rgba(r, g, b, 255);
        for (cell, date) in weeks(year, month, self.first).into_iter().enumerate() {
            let at = centre(Spot::Day(cell));
            let today = date == self.today;
            let here = date.1 == month;
            if today {
                retained_rounded_box(
                    c,
                    &days,
                    px(MARGIN + at.0 - DISC / 2.0),
                    px(MARGIN + at.1 - DISC / 2.0),
                    px(DISC),
                    px(DISC),
                    px(DISC) / 2.0,
                    accent,
                    None,
                )?;
            }
            self.text(
                gfx,
                &days,
                &date.2.to_string(),
                at,
                DAY_TEXT,
                today,
                // White on the accent circle, in either theme.
                if today { Ink::Light } else { ink },
                if here || today { 1.0 } else { OUTSIDE_OPACITY },
            )?;
        }
        layers.InsertAtTop(&grid)?;
        if turned && self.motion.enabled() {
            let arrive = c.CreateScalarKeyFrameAnimation()?;
            arrive.InsertKeyFrame(0.0, 0.0)?;
            arrive.InsertKeyFrame(1.0, 1.0)?;
            arrive.SetDuration(windows::Foundation::TimeSpan {
                Duration: TURN_MS * 10_000,
            })?;
            grid.StartAnimation(windows::core::h!("Opacity"), &arrive)?;
        }
        self.point_at(self.hover)
    }

    /// One line of text centred on `at`, from the glass's corner.
    #[expect(
        clippy::too_many_arguments,
        reason = "Typography and placement are chosen together for each label"
    )]
    fn text(
        &self,
        gfx: &Gfx,
        layers: &VisualCollection,
        value: &str,
        at: (f32, f32),
        size: f32,
        bold: bool,
        ink: Ink,
        opacity: f32,
    ) -> Result<()> {
        if value.is_empty() {
            return Ok(());
        }
        let label = gfx.text_brush_sized(value, self.scale, size, bold, ink)?;
        add_visual(
            &gfx.compositor,
            layers,
            &label.brush.cast()?,
            ((MARGIN + at.0) * self.scale - label.width / 2.0).round(),
            ((MARGIN + at.1) * self.scale - label.height / 2.0).round(),
            label.width,
            label.height,
        )?
        .SetOpacity(opacity)?;
        Ok(())
    }

    /// Moves the highlight behind `spot`, or hides it.
    fn point_at(&mut self, spot: Option<Spot>) -> Result<()> {
        self.hover = spot;
        let Some(highlight) = &self.highlight else {
            return Ok(());
        };
        // The month's name has no circle of its own.
        let spot = spot.filter(|&spot| spot != Spot::Title);
        highlight.SetIsVisible(spot.is_some())?;
        if let Some(spot) = spot {
            let (x, y) = centre(spot);
            highlight.SetOffset(Vector3::new(
                ((MARGIN + x - DISC / 2.0) * self.scale).round(),
                ((MARGIN + y - DISC / 2.0) * self.scale).round(),
                0.0,
            ))?;
        }
        Ok(())
    }

    /// Shows `month` of `year`.
    fn turn_to(&mut self, gfx: &Gfx, month: (i32, u32)) -> Result<()> {
        // The years Windows can name.
        if self.month == month || !(1601..=9999).contains(&month.0) {
            return Ok(());
        }
        self.month = month;
        self.build(gfx, true)
    }

    pub fn show_today(&mut self, gfx: &Gfx) -> Result<()> {
        self.turn_to(gfx, (self.today.0, self.today.1))
    }

    /// The wheel turned: away from the user, to the month before.
    pub fn scrolled(&mut self, gfx: &Gfx, delta: i16) -> Result<()> {
        if self.closing.is_some() || delta == 0 {
            return Ok(());
        }
        let by = if delta > 0 { -1 } else { 1 };
        self.turn_to(gfx, moved(self.month.0, self.month.1, by))
    }

    /// While its menu is open, nothing the menu does closes the calendar.
    pub fn hold(&mut self, held: bool) {
        self.held = held;
        // The press that closed the menu is not a press elsewhere.
        self.mouse_was_down = true;
        self.foreground = tracker::foreground_window();
    }

    /// The menu bar's button it hangs from, on screen.
    pub fn button(&self) -> RECT {
        self.button
    }

    pub fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Starts folding back into the clock. `false` if there is nothing to
    /// wait for, so the window can go at once.
    pub fn begin_close(&mut self) -> Result<bool> {
        if !self.motion.enabled() {
            return Ok(false);
        }
        self.closing = Some(Instant::now() + Duration::from_millis(motion::CLOSE_MS));
        self.motion
            .close(&[(self.sheet.clone(), self.middle())], self.origin())?;
        Ok(true)
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
        if self.held {
            return Ok(Tick::Keep);
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
        let inside = |area: RECT| {
            cursor.x >= area.left
                && cursor.x < area.right
                && cursor.y >= area.top
                && cursor.y < area.bottom
        };
        let pressed_outside = down && !was_down && !inside(self.bounds) && !inside(self.button);
        if (escape && !escape_was_down)
            || pressed_outside
            || tracker::foreground_window() != self.foreground
        {
            return Ok(Tick::Close);
        }
        // The pointer may leave without the window hearing of it.
        if self.hover.is_some() && !inside(self.bounds) {
            self.point_at(None)?;
        }
        let (look, today) = (appearance::menu_bar(), dates::today());
        if look != self.look || today != self.today {
            self.look = look;
            self.today = today;
            self.build(gfx, false)?;
        }
        Ok(Tick::Keep)
    }

    /// A mouse message at `point`, in window pixels.
    pub fn input(&mut self, gfx: &Gfx, message: u32, point: POINT) -> Result<Outcome> {
        if self.closing.is_some() {
            return Ok(Outcome::Stay);
        }
        let spot = spot_at(self.scale, point);
        if spot != self.hover {
            self.point_at(spot)?;
        }
        match message {
            WM_LBUTTONDOWN => self.pressed = spot,
            // A click: down and up on the same spot.
            WM_LBUTTONUP if self.pressed.take() == spot => {
                if let Some(spot) = spot {
                    return self.activate(gfx, spot);
                }
            }
            _ => {}
        }
        Ok(Outcome::Stay)
    }

    fn activate(&mut self, gfx: &Gfx, spot: Spot) -> Result<Outcome> {
        let (year, month) = self.month;
        match spot {
            Spot::Previous => self.turn_to(gfx, moved(year, month, -1))?,
            Spot::Next => self.turn_to(gfx, moved(year, month, 1))?,
            Spot::Title => self.show_today(gfx)?,
            // A day of another month leads there.
            Spot::Day(cell) => {
                if let Some(&(year, month, _)) = weeks(year, month, self.first).get(cell) {
                    self.turn_to(gfx, (year, month))?;
                }
            }
            Spot::More => {
                let (x, y) = centre(Spot::More);
                let on_screen = |x: f32, y: f32| POINT {
                    x: self.bounds.left + ((MARGIN + x) * self.scale).round() as i32,
                    y: self.bounds.top + ((MARGIN + y) * self.scale).round() as i32,
                };
                return Ok(Outcome::Menu {
                    anchor: on_screen(x, y + 14.0),
                    origin: on_screen(x, y),
                });
            }
        }
        Ok(Outcome::Stay)
    }
}

impl Drop for Calendar {
    fn drop(&mut self) {
        // Release the composition target before the HWND.
        self.target = None;
        // SAFETY: Our own timer and window.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn calendar_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if w.0 == TIMER => app::calendar_tick(),
        WM_MOUSEWHEEL => app::calendar_scrolled((w.0 >> 16) as u16 as i16),
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
            app::calendar_input(
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

    #[test]
    fn august_2023_starting_on_monday_is_laid_out_as_on_a_mac() {
        let days = weeks(2023, 8, 0);
        assert_eq!(days[0], (2023, 7, 31));
        assert_eq!(days[1], (2023, 8, 1));
        assert_eq!(days[29], (2023, 8, 29));
        assert_eq!(days[31], (2023, 8, 31));
        assert_eq!(days[32], (2023, 9, 1));
        assert_eq!(days[41], (2023, 9, 10));
    }

    #[test]
    fn weekdays_month_lengths_and_turning_years_are_right() {
        // 28 November 2026 is a Saturday; 1 January 2000 was one too.
        assert_eq!(weekday((2026, 11, 28)), 5);
        assert_eq!(weekday((2000, 1, 1)), 5);
        assert_eq!(weekday((2024, 2, 29)), 3);
        for (year, length) in [(2023, 28), (2024, 29), (1900, 28), (2000, 29)] {
            assert_eq!(days_in(year, 2), length, "{year}");
        }
        assert_eq!(moved(2026, 12, 1), (2027, 1));
        assert_eq!(moved(2026, 1, -1), (2025, 12));
        assert_eq!(moved(2026, 5, -17), (2024, 12));
    }

    #[test]
    fn every_month_starts_in_its_first_week_whatever_day_that_starts_on() {
        for year in [1999, 2024, 2026] {
            for month in 1..=12 {
                for first in 0..7 {
                    let days = weeks(year, month, first);
                    assert_eq!(weekday(days[0]), first);
                    let start = days.iter().position(|&day| day == (year, month, 1));
                    assert!(start.is_some_and(|at| at < 7), "{year}-{month} {first}");
                    // Consecutive days, with none skipped between months.
                    for pair in days.windows(2) {
                        assert_eq!(day_number(pair[1]) - day_number(pair[0]), 1);
                    }
                }
            }
        }
    }

    #[test]
    fn clicks_find_their_spot_at_every_scale_and_nothing_between() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let at = |x: f32, y: f32| {
                spot_at(
                    scale,
                    POINT {
                        x: ((MARGIN + x) * scale) as i32,
                        y: ((MARGIN + y) * scale) as i32,
                    },
                )
            };
            for spot in [
                Spot::Previous,
                Spot::Next,
                Spot::Title,
                Spot::More,
                Spot::Day(0),
                Spot::Day(20),
                Spot::Day(41),
            ] {
                let (x, y) = centre(spot);
                assert_eq!(at(x, y), Some(spot), "{spot:?} at {scale}");
            }
            assert_eq!(at(WIDTH / 2.0, LETTERS_Y), None);
            assert_eq!(at(-4.0, GRID_TOP + 4.0), None);
        }
    }

    #[test]
    fn it_hangs_under_the_clock_and_stays_on_screen() {
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let clock = |left| RECT {
            left,
            top: 0,
            right: left + 120,
            bottom: 28,
        };
        let (scale, bounds) = placement(screen, clock(900), 1.0);
        assert_eq!(scale, 1.0);
        assert_eq!((bounds.left + bounds.right) / 2, 960);
        // Near the edge, the glass keeps its distance from it.
        let (_, bounds) = placement(screen, clock(1790), 1.0);
        assert_eq!(bounds.right - MARGIN as i32, screen.right - EDGE as i32);
        let short = RECT {
            bottom: 300,
            ..screen
        };
        let (scale, bounds) = placement(short, clock(900), 2.0);
        assert!(scale < 2.0 && bounds.bottom <= short.bottom);
    }

    #[test]
    fn headings_are_capitals_where_the_script_has_them() {
        assert_eq!(letter(Language::En, "Mo"), "M");
        assert_eq!(letter(Language::Es, "lu"), "L");
        assert_eq!(letter(Language::Zh, "一"), "一");
        assert_eq!(heading("noviembre de 2026"), "Noviembre de 2026");
        assert_eq!(heading(""), "");
        assert_eq!(week_start(WeekStart::Sunday), 6);
        assert_eq!(week_start(WeekStart::Saturday), 5);
    }
}
