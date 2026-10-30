//! Arabic in a window egui lays out left to right.
//!
//! egui 0.36 shapes each run of text that one font face draws on its own,
//! and has no bidirectional algorithm: a space drawn by Geist splits an
//! Arabic sentence into runs that come out left to right, and a Latin word or
//! a number inside Arabic lands in the wrong place. Arabic letters are never
//! in Geist, so the Arabic face draws exactly the runs of Arabic letters, and
//! the shaper reverses each of those by itself. [`visual`] puts everything
//! else in display order: the Unicode bidirectional algorithm orders the
//! runs, and inside a right-to-left run the words swap places while each
//! Arabic word keeps its reading order for the shaper.
//!
//! Only whole lines can be reordered, so wrapped Arabic is broken into lines
//! first ([`wrap`]).

use unicode_bidi::{BidiInfo, Level};

/// First strong isolate: what follows keeps its own direction.
pub const FSI: char = '\u{2068}';
/// Pop directional isolate.
pub const PDI: char = '\u{2069}';

/// `text`, a right-to-left paragraph per line in reading order, in the order
/// egui should lay it out.
pub fn visual(text: &str) -> String {
    text.split('\n')
        .map(visual_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn visual_line(line: &str) -> String {
    let info = BidiInfo::new(line, Some(Level::rtl()));
    let mut out = String::with_capacity(line.len());
    for paragraph in &info.paragraphs {
        let (levels, runs) = info.visual_runs(paragraph, paragraph.range.clone());
        for run in runs {
            let text = &line[run.clone()];
            if levels[run.start].is_rtl() {
                push_right_to_left(text, &mut out);
            } else {
                out.push_str(text);
            }
        }
    }
    // The controls only steer the algorithm; no font draws them.
    out.retain(|c| !is_bidi_control(c));
    out
}

/// A right-to-left run: its pieces in reverse order. Arabic words stay in
/// reading order (the shaper reverses them); spaces and punctuation between
/// them are reversed here, and brackets mirrored.
fn push_right_to_left(text: &str, out: &mut String) {
    let mut pieces: Vec<(bool, &str)> = Vec::new();
    let (mut start, mut current) = (0, None);
    for (i, c) in text.char_indices() {
        let arabic = is_arabic(c);
        match current {
            Some(previous) if previous != arabic => {
                pieces.push((previous, &text[start..i]));
                start = i;
                current = Some(arabic);
            }
            None => current = Some(arabic),
            _ => {}
        }
    }
    if let Some(arabic) = current {
        pieces.push((arabic, &text[start..]));
    }
    for (arabic, piece) in pieces.into_iter().rev() {
        if arabic {
            out.push_str(piece);
        } else {
            out.extend(piece.chars().rev().map(mirror));
        }
    }
}

/// Drawn by the Arabic face, never by Geist.
pub fn is_arabic(c: char) -> bool {
    matches!(c,
        '\u{0600}'..='\u{06FF}'
        | '\u{0750}'..='\u{077F}'
        | '\u{08A0}'..='\u{08FF}'
        | '\u{FB50}'..='\u{FDFF}'
        | '\u{FE70}'..='\u{FEFC}'
        // Joiners inside an Arabic word.
        | '\u{200C}' | '\u{200D}')
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        other => other,
    }
}

/// `text` in reading order, broken at spaces into lines no wider than
/// `width` as `measure` finds the drawn line, each line in visual order.
pub fn wrap(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> String {
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && measure(&visual_line(&candidate)) > width {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
        .iter()
        .map(|line| visual_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}
