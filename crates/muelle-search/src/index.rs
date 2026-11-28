//! Builds the searchable list of apps, settings pages, folders and files.

use std::collections::{HashSet, VecDeque};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
    FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Videos, IEnumShellItems, IShellItem,
    KF_FLAG_DEFAULT, SHGetKnownFolderItem, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};

use muelle_config::SearchConfig;

use crate::search::{Entry, Kind};
use crate::shell;
use crate::strings::{PAGES, Strings};

/// Upper bound on indexed files and folders, which keeps typing responsive.
const MAX_FILES: usize = 200_000;
const MAX_DEPTH: usize = 12;
/// Opens the search page of Muelle's settings rather than a file or address.
pub const OWN_SETTINGS: &str = "muelle:search-settings";

/// Windows settings pages, plus the search window's own settings.
pub fn settings(strings: &Strings) -> Vec<Entry> {
    let own = Entry::new(Kind::Setting, strings.own_settings, OWN_SETTINGS)
        .with_note(strings.own_settings_note);
    PAGES
        .iter()
        .zip(strings.pages)
        .map(|(&uri, name)| Entry::new(Kind::Setting, name, uri))
        .chain([own])
        .collect()
}

/// Everything in the Start menu's "All apps" list, including Store apps.
/// The calling thread must have initialised COM.
pub fn apps() -> windows::core::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
        loop {
            let mut batch: [Option<IShellItem>; 1] = [None];
            let mut fetched = 0;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(item) = batch[0].take() else { break };
            let name = item
                .GetDisplayName(SIGDN_NORMALDISPLAY)
                .map(|p| shell::take_string(p));
            let id = item
                .GetDisplayName(SIGDN_PARENTRELATIVEPARSING)
                .map(|p| shell::take_string(p));
            if let (Ok(name), Ok(id)) = (name, id)
                && is_application(&id)
                && !name.is_empty()
            {
                entries.push(Entry::new(
                    Kind::App,
                    name,
                    format!("shell:AppsFolder\\{id}"),
                ));
            }
        }
    }
    Ok(entries)
}

/// The Start menu also lists web links and manuals, which are not apps.
fn is_application(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let document = [".url", ".chm", ".txt", ".htm", ".html", ".pdf", ".rtf"]
        .iter()
        .any(|suffix| id.ends_with(suffix));
    !document && !id.starts_with("http://") && !id.starts_with("https://")
}

/// What to index, taken from the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub roots: Vec<PathBuf>,
    /// Folders to leave out, in the form made by `path_key`.
    excluded: Vec<String>,
    pub hidden: bool,
}

/// Paths compare without regard to case or a trailing separator.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().trim_end_matches('\\').to_lowercase()
}

impl Options {
    /// The chosen standard folders (following any OneDrive redirection) and
    /// any added ones.
    pub fn new(config: &SearchConfig) -> Self {
        let standard = [
            FOLDERID_Desktop,
            FOLDERID_Documents,
            FOLDERID_Downloads,
            FOLDERID_Pictures,
            FOLDERID_Music,
            FOLDERID_Videos,
        ];
        let chosen = config.standard_folders;
        let enabled = [
            chosen.desktop,
            chosen.documents,
            chosen.downloads,
            chosen.pictures,
            chosen.music,
            chosen.videos,
        ];
        let mut roots: Vec<PathBuf> = standard
            .iter()
            .zip(enabled)
            .filter(|(_, enabled)| *enabled)
            .filter_map(|(id, _)| shell::known_folder(id))
            .collect();
        roots.extend(config.folders.iter().map(PathBuf::from));
        let excluded: Vec<PathBuf> = config.excluded_folders.iter().map(PathBuf::from).collect();
        Self::from_parts(roots, &excluded, config.hidden_files)
    }

    pub fn from_parts(roots: Vec<PathBuf>, excluded: &[PathBuf], hidden: bool) -> Self {
        let excluded: Vec<String> = excluded.iter().map(|p| path_key(p)).collect();
        let mut options = Self {
            roots,
            excluded,
            hidden,
        };
        let mut roots = std::mem::take(&mut options.roots);
        roots.retain(|root| !options.excludes(&root.to_string_lossy()));
        options.roots = roots;
        options
    }

    /// Whether `path` is an excluded folder or is inside one.
    pub fn excludes(&self, path: &str) -> bool {
        let path = path.to_lowercase();
        self.excluded.iter().any(|folder| {
            path.strip_prefix(folder.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
        })
    }
}

/// Walks the roots breadth first, so that if the limit is reached it is the
/// most deeply buried files that are left out.
pub fn files(options: &Options) -> Vec<Entry> {
    walk(options, MAX_FILES)
}

fn walk(options: &Options, limit: usize) -> Vec<Entry> {
    let mut entries = Vec::new();
    // Guards against nested or repeated roots.
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    for root in &options.roots {
        if root.is_dir() && seen.insert(path_key(root)) {
            if let Some(name) = root.file_name() {
                entries.push(Entry::new(
                    Kind::Folder,
                    name.to_string_lossy(),
                    root.to_string_lossy(),
                ));
            }
            queue.push_back((root.clone(), 0));
        }
    }
    while let Some((directory, depth)) = queue.pop_front() {
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        let cargo_project = directory.join("Cargo.toml").is_file();
        for child in children.flatten() {
            if entries.len() >= limit {
                return entries;
            }
            // On Windows this metadata comes with the directory listing.
            let Ok(metadata) = child.metadata() else {
                continue;
            };
            const HIDDEN: u32 = 0x2;
            const SYSTEM: u32 = 0x4;
            let attributes = metadata.file_attributes();
            if attributes & SYSTEM != 0 {
                continue;
            }
            let name = child.file_name().to_string_lossy().into_owned();
            let hidden = attributes & HIDDEN != 0 || name.starts_with('.');
            if (hidden && !options.hidden) || name.starts_with("~$") {
                continue;
            }
            let path = child.path();
            if options.excludes(&path.to_string_lossy()) {
                continue;
            }
            if metadata.is_dir() {
                if is_noise(&name) || (cargo_project && name == "target") {
                    continue;
                }
                if !seen.insert(path_key(&path)) {
                    continue;
                }
                entries.push(Entry::new(Kind::Folder, name, path.to_string_lossy()));
                // Links and junctions are listed but not followed, to avoid loops.
                if depth + 1 < MAX_DEPTH && !metadata.is_symlink() {
                    queue.push_back((path, depth + 1));
                }
            } else {
                entries.push(Entry::new(Kind::File, name, path.to_string_lossy()));
            }
        }
    }
    entries
}

/// Folders full of generated files that nobody searches for.
pub fn is_noise(name: &str) -> bool {
    [
        ".git",
        "node_modules",
        "__pycache__",
        "venv",
        "site-packages",
        "AppData",
        "$RECYCLE.BIN",
    ]
    .iter()
    .any(|noise| name.eq_ignore_ascii_case(noise))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_a_tree_and_skips_noise() {
        let root = std::env::temp_dir().join(format!("spotlight-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["sub", "node_modules", ".git", "proj\\target"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "a.txt",
            "sub\\b.txt",
            "node_modules\\c.js",
            ".git\\config",
            "proj\\Cargo.toml",
            "proj\\target\\d.exe",
        ] {
            std::fs::write(root.join(file), "x").unwrap();
        }

        let roots = vec![root.clone(), root.join("sub"), root.clone()];
        let options = Options::from_parts(roots.clone(), &[], false);
        let mut names: Vec<String> = walk(&options, 100).into_iter().map(|e| e.name).collect();
        names.sort();
        let root_name = root.file_name().unwrap().to_string_lossy().into_owned();
        let mut expected = vec![
            "Cargo.toml",
            "a.txt",
            "b.txt",
            "proj",
            "sub",
            root_name.as_str(),
        ];
        expected.sort();
        assert_eq!(names, expected);
        assert_eq!(walk(&options, 3).len(), 3);

        // Hidden items can be included, and folders left out.
        let excluded = [root.join("PROJ")];
        let options = Options::from_parts(roots, &excluded, true);
        let names: Vec<String> = walk(&options, 100).into_iter().map(|e| e.name).collect();
        assert!(!names.iter().any(|n| n == "proj" || n == "Cargo.toml"));
        assert!(!names.iter().any(|n| n == "config" || n == "c.js"));
        assert!(options.excludes(&excluded[0].join("x\\y").to_string_lossy()));
        assert!(!options.excludes(&root.join("project").to_string_lossy()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn filters_start_menu_documents() {
        assert!(is_application(
            "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
        ));
        assert!(!is_application("{GUID}\\Tool\\Manual.chm"));
        assert!(!is_application("https://example.com/"));
    }

    /// Run with `cargo test --release -- --ignored --nocapture` to see how big
    /// the real index on this PC is and how long searching it takes.
    #[test]
    #[ignore]
    fn real_index_timing() {
        use std::time::Instant;
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        let start = Instant::now();
        let mut entries = apps().unwrap();
        println!("apps: {} in {:?}", entries.len(), start.elapsed());
        let start = Instant::now();
        let found = files(&Options::new(&SearchConfig::default()));
        println!("files: {} in {:?}", found.len(), start.elapsed());
        entries.extend(found);
        entries.extend(settings(&crate::strings::ENGLISH));
        let config = SearchConfig::default();
        for query in ["n", "calc", "report", "settings", "zzzzqqq"] {
            let start = Instant::now();
            let extras = crate::search::Extras {
                definition: None,
                has_dictionary: false,
                contents: &Default::default(),
                max_rows: crate::search::MAX_ROWS,
                strings: &crate::strings::ENGLISH,
            };

            let history = Default::default();
            let rows = crate::search::search(query, &entries, &history, &config, &extras);
            println!(
                "{query:?}: {} rows, top = {:?}, {:?}",
                rows.len(),
                rows[0].section,
                start.elapsed()
            );
        }
    }
}
