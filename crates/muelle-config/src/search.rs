//! Settings of the search window, kept under `search` in the shared
//! configuration file. Its appearance and language are the dock's.

use serde::{Deserialize, Serialize};

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// A global shortcut, written in the file as text such as `Alt+Space`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    /// `MOD_*` bits, as `RegisterHotKey` takes them.
    pub modifiers: u32,
    /// A virtual-key code.
    pub key: u32,
}

impl Hotkey {
    pub const DEFAULT: Self = Self {
        modifiers: MOD_ALT,
        key: 0x20,
    };

    /// Accepts a combination that is safe to claim from every other program.
    #[must_use]
    pub fn new(modifiers: u32, key: u32) -> Option<Self> {
        let known_key = matches!(key, 0x20 | 0x30..=0x39 | 0x41..=0x5A | 0x70..=0x7B);
        // A global shortcut without Ctrl, Alt or Win would swallow typing.
        let has_modifier = modifiers & (MOD_ALT | MOD_CONTROL | MOD_WIN) != 0;
        // Alt+F4 must keep closing windows.
        let closes_windows = modifiers == MOD_ALT && key == 0x73;
        (known_key && has_modifier && !closes_windows).then_some(Self { modifiers, key })
    }

    /// Parses text such as `Alt+Space` or `Ctrl+Shift+K`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut modifiers = 0;
        let mut key = None;
        for part in text.split('+') {
            let part = part.trim().to_ascii_lowercase();
            match part.as_str() {
                "alt" => modifiers |= MOD_ALT,
                "ctrl" | "control" => modifiers |= MOD_CONTROL,
                "shift" => modifiers |= MOD_SHIFT,
                "win" => modifiers |= MOD_WIN,
                name => {
                    if key.replace(virtual_key(name)?).is_some() {
                        return None;
                    }
                }
            }
        }
        Self::new(modifiers, key?)
    }

    #[must_use]
    pub fn describe(self) -> String {
        let mut parts: Vec<String> = [
            (MOD_WIN, "Win"),
            (MOD_CONTROL, "Ctrl"),
            (MOD_ALT, "Alt"),
            (MOD_SHIFT, "Shift"),
        ]
        .iter()
        .filter(|(bit, _)| self.modifiers & bit != 0)
        .map(|(_, name)| (*name).to_owned())
        .collect();
        parts.push(match self.key {
            0x20 => "Space".into(),
            k @ 0x70..=0x7B => format!("F{}", k - 0x6F),
            k => char::from_u32(k).map_or_else(|| k.to_string(), String::from),
        });
        parts.join("+")
    }
}

fn virtual_key(name: &str) -> Option<u32> {
    if name == "space" {
        return Some(0x20);
    }
    if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=12).contains(&number).then(|| 0x6F + number);
    }
    let mut chars = name.chars();
    let c = chars.next()?;
    (chars.next().is_none() && c.is_ascii_alphanumeric()).then(|| c.to_ascii_uppercase() as u32)
}

/// Which screen the search window opens on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMonitor {
    Active,
    Primary,
    /// The screen with the pointer. Also what an unknown value from a newer
    /// version reads as (serde needs this fallback last).
    #[default]
    #[serde(other)]
    Mouse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    Bing,
    DuckDuckGo,
    Brave,
    Ecosia,
    Startpage,
    /// The address in `custom_search`.
    Custom,
    /// Also what an unknown value from a newer version reads as (serde needs
    /// this fallback last).
    #[default]
    #[serde(other)]
    Google,
}

const GOOGLE: &str = "https://www.google.com/search?q={query}";

impl SearchEngine {
    fn template(self) -> Option<&'static str> {
        Some(match self {
            Self::Google => GOOGLE,
            Self::Bing => "https://www.bing.com/search?q={query}",
            Self::DuckDuckGo => "https://duckduckgo.com/?q={query}",
            Self::Brave => "https://search.brave.com/search?q={query}",
            Self::Ecosia => "https://www.ecosia.org/search?q={query}",
            Self::Startpage => "https://www.startpage.com/sp/search?query={query}",
            Self::Custom => return None,
        })
    }
}

/// Only web addresses with a place for the search words, so that a search row
/// can never run a program.
#[must_use]
pub fn is_search_address(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.contains("{query}")
        && !url.contains(char::is_whitespace)
}

/// A shortcut such as `yt cats`, which searches one particular site.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchKeyword {
    pub word: String,
    pub url: String,
}

impl SearchKeyword {
    #[must_use]
    pub fn new(word: &str, url: &str) -> Option<Self> {
        let word = word.trim().to_lowercase();
        let url = url.trim();
        let valid = !word.is_empty() && !word.contains(char::is_whitespace) && !word.contains(':');
        (valid && is_search_address(url)).then(|| Self {
            word,
            url: url.to_owned(),
        })
    }

    /// The site's host name, for display.
    #[must_use]
    pub fn site(&self) -> &str {
        let rest = self.url.split("//").nth(1).unwrap_or(&self.url);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        host.strip_prefix("www.").unwrap_or(host)
    }
}

fn default_keywords() -> Vec<SearchKeyword> {
    [
        ("yt", "https://www.youtube.com/results?search_query={query}"),
        ("gh", "https://github.com/search?q={query}"),
        (
            "wiki",
            "https://en.wikipedia.org/wiki/Special:Search?search={query}",
        ),
        ("maps", "https://www.google.com/maps/search/{query}"),
    ]
    .iter()
    .filter_map(|(word, url)| SearchKeyword::new(word, url))
    .collect()
}

/// The standard user folders that are searched by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StandardFolders {
    pub desktop: bool,
    pub documents: bool,
    pub downloads: bool,
    pub pictures: bool,
    pub music: bool,
    pub videos: bool,
}

impl Default for StandardFolders {
    fn default() -> Self {
        Self {
            desktop: true,
            documents: true,
            downloads: true,
            pictures: true,
            music: true,
            videos: true,
        }
    }
}

pub const SEARCH_WIDTH_RANGE: (u32, u32) = (520, 960);
/// Distance from the top of the screen, in percent of its height.
pub const SEARCH_POSITION_RANGE: (u32, u32) = (0, 40);
pub const SEARCH_ROWS_RANGE: (u32, u32) = (4, 12);
pub const SEARCH_CONTENT_RESULTS_RANGE: (u32, u32) = (1, 6);
const REFRESH_RANGE: (u32, u32) = (1, 120);

/// The search window's settings. Missing fields use their default values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct SearchConfig {
    /// Off, no search process runs, the shortcut is free and the menu bar's
    /// search button opens Windows Search.
    pub enabled: bool,
    pub hotkey: String,
    pub monitor: SearchMonitor,
    /// Whether the last search is still there when reopened.
    pub keep_query: bool,
    pub close_on_blur: bool,

    pub width: u32,
    pub position: u32,
    pub max_rows: u32,
    /// One line per result.
    pub compact: bool,
    /// Whether sections have headings.
    pub headers: bool,

    pub show_apps: bool,
    pub show_system_settings: bool,
    pub show_folders: bool,
    pub show_files: bool,
    pub show_contents: bool,
    pub show_definitions: bool,
    pub show_calculator: bool,
    pub show_addresses: bool,
    pub show_web_search: bool,
    /// Matches inside files shown among ordinary results.
    pub content_results: u32,
    /// Whether things opened often rank higher.
    pub learn: bool,

    pub standard_folders: StandardFolders,
    /// Folders indexed in addition to the standard user folders.
    pub folders: Vec<String>,
    pub excluded_folders: Vec<String>,
    pub hidden_files: bool,
    pub refresh_minutes: u32,

    pub engine: SearchEngine,
    /// Used when `engine` is `Custom`.
    pub custom_search: String,
    pub keywords_enabled: bool,
    pub keywords: Vec<SearchKeyword>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: Hotkey::DEFAULT.describe(),
            monitor: SearchMonitor::Mouse,
            keep_query: true,
            close_on_blur: true,
            width: 680,
            position: 17,
            max_rows: 10,
            compact: false,
            headers: true,
            show_apps: true,
            show_system_settings: true,
            show_folders: true,
            show_files: true,
            show_contents: true,
            show_definitions: true,
            show_calculator: true,
            show_addresses: true,
            show_web_search: true,
            content_results: 3,
            learn: true,
            standard_folders: StandardFolders::default(),
            folders: Vec::new(),
            excluded_folders: Vec::new(),
            hidden_files: false,
            refresh_minutes: 2,
            engine: SearchEngine::Google,
            custom_search: String::new(),
            keywords_enabled: true,
            keywords: default_keywords(),
        }
    }
}

fn tidy_folders(folders: Vec<String>) -> Vec<String> {
    folders
        .into_iter()
        .map(|folder| folder.trim().trim_matches('"').to_owned())
        .filter(|folder| !folder.is_empty())
        .collect()
}

impl SearchConfig {
    /// The shortcut asked for, or the default if the text is not a safe one.
    #[must_use]
    pub fn shortcut(&self) -> Hotkey {
        Hotkey::parse(&self.hotkey).unwrap_or(Hotkey::DEFAULT)
    }

    /// The address used for the "search the web" row.
    #[must_use]
    pub fn search_template(&self) -> &str {
        match self.engine.template() {
            Some(template) => template,
            None if is_search_address(&self.custom_search) => &self.custom_search,
            None => GOOGLE,
        }
    }

    /// Brings out-of-range or unsafe values (a hand-edited file) back.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        let clamp = |value: u32, (low, high): (u32, u32)| value.clamp(low, high);
        self.hotkey = self.shortcut().describe();
        self.width = clamp(self.width, SEARCH_WIDTH_RANGE);
        self.position = clamp(self.position, SEARCH_POSITION_RANGE);
        self.max_rows = clamp(self.max_rows, SEARCH_ROWS_RANGE);
        self.content_results = clamp(self.content_results, SEARCH_CONTENT_RESULTS_RANGE);
        self.refresh_minutes = clamp(self.refresh_minutes, REFRESH_RANGE);
        self.folders = tidy_folders(self.folders);
        self.excluded_folders = tidy_folders(self.excluded_folders);
        if !is_search_address(&self.custom_search) {
            self.custom_search.clear();
        }
        // The last entry for a word wins.
        let mut keywords: Vec<SearchKeyword> = Vec::new();
        for keyword in self
            .keywords
            .iter()
            .filter_map(|k| SearchKeyword::new(&k.word, &k.url))
        {
            match keywords.iter_mut().find(|k| k.word == keyword.word) {
                Some(existing) => *existing = keyword,
                None => keywords.push(keyword),
            }
        }
        self.keywords = keywords;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hotkeys() {
        let hotkey = Hotkey::parse("ctrl + Shift+k");
        assert_eq!(
            hotkey,
            Some(Hotkey {
                modifiers: MOD_CONTROL | MOD_SHIFT,
                key: 'K' as u32
            })
        );
        assert_eq!(
            hotkey.map(Hotkey::describe).as_deref(),
            Some("Ctrl+Shift+K")
        );
        assert_eq!(
            Hotkey::parse("Alt+F2").map(Hotkey::describe).as_deref(),
            Some("Alt+F2")
        );
        for bad in [
            "Space", "Shift+A", "Alt", "Alt+A+B", "Alt+F13", "Alt+??", "Alt+F4",
        ] {
            assert_eq!(Hotkey::parse(bad), None, "{bad}");
        }
        // Keys without a name cannot be recorded.
        assert_eq!(Hotkey::new(MOD_CONTROL, 0xBC), None);
    }

    #[test]
    fn an_older_file_gets_the_defaults_and_choices_round_trip() -> Result<(), serde_json::Error> {
        let legacy: SearchConfig = serde_json::from_str("{}")?;
        assert_eq!(legacy, SearchConfig::default());
        let mut changed = SearchConfig {
            enabled: false,
            engine: SearchEngine::DuckDuckGo,
            monitor: SearchMonitor::Primary,
            folders: vec!["D:\\Work".into()],
            keywords: Vec::new(),
            ..SearchConfig::default()
        };
        changed.standard_folders.music = false;
        let restored: SearchConfig = serde_json::from_str(&serde_json::to_string(&changed)?)?;
        assert_eq!(restored.sanitized(), changed);
        let unknown: SearchConfig =
            serde_json::from_str(r#"{"engine":"future","monitor":"future"}"#)?;
        assert_eq!(unknown.engine, SearchEngine::Google);
        assert_eq!(unknown.monitor, SearchMonitor::Mouse);
        Ok(())
    }

    #[test]
    fn unsafe_or_out_of_range_values_are_replaced() {
        let config = SearchConfig {
            hotkey: "Shift+A".into(),
            custom_search: "calc.exe".into(),
            width: 5,
            max_rows: 999,
            folders: vec!["  ".into(), "\"D:\\Work\"".into()],
            keywords: vec![
                SearchKeyword {
                    word: "run".into(),
                    url: "C:\\x.exe {query}".into(),
                },
                SearchKeyword {
                    word: "YT".into(),
                    url: "https://a.com/{query}".into(),
                },
                SearchKeyword {
                    word: "yt".into(),
                    url: "https://www.youtube.com/?q={query}".into(),
                },
            ],
            ..SearchConfig::default()
        }
        .sanitized();
        assert_eq!(config.hotkey, "Alt+Space");
        assert_eq!(config.custom_search, "");
        assert_eq!((config.width, config.max_rows), (520, 12));
        assert_eq!(config.folders, ["D:\\Work"]);
        assert_eq!(config.keywords.len(), 1);
        assert_eq!(config.keywords[0].site(), "youtube.com");
        assert!(SearchKeyword::new("two words", "https://a.com/{query}").is_none());
        assert!(SearchKeyword::new("in:", "https://a.com/{query}").is_none());
    }
}
