//! Read-only identity and icon cache. Never changes Windows' visibility choices.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM},
        System::{
            Registry::*,
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
        UI::{
            Shell::{
                KF_FLAG_DEFAULT, NOTIFYICONIDENTIFIER, SHGetKnownFolderPath,
                Shell_NotifyIconGetRect,
            },
            WindowsAndMessaging::{
                EnumWindows, FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE,
            },
        },
    },
    core::{BOOL, GUID, PCWSTR, PWSTR, w},
};

pub(super) const PATH: PCWSTR = w!("Control Panel\\NotifyIconSettings");
fn path_key(path: &Path) -> PathBuf {
    let path = path.to_string_lossy().replace('/', "\\");
    let normalized = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\")
            .or_else(|| path.strip_prefix(r"\??\"))
            .unwrap_or(&path)
            .to_owned()
    };
    PathBuf::from(normalized.to_lowercase())
}
pub(super) fn same_executable(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}
pub(super) struct Record {
    pub id: u64,
    pub guid: Option<GUID>,
    pub uid: Option<u32>,
    pub executable: Option<PathBuf>,
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: This key was opened by us and has a single owner.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn value(key: HKEY, name: PCWSTR, kind: REG_ROUTINE_FLAGS, limit: usize) -> Option<Vec<u8>> {
    let mut size = 0;
    // SAFETY: First obtain the required size, then supply a bounded buffer.
    unsafe {
        RegGetValueW(key, None, name, kind, None, None, Some(&mut size))
            .ok()
            .ok()?;
        if size as usize > limit {
            return None;
        }
        let mut bytes = vec![0; size as usize];
        RegGetValueW(
            key,
            None,
            name,
            kind,
            None,
            Some(bytes.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
        bytes.truncate(size as usize);
        Some(bytes)
    }
}
fn number(key: HKEY, name: PCWSTR) -> Option<u32> {
    Some(u32::from_le_bytes(
        value(key, name, RRF_RT_REG_DWORD, 4)?.try_into().ok()?,
    ))
}
fn text(key: HKEY, name: PCWSTR) -> Option<String> {
    let data = value(key, name, RRF_RT_REG_SZ, 65536)?;
    let units: Vec<_> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| u16::from_le_bytes([v[0], v[1]]))
        .take_while(|v| *v != 0)
        .collect();
    String::from_utf16(&units).ok()
}
fn executable(text: String) -> Option<PathBuf> {
    if text.starts_with('{') {
        let end = text.find('}')?;
        let guid = GUID::try_from(&text[1..end]).ok()?;
        // SAFETY: The known folder API allocates a string released below.
        let prefix = unsafe {
            let path = SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None).ok()?;
            let result = path.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(path.0.cast()));
            result?
        };
        Some(PathBuf::from(prefix).join(text[end + 1..].trim_start_matches(['\\', '/'])))
    } else {
        Some(PathBuf::from(text))
    }
}

pub(super) fn records() -> Vec<Record> {
    let mut root = HKEY::default();
    // SAFETY: Keys are read only, bounded, and closed by Key.
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, PATH, None, KEY_READ, &mut root).is_err() {
            return Vec::new();
        }
        let root = Key(root);
        let mut records = Vec::new();
        for index in 0..4096 {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(
                root.0,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
            .is_err()
            {
                break;
            }
            let Ok(id) = String::from_utf16_lossy(&name[..len as usize]).parse() else {
                continue;
            };
            let mut key = HKEY::default();
            if RegOpenKeyExW(root.0, PCWSTR(name.as_ptr()), None, KEY_READ, &mut key).is_err() {
                continue;
            }
            let key = Key(key);
            // Visibility comes from the live tray tree, not this historical
            // preference: Windows' own default-visible icons can omit it.
            records.push(Record {
                id,
                guid: text(key.0, w!("IconGuid"))
                    .and_then(|s| GUID::try_from(s.trim_matches(['{', '}'])).ok())
                    .filter(|g| *g != GUID::zeroed()),
                uid: number(key.0, w!("UID")),
                executable: text(key.0, w!("ExecutablePath")).and_then(executable),
            });
            if records.len() == 512 {
                break;
            }
        }
        records
    }
}

/// Legacy icons use (callback HWND, UID). Enumerate local handles, never read
/// another process' memory. Message-only windows are common for tray apps.
pub(super) fn snapshot(id: u64) -> Option<Vec<u8>> {
    let path: Vec<_> = format!("Control Panel\\NotifyIconSettings\\{id}\0")
        .encode_utf16()
        .collect();
    let mut key = HKEY::default();
    // SAFETY: Numeric identity and read-only access. Historical images are
    // never loaded; pixels are requested only after binding a live button.
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            KEY_READ,
            &mut key,
        )
        .ok()
        .ok()?;
    }
    let key = Key(key);
    value(key.0, w!("IconSnapshot"), RRF_RT_REG_BINARY, 256 * 1024)
}

pub(super) fn owners(records: &[Record]) -> HashMap<PathBuf, Vec<isize>> {
    if !records.iter().any(|r| r.guid.is_none() && r.uid.is_some()) {
        return HashMap::new();
    }
    unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<isize>) };
        list.push(window.0 as isize);
        (list.len() < 4096).into()
    }
    let mut windows = Vec::new();
    // SAFETY: The callback borrows our vector only for this synchronous call.
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM((&raw mut windows) as isize));
        let mut previous = None;
        while windows.len() < 4096 {
            let Ok(window) = FindWindowExW(Some(HWND_MESSAGE), previous, None, None) else {
                break;
            };
            windows.push(window.0 as isize);
            previous = Some(window);
        }
    }
    let mut paths = HashMap::<u32, Option<PathBuf>>::new();
    let mut found = HashMap::<PathBuf, Vec<isize>>::new();
    for handle in windows {
        let mut pid = 0;
        // SAFETY: Query-only process handle; no activation, messages or memory reads.
        unsafe {
            GetWindowThreadProcessId(HWND(handle as _), Some(&mut pid));
        }
        let path = paths.entry(pid).or_insert_with(|| unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buffer = [0u16; 32768];
            let mut len = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(process);
            result.ok()?;
            Some(path_key(Path::new(&String::from_utf16_lossy(
                &buffer[..len as usize],
            ))))
        });
        let Some(path) = path else {
            continue;
        };
        if records.iter().any(|r| {
            r.guid.is_none()
                && r.executable
                    .as_ref()
                    .is_some_and(|p| same_executable(p, path))
        }) {
            let handles = found.entry(path.clone()).or_default();
            if handles.len() < 128 {
                handles.push(handle);
            }
        }
    }
    found
}

pub(super) fn location(
    record: &Record,
    owners: &HashMap<PathBuf, Vec<isize>>,
) -> Option<windows::Win32::Foundation::RECT> {
    let mut identifier = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        ..Default::default()
    };
    if let Some(guid) = record.guid {
        identifier.guidItem = guid;
        // SAFETY: Shell validates a documented GUID identity without invoking the app.
        return unsafe { Shell_NotifyIconGetRect(&identifier).ok() };
    }
    identifier.uID = record.uid?;
    let path = path_key(record.executable.as_ref()?);
    for window in owners.get(&path)? {
        identifier.hWnd = HWND(*window as _);
        // SAFETY: Exact executable + HWND + UID, not tooltip or basename guessing.
        if let Ok(rect) = unsafe { Shell_NotifyIconGetRect(&identifier) } {
            return Some(rect);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_extended_windows_paths_without_filesystem_queries() {
        assert!(same_executable(
            Path::new(r"\\?\C:\Apps\Client.exe"),
            Path::new("c:/apps/client.EXE")
        ));
    }
    #[test]
    fn matches_extended_unc_paths() {
        assert!(same_executable(
            Path::new(r"\\?\UNC\server\apps\client.exe"),
            Path::new(r"\\SERVER\Apps\Client.exe")
        ));
    }
    #[test]
    fn does_not_identify_apps_by_basename() {
        assert!(!same_executable(
            Path::new(r"C:\First\client.exe"),
            Path::new(r"C:\Second\client.exe")
        ));
    }
}
