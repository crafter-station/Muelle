//! Matching, ranking and grouping of results. Pure logic, no Windows calls.

use muelle_config::{SearchConfig, SearchKeyword};

use crate::calc;
use crate::contents;
use crate::dictionary::Definition;
use crate::history::History;
use crate::strings::Strings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    Setting,
    Folder,
    File,
    Calculator,
    Url,
    Web,
    /// A one-line definition, which opens the full one.
    Definition,
    /// The full definition of a word.
    FullDefinition,
    /// A message that cannot be opened.
    Notice,
}

/// Something that can be searched for and opened.
#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    /// Path, URI or `shell:` name to open. For the calculator and full
    /// definitions, the text to copy.
    pub target: String,
    /// Replaces the usual second line.
    note: Option<Box<str>>,
    /// Lowercased name.
    lower: String,
    /// Lowercased name with word boundaries (including camel case) as spaces.
    words: String,
}

impl Entry {
    pub fn new(kind: Kind, name: impl Into<String>, target: impl Into<String>) -> Self {
        let name = name.into();
        let lower = name.to_lowercase();
        let words = split_words(&name);
        Self {
            kind,
            name,
            target: target.into(),
            lower,
            words,
            note: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into().into_boxed_str());
        self
    }

    /// The secondary line shown under the name.
    pub fn detail(&self, strings: &Strings) -> String {
        if let Some(note) = &self.note {
            return note.to_string();
        }
        match self.kind {
            Kind::App => strings.application.into(),
            Kind::Setting => strings.system_settings.into(),
            Kind::Folder | Kind::File => match self.target.rfind('\\') {
                Some(i) => self.target[..i].to_string(),
                None => self.target.clone(),
            },
            Kind::Calculator => strings.press_enter_to_copy.into(),
            Kind::Url => strings.open_in_browser.into(),
            Kind::Web => strings.search_the_web.into(),
            Kind::Definition | Kind::FullDefinition | Kind::Notice => String::new(),
        }
    }
}

fn split_words(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            let camel = c.is_uppercase() && previous.is_some_and(char::is_lowercase);
            if camel && !out.ends_with(' ') {
                out.push(' ');
            }
            out.extend(c.to_lowercase());
        }
        previous = Some(c);
    }
    out
}

/// How well `query` (already lowercased and trimmed) matches the entry's name.
fn match_score(query: &str, entry: &Entry) -> Option<i32> {
    let name = entry.lower.as_str();
    if name == query {
        return Some(1000);
    }
    if entry.kind == Kind::File && name.rsplit_once('.').is_some_and(|(stem, _)| stem == query) {
        return Some(950);
    }
    if name.starts_with(query) {
        return Some(800);
    }
    // Ignoring separators: "wifi" finds "Wi-Fi", "7zip" finds "7-Zip".
    let mut letters = entry.words.chars().filter(|&c| c != ' ').peekable();
    if query
        .chars()
        .filter(|c| c.is_alphanumeric())
        .all(|c| letters.next() == Some(c))
        && query
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-')
    {
        return Some(if letters.peek().is_none() { 900 } else { 700 });
    }
    if starts_a_word(&entry.words, query) {
        return Some(650);
    }
    // Initials: "vsc" finds "Visual Studio Code".
    if query.len() >= 2 && initials_start_with(&entry.words, query) {
        return Some(600);
    }
    let mut tokens = query.split_whitespace();
    let first = tokens.next()?;
    if let Some(second) = tokens.next() {
        let all_present = [first, second]
            .into_iter()
            .chain(tokens)
            .all(|t| name.contains(t));
        if all_present {
            return Some(500);
        }
    }
    if name.contains(query) {
        return Some(400);
    }
    // Loose subsequence matching is only useful on the short app list; on
    // thousands of file names it mostly produces noise.
    if matches!(entry.kind, Kind::App | Kind::Setting) && query.chars().count() >= 3 {
        return subsequence_score(query, &entry.words);
    }
    None
}

fn starts_a_word(words: &str, query: &str) -> bool {
    words.starts_with(query)
        || words
            .match_indices(' ')
            .any(|(i, _)| words[i + 1..].starts_with(query))
}

fn initials_start_with(words: &str, query: &str) -> bool {
    let mut initials = words.split(' ').filter_map(|w| w.chars().next());
    query.chars().all(|c| initials.next() == Some(c))
}

fn subsequence_score(query: &str, words: &str) -> Option<i32> {
    let mut wanted = query.chars().filter(|c| !c.is_whitespace()).peekable();
    let first = *wanted.peek()?;
    // The match has to begin at the start of a word.
    let start = words
        .split(' ')
        .scan(0, |offset, word| {
            let at = *offset;
            *offset += word.len() + 1;
            Some((at, word))
        })
        .find(|(_, word)| word.starts_with(first))?
        .0;
    let mut gaps = 0;
    let mut matched_any = false;
    for c in words[start..].chars() {
        match wanted.peek() {
            Some(&w) if w == c => {
                wanted.next();
                matched_any = true;
            }
            Some(_) if matched_any && c != ' ' => gaps += 1,
            _ => {}
        }
    }
    (wanted.peek().is_none() && gaps <= query.len() * 2).then(|| 250 - (gaps as i32 * 8).min(150))
}

fn total_score(query: &str, entry: &Entry, history: &History) -> Option<i32> {
    let mut score = match_score(query, entry)?;
    score += match entry.kind {
        Kind::App => 200,
        Kind::Setting => 80,
        Kind::Folder => 30,
        _ => 0,
    };
    // Prefer short names and files near the top of the tree.
    score -= entry.lower.len().min(60) as i32;
    if matches!(entry.kind, Kind::File | Kind::Folder) {
        score -= entry.target.matches('\\').count().min(20) as i32 * 4;
    }
    if entry.lower.contains("uninstall") && !query.starts_with("unins") {
        score -= 400;
    }
    score += history.boost(query, &entry.target);
    Some(score)
}

/// The most rows that can ever be shown.
pub const MAX_ROWS: usize = 12;

/// The kinds listed under headings of their own, and how many of each first.
const SECTIONS: [(Kind, usize); 4] = [
    (Kind::App, 4),
    (Kind::Setting, 2),
    (Kind::Folder, 3),
    (Kind::File, 5),
];

/// What the typed text asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Query<'a> {
    Empty,
    /// `define word`
    Define(&'a str),
    /// `in:words`, searching only inside files.
    Contents(&'a str),
    General(&'a str),
}

impl<'a> Query<'a> {
    pub fn parse(input: &'a str) -> Self {
        let text = input.trim();
        let after = |prefix: &str| {
            text.get(..prefix.len())
                .filter(|start| start.eq_ignore_ascii_case(prefix))
                .map(|_| text[prefix.len()..].trim())
        };
        if text.is_empty() {
            Query::Empty
        } else if let Some(rest) = after("in:") {
            Query::Contents(rest)
        } else if let Some(word) = after("define ") {
            Query::Define(word)
        } else {
            Query::General(text)
        }
    }

    /// The word to look up in the dictionary, if this query could be one.
    pub fn word(&self) -> Option<&'a str> {
        match *self {
            Query::Define(word) => Some(word),
            Query::General(text) => {
                let plausible = (3..=40).contains(&text.chars().count())
                    && text.split_whitespace().count() <= 3
                    && text
                        .chars()
                        .all(|c| c.is_ascii_alphabetic() || matches!(c, ' ' | '-' | '\''));
                plausible.then_some(text)
            }
            _ => None,
        }
    }

    /// The terms to look for inside files and how many files are wanted.
    /// `inline` says whether ordinary searches include file contents.
    pub fn content_search(&self, inline: bool) -> Option<(Vec<String>, usize)> {
        let (text, minimum) = match *self {
            Query::Contents(text) => (text, 2),
            // Short words match nearly everything, and sums and web addresses
            // are not meant as file searches.
            Query::General(text)
                if inline && calc::evaluate(text).is_none() && as_url(text).is_none() =>
            {
                (text, 3)
            }
            _ => return None,
        };
        let terms = contents::terms(text);
        let length: usize = terms.iter().map(|t| t.chars().count()).sum();
        // Extra results make up for any already listed by name.
        (length >= minimum).then_some((terms, MAX_ROWS))
    }
}

/// Progress of the search inside files.
#[derive(Default)]
pub enum Contents {
    #[default]
    Pending,
    Found(Vec<Entry>),
    Unavailable,
}

/// Results gathered separately from the name index.
pub struct Extras<'a> {
    pub definition: Option<&'a Definition>,
    pub has_dictionary: bool,
    pub contents: &'a Contents,
    pub max_rows: usize,
    pub strings: &'static Strings,
}

/// One line in the result list.
#[derive(Clone, Debug)]
pub struct Row {
    pub section: &'static str,
    pub entry: Entry,
}

fn row(section: &'static str, entry: Entry) -> Row {
    Row { section, entry }
}

fn notice(section: &'static str, title: impl Into<String>, note: &str) -> Row {
    row(section, Entry::new(Kind::Notice, title, "").with_note(note))
}

fn web_row(text: &str, template: &str, strings: &Strings) -> Row {
    let url = template.replace("{query}", &percent_encode(text));
    let title = strings.search_for.replace("{text}", text);
    row(strings.web, Entry::new(Kind::Web, title, url))
}

/// `yt cats` searches the site that `yt` stands for.
fn keyword_row(text: &str, keywords: &[SearchKeyword], strings: &Strings) -> Option<Row> {
    let (word, rest) = text.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    let keyword = keywords
        .iter()
        .find(|k| k.word.eq_ignore_ascii_case(word) && !rest.is_empty())?;
    let url = keyword.url.replace("{query}", &percent_encode(rest));
    // The site first: the typed words may themselves contain "{site}".
    let title = strings
        .search_site_for
        .replace("{site}", keyword.site())
        .replace("{text}", rest);
    Some(row(strings.quick_search, Entry::new(Kind::Web, title, url)))
}

pub fn search<'a>(
    input: &str,
    entries: impl IntoIterator<Item = &'a Entry>,
    history: &History,
    config: &SearchConfig,
    extras: &Extras,
) -> Vec<Row> {
    match Query::parse(input) {
        Query::Empty => Vec::new(),
        Query::Define(word) => definition_results(word, config, extras),
        Query::Contents(text) => content_results(text, extras),
        Query::General(text) => general_results(text, entries, history, config, extras),
    }
}

fn definition_results(word: &str, config: &SearchConfig, extras: &Extras) -> Vec<Row> {
    let strings = extras.strings;
    let first = match extras.definition {
        Some(definition) => row(
            strings.definition_copy,
            Entry::new(Kind::FullDefinition, word, definition.to_text()),
        ),
        None if !extras.has_dictionary => notice(
            strings.definition,
            strings.dictionary_missing,
            strings.dictionary_missing_note,
        ),
        None => notice(
            strings.definition,
            strings.no_definition.replace("{text}", word),
            strings.check_spelling,
        ),
    };
    let mut rows = vec![first];
    if config.show_web_search {
        let query = format!("define {word}");
        rows.push(web_row(&query, config.search_template(), strings));
    }
    rows
}

fn content_results(text: &str, extras: &Extras) -> Vec<Row> {
    let strings = extras.strings;
    let section = strings.inside_files;
    if contents::terms(text).is_empty() {
        return vec![notice(section, strings.type_words, strings.type_words_note)];
    }
    match extras.contents {
        Contents::Found(files) if !files.is_empty() => files
            .iter()
            .take(extras.max_rows)
            .map(|file| row(section, file.clone()))
            .collect(),
        Contents::Found(_) => vec![notice(
            section,
            strings.no_files_contain.replace("{text}", text),
            strings.only_indexed,
        )],
        Contents::Pending => vec![notice(section, strings.searching_inside, "")],
        Contents::Unavailable => vec![notice(
            section,
            strings.search_unavailable,
            strings.search_unavailable_note,
        )],
    }
}

fn general_results<'a>(
    text: &str,
    entries: impl IntoIterator<Item = &'a Entry>,
    history: &History,
    config: &SearchConfig,
    extras: &Extras,
) -> Vec<Row> {
    let query = text.to_lowercase();
    let strings = extras.strings;
    let mut rows = Vec::new();

    if config.keywords_enabled {
        rows.extend(keyword_row(text, &config.keywords, strings));
    }
    if config.show_calculator
        && let Some(value) = calc::evaluate(text)
    {
        let answer = calc::format(value);
        rows.push(row(
            strings.calculator,
            Entry::new(Kind::Calculator, format!("= {answer}"), answer),
        ));
    }
    if config.show_addresses
        && let Some(url) = as_url(text)
    {
        rows.push(row(strings.website, Entry::new(Kind::Url, text, url)));
    }

    let mut matches: Vec<(i32, &Entry)> = entries
        .into_iter()
        .filter(|e| match e.kind {
            Kind::App => config.show_apps,
            // This program's own settings can always be found.
            Kind::Setting => config.show_system_settings || !e.target.starts_with("ms-settings:"),
            Kind::Folder => config.show_folders,
            Kind::File => config.show_files,
            _ => true,
        })
        .filter_map(|e| total_score(&query, e, history).map(|s| (s, e)))
        .collect();
    matches.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.lower.cmp(&b.1.lower))
            .then_with(|| a.1.target.cmp(&b.1.target))
    });

    let definition = extras
        .definition
        .filter(|_| config.show_definitions)
        .map(|found| {
            let target = format!("define {}", found.word);
            let entry = Entry::new(Kind::Definition, found.word.as_str(), target);
            row(strings.definition, entry.with_note(found.summary(strings)))
        });
    let files: &[Entry] = match extras.contents {
        Contents::Found(files) if config.show_contents => files,
        _ => &[],
    };

    // Slots are set aside for the web search, definition and content matches,
    // keeping at least one for the best name match.
    let fixed =
        rows.len() + usize::from(config.show_web_search) + usize::from(definition.is_some());
    let free = extras.max_rows.saturating_sub(fixed);
    let content_slots = files
        .len()
        .min(config.content_results as usize)
        .min(free.saturating_sub(1));
    let mut budget = free - content_slots;

    if budget > 0 && !matches.is_empty() {
        rows.push(row(strings.top_hit, matches.remove(0).1.clone()));
        budget -= 1;
    }

    let groups: Vec<Vec<&Entry>> = SECTIONS
        .iter()
        .map(|&(kind, limit)| {
            matches
                .iter()
                .filter(|(_, e)| e.kind == kind)
                .map(|&(_, e)| e)
                .take(limit)
                .collect()
        })
        .collect();

    // Hand out the remaining slots one per section at a time.
    let mut take = [0usize; SECTIONS.len()];
    while budget > 0 {
        let before = budget;
        for (i, group) in groups.iter().enumerate() {
            if budget > 0 && take[i] < group.len() {
                take[i] += 1;
                budget -= 1;
            }
        }
        if budget == before {
            break;
        }
    }
    for (i, group) in groups.iter().enumerate() {
        let section = match SECTIONS[i].0 {
            Kind::App => strings.applications,
            Kind::Setting => strings.system_settings,
            Kind::Folder => strings.folders,
            _ => strings.documents,
        };
        rows.extend(
            group[..take[i]]
                .iter()
                .map(|&entry| row(section, entry.clone())),
        );
    }

    let inside: Vec<Row> = files
        .iter()
        .filter(|file| {
            rows.iter()
                .all(|r| !r.entry.target.eq_ignore_ascii_case(&file.target))
        })
        .take(content_slots)
        .map(|file| row(strings.inside_files, file.clone()))
        .collect();
    rows.extend(inside);
    rows.extend(definition);
    if config.show_web_search {
        rows.push(web_row(text, config.search_template(), strings));
    }
    rows
}

/// Recognises input such as `github.com/rust-lang` or `https://example.com`.
fn as_url(text: &str) -> Option<String> {
    if text.contains(char::is_whitespace) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return (lower.split("//").nth(1)?.len() > 1).then(|| text.to_string());
    }
    let host = lower.split(['/', '?', '#']).next()?;
    let (name, suffix) = host.rsplit_once('.')?;
    // A short list of suffixes, so file names like `notes.txt` stay searches.
    const SUFFIXES: [&str; 16] = [
        "com", "org", "net", "io", "dev", "app", "edu", "gov", "co", "ai", "uk", "de", "ca", "us",
        "tv", "gg",
    ];
    let valid_host = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    (valid_host && SUFFIXES.contains(&suffix)).then(|| format!("https://{text}"))
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> SearchConfig {
        SearchConfig {
            engine: muelle_config::SearchEngine::Custom,
            custom_search: "https://example.com/?q={query}".into(),
            max_rows: 10,
            ..SearchConfig::default()
        }
    }
    /// Rows available in these tests.
    const ROWS: usize = 10;

    /// Search with nothing from the dictionary or inside files.
    fn search<'a>(
        input: &str,
        entries: impl IntoIterator<Item = &'a Entry>,
        history: &History,
        config: &SearchConfig,
    ) -> Vec<Row> {
        with_extras(input, entries, history, config, None, &Contents::Pending)
    }

    fn with_extras<'a>(
        input: &str,
        entries: impl IntoIterator<Item = &'a Entry>,
        history: &History,
        config: &SearchConfig,
        definition: Option<&Definition>,
        contents: &Contents,
    ) -> Vec<Row> {
        let extras = Extras {
            definition,
            has_dictionary: true,
            contents,
            max_rows: ROWS,
            strings: &crate::strings::ENGLISH,
        };
        super::search(input, entries, history, config, &extras)
    }

    fn definition() -> Definition {
        use crate::dictionary::{Group, Sense};
        Definition {
            word: "report".into(),
            groups: vec![Group {
                part: "noun",
                word: "report".into(),
                senses: vec![Sense {
                    meaning: "an account".into(),
                    examples: Vec::new(),
                    synonyms: Vec::new(),
                }],
            }],
        }
    }

    #[test]
    fn recognises_commands() {
        assert_eq!(Query::parse("  "), Query::Empty);
        assert_eq!(Query::parse("Define  cat "), Query::Define("cat"));
        assert_eq!(Query::parse("IN: tax 2026"), Query::Contents("tax 2026"));
        assert_eq!(Query::parse("define"), Query::General("define"));
        assert_eq!(Query::parse("é"), Query::General("é"));
        assert_eq!(Query::parse("ice cream").word(), Some("ice cream"));
        assert_eq!(Query::parse("v2.txt").word(), None);
        assert!(Query::parse("6*7").content_search(true).is_none());
        assert!(Query::parse("ab").content_search(true).is_none());
        assert!(Query::parse("in:ab").content_search(true).is_some());
        assert!(Query::parse("define cat").content_search(true).is_none());
        assert!(Query::parse("report").content_search(false).is_none());
        assert!(Query::parse("in:report").content_search(false).is_some());
    }

    #[test]
    fn mixes_in_content_matches_and_definitions() {
        let names: Vec<Entry> = (0..30)
            .map(|i| app(&format!("Report app {i}")))
            .chain([file("C:\\docs\\report")])
            .collect();
        let found = Contents::Found(vec![
            file("C:\\DOCS\\report"),
            file("C:\\docs\\a.pdf"),
            file("C:\\docs\\b.pdf"),
            file("C:\\docs\\c.pdf"),
        ]);
        let word = definition();
        let history = History::default();
        let rows = with_extras("report", &names, &history, &settings(), Some(&word), &found);
        assert_eq!(rows.len(), ROWS);
        let sections: Vec<&str> = rows.iter().map(|r| r.section).collect();
        assert_eq!(sections[0], "Top Hit");
        assert_eq!(sections[ROWS - 3..], ["Inside Files", "Definition", "Web"]);
        assert_eq!(
            rows[ROWS - 2].entry.detail(&crate::strings::ENGLISH),
            "(noun) an account"
        );
        // A file listed by name is not repeated.
        let listed = rows
            .iter()
            .filter(|r| r.entry.target.eq_ignore_ascii_case("C:\\docs\\report"))
            .count();
        assert_eq!(listed, 1);
    }

    #[test]
    fn definition_and_content_modes() {
        let none: [Entry; 0] = [];
        let history = History::default();
        let word = definition();
        let pending = Contents::Pending;
        let rows = with_extras(
            "define report",
            &none,
            &history,
            &settings(),
            Some(&word),
            &pending,
        );
        assert_eq!(rows[0].entry.kind, Kind::FullDefinition);
        assert!(rows[0].entry.target.contains("1. an account"));
        let rows = search("define qzx", &none, &history, &settings());
        assert_eq!(rows[0].entry.kind, Kind::Notice);

        let rows = search("in:tax", &none, &history, &settings());
        assert_eq!(rows[0].entry.name, "Searching inside files\u{2026}");
        let found = Contents::Found((0..30).map(|i| file(&format!("C:\\{i}.txt"))).collect());
        let rows = with_extras("in:tax", &none, &history, &settings(), None, &found);
        assert_eq!(rows.len(), ROWS);
        assert!(rows.iter().all(|r| r.entry.kind == Kind::File));
    }

    fn app(name: &str) -> Entry {
        Entry::new(Kind::App, name, format!("shell:AppsFolder\\{name}"))
    }

    fn file(path: &str) -> Entry {
        let name = path.rsplit('\\').next().unwrap();
        Entry::new(Kind::File, name, path)
    }

    fn top(query: &str, entries: &[Entry]) -> String {
        search(query, entries, &History::default(), &settings())[0]
            .entry
            .name
            .clone()
    }

    #[test]
    fn matches_prefixes_words_initials_and_typos() {
        let apps = [
            app("Visual Studio Code"),
            app("OneNote"),
            app("Notepad"),
            app("Calculator"),
        ];
        assert_eq!(top("note", &apps), "Notepad");
        assert_eq!(top("studio", &apps), "Visual Studio Code");
        assert_eq!(top("vsc", &apps), "Visual Studio Code");
        assert_eq!(top("onenote", &apps), "OneNote");
        assert_eq!(top("calcltr", &apps), "Calculator");
        assert_eq!(top("visual code", &apps), "Visual Studio Code");
    }

    #[test]
    fn punctuation_in_names_is_optional() {
        let entries = [
            app("libusb-win32 Filter Wizard"),
            Entry::new(Kind::Setting, "Wi-Fi", "ms-settings:network-wifi"),
            app("7-Zip File Manager"),
        ];
        assert_eq!(top("wifi", &entries), "Wi-Fi");
        assert_eq!(top("7zip", &entries), "7-Zip File Manager");
        let none = search("++", &entries, &History::default(), &settings());
        assert_eq!(none.len(), 1);
    }

    #[test]
    fn apps_outrank_files_and_uninstallers_sink() {
        let entries = [
            file("C:\\Users\\me\\Documents\\steam.txt"),
            app("Uninstall Steam"),
            app("Steam"),
        ];
        let rows = search("steam", &entries, &History::default(), &settings());
        assert_eq!(rows[0].entry.name, "Steam");
        assert_eq!(rows[1].entry.name, "Uninstall Steam");
        assert_eq!(top("uninstall", &entries), "Uninstall Steam");
    }

    #[test]
    fn history_changes_the_top_hit() {
        let apps = [app("Paint"), app("PowerPoint")];
        let mut history = History::default();
        assert_eq!(
            search("p", &apps, &history, &settings())[0].entry.name,
            "Paint"
        );
        history.record("p", &apps[1].target, 1);
        assert_eq!(
            search("p", &apps, &history, &settings())[0].entry.name,
            "PowerPoint"
        );
    }

    #[test]
    fn result_count_is_capped_and_web_search_is_last() {
        let mut entries: Vec<Entry> = (0..30).map(|i| app(&format!("Report app {i}"))).collect();
        entries.extend((0..30).map(|i| file(&format!("C:\\docs\\report {i}.pdf"))));
        let rows = search("report", &entries, &History::default(), &settings());
        assert_eq!(rows.len(), ROWS);
        assert!(rows.iter().any(|r| r.section == "Documents"));
        assert_eq!(rows.last().unwrap().entry.kind, Kind::Web);
    }

    #[test]
    fn calculator_urls_and_empty_input() {
        let none: [Entry; 0] = [];
        let history = History::default();
        assert!(search("   ", &none, &history, &settings()).is_empty());
        assert_eq!(top("6*7", &none), "= 42");
        let rows = search("github.com", &none, &history, &settings());
        assert_eq!(rows[0].entry.target, "https://github.com");
        let rows = search("notes.txt", &none, &history, &settings());
        assert_eq!(rows.len(), 1);
        let rows = search("a b&c", &none, &history, &settings());
        assert_eq!(rows[0].entry.target, "https://example.com/?q=a%20b%26c");
    }

    #[test]
    fn settings_hide_kinds_of_result() {
        let entries = [
            app("Report Maker"),
            file("C:\\report.txt"),
            Entry::new(Kind::Setting, "Report settings", "ms-settings:x"),
            Entry::new(Kind::Setting, "Report window", "muelle:search-settings"),
        ];
        let mut config = settings();
        config.show_apps = false;
        config.show_system_settings = false;
        config.show_web_search = false;
        config.show_definitions = false;
        config.show_contents = false;
        let word = definition();
        let found = Contents::Found(vec![file("C:\\other.pdf")]);
        let history = History::default();
        let rows = with_extras("report", &entries, &history, &config, Some(&word), &found);
        let names: Vec<&str> = rows.iter().map(|r| r.entry.name.as_str()).collect();
        assert_eq!(names, ["report.txt", "Report window"]);

        config.show_calculator = false;
        config.show_addresses = false;
        assert!(search("6*7", &entries, &history, &config).is_empty());
        assert!(search("github.com", &entries, &history, &config).is_empty());
        // Asking for a definition still works.
        let pending = Contents::Pending;
        let rows = with_extras(
            "define report",
            &[],
            &history,
            &config,
            Some(&word),
            &pending,
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn keywords_search_one_site() {
        let history = History::default();
        let mut config = settings();
        let rows = search("YT lofi beats", &[], &history, &config);
        assert_eq!(rows[0].section, "Quick Search");
        assert_eq!(
            rows[0].entry.target,
            "https://www.youtube.com/results?search_query=lofi%20beats"
        );
        assert_eq!(search("yt ", &[], &history, &config).len(), 1);
        config.keywords_enabled = false;
        assert_eq!(search("yt lofi", &[], &history, &config)[0].section, "Web");
    }

    #[test]
    fn handles_non_ascii_names() {
        let apps = [app("Éditeur Été"), app("日本語 アプリ")];
        assert_eq!(top("été", &apps), "Éditeur Été");
        assert_eq!(top("アプリ", &apps), "日本語 アプリ");
    }
}
