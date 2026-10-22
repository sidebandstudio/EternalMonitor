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
use std::path::Path;

/// A display mode for the virtual display's mode list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VddMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
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

/// `xml` naming `gpu` as the render adapter, everything else as it was:
/// the existing name is replaced, or a `<gpu>` element added at the top.
/// `None` when the document has no `<vdd_settings>` element to add it to.
pub fn with_gpu(xml: &str, gpu: &str) -> Option<String> {
    const OPEN: &str = "<friendlyname>";
    const CLOSE: &str = "</friendlyname>";
    if let Some(start) = xml.find(OPEN).map(|at| at + OPEN.len()) {
        let end = start + xml[start..].find(CLOSE)?;
        return Some(format!("{}{}{}", &xml[..start], escape(gpu), &xml[end..]));
    }
    let root = xml.find("<vdd_settings>")? + "<vdd_settings>".len();
    let line_end = xml[root..].find('\n').map_or(root, |at| root + at + 1);
    let separator = if line_end == root { "\n" } else { "" };
    Some(format!(
        "{}{separator}{}{}",
        &xml[..line_end],
        gpu_element(gpu),
        &xml[line_end..]
    ))
}

/// Point the settings file the host does not otherwise write at `gpu`.
/// True when it changed. A missing file stays missing: without one the
/// driver offers its built-in modes, and a document holding only a GPU
/// would leave it none.
pub fn pin_gpu_in(path: &Path, gpu: &str) -> io::Result<bool> {
    let current = std::fs::read_to_string(path)?;
    let updated = with_gpu(&current, gpu)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no <vdd_settings> element"))?;
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

fn replace(path: &Path, contents: &str) -> io::Result<()> {
    let tmp = path.with_extension("xml.tmp");
    std::fs::write(&tmp, contents)?;
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
