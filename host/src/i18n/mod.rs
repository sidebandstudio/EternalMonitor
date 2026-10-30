//! The window's language and its translated basics.
//!
//! Only what someone needs to operate the app is translated: page titles,
//! buttons, setting names and their values, short status words and every
//! setup step. Errors, troubleshooting, diagnostics, update and support text
//! stay in English. Fixed terms are in `i18n/glossary.md` at the repository
//! root, shared with the iPad app, the installer and the website.
//!
//! Each language is a JSON file from English text to its translation,
//! compiled in. The English text is the key, so the source reads as it did
//! and an untranslated key shows in English. Automation names stay English:
//! widgets name themselves with [`name`], which maps a shown translation back
//! to its key.

pub mod bidi;

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Lang {
    En,
    Es,
    Ar,
    ZhHans,
    Hi,
    Ru,
    Ja,
    PtBr,
    Fr,
    De,
    Ko,
}

impl Lang {
    /// In the glossary's order, which is also the picker's.
    pub const ALL: [Lang; 11] = [
        Lang::En,
        Lang::Es,
        Lang::Ar,
        Lang::ZhHans,
        Lang::Hi,
        Lang::Ru,
        Lang::Ja,
        Lang::PtBr,
        Lang::Fr,
        Lang::De,
        Lang::Ko,
    ];

    /// The code saved in settings.json and written by the installer.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
            Lang::Ar => "ar",
            Lang::ZhHans => "zh-Hans",
            Lang::Hi => "hi",
            Lang::Ru => "ru",
            Lang::Ja => "ja",
            Lang::PtBr => "pt-BR",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Ko => "ko",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|lang| lang.code() == code)
    }

    /// The name the picker shows, in the language itself.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Es => "Español",
            Lang::Ar => "العربية",
            Lang::ZhHans => "简体中文",
            Lang::Hi => "हिन्दी",
            Lang::Ru => "Русский",
            Lang::Ja => "日本語",
            Lang::PtBr => "Português (Brasil)",
            Lang::Fr => "Français",
            Lang::De => "Deutsch",
            Lang::Ko => "한국어",
        }
    }

    /// Arabic reads right to left; the window's layout stays left to right.
    pub fn is_rtl(self) -> bool {
        self == Lang::Ar
    }

    fn strings(self) -> &'static str {
        match self {
            Lang::En => include_str!("strings/en.json"),
            Lang::Es => include_str!("strings/es.json"),
            Lang::Ar => include_str!("strings/ar.json"),
            Lang::ZhHans => include_str!("strings/zh-Hans.json"),
            Lang::Hi => include_str!("strings/hi.json"),
            Lang::Ru => include_str!("strings/ru.json"),
            Lang::Ja => include_str!("strings/ja.json"),
            Lang::PtBr => include_str!("strings/pt-BR.json"),
            Lang::Fr => include_str!("strings/fr.json"),
            Lang::De => include_str!("strings/de.json"),
            Lang::Ko => include_str!("strings/ko.json"),
        }
    }
}

/// The saved `language` value for "System language". `None` means nothing
/// was picked in the app yet: then the installer's choice applies, if any.
pub const SYSTEM: &str = "system";

/// The language a saved choice shows: one picked in the app, the system's
/// for "System language", otherwise the installer's choice or the system's.
pub fn resolve(choice: Option<&str>) -> Lang {
    match choice {
        Some(SYSTEM) => system_language(),
        Some(code) => Lang::from_code(code).unwrap_or_else(default_language),
        None => default_language(),
    }
}

/// What applies before anything is picked in the app.
pub fn default_language() -> Lang {
    installer_language().unwrap_or_else(system_language)
}

/// The language picked in Setup, which it writes next to the exe (it runs
/// elevated, so it never writes the user's settings). Read once: Setup
/// replaces it only while the app is closed.
pub fn installer_language() -> Option<Lang> {
    static CHOICE: OnceLock<Option<Lang>> = OnceLock::new();
    *CHOICE.get_or_init(|| {
        let path = std::env::current_exe().ok()?.parent()?.join("language.txt");
        let text = std::fs::read_to_string(path).ok()?;
        Lang::from_code(text.trim())
    })
}

/// The Windows display language, when it is one of the eleven.
pub fn system_language() -> Lang {
    system_tag().map_or(Lang::En, |tag| from_system_tag(&tag))
}

/// A BCP 47 tag as Windows gives it, e.g. `pt-PT` or `zh-Hant-TW`. Spanish
/// and Portuguese of any region map to the one variant there is, Traditional
/// Chinese to English.
pub fn from_system_tag(tag: &str) -> Lang {
    let tag = tag.replace('_', "-").to_ascii_lowercase();
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or_default();
    let rest: Vec<&str> = parts.collect();
    match primary {
        "es" => Lang::Es,
        "pt" => Lang::PtBr,
        "ar" => Lang::Ar,
        "hi" => Lang::Hi,
        "ru" => Lang::Ru,
        "ja" => Lang::Ja,
        "fr" => Lang::Fr,
        "de" => Lang::De,
        "ko" => Lang::Ko,
        "zh" if rest.contains(&"hant") => Lang::En,
        "zh" if rest
            .iter()
            .any(|part| matches!(*part, "hans" | "cn" | "sg")) =>
        {
            Lang::ZhHans
        }
        _ => Lang::En,
    }
}

#[cfg(windows)]
fn system_tag() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    let (mut count, mut len) = (0u32, 0u32);
    unsafe {
        GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, PWSTR::null(), &mut len).ok()?;
        let mut buffer = vec![0u16; len as usize];
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
        // A double-NUL-terminated list; the first is the display language.
        let end = buffer.iter().position(|&c| c == 0)?;
        Some(String::from_utf16_lossy(&buffer[..end])).filter(|tag| !tag.is_empty())
    }
}

#[cfg(not(windows))]
fn system_tag() -> Option<String> {
    None
}

// ── Tables ──────────────────────────────────────────────────────────────────

struct Table {
    /// Key → the text as drawn (Arabic in visual order, see `bidi`).
    shown: HashMap<&'static str, &'static str>,
    /// Key → the text in reading order.
    logical: HashMap<&'static str, &'static str>,
    /// Drawn text → key, for automation names and Arabic wrapping.
    keys: HashMap<&'static str, &'static str>,
}

fn parse(json: &str) -> BTreeMap<String, String> {
    serde_json::from_str(json).expect("the compiled-in strings are valid JSON")
}

fn table(lang: Lang) -> &'static Table {
    static TABLES: [OnceLock<Table>; 11] = [const { OnceLock::new() }; 11];
    let index = Lang::ALL.iter().position(|l| *l == lang).unwrap_or(0);
    TABLES[index].get_or_init(|| {
        // Built once per language and kept for the process: a few kilobytes.
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let mut table = Table {
            shown: HashMap::new(),
            logical: HashMap::new(),
            keys: HashMap::new(),
        };
        for (key, value) in parse(lang.strings()) {
            let key = leak(key);
            let shown = if lang.is_rtl() {
                leak(bidi::visual(&value))
            } else {
                leak(value.clone())
            };
            table.logical.insert(key, leak(value));
            table.shown.insert(key, shown);
            table.keys.insert(shown, key);
        }
        table
    })
}

thread_local! {
    // The window draws on one thread. Per thread, so the unit tests that
    // change it do not race each other.
    static ACTIVE: Cell<Lang> = const { Cell::new(Lang::En) };
}

/// The language the window draws in.
pub fn active() -> Lang {
    ACTIVE.with(Cell::get)
}

/// Draws in `lang` from now on. Its fonts must already be installed.
pub fn activate(lang: Lang) {
    ACTIVE.with(|active| active.set(lang));
}

pub fn is_rtl() -> bool {
    active().is_rtl()
}

/// `key` in the active language, as drawn. English when untranslated.
pub fn tr(key: &'static str) -> &'static str {
    match active() {
        Lang::En => key,
        lang => table(lang).shown.get(key).copied().unwrap_or(key),
    }
}

/// [`tr`] with `{name}` placeholders filled in. In Arabic each value is
/// isolated, so a device name or a number keeps its own order.
pub fn trf(key: &'static str, args: &[(&str, &str)]) -> String {
    let lang = active();
    let mut text = match lang {
        Lang::En => key.to_string(),
        lang => table(lang)
            .logical
            .get(key)
            .copied()
            .unwrap_or(key)
            .to_string(),
    };
    for (name, value) in args {
        let value = if lang.is_rtl() {
            format!("{}{value}{}", bidi::FSI, bidi::PDI)
        } else {
            (*value).to_string()
        };
        text = text.replace(&format!("{{{name}}}"), &value);
    }
    if lang.is_rtl() {
        bidi::visual(&text)
    } else {
        text
    }
}

/// The English text a widget shows `shown` for: its automation name, which
/// scripts and tests find it by in every language.
pub fn name(shown: &str) -> &str {
    match active() {
        Lang::En => shown,
        lang => table(lang).keys.get(shown).copied().unwrap_or(shown),
    }
}

/// `shown` in reading order, when it is a right-to-left translation; for
/// widgets that wrap it (see [`bidi::wrap`]).
pub fn logical(shown: &str) -> Option<&'static str> {
    let lang = active();
    if !lang.is_rtl() {
        return None;
    }
    let table = table(lang);
    table
        .keys
        .get(shown)
        .and_then(|key| table.logical.get(key))
        .copied()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// Every translation in `lang`'s file.
    pub fn values(lang: Lang) -> Vec<String> {
        parse(lang.strings()).into_values().collect()
    }
}
