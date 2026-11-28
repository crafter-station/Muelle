//! Hides the Windows taskbar while the dock is active and restores its previous state.
//!
//! Auto-hide (which frees the work area) is combined with making the taskbar
//! window invisible and click-through. `ShowWindow(SW_HIDE)` does not work on
//! Windows 11 because Explorer shows the taskbar again.
//!
//! The original auto-hide setting is saved the first time the taskbar is taken
//! over and deleted only when it is restored. If the dock exits unexpectedly
//! and restarts, its own auto-hide setting is not mistaken for the user's.

use std::{
    env, fs,
    io::{self, Write},
    mem::size_of,
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, WPARAM},
        UI::{
            Controls::TTM_POP,
            Shell::{ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage},
            WindowsAndMessaging::{
                FindWindowExW, FindWindowW, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor,
                GetLayeredWindowAttributes, GetWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
                IsWindow, LWA_ALPHA, PostMessageW, SetLayeredWindowAttributes, SetWindowLongPtrW,
                WS_EX_LAYERED, WS_EX_TRANSPARENT,
            },
        },
    },
    core::{PCWSTR, w},
};

const PRIMARY: PCWSTR = w!("Shell_TrayWnd");
const SECONDARY: PCWSTR = w!("Shell_SecondaryTrayWnd");
const GHOST_STYLES: isize = (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as isize;

fn state_file() -> PathBuf {
    PathBuf::from(env::var_os("LOCALAPPDATA").unwrap_or_default())
        .join("Muelle")
        .join("taskbar-original")
}

/// Hides the taskbar on all monitors.
///
/// Idempotent; can be called again after Explorer restarts or Windows redraws
/// the taskbar.
pub fn take_over() -> bool {
    take_over_taskbars(&taskbars(), &state_file())
}

/// Whether Explorer has restored the visibility of a taskbar window.
pub fn needs_visibility_repair() -> bool {
    taskbars().into_iter().any(|hwnd| !is_ghosted(hwnd))
}

fn take_over_taskbars(bars: &[HWND], file: &std::path::Path) -> bool {
    // A logon task can run before Explorer creates Shell_TrayWnd. Saving
    // an absent shell's state would lose the user's original preference.
    // SAFETY: Read-only lookup; a secondary bar alone cannot supply GETSTATE.
    if bars.is_empty() || unsafe { FindWindowW(PRIMARY, None) }.is_err() {
        return false;
    }
    let Some(autohide) = autohide_enabled() else {
        return false;
    };
    let original = match read_original(file) {
        Ok(original) => original,
        Err(error) => {
            crate::log::line(&format!("respaldo de la barra no disponible: {error}"));
            return false;
        }
    };
    if original.is_none() {
        let original = if autohide { "1" } else { "0" };
        let saved = file
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| {
                let mut backup = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(file)?;
                backup.write_all(original.as_bytes())?;
                backup.sync_all()
            });
        if let Err(error) = saved {
            // Never hide the system taskbar unless the guard can restore its
            // original state after a crash or hang.
            crate::log::line(&format!(
                "no se pudo guardar el estado de la barra: {error}"
            ));
            return false;
        }
    }
    if !autohide && !set_autohide(true) {
        return false;
    }
    let mut hidden = true;
    for &hwnd in bars {
        hidden &= is_ghosted(hwnd) || set_ghost(hwnd, true);
    }
    hidden
}

/// What the dock and its watchdog do when the dock stops: give the taskbar
/// back, except while Windows shuts down, restarts or signs out with Muelle
/// set to start again. Then the taskbar stays auto-hidden for the next
/// sign-in; given back, Windows showed it in full at every start until the
/// dock took it over again. Turning autostart off, quitting or uninstalling
/// still gives it back.
pub fn give_back_on_exit() {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};
    // SAFETY: Read-only system query.
    let session_ending = unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) } != 0;
    if session_ending && crate::autostart::is_enabled() {
        return;
    }
    if !give_back() {
        crate::log::line("no se confirmó la restauración de la barra; se conserva el respaldo");
    }
}

/// Restores and verifies the saved preference before removing the backup.
/// Without a backup, reveal the taskbar but preserve Windows' current preference.
pub fn give_back() -> bool {
    restore_saved(&state_file(), |original| {
        let bars = taskbars();
        // SAFETY: Read-only lookup of Explorer's primary taskbar.
        if bars.is_empty() || unsafe { FindWindowW(PRIMARY, None) }.is_err() {
            return original.is_none();
        }
        let mut restored = true;
        for hwnd in bars {
            restored &= set_ghost(hwnd, false);
        }
        if let Some(original) = original {
            restored &= set_autohide(original);
        }
        restored
    })
}

fn read_original(file: &Path) -> io::Result<Option<bool>> {
    match fs::read_to_string(file) {
        Ok(value) => match value.trim() {
            "0" => Ok(Some(false)),
            "1" => Ok(Some(true)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid taskbar-original",
            )),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn restore_saved(file: &Path, mut restore: impl FnMut(Option<bool>) -> bool) -> bool {
    let original = match read_original(file) {
        Ok(original) => original,
        Err(_) => {
            // Recover visibility even if a corrupt record prevents restoring
            // the preference. Keep that record and report failure.
            restore(None);
            return false;
        }
    };
    if !restore(original) {
        return false;
    }
    if original.is_some()
        && let Err(error) = fs::remove_file(file)
        && error.kind() != io::ErrorKind::NotFound
    {
        return false;
    }
    true
}

/// Whether `hwnd` is a taskbar Muelle has hidden. Windows still gives it the
/// foreground when nothing else can take it (the last window closes, a
/// full-screen app ends), and its focused "^" button then shows its tooltip,
/// "Show hidden icons", in a popup of its own over the desktop (issue #48).
#[must_use]
pub fn is_hidden(hwnd: HWND) -> bool {
    hidden_root(hwnd, &taskbars()).is_some()
}

/// Whether `hwnd` is a monitor's taskbar or one of its child controls.
#[must_use]
pub fn is_taskbar(hwnd: HWND) -> bool {
    // Focus may belong to an internal taskbar control, not Shell_TrayWnd.
    // GA_ROOT follows parents only: owned flyouts remain independent windows.
    taskbars().contains(&unsafe { GetAncestor(hwnd, GA_ROOT) })
}

fn hidden_root(hwnd: HWND, bars: &[HWND]) -> Option<HWND> {
    // SAFETY: Read-only lookup; invalid/destroyed handles yield no match.
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    bars.contains(&root)
        .then_some(root)
        .filter(|&bar| is_ghosted(bar))
}

/// Dismisses residual native tooltips owned by a taskbar Muelle has hidden.
/// Called on Dock entry, never per mouse move/frame. Leaves all other apps'
/// tooltips and the notification-area flyout alone; no synchronous Explorer call.
pub fn dismiss_hidden_tooltips() {
    dismiss_taskbar_tooltips(&taskbars());
}

fn dismiss_taskbar_tooltips(bars: &[HWND]) {
    if !bars.iter().any(|&bar| is_ghosted(bar)) {
        return;
    }
    // SAFETY: Read-only class/owner/PID checks, then a pointer-free asynchronous
    // standard control message. No injection, remote memory or focus changes.
    unsafe {
        let mut previous = None;
        while let Ok(tip) = FindWindowExW(None, previous, w!("tooltips_class32"), None) {
            previous = Some(tip);
            let Some(bar) = GetWindow(tip, GW_OWNER)
                .ok()
                .and_then(|owner| hidden_root(owner, bars))
            else {
                continue;
            };
            let (mut bar_pid, mut tip_pid) = (0, 0);
            GetWindowThreadProcessId(bar, Some(&mut bar_pid));
            GetWindowThreadProcessId(tip, Some(&mut tip_pid));
            if bar_pid != 0 && bar_pid == tip_pid {
                let _ = PostMessageW(Some(tip), TTM_POP, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn taskbars() -> Vec<HWND> {
    let mut found = Vec::new();
    // SAFETY: Read-only calls with valid static strings.
    unsafe {
        if let Ok(hwnd) = FindWindowW(PRIMARY, None) {
            found.push(hwnd);
        }
        let mut previous = None;
        while let Ok(hwnd) = FindWindowExW(None, previous, SECONDARY, None) {
            found.push(hwnd);
            previous = Some(hwnd);
        }
    }
    found
}

fn appbar_data() -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: taskbars().first().copied().unwrap_or_default(),
        ..Default::default()
    }
}

fn autohide_enabled() -> Option<bool> {
    let mut data = appbar_data();
    // SAFETY: `data` is a valid APPBARDATA with its size initialized.
    let state = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) };
    // An absent/replaced shell must not be mistaken for autohide being off.
    let present = unsafe {
        !data.hWnd.is_invalid()
            && IsWindow(Some(data.hWnd)).as_bool()
            && FindWindowW(PRIMARY, None).ok() == Some(data.hWnd)
    };
    present.then_some(state as u32 & ABS_AUTOHIDE != 0)
}

fn set_autohide(enabled: bool) -> bool {
    let mut data = appbar_data();
    // SAFETY: Valid output buffer, as in autohide_enabled.
    let current = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) };
    data.lParam = LPARAM(
        ((current as u32 & !ABS_AUTOHIDE) | if enabled { ABS_AUTOHIDE } else { 0 }) as isize,
    );
    // SAFETY: Same as above.
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut data) };
    // SETSTATE always returns TRUE; only GETSTATE verifies the preference.
    autohide_enabled() == Some(enabled)
}

fn set_ghost(hwnd: HWND, ghost: bool) -> bool {
    // SAFETY: `hwnd` comes from FindWindow; if the window no longer exists, the calls have no effect.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ghost {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | GHOST_STYLES);
            if SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA).is_err() {
                return false;
            }
        } else if style & GHOST_STYLES != 0 {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !GHOST_STYLES);
        }
    }
    if ghost {
        is_ghosted(hwnd)
    } else {
        // SAFETY: Read-only verification; destroyed handles must not pass.
        unsafe {
            IsWindow(Some(hwnd)).as_bool()
                && GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & GHOST_STYLES == 0
        }
    }
}

fn is_ghosted(hwnd: HWND) -> bool {
    // SAFETY: Read-only queries with valid outputs. A destroyed window fails the check.
    unsafe {
        if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & GHOST_STYLES != GHOST_STYLES {
            return false;
        }
        let mut alpha = 255;
        let mut flags = Default::default();
        GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok()
            && alpha == 0
            && flags.contains(LWA_ALPHA)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Backup(PathBuf);

    impl Backup {
        fn new(value: &str) -> io::Result<Self> {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "muelle-taskbar-restore-{}-{unique}",
                std::process::id()
            ));
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?
                .write_all(value.as_bytes())?;
            Ok(Self(path))
        }
    }

    impl Drop for Backup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn restoration_verifies_both_original_preferences_before_deleting_the_backup() -> io::Result<()>
    {
        for (saved, enabled) in [("0", false), ("1", true)] {
            let backup = Backup::new(saved)?;
            assert!(restore_saved(&backup.0, |original| {
                assert!(
                    backup.0.exists(),
                    "backup must survive until verification finishes"
                );
                assert_eq!(original, Some(enabled));
                true
            }));
            assert!(!backup.0.exists());
            assert!(restore_saved(&backup.0, |original| original.is_none()));
        }
        Ok(())
    }

    #[test]
    fn a_missing_shell_or_failed_verification_keeps_the_original_for_retry() -> io::Result<()> {
        for saved in ["0", "1"] {
            let backup = Backup::new(saved)?;
            assert!(!restore_saved(&backup.0, |_| false));
            assert_eq!(fs::read_to_string(&backup.0)?, saved);
            assert!(restore_saved(&backup.0, |_| true));
        }
        Ok(())
    }

    #[test]
    fn corrupt_backups_are_kept_without_guessing_the_users_preference() -> io::Result<()> {
        for saved in ["", "2", "invalid"] {
            let backup = Backup::new(saved)?;
            let mut visibility_recovered = false;
            assert!(!restore_saved(&backup.0, |original| {
                assert_eq!(original, None);
                visibility_recovered = true;
                true
            }));
            assert!(visibility_recovered);
            assert_eq!(fs::read_to_string(&backup.0)?, saved);
        }
        Ok(())
    }

    #[test]
    fn an_absent_shell_does_not_save_a_false_original_taskbar_state() {
        let file = env::temp_dir().join(format!("muelle-absent-shell-{}.txt", std::process::id()));
        assert!(!file.exists());
        assert!(!take_over_taskbars(&[], &file));
        assert!(!file.exists());
    }

    #[test]
    fn a_destroyed_taskbar_is_not_reported_as_successfully_hidden() {
        assert!(!set_ghost(HWND::default(), true));
        assert!(!set_ghost(HWND::default(), false));
    }

    #[test]
    fn ghost_visibility_is_verified_and_can_be_restored() -> windows::core::Result<()> {
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP};
        // A hidden test window, never the user's real taskbar.
        unsafe {
            let window = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("Muelle taskbar test"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )?;
            let initially_ghosted = is_ghosted(window);
            let hidden = set_ghost(window, true);
            SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA)?;
            let reset_was_detected = !is_ghosted(window);
            let hidden_again = set_ghost(window, true);
            let restored = set_ghost(window, false);
            let styles = GetWindowLongPtrW(window, GWL_EXSTYLE);
            let _ = DestroyWindow(window);
            assert!(!initially_ghosted);
            assert!(hidden);
            assert!(reset_was_detected);
            assert!(hidden_again);
            assert!(restored);
            assert_eq!(styles & GHOST_STYLES, 0);
        }
        Ok(())
    }

    #[test]
    fn tooltip_cleanup_targets_only_the_hidden_taskbars_owned_control() -> windows::core::Result<()>
    {
        use windows::Win32::UI::{
            Controls::{ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx},
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, MSG, PM_REMOVE, PeekMessageW, WS_CHILD, WS_POPUP,
            },
        };
        struct Windows(Vec<HWND>);
        impl Drop for Windows {
            fn drop(&mut self) {
                for &hwnd in self.0.iter().rev() {
                    // SAFETY: Test-owned windows only, destroyed child-first.
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
        }
        let mut owned = Windows(Vec::new());
        let mut create = |class, style, parent| -> windows::core::Result<HWND> {
            // SAFETY: Hidden offscreen test windows, never the real taskbar.
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    class,
                    w!(""),
                    style,
                    -32000,
                    -32000,
                    1,
                    1,
                    parent,
                    None,
                    None,
                    None,
                )?
            };
            owned.0.push(hwnd);
            Ok(hwnd)
        };
        // SAFETY: Valid initialized size; registers the native tooltip class.
        unsafe {
            InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_WIN95_CLASSES,
            })
            .ok()?;
        }
        let bar = create(w!("STATIC"), WS_POPUP, None)?;
        let child = create(w!("STATIC"), WS_CHILD, Some(bar))?;
        let foreign = create(w!("STATIC"), WS_POPUP, None)?;
        let flyout = create(w!("STATIC"), WS_POPUP, Some(bar))?;
        let tip = create(w!("tooltips_class32"), WS_POPUP, Some(child))?;
        let foreign_tip = create(w!("tooltips_class32"), WS_POPUP, Some(foreign))?;
        let unowned_tip = create(w!("tooltips_class32"), WS_POPUP, None)?;
        let flyout_tip = create(w!("tooltips_class32"), WS_POPUP, Some(flyout))?;
        let has_pop = |hwnd| {
            // SAFETY: Reads only the test thread's messages for its own HWND.
            unsafe {
                PeekMessageW(&mut MSG::default(), Some(hwnd), TTM_POP, TTM_POP, PM_REMOVE).as_bool()
            }
        };
        assert_eq!(hidden_root(child, &[bar]), None);
        dismiss_taskbar_tooltips(&[bar]);
        assert!(!has_pop(tip));
        assert!(set_ghost(bar, true));
        assert_eq!(hidden_root(child, &[bar]), Some(bar));
        assert_eq!(hidden_root(flyout, &[bar]), None);
        assert_eq!(hidden_root(HWND::default(), &[bar]), None);
        dismiss_taskbar_tooltips(&[bar]);
        assert!(has_pop(tip));
        for untouched in [foreign_tip, unowned_tip, flyout_tip] {
            assert!(!has_pop(untouched));
        }
        assert!(set_ghost(bar, false));
        dismiss_taskbar_tooltips(&[bar]);
        assert!(!has_pop(tip));
        Ok(())
    }
}
