//! Remembers what was opened so that frequent choices rank higher.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default)]
pub struct History {
    /// target -> (times opened, last opened as a Unix timestamp)
    uses: HashMap<String, (u32, u64)>,
    /// query -> the target chosen for it last time
    choices: HashMap<String, String>,
    path: Option<PathBuf>,
}

impl History {
    pub fn load(path: PathBuf) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut history = Self::parse(&text);
        history.path = Some(path);
        history
    }

    fn parse(text: &str) -> Self {
        let mut history = Self::default();
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["U", count, last, target] => {
                    if let (Ok(count), Ok(last)) = (count.parse(), last.parse()) {
                        history.uses.insert(target.to_string(), (count, last));
                    }
                }
                ["C", query, target] => {
                    history
                        .choices
                        .insert(query.to_string(), target.to_string());
                }
                _ => {}
            }
        }
        history
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        for (target, (count, last)) in &self.uses {
            out.push_str(&format!("U\t{count}\t{last}\t{target}\n"));
        }
        for (query, target) in &self.choices {
            out.push_str(&format!("C\t{query}\t{target}\n"));
        }
        out
    }

    /// Extra ranking points for `target` when searching for `query`.
    pub fn boost(&self, query: &str, target: &str) -> i32 {
        let frequency = self
            .uses
            .get(target)
            .map_or(0, |&(count, _)| count.min(8) as i32 * 30);
        let chosen = if self.choices.get(query).is_some_and(|t| t == target) {
            500
        } else {
            0
        };
        frequency + chosen
    }

    pub fn record(&mut self, query: &str, target: &str, now: u64) {
        let clean = |s: &str| s.replace(['\t', '\r', '\n'], " ");
        let target = clean(target);
        let entry = self.uses.entry(target.clone()).or_insert((0, now));
        *entry = (entry.0.saturating_add(1), now);
        let query = clean(query.trim()).to_lowercase();
        if !query.is_empty() {
            self.choices.insert(query, target);
        }
        self.trim();
    }

    /// Drops the least recently used items so the file cannot grow forever.
    fn trim(&mut self) {
        const LIMIT: usize = 500;
        while self.uses.len() > LIMIT {
            let oldest = self
                .uses
                .iter()
                .min_by_key(|(_, (_, last))| *last)
                .map(|(target, _)| target.clone());
            if let Some(oldest) = oldest {
                self.uses.remove(&oldest);
            }
        }
        let uses = &self.uses;
        self.choices.retain(|_, target| uses.contains_key(target));
    }

    pub fn clear(&mut self) {
        self.uses.clear();
        self.choices.clear();
    }

    pub fn save(&self) {
        if let Some(path) = &self.path {
            // Losing history is harmless, so a failed write is ignored.
            let _ = std::fs::write(path, self.serialize());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_text() {
        let mut history = History::default();
        history.record("Code", "C:\\a b\\code.exe", 10);
        history.record("code", "C:\\a b\\code.exe", 20);
        let restored = History::parse(&history.serialize());
        assert_eq!(restored.boost("code", "C:\\a b\\code.exe"), 560);
        assert_eq!(restored.boost("other", "C:\\a b\\code.exe"), 60);
        assert_eq!(restored.boost("code", "missing"), 0);
    }

    #[test]
    fn stays_bounded() {
        let mut history = History::default();
        for i in 0..600u64 {
            history.record(&format!("q{i}"), &format!("t{i}"), i);
        }
        assert_eq!(history.uses.len(), 500);
        assert_eq!(history.choices.len(), 500);
        assert_eq!(history.boost("q0", "t0"), 0);
    }

    #[test]
    fn ignores_malformed_lines() {
        let history = History::parse("junk\nU\tx\ty\tz\nU\t2\t5\tok\n");
        assert_eq!(history.uses.len(), 1);
    }
}
