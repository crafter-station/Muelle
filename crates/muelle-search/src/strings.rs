//! The search window's text, in the language Muelle shows. `{text}`, `{site}`
//! and `{list}` are replaced when the text is used. What the user types to
//! ask for something (`define`, `in:`) is the same in every language.

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

#[cfg(test)]
pub use en::STRINGS as ENGLISH;

/// Windows settings pages that can be searched for, in the order of the names
/// in `Strings::pages`.
pub const PAGES: [&str; 26] = [
    "ms-settings:display",
    "ms-settings:sound",
    "ms-settings:notifications",
    "ms-settings:powersleep",
    "ms-settings:storagesense",
    "ms-settings:bluetooth",
    "ms-settings:printers",
    "ms-settings:mousetouchpad",
    "ms-settings:network-status",
    "ms-settings:network-wifi",
    "ms-settings:network-vpn",
    "ms-settings:personalization-background",
    "ms-settings:colors",
    "ms-settings:lockscreen",
    "ms-settings:taskbar",
    "ms-settings:appsfeatures",
    "ms-settings:defaultapps",
    "ms-settings:startupapps",
    "ms-settings:yourinfo",
    "ms-settings:signinoptions",
    "ms-settings:dateandtime",
    "ms-settings:regionlanguage",
    "ms-settings:easeofaccess-display",
    "ms-settings:privacy",
    "ms-settings:windowsupdate",
    "ms-settings:about",
];

pub struct Strings {
    /// Shown in the empty search field.
    pub placeholder: &'static str,
    pub indexing: &'static str,

    pub top_hit: &'static str,
    pub applications: &'static str,
    pub system_settings: &'static str,
    pub folders: &'static str,
    pub documents: &'static str,
    pub inside_files: &'static str,
    pub definition: &'static str,
    pub definition_copy: &'static str,
    pub calculator: &'static str,
    pub website: &'static str,
    pub web: &'static str,
    pub quick_search: &'static str,

    pub application: &'static str,
    pub press_enter_to_copy: &'static str,
    pub open_in_browser: &'static str,
    pub search_the_web: &'static str,
    pub search_for: &'static str,
    pub search_site_for: &'static str,

    pub dictionary_missing: &'static str,
    pub dictionary_missing_note: &'static str,
    pub no_definition: &'static str,
    pub check_spelling: &'static str,
    pub type_words: &'static str,
    pub type_words_note: &'static str,
    pub no_files_contain: &'static str,
    pub only_indexed: &'static str,
    pub searching_inside: &'static str,
    pub search_unavailable: &'static str,
    pub search_unavailable_note: &'static str,

    /// Words with the same meaning, under a definition.
    pub similar: &'static str,
    pub noun: &'static str,
    pub verb: &'static str,
    pub adjective: &'static str,
    pub adverb: &'static str,

    /// The result that opens Muelle's settings.
    pub own_settings: &'static str,
    pub own_settings_note: &'static str,
    pub pages: [&'static str; PAGES.len()],
}

/// The text for the language chosen in Settings.
#[must_use]
pub fn for_language(language: Language) -> &'static Strings {
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
    catalog(chosen)
}

fn catalog(language: UiLanguage) -> &'static Strings {
    match language {
        UiLanguage::Spanish => &es::STRINGS,
        UiLanguage::English => &en::STRINGS,
        UiLanguage::French => &fr::STRINGS,
        UiLanguage::Chinese => &zh::STRINGS,
        UiLanguage::Hindi => &hi::STRINGS,
        UiLanguage::Arabic => &ar::STRINGS,
        UiLanguage::Bengali => &bn::STRINGS,
        UiLanguage::Portuguese => &pt::STRINGS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_is_complete_and_keeps_its_placeholders() {
        use Language::{Ar, Bn, En, Es, Fr, Hi, Pt, Zh};
        for language in [En, Es, Fr, Zh, Hi, Ar, Bn, Pt] {
            let s = for_language(language);
            for (text, placeholders) in [
                (s.search_for, &["{text}"][..]),
                (s.search_site_for, &["{site}", "{text}"]),
                (s.no_definition, &["{text}"]),
                (s.no_files_contain, &["{text}"]),
                (s.similar, &["{list}"]),
            ] {
                for placeholder in placeholders {
                    assert!(text.contains(placeholder), "{language:?}: {text}");
                }
            }
            assert!(s.pages.iter().all(|name| !name.is_empty()), "{language:?}");
            assert!(s.type_words_note.contains("in:"), "{language:?}");
        }
    }
}
