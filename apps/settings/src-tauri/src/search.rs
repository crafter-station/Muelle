//! What the Search page asks of the running search process.

use std::{env, fs, io};

use muelle_shell::search_link;
use serde::Serialize;

/// The argument that opens Settings at a section, followed by its name.
pub const SECTION_ARG: &str = "--section";

/// The section asked for on a command line, if it is one the window has.
pub fn section_from(args: impl IntoIterator<Item = String>) -> Option<String> {
    args.into_iter()
        .skip_while(|arg| arg != SECTION_ARG)
        .nth(1)
        .filter(|section| section == "search")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStatus {
    running: bool,
    /// The shortcut in effect, which differs from the one chosen when another
    /// program owns that one. `None` when no shortcut could be claimed.
    hotkey: Option<String>,
    dictionary: bool,
}

#[tauri::command]
pub async fn search_status() -> Result<SearchStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let reply = search_link::send(search_link::STATUS, 0);
        SearchStatus {
            running: reply.is_some(),
            hotkey: reply
                .and_then(search_link::decode_hotkey)
                .map(|hotkey| hotkey.describe()),
            dictionary: search_link::dictionary_installed(),
        }
    })
    .await
    .map_err(|error| error.to_string())
}

/// Releases the search shortcut while a new one is being typed, so that the
/// window receives it; the search process takes it back by itself if this
/// app never asks it to.
#[tauri::command]
pub async fn suspend_search_hotkey(suspend: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Not running means nothing holds the shortcut.
        let _ = search_link::send(search_link::SUSPEND_HOTKEY, usize::from(suspend));
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn rebuild_search_index() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        search_link::send(search_link::REBUILD_INDEX, 0)
            .map(|_| ())
            .ok_or_else(|| "la búsqueda no está en marcha".to_owned())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn clear_search_history() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        // The running process holds the history in memory, so it clears it.
        if search_link::send(search_link::CLEAR_HISTORY, 0).is_some() {
            return Ok(());
        }
        if search_link::is_running() {
            return Err("la búsqueda no responde".to_owned());
        }
        let history = muelle_config::default_path().with_file_name("search-history.tsv");
        match fs::remove_file(history) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The section this app was started to show.
#[tauri::command]
pub fn initial_section() -> Option<String> {
    section_from(env::args())
}

#[cfg(test)]
mod tests {
    use super::section_from;

    #[test]
    fn only_a_known_section_can_be_asked_for() {
        let args = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            section_from(args("app --section search")).as_deref(),
            Some("search")
        );
        for other in ["app", "app --section", "app --section other", "app search"] {
            assert_eq!(section_from(args(other)), None, "{other}");
        }
    }
}
