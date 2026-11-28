//! How the dock and Settings talk to the search window, which runs in its own
//! process (`muelle.exe --search-worker`).

use std::{env, path::PathBuf};

use muelle_config::Hotkey;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
            SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_APP, WM_CLOSE,
        },
    },
    core::{PCWSTR, w},
};

pub const SEARCH_WINDOW_CLASS: PCWSTR = w!("MuelleSearchWindow");

/// Open the search window.
pub const SHOW: u32 = WM_APP + 1;
/// Read the settings file again.
pub const RELOAD: u32 = WM_APP + 20;
/// With a non-zero parameter, release the shortcut so it can be recorded;
/// with zero, claim it again.
pub const SUSPEND_HOTKEY: u32 = WM_APP + 21;
pub const REBUILD_INDEX: u32 = WM_APP + 22;
pub const CLEAR_HISTORY: u32 = WM_APP + 23;
/// Replies with the shortcut in effect, as `encode_hotkey`.
pub const STATUS: u32 = WM_APP + 24;

/// The running search process's window, if any.
fn window() -> Option<HWND> {
    // SAFETY: Read-only window lookup.
    unsafe { FindWindowW(SEARCH_WINDOW_CLASS, PCWSTR::null()).ok() }
}

#[must_use]
pub fn is_running() -> bool {
    window().is_some()
}

fn post(message: u32) -> bool {
    window().is_some_and(|window| {
        // SAFETY: No pointers or borrowed data cross the process boundary.
        unsafe { PostMessageW(Some(window), message, WPARAM(0), LPARAM(0)).is_ok() }
    })
}

/// Opens the search window. `false` if search is not running.
#[must_use]
pub fn show() -> bool {
    let Some(window) = window() else {
        return false;
    };
    let mut process = 0;
    // SAFETY: Reads the window's process id, then lets that process take the
    // foreground, which the caller may because the user just clicked it.
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut process));
        let _ = AllowSetForegroundWindow(process);
    }
    post(SHOW)
}

/// Asks the search process to apply the saved settings, without waiting.
pub fn reload() {
    let _ = post(RELOAD);
}

/// Asks the search process to exit, without waiting.
pub fn quit() {
    let _ = post(WM_CLOSE);
}

/// Sends a message and waits briefly for the reply. `None` if search is not
/// running or does not answer. Not for the dock's own thread.
#[must_use]
pub fn send(message: u32, parameter: usize) -> Option<isize> {
    let window = window()?;
    let mut reply = 0usize;
    // SAFETY: Only numbers cross the process boundary; the reply outlives the call.
    let delivered = unsafe {
        SendMessageTimeoutW(
            window,
            message,
            WPARAM(parameter),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            2000,
            Some(&mut reply),
        )
    };
    (delivered.0 != 0).then_some(reply as isize)
}

/// Where the dictionary behind definitions may be, best first: next to the
/// program, where `Get-Dictionary.ps1` puts it, and where the standalone
/// Spotlight launcher kept it.
#[must_use]
pub fn dictionary_folders() -> Vec<PathBuf> {
    let beside = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|folder| folder.join("dictionary")));
    let local = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let installed = local
        .iter()
        .flat_map(|local| ["Muelle", "Spotlight"].map(|name| local.join(name).join("dictionary")));
    beside.into_iter().chain(installed).collect()
}

/// Whether one of those folders holds the whole dictionary.
#[must_use]
pub fn dictionary_installed() -> bool {
    dictionary_folders().iter().any(|folder| {
        ["noun", "verb", "adj", "adv"].iter().all(|part| {
            folder.join(format!("index.{part}")).is_file()
                && folder.join(format!("data.{part}")).is_file()
        })
    })
}

const REGISTERED: isize = 1 << 28;
const NOT_REGISTERED: isize = 1 << 29;

/// Packs the shortcut in effect into a message reply.
#[must_use]
pub fn encode_hotkey(hotkey: Option<Hotkey>) -> isize {
    match hotkey {
        Some(h) => REGISTERED | ((h.modifiers as isize) << 16) | h.key as isize,
        None => NOT_REGISTERED,
    }
}

#[must_use]
pub fn decode_hotkey(reply: isize) -> Option<Hotkey> {
    (reply & REGISTERED != 0).then_some(Hotkey {
        modifiers: ((reply >> 16) & 0xF) as u32,
        key: (reply & 0xFFFF) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_survive_a_reply() {
        let hotkey = Hotkey::parse("Win+Ctrl+Alt+Shift+F12");
        assert!(hotkey.is_some());
        assert_eq!(decode_hotkey(encode_hotkey(hotkey)), hotkey);
        assert_eq!(decode_hotkey(encode_hotkey(None)), None);
        assert_ne!(encode_hotkey(None), 0);
    }
}
