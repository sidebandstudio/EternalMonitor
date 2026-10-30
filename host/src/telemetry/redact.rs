//! What uploads leave out: the Windows profile folder, which is a user name,
//! the user name, the PC's name and the names owners give their devices.
//! Support identifies a PC by its installation id. The session log and
//! "Copy logs" keep them all.
//!
//! - A profile path under any root (`C:\`, `\\server\share\`, `\\?\`,
//!   `\\?\UNC\`), with either separator or Debug-doubled backslashes: this
//!   user's profile (`%USERPROFILE%`, which may be outside `Users`) becomes
//!   `%USERPROFILE%`; another profile's folder under `Users` becomes
//!   `%USERNAME%`. `Public`, `Default`, `Default User` and `All Users` stay.
//! - The user name and the PC name, as whole words in any case, become
//!   `%USERNAME%` and `%COMPUTERNAME%`. A PC name ends at any character
//!   outside `A-Z`, `a-z`, `0-9` and `-`, so `USB_DESKTOP-DEMO_Audio` loses
//!   it and `DESKTOP-OTHER` keeps it.
//! - Names owners give their devices, which this run saw: each audio output
//!   the host opens (`Headphones (Maria's AirPods)`) becomes its kind
//!   (`<headphones>`, else `<audio-device>`) anywhere in a line, and each
//!   iPad's name becomes `<device-name>` unless it is Apple's own `iPad` or
//!   `iPhone`. In the lines that log one, `device=` loses its value the same
//!   way even if the name was never seen. The value ends at the field the log
//!   site writes after it, so a name with `owner=` inside goes whole.
//! - What stays: the line's time and level; field names (`name=`); the
//!   values of diagnostic fields and hardware descriptions, outside paths;
//!   names that are common words (`Intel`, `display`, `DESKTOP`), except as
//!   a profile folder; and names shorter than three characters, except as a
//!   whole path segment. None of those identify anyone, and replacing them
//!   would corrupt the values support reads.
//!
//! The scan works on characters and never slices at a computed index, so no
//! line can make it panic.

use once_cell::sync::Lazy;
use parking_lot::RwLock;

/// Folders under `Users` that name nobody. Support uses them.
const SHARED_PROFILES: [&str; 4] = ["Default User", "All Users", "Default", "Public"];

/// Windows local profile folder names are short: a user name of at most 20
/// characters plus a suffix such as `.DOMAIN` or `.000`.
const LOCAL_FOLDER_CHARS: usize = 32;

/// Shorter names are replaced only as a whole path segment: replacing every
/// `a` or `pc` would garble the line and hide little.
const MIN_NAME_CHARS: usize = 3;

/// Names that identify nobody on their own and are also hardware, vendor or
/// UI words in the log. As a user or PC name they are replaced only as a
/// profile folder.
const COMMON_WORDS: &[&str] = &[
    "user",
    "users",
    "admin",
    "administrator",
    "owner",
    "guest",
    "default",
    "public",
    "home",
    "pc",
    "desktop",
    "laptop",
    "notebook",
    "computer",
    "workstation",
    "server",
    "display",
    "monitor",
    "screen",
    "intel",
    "nvidia",
    "amd",
    "radeon",
    "geforce",
    "microsoft",
    "windows",
    "apple",
    "qualcomm",
    "realtek",
];

/// Fields whose values are tokens, numbers or hardware descriptions. Outside
/// paths, a name inside their value stays.
const DIAGNOSTIC_KEYS: &[&str] = &[
    // session_summary v=1.
    "v",
    "role",
    "session_id",
    "nonce",
    "transport",
    "mode",
    "stage",
    "outcome",
    "failure",
    "fallback",
    "end",
    "ttff_ms",
    "duration_ms",
    "os",
    "hw",
    "gpu",
    "hybrid",
    // Enumerated and numeric fields the host logs.
    "second_screen_fallback",
    "block",
    "reason",
    "codec",
    "encoder",
    "input",
    "hevc",
    "cpu_fallback",
    "resolution",
    "backend",
    "backends",
    "phase",
    "status",
    "code",
    "health",
    "call",
    "stall",
    "stall_refused",
    "stall_ended",
    "avoided",
    "attached",
    "advertised_formats",
    "usb_state",
    "wire_stream",
    "running",
    "version",
    "build",
    "slow",
    "changed",
    "remote_config",
    "rollout",
    "ss_rule",
    "problem",
    "requested",
    "preferred",
    "using",
    "from",
    "to",
    "arch",
    "installation_type",
    // Troubleshooting and the graphics card switch.
    "overrides",
    "encoder_input",
    "packet",
    "fps",
    "window",
    "graphics_card",
    "preference",
    "switch",
    "choice",
    // The startup network line.
    "route",
    // The virtual display snapshot.
    "driver",
    "tech",
    "vdd",
    "src",
    "tgt",
    "hags",
    "settings_path",
    "paths",
    "pin_matched",
    "pin_reason",
    "render_is_pin",
    // Hardware descriptions.
    "adapter",
    "vendor",
    "display",
    "vdd_device",
    "pin",
    "render",
    "registry_product",
    "identifier",
    "adapters",
    "outputs",
    "screen_gpu",
    "app_gpu",
    "power_saving_gpu",
    "high_performance_gpu",
    // The periodic stats summary.
    "stream_codec",
    "stream_input",
    "stream_resolution",
    "stream_usb_state",
    "stream_running",
    "stream_hevc",
    "stream_cpu_fallback",
];

/// Messages that describe hardware: the rest of the line after the label,
/// or the named field.
const DESCRIPTIONS: [(&str, Option<&str>); 4] = [
    ("GPU:", None),
    ("Encoder:", None),
    ("Capture:", None),
    ("Found GPU adapter", Some("name")),
];

/// Messages whose `device=` is a name its owner may have chosen, and the
/// field their log site writes after it (`None`: it is the last field).
const DEVICE_NAMES: [(&str, Device, Option<&str>); 5] = [
    // `audio/wasapi.rs`.
    ("WASAPI loopback active", Device::Audio, Some("rate")),
    // `audio/stage.rs`.
    ("Audio stream stats", Device::Audio, Some("kbps")),
    ("PC audio session started", Device::Audio, Some("bitrate")),
    // `transport/session.rs`.
    ("Client session established", Device::Client, Some("screen")),
    ("Client liveness expired", Device::Client, None),
];

/// Device names kept for uploads. Each makes every line's scan a little
/// longer, and an iPad picks its own.
const MAX_DEVICES: usize = 32;

#[derive(Clone, Copy)]
enum Device {
    /// A Windows audio endpoint, `Description (device)`: `Speakers (Realtek(R)
    /// Audio)`, `Headphones (Maria's AirPods)`.
    Audio,
    /// The name the iPad sends in its HELLO.
    Client,
}

/// An audio endpoint's kind, by the words Windows and drivers put in its
/// name: the description before ` (` first, then the whole name.
const AUDIO_KINDS: [(&[&str], &str); 4] = [
    (
        &["headphone", "headset", "earphone", "earbud"],
        "<headphones>",
    ),
    (&["speaker"], "<speakers>"),
    (&["bluetooth", "hands-free"], "<bluetooth>"),
    (
        &[
            "hdmi",
            "displayport",
            "display audio",
            "nvidia high definition",
            "amd high definition",
        ],
        "<hdmi>",
    ),
];
const AUDIO_DEVICE: &str = "<audio-device>";

/// iPadOS 16 and later send these unless the app may read the user's name.
const GENERIC_CLIENT_NAMES: [&str; 2] = ["iPad", "iPhone"];
const DEVICE_NAME: &str = "<device-name>";

/// The log's repeat count (`crate::log_repeat`) follows a line's last field.
const REPEATED: &str = " [repeated ";

const USERPROFILE: &str = "%USERPROFILE%";
const USERNAME: &str = "%USERNAME%";
const COMPUTERNAME: &str = "%COMPUTERNAME%";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    User,
    Pc,
}

struct Word {
    name: Vec<char>,
    kind: Kind,
    /// Shorter than [`MIN_NAME_CHARS`].
    short: bool,
}

pub(super) struct Names {
    /// This user's profile path without its root (`Users`, `Maria Lopez`).
    profile: Vec<Vec<char>>,
    /// Folder names that may end a profile segment without a separator:
    /// the profile's folder, the user name and `%USERNAME%`, longest first.
    folders: Vec<Vec<char>>,
    /// The user and PC names that may be replaced, longest first.
    words: Vec<Word>,
    /// Device names this run saw, and their labels, longest first.
    devices: Vec<(Vec<char>, &'static str)>,
}

impl Names {
    pub(super) fn new(profile: Option<&str>, user: Option<&str>, pc: Option<&str>) -> Self {
        fn clean(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|value| !value.is_empty())
        }
        let profile = clean(profile).map_or_else(Vec::new, path_parts);
        let profile = if profile.len() >= 2 {
            profile
        } else {
            Vec::new()
        };
        let (user, pc) = (clean(user), clean(pc));
        let mut folders: Vec<Vec<char>> = profile
            .last()
            .cloned()
            .into_iter()
            .chain(user.map(|user| user.chars().collect()))
            .chain([USERNAME.chars().collect()])
            .filter(|name: &Vec<char>| {
                let name: String = name.iter().collect();
                !SHARED_PROFILES
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(&name))
            })
            .collect();
        folders.sort_by_key(|name| std::cmp::Reverse(name.len()));
        let mut words: Vec<Word> = [(pc, Kind::Pc), (user, Kind::User)]
            .into_iter()
            .filter_map(|(name, kind)| Some((name?, kind)))
            .filter(|(name, _)| !COMMON_WORDS.iter().any(|w| w.eq_ignore_ascii_case(name)))
            .map(|(name, kind)| Word {
                short: name.chars().count() < MIN_NAME_CHARS,
                name: name.chars().collect(),
                kind,
            })
            .collect();
        words.sort_by_key(|word| std::cmp::Reverse(word.name.len()));
        Self {
            profile,
            folders,
            words,
            devices: Vec::new(),
        }
    }

    /// From now on, `name` becomes `label` anywhere in a line. A `?`-logged
    /// field writes it escaped and quoted, so that form goes too.
    fn note_device(&mut self, name: &str, label: &'static str) {
        let name = name.trim();
        let labels = AUDIO_KINDS
            .iter()
            .map(|(_, kind)| *kind)
            .chain([AUDIO_DEVICE, DEVICE_NAME]);
        let generic = GENERIC_CLIENT_NAMES
            .iter()
            .chain(COMMON_WORDS)
            .copied()
            .chain(labels.map(|label| label.trim_matches(['<', '>'])))
            .any(|word| word.eq_ignore_ascii_case(name));
        if generic || name.chars().count() < MIN_NAME_CHARS {
            return;
        }
        let debug = format!("{name:?}");
        let escaped = debug
            .strip_prefix('"')
            .and_then(|debug| debug.strip_suffix('"'))
            .unwrap_or(name);
        for form in [name, escaped] {
            let form: Vec<char> = form.chars().collect();
            let known = self
                .devices
                .iter()
                .any(|(seen, _)| seen.len() == form.len() && name_at(seen, 0, &form).is_some());
            if !known && self.devices.len() < MAX_DEVICES {
                self.devices.push((form, label));
            }
        }
        self.devices
            .sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    }

    /// This PC's names, from its environment.
    fn current() -> Self {
        let var = |name| std::env::var(name).ok();
        Self::new(
            var("USERPROFILE").as_deref(),
            var("USERNAME").as_deref(),
            var("COMPUTERNAME").as_deref(),
        )
    }

    /// Where this user's profile path ends if it starts at `i`.
    fn profile_at(&self, chars: &[char], i: usize) -> Option<usize> {
        let mut end = i;
        for (index, part) in self.profile.iter().enumerate() {
            if index > 0 {
                end = end
                    .checked_add(seps(chars, end))
                    .filter(|&after| after > end)?;
            }
            end = name_at(chars, end, part)?;
        }
        // A longer folder that starts with this name is another folder.
        (!self.profile.is_empty() && !at(chars, end).is_some_and(continues_name)).then_some(end)
    }

    /// Whether `folder` is this user's profile folder.
    fn is_own_folder(&self, folder: &[char]) -> bool {
        self.profile
            .last()
            .is_some_and(|own| own.len() == folder.len() && name_at(folder, 0, own).is_some())
    }
}

pub(super) static NAMES: Lazy<RwLock<Names>> = Lazy::new(|| RwLock::new(Names::current()));

/// Uploads name the audio output `name`, the host's to open, by its kind.
/// Call it before logging the name.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn note_audio_device(name: &str) {
    NAMES.write().note_device(name, audio_kind(name));
}

/// Uploads leave out `name`, an iPad's from its HELLO, unless Apple's
/// generic one. Call it before logging the name.
pub(crate) fn note_client_device(name: &str) {
    NAMES.write().note_device(name, DEVICE_NAME);
}

/// `line` without the names in `names`. A log line's time and level stay.
pub(super) fn redact(line: &str, names: &Names) -> String {
    let (head, message) = match crate::log_repeat::split_stamp(line) {
        Some((_, rest)) => {
            let message = rest.trim_start().split_once(' ').map_or("", |(_, m)| m);
            line.split_at_checked(line.len().saturating_sub(message.len()))
                .unwrap_or(("", line))
        }
        None => ("", line),
    };
    let chars: Vec<char> = message.chars().collect();
    let fields = Fields::of(&chars);
    let mut edits = devices(&chars, names, &fields);
    // The line's `device=` value takes in any device name it overlaps.
    if let Some(mut line) = device(&chars) {
        edits.retain(|edit| {
            let apart = edit.end <= line.start || line.end <= edit.start;
            if !apart {
                line.start = line.start.min(edit.start);
                line.end = line.end.max(edit.end);
            }
            apart
        });
        edits.push(line);
        edits.sort_by_key(|edit| edit.start);
    }
    // A profile inside a device name goes with it; one that runs into a
    // device name ends there.
    let found: Vec<Edit> = profiles(&chars, names)
        .into_iter()
        .filter_map(|mut found| {
            for edit in &edits {
                if edit.start <= found.start && found.start < edit.end {
                    return None;
                }
                if found.start < edit.start && edit.start < found.end {
                    found.end = edit.start;
                }
            }
            Some(found)
        })
        .collect();
    edits.extend(found);
    edits.sort_by_key(|edit| edit.start);
    let found = words(&chars, names, &edits, &fields);
    edits.extend(found);
    edits.sort_by_key(|edit| edit.start);
    let mut out = String::with_capacity(line.len());
    out.push_str(head);
    let mut copied = 0;
    for edit in edits {
        if edit.start < copied || edit.end < edit.start {
            continue;
        }
        out.extend(chars.get(copied..edit.start).unwrap_or_default());
        out.push_str(edit.token);
        copied = edit.end;
    }
    out.extend(chars.get(copied..).unwrap_or_default());
    out
}

/// `chars[start..end]` becomes `token`.
struct Edit {
    start: usize,
    end: usize,
    token: &'static str,
}

fn at(chars: &[char], i: usize) -> Option<char> {
    chars.get(i).copied()
}

fn before(chars: &[char], i: usize) -> Option<char> {
    at(chars, i.checked_sub(1)?)
}

fn is_sep(c: char) -> bool {
    matches!(c, '\\' | '/')
}

/// A separator, a character Windows forbids in file names (`: * ? " < > |`)
/// or `'`: a path segment ends there.
fn delimits(c: char) -> bool {
    is_sep(c) || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\'')
}

fn ends_segment(c: char) -> bool {
    delimits(c) || c.is_whitespace()
}

/// Whether `c` continues a name or path segment.
fn continues_name(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '%')
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// How many characters from `i` match `test`.
fn count(chars: &[char], i: usize, test: impl Fn(char) -> bool) -> usize {
    chars
        .get(i..)
        .map_or(0, |rest| rest.iter().take_while(|&&c| test(c)).count())
}

fn seps(chars: &[char], i: usize) -> usize {
    count(chars, i, is_sep)
}

/// `name` at `i`, in any case: where it ends.
fn name_at(chars: &[char], i: usize, name: &[char]) -> Option<usize> {
    let end = i.checked_add(name.len())?;
    let found = chars.get(i..end)?;
    found
        .iter()
        .zip(name)
        .all(|(&a, &b)| a == b || a.to_lowercase().eq(b.to_lowercase()))
        .then_some(end)
}

/// A path's parts after its root: `\\?\C:\Users\maria` is `Users`, `maria`.
fn path_parts(path: &str) -> Vec<Vec<char>> {
    let chars: Vec<char> = path.chars().collect();
    let mut i = seps(&chars, 0);
    if matches!(at(&chars, i), Some('?' | '.')) && at(&chars, i + 1).is_some_and(is_sep) {
        i += 1 + seps(&chars, i + 1);
    }
    if at(&chars, i).is_some_and(|c| c.is_ascii_alphabetic()) && at(&chars, i + 1) == Some(':') {
        i += 2;
    }
    chars
        .get(i..)
        .unwrap_or_default()
        .split(|&c| is_sep(c))
        .filter(|part| !part.is_empty())
        .map(<[char]>::to_vec)
        .collect()
}

/// If a path starts at `i`, where its first separator is: after a drive
/// (`C:\`), after a `\\?\` or `\\.\` prefix (and its drive), or a run of
/// separators that starts a word (`\\server`, `\Users`).
fn root(chars: &[char], i: usize) -> Option<usize> {
    let drive = |j: usize| {
        (at(chars, j).is_some_and(|c| c.is_ascii_alphabetic())
            && at(chars, j + 1) == Some(':')
            && at(chars, j + 2).is_some_and(is_sep))
        .then_some(j + 2)
    };
    let c = at(chars, i)?;
    let prev = before(chars, i);
    if c.is_ascii_alphabetic() {
        return drive(i)
            .filter(|_| !prev.is_some_and(|p| p.is_alphanumeric() || p == '_' || p == '%'));
    }
    let word = prev.is_some_and(|p| {
        p.is_alphanumeric() || is_sep(p) || matches!(p, '_' | '.' | '-' | '%' | '$' | '~' | ':')
    });
    // A device label stands for a name that may have ended in a word, so
    // the next pass reads the same.
    let label = || {
        let labels = AUDIO_KINDS.iter().map(|(_, kind)| *kind);
        labels.chain([AUDIO_DEVICE, DEVICE_NAME]).any(|label| {
            let label: Vec<char> = label.chars().collect();
            i.checked_sub(label.len())
                .is_some_and(|start| name_at(chars, start, &label).is_some())
        })
    };
    if !is_sep(c) || word || (prev == Some('>') && label()) {
        return None;
    }
    let after = i + seps(chars, i);
    if after >= i + 2
        && matches!(at(chars, after), Some('?' | '.'))
        && at(chars, after + 1).is_some_and(is_sep)
    {
        let prefix = after + 1 + seps(chars, after + 1);
        return Some(drive(prefix).unwrap_or(after + 1));
    }
    Some(i)
}

/// Where the segment that starts at `i` ends. A `%` ends it too, so a path
/// that runs into a replaced name reads the same in every pass.
fn segment_end(chars: &[char], i: usize) -> usize {
    i + count(chars, i, |c| !ends_segment(c) && c != '%')
}

/// Whether ` name=` follows the space at `i`.
fn field_follows(chars: &[char], i: usize) -> bool {
    let start = i + count(chars, i, char::is_whitespace);
    let first = at(chars, start).is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    first && at(chars, start + 1 + count(chars, start + 1, is_ident)) == Some('=')
}

/// Whether another path, or one replaced before, starts after the space at
/// `i`.
fn root_follows(chars: &[char], i: usize) -> bool {
    let start = i + count(chars, i, char::is_whitespace);
    let replaced: Vec<char> = USERPROFILE.chars().collect();
    start > i && (root(chars, start).is_some() || name_at(chars, start, &replaced).is_some())
}

/// Where the profile folder that starts at `start` ends. It runs to the next
/// separator, character Windows forbids in file names or quote; with none of
/// those, to a known name followed by a space or punctuation, else to the
/// next field or path if it is short enough to be a folder name, else to its
/// first space.
fn folder_end(chars: &[char], start: usize, names: &Names) -> usize {
    // A folder replaced before is that name alone.
    let replaced: Vec<char> = USERNAME.chars().collect();
    if let Some(end) = name_at(chars, start, &replaced) {
        return end;
    }
    let mut end = start;
    while let Some(c) = at(chars, end) {
        if delimits(c) {
            return end;
        }
        if c.is_whitespace() && (field_follows(chars, end) || root_follows(chars, end)) {
            break;
        }
        end += 1;
    }
    let shared = SHARED_PROFILES.iter().map(|name| name.chars().collect());
    let known = names
        .folders
        .iter()
        .cloned()
        .chain(shared)
        .find_map(|name| {
            name_at(chars, start, &name).filter(|&after| {
                after <= end
                    && at(chars, after)
                        .is_none_or(|c| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ')'))
            })
        });
    if let Some(known) = known {
        return known;
    }
    if end.saturating_sub(start) <= LOCAL_FOLDER_CHARS {
        return end;
    }
    start + count(chars, start, |c| !c.is_whitespace()).min(end.saturating_sub(start))
}

/// The edits to every profile path in `chars`, in order.
fn profiles(chars: &[char], names: &Names) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let Some(from) = root(chars, i) else {
            i += 1;
            continue;
        };
        // Scanning on after the folder, not after the path, finds a path
        // inside it (`\\server\C:\Users\x`) the same way in every pass.
        match profile(chars, i, from, names) {
            Some(edit) => {
                i = edit.end.max(i + 1);
                edits.push(edit);
            }
            None => i += 1,
        }
    }
    edits
}

/// The edit to the profile in the path rooted at `start` whose separators
/// begin at `from`.
fn profile(chars: &[char], start: usize, mut from: usize, names: &Names) -> Option<Edit> {
    loop {
        let segment = from + seps(chars, from);
        if segment == from {
            return None;
        }
        if let Some(end) = names.profile_at(chars, segment) {
            return Some(Edit {
                start,
                end,
                token: USERPROFILE,
            });
        }
        let end = segment_end(chars, segment);
        if end == segment {
            return None;
        }
        let users = chars.get(segment..end).is_some_and(|part| {
            part.len() == 5 && name_at(part, 0, &['u', 's', 'e', 'r', 's']).is_some()
        });
        let folder = end + seps(chars, end);
        if users && folder > end {
            let folder_end = folder_end(chars, folder, names);
            let name = chars.get(folder..folder_end).unwrap_or_default();
            let text: String = name.iter().collect();
            let shared = SHARED_PROFILES
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&text));
            return (!name.is_empty() && !shared).then(|| {
                if names.is_own_folder(name) {
                    Edit {
                        start,
                        end: folder_end,
                        token: USERPROFILE,
                    }
                } else {
                    Edit {
                        start: folder,
                        end: folder_end,
                        token: USERNAME,
                    }
                }
            });
        }
        from = end;
    }
}

/// Field names, and the values a name inside stays in.
struct Fields {
    names: Vec<(usize, usize)>,
    kept: Vec<(usize, usize)>,
}

impl Fields {
    fn of(chars: &[char]) -> Self {
        let mut names = Vec::new();
        // Fields after a space or at the start: `(name start, '=')`.
        let mut fields = Vec::new();
        for (equals, _) in chars.iter().enumerate().filter(|(_, &c)| c == '=') {
            let length = chars.get(..equals).map_or(0, |head| {
                head.iter().rev().take_while(|&&c| is_ident(c)).count()
            });
            let start = equals - length;
            if length == 0 {
                continue;
            }
            names.push((start, equals));
            if before(chars, start).is_none_or(char::is_whitespace) {
                fields.push((start, equals));
            }
        }
        let leading = count(chars, 0, char::is_whitespace);
        let description = DESCRIPTIONS.iter().find_map(|(label, field)| {
            let label: Vec<char> = label.chars().collect();
            Some((name_at(chars, leading, &label)?, *field))
        });
        let mut kept = Vec::new();
        if let Some((after, None)) = description {
            kept.push((after, chars.len()));
        }
        for (index, &(start, equals)) in fields.iter().enumerate() {
            let key: String = chars
                .get(start..equals)
                .unwrap_or_default()
                .iter()
                .collect();
            let end = fields.get(index + 1).map_or(chars.len(), |next| next.0);
            if DIAGNOSTIC_KEYS.contains(&key.as_str())
                || description.is_some_and(|(_, field)| field == Some(key.as_str()))
            {
                kept.push((equals + 1, end));
            }
        }
        Self { names, kept }
    }
}

/// Where `text` first starts in `chars` at or after `from`.
fn find(chars: &[char], from: usize, text: &str) -> Option<usize> {
    let text: Vec<char> = text.chars().collect();
    (from..chars.len()).find(|&i| chars.get(i..i + text.len()) == Some(&text[..]))
}

/// Where `text` last starts in `chars` at or after `from`.
fn rfind(chars: &[char], from: usize, text: &str) -> Option<usize> {
    let text: Vec<char> = text.chars().collect();
    (from..chars.len())
        .rev()
        .find(|&i| chars.get(i..i + text.len()) == Some(&text[..]))
}

/// The edit to the device name a message logs, if its owner may have chosen
/// it. The name runs from the first `device=` to the last occurrence of the
/// field after it, else to the repeat count or the end: a name holds any
/// text, `rate=` included, but the log site writes its own field last.
fn device(chars: &[char]) -> Option<Edit> {
    let leading = count(chars, 0, char::is_whitespace);
    let (_, kind, next) = DEVICE_NAMES.iter().find(|(message, _, _)| {
        let message: Vec<char> = message.chars().collect();
        name_at(chars, leading, &message).is_some()
    })?;
    let start = find(chars, leading, " device=")? + " device=".len();
    let repeated = (chars.last() == Some(&']'))
        .then(|| rfind(chars, start, REPEATED))
        .flatten();
    let end = next
        .and_then(|next| rfind(chars, start, &format!(" {next}=")))
        .or(repeated)
        .unwrap_or(chars.len());
    let name: String = chars.get(start..end)?.iter().collect();
    let token = match kind {
        Device::Audio => audio_kind(&name),
        Device::Client if GENERIC_CLIENT_NAMES.contains(&name.as_str()) => return None,
        Device::Client => DEVICE_NAME,
    };
    (start < end).then_some(Edit { start, end, token })
}

/// The device names this run saw, wherever they are in `chars`, in order.
/// A name stays only as a field name or part of a longer word.
fn devices(chars: &[char], names: &Names, fields: &Fields) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let found = names.devices.iter().find_map(|(name, label)| {
            let end = name_at(chars, i, name).filter(|&end| end > i)?;
            let joined = |inside: Option<&char>, outside: Option<char>| {
                inside.is_some_and(|&c| continues_name(c)) && outside.is_some_and(continues_name)
            };
            let word =
                joined(name.first(), before(chars, i)) || joined(name.last(), at(chars, end));
            let field = fields.names.iter().any(|&(s, e)| s <= i && end <= e);
            (!word && !field).then_some((end, *label))
        });
        match found {
            Some((end, token)) => {
                edits.push(Edit {
                    start: i,
                    end,
                    token,
                });
                i = end;
            }
            None => i += 1,
        }
    }
    edits
}

/// The label of the audio endpoint `name`.
fn audio_kind(name: &str) -> &'static str {
    let name = name.to_lowercase();
    let description = name.split(" (").next().unwrap_or_default();
    [description, name.as_str()]
        .iter()
        .find_map(|text| {
            AUDIO_KINDS
                .iter()
                .find(|(words, _)| words.iter().any(|word| text.contains(word)))
        })
        .map_or(AUDIO_DEVICE, |(_, kind)| kind)
}

/// The user and PC names in `chars` outside `taken`, in order.
fn words(chars: &[char], names: &Names, taken: &[Edit], fields: &Fields) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut taken = taken.iter().peekable();
    let mut i = 0;
    while i < chars.len() {
        if let Some(edit) = taken.peek() {
            if edit.end <= i {
                taken.next();
                continue;
            }
            if edit.start <= i {
                i = edit.end;
                continue;
            }
        }
        let limit = taken.peek().map_or(chars.len(), |edit| edit.start);
        let found = names.words.iter().find_map(|word| {
            let end = name_at(chars, i, &word.name).filter(|&end| end > i && end <= limit)?;
            replaces(chars, i, end, word, fields).then_some((end, word.kind))
        });
        match found {
            Some((end, kind)) => {
                let token = match kind {
                    Kind::User => USERNAME,
                    Kind::Pc => COMPUTERNAME,
                };
                edits.push(Edit {
                    start: i,
                    end,
                    token,
                });
                i = end;
            }
            None => i += 1,
        }
    }
    edits
}

/// Whether the name `word` at `chars[start..end]` is replaced.
fn replaces(chars: &[char], start: usize, end: usize, word: &Word, fields: &Fields) -> bool {
    let continues = |c: char| {
        c.is_alphanumeric()
            || c == '%'
            || match word.kind {
                Kind::User => c == '_',
                Kind::Pc => c == '-',
            }
    };
    if before(chars, start).is_some_and(continues) || at(chars, end).is_some_and(continues) {
        return false;
    }
    if fields.names.iter().any(|&(s, e)| s < end && start < e) {
        return false;
    }
    let in_path = before(chars, start).is_some_and(is_sep);
    if word.short {
        return in_path && at(chars, end).is_none_or(ends_segment);
    }
    in_path || !fields.kept.iter().any(|&(s, e)| s <= start && start < e)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: &str = "2026-10-29T17:46:01.123456Z";

    fn maria() -> Names {
        Names::new(
            Some(r"C:\Users\Maria Lopez"),
            Some("maria"),
            Some("MARIA-DESKTOP"),
        )
    }

    #[test]
    fn profile_path_forms_become_userprofile() {
        let names = maria();
        let cases = [
            (
                r"path=C:\Users\Maria Lopez\AppData\Roaming\EternalMonitor\logs\x.log",
                r"path=%USERPROFILE%\AppData\Roaming\EternalMonitor\logs\x.log",
            ),
            (
                r"dir=c:/users/maria lopez/Desktop next=1",
                r"dir=%USERPROFILE%/Desktop next=1",
            ),
            (
                r#"path="C:\\Users\\Maria Lopez\\AppData\\x" error=5"#,
                r#"path="%USERPROFILE%\\AppData\\x" error=5"#,
            ),
            (
                r"module=\\?\C:\Users\Maria Lopez\AppData\Local\x.dll",
                r"module=%USERPROFILE%\AppData\Local\x.dll",
            ),
            (
                r"home=C:\Users\Maria Lopez next=1",
                r"home=%USERPROFILE% next=1",
            ),
            (r"home=C:\Users\Maria Lopez", r"home=%USERPROFILE%"),
            // Other profiles on the PC keep all but their name.
            (
                r"Host panic at C:\Users\bob\.cargo\registry\src\renderer.rs:984:17:",
                r"Host panic at C:\Users\%USERNAME%\.cargo\registry\src\renderer.rs:984:17:",
            ),
            (r#"path='C:\Users\bob'"#, r#"path='C:\Users\%USERNAME%'"#),
            (
                r"home=C:\Users\bob next=1",
                r"home=C:\Users\%USERNAME% next=1",
            ),
            (
                r"module=\\?\D:\Users\Kim\x.dll",
                r"module=\\?\D:\Users\%USERNAME%\x.dll",
            ),
            // Shared folders name nobody.
            (
                r"path=C:\Users\Public\Documents\x",
                r"path=C:\Users\Public\Documents\x",
            ),
            (
                r"path=C:\Users\Default User\x",
                r"path=C:\Users\Default User\x",
            ),
            (r"member=BUILTIN\Users\group", r"member=BUILTIN\Users\group"),
            (
                r"path=C:\Program Files\EternalMonitor\eternal-host.exe",
                r"path=C:\Program Files\EternalMonitor\eternal-host.exe",
            ),
        ];
        let wrong: Vec<_> = cases
            .iter()
            .filter_map(|(text, uploaded)| {
                let got = redact(text, &names);
                (got != *uploaded).then(|| format!("{text}\n  want {uploaded}\n  got  {got}"))
            })
            .collect();
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    #[test]
    fn a_profile_outside_users_is_found_by_its_own_path() {
        let names = Names::new(Some(r"D:\Profiles\maria.CORP"), Some("maria"), None);
        for (text, uploaded) in [
            (
                r"settings=D:\Profiles\maria.CORP\AppData\Roaming\x.json",
                r"settings=%USERPROFILE%\AppData\Roaming\x.json",
            ),
            (r"dir=d:/profiles/MARIA.corp/x", r"dir=%USERPROFILE%/x"),
            (
                r"module=\\?\D:\Profiles\maria.CORP\x.dll",
                r"module=%USERPROFILE%\x.dll",
            ),
            // Another folder that starts with the same name.
            (
                r"dir=D:\Profiles\maria.CORPX\x",
                r"dir=D:\Profiles\%USERNAME%.CORPX\x",
            ),
        ] {
            assert_eq!(redact(text, &names), uploaded, "{text}");
        }
    }

    #[test]
    fn user_and_pc_names_become_their_variables() {
        let names = maria();
        for (text, uploaded) in [
            (
                "mDNS service registering host=MARIA-DESKTOP instance=EternalMonitor (MARIA-DESKTOP) port=9876",
                "mDNS service registering host=%COMPUTERNAME% instance=EternalMonitor (%COMPUTERNAME%) port=9876",
            ),
            (
                "Announce maria-desktop.local. on 192.0.2.1",
                "Announce %COMPUTERNAME%.local. on 192.0.2.1",
            ),
            // Part of a longer word is another word.
            ("user=mariadb mariana", "user=mariadb mariana"),
        ] {
            assert_eq!(redact(text, &names), uploaded, "{text}");
        }
    }

    #[test]
    fn device_names_become_their_kind() {
        // Not this PC's user: any owner's name goes.
        let names = Names::new(None, None, None);
        let wasapi = |device: &str| {
            format!("{STAMP}  INFO WASAPI loopback active device={device} rate=48000 channels=2")
        };
        for (device, kind) in [
            ("Headphones (Maria's AirPods)", "<headphones>"),
            ("Headset (Maria's AirPods Hands-Free)", "<headphones>"),
            ("Speakers (2- Maria's Bose Mini)", "<speakers>"),
            ("Speakers (Realtek(R) Audio)", "<speakers>"),
            ("Maria's Bluetooth Stereo", "<bluetooth>"),
            ("DELL U2720Q (NVIDIA High Definition Audio)", "<hdmi>"),
            ("Living Room TV (Intel(R) Display Audio)", "<hdmi>"),
            ("Maria's Studio (Focusrite USB Audio)", "<audio-device>"),
            ("Default PC output", "<audio-device>"),
        ] {
            assert_eq!(redact(&wasapi(device), &names), wasapi(kind), "{device}");
        }
        for (text, uploaded) in [
            (
                "Audio stream stats device=Headphones (Maria's AirPods) kbps=96.0 dropped=0",
                "Audio stream stats device=<headphones> kbps=96.0 dropped=0",
            ),
            // A frozen stream's repeat count uploads as it is.
            (
                "Audio stream stats device=Headphones (Maria's AirPods) kbps=0.0 wire_stream=3 \
                 [repeated 9 more times from 2026-10-29T17:46:01Z to 2026-10-29T17:46:10Z]",
                "Audio stream stats device=<headphones> kbps=0.0 wire_stream=3 \
                 [repeated 9 more times from 2026-10-29T17:46:01Z to 2026-10-29T17:46:10Z]",
            ),
            (
                "Client session established peer=192.0.2.7:50000 session_id=5 \
                 device=Maria's iPad Pro screen=2732x2048",
                "Client session established peer=192.0.2.7:50000 session_id=5 \
                 device=<device-name> screen=2732x2048",
            ),
            (
                "Client liveness expired — tearing session down peer=192.0.2.7:50000 \
                 device=Maria's iPad [repeated 1 more times from 1 to 2]",
                "Client liveness expired — tearing session down peer=192.0.2.7:50000 \
                 device=<device-name> [repeated 1 more times from 1 to 2]",
            ),
        ] {
            assert_eq!(redact(text, &names), uploaded, "{text}");
        }
        // With this PC's names known, the rest of the line still loses them.
        assert_eq!(
            redact(
                &format!("{STAMP}  INFO WASAPI loopback active device=Headphones (maria's AirPods) rate=48000 host=MARIA-DESKTOP"),
                &maria()
            ),
            format!("{STAMP}  INFO WASAPI loopback active device=<headphones> rate=48000 host=%COMPUTERNAME%")
        );
    }

    struct Captured(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The line `log` writes, formatted as the host's log is (`main.rs`).
    fn logged(log: impl FnOnce()) -> String {
        use tracing_subscriber::prelude::*;
        let out = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let writer = out.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(move || Captured(writer.clone())),
        );
        tracing::subscriber::with_default(subscriber, log);
        let line = String::from_utf8(out.lock().clone()).unwrap();
        line.trim_end().to_owned()
    }

    /// `audio/stage.rs` logs the name with `?`: quoted.
    #[test]
    fn the_audio_session_line_names_its_kind() {
        let name = "Headphones (Maria's AirPods)";
        let line = logged(|| {
            tracing::info!(
                session_id = 5u32,
                device = name,
                bitrate = 96_000u32,
                "PC audio session started (Opus, 48 kHz stereo, 20 ms)"
            )
        });
        assert!(line.contains(r#" device="Headphones (Maria's AirPods)" bitrate="#));
        assert_eq!(
            redact(&line, &Names::new(None, None, None)),
            line.replace(r#""Headphones (Maria's AirPods)""#, "<headphones>")
        );
    }

    /// A value ends at the field its log site writes next, at its last
    /// occurrence, not at the first `name=` inside it.
    #[test]
    fn a_device_name_with_fields_inside_goes_whole() {
        assert_cases(
            &Names::new(None, None, None),
            &[
                (
                    "WASAPI loopback active device=Headphones owner=Zoe Chen rate=48000 channels=2",
                    "WASAPI loopback active device=<headphones> rate=48000 channels=2",
                ),
                (
                    "WASAPI loopback active device=Speakers rate=0 (Zoe Chen) rate=48000 channels=2",
                    "WASAPI loopback active device=<speakers> rate=48000 channels=2",
                ),
                (
                    "Audio stream stats device=Zoe's Buds kbps=1 owner=Zoe kbps=96.0 dropped=0",
                    "Audio stream stats device=<audio-device> kbps=96.0 dropped=0",
                ),
                (
                    r#"PC audio session started (Opus, 48 kHz stereo, 20 ms) session_id=5 device="Zoe's owner=Zoe Chen Buds" bitrate=96000"#,
                    "PC audio session started (Opus, 48 kHz stereo, 20 ms) session_id=5 device=<audio-device> bitrate=96000",
                ),
                (
                    r#"Client session established peer=192.0.2.7:50000 session_id=5 device=Zoe's iPad owner=Zoe Chen screen="2732x2048" screen_points="1366x1024""#,
                    r#"Client session established peer=192.0.2.7:50000 session_id=5 device=<device-name> screen="2732x2048" screen_points="1366x1024""#,
                ),
                (
                    "Client liveness expired — tearing session down peer=192.0.2.7:50000 device=Zoe's iPad owner=Zoe Chen",
                    "Client liveness expired — tearing session down peer=192.0.2.7:50000 device=<device-name>",
                ),
                (
                    "Client liveness expired — tearing session down peer=192.0.2.7:50000 device=Zoe owner=Zoe Chen \
                     [repeated 3 more times from 2026-10-29T17:46:01Z to 2026-10-29T17:46:10Z]",
                    "Client liveness expired — tearing session down peer=192.0.2.7:50000 device=<device-name> \
                     [repeated 3 more times from 2026-10-29T17:46:01Z to 2026-10-29T17:46:10Z]",
                ),
            ],
        );
    }

    #[test]
    fn names_with_quotes_parentheses_and_unicode_go_whole() {
        let name = r#"Headphones (Zoë "Bean" Chen's Buds 🎧)"#;
        let mut names = Names::new(None, None, None);
        for line in [
            logged(
                || tracing::info!(device = %name, rate = 48_000u32, channels = 2u16, "WASAPI loopback active"),
            ),
            logged(|| {
                tracing::info!(
                    session_id = 5u32,
                    device = name,
                    bitrate = 96_000u32,
                    "PC audio session started (Opus, 48 kHz stereo, 20 ms)"
                )
            }),
        ] {
            let uploaded = redact(&line, &names);
            assert!(uploaded.contains(" device=<headphones> "), "{uploaded}");
            assert!(
                !uploaded.contains("Zoë") && !uploaded.contains("Bean"),
                "{uploaded}"
            );
        }
        // Seen once, the name goes from other lines too, in either form.
        names.note_device(name, audio_kind(name));
        let unrelated = [
            format!("Default PC audio endpoint changed from {name}"),
            logged(|| tracing::warn!(endpoint = name, "Could not open the PC audio output")),
        ];
        for line in unrelated {
            let uploaded = redact(&line, &names);
            assert!(uploaded.contains("<headphones>"), "{uploaded}");
            assert!(
                !uploaded.contains("Zoë") && !uploaded.contains("Bean"),
                "{uploaded}"
            );
        }
    }

    #[test]
    fn a_device_name_seen_this_run_goes_from_any_line() {
        let mut names = maria();
        names.note_device("Maria's iPad Pro", DEVICE_NAME);
        names.note_device("Kim", DEVICE_NAME);
        let speakers = "Speakers (2- Maria's Bose Mini)";
        names.note_device(speakers, audio_kind(speakers));
        // Generic, too short, a common word or a label: not a name.
        for generic in ["iPad", "IPHONE", "TV", "Monitor", "headphones", "  "] {
            names.note_device(generic, DEVICE_NAME);
        }
        assert_cases(
            &names,
            &[
                (
                    "Pairing approved for Maria's iPad Pro on MARIA-DESKTOP",
                    "Pairing approved for <device-name> on %COMPUTERNAME%",
                ),
                (
                    "Audio route speakers (2- maria's bose mini) muted",
                    "Audio route <speakers> muted",
                ),
                (
                    r#"error="Maria's iPad Pro went away" code=5"#,
                    r#"error="<device-name> went away" code=5"#,
                ),
                // In a kept diagnostic value too.
                ("reason=Kim's iPad", "reason=<device-name>'s iPad"),
                // The line's own rule and a seen name together.
                (
                    r#"Client session established peer=192.0.2.7:50000 session_id=5 device=Maria's iPad Pro screen="2732x2048""#,
                    r#"Client session established peer=192.0.2.7:50000 session_id=5 device=<device-name> screen="2732x2048""#,
                ),
                // Part of a longer word, or a field name, is something else.
                ("Kimberly kim=1", "Kimberly kim=1"),
                (
                    "An iPad and an iPhone on TV, with headphones: device=<headphones>",
                    "An iPad and an iPhone on TV, with headphones: device=<headphones>",
                ),
            ],
        );
    }

    #[test]
    fn seen_device_names_leave_gpu_and_display_models_alone() {
        let mut names = Names::new(None, None, None);
        for name in [
            "DELL U2720Q (NVIDIA High Definition Audio)",
            "Speakers (Realtek(R) Audio)",
        ] {
            names.note_device(name, audio_kind(name));
        }
        for word in ["NVIDIA", "Intel", "Display", "Monitor"] {
            names.note_device(word, DEVICE_NAME);
        }
        assert_eq!(
            redact(
                "Default PC audio endpoint changed to DELL U2720Q (NVIDIA High Definition Audio)",
                &names
            ),
            "Default PC audio endpoint changed to <hdmi>"
        );
        for line in [
            format!("{STAMP}  INFO GPU: NVIDIA GeForce RTX 5060 (8188 MB)"),
            format!(
                "{STAMP}  INFO Discovered display output device=\\\\.\\DISPLAY1 \
                 adapter=NVIDIA GeForce RTX 5060 width=2560 height=1440 primary=true"
            ),
            format!(
                "{STAMP}  WARN Virtual display failure snapshot vdd_device=ok \
                 outputs=[DELL U2720Q] device=unknown(display tasks missing)"
            ),
            format!(
                "{STAMP}  INFO Found GPU adapter adapter_index=0 name=Intel(R) Iris Xe \
                 vendor=Intel vram_mb=128"
            ),
            format!("{STAMP}  INFO Capture using adapter adapter=Microsoft Basic Display Adapter"),
            format!("{STAMP}  INFO Realtek(R) Audio driver 6.0.9600 on a DELL U2720Q monitor"),
        ] {
            assert_eq!(redact(&line, &names), line);
        }
    }

    #[test]
    fn hardware_and_display_devices_keep_their_names() {
        let names = Names::new(None, None, None);
        for line in [
            format!("{STAMP}  INFO Client session established session_id=5 device=iPad screen=2732x2048"),
            format!("{STAMP}  INFO Client session established session_id=5 device=iPhone"),
            format!(
                "{STAMP}  INFO Discovered display output device=\\\\.\\DISPLAY1 \
                 adapter=NVIDIA GeForce RTX 5060 width=2560 height=1440 primary=true"
            ),
            format!("{STAMP}  INFO GPU: NVIDIA GeForce RTX 5060 (8188 MB)"),
            format!("{STAMP}  WARN Virtual display could not be enabled device=problem_43"),
            format!(
                "{STAMP}  WARN Virtual display failure snapshot vdd_device=ok \
                 outputs=[DELL U2720Q] device=unknown(display tasks missing)"
            ),
        ] {
            assert_eq!(redact(&line, &names), line);
        }
    }

    #[test]
    fn names_that_are_words_leave_time_level_and_fields_alone() {
        let names = Names::new(None, Some("input"), Some("INFO"));
        let line = format!("{STAMP}  INFO Encoder opened input=Yuv420 info=1");
        assert_eq!(redact(&line, &names), line);
        let line = format!("{STAMP}  INFO Logged on as Input on info");
        assert_eq!(
            redact(&line, &names),
            format!("{STAMP}  INFO Logged on as %USERNAME% on %COMPUTERNAME%")
        );
        // Too short to replace safely.
        let names = Names::new(None, Some("a"), Some("PC"));
        let line = format!("{STAMP}  INFO Capture a frame on PC");
        assert_eq!(redact(&line, &names), line);
    }

    #[test]
    fn everything_else_stays_as_it_was() {
        let names = maria();
        for line in [
            format!("{STAMP}  INFO Client session established session_id=2654435769 device=iPad"),
            format!(
                "{STAMP}  WARN Capture failed error=0x887A0026 gpu=NVIDIA GeForce RTX 4070 \
                 version=0.3.4 build=abc1234"
            ),
            format!("{STAMP} ERROR second_screen_fallback=no_frames session_id=7"),
            "EternalMonitor stopped unexpectedly: Host panic at src\\main.rs:1:1".to_owned(),
            String::new(),
        ] {
            assert_eq!(redact(&line, &names), line);
        }
    }

    #[test]
    fn no_names_leaves_names_alone() {
        let names = Names::new(None, None, Some("  "));
        assert!(names.words.is_empty() && names.profile.is_empty());
        let line = format!("{STAMP}  INFO host=MARIA-DESKTOP");
        assert_eq!(redact(&line, &names), line);
    }

    fn assert_cases(names: &Names, cases: &[(&str, &str)]) {
        let wrong: Vec<_> = cases
            .iter()
            .filter_map(|(text, uploaded)| {
                let got = redact(text, names);
                (got != *uploaded).then(|| format!("{text}\n  want {uploaded}\n  got  {got}"))
            })
            .collect();
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    #[test]
    fn profiles_under_any_root_are_found() {
        assert_cases(
            &maria(),
            &[
                (
                    r"module=\\server\Users\OtherPerson\AppData\x.dll",
                    r"module=\\server\Users\%USERNAME%\AppData\x.dll",
                ),
                (
                    r"module=\\?\UNC\server\share\Users\Kim\x.dll",
                    r"module=\\?\UNC\server\share\Users\%USERNAME%\x.dll",
                ),
                (
                    r#"path="\\\\server\\Users\\Kim\\x""#,
                    r#"path="\\\\server\\Users\\%USERNAME%\\x""#,
                ),
                (
                    r"dir=\\server\share\Users\Maria Lopez\Desktop",
                    r"dir=%USERPROFILE%\Desktop",
                ),
                (
                    r"dir=\\?\UNC\server\Users\maria lopez\x",
                    r"dir=%USERPROFILE%\x",
                ),
                (r"dir=\Users\Kim\x", r"dir=\Users\%USERNAME%\x"),
                // Two paths on one line: the first folder ends where the
                // second path starts.
                (
                    r"move C:\Users\FirstPerson C:\Users\SecondPerson",
                    r"move C:\Users\%USERNAME% C:\Users\%USERNAME%",
                ),
                (
                    r"copy C:\Users\Maria Lopez D:\Users\Kim\x",
                    r"copy %USERPROFILE% D:\Users\%USERNAME%\x",
                ),
                // Truncated paths.
                (r"C:\Users\", r"C:\Users\"),
                (r"C:\Users", r"C:\Users"),
                (r"\\?\", r"\\?\"),
                (r"C:\Users\Kim\\", r"C:\Users\%USERNAME%\\"),
            ],
        );
    }

    #[test]
    fn short_names_are_replaced_only_as_a_path_segment() {
        let names = Names::new(None, Some("Al"), Some("XY"));
        assert_cases(
            &names,
            &[
                (
                    r"dir=C:\Users\Al\AppData",
                    r"dir=C:\Users\%USERNAME%\AppData",
                ),
                (r"dir=D:\Al\Projects", r"dir=D:\%USERNAME%\Projects"),
                (r"share=\\XY\share\x", r"share=\\%COMPUTERNAME%\share\x"),
                (
                    "Al said hi to XY; Albert al=1 xy=2",
                    "Al said hi to XY; Albert al=1 xy=2",
                ),
                (r"dir=D:\Alan\x", r"dir=D:\Alan\x"),
            ],
        );
    }

    #[test]
    fn a_pc_name_ends_at_punctuation_but_not_at_a_hyphen() {
        let names = Names::new(None, None, Some("DESKTOP-DEMO"));
        assert_cases(
            &names,
            &[
                (
                    "Audio device USB_DESKTOP-DEMO_Audio opened",
                    "Audio device USB_%COMPUTERNAME%_Audio opened",
                ),
                ("host=desktop-demo.local.", "host=%COMPUTERNAME%.local."),
                (
                    "host=DESKTOP-DEMO-2 peer=xDESKTOP-DEMO",
                    "host=DESKTOP-DEMO-2 peer=xDESKTOP-DEMO",
                ),
            ],
        );
    }

    /// A user called Intel or display, or a PC called DESKTOP, leaves the
    /// hardware it shares a word with alone. As a profile folder, the name
    /// still goes.
    #[test]
    fn names_that_are_common_words_leave_hardware_alone() {
        for (user, pc) in [("Intel", "DESKTOP"), ("display", "Microsoft")] {
            let names = Names::new(None, Some(user), Some(pc));
            for line in [
                format!("{STAMP}  INFO   GPU:     Intel(R) UHD Graphics 630 (Intel)"),
                format!(
                    "{STAMP}  INFO Found GPU adapter adapter_index=0 name=Intel(R) Iris Xe \
                     vendor=Intel vram_mb=128"
                ),
                format!(
                    "{STAMP}  INFO Capture using adapter adapter=Microsoft Basic Display Adapter"
                ),
                format!("{STAMP}  INFO Display mode changed on DESKTOP-OTHER, an Intel desktop"),
                format!("{STAMP}  INFO mDNS host=DESKTOP instance=EternalMonitor (DESKTOP)"),
            ] {
                assert_eq!(redact(&line, &names), line, "{user}/{pc}");
            }
            assert_eq!(
                redact(&format!(r"dir=C:\Users\{user}\AppData"), &names),
                r"dir=C:\Users\%USERNAME%\AppData"
            );
        }
    }

    /// Diagnostic fields keep their tokens, so the daily check still reads
    /// `second_screen_fallback=no_frames` from a PC whose user is called
    /// `no_frames`.
    #[test]
    fn diagnostic_values_keep_their_tokens() {
        let fallback = regex::Regex::new(r#"second_screen_fallback="?([a-z_]{1,32})"#).unwrap();
        for (user, pc) in [
            ("no_frames", "SECOND"),
            ("frames", "FALLBACK"),
            ("steady", "SCREEN-2"),
        ] {
            let names = Names::new(Some(&format!(r"C:\Users\{user}")), Some(user), Some(pc));
            for line in [
                format!("{STAMP}  WARN Virtual display fell back second_screen_fallback=no_frames session_id=7"),
                format!("{STAMP}  INFO Session end stage=steady outcome=streamed reason=no_frames"),
                format!(
                    "{STAMP}  INFO Periodic stats summary window_s=60 stream_codec=H.264 (NVENC) \
                     stream_usb_state=Waiting for an iPad"
                ),
            ] {
                let uploaded = redact(&line, &names);
                assert_eq!(uploaded, line, "{user}/{pc}");
                if line.contains("second_screen_fallback") {
                    assert_eq!(&fallback.captures(&uploaded).unwrap()[1], "no_frames");
                }
            }
        }
        let names = Names::new(None, Some("no_frames"), None);
        assert_cases(
            &names,
            &[
                (
                    "Logged on as no_frames device=no_frames's iPad",
                    "Logged on as %USERNAME% device=%USERNAME%'s iPad",
                ),
                (
                    r"log=C:\Users\no_frames\x.log",
                    r"log=C:\Users\%USERNAME%\x.log",
                ),
                // A path inside a diagnostic value still loses the name.
                (r"reason=D:\no_frames\x", r"reason=D:\%USERNAME%\x"),
            ],
        );
    }

    /// Every enumerated field the host logs keeps its value when the user or
    /// the PC has that name.
    #[test]
    fn every_logged_token_survives_a_name_equal_to_it() {
        let cases = [
            (
                "encoder_open",
                "WARN A Windows or driver call was slow but returned slow=encoder_open took_ms=1200 warn_ms=1000",
            ),
            (
                "graphics",
                "INFO The second screen failed on this PC before reason=no_frames changed=windows,graphics",
            ),
            ("applied", "INFO remote_config=applied rollout=none ss_rule=match attach_s=6"),
            ("none", "INFO remote_config=applied rollout=none ss_rule=none attach_s=6"),
            ("defaults", "INFO remote_config=defaults reason=offline rollout=7d"),
            ("NotInstalled", "vdd_device=missing problem=NotInstalled"),
            (
                "unknown",
                r#"WARN Virtual display failure snapshot vdd_device=no_monitor pin=none pin_reason="unknown" render=unknown hags=unset settings_path=unknown"#,
            ),
            (
                "Iris",
                r#"WARN Virtual display failure snapshot pin="Intel(R) Iris(R) Xe" pin_matched=true render="Intel(R) Iris(R) Xe" render_is_pin=true"#,
            ),
            (
                "GeForce3050",
                r#"WARN Virtual display failure snapshot adapters=[d2c1 "Intel(R) Iris(R) Xe" 8086:46a6 driver=32.0.101.7092 outputs=1 | 10000e5f0 "GeForce3050" 10de:25a0 driver=32.0.16.1742 outputs=0] paths=[active src=d2c1/0 tgt=d2c1/4352 tech=5 vdd=yes]"#,
            ),
            ("yes", "WARN Virtual display failure snapshot paths=[active src=d2c1/0 tgt=d2c1/4352 tech=5 vdd=yes]"),
            (
                "hevc_nvenc",
                "INFO Staying on the encoder that streams for this pipeline from=hevc_nvenc to=h264_nvenc",
            ),
            (
                "h264_nvenc",
                "INFO Trying the preferred encoder again after a stalled open preferred=h264_nvenc using=libx264 attempt=2",
            ),
            (
                "modes",
                "INFO Second screen mode mode=2732x2048 requested=the driver's modes",
            ),
            ("arm64", "INFO EternalMonitor host 0.3.4 revision=x | OS=Windows arch=arm64 logical_cpus=8"),
            (
                "Client",
                "INFO Windows os=10.0 registry_product=Windows 11 Pro installation_type=Client display_version=24H2",
            ),
            ("Family", "INFO CPU=x identifier=Intel64 Family 6 Model 154"),
            (
                "vulkan",
                "INFO Troubleshooting settings in effect overrides=encoder_input=bgra (setting), \
                 packet=1200 (environment), window=vulkan (setting), graphics_card=2 (windows)",
            ),
            (
                "HighPerformance",
                "INFO Graphics card switch outcome: capture works gpu=NVIDIA preference=Some(HighPerformance) \
                 app_gpu=\"NVIDIA\"",
            ),
            (
                "Radeon780M",
                "INFO Graphics card check screen_gpu=Radeon780M app_gpu=GeForce4060 \
                 switch=Some(PowerSaving) cards=2 power_saving_gpu=Radeon780M high_performance_gpu=GeForce4060",
            ),
            ("Dx12", "INFO Window renderer instance backends=DX12 choice=Some(Dx12) chosen_failed=false"),
        ];
        for (name, message) in cases {
            let line = format!("{STAMP}  {message}");
            for names in [
                Names::new(Some(&format!(r"C:\Users\{name}")), Some(name), None),
                Names::new(None, None, Some(name)),
            ] {
                assert_eq!(redact(&line, &names), line, "{name}");
            }
        }
    }

    /// xorshift64*: enough to vary the fuzz lines, and the same every run.
    struct Lines(u64);

    impl Lines {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
            from[(self.next() % from.len() as u64) as usize]
        }

        fn line(&mut self, names: &[&str]) -> String {
            const PIECES: &[&str] = &[
                r"C:",
                r"c:",
                r"\",
                r"/",
                r"\\",
                r"\\\\",
                r"\\?\",
                r"\\\\?\\",
                r"\\?\UNC\",
                r"\\.\",
                r"Users",
                r"USERS",
                r"Users\",
                r"C:\Users\",
                r"\\server\Users\",
                r"\\?\C:\Users\",
                r"Public",
                r"Default User",
                r"%USERNAME%",
                r"%USERPROFILE%",
                r"%COMPUTERNAME%",
                " ",
                "  ",
                "\t",
                "=",
                "x=",
                "name=",
                "second_screen_fallback=",
                "gpu=",
                "no_frames",
                "Intel",
                "GPU:",
                "Found GPU adapter",
                "WASAPI loopback active",
                "Client liveness expired",
                " device=",
                " rate=",
                " [repeated ",
                "]",
                "'",
                "\"",
                ":",
                "?",
                "*",
                "%",
                "_",
                "-",
                ".",
                ",",
                ")",
                "(",
                "0",
                "42",
                "AppData",
                "é",
                "ß",
                "İ",
                "e\u{301}",
                "日本",
                "🙂",
                "\u{200B}",
                "Ω",
                "K",
            ];
            let mut line = String::new();
            if self.next().is_multiple_of(3) {
                line.push_str(STAMP);
                line.push_str(self.pick(&["  INFO ", "  WARN ", " ERROR ", " INFO"]));
            }
            for _ in 0..self.next() % 40 {
                if self.next().is_multiple_of(4) {
                    line.push_str(self.pick(names));
                } else {
                    line.push_str(self.pick(PIECES));
                }
            }
            line
        }
    }

    /// Any line, including several paths, truncated paths, Unicode and
    /// empty folder names: no panic, and redacting twice changes nothing.
    #[test]
    fn redaction_never_panics_and_is_idempotent() {
        let sets: [(Option<&str>, Option<&str>, Option<&str>); 7] = [
            (
                Some(r"C:\Users\Maria Lopez"),
                Some("maria"),
                Some("MARIA-DESKTOP"),
            ),
            (
                Some(r"D:\Profiles\maria.CORP"),
                Some("maria"),
                Some("DESKTOP-DEMO"),
            ),
            (Some(r"\\?\C:\Users\Al"), Some("Al"), Some("PC")),
            (
                Some(r"C:\Users\no_frames"),
                Some("no_frames"),
                Some("USERNAME"),
            ),
            (Some(r"C:\Users\İrem"), Some("İrem"), Some("日本-PC")),
            (Some(r"C:\Users\K"), Some("K"), Some("Ω")),
            (Some(r"\\"), Some(""), None),
        ];
        let mut lines = Lines(0x9E37_79B9_7F4A_7C15);
        for (profile, user, pc) in sets {
            let mut names = Names::new(profile, user, pc);
            names.note_device("Headphones (Maria's AirPods)", "<headphones>");
            names.note_device("Kim's iPad", DEVICE_NAME);
            let pieces: Vec<&str> = [
                profile,
                user,
                pc,
                Some("Maria Lopez"),
                Some("Kim"),
                Some("Headphones (Maria's AirPods)"),
                Some("Kim's iPad"),
            ]
            .into_iter()
            .flatten()
            .collect();
            for _ in 0..3000 {
                let line = lines.line(&pieces);
                let once = redact(&line, &names);
                let twice = redact(&once, &names);
                assert_eq!(
                    twice, once,
                    "not idempotent for {line:?} with {user:?}/{pc:?}"
                );
            }
        }
    }
}
