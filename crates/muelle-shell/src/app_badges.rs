//! The badges apps put on their taskbar buttons through Windows'
//! notification platform (issue #74): the unread count of Outlook and other
//! web apps installed from Edge, Telegram (Unigram), WhatsApp... Unlike the
//! overlay icons of Teams or Slack (see `taskband`), these never reach a
//! window of ours: an app sends them to Windows as `<badge value="7"/>`, even
//! while it is closed, and Windows keeps them in its notification database.
//!
//! Windows has no API to read another app's badge, so the database is read
//! as the taskbar's own data: read-only, with the SQLite library Windows
//! ships (`winsqlite3.dll`), only the badge rows (never notification text).
//! A background thread asks once a second whether anything was written
//! (`PRAGMA data_version`, a few microseconds) and reads the badges again
//! only then. If the database cannot be read (another Windows version, a
//! schema change), there are simply no such badges.

use std::{
    collections::HashMap,
    ffi::{CString, c_char, c_int, c_void},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, LPARAM, WPARAM},
        System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
        UI::WindowsAndMessaging::PostMessageW,
    },
    core::{PCSTR, w},
};

/// How often the thread asks whether the database changed.
const CHECK_EVERY: Duration = Duration::from_secs(1);

/// What an app's badge shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// An unread count, more than zero.
    Count(u32),
    /// A glyph without a number (new message, alert...): shown as a dot.
    Dot,
}

impl Badge {
    /// The text in the badge: the count, or `99+`; nothing for a dot.
    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Count(count) if count > 99 => "99+".to_owned(),
            Self::Count(count) => count.to_string(),
            Self::Dot => String::new(),
        }
    }
}

/// Reads a badge payload (`<badge value="7"/>`). Zero, `none` or no value
/// mean no badge.
#[must_use]
pub fn parse(payload: &str) -> Option<Badge> {
    let start = payload.find("value=")? + "value=".len();
    let quote = payload[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value = &payload[start + 1..];
    let value = &value[..value.find(quote)?];
    match value.trim() {
        "" | "none" => None,
        number if number.bytes().all(|byte| byte.is_ascii_digit()) => {
            // Too long a number still means "many".
            let count = number.parse::<u32>().unwrap_or(u32::MAX);
            (count > 0).then_some(Badge::Count(count))
        }
        _ => Some(Badge::Dot),
    }
}

/// Watches the badges in the background; tells `window` with `message`
/// whenever they change.
pub struct Watch {
    badges: Arc<Mutex<HashMap<String, Badge>>>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    /// `None` when the database cannot be read here.
    #[must_use]
    pub fn start(window: HWND, message: u32) -> Option<Self> {
        let path = database_path()?;
        let badges = Arc::new(Mutex::new(HashMap::new()));
        let shared = Arc::clone(&badges);
        let (stop, stopped) = mpsc::channel();
        let (ready, opened) = mpsc::channel();
        let window = window.0 as isize;
        let thread = thread::Builder::new()
            .name("muelle-badges".into())
            .spawn(move || {
                crate::power::set_thread_efficiency(true);
                let Some(database) = Database::open(&path) else {
                    let _ = ready.send(false);
                    return;
                };
                let _ = ready.send(true);
                let mut version = None;
                loop {
                    let now = database.data_version();
                    if now.is_some() && now != version {
                        version = now;
                        if let Some(read) = database.badges() {
                            let mut current = shared.lock().unwrap_or_else(PoisonError::into_inner);
                            if *current != read {
                                *current = read;
                                drop(current);
                                // SAFETY: Posts a message with no data to the dock's window.
                                unsafe {
                                    let _ = PostMessageW(
                                        Some(HWND(window as *mut _)),
                                        message,
                                        WPARAM(0),
                                        LPARAM(0),
                                    );
                                }
                            }
                        }
                    }
                    match stopped.recv_timeout(CHECK_EVERY) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
            })
            .ok()?;
        if opened.recv().unwrap_or(false) {
            Some(Self {
                badges,
                stop: Some(stop),
                thread: Some(thread),
            })
        } else {
            let _ = thread.join();
            crate::log::line("insignias de Windows: no se pudo leer su base de datos");
            None
        }
    }

    /// Each app's badge, by its AppUserModelID.
    #[must_use]
    pub fn badges(&self) -> HashMap<String, Badge> {
        self.badges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // Ends the wait at once.
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn database_path() -> Option<String> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    Some(format!(
        r"{local}\Microsoft\Windows\Notifications\wpndatabase.db"
    ))
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_OPEN_READONLY: c_int = 0x01;
const SQLITE_OPEN_URI: c_int = 0x40;
const SQLITE_OPEN_NOMUTEX: c_int = 0x8000;

/// What `GetProcAddress` gives, before it is cast to the real signature.
type Symbol = unsafe extern "system" fn() -> isize;
type OpenV2 =
    unsafe extern "system" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
type PrepareV2 = unsafe extern "system" fn(
    *mut c_void,
    *const c_char,
    c_int,
    *mut *mut c_void,
    *mut *const c_char,
) -> c_int;
type Step = unsafe extern "system" fn(*mut c_void) -> c_int;
type ColumnInt64 = unsafe extern "system" fn(*mut c_void, c_int) -> i64;
type ColumnBlob = unsafe extern "system" fn(*mut c_void, c_int) -> *const c_void;
type ColumnBytes = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;
type Finalize = unsafe extern "system" fn(*mut c_void) -> c_int;
type Close = unsafe extern "system" fn(*mut c_void) -> c_int;
type BusyTimeout = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;

/// The few calls of Windows' SQLite this needs.
struct Sqlite {
    open_v2: OpenV2,
    prepare_v2: PrepareV2,
    step: Step,
    column_int64: ColumnInt64,
    column_blob: ColumnBlob,
    column_bytes: ColumnBytes,
    finalize: Finalize,
    close: Close,
    busy_timeout: BusyTimeout,
}

impl Sqlite {
    fn load() -> Option<Self> {
        // SAFETY: Loads a library from System32 only; each symbol is cast to
        // the signature SQLite documents for it.
        unsafe {
            let library: HMODULE =
                LoadLibraryExW(w!("winsqlite3.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let symbol = |name: &[u8]| GetProcAddress(library, PCSTR(name.as_ptr()));
            Some(Self {
                open_v2: std::mem::transmute::<Symbol, OpenV2>(symbol(b"sqlite3_open_v2\0")?),
                prepare_v2: std::mem::transmute::<Symbol, PrepareV2>(symbol(
                    b"sqlite3_prepare_v2\0",
                )?),
                step: std::mem::transmute::<Symbol, Step>(symbol(b"sqlite3_step\0")?),
                column_int64: std::mem::transmute::<Symbol, ColumnInt64>(symbol(
                    b"sqlite3_column_int64\0",
                )?),
                column_blob: std::mem::transmute::<Symbol, ColumnBlob>(symbol(
                    b"sqlite3_column_blob\0",
                )?),
                column_bytes: std::mem::transmute::<Symbol, ColumnBytes>(symbol(
                    b"sqlite3_column_bytes\0",
                )?),
                finalize: std::mem::transmute::<Symbol, Finalize>(symbol(b"sqlite3_finalize\0")?),
                close: std::mem::transmute::<Symbol, Close>(symbol(b"sqlite3_close\0")?),
                busy_timeout: std::mem::transmute::<Symbol, BusyTimeout>(symbol(
                    b"sqlite3_busy_timeout\0",
                )?),
            })
        }
    }
}

/// A read-only connection to the notification database.
struct Database {
    sqlite: Sqlite,
    connection: *mut c_void,
}

impl Database {
    fn open(path: &str) -> Option<Self> {
        let sqlite = Sqlite::load()?;
        let uri = CString::new(format!("file:{}?mode=ro", path.replace('\\', "/"))).ok()?;
        let mut connection = std::ptr::null_mut();
        // SAFETY: The URI is a valid C string; the connection is closed in Drop.
        let result = unsafe {
            (sqlite.open_v2)(
                uri.as_ptr(),
                &mut connection,
                SQLITE_OPEN_READONLY | SQLITE_OPEN_URI | SQLITE_OPEN_NOMUTEX,
                std::ptr::null(),
            )
        };
        let database = Self { sqlite, connection };
        if result != SQLITE_OK || connection.is_null() {
            return None;
        }
        // SAFETY: A live connection. The service may hold a write briefly.
        unsafe { (database.sqlite.busy_timeout)(database.connection, 200) };
        // Only a database with the expected tables is read at all.
        database.badges()?;
        Some(database)
    }

    /// Runs `sql`, calling `row` for each result row.
    fn query(&self, sql: &str, mut row: impl FnMut(&Self, *mut c_void)) -> Option<()> {
        let sql = CString::new(sql).ok()?;
        let mut statement = std::ptr::null_mut();
        // SAFETY: A live connection and a valid statement, finalized below.
        unsafe {
            if (self.sqlite.prepare_v2)(
                self.connection,
                sql.as_ptr(),
                -1,
                &mut statement,
                std::ptr::null_mut(),
            ) != SQLITE_OK
            {
                return None;
            }
            while (self.sqlite.step)(statement) == SQLITE_ROW {
                row(self, statement);
            }
            (self.sqlite.finalize)(statement);
        }
        Some(())
    }

    /// Text or blob column `index` of the current row, as a string.
    fn text(&self, statement: *mut c_void, index: c_int) -> String {
        // SAFETY: The pointer and length come from SQLite for the current
        // row and are read before the next step.
        unsafe {
            let data = (self.sqlite.column_blob)(statement, index);
            let len = (self.sqlite.column_bytes)(statement, index);
            if data.is_null() || len <= 0 {
                return String::new();
            }
            String::from_utf8_lossy(std::slice::from_raw_parts(data.cast::<u8>(), len as usize))
                .into_owned()
        }
    }

    /// Changes whenever another process writes to the database.
    fn data_version(&self) -> Option<i64> {
        let mut version = None;
        self.query("PRAGMA data_version", |database, statement| {
            // SAFETY: Column 0 of the current row.
            version = Some(unsafe { (database.sqlite.column_int64)(statement, 0) });
        })?;
        version
    }

    /// Each app's current badge, by its AppUserModelID.
    fn badges(&self) -> Option<HashMap<String, Badge>> {
        let mut badges = HashMap::new();
        self.query(
            "SELECT h.PrimaryId, n.Payload FROM Notification n \
             JOIN NotificationHandler h ON h.RecordId = n.HandlerId \
             WHERE n.Type = 'badge'",
            |database, statement| {
                let app = database.text(statement, 0);
                if let Some(badge) = parse(&database.text(statement, 1))
                    && !app.is_empty()
                {
                    badges.insert(app, badge);
                }
            },
        )?;
        Some(badges)
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // SAFETY: Our own connection, closed once.
        unsafe { (self.sqlite.close)(self.connection) };
    }
}

// SAFETY: The connection is used only by the thread that owns the Database.
unsafe impl Send for Database {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_payloads_become_counts_dots_or_nothing() {
        assert_eq!(parse(r#"<badge value="7"/>"#), Some(Badge::Count(7)));
        assert_eq!(
            parse(r#"<badge version="1" value='12'/>"#),
            Some(Badge::Count(12))
        );
        assert_eq!(parse(r#"<badge value="newMessage"/>"#), Some(Badge::Dot));
        assert_eq!(parse(r#"<badge value="alert"/>"#), Some(Badge::Dot));
        assert_eq!(parse(r#"<badge value="0"/>"#), None);
        assert_eq!(parse(r#"<badge value="none"/>"#), None);
        assert_eq!(parse("<badge/>"), None);
        assert_eq!(parse(""), None);
        assert_eq!(Badge::Count(4000).text(), "99+");
        assert_eq!(Badge::Count(7).text(), "7");
        assert_eq!(Badge::Dot.text(), "");
    }

    /// Reads this machine's notification database, read-only. Prints only
    /// how many badges there are, never which apps or what they say.
    #[test]
    #[ignore = "reads the local Windows notification database"]
    fn the_notification_database_reads_here() -> Result<(), &'static str> {
        let path = database_path().ok_or("LOCALAPPDATA")?;
        let database = Database::open(&path).ok_or("unreadable database")?;
        assert!(database.data_version().is_some());
        println!(
            "badges: {}",
            database.badges().map_or(0, |badges| badges.len())
        );
        Ok(())
    }
}
