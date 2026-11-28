//! Keyboard handling shared by every text box.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_A, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_END, VK_HOME, VK_LEFT,
    VK_MENU, VK_RIGHT, VK_SHIFT, VK_V, VK_X,
};

use crate::shell;
use crate::textfield::TextField;

pub fn key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(key.0 as i32) < 0 }
}

/// True while Ctrl is held. AltGr reports Ctrl+Alt, which does not count.
pub fn control_down() -> bool {
    key_down(VK_CONTROL) && !key_down(VK_MENU)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Not an editing key.
    Ignored,
    /// The caret or selection changed.
    Moved,
    /// The text changed.
    Changed,
}

/// Applies an editing key to `field`. `window` owns the clipboard.
pub fn edit_key(field: &mut TextField, key: VIRTUAL_KEY, window: HWND) -> Edit {
    let control = control_down();
    let shift = key_down(VK_SHIFT);
    match key {
        VK_BACK => field.backspace(control),
        VK_DELETE => field.delete(control),
        VK_LEFT => {
            field.left(control, shift);
            return Edit::Moved;
        }
        VK_RIGHT => {
            field.right(control, shift);
            return Edit::Moved;
        }
        VK_HOME => {
            field.home(shift);
            return Edit::Moved;
        }
        VK_END => {
            field.end(shift);
            return Edit::Moved;
        }
        VK_A if control => {
            field.select_all();
            return Edit::Moved;
        }
        VK_C | VK_X if control => {
            let Some(text) = field.selected_text() else {
                return Edit::Ignored;
            };
            shell::set_clipboard(window, &text);
            if key == VK_C {
                return Edit::Moved;
            }
            field.delete_selection();
        }
        VK_V if control => match shell::clipboard_text(window) {
            Some(text) => field.insert(&text),
            None => return Edit::Ignored,
        },
        _ => return Edit::Ignored,
    }
    Edit::Changed
}

/// Assembles typed characters, which arrive one UTF-16 unit at a time.
#[derive(Default)]
pub struct Typing {
    high_surrogate: Option<u16>,
}

impl Typing {
    /// Returns the text to insert once a character is complete. Control codes
    /// are handled as key presses instead.
    pub fn unit(&mut self, unit: u16) -> Option<String> {
        if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate = Some(unit);
            return None;
        }
        let units: Vec<u16> = self
            .high_surrogate
            .take()
            .into_iter()
            .chain([unit])
            .collect();
        let text = String::from_utf16_lossy(&units);
        (!text.chars().all(char::is_control)).then_some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_surrogate_pairs_and_skips_controls() {
        let mut typing = Typing::default();
        assert_eq!(typing.unit(0xD83D), None);
        assert_eq!(typing.unit(0xDE00).as_deref(), Some("😀"));
        assert_eq!(typing.unit(8), None);
        assert_eq!(typing.unit(u16::from(b'a')).as_deref(), Some("a"));
    }
}
