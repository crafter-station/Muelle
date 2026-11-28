//! The notification area's icons, for Muelle's own tray menu.
//!
//! Windows offers no way to list them. A program adds its icon with
//! `Shell_NotifyIcon`, which sends it to the first window of class
//! `Shell_TrayWnd`. This keeps a hidden window of that class in front of
//! Explorer's, notes what each program says, and passes every message on, so
//! Explorer's tray stays complete. Replacement taskbars such as RetroBar work
//! this way. When the window goes, with the dock or in a crash, programs reach
//! Explorer directly again and nothing is lost.
//!
//! Programs that added their icons earlier are asked to add them again with
//! the `TaskbarCreated` message Explorer itself sends after a restart. One that
//! ignores it stays only in Windows' own menu until it is restarted.

use std::{
    cell::RefCell,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::{
            DataExchange::COPYDATASTRUCT,
            LibraryLoader::GetModuleHandleW,
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CopyIcon, CreateWindowExW,
            DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, EnumWindows,
            FindWindowExW, FindWindowW, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetMessageW,
            GetPropW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, HICON,
            HWND_BOTTOM, HWND_TOPMOST, IsWindow, IsWindowVisible, MSG, MSGFLT_ALLOW, PM_NOREMOVE,
            PeekMessageW, PostMessageW, PostThreadMessageW, RegisterClassW, RegisterWindowMessageW,
            RemovePropW, SMTO_ABORTIFHUNG, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE,
            SWP_NOSIZE, SWP_NOZORDER, SendMessageTimeoutW, SendNotifyMessageW, SetPropW, SetTimer,
            SetWindowPos, WM_CONTEXTMENU, WM_COPYDATA, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
            WM_LBUTTONUP, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WM_USER, WNDCLASSW,
            WS_CAPTION, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
    core::{BOOL, PCWSTR, w},
};

use crate::{icons, icons::IconImage, taskbar, tracker};

const CLASS: PCWSTR = w!("Shell_TrayWnd");
/// Where `ITaskbarList` looks for its window (see `taskband`).
const TASKBAND: PCWSTR = w!("TaskbandHWND");
/// How often the window is checked to still be first in line.
const TICK_MS: u32 = 200;
/// Explorer answers a program at once, or is hung.
const FORWARD_MS: u32 = 4000;
/// After asking programs to add their icons again, Explorer refusing one it
/// already has is not passed on to the program.
const INVITATION: Duration = Duration::from_secs(20);
/// How many calls are being made with the window out of the way (see `aside`).
static ASIDE: AtomicUsize = AtomicUsize::new(0);

/// `COPYDATASTRUCT::dwData` of a `Shell_NotifyIcon` call.
const NOTIFY_ICON: usize = 1;
/// The first field of what it carries.
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
/// Sent for a click by icons of version 3 and later.
const NIN_SELECT: u32 = WM_USER;

/// One program's notification area icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayIcon {
    /// The program's window that is told about clicks.
    pub window: isize,
    pub uid: u32,
    guid: Option<u128>,
    /// The message it is told with; `None` if it asked for none.
    pub callback: Option<u32>,
    /// The behaviour version it asked for.
    pub version: u32,
    /// Its tooltip, which may span several lines.
    pub tip: String,
    pub hidden: bool,
    pub image: Option<Arc<IconImage>>,
}

impl TrayIcon {
    /// Whether both are the same icon, whatever it shows now.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        match (self.guid, other.guid) {
            (Some(a), Some(b)) => a == b,
            _ => self.window == other.window && self.uid == other.uid,
        }
    }

    /// The process that owns the icon.
    #[must_use]
    pub fn process(&self) -> u32 {
        let mut process = 0;
        // SAFETY: Read-only query; a window that is gone leaves zero.
        unsafe { GetWindowThreadProcessId(HWND(self.window as *mut _), Some(&mut process)) };
        process
    }

    fn alive(&self) -> bool {
        // SAFETY: Read-only check of a window handle.
        unsafe { IsWindow(Some(HWND(self.window as *mut _))) }.as_bool()
    }
}

/// What one `Shell_NotifyIcon` call says.
#[derive(Debug, Default, PartialEq, Eq)]
struct Notice {
    message: u32,
    window: isize,
    uid: u32,
    flags: u32,
    callback: u32,
    icon: u32,
    tip: String,
    state: u32,
    state_mask: u32,
    version: u32,
    guid: Option<u128>,
}

/// Reads the call from what travels to the tray: a signature and the message,
/// then `NOTIFYICONDATAW` with 32-bit handles, whatever the program. Older
/// programs send a shorter one.
fn parse(bytes: &[u8]) -> Option<Notice> {
    let word = |at: usize| {
        bytes
            .get(at..at + 4)
            .and_then(|four| four.try_into().ok())
            .map(u32::from_le_bytes)
    };
    if word(0)? != SIGNATURE {
        return None;
    }
    // Offsets within `NOTIFYICONDATAW`, which starts after those two.
    let data = |at: usize| word(8 + at);
    let flags = data(12)?;
    // The tooltip had room for 64 characters before it had room for 128.
    let size = data(0)? as usize;
    let tip_room = if size >= 280 { 128 } else { 64 };
    let (pairs, _) = bytes.get(8 + 24..).unwrap_or_default().as_chunks::<2>();
    let tip: Vec<u16> = pairs
        .iter()
        .take(tip_room)
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    let guid = (flags & NIF_GUID != 0)
        .then(|| bytes.get(8 + 936..8 + 952))
        .flatten()
        .and_then(|sixteen| sixteen.try_into().ok())
        .map(u128::from_le_bytes)
        .filter(|&guid| guid != 0);
    Some(Notice {
        message: word(4)?,
        // A 32-bit handle widens with its sign.
        window: data(4)? as i32 as isize,
        uid: data(8)?,
        flags,
        callback: data(16).unwrap_or(0),
        icon: data(20).unwrap_or(0),
        tip: String::from_utf16_lossy(&tip),
        state: data(280).unwrap_or(0),
        state_mask: data(284).unwrap_or(0),
        version: data(800).unwrap_or(0),
        guid,
    })
}

/// The icons as the programs have described them, oldest first.
#[derive(Default)]
struct Icons(Vec<TrayIcon>);

/// What a notice did to the list.
#[derive(Debug, PartialEq, Eq)]
enum Change {
    None,
    Changed,
    /// Learned from a change, not an addition: kept only if Explorer knows it.
    Discovered(usize),
}

impl Icons {
    fn find(&self, notice: &Notice) -> Option<usize> {
        let by_guid = notice
            .guid
            .and_then(|guid| self.0.iter().position(|icon| icon.guid == Some(guid)));
        by_guid.or_else(|| {
            self.0
                .iter()
                .position(|icon| icon.window == notice.window && icon.uid == notice.uid)
        })
    }

    fn apply(&mut self, notice: &Notice, image: Option<IconImage>) -> Change {
        let at = self.find(notice);
        match (notice.message, at) {
            (NIM_DELETE, Some(at)) => {
                self.0.remove(at);
                Change::Changed
            }
            (NIM_SETVERSION, Some(at)) => {
                self.0[at].version = notice.version;
                Change::Changed
            }
            (NIM_ADD, at) => {
                let mut icon = TrayIcon {
                    window: notice.window,
                    uid: notice.uid,
                    guid: notice.guid,
                    callback: None,
                    version: 0,
                    tip: String::new(),
                    hidden: false,
                    image: None,
                };
                update(&mut icon, notice, image);
                match at {
                    Some(at) => self.0[at] = icon,
                    None => self.0.push(icon),
                }
                Change::Changed
            }
            (NIM_MODIFY, Some(at)) => {
                let before = self.0[at].clone();
                update(&mut self.0[at], notice, image);
                if self.0[at] == before {
                    Change::None
                } else {
                    Change::Changed
                }
            }
            // Added before anyone was listening, and now described in full.
            (NIM_MODIFY, None) if notice.flags & NIF_MESSAGE != 0 && image.is_some() => {
                let added = Notice {
                    message: NIM_ADD,
                    tip: notice.tip.clone(),
                    ..*notice
                };
                self.apply(&added, image);
                Change::Discovered(self.0.len() - 1)
            }
            _ => Change::None,
        }
    }

    /// The icons to offer, after forgetting those of programs that ended.
    fn shown(&mut self) -> Vec<TrayIcon> {
        self.0.retain(TrayIcon::alive);
        self.0.iter().filter(|icon| !icon.hidden).cloned().collect()
    }
}

/// Copies into `icon` the parts the notice carries.
fn update(icon: &mut TrayIcon, notice: &Notice, image: Option<IconImage>) {
    icon.window = notice.window;
    icon.uid = notice.uid;
    if notice.flags & NIF_MESSAGE != 0 {
        icon.callback = (notice.callback != 0).then_some(notice.callback);
    }
    if notice.flags & NIF_ICON != 0 {
        icon.image = image.map(Arc::new);
    }
    if notice.flags & NIF_TIP != 0 {
        icon.tip.clone_from(&notice.tip);
    }
    if notice.flags & NIF_STATE != 0 && notice.state_mask & NIS_HIDDEN != 0 {
        icon.hidden = notice.state & NIS_HIDDEN != 0;
    }
}

/// The pixels of a program's icon, copied while its call waits for us.
fn picture(handle: u32) -> Option<IconImage> {
    if handle == 0 {
        return None;
    }
    let theirs = HICON(handle as i32 as isize as *mut _);
    // SAFETY: Icon handles are shared by the session; the copy is ours and
    // destroyed here.
    unsafe {
        let ours = CopyIcon(theirs).ok()?;
        let image = icons::icon_image(ours);
        let _ = DestroyIcon(ours);
        image
    }
}

#[derive(Default)]
struct Shared {
    icons: Mutex<Icons>,
    /// Counts the changes, so a reader knows when to look again.
    generation: AtomicU64,
}

/// What the window's thread keeps.
struct Host {
    shared: Arc<Shared>,
    window: Option<HWND>,
    /// The taskbar programs were last asked to add their icons for.
    invited: Option<(HWND, Instant)>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// Collects the notification area's icons for as long as it lives.
pub struct TrayHost {
    shared: Arc<Shared>,
    thread: Option<(u32, JoinHandle<()>)>,
}

impl TrayHost {
    /// Starts on a thread of its own, so that no program ever waits for the
    /// dock to finish drawing.
    #[must_use]
    pub fn start() -> Option<Self> {
        let shared = Arc::new(Shared::default());
        let (ready, started) = mpsc::channel();
        let theirs = shared.clone();
        let thread = thread::Builder::new()
            .name("muelle-tray".into())
            .spawn(move || run(theirs, &ready))
            .inspect_err(|error| crate::log::line(&format!("bandeja: {error}")))
            .ok()?;
        let id = started.recv().ok()?;
        Some(Self {
            shared,
            thread: Some((id, thread)),
        })
    }

    /// The icons the programs show now.
    #[must_use]
    pub fn icons(&self) -> Vec<TrayIcon> {
        self.shared
            .icons
            .lock()
            .map_or_else(|_| Vec::new(), |mut icons| icons.shown())
    }

    /// A number that changes whenever the icons do.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }
}

impl Drop for TrayHost {
    fn drop(&mut self) {
        if let Some((id, thread)) = self.thread.take() {
            // SAFETY: Only posts a message to our own thread.
            let posted = unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            if posted.is_ok() {
                let _ = thread.join();
            }
        }
    }
}

fn run(shared: Arc<Shared>, ready: &mpsc::Sender<u32>) {
    let mut message = MSG::default();
    // SAFETY: Creates this thread's queue, so that WM_QUIT can reach it, then
    // runs its message loop; the window it creates is destroyed before leaving.
    unsafe {
        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
        HOST.set(Some(Host {
            shared,
            window: None,
            invited: None,
        }));
        let timer = SetTimer(None, 0, TICK_MS, None);
        let _ = ready.send(GetCurrentThreadId());
        maintain();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            if message.hwnd.is_invalid() && message.message == WM_TIMER && message.wParam.0 == timer
            {
                maintain();
            } else {
                DispatchMessageW(&message);
            }
        }
        leave();
        HOST.set(None);
    }
}

/// Keeps the window in front of Explorer's while there is one to pass
/// messages on to, and asks for the icons again for each new one.
fn maintain() {
    let Some(real) = taskbar::primary() else {
        // Explorer is restarting: nothing must stand in its way.
        leave();
        return;
    };
    let held = HOST.with_borrow(|host| host.as_ref().map(|host| (host.window, host.invited)));
    let Some((window, invited)) = held else {
        return;
    };
    // SAFETY: Our own window, and read-only queries on Explorer's.
    unsafe {
        let window = match window.filter(|&window| IsWindow(Some(window)).as_bool()) {
            Some(window) => window,
            None => match create() {
                Ok(window) => window,
                Err(error) => {
                    crate::log::line(&format!("bandeja: {error}"));
                    return;
                }
            },
        };
        mirror(window, real);
        if ASIDE.load(Ordering::SeqCst) == 0 && FindWindowW(CLASS, None).ok() != Some(window) {
            restack(window, HWND_TOPMOST);
        }
        let new_taskbar = invited.is_none_or(|(taskbar, _)| taskbar != real);
        HOST.with_borrow_mut(|host| {
            if let Some(host) = host {
                host.window = Some(window);
                if new_taskbar {
                    host.invited = Some((real, Instant::now()));
                }
            }
        });
        if new_taskbar {
            invite();
        }
    }
}

/// Moves a stand-in to the front or the back of the line.
fn restack(window: HWND, place: HWND) {
    // SAFETY: Changes only the order of a window of ours.
    unsafe {
        let _ = SetWindowPos(
            window,
            Some(place),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// This process's stand-in windows.
fn own_windows() -> Vec<HWND> {
    let mut found = Vec::new();
    let mut previous = None;
    // SAFETY: Read-only queries with a static class name.
    unsafe {
        while let Ok(window) = FindWindowExW(None, previous, CLASS, None) {
            let mut process = 0;
            GetWindowThreadProcessId(window, Some(&mut process));
            if process == GetCurrentProcessId() && !GetPropW(window, taskbar::STAND_IN).is_invalid()
            {
                found.push(window);
            }
            previous = Some(window);
        }
    }
    found
}

/// Makes `call` with this process's stand-in behind Explorer's taskbar.
///
/// An AppBar request that expects an answer, such as a reservation, is lost
/// without an error when the stand-in it goes through belongs to the process
/// asking. Other programs' requests pass on as usual.
pub(crate) fn aside<T>(call: impl FnOnce() -> T) -> T {
    ASIDE.fetch_add(1, Ordering::SeqCst);
    let windows = own_windows();
    for &window in &windows {
        restack(window, HWND_BOTTOM);
    }
    let answer = call();
    if ASIDE.fetch_sub(1, Ordering::SeqCst) == 1 {
        for &window in &windows {
            restack(window, HWND_TOPMOST);
        }
    }
    answer
}

/// Programs that look the taskbar up find where it is and its overlay window.
///
/// # Safety
/// `window` must be this thread's window.
unsafe fn mirror(window: HWND, real: HWND) {
    // SAFETY: Reads from Explorer's window, writes to ours.
    unsafe {
        let band = GetPropW(real, TASKBAND);
        if !band.is_invalid() && GetPropW(window, TASKBAND) != band {
            let _ = SetPropW(window, TASKBAND, Some(band));
        }
        let (mut theirs, mut ours) = (RECT::default(), RECT::default());
        if GetWindowRect(real, &mut theirs).is_ok()
            && GetWindowRect(window, &mut ours).is_ok()
            && theirs != ours
        {
            let _ = SetWindowPos(
                window,
                None,
                theirs.left,
                theirs.top,
                theirs.right - theirs.left,
                theirs.bottom - theirs.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// # Safety
/// Called on the thread that will pump the window's messages.
unsafe fn create() -> windows::core::Result<HWND> {
    // SAFETY: The class and window belong to this thread.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            ..Default::default()
        });
        // Never shown.
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        // Marked before anything of ours could mistake it for the taskbar.
        if let Err(error) = SetPropW(window, taskbar::STAND_IN, Some(HANDLE(window.0))) {
            let _ = DestroyWindow(window);
            return Err(error);
        }
        // Programs running with fewer rights add icons too.
        let _ = ChangeWindowMessageFilterEx(window, WM_COPYDATA, MSGFLT_ALLOW, None);
        Ok(window)
    }
}

fn leave() {
    let window = HOST.with_borrow_mut(|host| host.as_mut().and_then(|host| host.window.take()));
    if let Some(window) = window {
        // SAFETY: Our own window and its properties.
        unsafe {
            let _ = RemovePropW(window, TASKBAND);
            let _ = RemovePropW(window, taskbar::STAND_IN);
            let _ = DestroyWindow(window);
        }
    }
}

/// Asks every other program to add its icon again, as a restarted Explorer
/// does. The dock itself would rebuild its bars on hearing it.
fn invite() {
    unsafe extern "system" fn tell(window: HWND, message: LPARAM) -> BOOL {
        let mut process = 0;
        // SAFETY: A query and a posted message; neither waits on the window.
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut process));
            if process != GetCurrentProcessId() {
                let _ = PostMessageW(Some(window), message.0 as u32, WPARAM(0), LPARAM(0));
            }
        }
        true.into()
    }
    // SAFETY: The callback only posts a registered message.
    unsafe {
        let message = RegisterWindowMessageW(w!("TaskbarCreated"));
        if message != 0 {
            let _ = EnumWindows(Some(tell), LPARAM(message as isize));
        }
    }
}

/// Passes a message on to Explorer's taskbar and returns its answer.
fn forward(message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(real) = taskbar::primary() else {
        return LRESULT(0);
    };
    let mut answer = 0usize;
    // SAFETY: The parameters are those the sender meant for this window, and
    // stay valid while it waits; `answer` lives for the call.
    unsafe {
        SendMessageTimeoutW(
            real,
            message,
            w,
            l,
            SMTO_ABORTIFHUNG,
            FORWARD_MS,
            Some(ptr::from_mut(&mut answer)),
        );
    }
    LRESULT(answer as isize)
}

/// A `Shell_NotifyIcon` call, or another request carried the same way.
fn copy_data(w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: WM_COPYDATA carries a COPYDATASTRUCT that Windows keeps valid,
    // with `cbData` bytes at `lpData`, until the message returns.
    let notice = unsafe {
        (l.0 as *const COPYDATASTRUCT)
            .as_ref()
            .filter(|data| data.dwData == NOTIFY_ICON && !data.lpData.is_null())
            .and_then(|data| {
                parse(std::slice::from_raw_parts(
                    data.lpData.cast::<u8>(),
                    data.cbData as usize,
                ))
            })
    };
    let held = HOST.with_borrow(|host| {
        host.as_ref()
            .map(|host| (host.shared.clone(), host.invited))
    });
    // No borrow or lock is held while Explorer answers: another program's call
    // may arrive meanwhile.
    let (Some(notice), Some((shared, invited))) = (notice, held) else {
        return forward(WM_COPYDATA, w, l);
    };
    let image = (notice.flags & NIF_ICON != 0)
        .then(|| picture(notice.icon))
        .flatten();
    let change = shared
        .icons
        .lock()
        .map_or(Change::None, |mut icons| icons.apply(&notice, image));
    let mut answer = forward(WM_COPYDATA, w, l);
    if let Change::Discovered(at) = change
        && answer.0 == 0
        && let Ok(mut icons) = shared.icons.lock()
        && at < icons.0.len()
    {
        icons.0.remove(at);
    }
    if change != Change::None {
        shared.generation.fetch_add(1, Ordering::Relaxed);
    }
    // Explorer already has the icon a program adds again at our asking; the
    // program must not take that for failure.
    if notice.message == NIM_ADD && invited.is_some_and(|(_, at)| at.elapsed() < INVITATION) {
        answer = LRESULT(1);
    }
    answer
}

unsafe extern "system" fn proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_COPYDATA => copy_data(w, l),
        // What programs ask of the taskbar: commands and its private messages.
        0x0111 => forward(message, w, l),
        WM_USER..0xC000 => forward(message, w, l),
        // SAFETY: Standard forwarding of unhandled messages.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

/// A window a program has on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    pub window: isize,
    pub bounds: RECT,
    /// A menu or a flyout, rather than a window of its own.
    pub menu: bool,
    /// One of Windows' own menus, which Escape and any click close.
    pub standard: bool,
}

/// The class of Windows' own popup menus.
const MENU_CLASS: &str = "#32768";
/// Windows that come with others and say nothing by showing.
const COMPANIONS: [&str; 4] = ["tooltips_class32", "SysShadow", "IME", "MSCTFIME UI"];

/// Whether a window of this class and style is a menu or a flyout: one of
/// Windows' own, or one without a title bar that floats above the others, as a
/// popup or, like the WinUI ones, kept off the taskbar.
fn is_menu(class: &str, style: u32, extended: u32) -> bool {
    if class == MENU_CLASS {
        return true;
    }
    let titled = style & WS_CAPTION.0 == WS_CAPTION.0;
    let popup = style & WS_POPUP.0 != 0;
    let above = extended & WS_EX_TOPMOST.0 != 0;
    let tool = extended & WS_EX_TOOLWINDOW.0 != 0;
    !titled && ((popup && (above || tool)) || (above && tool))
}

/// The windows `process` has on screen, Windows' own menus first.
#[must_use]
pub fn windows_of(process: u32) -> Vec<Shown> {
    struct Search {
        process: u32,
        found: Vec<Shown>,
    }
    unsafe extern "system" fn look(window: HWND, search: LPARAM) -> BOOL {
        // SAFETY: `search` points to the value below, alive for the whole
        // enumeration; the rest are read-only queries with valid buffers.
        unsafe {
            let search = &mut *(search.0 as *mut Search);
            let mut owner = 0;
            GetWindowThreadProcessId(window, Some(&mut owner));
            if owner != search.process
                || !IsWindowVisible(window).as_bool()
                || tracker::is_cloaked(window)
            {
                return true.into();
            }
            let mut bounds = RECT::default();
            // Smaller than any menu: a helper window.
            if GetWindowRect(window, &mut bounds).is_err()
                || bounds.right - bounds.left < 48
                || bounds.bottom - bounds.top < 24
            {
                return true.into();
            }
            let mut name = [0u16; 64];
            let length = GetClassNameW(window, &mut name).max(0) as usize;
            let class = String::from_utf16_lossy(&name[..length]);
            if COMPANIONS.contains(&class.as_str()) {
                return true.into();
            }
            let menu = is_menu(
                &class,
                GetWindowLongPtrW(window, GWL_STYLE) as u32,
                GetWindowLongPtrW(window, GWL_EXSTYLE) as u32,
            );
            search.found.push(Shown {
                window: window.0 as isize,
                bounds,
                menu,
                standard: class == MENU_CLASS,
            });
        }
        true.into()
    }
    let mut search = Search {
        process,
        found: Vec::new(),
    };
    // SAFETY: The callback only writes to `search`, which outlives the call.
    unsafe {
        let _ = EnumWindows(Some(look), LPARAM(ptr::from_mut(&mut search) as isize));
    }
    search.found.sort_by_key(|shown| !shown.standard);
    search.found
}

/// Moves a program's window without resizing it, raising it or waiting for
/// the program.
pub fn move_window(window: isize, x: i32, y: i32) {
    // SAFETY: A request that Windows refuses for a window that is gone, or one
    // of a program with more rights than ours.
    unsafe {
        let _ = SetWindowPos(
            HWND(window as *mut _),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// A click on an icon, in Muelle's tray menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Left,
    /// The second of two quick left clicks.
    Double,
    Right,
}

/// The messages Explorer tells a program for a click, in order.
fn told(click: Click, version: u32) -> Vec<u32> {
    let modern = version >= 3;
    match click {
        Click::Left if modern => vec![WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        Click::Left => vec![WM_LBUTTONDOWN, WM_LBUTTONUP],
        Click::Double => vec![WM_LBUTTONDBLCLK, WM_LBUTTONUP],
        Click::Right if modern => vec![WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
        Click::Right => vec![WM_RBUTTONDOWN, WM_RBUTTONUP],
    }
}

/// `wParam` and `lParam` for one of them: from version 4 they carry the screen
/// point and the icon beside the message.
fn packed(version: u32, message: u32, uid: u32, (x, y): (i32, i32)) -> (usize, isize) {
    if version >= 4 {
        let point = (x as u32 & 0xFFFF) | ((y as u32 & 0xFFFF) << 16);
        let what = (message & 0xFFFF) | ((uid & 0xFFFF) << 16);
        (point as usize, what as isize)
    } else {
        (uid as usize, message as isize)
    }
}

/// Tells the icon's program it was clicked at `point` on screen, as Explorer
/// would, letting it come to the front. `false` if it cannot be told.
pub fn click(icon: &TrayIcon, click: Click, point: (i32, i32)) -> bool {
    let Some(callback) = icon.callback else {
        return false;
    };
    if !icon.alive() {
        return false;
    }
    let window = HWND(icon.window as *mut _);
    // SAFETY: Posts scalar messages to the window the program gave for them.
    unsafe {
        // Fails unless this process received the last input, which it did.
        let _ = AllowSetForegroundWindow(icon.process());
        told(click, icon.version).into_iter().all(|message| {
            let (w, l) = packed(icon.version, message, icon.uid, point);
            SendNotifyMessageW(window, callback, WPARAM(w), LPARAM(l)).is_ok()
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A call as `Shell_NotifyIcon` sends it.
    fn wire(message: u32, flags: u32, uid: u32, tip: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 8 + 956];
        let mut put = |at: usize, value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(0, SIGNATURE);
        put(4, message);
        put(8, 956);
        put(12, 0x8000_1234);
        put(16, uid);
        put(20, flags);
        put(24, WM_USER + 7);
        put(288, NIS_HIDDEN);
        put(292, NIS_HIDDEN);
        put(808, 4);
        for (index, unit) in tip.encode_utf16().enumerate() {
            let at = 32 + 2 * index;
            bytes[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    fn notice(message: u32, flags: u32, uid: u32, tip: &str) -> Notice {
        parse(&wire(message, flags, uid, tip)).unwrap()
    }

    #[test]
    fn a_call_is_read_where_each_field_travels() {
        let read = notice(NIM_ADD, NIF_MESSAGE | NIF_TIP, 7, "OneDrive\nUp to date");
        assert_eq!(read.uid, 7);
        assert_eq!(read.window, 0x8000_1234_u32 as i32 as isize);
        assert_eq!(read.callback, WM_USER + 7);
        assert_eq!(read.tip, "OneDrive\nUp to date");
        assert_eq!(read.version, 4);
        assert_eq!(read.guid, None);
        // Not a tray call, or cut short.
        assert_eq!(parse(&[0; 64]), None);
        assert_eq!(parse(&wire(NIM_ADD, 0, 1, "")[..16]), None);
    }

    #[test]
    fn a_guid_names_the_icon_when_given() {
        let mut bytes = wire(NIM_ADD, NIF_GUID, 1, "");
        bytes[8 + 936..8 + 952].copy_from_slice(&42u128.to_le_bytes());
        assert_eq!(parse(&bytes).unwrap().guid, Some(42));
    }

    #[test]
    fn icons_follow_what_programs_say() {
        let mut icons = Icons::default();
        let add = notice(NIM_ADD, NIF_MESSAGE | NIF_TIP, 1, "One");
        assert_eq!(icons.apply(&add, None), Change::Changed);
        assert_eq!(icons.0[0].callback, Some(WM_USER + 7));
        // A change touches only what it carries.
        let rename = notice(NIM_MODIFY, NIF_TIP, 1, "Renamed");
        assert_eq!(icons.apply(&rename, None), Change::Changed);
        assert_eq!(icons.apply(&rename, None), Change::None);
        assert_eq!(icons.0[0].tip, "Renamed");
        assert_eq!(icons.0[0].callback, Some(WM_USER + 7));
        icons.apply(&notice(NIM_SETVERSION, 0, 1, ""), None);
        assert_eq!(icons.0[0].version, 4);
        icons.apply(&notice(NIM_MODIFY, NIF_STATE, 1, ""), None);
        assert!(icons.0[0].hidden);
        // Adding it again replaces it.
        icons.apply(&add, None);
        assert_eq!(icons.0.len(), 1);
        assert!(!icons.0[0].hidden);
        icons.apply(&notice(NIM_DELETE, 0, 1, ""), None);
        assert!(icons.0.is_empty());
    }

    #[test]
    fn an_icon_added_unheard_appears_once_described_in_full() {
        let image = || IconImage {
            width: 1,
            height: 1,
            bgra: vec![0; 4],
        };
        let mut icons = Icons::default();
        let tip = notice(NIM_MODIFY, NIF_TIP, 3, "Late");
        assert_eq!(icons.apply(&tip, None), Change::None);
        let full = notice(NIM_MODIFY, NIF_MESSAGE | NIF_ICON | NIF_TIP, 3, "Late");
        assert_eq!(icons.apply(&full, Some(image())), Change::Discovered(0));
        assert_eq!(icons.0[0].tip, "Late");
    }

    /// Reserves a strip from the process that hosts the stand-in, as the dock
    /// and the menu bar do. Maximized windows move down for a moment.
    #[test]
    #[ignore = "Reserves part of the live screen for a few seconds"]
    fn native_own_reservations_reach_explorer_past_the_host() {
        use crate::{
            appbar::{AppBar, Edge},
            system,
        };
        unsafe extern "system" fn plain(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            // SAFETY: Standard forwarding.
            unsafe { DefWindowProcW(h, m, w, l) }
        }
        let monitor = || system::primary_monitor().expect("a primary monitor");
        let top = || {
            thread::sleep(Duration::from_millis(1000));
            monitor().work_top
        };
        let host = TrayHost::start().expect("the host starts");
        thread::sleep(Duration::from_millis(500));
        // SAFETY: This test's own window, destroyed before it ends.
        unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            let class = w!("MuelleAppBarTest");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(plain),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let window = CreateWindowExW(
                Default::default(),
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let before = top();
            let mut bar = AppBar::register(window, WM_USER + 1).expect("registers");
            bar.reserve(Edge::Top, monitor().screen, 60);
            assert_eq!(top(), before + 60);
            drop(bar);
            assert_eq!(top(), before);
            let _ = DestroyWindow(window);
        }
        drop(host);
    }

    #[test]
    fn menus_are_told_from_windows() {
        assert!(is_menu("#32768", 0, 0));
        // A flyout a program draws itself.
        assert!(is_menu("Chrome_WidgetWin_1", WS_POPUP.0, WS_EX_TOPMOST.0));
        // Logitech G HUB's, a WinUI window that is not a popup.
        assert!(is_menu("WinUIDesktopWin32WindowClass", 0x1403_0000, 0x88));
        // A frameless window of an app, and a dialog.
        assert!(!is_menu("Chrome_WidgetWin_1", WS_POPUP.0, 0));
        assert!(!is_menu(
            "#32770",
            WS_POPUP.0 | WS_CAPTION.0,
            WS_EX_TOPMOST.0
        ));
    }

    #[test]
    fn clicks_are_told_as_explorer_tells_them() {
        assert_eq!(told(Click::Left, 0), [WM_LBUTTONDOWN, WM_LBUTTONUP]);
        assert_eq!(told(Click::Left, 4)[2], NIN_SELECT);
        assert_eq!(told(Click::Right, 4)[2], WM_CONTEXTMENU);
        assert_eq!(packed(0, WM_LBUTTONUP, 9, (10, 20)), (9, 0x0202));
        assert_eq!(
            packed(4, WM_LBUTTONUP, 9, (10, 20)),
            (0x0014_000A, 0x0009_0202)
        );
    }

    /// Adds a real icon through Windows while the host listens, clicks it and
    /// removes it. Explorer's tray shows it for a moment.
    #[test]
    #[ignore = "Stands in front of the live taskbar for a few seconds"]
    fn native_host_hears_real_icons_and_passes_them_on() {
        use windows::Win32::UI::{
            Shell::{
                ABM_GETSTATE, APPBARDATA, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, SHAppBarMessage,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{IDI_APPLICATION, LoadIconW, PM_REMOVE},
        };
        static HEARD: Mutex<Vec<u32>> = Mutex::new(Vec::new());
        unsafe extern "system" fn owner(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            if message == WM_USER + 77 {
                HEARD.lock().unwrap().push(l.0 as u32 & 0xFFFF);
                return LRESULT(0);
            }
            // SAFETY: Standard forwarding.
            unsafe { DefWindowProcW(h, message, w, l) }
        }
        // SAFETY: This test's own window and icon, removed before it ends.
        unsafe {
            let real = taskbar::primary().expect("Explorer's taskbar");
            let state = || {
                let mut data = APPBARDATA {
                    cbSize: size_of::<APPBARDATA>() as u32,
                    ..Default::default()
                };
                SHAppBarMessage(ABM_GETSTATE, &mut data)
            };
            let before = state();
            let host = TrayHost::start().expect("the host starts");
            thread::sleep(Duration::from_millis(500));
            let first = FindWindowW(CLASS, None).unwrap();
            assert_ne!(first, real, "the host is first in line");
            assert_eq!(taskbar::primary(), Some(real), "and is not the taskbar");
            // Requests about the taskbar still get Explorer's answer.
            assert_eq!(state(), before);

            let instance = GetModuleHandleW(None).unwrap();
            let class = w!("MuelleTrayTest");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(owner),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let window = CreateWindowExW(
                Default::default(),
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let mut data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: window,
                uID: 5,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_USER + 77,
                hIcon: LoadIconW(None, IDI_APPLICATION).unwrap(),
                ..Default::default()
            };
            let name: Vec<u16> = "Muelle tray test".encode_utf16().collect();
            data.szTip[..name.len()].copy_from_slice(&name);
            assert!(Shell_NotifyIconW(NIM_ADD, &data).as_bool());
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            assert!(Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool());
            // Explorer has it too: it accepts a change to it.
            assert!(Shell_NotifyIconW(NIM_MODIFY, &data).as_bool());

            let mine = || {
                host.icons()
                    .into_iter()
                    .find(|icon| icon.window == window.0 as isize)
            };
            let icon = mine().expect("the host heard of it");
            assert_eq!(icon.tip, "Muelle tray test");
            assert_eq!((icon.uid, icon.version), (5, 4));
            assert!(icon.image.as_ref().is_some_and(|image| image.width >= 16));
            assert!(click(&icon, Click::Left, (100, 100)));
            let mut message = MSG::default();
            let until = Instant::now() + Duration::from_millis(500);
            while Instant::now() < until {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&message);
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                *HEARD.lock().unwrap(),
                [WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT]
            );
            // Others, asked to, added theirs again; some take their time.
            thread::sleep(Duration::from_secs(3));
            for icon in host.icons() {
                println!(
                    "{:?} version {} picture {:?} hidden {} callback {:?} from {:?}",
                    icon.tip,
                    icon.version,
                    icon.image.as_ref().map(|image| image.width),
                    icon.hidden,
                    icon.callback,
                    crate::tracker::process_image(icon.process())
                );
            }
            assert!(Shell_NotifyIconW(NIM_DELETE, &data).as_bool());
            assert!(mine().is_none());
            let _ = DestroyWindow(window);
            drop(host);
            assert_eq!(FindWindowW(CLASS, None).ok(), Some(real));
        }
    }
}
