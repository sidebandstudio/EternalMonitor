//! What uploads leave out: the Windows profile folder, which is a user name,
//! the user name and the PC's name. Support identifies a PC by its
//! installation id. The session log and "Copy logs" keep them all.
//!
//! - `C:\Users\<name>`, with either separator, Debug-doubled backslashes or a
//!   `\\?\` prefix, and the profile's own path (`%USERPROFILE%`, which may
//!   be elsewhere), become `%USERPROFILE%`. `Public`, `Default`,
//!   `Default User` and `All Users` stay.
//! - The user name (`%USERNAME%`) and the PC name (`%COMPUTERNAME%`), as a
//!   whole word in any case, become those names. A name shorter than three
//!   characters, the line's time and level, and field names (`name=`) stay,
//!   so a PC called `INFO` or a user called `input` leaves the line readable.

use once_cell::sync::Lazy;
use regex::Regex;

/// Folders under `Users` that name nobody. Support uses them.
const SHARED_PROFILES: [&str; 4] = ["Default User", "All Users", "Default", "Public"];

/// Windows local profile folder names are short: a user name of at most 20
/// characters plus a suffix such as `.DOMAIN` or `.000`.
const LOCAL_FOLDER_CHARS: usize = 32;

/// Shorter names are left alone: replacing every `a` or `pc` would garble
/// the line and hide little.
const MIN_NAME_CHARS: usize = 3;

/// A `\\?\` prefix, also Debug-doubled (`\\\\?\\`).
const VERBATIM: &str = r"(?:[\\/]{2,4}\?[\\/]{1,2})?";

pub(super) struct Names {
    /// This user's profile path, in any case and with any separator.
    profile: Option<Regex>,
    /// Folder names that start a profile segment: the profile's folder and
    /// the user name, longest first.
    folders: Vec<String>,
    /// Whole-word names and what replaces them, longest first.
    words: Vec<(Regex, &'static str)>,
}

impl Names {
    pub(super) fn new(profile: Option<&str>, user: Option<&str>, pc: Option<&str>) -> Self {
        let clean = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let profile = clean(profile).map(|path| {
            path.trim_start_matches(['\\', '/', '?'])
                .trim_end_matches(['\\', '/'])
                .to_owned()
        });
        let (user, pc) = (clean(user), clean(pc));
        let profile_pattern = profile.as_deref().and_then(|path| {
            let parts: Vec<_> = path
                .split(['\\', '/'])
                .filter(|part| !part.is_empty())
                .map(regex::escape)
                .collect();
            (parts.len() >= 2)
                .then(|| Regex::new(&format!("(?i){VERBATIM}{}", parts.join(r"[\\/]+"))).ok())
                .flatten()
        });
        let mut folders: Vec<String> = profile
            .as_deref()
            .and_then(|path| path.rsplit(['\\', '/']).next())
            .map(str::to_owned)
            .into_iter()
            .chain(user.clone())
            .filter(|name| !SHARED_PROFILES.iter().any(|s| s.eq_ignore_ascii_case(name)))
            .collect();
        folders.sort_by_key(|name| std::cmp::Reverse(name.chars().count()));
        let mut words: Vec<(String, &'static str)> = [(pc, "%COMPUTERNAME%"), (user, "%USERNAME%")]
            .into_iter()
            .filter_map(|(name, token)| Some((name?, token)))
            .filter(|(name, _)| name.chars().count() >= MIN_NAME_CHARS)
            .collect();
        words.sort_by_key(|(name, _)| std::cmp::Reverse(name.chars().count()));
        Self {
            profile: profile_pattern,
            folders,
            words: words
                .into_iter()
                .filter_map(|(name, token)| {
                    Some((
                        Regex::new(&format!("(?i){}", regex::escape(&name))).ok()?,
                        token,
                    ))
                })
                .collect(),
        }
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
}

pub(super) static NAMES: Lazy<Names> = Lazy::new(Names::current);

/// `line` without the names in `names`. A log line's time and level stay.
pub(super) fn redact(line: &str, names: &Names) -> String {
    let (head, message) = match crate::log_repeat::split_stamp(line) {
        Some((stamp, rest)) => {
            let level = rest.len() - rest.trim_start().len();
            let level = level + rest[level..].find(' ').map_or(0, |end| end + 1);
            line.split_at(stamp.len() + level)
        }
        None => ("", line),
    };
    let message = redact_profile_path(message, names);
    let message = redact_profiles(&message, names);
    let mut message = message.into_owned();
    for (pattern, token) in &names.words {
        message = replace_words(&message, pattern, token);
    }
    format!("{head}{message}")
}

/// Whether `c` continues a name or path segment.
fn continues_name(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '%')
}

/// This user's profile path, ending at its folder name.
fn redact_profile_path<'a>(text: &'a str, names: &Names) -> std::borrow::Cow<'a, str> {
    let Some(pattern) = &names.profile else {
        return text.into();
    };
    pattern.replace_all(text, |captures: &regex::Captures| {
        let whole = captures.get(0).expect("match");
        // A longer folder that starts with this name is another folder.
        let continued = text[whole.end()..]
            .chars()
            .next()
            .is_some_and(continues_name)
            || text[..whole.start()]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
        if continued {
            whole.as_str().to_owned()
        } else {
            "%USERPROFILE%".to_owned()
        }
    })
}

/// How a profile segment ended.
#[derive(Clone, Copy, PartialEq)]
enum SegmentEnd {
    /// A separator, a character Windows forbids in file names
    /// (`: * ? " < > |`) or `'`: the segment is exactly the folder name.
    Delimiter,
    /// The end of the line, or a space before the next `word=` field.
    Open,
}

/// The segment after `\Users\` in `rest`: its length in bytes and how it
/// ended.
fn segment(rest: &str) -> (usize, SegmentEnd) {
    static FIELD: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^\s+[A-Za-z_][A-Za-z0-9_]*=").expect("field pattern"));
    for (index, c) in rest.char_indices() {
        if matches!(
            c,
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\''
        ) {
            return (index, SegmentEnd::Delimiter);
        }
        if c.is_whitespace() && FIELD.is_match(&rest[index..]) {
            return (index, SegmentEnd::Open);
        }
    }
    (rest.len(), SegmentEnd::Open)
}

/// `name` at the start of `text`, in any case: the length in bytes it takes.
fn starts_with_name(text: &str, name: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    for expected in name.chars() {
        let (_, found) = chars.next()?;
        if !found.to_lowercase().eq(expected.to_lowercase()) {
            return None;
        }
    }
    Some(chars.next().map_or(text.len(), |(index, _)| index))
}

/// Every `<drive>:\Users\<folder>` becomes `%USERPROFILE%`. The folder runs
/// to the next separator, character Windows forbids in file names or quote;
/// with none of those, it is a known name followed by a space or
/// punctuation, else the whole rest if it is short enough to be a folder
/// name, else the rest up to its first space.
fn redact_profiles<'a>(text: &'a str, names: &Names) -> std::borrow::Cow<'a, str> {
    static PREFIX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(&format!(r"(?i){VERBATIM}\b[a-z]:[\\/]+users[\\/]+"))
            .expect("profile prefix pattern")
    });
    let mut out = String::new();
    let mut copied = 0;
    for prefix in PREFIX.find_iter(text) {
        let rest = &text[prefix.end()..];
        let (length, end) = segment(rest);
        let folder = &rest[..length];
        if SHARED_PROFILES
            .iter()
            .any(|name| name.eq_ignore_ascii_case(folder))
        {
            continue;
        }
        let replaced = if end == SegmentEnd::Delimiter {
            length
        } else if let Some(name) = names.folders.iter().find_map(|name| {
            starts_with_name(folder, name).filter(|&after| {
                rest[after..]
                    .chars()
                    .next()
                    .is_none_or(|c| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ')'))
            })
        }) {
            name
        } else if folder.chars().count() <= LOCAL_FOLDER_CHARS {
            length
        } else {
            folder.find(char::is_whitespace).unwrap_or(length)
        };
        if replaced == 0 {
            continue;
        }
        out.push_str(&text[copied..prefix.start()]);
        out.push_str("%USERPROFILE%");
        copied = prefix.end() + replaced;
    }
    if copied == 0 {
        return text.into();
    }
    out.push_str(&text[copied..]);
    out.into()
}

/// Each whole-word `pattern` match that is not a field name becomes `token`.
fn replace_words(text: &str, pattern: &Regex, token: &str) -> String {
    pattern
        .replace_all(text, |captures: &regex::Captures| {
            let whole = captures.get(0).expect("match");
            let next = text[whole.end()..].chars().next();
            let word = !text[..whole.start()]
                .chars()
                .next_back()
                .is_some_and(continues_name)
                && !next.is_some_and(|c| continues_name(c) || c == '=');
            if word {
                token.to_owned()
            } else {
                whole.as_str().to_owned()
            }
        })
        .into_owned()
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
            // Other profiles on the PC.
            (
                r"Host panic at C:\Users\bob\.cargo\registry\src\renderer.rs:984:17:",
                r"Host panic at %USERPROFILE%\.cargo\registry\src\renderer.rs:984:17:",
            ),
            (r#"path='C:\Users\bob'"#, r#"path='%USERPROFILE%'"#),
            (r"home=C:\Users\bob next=1", r"home=%USERPROFILE% next=1"),
            (
                r"module=\\?\D:\Users\Kim\x.dll",
                r"module=%USERPROFILE%\x.dll",
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
            (
                "Client session established session_id=5 device=Maria's iPad",
                "Client session established session_id=5 device=%USERNAME%'s iPad",
            ),
            // Part of a longer word is another word.
            ("user=mariadb mariana", "user=mariadb mariana"),
        ] {
            assert_eq!(redact(text, &names), uploaded, "{text}");
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
        assert!(names.words.is_empty() && names.profile.is_none());
        let line = format!("{STAMP}  INFO host=MARIA-DESKTOP");
        assert_eq!(redact(&line, &names), line);
    }
}
