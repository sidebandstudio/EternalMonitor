//! Renders the bundled Virtual Display Driver's `vdd_settings.xml` so the
//! managed virtual display can offer the iPad's native resolution.
//!
//! The driver (VirtualDrivers/Virtual-Display-Driver v25.x) reads
//! `C:\VirtualDisplayDriver\vdd_settings.xml` at enable time; options absent
//! from the file fall back to driver defaults, so this writes a minimal
//! document: one monitor, a global refresh rate, and a resolution list led by
//! the client's native landscape mode. NEEDS_WINDOWS_VERIFY: the driver's
//! acceptance of these exact modes is a hardware-runbook item.
//!
//! It also names the graphics card the driver renders on: the one driving the
//! main screen. Left unnamed, the driver picks the card with the most video
//! memory. On Intel + NVIDIA laptops that is the NVIDIA card, which drives no
//! screen, and in 0.3.1 diagnostics none of those laptops got the display.

use std::io;
use std::ops::Range;
use std::path::Path;

/// A display mode for the virtual display's mode list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VddMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

impl std::fmt::Display for VddMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.refresh_hz)
    }
}

/// The modes to advertise for a client with the given native panel, best
/// first. The iPad reports portrait-native pixels; the desktop wants
/// landscape, so the long edge becomes the width. Odd dimensions are rounded
/// down — 4:2:0 video encoders need even sizes.
pub fn modes_for_client(screen_px_w: u16, screen_px_h: u16, refresh_hz: u8) -> Vec<VddMode> {
    let long = u32::from(screen_px_w.max(screen_px_h)) & !1;
    let short = u32::from(screen_px_w.min(screen_px_h)) & !1;
    let refresh = match refresh_hz {
        0 => 60,
        hz => u32::from(hz),
    };

    let mut modes = Vec::new();
    let mut push = |mode: VddMode| {
        if mode.width >= 640 && mode.height >= 480 && !modes.contains(&mode) {
            modes.push(mode);
        }
    };
    push(VddMode {
        width: long,
        height: short,
        refresh_hz: refresh,
    });
    if refresh != 60 {
        push(VddMode {
            width: long,
            height: short,
            refresh_hz: 60,
        });
    }
    // Safety net the driver definitely supports.
    push(VddMode {
        width: 1920,
        height: 1080,
        refresh_hz: 60,
    });
    modes
}

/// The modes to offer when the iPad's screen size is not known yet.
pub fn default_modes() -> Vec<VddMode> {
    vec![VddMode {
        width: 1920,
        height: 1080,
        refresh_hz: 60,
    }]
}

/// The mode to set on the virtual display after it attached, when Windows
/// picked another one. The driver offers the iPad's mode first, but Windows
/// restores the mode it last saved for these displays: 800x600 when the
/// driver's own settings were in place the first time, which is how the
/// vendor installer leaves them.
pub fn mode_to_set(requested: Option<VddMode>, attached: (u32, u32)) -> Option<VddMode> {
    requested.filter(|mode| (mode.width, mode.height) != attached)
}

/// Render the settings document. Pure so tests can assert the exact bytes.
/// `gpu` is the DXGI adapter description the driver matches its render
/// adapter by; without it the driver chooses.
pub fn render(modes: &[VddMode], gpu: Option<&str>) -> String {
    let global_refresh = modes.first().map_or(60, |m| m.refresh_hz);
    let mut xml = String::with_capacity(512);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<vdd_settings>\n");
    xml.push_str("    <monitors>\n        <count>1</count>\n    </monitors>\n");
    if let Some(gpu) = gpu {
        xml.push_str(&gpu_element(gpu));
    }
    xml.push_str("    <global>\n");
    xml.push_str(&format!(
        "        <g_refresh_rate>{global_refresh}</g_refresh_rate>\n"
    ));
    xml.push_str("    </global>\n");
    xml.push_str("    <resolutions>\n");
    for mode in modes {
        xml.push_str(&format!(
            "        <resolution>\n            <width>{}</width>\n            \
             <height>{}</height>\n            <refresh_rate>{}</refresh_rate>\n        \
             </resolution>\n",
            mode.width, mode.height, mode.refresh_hz
        ));
    }
    xml.push_str("    </resolutions>\n");
    xml.push_str("</vdd_settings>\n");
    xml
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn gpu_element(gpu: &str) -> String {
    format!(
        "    <gpu>\n        <friendlyname>{}</friendlyname>\n    </gpu>\n",
        escape(gpu)
    )
}

/// The graphics card to name for the driver: the one driving the main
/// screen. The driver matches by description and takes the first match, so
/// a name two adapters share is not pinned; the error says why for the log.
pub fn gpu_to_pin<'a>(
    main_screen: Option<&'a str>,
    adapters: &[String],
) -> Result<&'a str, &'static str> {
    let name = main_screen
        .filter(|name| !name.is_empty())
        .ok_or("no adapter drives the main screen")?;
    if adapters.iter().filter(|adapter| *adapter == name).count() > 1 {
        return Err("two graphics cards have the same name");
    }
    Ok(name)
}

/// Where a parsed settings document names its render adapter, and how to
/// make it name another one.
enum GpuSlot {
    /// The text of the one `<friendlyname>`, as a byte range.
    Name(Range<usize>),
    /// A self-closing `<friendlyname/>` or `<gpu/>`, to be written out.
    Empty(Range<usize>, &'static str),
    /// Right after the start tag of `<gpu>` without a name, or of the root.
    Insert(usize, &'static str),
}

/// A character XML 1.0 allows in a document.
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= ' ' && !matches!(c, '\u{fffe}' | '\u{ffff}'))
}

/// An XML name, loosely: what the driver's parser and Expat accept.
fn xml_name(name: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.'))
}

/// The tag's name and every attribute are well-formed: quoted, unique,
/// with known entities and allowed characters.
fn check_tag(tag: &quick_xml::events::BytesStart) -> Result<(), &'static str> {
    if !xml_name(tag.name().as_ref()) {
        return Err("an element name is not valid XML");
    }
    for attribute in tag.attributes() {
        let attribute = attribute.map_err(|_| "an attribute is not valid XML")?;
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|_| "an attribute is not valid XML")?;
        if !xml_name(attribute.key.as_ref()) || !value.chars().all(xml_char) {
            return Err("an attribute is not valid XML");
        }
    }
    Ok(())
}

/// Find the render adapter's place by parsing `xml`. Anything the driver
/// could read differently from this host is refused: a name outside
/// `<gpu>`, a second name or `<gpu>`, markup inside the name, text or
/// elements outside the `<vdd_settings>` root, or a document that is not
/// well-formed XML 1.0 (bad attributes, misplaced declarations, characters
/// XML does not allow, unknown entities). The driver matches
/// `friendlyname` by local name anywhere.
fn gpu_slot(xml: &str) -> Result<GpuSlot, &'static str> {
    use quick_xml::events::Event;
    if !xml.chars().all(xml_char) {
        return Err("the file has characters XML does not allow");
    }
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut open: Vec<Vec<u8>> = Vec::new();
    let (mut root, mut closed, mut slot) = (None, false, None);
    let (mut gpus, mut names, mut name_start) = (0, 0, 0);
    let mut doctype = false;
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|_| "the file is not well-formed XML")?;
        let after = reader.buffer_position() as usize;
        let in_name = open.last().is_some_and(|name| name == b"friendlyname");
        let outside = open.is_empty();
        match event {
            Event::Start(_) | Event::Empty(_) if closed => {
                return Err("there is markup after the root element")
            }
            Event::Start(_) | Event::Empty(_) | Event::CData(_) if in_name => {
                return Err("the graphics card name contains markup")
            }
            Event::Start(ref tag) | Event::Empty(ref tag) => {
                check_tag(tag)?;
                let empty = matches!(event, Event::Empty(_));
                let name = tag.local_name().as_ref().to_vec();
                match (open.len(), name.as_slice()) {
                    (0, b"vdd_settings") if !empty => root = Some(after),
                    (0, _) => return Err("the root element is not <vdd_settings>"),
                    (1, b"gpu") => {
                        gpus += 1;
                        slot = Some(if empty {
                            GpuSlot::Empty(before..after, "gpu")
                        } else {
                            GpuSlot::Insert(after, "gpu")
                        });
                    }
                    (2, b"friendlyname") if open[1] == b"gpu" => {
                        names += 1;
                        name_start = after;
                        if empty {
                            slot = Some(GpuSlot::Empty(before..after, "friendlyname"));
                        }
                    }
                    (_, b"friendlyname") => return Err("a graphics card name is outside <gpu>"),
                    (_, b"gpu") => return Err("a <gpu> element is not directly in the root"),
                    _ => {}
                }
                if !empty {
                    open.push(name);
                }
            }
            Event::End(_) => {
                if in_name {
                    slot = Some(GpuSlot::Name(name_start..before));
                }
                open.pop();
                closed = open.is_empty();
            }
            Event::Text(text) if outside && !text.iter().all(u8::is_ascii_whitespace) => {
                return Err("there is text outside the root element")
            }
            Event::CData(_) | Event::GeneralRef(_) if outside => {
                return Err("there is text outside the root element")
            }
            // Only the very first thing in the file may be the declaration,
            // and a DOCTYPE only once, before the root.
            Event::Decl(_) if before != 0 => return Err("an XML declaration is out of place"),
            Event::DocType(_) if doctype || root.is_some() => {
                return Err("a DOCTYPE is out of place")
            }
            Event::DocType(_) => doctype = true,
            Event::GeneralRef(ref reference) => {
                let known = if reference.is_char_ref() {
                    matches!(reference.resolve_char_ref(), Ok(Some(c)) if xml_char(c))
                } else {
                    matches!(
                        reference.as_ref(),
                        b"lt" | b"gt" | b"amp" | b"apos" | b"quot"
                    )
                };
                if !known {
                    return Err("the file uses an entity XML does not define");
                }
            }
            Event::Text(ref text) if text.windows(3).any(|w| w == b"]]>") => {
                return Err("the file is not well-formed XML")
            }
            Event::Comment(ref text)
                if text.windows(2).any(|w| w == b"--") || text.ends_with(b"-") =>
            {
                return Err("a comment is not well-formed XML")
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let root = root
        .filter(|_| closed)
        .ok_or("there is no complete <vdd_settings> element")?;
    if gpus > 1 || names > 1 {
        return Err("it names more than one graphics card");
    }
    Ok(slot.unwrap_or(GpuSlot::Insert(root, "vdd_settings")))
}

/// `xml` naming `gpu` as the render adapter, everything else byte for byte
/// as it was. The edit is checked by parsing the result. `Err` says why the
/// document was left alone.
fn gpu_edit(xml: &str, gpu: &str) -> Result<String, &'static str> {
    let (bom, body) = match xml.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", xml),
    };
    let nl = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let name = escape(gpu);
    let element = format!("<friendlyname>{name}</friendlyname>");
    let (range, text) = match gpu_slot(body)? {
        GpuSlot::Name(range) => (range, name.clone()),
        GpuSlot::Empty(range, "gpu") => (range, format!("<gpu>{element}</gpu>")),
        GpuSlot::Empty(range, _) => (range, element),
        GpuSlot::Insert(at, "gpu") => (at..at, format!("{nl}        {element}")),
        GpuSlot::Insert(at, _) => (
            at..at,
            format!("{nl}    <gpu>{nl}        {element}{nl}    </gpu>"),
        ),
    };
    let edited = format!("{}{text}{}", &body[..range.start], &body[range.end..]);
    match gpu_slot(&edited) {
        Ok(GpuSlot::Name(range)) if edited[range.clone()] == name => Ok(format!("{bom}{edited}")),
        _ => Err("the edited file did not parse as intended"),
    }
}

/// `xml` naming `gpu` as the render adapter, everything else as it was;
/// `None` when the document is not one this can edit safely.
pub fn with_gpu(xml: &str, gpu: &str) -> Option<String> {
    gpu_edit(xml, gpu).ok()
}

/// Point the settings file the host does not otherwise write at `gpu`.
/// True when it changed. A missing file stays missing: without one the
/// driver offers its built-in modes, and a document holding only a GPU
/// would leave it none. A file that does not parse as expected is left
/// alone, and the error says why.
pub fn pin_gpu_in(path: &Path, gpu: &str) -> io::Result<bool> {
    let current = std::fs::read_to_string(path)?;
    let updated = gpu_edit(&current, gpu)
        .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))?;
    if updated == current {
        return Ok(false);
    }
    replace(path, &updated)?;
    Ok(true)
}

/// Write atomically (tmp + rename) so the driver never reads a torn file.
/// Creates the directory if the driver hasn't made it yet.
pub fn write_to(path: &Path, modes: &[VddMode], gpu: Option<&str>) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    replace(path, &render(modes, gpu))
}

/// The new contents reach the disk before they replace the file, so an
/// interrupted write leaves the old file whole.
fn replace(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("xml.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipad_portrait_panel_becomes_landscape_lead_mode() {
        // iPad Pro 11" M4: 1668x2420 portrait native, 120 Hz.
        let modes = modes_for_client(1668, 2420, 120);
        assert_eq!(
            modes[0],
            VddMode {
                width: 2420,
                height: 1668,
                refresh_hz: 120
            }
        );
        assert_eq!(
            modes[1],
            VddMode {
                width: 2420,
                height: 1668,
                refresh_hz: 60
            },
            "a 60 Hz variant backs up high-refresh panels"
        );
        assert_eq!(
            modes[2],
            VddMode {
                width: 1920,
                height: 1080,
                refresh_hz: 60
            }
        );
    }

    #[test]
    fn a_second_screen_at_another_size_is_set_to_the_requested_mode() {
        let ipad = modes_for_client(1668, 2420, 120)[0];
        // Windows restored the 800x600 it saved the first time.
        assert_eq!(mode_to_set(Some(ipad), (800, 600)), Some(ipad));
        assert_eq!(mode_to_set(Some(ipad), (2420, 1668)), None);
        // Matching is off: the size is the user's.
        assert_eq!(mode_to_set(None, (800, 600)), None);
        assert_eq!(
            default_modes(),
            [VddMode {
                width: 1920,
                height: 1080,
                refresh_hz: 60
            }]
        );
    }

    #[test]
    fn odd_dimensions_are_evened_for_420_encoders() {
        let modes = modes_for_client(1667, 2421, 60);
        assert_eq!(modes[0].width, 2420);
        assert_eq!(modes[0].height, 1666);
    }

    #[test]
    fn nonsense_panels_still_yield_the_safety_mode() {
        let modes = modes_for_client(0, 0, 0);
        assert_eq!(
            modes,
            vec![VddMode {
                width: 1920,
                height: 1080,
                refresh_hz: 60
            }]
        );
    }

    #[test]
    fn render_produces_the_driver_schema() {
        let xml = render(&modes_for_client(1668, 2420, 120), None);
        assert!(!xml.contains("<gpu>"), "the driver chooses without a name");
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
        assert!(xml.contains("<vdd_settings>"));
        assert!(xml.contains("<count>1</count>"));
        assert!(xml.contains("<g_refresh_rate>120</g_refresh_rate>"));
        assert!(xml.contains(
            "<width>2420</width>\n            <height>1668</height>\n            \
             <refresh_rate>120</refresh_rate>"
        ));
        assert!(xml.ends_with("</vdd_settings>\n"));
    }

    #[test]
    fn render_names_the_main_screens_graphics_card() {
        let xml = render(
            &modes_for_client(1668, 2420, 120),
            Some("Intel(R) Iris(R) Xe Graphics"),
        );
        assert!(xml.contains(
            "    <gpu>\n        <friendlyname>Intel(R) Iris(R) Xe Graphics</friendlyname>\n    </gpu>\n"
        ));
        // Before the modes, as in the driver's own file.
        assert!(xml.find("<gpu>") < xml.find("<resolutions>"));
        let xml = render(&modes_for_client(1668, 2420, 60), Some("A&B <GPU>"));
        assert!(xml.contains("<friendlyname>A&amp;B &lt;GPU&gt;</friendlyname>"));
    }

    /// The vendor's own file, as its installer leaves it (shortened).
    const VENDOR: &str = "<?xml version='1.0' encoding='utf-8'?>\r\n<vdd_settings>\r\n    <monitors>\r\n        <count>1</count>\r\n    </monitors>\r\n    <gpu>\r\n        <friendlyname>default</friendlyname>\r\n    </gpu>\r\n\t<global>\r\n\t\t<g_refresh_rate>60</g_refresh_rate>\r\n\t</global>\r\n\t<logging>\r\n\t\t<logging>true</logging>\r\n\t</logging>\r\n</vdd_settings>\r\n";

    #[test]
    fn the_main_screens_card_is_pinned_unless_its_name_is_shared() {
        let adapters = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let hybrid = adapters(&[
            "Intel(R) Iris(R) Xe Graphics",
            "NVIDIA GeForce RTX 3050 Ti Laptop GPU",
        ]);
        assert_eq!(
            gpu_to_pin(Some("Intel(R) Iris(R) Xe Graphics"), &hybrid),
            Ok("Intel(R) Iris(R) Xe Graphics")
        );
        // A 0.3.1 host listed "AMD Radeon 780M Graphics" twice; the driver
        // would take whichever it enumerates first.
        let twins = adapters(&["AMD Radeon 780M Graphics", "AMD Radeon 780M Graphics"]);
        assert!(gpu_to_pin(Some("AMD Radeon 780M Graphics"), &twins).is_err());
        assert!(gpu_to_pin(None, &hybrid).is_err());
        assert!(gpu_to_pin(Some(""), &hybrid).is_err());
    }

    #[test]
    fn with_matching_off_only_the_gpu_name_changes() {
        let pinned = with_gpu(VENDOR, "Intel(R) Iris(R) Xe Graphics").unwrap();
        assert_eq!(
            pinned,
            VENDOR.replace(
                "<friendlyname>default</friendlyname>",
                "<friendlyname>Intel(R) Iris(R) Xe Graphics</friendlyname>"
            )
        );
        // A file without a <gpu> element gets one under the root.
        let bare = "<vdd_settings>\n    <monitors>\n        <count>1</count>\n    </monitors>\n</vdd_settings>\n";
        assert_eq!(
            with_gpu(bare, "A&B").unwrap(),
            "<vdd_settings>\n    <gpu>\n        <friendlyname>A&amp;B</friendlyname>\n    </gpu>\n    <monitors>\n        <count>1</count>\n    </monitors>\n</vdd_settings>\n"
        );
        assert_eq!(with_gpu("<other/>", "x"), None);
    }

    #[test]
    fn the_card_goes_inside_a_compact_root() {
        // Sol's case: no newline inside the root, one after it.
        let compact = "<vdd_settings><monitors><count>1</count></monitors></vdd_settings>\n";
        assert_eq!(
            with_gpu(compact, "X").as_deref(),
            Some("<vdd_settings>\n    <gpu>\n        <friendlyname>X</friendlyname>\n    </gpu><monitors><count>1</count></monitors></vdd_settings>\n")
        );
    }

    #[test]
    fn a_gpu_element_without_a_name_gets_one() {
        assert_eq!(
            with_gpu("<vdd_settings>\n    <gpu>\n    </gpu>\n</vdd_settings>\n", "X").as_deref(),
            Some("<vdd_settings>\n    <gpu>\n        <friendlyname>X</friendlyname>\n    </gpu>\n</vdd_settings>\n")
        );
        assert_eq!(
            with_gpu("<vdd_settings><gpu/></vdd_settings>", "X").as_deref(),
            Some("<vdd_settings><gpu><friendlyname>X</friendlyname></gpu></vdd_settings>")
        );
        assert_eq!(
            with_gpu(
                "<vdd_settings><gpu><friendlyname/></gpu></vdd_settings>",
                "X"
            )
            .as_deref(),
            Some("<vdd_settings><gpu><friendlyname>X</friendlyname></gpu></vdd_settings>")
        );
    }

    #[test]
    fn comments_attributes_line_endings_and_bom_are_kept() {
        let commented = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!-- e.g. <friendlyname>NVIDIA</friendlyname> -->\n<vdd_settings version=\"2\">\n    <gpu enabled=\"yes\">\n        <friendlyname note=\"x\">A&amp;B</friendlyname> <!-- the card -->\n    </gpu>\n</vdd_settings>\n";
        assert_eq!(
            with_gpu(commented, "Intel(R) Iris(R) Xe Graphics"),
            Some(commented.replace(">A&amp;B<", ">Intel(R) Iris(R) Xe Graphics<"))
        );
        let crlf = "<vdd_settings>\r\n\t<monitors>\r\n\t</monitors>\r\n</vdd_settings>\r\n";
        assert_eq!(
            with_gpu(crlf, "X").as_deref(),
            Some("<vdd_settings>\r\n    <gpu>\r\n        <friendlyname>X</friendlyname>\r\n    </gpu>\r\n\t<monitors>\r\n\t</monitors>\r\n</vdd_settings>\r\n")
        );
        let bom = format!("\u{feff}{VENDOR}");
        assert_eq!(with_gpu(&bom, "X"), Some(bom.replace(">default<", ">X<")));
    }

    #[test]
    fn a_file_that_does_not_parse_as_expected_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vdd_settings.xml");
        for document in [
            "<vdd_settings><monitors></vdd_settings>",
            "<vdd_settings><count>1</count>",
            "<vdd_settings></vdd_settings>\n<gpu><friendlyname>x</friendlyname></gpu>\n",
            "<vdd_settings></vdd_settings>junk",
            "<vdd_settings><friendlyname>x</friendlyname></vdd_settings>",
            "<vdd_settings><gpu><friendlyname>a</friendlyname></gpu><gpu><friendlyname>b</friendlyname></gpu></vdd_settings>",
            "<vdd_settings><gpu><friendlyname>a<b/></friendlyname></gpu></vdd_settings>",
            "<other><gpu><friendlyname>a</friendlyname></gpu></other>",
            "",
        ] {
            assert_eq!(with_gpu(document, "X"), None, "{document}");
            std::fs::write(&path, document).unwrap();
            assert!(pin_gpu_in(&path, "X").is_err(), "{document}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), document, "changed: {document}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn documents_expat_rejects_are_left_alone() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-strict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vdd_settings.xml");
        let name = "<gpu><friendlyname>default</friendlyname></gpu>";
        for document in [
            // Sol's four: each was rewritten, and Expat rejected the result.
            format!("<vdd_settings version=2>{name}</vdd_settings>\n"),
            format!("<vdd_settings a=\"1\" a=\"2\">{name}</vdd_settings>\n"),
            format!("<vdd_settings>{name}</vdd_settings>\n<!DOCTYPE vdd_settings>\n"),
            format!("<vdd_settings>{name}<count>\u{1}</count></vdd_settings>\n"),
            // More of the same kind.
            format!("<vdd_settings>\n<?xml version=\"1.0\"?>{name}</vdd_settings>"),
            format!("<!DOCTYPE a><!DOCTYPE b><vdd_settings>{name}</vdd_settings>"),
            format!("<vdd_settings>{name}<count>&nbsp;</count></vdd_settings>"),
            format!("<vdd_settings>{name}<count>&#1;</count></vdd_settings>"),
            format!("<vdd_settings x=\"&bogus;\">{name}</vdd_settings>"),
            format!("<vdd_settings>{name}<!-- a -- b --></vdd_settings>"),
            format!("<vdd_settings>{name}<count>]]></count></vdd_settings>"),
            format!("<vdd_settings>{name}<1count/></vdd_settings>"),
        ] {
            assert_eq!(with_gpu(&document, "X"), None, "{document:?}");
            std::fs::write(&path, &document).unwrap();
            assert!(pin_gpu_in(&path, "X").is_err(), "{document:?}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                document,
                "changed: {document:?}"
            );
        }
        // A name that is not valid XML is not written either.
        let vendor = with_gpu(VENDOR, "Bad\u{1}Card");
        assert_eq!(vendor, None);
        // A DOCTYPE in the prolog and references XML defines are fine.
        let fine = format!("<?xml version=\"1.0\"?>\n<!DOCTYPE vdd_settings>\n<vdd_settings a=\"&amp;\">{name}<count>&#65;&lt;</count></vdd_settings>\n");
        assert_eq!(with_gpu(&fine, "X"), Some(fine.replace(">default<", ">X<")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_pin_is_corrected_and_a_current_one_left_alone() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vdd_settings.xml");
        // Pinned earlier to a card that no longer drives the main screen.
        let stale = with_gpu(VENDOR, "AMD Radeon(TM) Graphics").unwrap();
        std::fs::write(&path, &stale).unwrap();
        assert!(pin_gpu_in(&path, "NVIDIA GeForce RTX 5080").unwrap());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            with_gpu(VENDOR, "NVIDIA GeForce RTX 5080").unwrap()
        );
        assert!(
            !pin_gpu_in(&path, "NVIDIA GeForce RTX 5080").unwrap(),
            "no rewrite"
        );
        // No file: none is made, so the driver keeps its built-in modes.
        let missing = dir.join("missing.xml");
        assert!(pin_gpu_in(&missing, "NVIDIA GeForce RTX 5080").is_err());
        assert!(!missing.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_is_atomic_and_creates_the_directory() {
        let dir = std::env::temp_dir().join(format!("vdd-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("vdd_settings.xml");
        write_to(&path, &modes_for_client(1668, 2420, 120), None).expect("write settings");
        let written = std::fs::read_to_string(&path).expect("read back");
        assert!(written.contains("<width>2420</width>"));
        assert!(!path.with_extension("xml.tmp").exists(), "tmp cleaned up");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
