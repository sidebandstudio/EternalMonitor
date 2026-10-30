use std::collections::{BTreeMap, BTreeSet};

use super::bidi::{self, visual};
use super::*;

fn strings(lang: Lang) -> BTreeMap<String, String> {
    parse(lang.strings())
}

/// `{name}` placeholders, in order.
fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        found.push(&rest[start..start + end + 1]);
        rest = &rest[start + end + 1..];
    }
    found.sort_unstable();
    found
}

#[test]
fn every_language_has_exactly_the_english_keys_and_placeholders() {
    let english = strings(Lang::En);
    for (key, value) in &english {
        assert_eq!(key, value, "en.json maps each key to itself");
    }
    for lang in Lang::ALL {
        let translated = strings(lang);
        let missing: Vec<_> = english
            .keys()
            .filter(|k| !translated.contains_key(*k))
            .collect();
        let extra: Vec<_> = translated
            .keys()
            .filter(|k| !english.contains_key(*k))
            .collect();
        assert!(missing.is_empty(), "{} lacks {missing:?}", lang.code());
        assert!(
            extra.is_empty(),
            "{} has keys English lacks: {extra:?}",
            lang.code()
        );
        for (key, value) in &translated {
            assert_eq!(
                placeholders(key),
                placeholders(value),
                "{}: placeholders differ in {key:?}",
                lang.code()
            );
            assert!(
                !value.trim().is_empty(),
                "{}: {key:?} is empty",
                lang.code()
            );
        }
    }
}

/// Automation names come from mapping shown text back to its key, so no two
/// keys may share a translation. Settings' "Stream PC audio" and setup's
/// "Play PC audio" are one toggle, translated with the glossary's term.
#[test]
fn translations_are_unique_within_a_language() {
    for lang in Lang::ALL {
        let mut seen = BTreeMap::new();
        for (key, value) in strings(lang) {
            if key == "Stream PC audio" && lang != Lang::En {
                assert_eq!(value, strings(lang)["Play PC audio"], "{}", lang.code());
                continue;
            }
            if let Some(other) = seen.insert(value.clone(), key.clone()) {
                panic!("{}: {other:?} and {key:?} both read {value:?}", lang.code());
            }
        }
    }
}

/// Every `tr("…")` in the source has a key, and every key is still used.
#[test]
fn the_source_and_the_english_file_agree() {
    let english = strings(Lang::En);
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut literals = BTreeSet::new();
    let mut translated = BTreeSet::new();
    let pattern = regex::Regex::new(r#"\btrf?\(\s*"((?:[^"\\]|\\.)*)""#).unwrap();
    let literal = regex::Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                let text = std::fs::read_to_string(&path).unwrap();
                let unescape = |s: &str| s.replace("\\\"", "\"").replace("\\'", "'");
                translated.extend(pattern.captures_iter(&text).map(|c| unescape(&c[1])));
                literals.extend(literal.captures_iter(&text).map(|c| unescape(&c[1])));
            }
        }
    }
    let missing: Vec<_> = translated
        .iter()
        .filter(|k| !english.contains_key(*k))
        .collect();
    assert!(missing.is_empty(), "en.json lacks {missing:?}");
    let unused: Vec<_> = english.keys().filter(|k| !literals.contains(*k)).collect();
    assert!(unused.is_empty(), "no source uses {unused:?}");
}

/// The glossary's terms, shared with the iPad app, read the same here.
#[test]
fn glossary_terms_are_used_exactly() {
    let glossary = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../i18n/glossary.md"),
    )
    .unwrap();
    let mut header: Vec<String> = Vec::new();
    let mut checked = 0;
    for line in glossary.lines() {
        let cells: Vec<String> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        if cells.first().is_some_and(|c| c == "English") {
            header = cells;
            continue;
        }
        if header.is_empty() || cells.len() != header.len() || cells[0].starts_with("---") {
            continue;
        }
        // "Mirror (mode)" names the "Mirror my screen" choice; the others
        // are the keys themselves.
        let term = cells[0].split(" (").next().unwrap_or_default();
        let key = match term {
            "Mirror" => "Mirror my screen",
            other => other,
        };
        if !strings(Lang::En).contains_key(key) {
            continue;
        }
        for (column, value) in header.iter().zip(&cells).skip(1) {
            let lang = Lang::from_code(column).expect("a glossary column is a language");
            assert_eq!(
                strings(lang)[key],
                *value,
                "{}: {key:?} must be the glossary's term",
                lang.code()
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 10 * 8,
        "only {checked} glossary terms were checked"
    );
}

#[test]
fn never_translated_names_stay() {
    for lang in Lang::ALL {
        for (key, value) in strings(lang) {
            for name in [
                "EternalMonitor",
                "iPad",
                "Windows",
                "Apple Devices",
                "Wi-Fi",
                "USB",
            ] {
                if key.contains(name) {
                    assert!(
                        value.contains(name),
                        "{}: {key:?} must keep {name:?}, reads {value:?}",
                        lang.code()
                    );
                }
            }
        }
    }
}

/// Arabic uses Western digits: Arabic-Indic digits are Arabic script, which
/// the shaper would reverse.
#[test]
fn arabic_has_no_arabic_indic_digits() {
    for (key, value) in strings(Lang::Ar) {
        assert!(
            !value
                .chars()
                .any(|c| ('\u{0660}'..='\u{0669}').contains(&c)),
            "{key:?}"
        );
    }
}

#[test]
fn system_languages_map_to_the_eleven() {
    for (tag, lang) in [
        ("en-US", Lang::En),
        ("es-MX", Lang::Es),
        ("es-ES", Lang::Es),
        ("pt-PT", Lang::PtBr),
        ("pt-BR", Lang::PtBr),
        ("zh-CN", Lang::ZhHans),
        ("zh-SG", Lang::ZhHans),
        ("zh-Hans-HK", Lang::ZhHans),
        ("zh-TW", Lang::En),
        ("zh-HK", Lang::En),
        ("zh-Hant-TW", Lang::En),
        ("ar-SA", Lang::Ar),
        ("hi-IN", Lang::Hi),
        ("ru-RU", Lang::Ru),
        ("ja-JP", Lang::Ja),
        ("fr-CA", Lang::Fr),
        ("de-CH", Lang::De),
        ("ko-KR", Lang::Ko),
        ("nl-NL", Lang::En),
        ("", Lang::En),
    ] {
        assert_eq!(from_system_tag(tag), lang, "{tag}");
    }
}

#[test]
fn a_choice_in_the_app_wins_over_the_installer_and_the_system() {
    assert_eq!(resolve(Some("de")), Lang::De);
    assert_eq!(resolve(Some("zh-Hans")), Lang::ZhHans);
    // Unknown codes and System language fall back like no choice; the test
    // binary has no installer file and macOS reports no system language.
    if cfg!(not(windows)) {
        assert_eq!(resolve(Some(SYSTEM)), Lang::En);
        assert_eq!(resolve(Some("xx")), Lang::En);
        assert_eq!(resolve(None), Lang::En);
    }
}

#[test]
fn translations_switch_and_name_back_to_english() {
    activate(Lang::De);
    assert_eq!(tr("Settings"), "Einstellungen");
    assert_eq!(name("Einstellungen"), "Settings");
    assert_eq!(tr("No such key"), "No such key");
    assert_eq!(
        trf("Streaming to {device}", &[("device", "iPad Pro")]),
        strings(Lang::De)["Streaming to {device}"].replace("{device}", "iPad Pro")
    );
    activate(Lang::En);
    assert_eq!(tr("Settings"), "Settings");
    assert_eq!(name("Einstellungen"), "Einstellungen");
}

#[test]
fn arabic_translations_are_drawn_in_visual_order_and_named_in_english() {
    activate(Lang::Ar);
    let settings = tr("Settings");
    assert_eq!(
        settings,
        strings(Lang::Ar)["Settings"],
        "one word stays as is"
    );
    assert_eq!(name(settings), "Settings");
    assert!(logical(settings).is_some());
    activate(Lang::En);
    assert_eq!(logical("Settings"), None);
}

// ── Visual order ────────────────────────────────────────────────────────────
// The expected strings are in display order, left to right. Each Arabic word
// keeps its reading order, since the shaper reverses it when it draws.

#[test]
fn arabic_words_swap_places_and_keep_their_letters() {
    // "تشغيل صوت الكمبيوتر": play, sound, the computer.
    assert_eq!(visual("تشغيل صوت الكمبيوتر"), "الكمبيوتر صوت تشغيل");
}

#[test]
fn a_latin_word_inside_arabic_keeps_its_place() {
    // "افتح EternalMonitor على iPad": open, EternalMonitor, on, iPad.
    assert_eq!(
        visual("افتح EternalMonitor على iPad"),
        "iPad على EternalMonitor افتح"
    );
}

#[test]
fn a_number_with_its_unit_reads_in_order() {
    // "الخطوة 2 من 5": step 2 of 5.
    assert_eq!(visual("الخطوة 2 من 5"), "5 من 2 الخطوة");
    // An isolated value, as `trf` makes it, stays whole: "60 fps".
    let line = format!("البث {}60 fps{}", bidi::FSI, bidi::PDI);
    assert_eq!(visual(&line), "60 fps البث");
}

#[test]
fn several_latin_words_stay_together_and_punctuation_follows_the_arabic() {
    // "البث إلى Test iPad Pro.": streaming to Test iPad Pro, full stop.
    assert_eq!(visual("البث إلى Test iPad Pro."), ".Test iPad Pro إلى البث");
    // Brackets are mirrored inside right-to-left text.
    assert_eq!(visual("شاشة (ثانية)"), "(ثانية) شاشة");
}

#[test]
fn latin_text_is_left_alone() {
    assert_eq!(visual("Settings"), "Settings");
    assert_eq!(visual("Test iPad Pro"), "Test iPad Pro");
}

#[test]
fn wrapped_arabic_breaks_in_reading_order() {
    // One "character" per letter as the width: three words of three letters
    // and a space fit two words to a line.
    let text = "ابت ثجح خدذ";
    let lines = bidi::wrap(text, 7.0, |line| line.chars().count() as f32);
    assert_eq!(lines, "ثجح ابت\nخدذ");
}
