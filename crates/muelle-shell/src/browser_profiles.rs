//! Chromium profiles through public window properties. No Explorer hooks,
//! process memory, browser databases, network requests or directory polling.
//!
//! The id includes the validated launch destination, so a pinned profile can
//! reopen after a restart even before its icon cache has been loaded. Cosmetic
//! metadata is cached separately and never determines which profile to open.

pub(crate) mod avatar;

use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Foundation::{HLOCAL, HWND, LocalFree, PROPERTYKEY},
        Storage::EnhancedStorage::{
            PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
            PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ},
        UI::Shell::{
            CommandLineToArgvW, IShellLinkW,
            PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow},
            SHLoadIndirectString, ShellLink,
        },
    },
    core::{HSTRING, Interface},
};

const PREFIX: &str = "muelle:browser-profile:";
const MAX_METADATA: usize = 128;
const MAX_TEXT: usize = 32_768;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub executable: String,
    aumid: String,
    directory: String,
    user_data: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Visual {
    id: String,
    pub name: String,
    pub icon: String,
}

#[derive(Default)]
struct Metadata {
    visuals: HashMap<String, Visual>,
    changed: Vec<String>,
}

static METADATA: OnceLock<Mutex<Metadata>> = OnceLock::new();
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

fn metadata() -> &'static Mutex<Metadata> {
    METADATA.get_or_init(Mutex::default)
}

pub fn is_profile(id: &str) -> bool {
    id.starts_with(PREFIX)
}

/// The supported family, not a claim that every build publishes profile data.
pub fn browser_name(executable: &str) -> Option<&'static str> {
    match executable
        .rsplit(['\\', '/'])
        .next()?
        .to_ascii_lowercase()
        .as_str()
    {
        "chrome.exe" => Some("Google Chrome"),
        "msedge.exe" => Some("Microsoft Edge"),
        "brave.exe" => Some("Brave"),
        "vivaldi.exe" => Some("Vivaldi"),
        "chromium.exe" => Some("Chromium"),
        _ => None,
    }
}

pub fn program(id: &str) -> Option<Cow<'_, str>> {
    if is_profile(id) {
        Profile::parse(id).map(|profile| Cow::Owned(profile.executable))
    } else {
        browser_name(id).map(|_| Cow::Borrowed(id))
    }
}

/// Firefox and the browsers built on it. They have no Chromium profiles here;
/// their web apps and private windows carry identities of their own (#72).
pub(crate) fn is_gecko(executable: &str) -> bool {
    executable.rsplit(['\\', '/']).next().is_some_and(|name| {
        [
            "firefox.exe",
            "librewolf.exe",
            "waterfox.exe",
            "floorp.exe",
            "zen.exe",
        ]
        .iter()
        .any(|gecko| name.eq_ignore_ascii_case(gecko))
    })
}

/// A Firefox web app's shortcut (Firefox 143+): the browser itself opening
/// `-taskbar-tab <id>`, which shows a web app window instead of a browser.
pub(crate) fn taskbar_tab_arguments(arguments: &str) -> bool {
    if arguments.len() > MAX_TEXT - 8 {
        return false;
    }
    command_line(&format!("browser {arguments}")).is_some_and(|args| {
        args.windows(2).skip(1).any(|pair| {
            let flag = pair[0].trim_start_matches('-');
            flag.eq_ignore_ascii_case("taskbar-tab")
                && !pair[1].is_empty()
                && !pair[1].starts_with('-')
        })
    })
}

/// Identifies an installed Chromium app's shortcut, without replaying any args.
pub(crate) fn web_app_arguments(arguments: &str) -> bool {
    if arguments.len() > MAX_TEXT - 8 {
        return false;
    }
    let Some(args) = command_line(&format!("browser {arguments}")) else {
        return false;
    };
    let mut found = false;
    for value in args
        .iter()
        .skip(1)
        .filter_map(|arg| arg.strip_prefix("--app-id="))
    {
        if found
            || value.len() != 32
            || !value
                .bytes()
                .all(|byte| (b'a'..=b'p').contains(&byte.to_ascii_lowercase()))
        {
            return false;
        }
        found = true;
    }
    found
}

impl Profile {
    /// Only launch switches needed to select this profile are retained. Never
    /// replay URLs, extension/app ids, remote debugging flags or arbitrary args.
    pub(crate) fn from_command(executable: &str, aumid: &str, command: &str) -> Option<Self> {
        browser_name(executable)?;
        if !local_path(executable) || aumid.is_empty() || aumid.len() > 512 {
            return None;
        }
        let args = command_line(command)?;
        if !args
            .first()
            .is_some_and(|arg| arg.eq_ignore_ascii_case(executable))
        {
            return None;
        }
        let mut directory = None;
        let mut user_data = None;
        for arg in args.iter().skip(1) {
            if let Some(value) = arg.strip_prefix("--profile-directory=") {
                if directory.replace(value.to_owned()).is_some() || !profile_directory(value) {
                    return None;
                }
            } else if let Some(value) = arg.strip_prefix("--user-data-dir=") {
                if user_data.replace(value.to_owned()).is_some() || !local_path(value) {
                    return None;
                }
            } else if arg.starts_with("--source-shortcut-location=") {
                // Chrome adds this provenance tag to desktop shortcuts. It
                // does not select a profile and is deliberately not replayed.
            } else {
                // Guest/incognito, PWAs and unusual launchers keep their
                // existing identity; they must not masquerade as a profile.
                return None;
            }
        }
        Some(Self {
            executable: executable.to_lowercase(),
            aumid: aumid.to_owned(),
            directory: directory?,
            user_data: user_data.unwrap_or_default(),
        })
    }

    pub fn id(&self) -> String {
        let components = [
            &self.executable,
            &self.aumid,
            &self.directory,
            &self.user_data,
        ];
        format!(
            "{PREFIX}{}",
            components.map(|part| hex(part.as_bytes())).join("/")
        )
    }

    pub fn parse(id: &str) -> Option<Self> {
        if id.len() > MAX_TEXT * 4 {
            return None;
        }
        let mut parts = id.strip_prefix(PREFIX)?.split('/');
        let profile = Self {
            executable: unhex(parts.next()?)?,
            aumid: unhex(parts.next()?)?,
            directory: unhex(parts.next()?)?,
            user_data: unhex(parts.next()?)?,
        };
        if parts.next().is_some() {
            return None;
        }
        browser_name(&profile.executable)?;
        (local_path(&profile.executable)
            && !profile.aumid.is_empty()
            && profile.aumid.len() <= 512
            && profile_directory(&profile.directory)
            && (profile.user_data.is_empty() || local_path(&profile.user_data)))
        .then_some(profile)
    }

    pub fn arguments(&self) -> String {
        let mut arguments = format!("--profile-directory={}", quote(&self.directory));
        if !self.user_data.is_empty() {
            arguments.push_str(&format!(" --user-data-dir={}", quote(&self.user_data)));
        }
        arguments
    }

    pub fn fallback_name(&self) -> String {
        format!(
            "{} — {}",
            browser_name(&self.executable).unwrap_or("Browser"),
            self.directory
        )
    }

    pub fn is_default(&self) -> bool {
        self.directory == "Default" && self.user_data.is_empty()
    }

    pub fn aumid(&self) -> &str {
        &self.aumid
    }
}

/// Profile shortcuts imported or dropped into Muelle use exactly the same id
/// as running windows; two profiles never overwrite one another's launch args.
pub(crate) fn shortcut_id(
    link: &Path,
    executable: &str,
    arguments: &str,
    aumid: &str,
) -> Option<String> {
    let profile = Profile::from_command(
        executable,
        aumid,
        &format!("{} {arguments}", quote(executable)),
    )?;
    // SAFETY: Read an existing shortcut; never resolve or execute its target.
    let (name, icon) = unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        shell
            .cast::<IPersistFile>()
            .ok()?
            .Load(&HSTRING::from(link.as_os_str()), STGM_READ)
            .ok()?;
        let mut icon = [0u16; 4096];
        let mut index = 0;
        let resource = shell
            .GetIconLocation(&mut icon, &mut index)
            .ok()
            .map(|()| {
                let end = icon
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(icon.len());
                format!("{},{index}", String::from_utf16_lossy(&icon[..end]))
            })
            .unwrap_or_default();
        (link.file_stem()?.to_string_lossy().into_owned(), resource)
    };
    let id = profile.id();
    remember(Visual {
        id: id.clone(),
        name,
        icon,
    });
    Some(id)
}

/// Called only for an identified browser window. Reads its public property
/// store; no messaging to the browser, file access or shell item resolution.
pub(crate) fn window_id(hwnd: HWND, executable: &str) -> Option<String> {
    browser_name(executable)?;
    // SAFETY: Read-only property store for the supplied top-level window.
    let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(hwnd).ok()? };
    let read = |key: &PROPERTYKEY| {
        // SAFETY: Store owns the value until it is copied to a Rust string.
        let value = unsafe { store.GetValue(key).ok()? }.to_string();
        (!value.is_empty() && value.len() <= MAX_TEXT).then_some(value)
    };
    let profile = Profile::from_command(
        executable,
        &read(&PKEY_AppUserModel_ID)?,
        &read(&PKEY_AppUserModel_RelaunchCommand)?,
    )?;
    let id = profile.id();
    remember(Visual {
        id: id.clone(),
        name: read(&PKEY_AppUserModel_RelaunchDisplayNameResource)
            .unwrap_or_else(|| profile.fallback_name()),
        icon: read(&PKEY_AppUserModel_RelaunchIconResource).unwrap_or_default(),
    });
    Some(id)
}

/// Cheap focus-time check before resolving a registered web app. Ordinary
/// browser focus changes read public window properties, never shell shortcuts.
pub(crate) fn window_web_app(hwnd: HWND) -> bool {
    // SAFETY: Read-only property of the supplied window; no browser messages.
    let command = unsafe {
        let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd) else {
            return false;
        };
        store.GetValue(&PKEY_AppUserModel_RelaunchCommand).ok()
    };
    command.is_some_and(|command| web_app_arguments(&command.to_string()))
}

fn remember(visual: Visual) {
    let Ok(mut cache) = metadata().lock() else {
        return;
    };
    if cache.visuals.len() >= MAX_METADATA
        && !cache.visuals.contains_key(&visual.id)
        && let Some(old) = cache.visuals.keys().next().cloned()
    {
        cache.visuals.remove(&old);
    }
    if !cache.changed.contains(&visual.id) {
        if cache.changed.len() >= MAX_METADATA {
            cache.changed.remove(0);
        }
        cache.changed.push(visual.id.clone());
    }
    cache.visuals.insert(visual.id.clone(), visual);
}

/// Coalesced by the existing window sync; no new timer or worker is needed.
pub fn take_changed() -> Vec<String> {
    metadata().lock().map_or_else(
        |_| Vec::new(),
        |mut cache| std::mem::take(&mut cache.changed),
    )
}

/// Called on an icon worker only. Persist only cosmetic metadata. An unreadable
/// or mismatched record falls back to the browser icon, never to another profile.
pub(crate) fn visual(id: &str, folder: &Path) -> Option<Visual> {
    let path = visual_path(id, folder);
    let live = metadata().lock().ok()?.visuals.get(id).cloned();
    let saved = fs::metadata(&path)
        .ok()
        .filter(|meta| meta.len() <= 131_072)
        .and_then(|_| fs::read(&path).ok())
        .and_then(|bytes| serde_json::from_slice::<Visual>(&bytes).ok())
        .filter(|visual| visual.id == id);
    if let Some(live) = live {
        if saved.as_ref() != Some(&live)
            && let Ok(bytes) = serde_json::to_vec(&live)
        {
            let _ = fs::create_dir_all(folder);
            let temporary = path.with_extension(format!(
                "{}-{}.tmp",
                std::process::id(),
                TEMPORARY.fetch_add(1, Ordering::Relaxed)
            ));
            if fs::write(&temporary, bytes).is_ok() && fs::rename(&temporary, &path).is_err() {
                let _ = fs::remove_file(&temporary);
            }
        }
        Some(live)
    } else {
        saved
    }
}

fn visual_path(id: &str, folder: &Path) -> PathBuf {
    let mut hash = DefaultHasher::new();
    id.hash(&mut hash);
    folder.join(format!("profile-{:016x}.json", hash.finish()))
}

pub(crate) fn display_name(value: &str) -> String {
    if let Some(resource) = value.strip_prefix('@') {
        if icon_location(resource).is_none() {
            return String::new();
        }
        let mut buffer = [0u16; 1024];
        // SAFETY: Resolves a Windows string resource without executing it.
        if unsafe { SHLoadIndirectString(&HSTRING::from(value), &mut buffer, None) }.is_ok() {
            let len = buffer
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(buffer.len());
            return String::from_utf16_lossy(&buffer[..len]);
        }
        return String::new();
    }
    value.trim().to_owned()
}

pub(crate) fn icon_location(resource: &str) -> Option<(&str, i32)> {
    let (file, index) = resource
        .rsplit_once(',')
        .map_or(Some((resource, 0)), |(file, index)| {
            Some((file, index.trim().parse().ok()?))
        })?;
    let file = file.trim().trim_matches('"');
    local_path(file).then_some((file, index))
}

fn local_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
        && !value.chars().any(|c| c.is_control() || c == '"')
        && !value[2..].contains(':')
}

fn profile_directory(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
}

fn command_line(value: &str) -> Option<Vec<String>> {
    if value.is_empty() || value.len() > MAX_TEXT || value.contains('\0') {
        return None;
    }
    let mut count = 0;
    // SAFETY: Windows allocates argv as one block; copy and release it exactly once.
    unsafe {
        let args = CommandLineToArgvW(&HSTRING::from(value), &mut count);
        if args.is_null() {
            return None;
        }
        let result = if (1..=16).contains(&count) {
            std::slice::from_raw_parts(args, count as usize)
                .iter()
                .map(|arg| arg.to_string().ok())
                .collect()
        } else {
            None
        };
        let _ = LocalFree(Some(HLOCAL(args.cast())));
        result
    }
}

// CommandLineToArgvW-compatible quoting, including trailing backslashes.
fn quote(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        result.push(c);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                DIGITS[(byte >> 4) as usize] as char,
                DIGITS[(byte & 15) as usize] as char,
            ]
        })
        .collect()
}

fn unhex(value: &str) -> Option<String> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| char::from(byte).to_digit(16).map(|digit| digit as u8);
            Some((digit(pair[0])? << 4) | digit(pair[1])?)
        })
        .collect();
    String::from_utf8(bytes?).ok()
}

#[cfg(test)]
mod tests;
