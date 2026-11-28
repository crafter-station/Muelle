//! Claiming the global keyboard shortcut.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};

use muelle_config::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT};

const ID: i32 = 1;

pub fn unregister(window: HWND) {
    unsafe {
        let _ = UnregisterHotKey(Some(window), ID);
    }
}

/// Registers `wanted`, or a fallback if another program owns it, replacing any
/// earlier registration. Returns the shortcut now in effect.
pub fn register(window: HWND, wanted: Hotkey) -> Option<Hotkey> {
    unregister(window);
    let space = |modifiers| Hotkey {
        modifiers,
        key: 0x20,
    };
    [
        wanted,
        space(MOD_CONTROL | MOD_ALT),
        space(MOD_CONTROL | MOD_SHIFT),
        space(MOD_ALT | MOD_SHIFT),
    ]
    .into_iter()
    .find(|hotkey| {
        let modifiers = HOT_KEY_MODIFIERS(hotkey.modifiers) | MOD_NOREPEAT;
        unsafe { RegisterHotKey(Some(window), ID, modifiers, hotkey.key).is_ok() }
    })
}
