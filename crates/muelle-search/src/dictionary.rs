//! Offline word definitions from the WordNet database files.
//!
//! The files are searched directly on disk, so nothing is loaded up front and
//! no word is ever sent anywhere.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use muelle_shell::search_link;

use crate::strings::Strings;

/// Meanings kept for each part of speech.
const MAX_SENSES: usize = 8;
const MAX_SYNONYMS: usize = 6;

/// File suffix and the label shown for it.
const PARTS: [(&str, &str); 4] = [
    ("noun", "noun"),
    ("verb", "verb"),
    ("adj", "adjective"),
    ("adv", "adverb"),
];

pub struct Sense {
    pub meaning: String,
    pub examples: Vec<String>,
    pub synonyms: Vec<String>,
}

/// The meanings of a word as one part of speech.
pub struct Group {
    pub part: &'static str,
    /// The dictionary form, which differs from the query for "ran" or "geese".
    pub word: String,
    pub senses: Vec<Sense>,
}

pub struct Definition {
    /// What was looked up.
    pub word: String,
    pub groups: Vec<Group>,
}

impl Definition {
    /// One line describing the most common meaning.
    pub fn summary(&self, strings: &Strings) -> String {
        let Some(group) = self.groups.first() else {
            return String::new();
        };
        let meaning = group.senses.first().map_or("", |s| s.meaning.as_str());
        let part = part_name(group.part, strings);
        if group.word == self.word {
            format!("({part}) {meaning}")
        } else {
            format!("({part} \u{2013} {}) {meaning}", group.word)
        }
    }

    /// Plain text for the clipboard.
    pub fn to_text(&self) -> String {
        let mut out = self.word.clone();
        for group in &self.groups {
            out.push_str(&format!("\r\n\r\n{} ({})", group.word, group.part));
            for (number, sense) in group.senses.iter().enumerate() {
                out.push_str(&format!("\r\n{}. {}", number + 1, sense.meaning));
            }
        }
        out
    }
}

/// A part of speech, as `Group::part` names it, in the language shown. The
/// meanings themselves are in English.
pub fn part_name(part: &str, strings: &Strings) -> &'static str {
    match part {
        "noun" => strings.noun,
        "verb" => strings.verb,
        "adjective" => strings.adjective,
        _ => strings.adverb,
    }
}

pub struct Dictionary {
    directory: PathBuf,
}

impl Dictionary {
    /// Uses the first of `candidates` that holds the database.
    pub fn open(candidates: impl IntoIterator<Item = PathBuf>) -> Option<Self> {
        candidates
            .into_iter()
            .find(|d| {
                PARTS.iter().all(|(part, _)| {
                    d.join(format!("index.{part}")).is_file()
                        && d.join(format!("data.{part}")).is_file()
                })
            })
            .map(|directory| Self { directory })
    }

    /// The database next to the program, or where `Get-Dictionary.ps1` puts it.
    pub fn installed() -> Option<Self> {
        Self::open(search_link::dictionary_folders())
    }

    pub fn lookup(&self, word: &str) -> Option<Definition> {
        let word = word.split_whitespace().collect::<Vec<_>>().join(" ");
        let word = word.to_lowercase();
        if word.is_empty() {
            return None;
        }
        let key = word.replace(' ', "_");

        // (is another form of the word, how common it is, meanings)
        let mut found: Vec<(bool, u32, Group)> = Vec::new();
        for (part, label) in PARTS {
            // A damaged or unreadable file just means fewer results.
            for entry in self.entries(part, &key).unwrap_or_default() {
                let senses: Vec<Sense> = entry
                    .offsets
                    .iter()
                    .take(MAX_SENSES)
                    .filter_map(|&offset| self.sense(part, offset, &entry.lemma).ok().flatten())
                    .collect();
                if !senses.is_empty() {
                    let group = Group {
                        part: label,
                        word: entry.lemma.replace('_', " "),
                        senses,
                    };
                    found.push((entry.lemma != key, entry.frequency, group));
                }
            }
        }
        // The word as typed comes first, then the most frequently used.
        found.sort_by_key(|(inflected, frequency, _)| (*inflected, u32::MAX - frequency));
        let groups: Vec<Group> = found.into_iter().map(|(_, _, g)| g).collect();
        (!groups.is_empty()).then_some(Definition { word, groups })
    }

    fn reader(&self, name: String) -> io::Result<BufReader<File>> {
        Ok(BufReader::new(File::open(self.directory.join(name))?))
    }

    /// The dictionary forms of `key` that exist as this part of speech.
    fn entries(&self, part: &str, key: &str) -> io::Result<Vec<IndexEntry>> {
        let mut index = self.reader(format!("index.{part}"))?;
        let mut find = |lemma: &str| -> io::Result<Option<IndexEntry>> {
            Ok(find_line(&mut index, lemma)?.and_then(|line| parse_index(&line)))
        };
        let mut entries: Vec<IndexEntry> = find(key)?.into_iter().collect();

        // Irregular forms, such as "geese" or "ran".
        let exceptions = match self.reader(format!("{part}.exc")) {
            Ok(mut file) => find_line(&mut file, key)?,
            Err(_) => None,
        };
        if let Some(line) = exceptions {
            for base in line.split_whitespace().skip(1) {
                if entries.iter().all(|e| e.lemma != base) {
                    entries.extend(find(base)?);
                }
            }
        }

        // Regular endings, such as "dogs" or "jumped".
        if entries.is_empty() {
            for base in base_forms(part, key) {
                if let Some(entry) = find(&base)? {
                    entries.push(entry);
                    break;
                }
            }
        }
        Ok(entries)
    }

    fn sense(&self, part: &str, offset: u64, lemma: &str) -> io::Result<Option<Sense>> {
        let mut data = self.reader(format!("data.{part}"))?;
        data.seek(SeekFrom::Start(offset))?;
        let mut line = String::new();
        data.read_line(&mut line)?;
        Ok(parse_data(&line, lemma))
    }
}

fn base_forms(part: &str, word: &str) -> Vec<String> {
    let rules: &[(&str, &str)] = match part {
        "noun" => &[
            ("s", ""),
            ("ses", "s"),
            ("xes", "x"),
            ("zes", "z"),
            ("ches", "ch"),
            ("shes", "sh"),
            ("men", "man"),
            ("ies", "y"),
        ],
        "verb" => &[
            ("s", ""),
            ("ies", "y"),
            ("es", "e"),
            ("es", ""),
            ("ed", "e"),
            ("ed", ""),
            ("ing", "e"),
            ("ing", ""),
        ],
        "adj" => &[("er", ""), ("est", ""), ("er", "e"), ("est", "e")],
        _ => &[],
    };
    if word.len() < 3 {
        return Vec::new();
    }
    rules
        .iter()
        .filter_map(|(ending, replacement)| {
            let stem = word.strip_suffix(ending)?;
            (!stem.is_empty()).then(|| format!("{stem}{replacement}"))
        })
        .collect()
}

/// Binary search of a file whose lines are sorted by their first word.
fn find_line<R: Read + Seek>(file: &mut BufReader<R>, key: &str) -> io::Result<Option<String>> {
    // Header lines begin with a space, so their first word is empty.
    if key.is_empty() {
        return Ok(None);
    }
    // Any line with the key starts in `low..high`.
    let mut low = 0;
    let mut high = file.seek(SeekFrom::End(0))?;
    let mut line = Vec::new();
    while low < high {
        let middle = low + (high - low) / 2;
        // Find the first line that starts at or after the middle.
        let mut start = middle;
        if middle > 0 {
            file.seek(SeekFrom::Start(middle - 1))?;
            line.clear();
            start = middle - 1 + file.read_until(b'\n', &mut line)? as u64;
        } else {
            file.seek(SeekFrom::Start(0))?;
        }
        if start >= high {
            high = middle;
            continue;
        }
        line.clear();
        let length = file.read_until(b'\n', &mut line)? as u64;
        // A header line's empty first word sorts before every key.
        let first = line
            .split(|&b| b == b' ' || b == b'\r' || b == b'\n')
            .next()
            .unwrap_or_default();
        match first.cmp(key.as_bytes()) {
            std::cmp::Ordering::Less => low = start + length,
            std::cmp::Ordering::Greater => high = middle,
            std::cmp::Ordering::Equal => {
                return Ok(Some(String::from_utf8_lossy(&line).trim_end().to_string()));
            }
        }
    }
    Ok(None)
}

struct IndexEntry {
    lemma: String,
    /// How many of its meanings appear in tagged text; higher is more common.
    frequency: u32,
    offsets: Vec<u64>,
}

/// `lemma pos synset_cnt p_cnt [symbols] sense_cnt tagsense_cnt offsets...`
fn parse_index(line: &str) -> Option<IndexEntry> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let pointers: usize = fields.get(3)?.parse().ok()?;
    let tagged = 5usize.checked_add(pointers)?;
    let frequency = fields.get(tagged)?.parse().ok()?;
    let offsets = fields[tagged + 1..]
        .iter()
        .map(|f| f.parse().ok())
        .collect::<Option<Vec<u64>>>()?;
    Some(IndexEntry {
        lemma: fields[0].to_string(),
        frequency,
        offsets,
    })
}

/// `offset file type word_count (word id)... pointers | definition; "example"`
fn parse_data(line: &str, lemma: &str) -> Option<Sense> {
    let (head, gloss) = line.split_once(" | ")?;
    let fields: Vec<&str> = head.split_whitespace().collect();
    let count = usize::from_str_radix(fields.get(3)?, 16).ok()?;
    let synonyms = fields
        .iter()
        .skip(4)
        .step_by(2)
        .take(count)
        // Adjectives may carry a marker such as "(p)".
        .map(|w| w.split('(').next().unwrap_or(w))
        .filter(|w| !w.eq_ignore_ascii_case(lemma))
        .take(MAX_SYNONYMS)
        .map(|w| w.replace('_', " "))
        .collect();

    let gloss = gloss.trim();
    let (meaning, examples) = match gloss.split_once("; \"") {
        // Splitting on quotes leaves the examples at every other position.
        Some((meaning, rest)) => (
            meaning,
            std::iter::once(rest.split('"').next().unwrap_or_default())
                .chain(rest.split('"').skip(2).step_by(2))
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(String::from)
                .collect(),
        ),
        None => (gloss, Vec::new()),
    };
    Some(Sense {
        meaning: meaning.trim_end_matches(';').to_string(),
        examples,
        synonyms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn binary_search_finds_every_line() {
        let words = ["'hood", "a", "ab", "ab-c", "ab_c", "abc", "b", "zoo"];
        let mut text = String::from("  1 licence text  \n  2 more text  \n");
        for (i, word) in words.iter().enumerate() {
            text.push_str(&format!("{word} {}{i}\n", "x".repeat(i * 13 % 40)));
        }
        let mut file = BufReader::with_capacity(8, Cursor::new(text.into_bytes()));
        for word in words {
            let line = find_line(&mut file, word).unwrap().unwrap();
            assert_eq!(line.split(' ').next(), Some(word));
        }
        for missing in ["", "0", "aa", "abd", "c", "zzz", "licence"] {
            assert_eq!(find_line(&mut file, missing).unwrap(), None, "{missing:?}");
        }
    }

    #[test]
    fn parses_database_lines() {
        let entry = parse_index("dog n 2 3 @ ~ #m 2 1 00000010 00000020  ").unwrap();
        assert_eq!((entry.frequency, entry.offsets), (1, vec![10, 20]));
        assert!(parse_index("broken n 1").is_none());

        let line = "00000010 05 n 02 dog 0 domestic_dog 0 000 | a pet; kept at home; \
                    \"the dog barked\"; \"good dog\"  \n";
        let sense = parse_data(line, "dog").unwrap();
        assert_eq!(sense.meaning, "a pet; kept at home");
        assert_eq!(sense.examples, ["the dog barked", "good dog"]);
        assert_eq!(sense.synonyms, ["domestic dog"]);
    }

    #[test]
    fn suggests_base_forms() {
        assert!(base_forms("noun", "boxes").contains(&"box".to_string()));
        assert!(base_forms("verb", "making").contains(&"make".to_string()));
        assert!(base_forms("noun", "as").is_empty());
    }

    /// Needs the dictionary from `get-dictionary.ps1`.
    #[test]
    #[ignore]
    fn real_dictionary() {
        let local = std::env::var("LOCALAPPDATA").unwrap();
        let dictionary =
            Dictionary::open([PathBuf::from(local).join("Muelle\\dictionary")]).unwrap();
        for word in [
            "serendipity",
            "dogs",
            "ran",
            "geese",
            "ice cream",
            "happiest",
        ] {
            let start = std::time::Instant::now();
            let found = dictionary.lookup(word).expect(word);
            println!(
                "{word}: {} {:?}",
                found.summary(&crate::strings::ENGLISH),
                start.elapsed()
            );
        }
        assert!(dictionary.lookup("qzxvv").is_none());
        assert_eq!(dictionary.lookup("geese").unwrap().groups[0].word, "goose");
        // Every index line must be reachable.
        let text = std::fs::read_to_string(dictionary.directory.join("index.adv")).unwrap();
        let mut file = dictionary.reader("index.adv".into()).unwrap();
        for line in text.lines().filter(|l| !l.starts_with(' ')) {
            let word = line.split(' ').next().unwrap();
            assert!(find_line(&mut file, word).unwrap().is_some(), "{word}");
        }
    }
}
