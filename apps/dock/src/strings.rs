//! User-facing dock text. The language follows
//! the Windows display language unless the user overrides it in Settings.
//! `&` marks the menu access key.

use std::sync::atomic::{AtomicU8, Ordering};

use muelle_config::Language;
use muelle_shell::system::{UiLanguage, ui_language};

mod ar;
mod bn;
mod en;
mod es;
mod fr;
mod hi;
mod pt;
mod zh;

pub struct Strings {
    pub open: &'static str,
    pub new_window: &'static str,
    pub keep_in_dock: &'static str,
    pub show_in_explorer: &'static str,
    pub quit_app: &'static str,
    pub end_task: &'static str,
    pub end_task_failed: &'static str,
    pub empty_bin: &'static str,
    pub turn_hiding_on: &'static str,
    pub turn_hiding_off: &'static str,
    pub quit_dock: &'static str,
    pub start: &'static str,
    pub remove: &'static str,
    pub close_window: &'static str,
    pub ungroup: &'static str,
    pub settings: &'static str,
    pub run_as_admin: &'static str,
    pub show_desktop: &'static str,
    pub task_manager: &'static str,
    pub taskbar_settings: &'static str,
    pub usage_title: &'static str,
    pub usage_refresh: &'static str,
    pub usage_refreshing: &'static str,
    pub usage_sign_in: &'static str,
    pub usage_claude_sign_in: &'static str,
    pub usage_unavailable: &'static str,
    pub usage_loading: &'static str,
    pub usage_stale: &'static str,
    pub usage_resets: &'static str,
    pub usage_updated: &'static str,
    pub usage_used: &'static str,
    pub media_title: &'static str,
    pub media_no_output: &'static str,
    pub media_no_progress: &'static str,
    pub folder_empty: &'static str,
    pub folder_unavailable: &'static str,
    pub change_folder: &'static str,
    pub remove_from_dock: &'static str,
    pub shell_operation_failed: &'static str,
    pub keyboard_settings: &'static str,
    pub keyboard: &'static str,
    pub jump_pinned: &'static str,
    pub jump_frequent: &'static str,
    pub jump_recent: &'static str,
    pub jump_tasks: &'static str,
    pub control_network: &'static str,
    pub control_ethernet: &'static str,
    pub control_off: &'static str,
    pub control_not_connected: &'static str,
    pub control_focus: &'static str,
    pub control_dnd: &'static str,
    pub control_on: &'static str,
    pub tray_windows: &'static str,
    pub control_display: &'static str,
    pub control_sound: &'static str,
    pub control_not_playing: &'static str,
    pub control_more: &'static str,
    pub control_nearby: &'static str,
    pub control_desktops: &'static str,
    pub control_screen: &'static str,
    pub control_appearance: &'static str,
    pub control_screenshot: &'static str,
    pub control_searching: &'static str,
    pub control_no_networks: &'static str,
    pub control_saved_only: &'static str,
    pub control_location_off: &'static str,
    pub control_other_networks: &'static str,
    pub control_wifi_settings: &'static str,
    pub control_no_bluetooth: &'static str,
    pub control_no_devices: &'static str,
    pub control_connected: &'static str,
    pub control_bluetooth_settings: &'static str,
    pub control_sharing_settings: &'static str,
    pub control_focus_session: &'static str,
    pub control_notification_settings: &'static str,
    pub control_focus_settings: &'static str,
    pub control_task_view: &'static str,
    pub control_new_desktop: &'static str,
    pub control_previous_desktop: &'static str,
    pub control_next_desktop: &'static str,
    pub control_show_desktop: &'static str,
    pub control_cast_device: &'static str,
    pub control_this_screen: &'static str,
    pub control_duplicate: &'static str,
    pub control_extend: &'static str,
    pub control_second_screen: &'static str,
    pub control_dark_mode: &'static str,
    pub control_night_light: &'static str,
    pub control_display_settings: &'static str,
    pub control_sound_settings: &'static str,
    pub control_light: &'static str,
    pub control_dark: &'static str,
    pub control_personalize: &'static str,
    pub control_region: &'static str,
    pub control_full_screen: &'static str,
    pub control_record: &'static str,
    pub control_snipping_tool: &'static str,
    pub calendar_today: &'static str,
    pub calendar_notifications: &'static str,
    pub calendar_open_app: &'static str,
    pub calendar_date_settings: &'static str,
    pub system_about: &'static str,
    pub system_windows_settings: &'static str,
    pub system_muelle_settings: &'static str,
    pub system_store: &'static str,
    pub system_recent: &'static str,
    pub system_no_recent: &'static str,
    pub system_recent_files: &'static str,
    pub system_force_quit_app: &'static str,
    pub system_force_quit: &'static str,
    pub system_no_apps: &'static str,
    pub system_sleep: &'static str,
    pub system_restart: &'static str,
    pub system_shut_down: &'static str,
    pub system_lock: &'static str,
    pub system_sign_out: &'static str,
    pub system_ask_restart: &'static str,
    pub system_ask_shut_down: &'static str,
    pub system_ask_sign_out: &'static str,
    pub system_ask_force_quit: &'static str,
}

/// 0 = not chosen yet (follow Windows). Nonzero values identify static catalogs.
static LANGUAGE: AtomicU8 = AtomicU8::new(0);

pub fn set_language(language: Language) {
    let chosen = match language {
        Language::Auto => ui_language(),
        Language::Es => UiLanguage::Spanish,
        Language::En => UiLanguage::English,
        Language::Fr => UiLanguage::French,
        Language::Zh => UiLanguage::Chinese,
        Language::Hi => UiLanguage::Hindi,
        Language::Ar => UiLanguage::Arabic,
        Language::Bn => UiLanguage::Bengali,
        Language::Pt => UiLanguage::Portuguese,
    };
    let stored = match chosen {
        UiLanguage::Spanish => 1,
        UiLanguage::English => 2,
        UiLanguage::French => 3,
        UiLanguage::Chinese => 4,
        UiLanguage::Hindi => 5,
        UiLanguage::Arabic => 6,
        UiLanguage::Bengali => 7,
        UiLanguage::Portuguese => 8,
    };
    LANGUAGE.store(stored, Ordering::Relaxed);
}

fn chosen_language() -> u8 {
    let language = LANGUAGE.load(Ordering::Relaxed);
    if language == 0 {
        set_language(Language::Auto);
        LANGUAGE.load(Ordering::Relaxed)
    } else {
        language
    }
}

pub fn text() -> &'static Strings {
    match chosen_language() {
        1 => &es::TEXT,
        3 => &fr::TEXT,
        4 => &zh::TEXT,
        5 => &hi::TEXT,
        6 => &ar::TEXT,
        7 => &bn::TEXT,
        8 => &pt::TEXT,
        _ => &en::TEXT,
    }
}

/// The language the text is in, never `Auto`. Part of the menu bar's
/// rendered-content key.
pub fn language() -> Language {
    match chosen_language() {
        1 => Language::Es,
        3 => Language::Fr,
        4 => Language::Zh,
        5 => Language::Hi,
        6 => Language::Ar,
        7 => Language::Bn,
        8 => Language::Pt,
        _ => Language::En,
    }
}
