use std::{
    sync::{OnceLock, mpsc},
    time::{Duration, Instant},
};

mod hidden_icons;

use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, RECT, WPARAM},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
        UI::{
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_A, VK_B, VK_D, VK_ESCAPE, VK_LCONTROL,
                VK_LWIN, VK_N, VK_RETURN, VK_S, VK_SPACE, VK_TAB, VK_V, VK_W,
            },
            Shell::ShellExecuteW,
            WindowsAndMessaging::{
                ASFW_ANY, AllowSetForegroundWindow, FindWindowW, GetForegroundWindow,
                GetWindowRect, GetWindowThreadProcessId, HWND_TOP, IsIconic, IsWindowVisible,
                PostMessageW, SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOMOVE,
                SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow,
                ShowWindowAsync, SwitchToThisWindow, WM_CLOSE,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

#[derive(Debug, Error)]
#[error("no se pudo abrir `{id}` (código {code} del shell)")]
pub struct LaunchError {
    id: String,
    code: isize,
}

/// The dock never takes the focus, so Windows may refuse the foreground to the
/// app it starts and open it behind other windows. The click that asked for it
/// was the dock's, which lets the dock hand that right on.
fn allow_foreground() {
    // SAFETY: No pointers; failure only means Windows keeps its default.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

pub fn open(id: &str) -> Result<(), LaunchError> {
    shell_execute(w!("open"), id)
}

/// Opens `id` as administrator, with the UAC prompt, as the taskbar's
/// "Run as administrator" and Ctrl+Shift+click do.
pub fn open_as_admin(id: &str) -> Result<(), LaunchError> {
    shell_execute(w!("runas"), id)
}

fn shell_execute(verb: PCWSTR, id: &str) -> Result<(), LaunchError> {
    allow_foreground();
    // SAFETY: The strings live for the duration of the call.
    let result =
        unsafe { ShellExecuteW(None, verb, &HSTRING::from(id), None, None, SW_SHOWNORMAL) };
    // ShellExecute returns a value greater than 32 on success.
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(LaunchError {
            id: id.to_owned(),
            code,
        })
    }
}

/// Opens `files` with the executable `app`.
pub fn open_with(app: &str, files: &[String]) -> Result<(), LaunchError> {
    let arguments: Vec<String> = files.iter().map(|file| format!("\"{file}\"")).collect();
    allow_foreground();
    // SAFETY: The strings live for the duration of the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(app),
            &HSTRING::from(arguments.join(" ")),
            None,
            SW_SHOWNORMAL,
        )
    };
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(LaunchError {
            id: app.to_owned(),
            code,
        })
    }
}

/// `true` if the window in front belongs to a program running as
/// administrator (Task Manager, an admin terminal, an installer), or to one
/// the dock cannot even inspect, which is never less privileged.
fn foreground_is_elevated() -> bool {
    // SAFETY: Read-only queries on the foreground window.
    let pid = unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        pid
    };
    process_is_elevated(pid)
}

/// See [`foreground_is_elevated`].
#[must_use]
pub fn process_is_elevated(pid: u32) -> bool {
    // SAFETY: Read-only queries; both handles are closed before returning.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true;
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token).is_ok();
        let _ = CloseHandle(process);
        if !opened {
            return true;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let read = GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(token);
        !read || elevation.TokenIsElevated != 0
    }
}

/// Presses the given keys together and releases them.
///
/// Windows drops simulated input aimed at a program running as administrator
/// (UIPI), so with one in front the Windows key did nothing. In that case the
/// dock first takes the focus itself, which it may because the user just
/// clicked it, and the keys then go to an ordinary process.
fn tap(keys: &[VIRTUAL_KEY]) {
    if foreground_is_elevated() {
        // SAFETY: Looks up the dock's own window and focuses it.
        unsafe {
            if let Ok(dock) = FindWindowW(crate::dock_link::DOCK_WINDOW_CLASS, None) {
                bring_to_front(dock);
            }
        }
    }
    send_keys(keys);
}

/// Sends keyboard input without changing focus or injecting mouse input.
fn send_keys(keys: &[VIRTUAL_KEY]) {
    let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&vk| key(vk, KEYBD_EVENT_FLAGS(0)))
        .chain(keys.iter().rev().map(|&vk| key(vk, KEYEVENTF_KEYUP)))
        .collect();
    // SAFETY: The array lives for the duration of the call.
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Opens the Windows Start menu, exactly as the Windows key does.
pub fn open_start_menu() {
    tap(&[VK_LWIN]);
}

/// Opens the Windows 11 quick settings panel (network, volume, battery): Win+A.
pub fn open_quick_settings() {
    tap(&[VK_LWIN, VK_A]);
}

/// Opens the notification center and calendar: Win+N on Windows 11. Windows
/// 10 keeps notifications in the Action Center, which is Win+A there.
pub fn open_notifications() {
    if crate::system::is_windows_11() {
        tap(&[VK_LWIN, VK_N]);
    } else {
        tap(&[VK_LWIN, VK_A]);
    }
}

/// Windows Search, as the taskbar's search button: Win+S.
pub fn open_search() {
    tap(&[VK_LWIN, VK_S]);
}

/// Task View (open windows and virtual desktops): Win+Tab.
pub fn open_task_view() {
    tap(&[VK_LWIN, VK_TAB]);
}

/// The Widgets board: Win+W.
pub fn open_widgets() {
    tap(&[VK_LWIN, VK_W]);
}

/// The sound output page of quick settings (Windows 11 22H2+): Win+Ctrl+V.
/// Windows 10 has no shortcut for its volume flyout, so it opens the sound
/// page of Settings instead.
pub fn open_sound() {
    if crate::system::is_windows_11() {
        tap(&[VK_LWIN, VK_LCONTROL, VK_V]);
    } else {
        let _ = open("ms-settings:sound");
    }
}

/// Shows the desktop, or brings the windows back: what the taskbar's corner
/// button does (Win+D).
pub fn show_desktop() {
    tap(&[VK_LWIN, VK_D]);
}

/// The input language switcher, as clicking the taskbar's language
/// indicator (Win+Space).
pub fn open_input_switcher() {
    tap(&[VK_LWIN, VK_SPACE]);
}

/// Space between the menu bar and the flyout, in physical pixels.
const HIDDEN_ICONS_GAP: i32 = 6;
/// The hidden-icons flyout: Windows 11, then Windows 10.
const OVERFLOW_CLASSES: [PCWSTR; 2] = [
    w!("TopLevelWindowForOverflowXamlIsland"),
    w!("NotifyIconOverflowWindow"),
];

/// Toggles the notification area's hidden icons, the taskbar's "^" button.
/// Win+B focuses that button and Enter opens it; Escape dismisses the flyout.
/// The taskbar itself stays invisible while Muelle runs.
///
/// Windows opens the flyout at the bottom of the screen, above where the
/// taskbar would be, so it is then moved under the arrow that opened it:
/// `anchor` is the arrow's centre and the bottom of the menu bar.
pub fn toggle_hidden_icons(anchor: (i32, i32)) {
    static CLICKS: OnceLock<mpsc::Sender<(i32, i32)>> = OnceLock::new();
    let clicks = CLICKS.get_or_init(|| {
        let (send, receive) = mpsc::channel();
        // A single worker serializes clicks and delayed input. It sleeps on
        // the channel when idle instead of polling the shell indefinitely.
        std::thread::spawn(move || {
            let mut toggle = hidden_icons::Toggle::default();
            let mut anchor = (0, 0);
            loop {
                let click = if toggle.active() {
                    match receive.recv_timeout(Duration::from_millis(10)) {
                        Ok(anchor) => Some(anchor),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match receive.recv() {
                        Ok(anchor) => Some(anchor),
                        Err(_) => break,
                    }
                };
                let flyout = open_overflow();
                let now = Instant::now();
                let action = if let Some(clicked_anchor) = click {
                    anchor = clicked_anchor;
                    toggle.click(now, flyout.is_some())
                } else {
                    toggle.tick(now, flyout.is_some())
                };
                match action {
                    hidden_icons::Action::FocusTray => tap(&[VK_LWIN, VK_B]),
                    hidden_icons::Action::Activate => {
                        // The user may have switched apps during the delay.
                        // Do not deliver the pending Enter to that app.
                        if tray_has_focus() {
                            send_keys(&[VK_RETURN]);
                        }
                    }
                    hidden_icons::Action::Dismiss => dismiss_overflow(flyout),
                    hidden_icons::Action::Reposition => {
                        if let Some(flyout) = flyout {
                            keep_under(flyout, anchor);
                        }
                    }
                    hidden_icons::Action::None => {}
                }
            }
        });
        send
    });
    let _ = clicks.send(anchor);
}

/// Let Explorer dismiss its own flyout rather than hiding or destroying its
/// window. Never send Escape to an unrelated foreground app, or use the
/// mouse-input fallback in `bring_to_front`.
fn dismiss_overflow(flyout: Option<HWND>) {
    // SAFETY: Window queries and focus changes within the current session.
    unsafe {
        if let Some(flyout) = flyout {
            if GetForegroundWindow() != flyout {
                let _ = SetForegroundWindow(flyout);
            }
            if GetForegroundWindow() == flyout {
                send_keys(&[VK_ESCAPE]);
            }
        } else if tray_has_focus() {
            // Cancelling before Enter also releases Win+B's tray focus.
            send_keys(&[VK_ESCAPE]);
        }
    }
}

fn tray_has_focus() -> bool {
    // SAFETY: Read-only lookups of the taskbar and the foreground window.
    unsafe {
        FindWindowW(w!("Shell_TrayWnd"), None).is_ok_and(|taskbar| GetForegroundWindow() == taskbar)
    }
}

/// The hidden-icons flyout, if it is open.
fn open_overflow() -> Option<HWND> {
    OVERFLOW_CLASSES.iter().find_map(|&class| {
        // SAFETY: Read-only window lookups.
        unsafe {
            FindWindowW(class, None)
                .ok()
                .filter(|&window| IsWindowVisible(window).as_bool())
        }
    })
}

/// Moves `flyout` (keeping its size) so it hangs centred under `anchor`,
/// inside the primary monitor. Does nothing once it is there.
fn keep_under(flyout: HWND, (center, top): (i32, i32)) {
    let Some(monitor) = crate::system::primary_monitor() else {
        return;
    };
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out parameter.
    if unsafe { GetWindowRect(flyout, &mut rect) }.is_err() {
        return;
    }
    let width = rect.right - rect.left;
    let screen = monitor.screen;
    let left = (center - width / 2).clamp(
        screen.left + HIDDEN_ICONS_GAP,
        (screen.right - HIDDEN_ICONS_GAP - width).max(screen.left),
    );
    let top = top + HIDDEN_ICONS_GAP;
    if (rect.left, rect.top) == (left, top) {
        return;
    }
    // SAFETY: Moves a window of the same user session; its size and
    // z-order stay as Windows set them.
    unsafe {
        let _ = SetWindowPos(
            flyout,
            None,
            left,
            top,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub fn reveal(path: &str) -> Result<(), LaunchError> {
    let arguments = HSTRING::from(format!("/select,\"{path}\""));
    allow_foreground();
    // SAFETY: The strings live for the duration of the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("explorer.exe"),
            &arguments,
            None,
            SW_SHOWNORMAL,
        )
    };
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(LaunchError {
            id: path.to_owned(),
            code,
        })
    }
}

/// Asks the windows to close, as the close button does; each app can prompt
/// the user to save changes.
pub fn close(windows: &[isize]) {
    for &hwnd in windows {
        // SAFETY: Only a message is sent; if the window no longer exists, the call has no effect.
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Brings an app's windows to the foreground. `windows` is ordered from front
/// to back; the first window receives focus.
///
/// If all windows are minimized, restores only the first one, as macOS does.
pub fn activate(windows: &[isize]) {
    let handles = || windows.iter().map(|&raw| HWND(raw as *mut _));
    // SAFETY: These operations target other windows; calls on a window that no
    // longer exists fail without further effect.
    unsafe {
        // Process from back to front to preserve their relative order.
        for hwnd in handles().rev().filter(|&hwnd| !IsIconic(hwnd).as_bool()) {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        let Some(front) = handles()
            .find(|&hwnd| !IsIconic(hwnd).as_bool())
            .or_else(|| handles().next())
        else {
            return;
        };
        if IsIconic(front).as_bool() {
            let _ = ShowWindow(front, SW_RESTORE);
            // Windows refuses ShowWindow on an app running as administrator
            // (Task Manager, for one). SwitchToThisWindow, what Alt+Tab uses,
            // restores it anyway and also brings it to the front.
            if IsIconic(front).as_bool() {
                SwitchToThisWindow(front, true);
            }
        }
        bring_to_front(front);
    }
}

/// Gives `hwnd` the focus. Windows only lets the process that received the
/// last input do this; when it refuses (the click was some time ago, or
/// another app grabbed the input since), an empty mouse input makes the dock
/// that process again, and the second attempt succeeds.
unsafe fn bring_to_front(hwnd: HWND) {
    // SAFETY: Calls on another window; they fail without effect if it is gone.
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return;
        }
        let nudge = INPUT {
            r#type: INPUT_MOUSE,
            ..Default::default()
        };
        SendInput(&[nudge], size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Minimizes an app's windows, as clicking the active app's taskbar button
/// does. Asynchronous, so an app that is not responding cannot block the dock.
pub fn minimize(windows: &[isize]) {
    for &raw in windows {
        let hwnd = HWND(raw as *mut _);
        // SAFETY: Calls on another window; they fail without effect if it is gone.
        unsafe {
            if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
                let _ = ShowWindowAsync(hwnd, SW_MINIMIZE);
            }
        }
    }
}
