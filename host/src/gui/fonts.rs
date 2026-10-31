//! Fonts for the window's language. Geist covers Latin and Cyrillic and
//! nothing else, and large fonts are not bundled: Chinese, Japanese, Korean,
//! Hindi and Arabic come from the fonts Windows ships, loaded only for the
//! language shown, so Japanese Han is drawn by a Japanese font and Chinese by
//! a Chinese one. The picker's language names come from bundled subsets that
//! hold only their own few glyphs (about 17 KB together), so showing the
//! picker never loads a system font.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use eframe::egui::{FontData, FontFamily};
use parking_lot::Mutex;

use crate::i18n::Lang;

/// Whether `lang` needs a font besides Geist.
pub fn needs_system_font(lang: Lang) -> bool {
    matches!(
        lang,
        Lang::ZhHans | Lang::Ja | Lang::Ko | Lang::Hi | Lang::Ar
    )
}

/// The font files for `lang` in [`fonts_dir`], best first, each with the
/// family of the face to use (a `.ttc` holds several).
pub fn candidates(lang: Lang) -> &'static [(&'static str, &'static str)] {
    #[cfg(windows)]
    {
        match lang {
            Lang::ZhHans => &[("msyh.ttc", "Microsoft YaHei UI")],
            Lang::Ja => &[
                ("YuGothM.ttc", "Yu Gothic UI"),
                ("YuGothR.ttc", "Yu Gothic UI Semilight"),
                ("meiryo.ttc", "Meiryo UI"),
            ],
            Lang::Ko => &[("malgun.ttf", "Malgun Gothic")],
            Lang::Hi => &[("Nirmala.ttc", "Nirmala UI"), ("Nirmala.ttf", "Nirmala UI")],
            Lang::Ar => &[("segoeui.ttf", "Segoe UI")],
            _ => &[],
        }
    }
    // Stand-ins for development and the GUI snapshots, which run on macOS.
    #[cfg(target_os = "macos")]
    {
        match lang {
            Lang::ZhHans => &[("Hiragino Sans GB.ttc", "Hiragino Sans GB")],
            Lang::Ja => &[("ヒラギノ角ゴシック W3.ttc", "Hiragino Sans")],
            Lang::Ko => &[("AppleSDGothicNeo.ttc", "Apple SD Gothic Neo")],
            Lang::Hi => &[("Kohinoor.ttc", "Kohinoor Devanagari")],
            Lang::Ar => &[("GeezaPro.ttc", "Geeza Pro")],
            _ => &[],
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = lang;
        &[]
    }
}

pub fn fonts_dir() -> PathBuf {
    if cfg!(windows) {
        let windows = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(windows).join("Fonts")
    } else {
        PathBuf::from("/System/Library/Fonts")
    }
}

/// The system face `lang` is drawn with, or `None` when no candidate file
/// is there.
pub fn load(lang: Lang) -> Option<FontData> {
    let dir = fonts_dir();
    candidates(lang).iter().find_map(|(file, family)| {
        let bytes = read_once(&dir.join(file))?;
        let index = face_index(bytes, family)?;
        Some(FontData {
            font: Cow::Borrowed(bytes),
            index,
            tweak: Default::default(),
        })
    })
}

/// A font file's bytes, read once and kept for the process. egui copies an
/// owned font each time it builds its fonts, which doubled a 20 MB Chinese
/// font; borrowed bytes are shared. Switching back to a language reuses its
/// file.
fn read_once(path: &Path) -> Option<&'static [u8]> {
    static FILES: Mutex<Vec<(PathBuf, &'static [u8])>> = Mutex::new(Vec::new());
    let mut files = FILES.lock();
    if let Some((_, bytes)) = files.iter().find(|(p, _)| p == path) {
        return Some(bytes);
    }
    let bytes: &'static [u8] = Box::leak(std::fs::read(path).ok()?.into_boxed_slice());
    files.push((path.to_path_buf(), bytes));
    Some(bytes)
}

/// Which face in a font file (or collection) is `family`, by its family or
/// typographic family name (some put the weight in the first).
pub fn face_index(bytes: &[u8], family: &str) -> Option<u32> {
    use skrifa::raw::FileRef;
    use skrifa::string::StringId;
    use skrifa::MetadataProvider;

    let file = FileRef::new(bytes).ok()?;
    let index = file.fonts().position(|font| {
        let Ok(font) = font else {
            return false;
        };
        [StringId::FAMILY_NAME, StringId::TYPOGRAPHIC_FAMILY_NAME]
            .into_iter()
            .filter_map(|id| font.localized_strings(id).english_or_first())
            .any(|name| name.to_string() == family)
    })?;
    u32::try_from(index).ok()
}

/// The characters of `text` the face has no glyph for, apart from spaces
/// and controls.
#[cfg(test)]
pub fn missing_glyphs(bytes: &[u8], index: u32, text: &str) -> Vec<char> {
    use skrifa::raw::FileRef;
    use skrifa::MetadataProvider;

    let Some(font) = FileRef::new(bytes)
        .ok()
        .and_then(|file| file.fonts().nth(index as usize))
        .and_then(Result::ok)
    else {
        return text.chars().collect();
    };
    let charmap = font.charmap();
    let mut missing: Vec<char> = text
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control() && charmap.map(*c).is_none())
        .collect();
    missing.sort_unstable();
    missing.dedup();
    missing
}

/// The bundled subsets that draw the picker's language names.
pub(super) const NAME_FACES: [(Lang, &str, &[u8]); 5] = [
    (
        Lang::ZhHans,
        "names-zh-Hans",
        include_bytes!("../../assets/fonts/NotoSansSC-names.ttf"),
    ),
    (
        Lang::Ja,
        "names-ja",
        include_bytes!("../../assets/fonts/NotoSansJP-names.ttf"),
    ),
    (
        Lang::Ko,
        "names-ko",
        include_bytes!("../../assets/fonts/NotoSansKR-names.ttf"),
    ),
    (
        Lang::Hi,
        "names-hi",
        include_bytes!("../../assets/fonts/NotoSansDevanagari-names.ttf"),
    ),
    (
        Lang::Ar,
        "names-ar",
        include_bytes!("../../assets/fonts/NotoSansArabic-names.ttf"),
    ),
];

/// The family `lang`'s name is drawn with in the picker.
pub fn name_family(lang: Lang) -> FontFamily {
    NAME_FACES
        .iter()
        .find(|(l, _, _)| *l == lang)
        .map_or(FontFamily::Proportional, |(_, name, _)| {
            FontFamily::Name((*name).into())
        })
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod tests {
    use super::*;

    fn strings(lang: Lang) -> String {
        crate::i18n::tests_support::values(lang).join("\n")
    }

    const GEIST: &[u8] = include_bytes!("../../assets/fonts/Geist-Regular.ttf");

    /// Each language's font, from the real fonts folder (Windows, or the
    /// macOS stand-ins), has a glyph for every character of that language's
    /// string file that it is asked to draw: all but the Latin, digits and
    /// punctuation Geist draws first.
    #[test]
    fn every_language_font_draws_its_strings() {
        for lang in Lang::ALL.into_iter().filter(|l| needs_system_font(*l)) {
            let dir = fonts_dir();
            let (file, family) = candidates(lang)
                .iter()
                .find(|(file, _)| dir.join(file).exists())
                .unwrap_or_else(|| panic!("no font file for {} in {}", lang.code(), dir.display()));
            let bytes = std::fs::read(dir.join(file)).unwrap();
            let index = face_index(&bytes, family)
                .unwrap_or_else(|| panic!("{file} has no face named {family}"));
            let not_geist: String = missing_glyphs(GEIST, 0, &strings(lang))
                .into_iter()
                .collect();
            assert!(
                !not_geist.is_empty(),
                "{} needs no font of its own",
                lang.code()
            );
            let missing = missing_glyphs(&bytes, index, &not_geist);
            assert!(
                missing.is_empty(),
                "{} ({file}) lacks {missing:?}",
                lang.code()
            );
            assert!(load(lang).is_some(), "{} does not load", lang.code());
        }
    }

    /// Latin and Cyrillic are Geist's alone.
    #[test]
    fn geist_draws_the_latin_and_cyrillic_languages() {
        for face in [
            GEIST,
            include_bytes!("../../assets/fonts/Geist-Medium.ttf"),
            include_bytes!("../../assets/fonts/Geist-SemiBold.ttf"),
        ] {
            for lang in Lang::ALL.into_iter().filter(|l| !needs_system_font(*l)) {
                let mut text = strings(lang);
                text.push_str(lang.native_name());
                let missing = missing_glyphs(face, 0, &text);
                assert!(missing.is_empty(), "{} lacks {missing:?}", lang.code());
            }
        }
    }

    #[test]
    fn the_name_subsets_draw_the_language_names() {
        for (lang, _, bytes) in NAME_FACES {
            assert_eq!(
                missing_glyphs(bytes, 0, lang.native_name()),
                Vec::<char>::new(),
                "{}",
                lang.code()
            );
        }
    }

    /// Glyphs of `shown` as egui lays them out with the Arabic font, left
    /// to right.
    fn laid_out(shown: &str) -> Vec<char> {
        let ctx = eframe::egui::Context::default();
        assert_eq!(super::super::theme::set_language(&ctx, Lang::Ar), Lang::Ar);
        let mut glyphs = Vec::new();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(
                shown.to_string(),
                eframe::egui::FontId::proportional(14.0),
                eframe::egui::Color32::WHITE,
            );
            let mut row: Vec<_> = galley.rows[0]
                .glyphs
                .iter()
                // Not spaces, nor the zero-width stand-ins egui adds for
                // the other characters of a shaped cluster.
                .filter(|g| !g.chr.is_whitespace() && !g.uv_rect.is_nothing())
                .map(|g| (g.pos.x, g.chr))
                .collect();
            row.sort_by(|a, b| a.0.total_cmp(&b.0));
            glyphs = row.into_iter().map(|(_, c)| c).collect();
        });
        output.textures_delta.clear();
        glyphs
    }

    /// Arabic as drawn: the first word of the sentence is rightmost, a
    /// Latin name sits between the Arabic words around it, and the full
    /// stop ends up leftmost.
    #[test]
    fn arabic_reads_right_to_left_in_the_window() {
        crate::i18n::activate(Lang::Ar);
        // "كابل بين iPad وهذا الكمبيوتر." — a cable between iPad and this PC.
        let shown = crate::i18n::tr("A cable between your iPad and this PC.");
        crate::i18n::activate(Lang::En);
        let glyphs = laid_out(shown);
        let at = |c: char| {
            glyphs
                .iter()
                .position(|g| *g == c)
                .unwrap_or_else(|| panic!("{c} in {glyphs:?}"))
        };
        assert_eq!(glyphs.first(), Some(&'.'), "{glyphs:?}");
        assert_eq!(glyphs.last(), Some(&'ك'), "{glyphs:?}");
        // Left to right: "الكمبيوتر", "وهذا", then iPad in its own order,
        // then "بين كابل". (Fonts join some letter pairs into one glyph.)
        assert!(at('ت') < at('ذ') && at('ذ') < at('i'), "{glyphs:?}");
        assert!(at('i') < at('P') && at('P') < at('d'), "{glyphs:?}");
    }

    /// A long iPad name in Arabic, cut where the line ends: the sentence
    /// keeps its start, "البث إلى" (streaming to), rightmost, and the name
    /// loses its end, where the "…" goes.
    #[test]
    fn a_long_ipad_name_is_cut_at_the_end_of_the_arabic_sentence() {
        use eframe::egui::RichText;
        crate::i18n::activate(Lang::Ar);
        let shown = crate::i18n::trf(
            "Streaming to {device}",
            &[("device", "Test iPad Pro 13-inch with a very long name")],
        );
        let ctx = eframe::egui::Context::default();
        assert_eq!(super::super::theme::set_language(&ctx, Lang::Ar), Lang::Ar);
        let style = |text: String| RichText::new(text).size(14.0);
        let (mut cut, mut cut_width, mut whole_width) = (String::new(), 0.0, 0.0);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let width = |text: &str| {
                ui.painter()
                    .layout_no_wrap(
                        text.to_string(),
                        eframe::egui::FontId::proportional(14.0),
                        eframe::egui::Color32::WHITE,
                    )
                    .size()
                    .x
            };
            cut = super::super::widgets::fit_line(ui, &shown, 160.0, &style);
            cut_width = width(&cut);
            whole_width = width(&shown);
        });
        output.textures_delta.clear();
        crate::i18n::activate(Lang::En);
        assert!(whole_width > 160.0, "the whole line fits: {whole_width}");
        assert!(cut_width <= 160.0, "{cut} is {cut_width} wide");
        assert!(!cut.contains("name"), "{cut}");
        let glyphs = laid_out(&cut);
        let at = |c: char| {
            glyphs
                .iter()
                .position(|g| *g == c)
                .unwrap_or_else(|| panic!("{c} in {glyphs:?}"))
        };
        // Left to right: the name and its "…", then "إلى", then "البث".
        assert!(at('T') < at('…') && at('…') < at('إ'), "{glyphs:?}");
        assert!(at('إ') < at('ث'), "{glyphs:?}");
    }

    /// The process's memory once the window's strings in `EM_MEMORY_LANG`
    /// (default `en`) are laid out and drawn into the font atlas. Compare a
    /// run per language:
    /// `EM_MEMORY_LANG=zh-Hans cargo test -p eternal-host --lib memory_with_language -- --ignored --nocapture`
    #[test]
    #[ignore = "measures memory; run explicitly"]
    fn memory_with_language() {
        let code = std::env::var("EM_MEMORY_LANG").unwrap_or_else(|_| "en".into());
        let lang = Lang::from_code(&code).expect("a language code");
        let before = memory();
        let ctx = eframe::egui::Context::default();
        let shown = super::super::theme::set_language(&ctx, lang);
        assert_eq!(shown, lang, "the font for {code} is missing");
        let text = strings(lang);
        for _ in 0..3 {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                for size in [11.5, 13.5, 20.0] {
                    for line in text.lines() {
                        ui.label(eframe::egui::RichText::new(line).size(size));
                    }
                }
            });
            let _ = ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
            // No renderer takes the atlas here.
            output.textures_delta.clear();
        }
        let after = memory();
        println!(
            "EM_MEMORY {code}: {:.1} MB before, {:.1} MB after, +{:.1} MB",
            before,
            after,
            after - before
        );
    }

    /// Private bytes on Windows, resident memory on macOS, in MB.
    fn memory() -> f64 {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            };
            use windows::Win32::System::Threading::GetCurrentProcess;
            let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
            .unwrap();
            counters.PrivateUsage as f64 / 1e6
        }
        #[cfg(not(windows))]
        {
            let out = std::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<f64>()
                .unwrap()
                / 1e3
        }
    }
}
