//! Tray-icon relay host.
//!
//! Apps add tray icons by sending `WM_COPYDATA` to the window of class
//! `Shell_TrayWnd`. This module owns a hidden, topmost window of that class,
//! records every icon it sees, and then forwards the very same message to
//! Explorer's real tray window, so Windows' own tray keeps working.
//!
//! Apps reach us only while we are ahead of Explorer's taskbar in
//! `FindWindow`'s z-order walk. Fighting for that permanently makes Explorer
//! burn CPU, so priority is taken in short bursts: [`TrayHost::refresh`]
//! clears the recorded icons, demotes Explorer's taskbar, raises our window,
//! broadcasts `TaskbarCreated` so apps re-add their icons to us, and after
//! [`BURST_MS`] gives Explorer its band back. Outside a burst the window
//! still records and forwards whatever message happens to reach it. When the
//! host is dropped its window disappears and apps find Explorer again.
use std::{
    cell::{Cell, RefCell},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, GetObjectW, SelectObject,
        },
        System::{
            DataExchange::COPYDATASTRUCT, LibraryLoader::GetModuleHandleW,
            Threading::GetCurrentProcessId,
        },
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, CopyIcon, CreateWindowExW, DI_NORMAL, DefWindowProcW,
            DestroyIcon, DispatchMessageW, DrawIconEx, FindWindowExW, FindWindowW, GWL_EXSTYLE,
            GetIconInfo, GetMessageW, GetWindowLongW, GetWindowThreadProcessId, HICON,
            HWND_BROADCAST, HWND_NOTOPMOST, HWND_TOPMOST, ICONINFO, IsWindow, KillTimer, MSG,
            PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
            SMTO_ABORTIFHUNG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageTimeoutW,
            SetTimer, SetWindowPos, TranslateMessage, WM_CLOSE, WM_COPYDATA, WM_DESTROY,
            WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER,
            WM_USER, WM_WINDOWPOSCHANGED, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
    core::{PCWSTR, w},
};

use crate::icons::IconImage;

const SIGNATURE: u32 = 0x3475_3423;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x01;
const NOTIFYICON_VERSION_4: u32 = 4;
/// `NIN_SELECT`, sent by version 4 icons after a left click.
const NIN_SELECT: u32 = WM_USER;
const WM_CONTEXTMENU: u32 = 0x007B;
/// Re-asserts our priority during a burst.
const TIMER_BURST: usize = 1;
/// Private message: start (or extend) a priority burst.
const WM_REFRESH: u32 = WM_USER + 1;
/// How long a burst keeps priority, and how often it re-asserts it.
const BURST_MS: u64 = 800;
const BURST_TICK_MS: u32 = 50;
const FORWARD_TIMEOUT_MS: u32 = 3000;

// Offsets inside the 32-bit-safe NOTIFYICONDATAW explorer receives.
const OFF_TIP: usize = 24;
const OFF_STATE: usize = 280;
const OFF_VERSION: usize = 800;
const OFF_GUID: usize = 936;
const END_STATE: usize = 288;
const END_VERSION: usize = 804;
const END_GUID: usize = 952;

/// A parsed `SHELLTRAYDATA` message (handles are the 32-bit values on the wire).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayMessage {
    pub command: u32,
    pub hwnd: u32,
    pub uid: u32,
    pub flags: u32,
    pub callback: u32,
    pub icon: u32,
    pub tip: String,
    pub state: u32,
    pub state_mask: u32,
    pub version: u32,
    pub guid: Option<[u8; 16]>,
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn wide_at(bytes: &[u8], offset: usize, max_chars: usize) -> Option<String> {
    let end = offset.checked_add(max_chars.checked_mul(2)?)?;
    let units: Vec<u16> = bytes
        .get(offset..end)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Parses the payload of a `WM_COPYDATA` with `dwData == 1`. Fields are only
/// read when both the struct's own `cbSize` and the buffer cover them.
#[must_use]
pub fn parse_tray_data(data: &[u8]) -> Option<TrayMessage> {
    if u32_at(data, 0)? != SIGNATURE {
        return None;
    }
    let command = u32_at(data, 4)?;
    let nid = data.get(8..)?;
    let cb_size = u32_at(nid, 0)? as usize;
    // Bytes that both the declared struct size and the buffer provide.
    let effective = cb_size.min(nid.len());
    if effective < OFF_TIP {
        return None;
    }
    let nid = &nid[..effective];
    let mut message = TrayMessage {
        command,
        hwnd: u32_at(nid, 4)?,
        uid: u32_at(nid, 8)?,
        flags: u32_at(nid, 12)?,
        callback: u32_at(nid, 16)?,
        icon: u32_at(nid, 20)?,
        ..TrayMessage::default()
    };
    // Version 1 structs have a 64-character tip, later ones 128.
    let tip_chars = if effective >= OFF_STATE {
        128
    } else {
        64.min((effective - OFF_TIP) / 2)
    };
    message.tip = wide_at(nid, OFF_TIP, tip_chars)?;
    if effective >= END_STATE {
        message.state = u32_at(nid, OFF_STATE)?;
        message.state_mask = u32_at(nid, OFF_STATE + 4)?;
    }
    if effective >= END_VERSION {
        message.version = u32_at(nid, OFF_VERSION)?;
    }
    if effective >= END_GUID {
        message.guid = Some(nid.get(OFF_GUID..OFF_GUID + 16)?.try_into().ok()?);
    }
    Some(message)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum KeyKind {
    Guid([u8; 16]),
    Id(isize, u32),
}

/// Stable identity of a tray icon: its GUID, or else `(hwnd, uid)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TrayKey(KeyKind);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayButton {
    Left,
    Right,
    Double,
}

#[derive(Clone)]
pub struct TrayIconInfo {
    pub key: TrayKey,
    pub tip: String,
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, rows from top to bottom.
    pub bgra: Arc<Vec<u8>>,
    pub pid: u32,
}

impl TrayKey {
    /// A key that identifies no real icon, for tests of code that lists icons.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test(id: u32) -> Self {
        Self(KeyKind::Id(0, id))
    }
}

/// The file name (without `.exe`) of the program running process `pid`.
#[must_use]
pub fn process_exe_name(pid: u32) -> Option<String> {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    };
    if pid == 0 {
        return None;
    }
    // SAFETY: The handle is opened for a query only and closed below; the
    // buffer length passed is its real capacity.
    let path = unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0_u16; 520];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &raw mut length,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        String::from_utf16_lossy(buffer.get(..length as usize)?)
    };
    let name = path.rsplit(['\\', '/']).next()?;
    let stem = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
        .unwrap_or(name);
    (!stem.is_empty()).then(|| stem.to_owned())
}

/// An icon's converted pixels. The buffer is shared, so every listing of the
/// icon hands out the same allocation and equality checks can compare pointers.
#[derive(Clone)]
struct CachedImage {
    width: u32,
    height: u32,
    bgra: Arc<Vec<u8>>,
}

impl CachedImage {
    /// Keeps `old` when `new` holds the same pixels, so a re-added but
    /// unchanged icon is not seen as a change; otherwise adopts `new`.
    fn reuse_or_new(old: Option<&Self>, new: IconImage) -> Self {
        match old {
            Some(old)
                if old.width == new.width && old.height == new.height && *old.bgra == new.bgra =>
            {
                old.clone()
            }
            _ => Self {
                width: new.width,
                height: new.height,
                bgra: Arc::new(new.bgra),
            },
        }
    }
}

struct Entry {
    key: TrayKey,
    hwnd: isize,
    uid: u32,
    callback: u32,
    version: u32,
    /// Our own `CopyIcon` of the app's icon; 0 when none.
    icon: isize,
    tip: String,
    hidden: bool,
    /// The last converted pixels; kept across an icon change so identical
    /// pixels can reuse them.
    image: Option<CachedImage>,
    /// `icon` changed since `image` was converted.
    stale: bool,
    /// Counts icon changes, so a conversion finished after a newer icon
    /// arrived is recognized as out of date.
    generation: u32,
}

/// A number no other icon change has had, for [`Entry::generation`].
fn next_generation() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Sign-extends a 32-bit wire handle, as Windows does for 32-bit handles.
fn widen(handle: u32) -> isize {
    handle as i32 as isize
}

fn key_of(message: &TrayMessage) -> TrayKey {
    match message.guid {
        Some(guid) if message.flags & NIF_GUID != 0 => TrayKey(KeyKind::Guid(guid)),
        _ => TrayKey(KeyKind::Id(widen(message.hwnd), message.uid)),
    }
}

fn hidden_from(message: &TrayMessage, current: bool) -> bool {
    if message.flags & NIF_STATE == 0 || message.state_mask & NIS_HIDDEN == 0 {
        return current;
    }
    message.state & NIS_HIDDEN != 0
}

/// Applies one message to the icon list. `copy` duplicates an icon handle
/// (0 on failure) and `destroy` releases one of our copies. Returns whether
/// the list changed.
fn apply(
    icons: &mut Vec<Entry>,
    message: &TrayMessage,
    copy: &mut dyn FnMut(isize) -> isize,
    destroy: &mut dyn FnMut(isize),
) -> bool {
    let key = key_of(message);
    let position = icons.iter().position(|entry| entry.key == key);
    match message.command {
        NIM_ADD => {
            let icon = if message.flags & NIF_ICON != 0 && message.icon != 0 {
                copy(widen(message.icon))
            } else {
                0
            };
            let entry = Entry {
                key,
                hwnd: widen(message.hwnd),
                uid: message.uid,
                callback: if message.flags & NIF_MESSAGE != 0 {
                    message.callback
                } else {
                    0
                },
                version: 0,
                icon,
                tip: if message.flags & NIF_TIP != 0 {
                    message.tip.clone()
                } else {
                    String::new()
                },
                hidden: hidden_from(message, false),
                image: None,
                stale: icon != 0,
                generation: next_generation(),
            };
            match position {
                Some(index) => {
                    let mut entry = entry;
                    // Apps re-add all their icons in a burst, mostly
                    // unchanged: keep the old pixels until the new icon is
                    // converted and found different.
                    if icon != 0 {
                        entry.image = icons[index].image.take();
                    }
                    entry.generation = next_generation();
                    let old = std::mem::replace(&mut icons[index], entry);
                    if old.icon != 0 {
                        destroy(old.icon);
                    }
                }
                None => icons.push(entry),
            }
            true
        }
        NIM_MODIFY => {
            let Some(index) = position else { return false };
            let entry = &mut icons[index];
            if message.hwnd != 0 {
                entry.hwnd = widen(message.hwnd);
            }
            if message.flags & NIF_MESSAGE != 0 {
                entry.callback = message.callback;
            }
            if message.flags & NIF_ICON != 0 {
                if message.icon == 0 {
                    if entry.icon != 0 {
                        destroy(entry.icon);
                    }
                    entry.icon = 0;
                    entry.image = None;
                    entry.stale = false;
                    entry.generation = next_generation();
                } else {
                    let new = copy(widen(message.icon));
                    if new != 0 {
                        if entry.icon != 0 {
                            destroy(entry.icon);
                        }
                        entry.icon = new;
                        entry.stale = true;
                        entry.generation = next_generation();
                    }
                }
            }
            if message.flags & NIF_TIP != 0 {
                entry.tip.clone_from(&message.tip);
            }
            entry.hidden = hidden_from(message, entry.hidden);
            true
        }
        NIM_DELETE => {
            let Some(index) = position else { return false };
            let old = icons.remove(index);
            if old.icon != 0 {
                destroy(old.icon);
            }
            true
        }
        NIM_SETVERSION => {
            let Some(index) = position else { return false };
            icons[index].version = message.version;
            true
        }
        // NIM_SETFOCUS and anything unknown change nothing we track.
        // NIM_SETFOCUS (3) and anything unknown change nothing we track.
        _ => false,
    }
}

/// The messages a click posts to the icon's callback. Each tuple is
/// `(event, wParam, lParam)`: the notification event (also encoded in `lParam`
/// for version 4) and the parameters to post to the icon's callback message.
#[must_use]
pub fn click_messages(
    version: u32,
    uid: u32,
    button: TrayButton,
    at: (i32, i32),
) -> Vec<(u32, usize, isize)> {
    if version >= NOTIFYICON_VERSION_4 {
        let wparam = (usize::from(at.1 as u16) << 16) | usize::from(at.0 as u16);
        let pack = |event: u32| {
            (
                event,
                wparam,
                ((uid as u16 as isize) << 16) | (event as u16 as isize),
            )
        };
        let events: &[u32] = match button {
            TrayButton::Left => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
            TrayButton::Right => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
            TrayButton::Double => &[WM_LBUTTONDBLCLK, NIN_SELECT],
        };
        events.iter().map(|event| pack(*event)).collect()
    } else {
        let events: &[u32] = match button {
            TrayButton::Left => &[WM_LBUTTONDOWN, WM_LBUTTONUP],
            TrayButton::Right => &[WM_RBUTTONDOWN, WM_RBUTTONUP],
            TrayButton::Double => &[WM_LBUTTONDBLCLK],
        };
        events
            .iter()
            .map(|event| (*event, uid as usize, *event as isize))
            .collect()
    }
}

struct Shared {
    icons: Mutex<Vec<Entry>>,
    notify_hwnd: isize,
    notify_msg: u32,
    pending: AtomicBool,
    taskbar_created: u32,
    /// A burst is running (Explorer's taskbar is demoted); set by the window
    /// thread.
    bursting: AtomicBool,
    /// Explorer's tray window as last found; 0 when unknown. Looked up again
    /// when it stops being a window.
    explorer: AtomicIsize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn destroy_icon(icon: isize) {
    // SAFETY: `icon` is a copy made by `copy_icon` that is destroyed once.
    unsafe {
        let _ = DestroyIcon(HICON(icon as *mut _));
    }
}

fn copy_icon(icon: isize) -> isize {
    // SAFETY: CopyIcon only reads the handle and fails for an invalid one.
    unsafe { CopyIcon(HICON(icon as *mut _)) }.map_or(0, |copy| copy.0 as isize)
}

impl Shared {
    fn request_notify(&self) {
        if self.notify_hwnd == 0 || self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: Posting to a window handle is sound; a stale one just fails.
        let posted = unsafe {
            PostMessageW(
                Some(HWND(self.notify_hwnd as *mut _)),
                self.notify_msg,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if posted.is_err() {
            self.pending.store(false, Ordering::Release);
        }
    }
}

thread_local! {
    static CONTEXT: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// When the running burst ends; `None` outside a burst. Window thread only.
    static BURST_END: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Explorer's real tray window: the first `Shell_TrayWnd` that is not ours.
#[must_use]
pub fn find_explorer_tray() -> Option<isize> {
    // SAFETY: Read-only window queries with valid arguments.
    unsafe {
        let own = GetCurrentProcessId();
        let mut after: Option<HWND> = None;
        for _ in 0..16 {
            let hwnd = FindWindowExW(None, after, w!("Shell_TrayWnd"), None).ok()?;
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
            if pid != own {
                return Some(hwnd.0 as isize);
            }
            after = Some(hwnd);
        }
    }
    None
}

/// Explorer's tray window, from the cache while it is still a window.
fn cached_explorer_tray(shared: &Shared) -> Option<isize> {
    let cached = shared.explorer.load(Ordering::Relaxed);
    // SAFETY: IsWindow accepts any handle value.
    if cached != 0 && unsafe { IsWindow(Some(HWND(cached as *mut _))) }.as_bool() {
        return Some(cached);
    }
    let found = find_explorer_tray();
    shared.explorer.store(found.unwrap_or(0), Ordering::Relaxed);
    found
}

fn forward(shared: &Shared, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(explorer) = cached_explorer_tray(shared) else {
        return LRESULT(0);
    };
    let mut result = 0usize;
    // SAFETY: `lparam` is the COPYDATASTRUCT we were sent, valid for this
    // call; the system marshals it again for the other process.
    let sent = unsafe {
        SendMessageTimeoutW(
            HWND(explorer as *mut _),
            WM_COPYDATA,
            wparam,
            lparam,
            SMTO_ABORTIFHUNG,
            FORWARD_TIMEOUT_MS,
            Some(&raw mut result),
        )
    };
    if sent.0 == 0 {
        LRESULT(0)
    } else {
        LRESULT(result as isize)
    }
}

fn handle_copydata(shared: &Shared, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: For WM_COPYDATA, lParam points to a COPYDATASTRUCT valid for the
    // duration of this message; the data slice is bounded by `cbData`.
    let data = unsafe {
        (lparam.0 as *const COPYDATASTRUCT)
            .as_ref()
            .filter(|cds| cds.dwData == 1 && !cds.lpData.is_null() && cds.cbData <= 8192)
            .map(|cds| std::slice::from_raw_parts(cds.lpData.cast::<u8>(), cds.cbData as usize))
    };
    if let Some(message) = data.and_then(parse_tray_data) {
        let changed = {
            let mut icons = lock(&shared.icons);
            apply(&mut icons, &message, &mut copy_icon, &mut destroy_icon)
        };
        if changed {
            shared.request_notify();
        }
    }
    forward(shared, wparam, lparam)
}

fn assert_topmost(hwnd: HWND) {
    // Explorer's taskbar (hidden and click-through while the dock runs) keeps
    // raising itself into the topmost band, where FindWindow reaches it before
    // us. Move it out of that band; `restore_explorer_topmost` undoes this.
    if let Some(explorer) = find_explorer_tray() {
        // SAFETY: Changes only the z-band of Explorer's live, invisible window.
        unsafe {
            let _ = SetWindowPos(
                HWND(explorer as *mut _),
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
    // SAFETY: `hwnd` is our own live window.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Gives Explorer's taskbar back its usual always-on-top band.
fn restore_explorer_topmost() {
    if let Some(explorer) = find_explorer_tray() {
        // SAFETY: Changes only the z-band of Explorer's live window.
        unsafe {
            let _ = SetWindowPos(
                HWND(explorer as *mut _),
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

/// When this process last broadcast `TaskbarCreated`.
static OWN_BROADCAST_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether a `TaskbarCreated` just received was most likely our own
/// broadcast (sent in the last few seconds), not Explorer restarting. Code
/// that rebuilds the taskbar on that message must skip it then.
#[must_use]
pub fn sent_taskbar_created_recently() -> bool {
    lock(&OWN_BROADCAST_AT).is_some_and(|at| at.elapsed() < Duration::from_secs(3))
}

/// Starts a priority burst, or just extends the one running.
fn begin_burst(hwnd: HWND, shared: &Shared) {
    let deadline = Instant::now() + Duration::from_millis(BURST_MS);
    if BURST_END.replace(Some(deadline)).is_some() {
        return;
    }
    shared.bursting.store(true, Ordering::Release);
    // Keep showing the icons we know while apps re-add theirs in response to
    // the broadcast below. Icons of apps that quit drop out on their own
    // (their window is gone, see `icons`); slow apps are never dropped.
    assert_topmost(hwnd);
    // SAFETY: Our own timer; the broadcast is a registered message that
    // windows ignore when they do not know it.
    *lock(&OWN_BROADCAST_AT) = Some(Instant::now());
    unsafe {
        SetTimer(Some(hwnd), TIMER_BURST, BURST_TICK_MS, None);
        let _ = PostMessageW(
            Some(HWND_BROADCAST),
            shared.taskbar_created,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// Re-asserts priority while a burst runs, and ends it at its deadline.
fn tick_burst(hwnd: HWND, shared: &Shared) {
    let Some(deadline) = BURST_END.get() else {
        // SAFETY: Our own timer.
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_BURST);
        }
        return;
    };
    if Instant::now() >= deadline {
        BURST_END.set(None);
        shared.bursting.store(false, Ordering::Release);
        // SAFETY: Our own timer.
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_BURST);
        }
        restore_explorer_topmost();
        return;
    }
    // SAFETY: Read-only lookup of a static class name.
    if unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok() != Some(hwnd) {
        assert_topmost(hwnd);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let shared = CONTEXT.with(|context| context.borrow().clone());
    match (message, shared) {
        (WM_COPYDATA, Some(shared)) => handle_copydata(&shared, wparam, lparam),
        (WM_REFRESH, Some(shared)) => {
            begin_burst(hwnd, &shared);
            LRESULT(0)
        }
        (WM_TIMER, Some(shared)) if wparam.0 == TIMER_BURST => {
            tick_burst(hwnd, &shared);
            LRESULT(0)
        }
        (WM_WINDOWPOSCHANGED, _) => {
            // Only a burst defends its z-order; outside one we stay passive.
            if BURST_END.get().is_some() {
                // SAFETY: Reading our own window's style.
                let style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
                if style & WS_EX_TOPMOST.0 == 0 {
                    assert_topmost(hwnd);
                }
            }
            // SAFETY: Default processing of the message we were given.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        (WM_DESTROY, _) => {
            // SAFETY: Our own timer and message queue.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_BURST);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        // Our own TaskbarCreated broadcast (or Explorer's) is ignored: apps
        // re-add their icons by finding us through FindWindow.
        (other, Some(shared)) if other == shared.taskbar_created => LRESULT(0),
        // SAFETY: Default processing of the message we were given.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn run_window(shared: &Arc<Shared>, ready: &mpsc::Sender<Option<isize>>) {
    CONTEXT.with(|context| *context.borrow_mut() = Some(Arc::clone(shared)));
    let class = w!("Shell_TrayWnd");
    // SAFETY: Window creation and the message loop all happen on this thread,
    // which owns the window; strings are static.
    unsafe {
        let instance = GetModuleHandleW(PCWSTR::null())
            .ok()
            .map(|module| module.into());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        // Already registered by a previous host in this process is fine.
        RegisterClassW(&raw const wc);
        let created = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        );
        let Ok(hwnd) = created else {
            crate::log::line("tray: no se pudo crear la ventana Shell_TrayWnd");
            let _ = ready.send(None);
            return;
        };
        // No priority yet: `TrayHost::refresh` takes it in short bursts.
        let _ = ready.send(Some(hwnd.0 as isize));
        let mut message = MSG::default();
        while GetMessageW(&raw mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

pub struct TrayHost {
    shared: Arc<Shared>,
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
    /// When [`Self::refresh`] last asked the apps to re-add their icons.
    last_refresh: Cell<Option<Instant>>,
}

/// Whether a refresh asked at `last` is old enough to ask again at `now`.
fn refresh_due(last: Option<Instant>, now: Instant, min_age: Duration) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= min_age)
}

impl TrayHost {
    /// Starts the relay window. `notify_msg` is posted to `notify_hwnd`
    /// (coalesced) whenever the icon list changes. `None` only when the
    /// window cannot be created. Call [`Self::refresh`] to collect icons.
    #[must_use]
    pub fn start(notify_hwnd: isize, notify_msg: u32) -> Option<Self> {
        // SAFETY: Registering a message name from a static string.
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let shared = Arc::new(Shared {
            icons: Mutex::new(Vec::new()),
            notify_hwnd,
            notify_msg,
            pending: AtomicBool::new(false),
            taskbar_created,
            bursting: AtomicBool::new(false),
            explorer: AtomicIsize::new(0),
        });
        let (sender, receiver) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("muelle-tray".into())
            .spawn(move || {
                run_window(&thread_shared, &sender);
                CONTEXT.with(|context| *context.borrow_mut() = None);
            })
            .ok()?;
        match receiver.recv() {
            Ok(Some(hwnd)) => Some(Self {
                shared,
                hwnd,
                thread: Some(thread),
                last_refresh: Cell::new(None),
            }),
            _ => {
                let _ = thread.join();
                None
            }
        }
    }

    /// Takes priority over Explorer's tray for a short burst and asks every
    /// app to re-add its icon to us (`TaskbarCreated`). The recorded list is
    /// cleared first and refills as apps answer; each change notifies. A call
    /// during a burst only extends it.
    pub fn refresh(&self) {
        self.last_refresh.set(Some(Instant::now()));
        // SAFETY: Posting to our own window; a closed one just fails.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                WM_REFRESH,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    /// [`Self::refresh`], unless the last one was less than `min_age` ago.
    /// Returns whether it ran.
    pub fn refresh_if_older_than(&self, min_age: Duration) -> bool {
        if !refresh_due(self.last_refresh.get(), Instant::now(), min_age) {
            return false;
        }
        self.refresh();
        true
    }

    /// Visible icons: not `NIS_HIDDEN`, with a live owner window and an image.
    /// Pixels are converted once per icon and cached.
    #[must_use]
    pub fn icons(&self) -> Vec<TrayIconInfo> {
        self.shared.pending.store(false, Ordering::Release);
        // Under the lock: forget icons whose owner is gone and take a private
        // copy of each icon to convert. The GDI work happens outside it, so
        // an app's `Shell_NotifyIcon` is never kept waiting on a conversion;
        // our copy stays valid even if the entry is deleted meanwhile.
        let jobs: Vec<(TrayKey, u32, isize)> = {
            let mut icons = lock(&self.shared.icons);
            icons.retain(|entry| {
                // SAFETY: IsWindow accepts any handle value.
                let alive = unsafe { IsWindow(Some(HWND(entry.hwnd as *mut _))) }.as_bool();
                if !alive && entry.icon != 0 {
                    destroy_icon(entry.icon);
                }
                alive
            });
            icons
                .iter()
                .filter(|entry| !entry.hidden && entry.icon != 0 && entry.stale)
                .filter_map(|entry| {
                    let copy = copy_icon(entry.icon);
                    (copy != 0).then(|| (entry.key.clone(), entry.generation, copy))
                })
                .collect()
        };
        let converted: Vec<(TrayKey, u32, Option<IconImage>)> = jobs
            .into_iter()
            .map(|(key, generation, copy)| {
                let icon = HICON(copy as *mut _);
                let image = crate::icons::icon_image(icon).or_else(|| mono_icon_image(icon));
                destroy_icon(copy);
                (key, generation, image)
            })
            .collect();
        let mut icons = lock(&self.shared.icons);
        for (key, generation, image) in converted {
            // An icon replaced while we converted stays stale for next time.
            let Some(entry) = icons
                .iter_mut()
                .find(|entry| entry.key == key && entry.generation == generation)
            else {
                continue;
            };
            entry.image = image.map(|new| CachedImage::reuse_or_new(entry.image.as_ref(), new));
            entry.stale = false;
        }
        let mut list = Vec::new();
        for entry in icons.iter() {
            if entry.hidden || entry.icon == 0 {
                continue;
            }
            let Some(image) = &entry.image else { continue };
            let mut pid = 0;
            // SAFETY: Read-only query on a window handle.
            unsafe { GetWindowThreadProcessId(HWND(entry.hwnd as *mut _), Some(&raw mut pid)) };
            list.push(TrayIconInfo {
                key: entry.key.clone(),
                tip: entry.tip.clone(),
                width: image.width,
                height: image.height,
                bgra: Arc::clone(&image.bgra),
                pid,
            });
        }
        list
    }

    /// Delivers a click to the icon's owner as Explorer would.
    pub fn click(&self, key: &TrayKey, button: TrayButton, at: (i32, i32)) {
        let target = lock(&self.shared.icons)
            .iter()
            .find(|entry| &entry.key == key)
            .map(|entry| (entry.hwnd, entry.uid, entry.callback, entry.version));
        let Some((hwnd, uid, callback, version)) = target else {
            return;
        };
        let hwnd = HWND(hwnd as *mut _);
        // SAFETY: Handle queries and posts; stale handles just fail.
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() || callback == 0 {
                return;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
            let _ = AllowSetForegroundWindow(pid);
            for (_, wparam, lparam) in click_messages(version, uid, button, at) {
                let _ = PostMessageW(Some(hwnd), callback, WPARAM(wparam), LPARAM(lparam));
            }
        }
    }
}

impl Drop for TrayHost {
    fn drop(&mut self) {
        // SAFETY: Posting to our own window; it is gone harmlessly if closed.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // The window thread is gone; a burst cut short left Explorer demoted.
        if self.shared.bursting.swap(false, Ordering::AcqRel) {
            restore_explorer_topmost();
        }
        for entry in lock(&self.shared.icons).drain(..) {
            if entry.icon != 0 {
                destroy_icon(entry.icon);
            }
        }
    }
}

/// Renders a monochrome (mask-only) icon by drawing it over black and over
/// white, which recovers its alpha.
fn mono_icon_image(icon: HICON) -> Option<IconImage> {
    let mut info = ICONINFO::default();
    // SAFETY: GetIconInfo creates two bitmaps, deleted below.
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;
    let mut desc = BITMAP::default();
    // SAFETY: `desc` is a BITMAP of the size passed.
    let written = unsafe {
        GetObjectW(
            info.hbmMask.into(),
            size_of::<BITMAP>() as i32,
            Some((&raw mut desc).cast()),
        )
    };
    // SAFETY: Both bitmaps are ours; a null color bitmap is skipped by Windows.
    unsafe {
        let _ = DeleteObject(info.hbmMask.into());
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(info.hbmColor.into());
        }
    }
    // The mask of a monochrome icon holds the AND and XOR images stacked.
    let (width, height) = (desc.bmWidth, desc.bmHeight / 2);
    if written == 0 || !(1..=256).contains(&width) || !(1..=256).contains(&height) {
        return None;
    }
    let draw = |fill: u8| -> Option<Vec<u8>> {
        let header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let len = (width * height * 4) as usize;
        let mut bits = std::ptr::null_mut();
        // SAFETY: The DIB section's `len` bytes live as long as the bitmap,
        // which is deselected and deleted before the copy goes out of scope.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bitmap = CreateDIBSection(
                Some(dc),
                &raw const header,
                DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            );
            let result = bitmap.ok().filter(|_| !bits.is_null()).map(|bitmap| {
                let old = SelectObject(dc, bitmap.into());
                let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), len);
                pixels.fill(fill);
                let _ = DrawIconEx(dc, 0, 0, icon, width, height, 0, None, DI_NORMAL);
                let copy = pixels.to_vec();
                SelectObject(dc, old);
                let _ = DeleteObject(bitmap.into());
                copy
            });
            let _ = DeleteDC(dc);
            result
        }
    };
    let (black, white) = (draw(0)?, draw(255)?);
    let mut bgra = black;
    for (out, over_white) in bgra
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(white.as_chunks::<4>().0)
    {
        // Over black the color is premultiplied; white minus black is 1 - alpha.
        out[3] = 255 - over_white[1].saturating_sub(out[1]);
    }
    Some(IconImage {
        width: width as u32,
        height: height as u32,
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide_field(text: &str, chars: usize) -> Vec<u8> {
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.resize(chars, 0);
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    }

    fn header(command: u32, cb_size: u32, hwnd: u32, uid: u32, flags: u32) -> Vec<u8> {
        [SIGNATURE, command, cb_size, hwnd, uid, flags, 0x401, 0x1234]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn full(command: u32, flags: u32, state: (u32, u32), version: u32) -> Vec<u8> {
        let mut data = header(command, 956, 0x1001, 7, flags);
        data.extend(wide_field("Hello tray", 128));
        data.extend(state.0.to_le_bytes());
        data.extend(state.1.to_le_bytes());
        data.extend(wide_field("", 256));
        data.extend(version.to_le_bytes());
        data.extend(wide_field("", 64));
        data.extend(0u32.to_le_bytes());
        data.extend([9u8; 16]);
        data.extend(0u32.to_le_bytes());
        data
    }

    fn message(command: u32, flags: u32) -> TrayMessage {
        TrayMessage {
            command,
            hwnd: 0x1001,
            uid: 7,
            flags,
            callback: 0x401,
            icon: 0x55,
            tip: "tip".into(),
            ..TrayMessage::default()
        }
    }

    fn run(icons: &mut Vec<Entry>, message: &TrayMessage) -> (bool, Vec<isize>) {
        let mut destroyed = Vec::new();
        let changed = apply(icons, message, &mut |icon| icon + 1000, &mut |icon| {
            destroyed.push(icon)
        });
        (changed, destroyed)
    }

    #[test]
    fn parses_a_full_add_with_icon_tip_and_guid() {
        let flags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_GUID;
        let data = full(NIM_ADD, flags, (0, 0), 0);
        let parsed = parse_tray_data(&data);
        let expected = TrayMessage {
            command: NIM_ADD,
            hwnd: 0x1001,
            uid: 7,
            flags,
            callback: 0x401,
            icon: 0x1234,
            tip: "Hello tray".into(),
            guid: Some([9; 16]),
            ..TrayMessage::default()
        };
        assert_eq!(parsed, Some(expected));
        assert_eq!(
            parsed.map(|m| key_of(&m)),
            Some(TrayKey(KeyKind::Guid([9; 16])))
        );
    }

    #[test]
    fn parses_old_structs_with_a_64_character_tip() {
        let mut data = header(NIM_ADD, 152, 0x10, 1, NIF_TIP);
        data.extend(wide_field("Old", 64));
        let parsed = parse_tray_data(&data);
        assert_eq!(parsed.as_ref().map(|m| m.tip.as_str()), Some("Old"));
        assert_eq!(
            parsed.map(|m| (m.state, m.version, m.guid)),
            Some((0, 0, None))
        );
        // A struct cut short at 88 bytes yields a shorter tip without overreading.
        let mut short = header(NIM_ADD, 88, 0x10, 1, NIF_TIP);
        short.extend(wide_field("Old", 32));
        assert_eq!(parse_tray_data(&short).map(|m| m.tip), Some("Old".into()));
    }

    #[test]
    fn rejects_truncated_buffers_and_wrong_signatures() {
        let data = full(NIM_ADD, NIF_TIP, (0, 0), 0);
        assert_eq!(parse_tray_data(&data[..20]), None);
        assert_eq!(parse_tray_data(&[]), None);
        let mut wrong = data.clone();
        wrong[0] ^= 0xFF;
        assert_eq!(parse_tray_data(&wrong), None);
        // A big cbSize with a short buffer never reads past the buffer.
        let cut = parse_tray_data(&data[..300]);
        assert_eq!(
            cut.map(|m| (m.tip, m.guid)),
            Some(("Hello tray".into(), None))
        );
    }

    #[test]
    fn reads_state_and_version_when_present() {
        let data = full(NIM_SETVERSION, NIF_STATE, (NIS_HIDDEN, NIS_HIDDEN), 4);
        let parsed = parse_tray_data(&data);
        assert_eq!(
            parsed.map(|m| (m.state, m.state_mask, m.version)),
            Some((1, 1, 4))
        );
    }

    #[test]
    fn add_modify_hide_and_delete_update_the_list() {
        let mut icons = Vec::new();
        let add = message(NIM_ADD, NIF_MESSAGE | NIF_ICON | NIF_TIP);
        assert!(run(&mut icons, &add).0);
        assert_eq!(
            (icons.len(), icons[0].icon, icons[0].callback),
            (1, 0x55 + 1000, 0x401)
        );
        assert_eq!(icons[0].tip, "tip");
        assert!(!icons[0].hidden);

        let mut modify = message(NIM_MODIFY, NIF_ICON | NIF_TIP | NIF_STATE);
        modify.icon = 0x66;
        modify.tip = "new".into();
        modify.state = NIS_HIDDEN;
        modify.state_mask = NIS_HIDDEN;
        let (changed, destroyed) = run(&mut icons, &modify);
        assert!(changed && icons[0].hidden && icons[0].tip == "new");
        assert_eq!((icons[0].icon, destroyed), (0x66 + 1000, vec![0x55 + 1000]));

        modify.state = 0;
        modify.flags = NIF_STATE;
        run(&mut icons, &modify);
        assert!(!icons[0].hidden);

        let mut version = message(NIM_SETVERSION, 0);
        version.version = 4;
        run(&mut icons, &version);
        assert_eq!(icons[0].version, 4);

        let (changed, destroyed) = run(&mut icons, &message(NIM_DELETE, 0));
        assert!(changed && icons.is_empty() && destroyed == vec![0x66 + 1000]);
        assert!(!run(&mut icons, &message(NIM_MODIFY, NIF_TIP)).0);
    }

    #[test]
    fn a_repeated_add_replaces_the_icon_and_guid_keys_match_across_windows() {
        let mut icons = Vec::new();
        let mut add = message(NIM_ADD, NIF_ICON | NIF_GUID);
        add.guid = Some([3; 16]);
        run(&mut icons, &add);
        add.hwnd = 0x2002;
        add.icon = 0x77;
        let (_, destroyed) = run(&mut icons, &add);
        assert_eq!(
            (icons.len(), icons[0].icon, destroyed),
            (1, 0x77 + 1000, vec![0x55 + 1000])
        );
        assert_eq!(icons[0].hwnd, 0x2002);
    }

    fn pixels(width: u32, fill: u8) -> IconImage {
        IconImage {
            width,
            height: 1,
            bgra: vec![fill; width as usize * 4],
        }
    }

    #[test]
    fn identical_pixels_reuse_the_old_buffer() {
        let first = CachedImage::reuse_or_new(None, pixels(2, 7));
        let same = CachedImage::reuse_or_new(Some(&first), pixels(2, 7));
        assert!(Arc::ptr_eq(&first.bgra, &same.bgra));
        let other = CachedImage::reuse_or_new(Some(&first), pixels(2, 8));
        assert!(!Arc::ptr_eq(&first.bgra, &other.bgra));
        let resized = CachedImage::reuse_or_new(Some(&first), pixels(3, 7));
        assert_eq!((resized.width, resized.bgra.len()), (3, 12));
    }

    #[test]
    fn a_re_add_keeps_the_pixels_but_marks_them_stale() {
        let mut icons = Vec::new();
        let add = message(NIM_ADD, NIF_ICON);
        run(&mut icons, &add);
        assert!(icons[0].stale && icons[0].image.is_none());
        icons[0].image = Some(CachedImage::reuse_or_new(None, pixels(1, 1)));
        icons[0].stale = false;
        let before = icons[0].generation;
        run(&mut icons, &add);
        assert!(icons[0].stale && icons[0].image.is_some());
        assert_ne!(icons[0].generation, before);
        // An add without an icon leaves nothing to show.
        run(&mut icons, &message(NIM_ADD, NIF_TIP));
        assert!(!icons[0].stale && icons[0].image.is_none());
    }

    #[test]
    fn refresh_waits_for_the_minimum_age() {
        let now = Instant::now();
        let age = Duration::from_secs(30);
        assert!(refresh_due(None, now, age));
        assert!(!refresh_due(Some(now), now + Duration::from_secs(29), age));
        assert!(refresh_due(Some(now), now + Duration::from_secs(30), age));
    }

    #[test]
    fn version_4_clicks_pack_coordinates_and_ids() {
        let wparam = (0x0102usize << 16) | 0x0304;
        let pack = |event: u32| (0x0007isize << 16) | event as isize;
        assert_eq!(
            click_messages(4, 7, TrayButton::Left, (0x304, 0x102)),
            vec![
                (WM_LBUTTONDOWN, wparam, pack(WM_LBUTTONDOWN)),
                (WM_LBUTTONUP, wparam, pack(WM_LBUTTONUP)),
                (NIN_SELECT, wparam, pack(NIN_SELECT)),
            ]
        );
        let right = click_messages(4, 7, TrayButton::Right, (0x304, 0x102));
        assert_eq!(
            right.iter().map(|m| m.0).collect::<Vec<_>>(),
            [WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU]
        );
        let double = click_messages(4, 7, TrayButton::Double, (0, 0));
        assert_eq!(
            double.iter().map(|m| m.0).collect::<Vec<_>>(),
            [WM_LBUTTONDBLCLK, NIN_SELECT]
        );
    }

    #[test]
    fn legacy_clicks_use_the_id_and_event() {
        assert_eq!(
            click_messages(0, 9, TrayButton::Left, (5, 6)),
            vec![
                (WM_LBUTTONDOWN, 9, WM_LBUTTONDOWN as isize),
                (WM_LBUTTONUP, 9, WM_LBUTTONUP as isize),
            ]
        );
        assert_eq!(click_messages(3, 9, TrayButton::Right, (5, 6)).len(), 2);
        assert_eq!(
            click_messages(0, 9, TrayButton::Double, (5, 6)),
            vec![(WM_LBUTTONDBLCLK, 9, WM_LBUTTONDBLCLK as isize)]
        );
    }

    #[test]
    fn negative_coordinates_wrap_to_signed_words() {
        let (_, wparam, _) = click_messages(4, 1, TrayButton::Left, (-1, -2))[0];
        assert_eq!(wparam & 0xFFFF, 0xFFFF);
        assert_eq!(wparam >> 16, 0xFFFE);
    }
}
