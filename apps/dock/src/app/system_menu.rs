//! The menu under the menu bar's circle, as macOS's Apple menu: this PC and
//! its settings, recent apps, Force Quit, and sleeping, restarting, shutting
//! down, locking and signing out.
//!
//! Whatever can lose unsaved work asks first, with "No" selected.

use std::{
    collections::HashSet,
    env,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use anyhow::Result;
use muelle_shell::{
    browser_profiles,
    end_task::PreparedEndTask,
    icon_cache::IconCache,
    icons::IconImage,
    session, system,
    tracker::{self, TrackedWindow},
};
use windows::{
    Win32::{
        Foundation::{HWND, POINT, RECT},
        UI::WindowsAndMessaging::{
            IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_SYSTEMMODAL, MB_YESNO,
            MessageBoxW,
        },
    },
    core::{HSTRING, w},
};

use crate::{
    assets::fallback_name,
    gfx::Gfx,
    menu::{self, MenuItem},
    shell_work,
    strings::text,
};

use super::{App, MENU_TITLE_CHARS, Outside, preferences::explorer_id, shorten};

const ABOUT: u32 = 1;
const WINDOWS_SETTINGS: u32 = 2;
const MUELLE_SETTINGS: u32 = 3;
const STORE: u32 = 4;
const RECENT: u32 = 5;
const FORCE_FRONT: u32 = 6;
const FORCE: u32 = 7;
const TASK_MANAGER: u32 = 8;
const SLEEP: u32 = 9;
const RESTART: u32 = 10;
const SHUT_DOWN: u32 = 11;
const LOCK: u32 = 12;
const SIGN_OUT: u32 = 13;
/// In the second menu: the folder of recent files, then one row per app.
const RECENT_FILES: u32 = 99;
const FIRST_APP: u32 = 100;
/// Apps listed in Recent Items.
const RECENT_SHOWN: usize = 10;
/// Icon pixels asked for; the menu scales them to its rows.
const ICON_PIXELS: u32 = 32;
/// Space between the menu bar and the menu.
const GAP: i32 = 6;

/// A question is on screen: another waits for it.
static ASKING: AtomicBool = AtomicBool::new(false);

/// An app with windows open.
#[derive(Debug, PartialEq, Eq)]
struct Running {
    id: String,
    windows: Vec<isize>,
}

/// What the menu needs, read while the app state is at hand.
pub(crate) struct Request {
    gfx: Gfx,
    anchor: POINT,
    dark: bool,
    /// Where the menu springs from; `None` shows it at once.
    spring: Option<POINT>,
    /// Every window, those of other desktops and hidden ones included.
    windows: Vec<TrackedWindow>,
    /// The windows the dock shows: their apps are the ones open.
    shown: Vec<TrackedWindow>,
    /// The window in front.
    front: isize,
    recent: Vec<String>,
}

/// Something that ends what the user is doing, or pauses it.
enum Act {
    Sleep,
    Restart,
    ShutDown,
    Lock,
    SignOut,
    ForceQuit(String, PreparedEndTask),
}

impl Act {
    /// What to ask before doing it; `None` for what loses nothing.
    fn question(&self) -> Option<String> {
        let text = text();
        match self {
            Self::Sleep | Self::Lock => None,
            Self::Restart => Some(text.system_ask_restart.into()),
            Self::ShutDown => Some(text.system_ask_shut_down.into()),
            Self::SignOut => Some(text.system_ask_sign_out.into()),
            Self::ForceQuit(name, _) => Some(text.system_ask_force_quit.replace("{app}", name)),
        }
    }

    fn run(self) {
        let done = match self {
            Self::Sleep => session::sleep(),
            Self::Restart => session::restart(),
            Self::ShutDown => session::shut_down(),
            Self::Lock => session::lock(),
            Self::SignOut => session::sign_out(),
            Self::ForceQuit(_, target) => {
                if let Err(error) = target.terminate() {
                    muelle_shell::log::line(&format!("forzar salida: {error}"));
                    shell_work::notify_end_task_failed();
                }
                return;
            }
        };
        if let Err(error) = done {
            muelle_shell::log::line(&format!("menú del sistema: {error}"));
            shell_work::notify_not_started();
        }
    }

    /// Asks if need be, then acts, on a thread of its own: neither the
    /// question nor sleeping may hold up the dock.
    fn perform(self) {
        let started = thread::Builder::new()
            .name("muelle-system".into())
            .spawn(move || {
                if let Some(question) = self.question() {
                    if ASKING.swap(true, Ordering::SeqCst) {
                        return;
                    }
                    let yes = confirmed(&question);
                    ASKING.store(false, Ordering::SeqCst);
                    if !yes {
                        return;
                    }
                }
                self.run();
            });
        if let Err(error) = started {
            muelle_shell::log::line(&format!("menú del sistema: {error}"));
            shell_work::notify_not_started();
        }
    }
}

fn confirmed(question: &str) -> bool {
    // SAFETY: The strings live through this modal call, on a thread that owns
    // no other window.
    let answer = unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(question),
            w!("Muelle"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND | MB_SYSTEMMODAL,
        )
    };
    answer == IDYES
}

/// The id Force Quit files an app under: every profile of a browser is the
/// same program.
fn program_of(id: &str) -> String {
    if browser_profiles::is_profile(id)
        && let Some(program) = browser_profiles::program(id)
    {
        return tracker::normalize_id(&program);
    }
    id.to_owned()
}

/// The apps with a window in `shown`, each with all of its windows, and all
/// the windows filed under their apps. File Explorer is left out: ending it
/// would take Windows' shell with it.
fn running(
    all: &[TrackedWindow],
    shown: &[TrackedWindow],
    explorer: &str,
) -> (Vec<Running>, Vec<TrackedWindow>) {
    let peers: Vec<TrackedWindow> = all
        .iter()
        .map(|window| TrackedWindow {
            app_id: program_of(&window.app_id),
            ..window.clone()
        })
        .collect();
    // Windows keeps some of its own parts as hidden windows, which look like
    // those of another desktop; an app is open if the dock shows it.
    let open: HashSet<String> = shown
        .iter()
        .map(|window| program_of(&window.app_id))
        .collect();
    let mut apps: Vec<Running> = Vec::new();
    for window in peers
        .iter()
        .filter(|window| window.app_id != explorer && open.contains(&window.app_id))
    {
        match apps.iter_mut().find(|app| app.id == window.app_id) {
            Some(app) => app.windows.push(window.hwnd),
            None => apps.push(Running {
                id: window.app_id.clone(),
                windows: vec![window.hwnd],
            }),
        }
    }
    (apps, peers)
}

/// A label that shows `name` as it is, whatever it holds.
fn label(name: &str) -> String {
    shorten(name, MENU_TITLE_CHARS).replace('&', "&&")
}

fn main_items(front: Option<&str>, user: &str) -> Vec<MenuItem> {
    let text = text();
    let mut items = vec![
        MenuItem::action(ABOUT, text.system_about),
        MenuItem::Separator,
        MenuItem::action(WINDOWS_SETTINGS, text.system_windows_settings),
        MenuItem::action(MUELLE_SETTINGS, text.system_muelle_settings),
        MenuItem::action(STORE, text.system_store),
        MenuItem::Separator,
        MenuItem::action(RECENT, text.system_recent),
        MenuItem::Separator,
    ];
    if let Some(name) = front {
        items.push(MenuItem::action(
            FORCE_FRONT,
            text.system_force_quit_app.replace("{app}", &label(name)),
        ));
    }
    items.extend([
        MenuItem::action(FORCE, text.system_force_quit),
        MenuItem::action(TASK_MANAGER, text.task_manager),
        MenuItem::Separator,
        MenuItem::action(SLEEP, text.system_sleep),
        MenuItem::action(RESTART, text.system_restart),
        MenuItem::action(SHUT_DOWN, text.system_shut_down),
        MenuItem::Separator,
        MenuItem::action(LOCK, text.system_lock),
        MenuItem::action(
            SIGN_OUT,
            text.system_sign_out
                .replace("{user}", &user.replace('&', "&&")),
        ),
    ]);
    items
}

/// The name and icon of the app `id`, as the dock shows them. Read from the
/// icon cache the dock keeps, so this is quick for apps it has seen.
fn describe(id: &str) -> (String, Option<IconImage>) {
    match IconCache::default().get(id, ICON_PIXELS) {
        Ok(visual) => (visual.name, Some(visual.icon)),
        Err(_) => (fallback_name(id), None),
    }
}

fn app_row(index: usize, name: &str, icon: Option<IconImage>, enabled: bool) -> MenuItem {
    MenuItem::Action {
        command: FIRST_APP + index as u32,
        label: label(name),
        checked: false,
        enabled,
        icon,
    }
}

/// The open menu. Returns what is left for the dock to open.
pub(crate) fn show(request: Request, owner: HWND) -> Option<Outside> {
    let Request {
        gfx,
        anchor,
        dark,
        spring,
        windows,
        shown,
        front,
        recent,
    } = request;
    let below = |items: &[MenuItem]| match spring {
        Some(origin) => menu::show_springing(&gfx, owner, items, anchor, dark, origin),
        None => menu::show_below(&gfx, owner, items, anchor, dark),
    };
    let (apps, peers) = running(&windows, &shown, &explorer_id());
    // Held before the menu opens: the app in front is the one meant.
    let mut front_target = apps
        .iter()
        .find(|app| app.windows.contains(&front))
        .and_then(|app| {
            let target = PreparedEndTask::capture(&app.id, &app.windows, &peers)?;
            Some((describe(&app.id).0, target))
        });
    let user = env::var("USERNAME").unwrap_or_default();
    let front_name = front_target.as_ref().map(|(name, _)| name.as_str());
    let open = |id: &str| Some(Outside::Open(id.to_owned()));
    match below(&main_items(front_name, &user))? {
        ABOUT => return open("ms-settings:about"),
        WINDOWS_SETTINGS => return open("ms-settings:"),
        MUELLE_SETTINGS => return Some(Outside::OpenSettings),
        STORE => return open("ms-windows-store:"),
        TASK_MANAGER => return open("taskmgr.exe"),
        RECENT => {
            let text = text();
            let mut items = vec![MenuItem::Header(text.system_recent.replace('…', ""))];
            if recent.is_empty() {
                items.push(disabled(FIRST_APP, text.system_no_recent.into()));
            }
            for (index, id) in recent.iter().enumerate() {
                let (name, icon) = describe(id);
                items.push(app_row(index, &name, icon, true));
            }
            items.push(MenuItem::Separator);
            items.push(MenuItem::action(RECENT_FILES, text.system_recent_files));
            return match below(&items)? {
                RECENT_FILES => open("shell:recent"),
                command => {
                    let id = recent.get(command.checked_sub(FIRST_APP)? as usize)?;
                    open(id)
                }
            };
        }
        FORCE_FRONT => {
            let (name, target) = front_target.take()?;
            Act::ForceQuit(name, target).perform();
        }
        FORCE => {
            let text = text();
            // Captured now, so that each row says whether it can be ended.
            let mut listed: Vec<_> = apps
                .iter()
                .map(|app| {
                    let target = PreparedEndTask::capture(&app.id, &app.windows, &peers);
                    let (name, icon) = describe(&app.id);
                    (name, icon, target)
                })
                .collect();
            listed.sort_by_key(|(name, ..)| name.to_lowercase());
            let mut items = vec![MenuItem::Header(text.system_force_quit.replace('…', ""))];
            if listed.is_empty() {
                items.push(disabled(FIRST_APP, text.system_no_apps.into()));
            }
            for (index, (name, icon, target)) in listed.iter_mut().enumerate() {
                // Elevated and system apps are Task Manager's business.
                items.push(app_row(index, name, icon.take(), target.is_some()));
            }
            let index = below(&items)?.checked_sub(FIRST_APP)? as usize;
            let (name, _, target) = listed.into_iter().nth(index)?;
            let target = target?;
            Act::ForceQuit(name, target).perform();
        }
        SLEEP => Act::Sleep.perform(),
        RESTART => Act::Restart.perform(),
        SHUT_DOWN => Act::ShutDown.perform(),
        LOCK => Act::Lock.perform(),
        SIGN_OUT => Act::SignOut.perform(),
        _ => {}
    }
    None
}

fn disabled(command: u32, label: String) -> MenuItem {
    MenuItem::Action {
        command,
        label,
        checked: false,
        enabled: false,
        icon: None,
    }
}

impl App {
    /// The menu bar's circle, at `button` on screen, was pressed.
    pub(super) fn system_menu(&mut self, button: RECT) -> Result<Option<Outside>> {
        if self.menu_open {
            return Ok(None);
        }
        self.close_control_center();
        self.close_tray_menu();
        self.close_calendar();
        self.close_preview();
        self.close_group_view();
        if let Some(bar) = &mut self.menubar {
            bar.dismiss_island(&self.gfx)?;
            bar.set_system_menu(&self.gfx, true)?;
        }
        let front = tracker::foreground_window();
        // Other desktops too: an app is ended with all of its windows.
        let windows = self.tracker.list_with_workspaces(true);
        let recent = self
            .roster
            .recent()
            .iter()
            .take(RECENT_SHOWN)
            .cloned()
            .collect();
        let spring = self
            .menubar
            .as_ref()
            .and_then(|bar| bar.system_menu_origin())
            .filter(|_| self.config.menu_bar_icon_animation && system::animations_enabled());
        self.menu_open = true;
        self.render()?;
        Ok(Some(Outside::SystemMenu(Request {
            gfx: self.gfx.clone(),
            anchor: POINT {
                x: button.left,
                y: button.bottom + GAP,
            },
            dark: !crate::appearance::menu_bar_light(),
            spring,
            windows,
            shown: self.windows.clone(),
            front,
            recent,
        })))
    }

    pub(super) fn system_menu_closed(&mut self) -> Result<()> {
        if let Some(bar) = &mut self.menubar {
            bar.set_system_menu(&self.gfx, false)?;
        }
        self.after_menu()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(hwnd: isize, app_id: &str) -> TrackedWindow {
        TrackedWindow {
            hwnd,
            app_id: app_id.into(),
            minimized: false,
        }
    }

    #[test]
    fn force_quit_lists_each_open_app_once_with_all_its_windows() {
        let explorer = r"c:\windows\explorer.exe";
        let shown = [
            window(1, r"c:\b\zed.exe"),
            window(2, explorer),
            window(3, r"c:\a\alpha.exe"),
        ];
        // One more window of zed on another desktop, and a hidden part of
        // Windows that no desktop shows.
        let mut all = shown.to_vec();
        all.push(window(4, r"c:\b\zed.exe"));
        all.push(window(5, r"c:\windows\hidden.exe"));
        let (apps, peers) = running(&all, &shown, explorer);
        let listed: Vec<_> = apps
            .iter()
            .map(|app| (app.id.as_str(), app.windows.clone()))
            .collect();
        assert_eq!(
            listed,
            [(r"c:\b\zed.exe", vec![1, 4]), (r"c:\a\alpha.exe", vec![3])]
        );
        // The rest still count against a process shared with them.
        assert_eq!(peers.len(), all.len());
    }

    #[test]
    fn only_what_loses_work_asks_first() {
        assert!(Act::Sleep.question().is_none());
        assert!(Act::Lock.question().is_none());
        for act in [Act::Restart, Act::ShutDown, Act::SignOut] {
            assert!(act.question().is_some_and(|asked| !asked.is_empty()));
        }
    }

    #[test]
    fn the_menu_names_the_app_in_front_only_when_it_can_be_ended() {
        let commands = |front| -> Vec<u32> {
            main_items(front, "Ana")
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Action { command, .. } => Some(*command),
                    _ => None,
                })
                .collect()
        };
        assert!(commands(Some("Notes")).contains(&FORCE_FRONT));
        let without = commands(None);
        assert!(!without.contains(&FORCE_FRONT));
        for wanted in [FORCE, SLEEP, RESTART, SHUT_DOWN, LOCK, SIGN_OUT] {
            assert!(without.contains(&wanted));
        }
        assert_eq!(label("A & B"), "A && B");
    }
}
