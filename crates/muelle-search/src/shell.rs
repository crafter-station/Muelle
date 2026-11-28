//! Thin wrappers over the Windows shell and clipboard.

use std::path::PathBuf;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Shell::{
    ILFree, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHGetKnownFolderPath,
    SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{GUID, HSTRING, PCWSTR, PWSTR};

/// A null-terminated UTF-16 copy of `text`.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Copies a string allocated by the shell, then frees the original.
///
/// # Safety
/// `text` must be a valid string from `CoTaskMemAlloc`, not used afterwards.
pub unsafe fn take_string(text: PWSTR) -> String {
    unsafe {
        let copy = String::from_utf16_lossy(text.as_wide());
        CoTaskMemFree(Some(text.0 as *const _));
        copy
    }
}

pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        Some(PathBuf::from(take_string(path)))
    }
}

/// Runs `action` on a new thread with COM ready, so a slow shell call (a
/// network path, say) never freezes the search window.
fn on_shell_thread(action: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        action();
    });
}

/// Opens a file, folder, app or URL the same way Explorer would.
/// Windows shows its own message if this fails.
pub fn open(target: String) {
    on_shell_thread(move || unsafe {
        let file = HSTRING::from(target.as_str());
        // Programs expect to start in their own folder, not in ours.
        let directory = std::path::Path::new(&target)
            .parent()
            .filter(|p| p.is_absolute() && p.is_dir())
            .map(|p| HSTRING::from(p.as_os_str()));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpFile: PCWSTR(file.as_ptr()),
            lpDirectory: directory
                .as_ref()
                .map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let _ = ShellExecuteExW(&mut info);
    });
}

/// Shows a file or folder selected in an Explorer window.
pub fn reveal(path: String) {
    on_shell_thread(move || unsafe {
        let mut item = std::ptr::null_mut();
        let name = HSTRING::from(path.as_str());
        if SHParseDisplayName(&name, None, &mut item, 0, None).is_ok() {
            let _ = SHOpenFolderAndSelectItems(item, None, 0);
            ILFree(Some(item));
        }
    });
}

pub fn set_clipboard(owner: HWND, text: &str) {
    let data = wide(text);
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return;
        }
        if EmptyClipboard().is_ok()
            && let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2)
        {
            let pointer = GlobalLock(memory) as *mut u16;
            let mut stored = false;
            if !pointer.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
                let _ = GlobalUnlock(memory);
                // On success the clipboard owns the memory.
                stored = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(memory.0))).is_ok();
            }
            if !stored {
                let _ = GlobalFree(Some(memory));
            }
        }
        let _ = CloseClipboard();
    }
}

pub fn clipboard_text(owner: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT.0 as u32)
            .ok()
            .and_then(|handle| {
                let memory = HGLOBAL(handle.0);
                let pointer = GlobalLock(memory) as *const u16;
                if pointer.is_null() {
                    return None;
                }
                // Never read past the allocation, even without a terminator.
                let all = std::slice::from_raw_parts(pointer, GlobalSize(memory) / 2);
                let length = all.iter().position(|&c| c == 0).unwrap_or(all.len());
                let text = String::from_utf16_lossy(&all[..length]);
                let _ = GlobalUnlock(memory);
                Some(text)
            });
        let _ = CloseClipboard();
        text
    }
}
