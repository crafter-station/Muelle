//! Searches inside files using the index that Windows Search already keeps,
//! which understands PDF, Office and plain text documents.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSIDFromProgID, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Search::{
    DBACCESSOR_ROWDATA, DBBINDING, DBMEMOWNER_CLIENTOWNED, DBPARAMIO_NOTPARAM, DBPART_LENGTH,
    DBPART_STATUS, DBPART_VALUE, DBSTATUS_S_OK, DBTYPE_WSTR, HACCESSOR, IAccessor, ICommandText,
    IDBCreateCommand, IDBCreateSession, IDBInitialize, IRowset,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{GUID, HSTRING, IUnknown, Interface, Result, w};

use crate::index::{self, Options};
use crate::search::{Entry, Kind};

/// Waits for a pause in typing before querying.
const PAUSE: Duration = Duration::from_millis(150);
const PATH_UNITS: usize = 1024;
/// Listed ahead of other matches, so code and logs do not bury them.
const DOCUMENTS: [&str; 14] = [
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "txt", "md",
    "csv",
];
const DEFAULT_DIALECT: GUID = GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);

pub struct Request {
    pub id: u64,
    pub terms: Vec<String>,
    pub limit: usize,
    pub options: Options,
}

pub struct Response {
    pub id: u64,
    /// `None` when Windows Search cannot be reached.
    pub files: Option<Vec<Entry>>,
}

/// Starts the search thread. It answers the newest request it has, then posts
/// `message` to `window`.
pub fn spawn(window: HWND, message: u32) -> (Sender<Request>, Receiver<Response>) {
    let (requests, incoming) = channel::<Request>();
    let (outgoing, responses) = channel();
    let window = window.0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut session = None;
        while let Ok(mut request) = incoming.recv() {
            while let Ok(newer) = incoming.recv_timeout(PAUSE) {
                request = newer;
            }
            if session.is_none() {
                session = connect().ok();
            }
            let files = session.as_ref().map(|session| find(session, &request));
            let response = Response {
                id: request.id,
                files,
            };
            if outgoing.send(response).is_err() {
                break;
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0));
            }
        }
    });
    (requests, responses)
}

/// The newest matching documents, then the newest of any other matching files.
fn find(session: &IDBCreateCommand, request: &Request) -> Vec<Entry> {
    let mut found: Vec<Entry> = Vec::new();
    let options = &request.options;
    if options.roots.is_empty() {
        return found;
    }
    for documents_only in [true, false] {
        // Extra rows make up for the ones filtered out.
        let query = sql(
            &request.terms,
            &options.roots,
            documents_only,
            request.limit * 4,
        );
        // A query can fail on words the index ignores, such as "the".
        for url in execute(session, &query).unwrap_or_default() {
            if found.len() == request.limit {
                return found;
            }
            if let Some(file) = entry(&url, options)
                && found.iter().all(|f| f.target != file.target)
            {
                found.push(file);
            }
        }
    }
    found
}

/// The words to look for. Everything else is dropped, so that what the user
/// types can never change the meaning of the query.
pub fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .take(8)
        .map(str::to_lowercase)
        .collect()
}

/// Files under `roots` containing every term (or a word starting with it),
/// newest first.
fn sql(
    terms: &[String],
    roots: &[std::path::PathBuf],
    documents_only: bool,
    limit: usize,
) -> String {
    let scopes: Vec<String> = roots
        .iter()
        .map(|root| {
            let path = root.to_string_lossy().replace('\\', "/");
            format!("SCOPE='file:{}'", path.replace('\'', "''"))
        })
        .collect();
    let words: Vec<String> = terms.iter().map(|t| format!("\"{t}*\"")).collect();
    let kinds = if documents_only {
        let types: Vec<String> = DOCUMENTS
            .iter()
            .map(|extension| format!("System.ItemType='.{extension}'"))
            .collect();
        format!("({})", types.join(" OR "))
    } else {
        "System.ItemType <> 'Directory'".to_string()
    };
    format!(
        "SELECT TOP {limit} System.ItemUrl FROM SystemIndex WHERE ({}) \
         AND {kinds} \
         AND CONTAINS(System.Search.Contents, '{}') \
         ORDER BY System.DateModified DESC",
        scopes.join(" OR "),
        words.join(" AND "),
    )
}

/// Turns a `file:C:/...` address into a result, skipping tool folders,
/// excluded folders and, unless wanted, hidden ones.
fn entry(url: &str, options: &Options) -> Option<Entry> {
    let path = url.strip_prefix("file:")?.replace('/', "\\");
    let (folders, name) = path.rsplit_once('\\')?;
    let hidden = |part: &str| !options.hidden && part.starts_with('.');
    let buried = folders
        .split('\\')
        .any(|part| hidden(part) || index::is_noise(part));
    if buried || hidden(name) || name.starts_with("~$") || options.excludes(&path) {
        return None;
    }
    Some(Entry::new(Kind::File, name, path.as_str()))
}

fn connect() -> Result<IDBCreateCommand> {
    unsafe {
        let class = CLSIDFromProgID(w!("Search.CollatorDSO"))?;
        let source: IDBInitialize = CoCreateInstance(&class, None, CLSCTX_INPROC_SERVER)?;
        source.Initialize()?;
        let sessions: IDBCreateSession = source.cast()?;
        sessions
            .CreateSession(None::<&IUnknown>, &IDBCreateCommand::IID)?
            .cast()
    }
}

/// One result row as laid out by the accessor.
#[repr(C)]
struct RowData {
    status: u32,
    /// Length of `value` in bytes.
    length: usize,
    value: [u16; PATH_UNITS],
}

/// Runs a query that selects a single text column.
fn execute(session: &IDBCreateCommand, query: &str) -> Result<Vec<String>> {
    unsafe {
        let command: ICommandText = session
            .CreateCommand(None::<&IUnknown>, &ICommandText::IID)?
            .cast()?;
        command.SetCommandText(&DEFAULT_DIALECT, &HSTRING::from(query))?;
        let mut result = None;
        command.Execute(
            None::<&IUnknown>,
            &IRowset::IID,
            None,
            None,
            Some(&mut result),
        )?;
        let Some(result) = result else {
            return Ok(Vec::new());
        };
        let rows: IRowset = result.cast()?;
        let accessors: IAccessor = rows.cast()?;

        let binding = DBBINDING {
            iOrdinal: 1,
            obStatus: std::mem::offset_of!(RowData, status),
            obLength: std::mem::offset_of!(RowData, length),
            obValue: std::mem::offset_of!(RowData, value),
            dwPart: (DBPART_VALUE.0 | DBPART_LENGTH.0 | DBPART_STATUS.0) as u32,
            dwMemOwner: DBMEMOWNER_CLIENTOWNED.0 as u32,
            eParamIO: DBPARAMIO_NOTPARAM.0 as u32,
            cbMaxLen: PATH_UNITS * 2,
            wType: DBTYPE_WSTR.0 as u16,
            ..Default::default()
        };
        let mut accessor = HACCESSOR::default();
        accessors.CreateAccessor(
            DBACCESSOR_ROWDATA.0 as u32,
            1,
            &binding,
            0,
            &mut accessor,
            None,
        )?;

        let mut values = Vec::new();
        let mut data = Box::new(RowData {
            status: 0,
            length: 0,
            value: [0; PATH_UNITS],
        });
        let outcome = loop {
            let mut handles = [0usize; 16];
            // The binding takes the row count from the length of this slice,
            // and the provider writes handles to where its first item points.
            let mut destinations = [std::ptr::null_mut::<usize>(); 16];
            destinations[0] = handles.as_mut_ptr();
            let mut count = 0;
            if let Err(error) = rows.GetNextRows(0, 0, &mut count, &mut destinations) {
                break Err(error);
            }
            if count == 0 {
                break Ok(());
            }
            for &handle in &handles[..count.min(handles.len())] {
                let pointer = &mut *data as *mut RowData;
                if rows.GetData(handle, accessor, pointer as *mut _).is_ok()
                    && data.status == DBSTATUS_S_OK.0 as u32
                {
                    let units = (data.length / 2).min(PATH_UNITS);
                    values.push(String::from_utf16_lossy(&data.value[..units]));
                }
            }
            let released = rows.ReleaseRows(
                count,
                handles.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if let Err(error) = released {
                break Err(error);
            }
        };
        let _ = accessors.ReleaseAccessor(accessor, None);
        outcome.map(|()| values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muelle_config::SearchConfig;
    use std::path::PathBuf;

    #[test]
    fn typed_text_cannot_escape_the_query() {
        let words = terms("x') OR 1=1 -- \"Budget\" report; a");
        assert_eq!(words, ["or", "budget", "report"]);
        let roots = [PathBuf::from("C:\\Users\\O'Neil\\Documents")];
        let query = sql(&words, &roots, false, 5);
        assert!(sql(&words, &roots, true, 5).contains("System.ItemType='.pdf'"));
        assert!(query.contains("SCOPE='file:C:/Users/O''Neil/Documents'"));
        assert!(query.contains("'\"or*\" AND \"budget*\" AND \"report*\"'"));
    }

    #[test]
    fn converts_addresses_to_files() {
        let excluded = [PathBuf::from("C:\\p\\old")];
        let options = Options::from_parts(Vec::new(), &excluded, false);
        let entry = |url| entry(url, &options);
        assert!(entry("file:C:/p/old/x.txt").is_none());
        let found = entry("file:C:/Users/me/Documents/My Notes.txt").unwrap();
        assert_eq!(found.name, "My Notes.txt");
        assert_eq!(found.target, "C:\\Users\\me\\Documents\\My Notes.txt");
        assert!(entry("file:C:/p/node_modules/x.js").is_none());
        assert!(entry("file:C:/p/.git/config").is_none());
        assert!(entry("mapi://mailbox/item").is_none());
    }

    /// Queries the real Windows index on this PC.
    #[test]
    #[ignore]
    fn real_windows_search() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let session = connect().unwrap();
        let options = Options::new(&SearchConfig::default());
        let roots = options.roots.clone();
        for text in ["report", "game version", "the", "zzqqxxjj"] {
            let start = std::time::Instant::now();
            let found = execute(&session, &sql(&terms(text), &roots, false, 40));
            let documents = execute(&session, &sql(&terms(text), &roots, true, 40));
            println!("  documents: {:?}", documents.as_ref().map(Vec::len));
            let request = Request {
                id: 0,
                terms: terms(text),
                limit: 10,
                options: options.clone(),
            };
            assert!(find(&session, &request).len() <= 10);
            println!(
                "{text:?}: {:?} files in {:?}",
                found.as_ref().map(Vec::len),
                start.elapsed()
            );
            if text != "the" {
                assert!(found.is_ok());
            }
        }
    }
}
