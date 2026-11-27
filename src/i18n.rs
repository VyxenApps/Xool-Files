use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Language {
    #[default]
    Automatic,
    Spanish,
    English,
}

impl Language {
    pub(crate) const OPTIONS: [Self; 3] = [Self::Automatic, Self::Spanish, Self::English];
}

static ENGLISH: AtomicBool = AtomicBool::new(false);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

pub(crate) fn set(language: Language) {
    let use_english = resolves_to_english(language, unsafe { GetUserDefaultUILanguage() });
    ENGLISH.store(use_english, Ordering::Relaxed);
    gpui_component::set_locale(if use_english { "en" } else { "es" });
}

fn resolves_to_english(language: Language, windows_language: u16) -> bool {
    match language {
        Language::Automatic => (windows_language & 0x03ff) != 0x0a,
        Language::Spanish => false,
        Language::English => true,
    }
}

#[cfg(test)]
#[test]
fn locale_uses_windows_for_automatic_and_respects_manual_choices() {
    assert!(!resolves_to_english(Language::Automatic, 0x280a)); // Spanish (Peru)
    assert!(resolves_to_english(Language::Automatic, 0x0409)); // English (US)
    assert!(resolves_to_english(Language::Automatic, 0x0411)); // Japanese
    assert!(resolves_to_english(Language::English, 0x280a));
    assert!(!resolves_to_english(Language::Spanish, 0x0409));
}

pub(crate) fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub(crate) fn tr<'a>(spanish: &'a str, english: &'a str) -> &'a str {
    if is_english() { english } else { spanish }
}

#[macro_export]
macro_rules! localized {
    ($formatter:ident, $spanish:literal, $english:literal $($args:tt)*) => {{
        if $crate::i18n::is_english() {
            $formatter!($english $($args)*)
        } else {
            $formatter!($spanish $($args)*)
        }
    }};
}
