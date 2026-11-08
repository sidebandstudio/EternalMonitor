//! The one way to list the PC's display outputs. Every listing is judged for
//! a second screen's proof ([`super::second_screen::observe_listing`]): one
//! that does not show the proven output as it was clears the proof. Kept, the
//! same display back before the next reconciliation inherited it, and a dark
//! one cost a watchdog restart. The DXGI enumerator takes a [`Judged`] that
//! only this module makes, so a new listing path cannot skip the judge.

use super::OutputInfo;
use crate::control::SharedControl;

/// Made only here: the DXGI enumerator and the debug-build fake display take
/// one, so every listing either makes passes through this module.
#[cfg(any(windows, debug_assertions))]
pub(super) struct Judged(());

/// The outputs attached to the desktop, judged for `shared`'s second screen.
/// In debug builds with a fake virtual display installed, the fake's.
pub fn outputs(shared: &SharedControl) -> Vec<OutputInfo> {
    let outputs = unjudged(shared);
    super::second_screen::observe_listing(shared, &outputs);
    outputs
}

/// Whether any output is attached to the desktop, judged as [`outputs`].
pub fn any_attached(shared: &SharedControl) -> bool {
    !outputs(shared).is_empty()
}

/// [`outputs`] with the adapter and output objects DXGI listed them from,
/// for capture to open, and how many adapters were listed.
#[cfg(windows)]
pub(super) fn listed(
    shared: &SharedControl,
    factory: &windows::Win32::Graphics::Dxgi::IDXGIFactory1,
) -> (Vec<super::dxgi::Listed>, u32) {
    let (listed, adapters) = super::dxgi::list_outputs(factory, Judged(()));
    let outputs: Vec<OutputInfo> = listed.iter().map(|listed| listed.info.clone()).collect();
    super::second_screen::observe_listing(shared, &outputs);
    (listed, adapters)
}

/// The display the startup banner names: the primary, or the first one.
/// Before any host state exists there is no proof to judge; only this label
/// leaves the module, never the outputs.
pub fn startup_display_label() -> String {
    let outputs = from_dxgi();
    outputs
        .iter()
        .find(|output| output.is_primary)
        .or_else(|| outputs.first())
        .map(|output| {
            format!(
                "{} ({}x{})",
                output.device_name, output.width, output.height
            )
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn unjudged(shared: &SharedControl) -> Vec<OutputInfo> {
    #[cfg(debug_assertions)]
    if let Some(outputs) = super::second_screen::fake::outputs(shared, Judged(())) {
        return outputs;
    }
    // Only the debug-build fake reads it.
    #[cfg(not(debug_assertions))]
    let _ = shared;
    from_dxgi()
}

/// Windows-only in substance; other platforms have no selectable outputs
/// (the synthetic source ignores them).
fn from_dxgi() -> Vec<OutputInfo> {
    #[cfg(windows)]
    {
        super::dxgi::outputs(Judged(()))
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    /// Each name that sees outputs without the judge, and the files that may
    /// name it. The compiler holds the token-taking ones (`list_outputs`,
    /// `dxgi::outputs`, `fake::outputs`) to this module; this is the backstop
    /// for what it cannot hold: the Windows API itself, and names that must
    /// not come back.
    const UNJUDGED: [(&str, &[&str]); 6] = [
        // The DXGI enumerator.
        ("list_outputs", &["capture/dxgi.rs"]),
        // DXGI's own enumeration: capture's, and a per-adapter output count in
        // the virtual display's diagnostics snapshot, which names no output.
        ("EnumOutputs", &["capture/dxgi.rs", "vdd_snapshot.rs"]),
        // The banner's label, which carries no outputs.
        ("startup_display_label", &["main.rs"]),
        // Removed unjudged entry points.
        ("outputs_before_the_host_starts", &[]),
        ("enumerate_outputs", &[]),
        ("display_attached", &[]),
    ];

    /// Whether `text` uses the identifier `name` (aliases included: a `use …
    /// as` names it too).
    fn uses(text: &str, name: &str) -> bool {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        text.match_indices(name).any(|(at, _)| {
            !text[..at].ends_with(word) && !text[at + name.len()..].starts_with(word)
        })
    }

    /// `line` without its string literals.
    fn code_only(line: &str) -> String {
        let mut code = String::new();
        let mut in_string = false;
        let mut escaped = false;
        for c in line.chars() {
            if in_string {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
            } else if c == '"' {
                in_string = true;
            } else {
                code.push(c);
            }
        }
        code
    }

    /// The unjudged names `file` uses where it may not. Comments and string
    /// literals may name them.
    fn strays(file: &str, text: &str) -> Vec<String> {
        let code: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(code_only)
            .collect::<Vec<_>>()
            .join("\n");
        UNJUDGED
            .iter()
            .filter(|(name, places)| uses(&code, name) && !places.contains(&file))
            .map(|(name, _)| format!("{file}: {name}"))
            .collect()
    }

    /// Review r5 of #205: listings that bypassed the judge let a returning
    /// display inherit an old proof, one path at a time.
    #[test]
    fn every_listing_goes_through_the_judge() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        assert!(files.len() > 50, "{} source files", files.len());
        let mut found = Vec::new();
        for file in &files {
            let name = file
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // This module is the judge, and this test names the calls.
            if name != "capture/listing.rs" {
                found.extend(strays(&name, &std::fs::read_to_string(file).unwrap()));
            }
        }
        assert!(
            found.is_empty(),
            "list outputs through capture::listing: {found:?}"
        );
    }

    /// Review r6 of #205: the scan matched calls, so an alias made through
    /// `use … as` went unseen.
    #[test]
    fn the_listing_scan_catches_aliases_and_ignores_comments() {
        let alias = "use crate::capture::listing::startup_display_label as raw;\nfn f() { raw(); }";
        assert_eq!(
            strays("gui/mod.rs", alias),
            ["gui/mod.rs: startup_display_label"]
        );
        assert!(strays("main.rs", alias).is_empty(), "the banner may");
        let grouped = "use windows::Win32::Graphics::Dxgi::{IDXGIAdapter1, EnumOutputs as list};";
        assert_eq!(strays("gpu.rs", grouped), ["gpu.rs: EnumOutputs"]);
        let back = "pub fn outputs_before_the_host_starts() {}";
        assert_eq!(
            strays("capture/mod.rs", back),
            ["capture/mod.rs: outputs_before_the_host_starts"]
        );
        // A comment, a string, or a longer name that contains one, is not a
        // use; code after a string still counts.
        assert!(strays("gui/mod.rs", "// startup_display_label as raw").is_empty());
        assert!(strays(
            "capture/recovery.rs",
            r#"detail: "EnumOutputs: 0x887a0002".into(),"#
        )
        .is_empty());
        assert_eq!(
            strays("gpu.rs", r#"let a = "x\"y"; adapter.EnumOutputs(0);"#),
            ["gpu.rs: EnumOutputs"]
        );
        assert!(strays("gui/mod.rs", "fn startup_display_label_text() {}").is_empty());
    }
}
