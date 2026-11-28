//! Keyboard ownership lasts only for an explicitly opened music popup.
//! Ordinary window messages, no global shortcuts or keyboard interception.

use super::{Gfx, Island, MediaControl, MenuBar};
use anyhow::Result;
use windows::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::{GetKeyState, SetFocus},
        WindowsAndMessaging::{
            GetForegroundWindow, GetShellWindow, IsIconic, IsWindow, IsWindowVisible,
            SetForegroundWindow,
        },
    },
};

pub(super) struct MusicKeyboard {
    owner: HWND,
    previous: HWND,
}

impl MusicKeyboard {
    fn acquire(owner: HWND) -> Option<Self> {
        // SetForegroundWindow is allowed after the user's explicit click.
        // WS_EX_NOACTIVATE continues protecting other bar/popup interactions.
        unsafe {
            let previous = GetForegroundWindow();
            let _ = SetForegroundWindow(owner);
            if GetForegroundWindow() != owner {
                return None;
            }
            let _ = SetFocus(Some(owner));
            Some(Self { owner, previous })
        }
    }
    pub(super) fn owns_focus(&self) -> bool {
        unsafe { GetForegroundWindow() == self.owner }
    }
}

impl Drop for MusicKeyboard {
    fn drop(&mut self) {
        // Closing by Escape restores the former app; clicking/Alt-Tabbing to
        // another window must never activate an unrelated app over it.
        unsafe {
            if self.owns_focus() {
                let target = if self.previous != self.owner
                    && IsWindow(Some(self.previous)).as_bool()
                    && IsWindowVisible(self.previous).as_bool()
                    && !IsIconic(self.previous).as_bool()
                {
                    self.previous
                } else {
                    GetShellWindow()
                };
                // A previous window may have closed/minimized while the popup
                // was open. Do not leave the persistent bar holding keyboard input.
                let _ = SetForegroundWindow(target);
            }
        }
    }
}

fn space_action(key: u16, repeated: bool, modified: bool) -> bool {
    key == 0x20 && !repeated && !modified
}

impl MenuBar {
    fn music_shown(&self) -> bool {
        self.panel == Some(Island::Music)
            && self.visible
            && (!self.auto_hide || self.revealed)
            && self.panel_span.width() > 0.0
    }

    pub(super) fn focus_music(&mut self) {
        if self.music_shown() && self.keyboard.is_none() {
            self.keyboard = MusicKeyboard::acquire(self.hwnd);
        }
    }

    pub(super) fn release_music_if_hidden(&mut self) {
        if !self.music_shown() {
            self.keyboard = None;
        }
    }

    pub fn music_focus_lost(&mut self, gfx: &Gfx) -> Result<()> {
        if self
            .keyboard
            .as_ref()
            .is_some_and(|keyboard| !keyboard.owns_focus())
        {
            self.dismiss_island(gfx)?;
        }
        Ok(())
    }

    pub fn music_key(&mut self, gfx: &Gfx, key: u16, repeated: bool) -> Result<()> {
        if !self.music_shown()
            || !self
                .keyboard
                .as_ref()
                .is_some_and(MusicKeyboard::owns_focus)
        {
            return Ok(());
        }
        if key == 0x1b {
            return self.dismiss_island(gfx);
        }
        // GetKeyState reads this window's keyboard queue, only on key input.
        let modified = [0x10, 0x11, 0x12, 0x5b, 0x5c]
            .iter()
            .any(|key| unsafe { GetKeyState(*key) } < 0);
        if space_action(key, repeated, modified)
            && let Some(track) = self
                .look
                .as_ref()
                .and_then(|look| look.track.as_ref())
                .filter(|track| track.toggle)
            && let Some(media) = &self.media
        {
            media.send(track, MediaControl::PlayPause);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_toggles_once_per_press_and_never_takes_modified_shortcuts() {
        assert!(space_action(0x20, false, false));
        assert!(!space_action(0x20, true, false));
        assert!(!space_action(0x20, false, true));
        assert!(!space_action(0x41, false, false));
    }

    #[test]
    #[ignore = "Requires an interactive desktop permitting foreground activation; uses only offscreen test windows and restores focus"]
    fn native_music_keyboard_restores_focus_but_never_takes_it_from_another_window() -> Result<()> {
        use windows::Win32::{
            Foundation::HMODULE,
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                DefWindowProcW, RegisterClassW, UnregisterClassW, WNDCLASSW,
            },
        };
        use windows::{
            Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, SW_SHOWNOACTIVATE, ShowWindow, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_POPUP,
            },
            core::w,
        };
        unsafe extern "system" fn fixture_proc(
            hwnd: HWND,
            message: u32,
            w: windows::Win32::Foundation::WPARAM,
            l: windows::Win32::Foundation::LPARAM,
        ) -> windows::Win32::Foundation::LRESULT {
            unsafe { DefWindowProcW(hwnd, message, w, l) }
        }
        let module = unsafe { GetModuleHandleW(None)? };
        let class = w!("Muelle.Music.Keyboard.Fixture");
        anyhow::ensure!(
            unsafe {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(fixture_proc),
                    hInstance: module.into(),
                    lpszClassName: class,
                    ..Default::default()
                })
            } != 0,
            "could not register fixture window class"
        );
        struct Windows {
            previous: HWND,
            owned: Vec<HWND>,
            module: HMODULE,
        }
        impl Drop for Windows {
            fn drop(&mut self) {
                unsafe {
                    let restore = self.owned.contains(&GetForegroundWindow());
                    for window in &self.owned {
                        let _ = DestroyWindow(*window);
                    }
                    let _ = UnregisterClassW(
                        w!("Muelle.Music.Keyboard.Fixture"),
                        Some(self.module.into()),
                    );
                    if restore {
                        let _ = SetForegroundWindow(self.previous);
                    }
                }
            }
        }
        let mut windows = Windows {
            previous: unsafe { GetForegroundWindow() },
            owned: Vec::new(),
            module,
        };
        for _ in 0..3 {
            let window = unsafe {
                CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    class,
                    w!("Muelle keyboard fixture"),
                    WS_POPUP,
                    -32000,
                    -32000,
                    2,
                    2,
                    None,
                    None,
                    Some(module.into()),
                    None,
                )?
            };
            windows.owned.push(window);
            unsafe {
                let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            }
        }
        let (previous, owner, other) = (windows.owned[0], windows.owned[1], windows.owned[2]);
        anyhow::ensure!(
            unsafe { SetForegroundWindow(previous) }.as_bool(),
            "test desktop denied foreground activation"
        );
        let keyboard = MusicKeyboard::acquire(owner)
            .ok_or_else(|| anyhow::anyhow!("popup could not acquire focus"))?;
        assert!(keyboard.owns_focus());
        drop(keyboard);
        assert_eq!(unsafe { GetForegroundWindow() }, previous);
        let keyboard = MusicKeyboard::acquire(owner)
            .ok_or_else(|| anyhow::anyhow!("popup could not reacquire focus"))?;
        anyhow::ensure!(
            unsafe { SetForegroundWindow(other) }.as_bool(),
            "test focus transfer failed"
        );
        drop(keyboard);
        assert_eq!(
            unsafe { GetForegroundWindow() },
            other,
            "dismissing must not steal focus back"
        );
        Ok(())
    }
}
