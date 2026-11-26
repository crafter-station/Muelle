//! Liquid Glass for other apps' windows, browsers included: a DWM system
//! backdrop (Mica, acrylic or tabbed Mica, Windows 11 22H2 and later) and,
//! optionally, a see-through window.
//!
//! Both work across processes with no code injected into the app. What each
//! window had before is kept in a property on the window itself, so the dock
//! gives it back when the option is turned off or the dock exits, and the
//! guard can do it after a crash without any state of its own. Elevated
//! windows refuse the calls (UIPI) and simply stay as they are.
//!
//! The backdrop shows where the app lets DWM draw: the title bar of apps with
//! a system frame, and any surface the app leaves transparent. Apps that draw
//! their own title bar (Chromium browsers, for instance) show the glass only
//! through the window opacity.

use std::mem::size_of;

use windows::{
    Win32::{
        Foundation::{COLORREF, HANDLE, HWND, LPARAM, WPARAM},
        Graphics::Dwm::{
            DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMSBT_TABBEDWINDOW,
            DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmGetWindowAttribute,
            DwmSetWindowAttribute,
        },
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetPropW, GetWindowLongPtrW, LWA_ALPHA, RemovePropW, SMTO_ABORTIFHUNG,
            SendMessageTimeoutW, SetLayeredWindowAttributes, SetPropW, SetWindowLongPtrW, WM_NULL,
            WS_EX_LAYERED,
        },
    },
    core::w,
};

/// The window property holding what the window had before (see `Original`).
const PROPERTY: windows::core::PCWSTR = w!("Muelle.Glass");

/// The material DWM draws behind the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Mica,
    Acrylic,
    /// Mica tinted by the wallpaper, as tabbed windows use.
    Tabbed,
}

impl Material {
    const fn backdrop(self) -> DWM_SYSTEMBACKDROP_TYPE {
        match self {
            Self::Mica => DWMSBT_MAINWINDOW,
            Self::Acrylic => DWMSBT_TRANSIENTWINDOW,
            Self::Tabbed => DWMSBT_TABBEDWINDOW,
        }
    }
}

/// The glass given to a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glaze {
    pub material: Material,
    /// How opaque the whole window is, 1..=100 percent. 100 leaves it opaque.
    pub opacity: u8,
}

impl Glaze {
    fn alpha(self) -> Option<u8> {
        (self.opacity < 100).then(|| percent_to_alpha(self.opacity))
    }
}

fn percent_to_alpha(percent: u8) -> u8 {
    let percent = u32::from(percent.clamp(1, 100));
    // Rounded, so 100 % is 255 and 50 % is 128.
    u8::try_from((percent * 255 + 50) / 100).unwrap_or(u8::MAX)
}

/// What a window had before the dock glazed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Original {
    /// Its `DWM_SYSTEMBACKDROP_TYPE`, 0 (auto) if it could not be read.
    backdrop: i32,
    /// It was layered already: the app manages its own transparency, which
    /// the dock never touches.
    layered: bool,
}

impl Original {
    /// Never zero, which `GetPropW` returns for "no property".
    fn encode(self) -> usize {
        // Backdrop types are 0..=4; a stray value keeps "auto".
        let backdrop = usize::try_from(self.backdrop).unwrap_or(0) & 0xff;
        0x1_0000 | (usize::from(self.layered) << 8) | backdrop
    }

    fn decode(value: usize) -> Option<Self> {
        (value & 0x1_0000 != 0).then(|| Self {
            backdrop: i32::try_from(value & 0xff).unwrap_or(0),
            layered: value & 0x100 != 0,
        })
    }
}

fn handle(hwnd: isize) -> HWND {
    HWND(hwnd as *mut _)
}

fn original(hwnd: HWND) -> Option<Original> {
    // SAFETY: Reading a property of a window that may have closed only fails.
    let value = unsafe { GetPropW(hwnd, PROPERTY) };
    Original::decode(value.0 as usize)
}

fn read_backdrop(hwnd: HWND) -> i32 {
    let mut backdrop = DWM_SYSTEMBACKDROP_TYPE(0);
    // SAFETY: `backdrop` is a valid i32-sized value the call writes to.
    let read = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw mut backdrop).cast(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )
    };
    read.map_or(0, |()| backdrop.0)
}

fn write_backdrop(hwnd: HWND, backdrop: DWM_SYSTEMBACKDROP_TYPE) {
    // SAFETY: The value outlives the call; before Windows 11 22H2, or for a
    // window that has closed or is elevated, it fails harmlessly.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const backdrop).cast(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        );
    }
}

fn ex_style(hwnd: HWND) -> isize {
    // SAFETY: Reading another process's window style is allowed; a closed
    // window reads 0.
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }
}

/// Whether the window's thread answers within a moment. Changing a style
/// sends it messages and waits for them, so a hung app would hang the dock.
fn responsive(hwnd: HWND) -> bool {
    // SAFETY: WM_NULL does nothing; the call gives up on a hung window at
    // once and on a busy one after the timeout.
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            50,
            None,
        )
    }
    .0 != 0
}

/// `false` if the window did not answer, so the change has to wait.
fn set_layered(hwnd: HWND, layered: bool) -> bool {
    let style = ex_style(hwnd);
    let bit = WS_EX_LAYERED.0 as isize;
    let wanted = if layered { style | bit } else { style & !bit };
    if wanted == style {
        return true;
    }
    if !responsive(hwnd) {
        return false;
    }
    // SAFETY: Only the layered bit changes; a closed or elevated window
    // makes the call fail harmlessly.
    unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted) };
    true
}

/// Gives the window `glaze`, remembering what it had the first time.
/// `false` if the window did not answer and only part of it is applied yet:
/// calling it again later finishes the job.
#[must_use]
pub fn apply(hwnd: isize, glaze: Glaze) -> bool {
    let window = handle(hwnd);
    let before = original(window).unwrap_or_else(|| {
        let before = Original {
            backdrop: read_backdrop(window),
            layered: ex_style(window) & WS_EX_LAYERED.0 as isize != 0,
        };
        // SAFETY: The property holds a plain number, never dereferenced.
        let _ = unsafe { SetPropW(window, PROPERTY, Some(HANDLE(before.encode() as *mut _))) };
        before
    });
    write_backdrop(window, glaze.material.backdrop());
    if before.layered {
        return true;
    }
    match glaze.alpha() {
        Some(alpha) => {
            if !set_layered(window, true) {
                return false;
            }
            // SAFETY: The window is layered now; the call only sets its alpha.
            unsafe {
                let _ = SetLayeredWindowAttributes(window, COLORREF(0), alpha, LWA_ALPHA);
            }
            true
        }
        None => set_layered(window, false),
    }
}

/// Gives back what the window had before `apply`. Harmless on a window the
/// dock never glazed, so the guard can call it for every window.
pub fn restore(hwnd: isize) {
    let window = handle(hwnd);
    let Some(before) = original(window) else {
        return;
    };
    write_backdrop(window, DWM_SYSTEMBACKDROP_TYPE(before.backdrop));
    // A window that does not answer stays see-through until it does and is
    // restored again, or until it closes; the property says it still needs it.
    if !before.layered && !set_layered(window, false) {
        return;
    }
    // SAFETY: Removing our own property; a closed window makes it fail.
    unsafe {
        let _ = RemovePropW(window, PROPERTY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_window_had_survives_the_round_trip_and_is_never_zero() {
        for backdrop in 0..=4 {
            for layered in [false, true] {
                let before = Original { backdrop, layered };
                assert_ne!(before.encode(), 0);
                assert_eq!(Original::decode(before.encode()), Some(before));
            }
        }
        assert_eq!(Original::decode(0), None);
    }

    /// A window in another process: what the dock meets in real apps.
    struct Fixture {
        child: std::process::Child,
        hwnd: isize,
    }

    impl Fixture {
        fn spawn(mode: &str) -> Result<Self, Box<dyn std::error::Error>> {
            use std::io::BufRead;
            let mut child = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "glaze::tests::glaze_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .env("MUELLE_GLAZE_FIXTURE", mode)
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let stdout = child.stdout.take().ok_or("missing fixture stdout")?;
            let hwnd = std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
                .find_map(|line| line.strip_prefix("MUELLE_GLAZE_FIXTURE ")?.parse().ok())
                .ok_or("the fixture printed no window")?;
            Ok(Self { child, hwnd })
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    #[ignore = "Internal subprocess fixture, started only by the native glaze tests"]
    fn glaze_fixture() -> windows::core::Result<()> {
        use std::io::Write;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DispatchMessageW, GetMessageW, MSG, SW_SHOWNOACTIVATE, ShowWindow,
            WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
        };
        // SAFETY: A plain window of the system STATIC class, owned by this thread.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Muelle glaze fixture"),
                WS_OVERLAPPEDWINDOW,
                -32000,
                -32000,
                200,
                150,
                None,
                None,
                None,
                None,
            )?
        };
        // SAFETY: Our own window.
        let _ = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
        println!("MUELLE_GLAZE_FIXTURE {}", window.0 as isize);
        let _ = std::io::stdout().flush();
        if std::env::var("MUELLE_GLAZE_FIXTURE").as_deref() == Ok("frozen") {
            // No message pump: the window never answers.
            std::thread::sleep(std::time::Duration::from_secs(30));
            return Ok(());
        }
        let mut message = MSG::default();
        // SAFETY: A standard message loop on the window's own thread.
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
            unsafe { DispatchMessageW(&message) };
        }
        Ok(())
    }

    fn layered_alpha(hwnd: isize) -> Option<u8> {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetLayeredWindowAttributes, LAYERED_WINDOW_ATTRIBUTES_FLAGS,
        };
        if ex_style(handle(hwnd)) & WS_EX_LAYERED.0 as isize == 0 {
            return None;
        }
        let (mut alpha, mut flags) = (0, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
        // SAFETY: Both outputs are valid for the call.
        unsafe {
            GetLayeredWindowAttributes(handle(hwnd), None, Some(&mut alpha), Some(&mut flags))
        }
        .ok()?;
        Some(alpha)
    }

    #[test]
    #[ignore = "Glazes only windows of test-owned child processes"]
    fn another_apps_window_gets_glass_and_then_exactly_what_it_had()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::spawn("pump")?;
        let hwnd = fixture.hwnd;
        let before = read_backdrop(handle(hwnd));
        assert_eq!(layered_alpha(hwnd), None);

        let acrylic = Glaze {
            material: Material::Acrylic,
            opacity: 80,
        };
        assert!(apply(hwnd, acrylic));
        assert_eq!(read_backdrop(handle(hwnd)), DWMSBT_TRANSIENTWINDOW.0);
        assert_eq!(layered_alpha(hwnd), Some(percent_to_alpha(80)));

        // A new choice replaces the glass but keeps what the window had.
        let mica = Glaze {
            material: Material::Mica,
            opacity: 100,
        };
        assert!(apply(hwnd, mica));
        assert_eq!(read_backdrop(handle(hwnd)), DWMSBT_MAINWINDOW.0);
        assert_eq!(layered_alpha(hwnd), None);
        assert_eq!(
            original(handle(hwnd)).map(|original| original.backdrop),
            Some(before)
        );

        assert!(apply(hwnd, acrylic));
        restore(hwnd);
        assert_eq!(read_backdrop(handle(hwnd)), before);
        assert_eq!(layered_alpha(hwnd), None);
        assert_eq!(original(handle(hwnd)), None);
        // Again, on a window the dock no longer glazes: nothing happens.
        restore(hwnd);
        assert_eq!(read_backdrop(handle(hwnd)), before);
        Ok(())
    }

    #[test]
    #[ignore = "Uses only a test-owned frozen child process"]
    fn a_window_that_does_not_answer_never_holds_the_dock_up()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::spawn("frozen")?;
        let start = std::time::Instant::now();
        let glaze = Glaze {
            material: Material::Acrylic,
            opacity: 80,
        };
        assert!(!apply(fixture.hwnd, glaze), "the opacity has to wait");
        assert_eq!(layered_alpha(fixture.hwnd), None);
        restore(fixture.hwnd);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn full_opacity_leaves_the_window_opaque() {
        let glaze = |opacity| Glaze {
            material: Material::Acrylic,
            opacity,
        };
        assert_eq!(glaze(100).alpha(), None);
        assert_eq!(glaze(99).alpha(), Some(252));
        assert_eq!(glaze(50).alpha(), Some(128));
        assert_eq!(glaze(0).alpha(), Some(3));
    }
}
