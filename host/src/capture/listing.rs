//! The one way to list the PC's display outputs. Every listing is judged for
//! a second screen's proof ([`super::second_screen::observe_listing`]): one
//! that does not show the proven output as it was clears the proof. Kept, the
//! same display back before the next reconciliation inherited it, and a dark
//! one cost a watchdog restart. The DXGI enumerator takes a [`Judged`] that
//! only this module makes, so a new listing path cannot skip the judge.

use super::OutputInfo;
use crate::control::SharedControl;

/// Made only here: the DXGI enumerator takes one, so every listing it makes
/// passes through this module.
#[cfg(windows)]
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

/// For the startup banner, before any host state exists: there is no proof
/// to judge yet. `every_listing_goes_through_the_judge` holds its callers to
/// that.
pub fn outputs_before_the_host_starts() -> Vec<OutputInfo> {
    from_dxgi()
}

fn unjudged(shared: &SharedControl) -> Vec<OutputInfo> {
    #[cfg(debug_assertions)]
    if let Some(outputs) = super::second_screen::fake::outputs(shared) {
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
    /// Review r5 of #205: listings that bypassed the judge let a returning
    /// display inherit an old proof, one path at a time. The DXGI enumerator
    /// needs this module's token; this test holds the remaining ways to see
    /// outputs to their known places, so a new one has to be routed here.
    #[test]
    fn every_listing_goes_through_the_judge() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // Each call, and the files allowed to make it.
        let allowed: [(&str, &[&str]); 5] = [
            // The DXGI enumerator, which takes a `Judged`.
            ("list_outputs(", &["capture/dxgi.rs"]),
            ("dxgi::outputs(", &[]),
            // The debug-build fake's outputs.
            ("fake::outputs(", &["capture/second_screen/fake.rs"]),
            // Before the host exists, and a native test that prints them.
            (
                "outputs_before_the_host_starts(",
                &["main.rs", "vdd_device.rs"],
            ),
            // DXGI's own enumeration: capture's, and a per-adapter output
            // count in the virtual display's diagnostics snapshot, which
            // names no output.
            ("EnumOutputs(", &["capture/dxgi.rs", "vdd_snapshot.rs"]),
        ];
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
        let mut strays = Vec::new();
        for file in &files {
            let name = file
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // This module is the judge, and this test names the calls.
            if name == "capture/listing.rs" {
                continue;
            }
            // Code only: comments may name the calls.
            let text: String = std::fs::read_to_string(file)
                .unwrap()
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for (call, places) in allowed {
                if text.contains(call) && !places.contains(&name.as_str()) {
                    strays.push(format!("{name}: {call}"));
                }
            }
        }
        assert!(
            strays.is_empty(),
            "list outputs through capture::listing: {strays:?}"
        );
        // The old unjudged entry points are gone.
        for file in files.iter().filter(|file| !file.ends_with("listing.rs")) {
            let text = std::fs::read_to_string(file).unwrap();
            assert!(
                !text.contains(concat!("enumerate", "_outputs(")),
                "{file:?}"
            );
        }
    }
}
