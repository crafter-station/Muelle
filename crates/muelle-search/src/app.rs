//! The search window: its state, input handling and message procedure.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use muelle_config::{Appearance, Config, Hotkey, Material, SearchConfig, SearchMonitor};
use muelle_shell::search_link;
use muelle_shell::system::{accent_color, transparency_enabled, uses_light_theme};
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, MonitorFromWindow, UpdateWindow, ValidateRect,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CreateMutexW, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_NEXT, VK_OEM_COMMA, VK_PRIOR,
    VK_RETURN, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GetCaretBlinkTime, GetCursorPos, GetForegroundWindow, GetMessageW,
    HWND_TOPMOST, IDC_ARROW, IsWindowVisible, KillTimer, LoadCursorW, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SC_CLOSE, SC_KEYMENU, SW_HIDE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_CHAR, WM_CLOSE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_PAINT, WM_SYSCHAR, WM_SYSCOMMAND, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows::core::{BOOL, Error, Result, w};

use crate::contents;
use crate::dictionary::{Definition, Dictionary};
use crate::history::History;
use crate::icons::{self, Loaded};
use crate::index;
use crate::input::{self, Edit, Typing, key_down};
use crate::render::{self, Layout, Look, Renderer, View};
use crate::search::{self, Contents, Entry, Extras, Kind, MAX_ROWS, Query, Row};
use crate::strings::{self, Strings};
use crate::textfield::TextField;
use crate::{hotkey, shell};

const WM_APP_INDEX: u32 = WM_APP + 2;
const WM_APP_ICON: u32 = WM_APP + 3;
const WM_APP_CONTENTS: u32 = WM_APP + 5;

/// The tallest a full definition gets before it scrolls.
const MAX_PANEL: f32 = 440.0;
/// How far one notch of the mouse wheel scrolls.
const SCROLL_STEP: f32 = 48.0;

const CARET_TIMER: usize = 1;
/// Reclaims the shortcut if Settings never gives it back.
const RESUME_TIMER: usize = 2;
const RESUME_AFTER_MS: u32 = 30_000;
const ICON_CACHE_LIMIT: usize = 1000;

enum IndexUpdate {
    Apps(Vec<Entry>),
    /// Always the last message of a rebuild, with the request's number.
    Files(u64, Vec<Entry>),
}

/// Builds the index on its own thread each time it is asked to.
fn spawn_indexer(window: HWND) -> (Sender<(u64, index::Options)>, Receiver<IndexUpdate>) {
    let (requests, incoming) = channel::<(u64, index::Options)>();
    let (outgoing, updates) = channel();
    let window = window.0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let publish = |update| {
            let sent = outgoing.send(update).is_ok();
            unsafe {
                let target = Some(HWND(window as *mut _));
                let _ = PostMessageW(target, WM_APP_INDEX, WPARAM(0), LPARAM(0));
            }
            sent
        };
        while let Ok(mut request) = incoming.recv() {
            // Only the newest settings matter.
            while let Ok(newer) = incoming.try_recv() {
                request = newer;
            }
            // On failure, keep whatever list of apps we already have.
            if let Ok(apps) = index::apps() {
                publish(IndexUpdate::Apps(apps));
            }
            let files = index::files(&request.1);
            if !publish(IndexUpdate::Files(request.0, files)) {
                break;
            }
        }
    });
    (requests, updates)
}

/// Work to do once the application state is no longer borrowed, because these
/// calls can send further messages to the window.
enum Action {
    None,
    Show,
    Hide,
    /// The result list changed, so resize and redraw.
    Refresh,
    Redraw,
    /// Hide the window, then open the target.
    Open(String),
    Reveal(String),
    OpenSettings,
}

struct App {
    window: HWND,
    config: SearchConfig,
    /// The dock's theme, which the search window shares.
    appearance: Appearance,
    strings: &'static Strings,
    config_path: PathBuf,
    /// When the settings file was last changed, as of reading it.
    config_stamp: Option<SystemTime>,
    history: History,
    renderer: Renderer,
    field: TextField,
    rows: Vec<Row>,
    selected: usize,

    apps: Vec<Entry>,
    files: Vec<Entry>,
    indexer: Sender<(u64, index::Options)>,
    index_updates: Receiver<IndexUpdate>,
    index_options: index::Options,
    /// Numbers the latest request to rebuild.
    index_id: u64,
    indexing: bool,
    indexed_at: Instant,

    icon_requests: Sender<String>,
    icon_results: Receiver<Loaded>,
    icons_requested: HashSet<String>,

    dictionary: Option<Dictionary>,
    definition: Option<Definition>,
    /// The word `definition` was looked up for.
    defined_word: Option<String>,
    panel_scroll: f32,

    content_requests: Sender<contents::Request>,
    content_responses: Receiver<contents::Response>,
    /// The search inside files that `contents` belongs to.
    content_query: Option<(Vec<String>, usize)>,
    content_id: u64,
    contents: Contents,

    /// How much fits on the current screen.
    max_rows: usize,
    max_panel: f32,

    /// The shortcut that was actually registered.
    hotkey: Option<Hotkey>,

    scale: f32,
    look: Look,
    caret_visible: bool,
    last_mouse: (i32, i32),
    typing: Typing,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the state. Returns `None` before setup, or when a message
/// arrives while the state is already in use.
fn with_app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Runs the search process until the dock `dock` exits or search is turned off.
pub fn run(dock: u32) -> Result<()> {
    // SAFETY: Standard window creation and message loop on this one thread;
    // the names are static and the handles live as long as the process.
    unsafe {
        // Only one search process at a time. A restarted dock may start the
        // new one before the old one has noticed, so wait for it briefly.
        let mutex = CreateMutexW(None, false, w!("Local\\Muelle.Search"))?;
        let owned = WaitForSingleObject(mutex, 10_000);
        if owned.0 != 0 && owned.0 != 0x80 {
            return Ok(());
        }
        let dock = OpenProcess(PROCESS_SYNCHRONIZE, false, dock)?;

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;

        let config_path = muelle_config::default_path();
        let config = load(&config_path).unwrap_or_default();
        if !config.search.enabled {
            return Ok(());
        }

        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: search_link::SEARCH_WINDOW_CLASS,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(Error::from_thread());
        }
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            search_link::SEARCH_WINDOW_CLASS,
            w!("Muelle Search"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )?;
        install(window, config, config_path)?;
        close_with(dock, window);

        let mut message = MSG::default();
        // GetMessageW returns -1 on error, which must also end the loop.
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

/// Closes `window` when the process behind `dock` exits.
fn close_with(dock: HANDLE, window: HWND) {
    let (dock, window) = (dock.0 as isize, window.0 as isize);
    std::thread::spawn(move || {
        // SAFETY: The handle is never closed, and only a message is posted.
        unsafe {
            WaitForSingleObject(HANDLE(dock as *mut _), INFINITE);
            let target = Some(HWND(window as *mut _));
            let _ = PostMessageW(target, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    });
}

/// The saved configuration; `None` if it cannot be read just now.
fn load(path: &Path) -> Option<Config> {
    match muelle_config::load(path) {
        Ok(saved) => Some(saved.unwrap_or_default()),
        Err(error) => {
            muelle_shell::log::line(&format!("búsqueda: {error}"));
            None
        }
    }
}

/// Creates the state, claims the shortcut and starts indexing.
fn install(window: HWND, config: Config, config_path: PathBuf) -> Result<()> {
    let (indexer, index_updates) = spawn_indexer(window);
    let (icon_requests, icon_results) = icons::spawn(window, WM_APP_ICON);
    let (content_requests, content_responses) = contents::spawn(window, WM_APP_CONTENTS);
    let strings = strings::for_language(config.language);
    let appearance = config.appearance;
    let config = config.search;
    let look = look(&config, appearance);
    let registered = hotkey::register(window, config.shortcut());
    let history_path = config_path.with_file_name("search-history.tsv");
    let mut app = App {
        window,
        index_options: index::Options::new(&config),
        config_stamp: modified(&config_path),
        config,
        appearance,
        strings,
        config_path,
        history: History::load(history_path),
        renderer: Renderer::new()?,
        field: TextField::default(),
        rows: Vec::new(),
        selected: 0,
        apps: index::settings(strings),
        files: Vec::new(),
        indexer,
        index_updates,
        index_id: 0,
        indexing: false,
        indexed_at: Instant::now(),
        icon_requests,
        icon_results,
        icons_requested: HashSet::new(),
        dictionary: Dictionary::installed(),
        definition: None,
        defined_word: None,
        panel_scroll: 0.0,
        content_requests,
        content_responses,
        content_query: None,
        content_id: 0,
        contents: Contents::Pending,
        max_rows: MAX_ROWS,
        max_panel: MAX_PANEL,
        hotkey: registered,
        scale: 1.0,
        look,
        caret_visible: true,
        last_mouse: (0, 0),
        typing: Typing::default(),
    };
    app.rebuild_index(true);
    app.look.translucent = style_window(window, app.look);
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    Ok(())
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// How the window looks: the dock's theme, and the user's accent colour for
/// the selection, as the dock's menus use it. `translucent` says whether the
/// theme asks for glass until `style_window` reports what Windows allows.
fn look(config: &SearchConfig, appearance: Appearance) -> Look {
    let system_light = uses_light_theme();
    let theme = muelle_config::resolve(appearance, system_light, transparency_enabled());
    let opacity = match theme.material {
        Material::Standard => 0.80,
        Material::Frosted => 0.88,
        Material::Clear => 0.55,
        Material::LiquidGlass => 0.66,
    };
    Look {
        width: config.width as f32,
        compact: config.compact,
        headers: config.headers,
        dark: !theme.light,
        translucent: theme.glass,
        opacity,
        accent: accent_color().map_or((10, 132, 255), |[r, g, b]| (r, g, b)),
    }
}

/// Asks Windows for rounded corners, the theme's title colours and, for glass,
/// a blurred backdrop. Returns whether the backdrop is showing (it needs
/// Windows 11 22H2 or later).
fn style_window(window: HWND, look: Look) -> bool {
    unsafe fn set<T>(
        window: HWND,
        attribute: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE,
        value: T,
    ) -> bool {
        // SAFETY: `value` is a plain value of the size given.
        unsafe {
            DwmSetWindowAttribute(
                window,
                attribute,
                &value as *const T as *const _,
                size_of::<T>() as u32,
            )
            .is_ok()
        }
    }
    // SAFETY: Attributes of our own window.
    unsafe {
        set(window, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
        set(window, DWMWA_USE_IMMERSIVE_DARK_MODE, BOOL::from(look.dark));
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let backdrop = if look.translucent {
            DWMSBT_TRANSIENTWINDOW
        } else {
            DWMSBT_NONE
        };
        DwmExtendFrameIntoClientArea(window, &margins).is_ok()
            && set(window, DWMWA_SYSTEMBACKDROP_TYPE, backdrop)
            && look.translucent
    }
}

impl App {
    /// Puts new settings into effect.
    fn apply(&mut self, config: Config) {
        let strings = strings::for_language(config.language);
        let language_changed = !std::ptr::eq(strings, self.strings);
        self.strings = strings;
        self.appearance = config.appearance;
        let shortcut_changed = config.search.shortcut() != self.config.shortcut();
        self.config = config.search;
        if shortcut_changed || self.hotkey.is_none() {
            self.hotkey = hotkey::register(self.window, self.config.shortcut());
        }
        if language_changed {
            // The settings pages are listed under their translated names.
            self.apps.retain(|entry| entry.kind != Kind::Setting);
            self.apps.extend(index::settings(strings));
        }
        let options = index::Options::new(&self.config);
        if options != self.index_options {
            self.index_options = options;
            self.rebuild_index(true);
        }
        self.look = look(&self.config, self.appearance);
        self.forget_lookups();
    }

    /// Makes the next search look everything up and lay it out afresh.
    fn forget_lookups(&mut self) {
        self.content_query = None;
        self.defined_word = None;
        self.definition = None;
        self.renderer
            .set_definition(None, self.look.width, self.strings);
    }

    fn rebuild_index(&mut self, even_if_busy: bool) {
        if even_if_busy || !self.indexing {
            self.index_id += 1;
            let request = (self.index_id, self.index_options.clone());
            self.indexing = self.indexer.send(request).is_ok();
        }
    }

    fn receive_index(&mut self) -> Action {
        while let Ok(update) = self.index_updates.try_recv() {
            match update {
                IndexUpdate::Apps(mut apps) => {
                    apps.extend(index::settings(self.strings));
                    self.apps = apps;
                }
                IndexUpdate::Files(id, files) if id == self.index_id => {
                    self.files = files;
                    self.indexing = false;
                    self.indexed_at = Instant::now();
                }
                // Built with settings that have since changed.
                IndexUpdate::Files(..) => {}
            }
        }
        self.update_results();
        Action::Refresh
    }

    fn receive_contents(&mut self) -> Action {
        let mut changed = false;
        while let Ok(response) = self.content_responses.try_recv() {
            // Answers to earlier searches are out of date.
            if response.id == self.content_id {
                self.contents = match response.files {
                    Some(files) => Contents::Found(files),
                    None => Contents::Unavailable,
                };
                changed = true;
            }
        }
        if !changed {
            return Action::None;
        }
        self.update_results();
        Action::Refresh
    }

    /// Rebuilds the list after new data arrives, keeping the user's place if
    /// they had moved down it.
    fn update_results(&mut self) {
        let selected = self.selected_target();
        self.compose();
        self.selected = self
            .rows
            .iter()
            .position(|r| Some(&r.entry.target) == selected.as_ref())
            .unwrap_or(0);
    }

    fn selected_target(&self) -> Option<String> {
        (self.selected > 0)
            .then(|| self.rows.get(self.selected))
            .flatten()
            .map(|row| row.entry.target.clone())
    }

    /// Searches again after the text changes.
    fn run_search(&mut self) {
        let text = self.field.text();
        let query = Query::parse(&text);
        // Asking for a definition works even when previews are turned off.
        let define = matches!(query, Query::Define(_));
        let wanted = define || self.config.show_definitions;
        self.look_up(query.word().filter(|_| wanted));
        self.search_contents(query.content_search(self.config.show_contents));
        self.compose();
        self.selected = 0;
    }

    fn look_up(&mut self, word: Option<&str>) {
        let word = word.map(str::to_lowercase);
        if word == self.defined_word {
            return;
        }
        self.definition = match (&self.dictionary, &word) {
            (Some(dictionary), Some(word)) => dictionary.lookup(word),
            _ => None,
        };
        self.renderer
            .set_definition(self.definition.as_ref(), self.look.width, self.strings);
        self.defined_word = word;
        self.panel_scroll = 0.0;
    }

    fn search_contents(&mut self, wanted: Option<(Vec<String>, usize)>) {
        if wanted == self.content_query {
            return;
        }
        // While a word is being typed, the previous matches stay up until the
        // new ones arrive, so the window does not flicker.
        let extends = match (&self.content_query, &wanted) {
            (Some(old), Some(new)) => new.0.join(" ").starts_with(&old.0.join(" ")),
            _ => false,
        };
        if !extends {
            self.contents = Contents::Pending;
        }
        self.content_id += 1;
        if let Some((terms, limit)) = &wanted {
            let request = contents::Request {
                id: self.content_id,
                terms: terms.clone(),
                limit: *limit,
                options: self.index_options.clone(),
            };
            if self.content_requests.send(request).is_err() {
                self.contents = Contents::Unavailable;
            }
        }
        self.content_query = wanted;
    }

    /// Builds the result list from everything gathered so far.
    fn compose(&mut self) {
        let extras = Extras {
            definition: self.definition.as_ref(),
            has_dictionary: self.dictionary.is_some(),
            contents: &self.contents,
            max_rows: self.max_rows,
            strings: self.strings,
        };
        let unused = History::default();
        let history = if self.config.learn {
            &self.history
        } else {
            &unused
        };
        self.rows = search::search(
            &self.field.text(),
            self.apps.iter().chain(&self.files),
            history,
            &self.config,
            &extras,
        );
        if self.icons_requested.len() > ICON_CACHE_LIMIT {
            self.icons_requested.clear();
            self.renderer.clear_icons();
        }
        for row in &self.rows {
            let entry = &row.entry;
            if matches!(entry.kind, Kind::App | Kind::Folder | Kind::File)
                && self.icons_requested.insert(entry.target.clone())
            {
                let _ = self.icon_requests.send(entry.target.clone());
            }
        }
    }

    fn receive_icons(&mut self) -> Action {
        while let Ok(loaded) = self.icon_results.try_recv() {
            if let Some(icon) = loaded.icon {
                self.renderer.add_icon(loaded.target, icon);
            }
        }
        Action::Redraw
    }

    /// Chooses where to appear, on the screen picked in the settings.
    fn prepare_show(&mut self) -> RECT {
        let age = Duration::from_secs(u64::from(self.config.refresh_minutes) * 60);
        if self.indexed_at.elapsed() > age {
            self.rebuild_index(false);
        }
        // Windows may have switched between light and dark, or changed its
        // accent colour; the dictionary may have been added since starting.
        self.look = look(&self.config, self.appearance);
        if self.dictionary.is_none() {
            self.dictionary = Dictionary::installed();
        }
        if self.config.keep_query {
            self.field.select_all();
        } else {
            self.field.clear();
        }
        self.caret_visible = true;
        self.typing = Typing::default();

        let mut cursor = POINT::default();
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (mut dpi_x, mut dpi_y) = (96, 96);
        unsafe {
            let _ = GetCursorPos(&mut cursor);
            let monitor = match self.config.monitor {
                SearchMonitor::Mouse => MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST),
                SearchMonitor::Active => {
                    MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST)
                }
                SearchMonitor::Primary => {
                    MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY)
                }
            };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        }
        self.last_mouse = (cursor.x, cursor.y);
        let work = info.rcWork;
        self.scale = dpi_x as f32 / 96.0;
        let screen_width = (work.right - work.left) as f32 / self.scale;
        self.look.width = self.look.width.min(screen_width - 16.0);
        let top = work.top + (work.bottom - work.top) * self.config.position as i32 / 100;
        // Show no more than fits between here and the bottom of the screen.
        let room = (work.bottom - top) as f32 / self.scale - 12.0;
        let height = |rows, headers| render::fixed_height(rows, headers, &self.look);
        let fitting = (room - height(0, 6)) / (height(1, 0) - height(0, 0));
        let chosen = self.config.max_rows as usize;
        self.max_rows = (fitting.max(4.0) as usize).min(chosen);
        self.max_panel = (room - height(1, 2)).clamp(150.0, MAX_PANEL);
        // Files may have changed since the window was last open.
        self.forget_lookups();
        self.run_search();

        let (width, height) = self.pixel_size();
        let left = work.left + (work.right - work.left - width) / 2;
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    fn panel_height(&self) -> f32 {
        self.renderer.definition_height().min(self.max_panel)
    }

    fn layout(&self) -> Layout {
        Layout::new(&self.rows, self.panel_height(), &self.look)
    }

    fn scroll(&mut self, distance: f32) -> Action {
        let limit = (self.renderer.definition_height() - self.panel_height()).max(0.0);
        self.panel_scroll = (self.panel_scroll + distance).clamp(0.0, limit);
        Action::Redraw
    }

    fn pixel_size(&self) -> (i32, i32) {
        let height = self.layout().height;
        (
            (self.look.width * self.scale).round() as i32,
            (height * self.scale).round() as i32,
        )
    }

    fn paint(&mut self) {
        let panel_height = self.panel_height();
        let view = View {
            strings: self.strings,
            panel_height,
            panel_scroll: self.panel_scroll,
            field: &self.field,
            rows: &self.rows,
            selected: self.selected,
            caret_visible: self.caret_visible,
            indexing: self.indexing,
            look: self.look,
        };
        self.renderer.paint(self.window, self.scale, &view);
    }

    fn activate(&mut self, reveal: bool) -> Action {
        let Some(row) = self.rows.get(self.selected) else {
            return Action::None;
        };
        let entry = row.entry.clone();
        if reveal && !matches!(entry.kind, Kind::File | Kind::Folder) {
            return Action::None;
        }
        match entry.kind {
            Kind::Calculator | Kind::FullDefinition => {
                shell::set_clipboard(self.window, &entry.target);
                self.field.clear();
                return Action::Hide;
            }
            // Show the whole definition.
            Kind::Definition => {
                self.field.clear();
                self.field.insert(&entry.target);
                return self.edited();
            }
            Kind::Notice => return Action::None,
            Kind::Setting if entry.target == index::OWN_SETTINGS => {
                self.field.clear();
                return Action::OpenSettings;
            }
            _ => {}
        }
        if self.config.learn && !matches!(entry.kind, Kind::Url | Kind::Web) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            self.history.record(&self.field.text(), &entry.target, now);
            self.history.save();
        }
        self.field.clear();
        if reveal {
            Action::Reveal(entry.target)
        } else {
            Action::Open(entry.target)
        }
    }

    fn move_selection(&mut self, step: isize) -> Action {
        let count = self.rows.len() as isize;
        if count > 0 {
            self.selected = (self.selected as isize + step).rem_euclid(count) as usize;
        }
        Action::Redraw
    }

    /// Called after any change to the text.
    fn edited(&mut self) -> Action {
        self.run_search();
        Action::Refresh
    }

    fn key(&mut self, key: VIRTUAL_KEY) -> Action {
        let control = input::control_down();
        self.caret_visible = true;
        match key {
            VK_ESCAPE if self.field.is_empty() => Action::Hide,
            VK_ESCAPE => {
                self.field.clear();
                self.edited()
            }
            VK_RETURN => self.activate(control),
            VK_UP => self.move_selection(-1),
            VK_DOWN => self.move_selection(1),
            VK_TAB => self.move_selection(if key_down(VK_SHIFT) { -1 } else { 1 }),
            VK_PRIOR => self.scroll(-self.panel_height() + SCROLL_STEP),
            VK_NEXT => self.scroll(self.panel_height() - SCROLL_STEP),
            VK_OEM_COMMA if control => Action::OpenSettings,
            _ => match input::edit_key(&mut self.field, key, self.window) {
                Edit::Changed => self.edited(),
                Edit::Moved => Action::Redraw,
                Edit::Ignored => Action::None,
            },
        }
    }

    /// Typed text arrives one UTF-16 unit at a time.
    fn character(&mut self, unit: u16) -> Action {
        let Some(text) = self.typing.unit(unit) else {
            return Action::None;
        };
        self.caret_visible = true;
        self.field.insert(&text);
        self.edited()
    }

    /// The row under a mouse message, if any.
    fn row_under(&self, lparam: LPARAM) -> Option<usize> {
        let y = ((lparam.0 >> 16) as i16) as f32 / self.scale;
        self.layout().row_at(y)
    }

    fn mouse_moved(&mut self, lparam: LPARAM) -> Action {
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        // Windows sends moves when the window appears or resizes under a still
        // pointer; those must not override the keyboard selection.
        if (cursor.x, cursor.y) == self.last_mouse {
            return Action::None;
        }
        self.last_mouse = (cursor.x, cursor.y);
        match self.row_under(lparam) {
            Some(row) if row != self.selected => {
                self.selected = row;
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn clicked(&mut self, lparam: LPARAM) -> Action {
        match self.row_under(lparam) {
            Some(row) => {
                self.selected = row;
                // Clicking a definition being read must not close the window.
                if self.rows[row].entry.kind == Kind::FullDefinition {
                    return Action::Redraw;
                }
                self.activate(key_down(VK_CONTROL))
            }
            None => Action::None,
        }
    }
}

fn show(window: HWND) {
    // Pick up edits made to the settings file since it was last read.
    let changed = with_app(|app| modified(&app.config_path) != app.config_stamp);
    if changed == Some(true) {
        reload(window);
    }
    let Some(area) = with_app(App::prepare_show) else {
        return;
    };
    if let Some(look) = with_app(|app| app.look) {
        let translucent = style_window(window, look);
        with_app(|app| app.look.translucent = translucent);
    }
    // SAFETY: Placing, focusing and timing our own window.
    unsafe {
        let _ = SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            area.left,
            area.top,
            area.right - area.left,
            area.bottom - area.top,
            SWP_SHOWWINDOW,
        );
        let _ = SetForegroundWindow(window);
        let _ = SetFocus(Some(window));
        // A blink time of u32::MAX means blinking is turned off.
        let blink = GetCaretBlinkTime();
        if blink > 0 && blink != u32::MAX {
            SetTimer(Some(window), CARET_TIMER, blink, None);
        }
        redraw(window);
    }
}

fn hide(window: HWND) {
    unsafe {
        let _ = KillTimer(Some(window), CARET_TIMER);
        let _ = ShowWindow(window, SW_HIDE);
    }
}

fn redraw(window: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(window), None, false);
        let _ = UpdateWindow(window);
    }
}

/// Reads the settings file again and puts it into effect, closing the process
/// if search was turned off.
fn reload(window: HWND) {
    let Some(path) = with_app(|app| app.config_path.clone()) else {
        return;
    };
    let stamp = modified(&path);
    // On failure, keep the settings already in effect.
    let Some(config) = load(&path) else {
        return;
    };
    if !config.search.enabled {
        // SAFETY: Destroying our own window, with the state not borrowed.
        unsafe {
            let _ = DestroyWindow(window);
        }
        return;
    }
    // The dock also writes this file, for its pinned and recent apps.
    let changed = with_app(|app| {
        app.config_stamp = stamp;
        let same = config.search == app.config
            && config.appearance == app.appearance
            && std::ptr::eq(strings::for_language(config.language), app.strings)
            && app.hotkey.is_some();
        if !same {
            app.apply(config);
        }
        !same
    });
    if changed == Some(true) {
        hide(window);
        if let Some(look) = with_app(|app| app.look) {
            let translucent = style_window(window, look);
            with_app(|app| app.look.translucent = translucent);
        }
    }
}

/// Gives up the shortcut while Settings records a new one, or takes it back.
fn suspend_hotkey(window: HWND, suspend: bool) {
    // SAFETY: Timers on our own window.
    unsafe {
        let _ = KillTimer(Some(window), RESUME_TIMER);
    }
    if suspend {
        hotkey::unregister(window);
        // SAFETY: As above.
        unsafe {
            SetTimer(Some(window), RESUME_TIMER, RESUME_AFTER_MS, None);
        }
    } else {
        // The new shortcut may have been saved while this one was released.
        reload(window);
        with_app(|app| app.hotkey = hotkey::register(window, app.config.shortcut()));
    }
}

/// Lets the program about to be opened come to the front. Must be called while
/// this window still has the focus.
fn hand_over_foreground() {
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

/// Opens Muelle's settings at the search page.
fn open_settings(window: HWND) {
    hand_over_foreground();
    let settings = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name("muelle-settings.exe"))
        .filter(|path| path.is_file());
    if let Some(settings) = settings
        && let Err(error) = std::process::Command::new(settings)
            .args(["--section", "search"])
            .spawn()
    {
        muelle_shell::log::line(&format!("búsqueda: no se pudo abrir Ajustes: {error}"));
    }
    hide(window);
}

fn perform(window: HWND, action: Action) {
    let visible = unsafe { IsWindowVisible(window).as_bool() };
    match action {
        Action::None => {}
        Action::Show => show(window),
        Action::Hide => hide(window),
        Action::Refresh if visible => {
            if let Some((width, height)) = with_app(|app| app.pixel_size()) {
                unsafe {
                    let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
                    let _ = SetWindowPos(window, None, 0, 0, width, height, flags);
                }
                redraw(window);
            }
        }
        Action::Redraw if visible => redraw(window),
        Action::Refresh | Action::Redraw => {}
        Action::Open(target) => {
            hand_over_foreground();
            shell::open(target);
            hide(window);
        }
        Action::Reveal(path) => {
            hand_over_foreground();
            shell::reveal(path);
            hide(window);
        }
        Action::OpenSettings => open_settings(window),
    }
}

/// Handles a message, returning `None` to fall back to the default handling.
fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let action = match message {
        WM_HOTKEY => {
            let open =
                unsafe { IsWindowVisible(window).as_bool() && GetForegroundWindow() == window };
            if open { Action::Hide } else { Action::Show }
        }
        // Requests from the dock and from Settings.
        search_link::SHOW => Action::Show,
        search_link::RELOAD => {
            reload(window);
            return Some(LRESULT(1));
        }
        search_link::SUSPEND_HOTKEY => {
            suspend_hotkey(window, wparam.0 != 0);
            return Some(LRESULT(1));
        }
        search_link::REBUILD_INDEX => {
            with_app(|app| app.rebuild_index(true))?;
            return Some(LRESULT(1));
        }
        search_link::CLEAR_HISTORY => {
            with_app(|app| {
                app.history.clear();
                app.history.save();
            })?;
            return Some(LRESULT(1));
        }
        search_link::STATUS => {
            let hotkey = with_app(|app| app.hotkey)?;
            return Some(LRESULT(search_link::encode_hotkey(hotkey)));
        }
        WM_APP_INDEX => with_app(App::receive_index)?,
        WM_APP_ICON => with_app(App::receive_icons)?,
        WM_APP_CONTENTS => with_app(App::receive_contents)?,
        WM_MOUSEWHEEL => {
            // The high word holds the turn, 120 per notch, positive away.
            let notches = ((wparam.0 >> 16) as i16) as f32 / 120.0;
            with_app(|app| app.scroll(-notches * SCROLL_STEP))?
        }
        // Clicking anywhere else dismisses the window, unless turned off.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if with_app(|app| app.config.close_on_blur)? {
                Action::Hide
            } else {
                Action::None
            }
        }
        WM_PAINT => {
            with_app(App::paint)?;
            unsafe {
                let _ = ValidateRect(Some(window), None);
            }
            Action::None
        }
        WM_ERASEBKGND => return Some(LRESULT(1)),
        // The size is chosen for the target monitor when the window is shown.
        WM_DPICHANGED => Action::None,
        WM_TIMER if wparam.0 == CARET_TIMER => with_app(|app| {
            app.caret_visible = !app.caret_visible;
            Action::Redraw
        })?,
        WM_TIMER if wparam.0 == RESUME_TIMER => {
            suspend_hotkey(window, false);
            Action::None
        }
        WM_KEYDOWN => with_app(|app| app.key(VIRTUAL_KEY(wparam.0 as u16)))?,
        WM_CHAR => with_app(|app| app.character(wparam.0 as u16))?,
        WM_MOUSEMOVE => with_app(|app| app.mouse_moved(lparam))?,
        WM_LBUTTONUP => with_app(|app| app.clicked(lparam))?,
        // Stop Alt combinations from beeping or opening a window menu.
        WM_SYSCHAR => Action::None,
        WM_SYSCOMMAND => match (wparam.0 & 0xFFF0) as u32 {
            SC_KEYMENU => Action::None,
            // Alt+F4 closes the search window, not the whole program.
            SC_CLOSE => Action::Hide,
            _ => return None,
        },
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            Action::None
        }
        WM_DESTROY => {
            // SAFETY: Ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            Action::None
        }
        _ => return None,
    };
    perform(window, action);
    Some(LRESULT(0))
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Windows calls this with the arguments of our own window.
    handle(window, message, wparam, lparam)
        .unwrap_or_else(|| unsafe { DefWindowProcW(window, message, wparam, lparam) })
}
