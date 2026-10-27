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

/// When the requested mode cannot be set (the driver offers only its own
/// list), the largest offered size that fits inside it, so the screen is
/// as sharp as it can be instead of staying at 800x600.
pub fn best_offered(requested: VddMode, offered: &[(u32, u32)]) -> Option<(u32, u32)> {
    offered
        .iter()
        .copied()
        .filter(|&(w, h)| w <= requested.width && h <= requested.height)
        .max_by_key(|&(w, h)| u64::from(w) * u64::from(h))
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
/// screen. `adapters` are in DXGI order, as the driver enumerates them. The
/// driver matches by description and takes the first match, so a name
/// several adapters share is pinned only when the first of them is the one
/// with screens and the others have none: Windows lists some laptops'
/// integrated card twice. Otherwise the error says why for the log.
pub fn gpu_to_pin<'a>(
    main_screen: Option<&'a str>,
    adapters: &[crate::vdd_snapshot::Adapter],
) -> Result<&'a str, &'static str> {
    let name = main_screen
        .filter(|name| !name.is_empty())
        .ok_or("no adapter drives the main screen")?;
    let mut named = adapters
        .iter()
        .filter(|adapter| same_card(&adapter.description, name));
    if let Some(first) = named.next() {
        let mut rest = named.peekable();
        if rest.peek().is_some() && (first.outputs == 0 || rest.any(|a| a.outputs > 0)) {
            return Err("two graphics cards have the same name");
        }
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

/// Elements carry no attributes: neither this host nor the driver's own
/// file writes any, so a file with one is not a file this host edits.
fn check_tag(tag: &quick_xml::events::BytesStart) -> Result<(), &'static str> {
    if !xml_name(tag.name().as_ref()) {
        return Err("an element name is not valid XML");
    }
    if !tag.attributes_raw().iter().all(u8::is_ascii_whitespace) {
        return Err("an element has attributes, which this file never has");
    }
    Ok(())
}

/// The declaration this host writes or the driver ships:
/// `<?xml version="1.0"( encoding="utf-8|UTF-8")?( standalone="yes|no")??>`,
/// with either quote.
fn xml_declaration(raw: &str) -> bool {
    let attribute =
        |name: &str, value: &str| [format!(" {name}=\"{value}\""), format!(" {name}='{value}'")];
    let optional = |name: &str, values: &[&str]| {
        std::iter::once(String::new())
            .chain(values.iter().flat_map(|value| attribute(name, value)))
            .collect::<Vec<_>>()
    };
    let encodings = optional("encoding", &["utf-8", "UTF-8"]);
    let standalones = optional("standalone", &["yes", "no"]);
    attribute("version", "1.0").iter().any(|version| {
        encodings.iter().any(|encoding| {
            standalones
                .iter()
                .any(|standalone| raw == format!("<?xml{version}{encoding}{standalone}?>"))
        })
    })
}

/// Find the render adapter's place by parsing `xml`. Only the shapes this
/// host writes and the driver ships are edited: the declaration above at
/// the start, comments, whitespace, elements without attributes, and text
/// with only predefined entities and legal character references. Anything
/// else is refused, including a DOCTYPE, processing instructions, CDATA and
/// characters XML does not allow; so is anything the driver could read
/// differently from this host: a name outside `<gpu>`, a second name or
/// `<gpu>`, markup inside the name, or text or elements outside the
/// `<vdd_settings>` root. The driver matches `friendlyname` by local name
/// anywhere.
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
            Event::Start(_) | Event::Empty(_) if in_name => {
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
            Event::GeneralRef(_) if outside => {
                return Err("there is text outside the root element")
            }
            // Only the very first thing in the file may be the declaration.
            Event::Decl(_) if before != 0 || !xml_declaration(&xml[before..after]) => {
                return Err("the XML declaration is not one this file has")
            }
            Event::Decl(_) => {}
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
            Event::Text(_) | Event::Comment(_) => {}
            Event::Eof => break,
            // A DOCTYPE, processing instructions, CDATA, or anything newer.
            _ => return Err("the file uses XML this host does not edit"),
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

/// Whether two graphics card names are one card to the driver, which
/// compares them ignoring case.
pub fn same_card(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// A name that leaves the card to the driver: none, empty or `default`.
fn driver_chooses(name: &str) -> bool {
    let name = name.trim();
    name.is_empty() || name.eq_ignore_ascii_case("default")
}

/// The graphics card the document names, `None` when it leaves the card to
/// the driver. `Err` when the document is not one this host edits.
pub fn named_gpu(xml: &str) -> Result<Option<String>, &'static str> {
    let body = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    match gpu_slot(body)? {
        GpuSlot::Name(range) => {
            let text = quick_xml::escape::unescape(&body[range])
                .map_err(|_| "the graphics card name is not valid XML text")?;
            Ok((!driver_chooses(&text)).then(|| text.trim().to_string()))
        }
        _ => Ok(None),
    }
}

/// What to do to the settings file's graphics card name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinChange {
    /// Name this card.
    Write(String),
    /// Take back this host's earlier name: the driver chooses again.
    Reset,
    /// Nothing to change.
    Leave,
    /// Another app or the user named a card; it stays, and the driver
    /// renders there.
    Refuse(&'static str),
}

/// Why a card someone else named stays.
pub const NAMED_ELSEWHERE: &str = "the settings file names a card this host did not";

/// Whose name the file may hold: this host replaces a name only when it
/// installed the driver (`owned`), when the file leaves the card to the
/// driver, or when the name is one it recorded writing (`ours`). Other apps
/// share this driver and its file. With no card to name (`wanted` is
/// `None`), this host's own earlier name is taken back, so the file never
/// keeps a pin the log says was not made.
pub fn pin_change(
    named: Option<&str>,
    wanted: Option<&str>,
    owned: bool,
    ours: &[&str],
) -> PinChange {
    let is_ours = |name: &str| ours.iter().any(|ours| same_card(ours, name));
    match (wanted, named) {
        (Some(card), Some(name)) if same_card(card, name) => PinChange::Leave,
        (Some(_), Some(name)) if !owned && !is_ours(name) => PinChange::Refuse(NAMED_ELSEWHERE),
        (Some(card), _) => PinChange::Write(card.to_string()),
        (None, Some(name)) if is_ours(name) => PinChange::Reset,
        (None, _) => PinChange::Leave,
    }
}

/// What `apply_pin` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    Named,
    Unchanged,
    Reset,
    Kept(&'static str),
}

/// Whether Setup installed the virtual display driver beside the app in
/// `app_dir`, and it is still that install. Setup records the driver's
/// uninstaller and version in this marker (installer/EternalMonitor.iss);
/// another app that replaced or upgraded the driver since changed its
/// registration, and the marker no longer counts, as in
/// installer/scripts/vdd-driver-remove.ps1.
pub fn driver_owned(app_dir: &Path) -> bool {
    let marker = app_dir
        .join("driver")
        .join("installed-by-eternalmonitor.txt");
    let Ok(marker) = std::fs::read_to_string(marker) else {
        return false;
    };
    driver_registration()
        .is_some_and(|(location, version)| marker_matches(&marker, &location, &version))
}

/// Whether the marker records the driver registered at `location` with
/// `version`: its uninstaller's path and the version on two lines, compared
/// ignoring case as PowerShell's `-eq` does in the scripts.
fn marker_matches(marker: &str, location: &str, version: &str) -> bool {
    let location = location.strip_suffix('\\').unwrap_or(location);
    let expected = format!("{location}\\unins000.exe\r\n{version}");
    marker.to_lowercase() == expected.to_lowercase()
}

/// Where the driver's own setup registered it, and its version, as the
/// scripts read it: the first uninstall entry with both.
#[cfg(windows)]
fn driver_registration() -> Option<(String, String)> {
    [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1",
    ]
    .into_iter()
    .find_map(|key| {
        let value = |name| crate::diagnostics::registry_value(key, name);
        Some((value("InstallLocation")?, value("DisplayVersion")?))
    })
}

#[cfg(not(windows))]
fn driver_registration() -> Option<(String, String)> {
    None
}

/// Remember the cards this host may have written into the settings file
/// last, one per line, or that it wrote none. The record is replaced as the
/// settings file is, so a crash or power cut leaves the old or the new one.
pub fn remember_pins(record: &Path, names: &[&str]) -> io::Result<()> {
    if names.is_empty() {
        return match std::fs::remove_file(record) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    if let Some(parent) = record.parent() {
        std::fs::create_dir_all(parent)?;
    }
    replace(record, &names.join("\n"))
}

/// Make the settings file at `path` name `wanted`, changing only the card,
/// under the rules of `pin_change`. `record` holds the names this host
/// recorded writing. A missing file stays missing, and one that does not
/// parse as expected is left alone; the error says why.
pub fn apply_pin(
    path: &Path,
    record: &Path,
    owned: bool,
    wanted: Option<&str>,
) -> io::Result<PinOutcome> {
    let invalid = |reason| io::Error::new(io::ErrorKind::InvalidData, reason);
    let current = std::fs::read_to_string(path)?;
    let named = named_gpu(&current).map_err(invalid)?;
    let ours = recorded_pins(record);
    let ours: Vec<&str> = ours.iter().map(String::as_str).collect();
    let (name, outcome) = match pin_change(named.as_deref(), wanted, owned, &ours) {
        PinChange::Leave => return Ok(PinOutcome::Unchanged),
        PinChange::Refuse(reason) => return Ok(PinOutcome::Kept(reason)),
        PinChange::Write(card) => (card, PinOutcome::Named),
        PinChange::Reset => ("default".to_string(), PinOutcome::Reset),
    };
    let updated = gpu_edit(&current, &name).map_err(invalid)?;
    let new_record = (outcome == PinOutcome::Named).then_some(name.as_str());
    record_around(record, new_record, &ours, || replace(path, &updated))?;
    Ok(outcome)
}

/// `write` the settings file, `new` taking the place of `ours` as the names
/// this host wrote, with the record true wherever a crash or power cut
/// stops it: until the file is replaced it may hold either, so the record
/// holds both; once it is, only `new`. With `new` as `None` the record
/// keeps `ours` until the file no longer names them. When the write fails,
/// the record goes back to `ours`.
fn record_around(
    record: &Path,
    new: Option<&str>,
    ours: &[&str],
    write: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let mut both = ours.to_vec();
    both.extend(new.filter(|new| !ours.iter().any(|name| same_card(name, new))));
    let grew = both.len() > ours.len();
    if grew {
        remember_pins(record, &both)?;
    }
    write().inspect_err(|_| {
        if grew {
            let _ = remember_pins(record, ours);
        }
    })?;
    // A record left holding both is still true.
    let _ = remember_pins(record, new.as_slice());
    Ok(())
}

/// The names this host recorded writing: one, or more after a write that
/// a crash or power cut stopped.
fn recorded_pins(record: &Path) -> Vec<String> {
    let Ok(ours) = std::fs::read_to_string(record) else {
        return Vec::new();
    };
    ours.lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(String::from)
        .collect()
}

/// Write the whole settings file at `path` with `modes`, naming `wanted`
/// under the rules of `pin_change`: a card someone else named stays named
/// in the new file. A missing file names no card. A file this host cannot
/// read may name anyone's card, so it is replaced only where any name may
/// be, on a driver Setup installed and with a card to name; otherwise it is
/// left as it is and the error says why. `record` holds the names this
/// host recorded writing.
pub fn write_with_pin(
    path: &Path,
    record: &Path,
    owned: bool,
    modes: &[VddMode],
    wanted: Option<&str>,
) -> io::Result<PinOutcome> {
    let named = match std::fs::read_to_string(path) {
        Ok(current) => match named_gpu(&current) {
            Ok(named) => named,
            Err(_) if owned && wanted.is_some() => None,
            Err(reason) => return Err(io::Error::new(io::ErrorKind::InvalidData, reason)),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let ours = recorded_pins(record);
    let ours: Vec<&str> = ours.iter().map(String::as_str).collect();
    let (name, outcome) = match pin_change(named.as_deref(), wanted, owned, &ours) {
        PinChange::Leave => (named, PinOutcome::Unchanged),
        PinChange::Refuse(reason) => (named, PinOutcome::Kept(reason)),
        PinChange::Write(card) => (Some(card), PinOutcome::Named),
        PinChange::Reset => (None, PinOutcome::Reset),
    };
    let write = || write_to(path, modes, name.as_deref());
    match outcome {
        PinOutcome::Named => record_around(record, name.as_deref(), &ours, write)?,
        PinOutcome::Reset => record_around(record, None, &ours, write)?,
        _ => write()?,
    }
    Ok(outcome)
}

/// What the snapshot reports as asked of the driver after `apply_pin`: the
/// card the file names for this host, or why it names none.
pub fn pin_result(
    wanted: Result<&str, &'static str>,
    applied: &io::Result<PinOutcome>,
) -> Result<String, &'static str> {
    match (wanted, applied) {
        (Err(reason), _) => Err(reason),
        (Ok(_), Ok(PinOutcome::Kept(reason))) => Err(reason),
        (Ok(_), Err(_)) => Err("the settings file was left as it is"),
        (Ok(card), Ok(_)) => Ok(card.to_string()),
    }
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
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
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
    fn without_the_ipads_mode_the_largest_size_that_fits_is_used() {
        // The driver's own list, as its installer leaves it.
        let vendor = [
            (800, 600),
            (1366, 768),
            (1920, 1080),
            (2560, 1440),
            (3840, 2160),
        ];
        let ipad = |w, h| VddMode {
            width: w,
            height: h,
            refresh_hz: 60,
        };
        assert_eq!(best_offered(ipad(2048, 1536), &vendor), Some((1920, 1080)));
        assert_eq!(best_offered(ipad(2752, 2064), &vendor), Some((2560, 1440)));
        assert_eq!(best_offered(ipad(640, 480), &vendor), None);
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

    /// Adapters in DXGI order, as (description, vendor:device, outputs).
    fn cards(list: &[(&str, (u32, u32), u32)]) -> Vec<crate::vdd_snapshot::Adapter> {
        list.iter()
            .enumerate()
            .map(
                |(index, &(name, (vendor_id, device_id), outputs))| crate::vdd_snapshot::Adapter {
                    luid: 0x1000 + index as u64,
                    description: name.into(),
                    vendor_id,
                    device_id,
                    driver: String::new(),
                    outputs,
                },
            )
            .collect()
    }

    #[test]
    fn the_main_screens_card_is_pinned_unless_its_name_is_shared() {
        let hybrid = cards(&[
            ("Intel(R) Iris(R) Xe Graphics", (0x8086, 0x46a6), 1),
            ("NVIDIA GeForce RTX 3050 Ti Laptop GPU", (0x10de, 0x25a0), 0),
        ]);
        assert_eq!(
            gpu_to_pin(Some("Intel(R) Iris(R) Xe Graphics"), &hybrid),
            Ok("Intel(R) Iris(R) Xe Graphics")
        );
        // A 0.3.1 host listed "AMD Radeon 780M Graphics" twice; the driver
        // would take whichever it enumerates first.
        let twins = cards(&[
            ("AMD Radeon 780M Graphics", (0x1002, 0x15bf), 0),
            ("AMD Radeon 780M Graphics", (0x1002, 0x15bf), 0),
        ]);
        assert!(gpu_to_pin(Some("AMD Radeon 780M Graphics"), &twins).is_err());
        assert!(gpu_to_pin(None, &hybrid).is_err());
        assert!(gpu_to_pin(Some(""), &hybrid).is_err());
    }

    /// An Intel HD 630 + GTX 1050 laptop on 0.3.2 listed its Intel card
    /// twice with the virtual display off. Only the first drives the panel,
    /// and the driver takes the first match, so the name is pinned.
    #[test]
    fn a_card_listed_twice_is_pinned_when_only_the_first_drives_a_screen() {
        const INTEL: &str = "Intel(R) HD Graphics 630";
        let listed = |outputs: [u32; 2]| {
            cards(&[
                (INTEL, (0x8086, 0x591b), outputs[0]),
                ("NVIDIA GeForce GTX 1050", (0x10de, 0x1c8d), 0),
                (INTEL, (0x8086, 0x591b), outputs[1]),
            ])
        };
        assert_eq!(gpu_to_pin(Some(INTEL), &listed([1, 0])), Ok(INTEL));
        // The driver would find the one without screens first.
        assert!(gpu_to_pin(Some(INTEL), &listed([0, 1])).is_err());
        // Both drive screens, or neither is known to: no telling which.
        assert!(gpu_to_pin(Some(INTEL), &listed([1, 1])).is_err());
        assert!(gpu_to_pin(Some(INTEL), &listed([0, 0])).is_err());
        // The driver matches names ignoring case, so a listing in another
        // case is the same card.
        assert!(gpu_to_pin(Some("intel(r) hd graphics 630"), &listed([0, 1])).is_err());
        // The duplicate does not stop a pin to the other card.
        assert_eq!(
            gpu_to_pin(Some("NVIDIA GeForce GTX 1050"), &listed([1, 0])),
            Ok("NVIDIA GeForce GTX 1050")
        );
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

    /// 0.3.2 rewrote the file on every connect without a `<gpu>` element.
    /// After an upgrade, with matching off, only the card is added.
    #[test]
    fn a_file_written_by_0_3_2_gets_the_card_and_keeps_its_modes() {
        let v032 = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<vdd_settings>\n    <monitors>\n        <count>1</count>\n    </monitors>\n    <global>\n        <g_refresh_rate>60</g_refresh_rate>\n    </global>\n    <resolutions>\n        <resolution>\n            <width>2160</width>\n            <height>1620</height>\n            <refresh_rate>60</refresh_rate>\n        </resolution>\n    </resolutions>\n</vdd_settings>\n";
        assert_eq!(
            with_gpu(v032, "Intel(R) HD Graphics 630").as_deref(),
            Some(
                v032.replacen(
                    "<vdd_settings>\n",
                    "<vdd_settings>\n    <gpu>\n        <friendlyname>Intel(R) HD Graphics 630</friendlyname>\n    </gpu>\n",
                    1
                )
                .as_str()
            )
        );
    }

    #[test]
    fn a_name_is_replaced_only_when_this_host_may() {
        use PinChange::*;
        const INTEL: &str = "Intel(R) HD Graphics 630";
        const OTHER: &str = "NVIDIA GeForce GTX 1050";
        let write = || Write(INTEL.to_string());
        // The driver chooses: this host may name a card, owned or not.
        for owned in [false, true] {
            assert_eq!(pin_change(None, Some(INTEL), owned, &[]), write());
        }
        // A card another app or the user named stays on a driver Setup did
        // not install, and is replaced on one it did.
        assert_eq!(
            pin_change(Some(OTHER), Some(INTEL), false, &[]),
            Refuse(NAMED_ELSEWHERE)
        );
        assert_eq!(
            pin_change(Some(OTHER), Some(INTEL), false, &[INTEL]),
            Refuse(NAMED_ELSEWHERE)
        );
        assert_eq!(pin_change(Some(OTHER), Some(INTEL), true, &[]), write());
        // This host's own earlier name, in any case, is replaced.
        assert_eq!(
            pin_change(
                Some("nvidia geforce gtx 1050"),
                Some(INTEL),
                false,
                &[OTHER]
            ),
            write()
        );
        // Already named, ignoring case.
        assert_eq!(
            pin_change(Some("intel(r) hd graphics 630"), Some(INTEL), false, &[]),
            Leave
        );
        // No card to name: this host's earlier name is taken back, anyone
        // else's stays.
        assert_eq!(pin_change(Some(INTEL), None, false, &[INTEL]), Reset);
        assert_eq!(pin_change(Some(INTEL), None, true, &[]), Leave);
        assert_eq!(pin_change(None, None, false, &[INTEL]), Leave);
    }

    #[test]
    fn the_named_card_is_read_back_as_the_driver_reads_it() {
        let file = |name: &str| VENDOR.replace("default", name);
        assert_eq!(named_gpu(VENDOR), Ok(None));
        assert_eq!(named_gpu(&file(" DEFAULT ")), Ok(None));
        assert_eq!(named_gpu(&file("")), Ok(None));
        assert_eq!(named_gpu(&file("A&amp;B")), Ok(Some("A&B".to_string())));
        assert_eq!(named_gpu("<vdd_settings></vdd_settings>"), Ok(None));
        assert!(named_gpu("<other/>").is_err());
    }

    #[test]
    fn apply_pin_records_its_own_name_and_takes_it_back() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (path, record) = (
            dir.join("vdd_settings.xml"),
            dir.join("app").join("vdd-pin.txt"),
        );
        let named = || named_gpu(&std::fs::read_to_string(&path).unwrap()).unwrap();
        const INTEL: &str = "Intel(R) HD Graphics 630";

        // The vendor's file, on a driver another app installed.
        std::fs::write(&path, VENDOR).unwrap();
        assert_eq!(
            apply_pin(&path, &record, false, Some(INTEL)).unwrap(),
            PinOutcome::Named
        );
        assert_eq!(named().as_deref(), Some(INTEL));
        assert_eq!(std::fs::read_to_string(&record).unwrap(), INTEL);
        assert_eq!(
            apply_pin(&path, &record, false, Some(INTEL)).unwrap(),
            PinOutcome::Unchanged
        );
        // The pin is refused later: the name this host wrote goes, and so
        // does the record.
        assert_eq!(
            apply_pin(&path, &record, false, None).unwrap(),
            PinOutcome::Reset
        );
        assert_eq!(named(), None);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("<friendlyname>default</friendlyname>"));
        assert!(!record.exists());

        // A card the user or another app named stays, and is reported.
        let theirs = VENDOR.replace("default", "NVIDIA GeForce GTX 1050");
        std::fs::write(&path, &theirs).unwrap();
        let applied = apply_pin(&path, &record, false, Some(INTEL));
        assert_eq!(
            applied.as_ref().unwrap(),
            &PinOutcome::Kept(NAMED_ELSEWHERE)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), theirs);
        assert_eq!(pin_result(Ok(INTEL), &applied), Err(NAMED_ELSEWHERE));
        assert_eq!(
            apply_pin(&path, &record, false, None).unwrap(),
            PinOutcome::Unchanged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), theirs);
        // Setup installed this driver: the main screen's card wins.
        assert_eq!(
            apply_pin(&path, &record, true, Some(INTEL)).unwrap(),
            PinOutcome::Named
        );
        assert_eq!(named().as_deref(), Some(INTEL));
        assert_eq!(
            pin_result(Ok(INTEL), &Ok(PinOutcome::Named)),
            Ok(INTEL.to_string())
        );

        // A missing file stays missing.
        std::fs::remove_file(&path).unwrap();
        let applied = apply_pin(&path, &record, true, Some(INTEL));
        assert!(applied.is_err() && !path.exists());
        assert_eq!(
            pin_result(Ok(INTEL), &applied),
            Err("the settings file was left as it is")
        );
        assert_eq!(
            pin_result(
                Err("no adapter drives the main screen"),
                &Ok(PinOutcome::Unchanged)
            ),
            Err("no adapter drives the main screen")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Match the iPad's resolution rewrites the whole file, under the same
    /// rule: on a driver Setup did not install, a card the user or another
    /// app named stays named.
    #[test]
    fn a_whole_file_rewrite_keeps_a_card_someone_else_named() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-rewrite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (path, record) = (
            dir.join("vdd_settings.xml"),
            dir.join("app").join("vdd-pin.txt"),
        );
        let named = || named_gpu(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let modes = default_modes();
        const INTEL: &str = "Intel(R) HD Graphics 630";
        const OTHER: &str = "NVIDIA GeForce GTX 1050";

        // No file yet: this host names the card and records it.
        assert_eq!(
            write_with_pin(&path, &record, false, &modes, Some(INTEL)).unwrap(),
            PinOutcome::Named
        );
        assert_eq!(named().as_deref(), Some(INTEL));
        assert_eq!(std::fs::read_to_string(&record).unwrap(), INTEL);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("<width>1920</width>"));
        // No card to name later: its own name goes, and so does the record.
        assert_eq!(
            write_with_pin(&path, &record, false, &modes, None).unwrap(),
            PinOutcome::Reset
        );
        assert_eq!(named(), None);
        assert!(!record.exists());

        // Someone else's card stays, with or without a card to name.
        std::fs::write(&path, VENDOR.replace("default", OTHER)).unwrap();
        let written = write_with_pin(&path, &record, false, &modes, Some(INTEL));
        assert_eq!(
            written.as_ref().unwrap(),
            &PinOutcome::Kept(NAMED_ELSEWHERE)
        );
        assert_eq!(named().as_deref(), Some(OTHER));
        assert_eq!(pin_result(Ok(INTEL), &written), Err(NAMED_ELSEWHERE));
        assert_eq!(
            write_with_pin(&path, &record, false, &modes, None).unwrap(),
            PinOutcome::Unchanged
        );
        assert_eq!(named().as_deref(), Some(OTHER));
        assert!(!record.exists());
        // On a driver Setup installed, the main screen's card wins.
        assert_eq!(
            write_with_pin(&path, &record, true, &modes, Some(INTEL)).unwrap(),
            PinOutcome::Named
        );
        assert_eq!(named().as_deref(), Some(INTEL));

        // A file that does not read as expected is replaced, naming the
        // card, only on a driver Setup installed.
        std::fs::write(&path, "<other/>").unwrap();
        assert_eq!(
            write_with_pin(&path, &record, true, &modes, Some(INTEL)).unwrap(),
            PinOutcome::Named
        );
        assert_eq!(named().as_deref(), Some(INTEL));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review round 3, defect 2: a card named in a way this host does not
    /// read (here CDATA, which the driver reads) counted as no card, and
    /// Match the iPad's resolution replaced it on a driver another app
    /// installed.
    #[test]
    fn a_whole_file_rewrite_keeps_a_file_it_cannot_read() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-unread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (path, record) = (
            dir.join("vdd_settings.xml"),
            dir.join("app").join("vdd-pin.txt"),
        );
        let modes = default_modes();
        const INTEL: &str = "Intel(R) HD Graphics 630";
        let cdata = VENDOR.replace(">default<", "><![CDATA[NVIDIA GeForce GTX 1050]]><");
        for document in [cdata.as_str(), "<other/>"] {
            for wanted in [Some(INTEL), None] {
                std::fs::write(&path, document).unwrap();
                let written = write_with_pin(&path, &record, false, &modes, wanted);
                assert!(written.is_err(), "{document:?} {wanted:?}");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), document);
                assert_eq!(
                    pin_result(Ok(INTEL), &written),
                    Err("the settings file was left as it is")
                );
                assert!(!record.exists());
            }
            // Nor is it replaced without a card to name on a driver Setup
            // installed: the name in it may not be this host's.
            let written = write_with_pin(&path, &record, true, &modes, None);
            assert!(written.is_err(), "{document:?}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), document);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review round 3, defect 4: the record named the new card before the
    /// file was replaced. When the replacement failed, the file still held
    /// this host's earlier name, which the record no longer did, so every
    /// later attempt refused to replace it.
    #[test]
    fn a_failed_write_keeps_this_hosts_earlier_name_its_own() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-failed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (path, record) = (
            dir.join("vdd_settings.xml"),
            dir.join("app").join("vdd-pin.txt"),
        );
        let named = || named_gpu(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // A directory where the new file is written first makes it fail.
        let blocker = path.with_extension("xml.tmp");
        let modes = default_modes();
        const INTEL: &str = "Intel(R) HD Graphics 630";
        const AMD: &str = "AMD Radeon(TM) Graphics";
        type Pin<'a> = dyn Fn(Option<&str>) -> io::Result<PinOutcome> + 'a;
        let edit: &Pin = &|wanted| apply_pin(&path, &record, false, wanted);
        let rewrite: &Pin = &|wanted| write_with_pin(&path, &record, false, &modes, wanted);
        for pin in [edit, rewrite] {
            std::fs::write(&path, VENDOR).unwrap();
            assert_eq!(pin(Some(INTEL)).unwrap(), PinOutcome::Named);
            for wanted in [Some(AMD), None] {
                std::fs::create_dir(&blocker).unwrap();
                assert!(pin(wanted).is_err(), "{wanted:?}");
                std::fs::remove_dir(&blocker).unwrap();
                assert_eq!(named().as_deref(), Some(INTEL));
                assert_eq!(std::fs::read_to_string(&record).unwrap(), INTEL);
            }
            // The next attempt replaces it.
            assert_eq!(pin(Some(AMD)).unwrap(), PinOutcome::Named);
            assert_eq!(named().as_deref(), Some(AMD));
            assert_eq!(std::fs::read_to_string(&record).unwrap(), AMD);
            assert_eq!(pin(None).unwrap(), PinOutcome::Reset);
            assert_eq!(named(), None);
            assert!(!record.exists());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Run a pin that a crash or power cut stops just before the settings
    /// file is replaced: what stays on disk is the record as it stood then,
    /// and the old file.
    fn crash_before_replace(record: &Path, new: Option<&str>) {
        let ours = recorded_pins(record);
        let ours: Vec<&str> = ours.iter().map(String::as_str).collect();
        let mut on_disk = None;
        record_around(record, new, &ours, || {
            on_disk = Some(std::fs::read_to_string(record).ok());
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_file(record);
        if let Some(names) = on_disk.unwrap() {
            std::fs::write(record, names).unwrap();
        }
    }

    /// Review P3-1: the record named the new card before the settings file
    /// held it, so a crash or power cut between the two left this host's own
    /// name looking like another app's, and later attempts kept it.
    #[test]
    fn a_crash_before_a_new_name_is_written_keeps_the_old_one_this_hosts() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (path, record) = (
            dir.join("vdd_settings.xml"),
            dir.join("app").join("vdd-pin.txt"),
        );
        let named = || named_gpu(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let modes = default_modes();
        const INTEL: &str = "Intel(R) HD Graphics 630";
        const AMD: &str = "AMD Radeon(TM) Graphics";
        type Pin<'a> = dyn Fn(Option<&str>) -> io::Result<PinOutcome> + 'a;
        let edit: &Pin = &|wanted| apply_pin(&path, &record, false, wanted);
        let rewrite: &Pin = &|wanted| write_with_pin(&path, &record, false, &modes, wanted);
        for pin in [edit, rewrite] {
            std::fs::write(&path, VENDOR).unwrap();
            assert_eq!(pin(Some(INTEL)).unwrap(), PinOutcome::Named);
            crash_before_replace(&record, Some(AMD));
            assert_eq!(named().as_deref(), Some(INTEL));
            // The next run still knows the name the file holds as its own.
            assert_eq!(pin(Some(AMD)).unwrap(), PinOutcome::Named);
            assert_eq!(named().as_deref(), Some(AMD));
            // Once the file is replaced, the record holds only the new name.
            assert_eq!(std::fs::read_to_string(&record).unwrap(), AMD);
            crash_before_replace(&record, Some(INTEL));
            assert_eq!(pin(None).unwrap(), PinOutcome::Reset);
            assert_eq!(named(), None);
            assert!(!record.exists());
        }
        // Neither file is left half written beside the real one.
        assert!(!record.with_extension("txt.tmp").exists());
        assert!(!path.with_extension("xml.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review P3-1: taking the name back deleted the record before the
    /// settings file was replaced, so a crash or power cut between the two
    /// left the file naming a card this host no longer knew it wrote.
    #[test]
    fn a_crash_before_the_name_is_taken_back_keeps_it_this_hosts() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (path, record) = (
            dir.join("vdd_settings.xml"),
            dir.join("app").join("vdd-pin.txt"),
        );
        let named = || named_gpu(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let modes = default_modes();
        const INTEL: &str = "Intel(R) HD Graphics 630";
        const AMD: &str = "AMD Radeon(TM) Graphics";
        type Pin<'a> = dyn Fn(Option<&str>) -> io::Result<PinOutcome> + 'a;
        let edit: &Pin = &|wanted| apply_pin(&path, &record, false, wanted);
        let rewrite: &Pin = &|wanted| write_with_pin(&path, &record, false, &modes, wanted);
        for pin in [edit, rewrite] {
            std::fs::write(&path, VENDOR).unwrap();
            assert_eq!(pin(Some(INTEL)).unwrap(), PinOutcome::Named);
            crash_before_replace(&record, None);
            assert_eq!(named().as_deref(), Some(INTEL));
            // Still this host's: taken back on the next run.
            assert_eq!(pin(None).unwrap(), PinOutcome::Reset);
            assert_eq!(named(), None);
            assert!(!record.exists());
            // And replaced by another card.
            assert_eq!(pin(Some(INTEL)).unwrap(), PinOutcome::Named);
            crash_before_replace(&record, None);
            assert_eq!(pin(Some(AMD)).unwrap(), PinOutcome::Named);
            assert_eq!(named().as_deref(), Some(AMD));
            assert_eq!(pin(None).unwrap(), PinOutcome::Reset);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review round 3, defect 3: the marker counted whatever it said, after
    /// another app replaced or upgraded the driver.
    #[test]
    fn only_setups_marker_for_the_registered_driver_makes_it_this_hosts() {
        let dir = std::env::temp_dir().join(format!("vdd-owned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("driver")).unwrap();
        assert!(!driver_owned(&dir));
        // A marker for no driver registered here.
        std::fs::write(
            dir.join("driver").join("installed-by-eternalmonitor.txt"),
            "C:\\Elsewhere\\unins000.exe\r\n0.0.0",
        )
        .unwrap();
        assert!(!driver_owned(&dir));
        std::fs::remove_dir_all(&dir).unwrap();

        // What Setup writes, for the install the scripts find.
        const LOCATION: &str = "C:\\VirtualDisplayDriver";
        let setup = "C:\\VirtualDisplayDriver\\unins000.exe\r\n25.5.2";
        assert!(marker_matches(setup, LOCATION, "25.5.2"));
        assert!(marker_matches(
            setup,
            "C:\\VirtualDisplayDriver\\",
            "25.5.2"
        ));
        assert!(marker_matches(&setup.to_uppercase(), LOCATION, "25.5.2"));
        // Upgraded, or installed again elsewhere, by another app.
        assert!(!marker_matches(setup, LOCATION, "25.7.23"));
        assert!(!marker_matches(setup, "D:\\Tools\\VDD", "25.5.2"));
        assert!(!marker_matches("x", LOCATION, "25.5.2"));
        assert!(!marker_matches(&format!("{setup}\r\n"), LOCATION, "25.5.2"));
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
    fn comments_line_endings_and_bom_are_kept() {
        let commented = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!-- e.g. <friendlyname>NVIDIA</friendlyname> -->\n<vdd_settings>\n    <gpu>\n        <friendlyname>A&amp;B</friendlyname> <!-- the card -->\n    </gpu>\n</vdd_settings>\n";
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
        // References XML defines are fine.
        let fine = format!("<?xml version=\"1.0\"?>\n<vdd_settings>{name}<count>&#65;&lt;</count></vdd_settings>\n");
        assert_eq!(with_gpu(&fine, "X"), Some(fine.replace(">default<", ">X<")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The driver's own vdd_settings.xml from Virtual-Display-Driver 25.5.2
    /// (MIT), byte for byte: single-quoted declaration, many comments.
    const VENDOR_FILE: &str = include_str!("../tests/fixtures/vdd_settings-25.5.2.xml");

    #[test]
    fn only_the_shapes_this_host_and_the_driver_write_are_edited() {
        let dir = std::env::temp_dir().join(format!("vdd-pin-allow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vdd_settings.xml");
        let name = "<gpu><friendlyname>default</friendlyname></gpu>";
        for document in [
            // Sol's four: each was rewritten.
            format!("<vdd_settings a=\"<\">{name}</vdd_settings>\n"),
            format!("<?xml?>\n<vdd_settings>{name}</vdd_settings>\n"),
            format!("<!DOCTYPE vdd_settings [<!ENTITY>]>\n<vdd_settings>{name}</vdd_settings>\n"),
            format!("<?1pi data?>\n<vdd_settings>{name}</vdd_settings>\n"),
            // Everything outside the allow-list.
            format!(
                "<vdd_settings>\n<gpu enabled=\"yes\">{}</gpu></vdd_settings>",
                "<friendlyname>default</friendlyname>"
            ),
            format!("<!DOCTYPE vdd_settings>\n<vdd_settings>{name}</vdd_settings>"),
            format!("<?xml-stylesheet href=\"a.css\"?>\n<vdd_settings>{name}</vdd_settings>"),
            format!("<vdd_settings>{name}<?pi x?></vdd_settings>"),
            format!("<vdd_settings>{name}<count><![CDATA[1]]></count></vdd_settings>"),
            format!("<?xml version=\"1.1\"?>\n<vdd_settings>{name}</vdd_settings>"),
            format!(
                "<?xml version=\"1.0\" encoding=\"latin1\"?>\n<vdd_settings>{name}</vdd_settings>"
            ),
            format!(
                "<?xml version=\"1.0\"  encoding=\"utf-8\"?>\n<vdd_settings>{name}</vdd_settings>"
            ),
            format!("<?xml version=\"1.0' ?>\n<vdd_settings>{name}</vdd_settings>"),
            format!(" <?xml version=\"1.0\"?>\n<vdd_settings>{name}</vdd_settings>"),
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
        // Every declaration shape the allow-list names, and none.
        for declaration in [
            "",
            "<?xml version=\"1.0\"?>\n",
            "<?xml version='1.0' encoding='utf-8'?>\n",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<?xml version=\"1.0\" standalone='no'?>\n",
        ] {
            let document = format!("{declaration}<vdd_settings>{name}</vdd_settings>\n");
            assert_eq!(
                with_gpu(&document, "X"),
                Some(document.replace(">default<", ">X<")),
                "{document:?}"
            );
        }
        // The driver's own file is edited in place, and so is this host's.
        assert_eq!(
            with_gpu(VENDOR_FILE, "Intel(R) Iris(R) Xe Graphics"),
            Some(VENDOR_FILE.replace(
                "<friendlyname>default</friendlyname>",
                "<friendlyname>Intel(R) Iris(R) Xe Graphics</friendlyname>"
            ))
        );
        std::fs::write(&path, VENDOR_FILE).unwrap();
        assert!(pin_gpu_in(&path, "X").unwrap());
        let ours = render(&modes_for_client(1668, 2420, 120), Some("A"));
        assert_eq!(with_gpu(&ours, "B"), Some(ours.replace(">A<", ">B<")));
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
