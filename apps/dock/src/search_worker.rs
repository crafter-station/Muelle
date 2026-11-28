//! The search window runs in a process of its own (`muelle.exe
//! --search-worker`), which the dock starts while search is on. It exits by
//! itself when the dock does.

use std::{
    env, process,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

use muelle_shell::{launcher, search_link};

/// The setting in effect, for the menu bar's button.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Starts the search process as the dock starts, if search is on. A previous
/// dock's process may still be closing; the new one waits for it.
pub fn launch(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        start();
    }
}

/// Starts, updates or closes the search process after the settings change.
/// Never waits for it.
pub fn sync(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        search_link::quit();
    } else if search_link::is_running() {
        search_link::reload();
    } else {
        start();
    }
}

/// The menu bar's search button: the search window, or Windows Search while
/// ours is off or not running.
pub fn open() {
    let enabled = ENABLED.load(Ordering::Relaxed);
    if enabled && search_link::show() {
        return;
    }
    launcher::open_search();
    // If it stopped by itself, have it ready for next time.
    if enabled {
        start();
    }
}

fn start() {
    // A second copy started meanwhile leaves the first in charge.
    let started = env::current_exe().and_then(|exe| {
        Command::new(exe)
            .args([muelle_search::FLAG, &process::id().to_string()])
            .spawn()
    });
    if let Err(error) = started {
        muelle_shell::log::line(&format!("no se pudo iniciar la búsqueda: {error}"));
    }
}
