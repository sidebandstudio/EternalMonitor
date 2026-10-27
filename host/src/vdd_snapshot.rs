//! One log record of how Windows sees the displays when the second screen
//! did not appear, written before the Disable task removes the virtual
//! display. It goes into the session log, so "Copy logs" and the support
//! diagnostics carry it.
//!
//! It holds graphics card names and IDs, display path numbers and flags,
//! and a few settings. Monitor device paths are read only to tell which
//! paths belong to the virtual display and are never written, and nothing
//! names the machine or the user. The record is bounded in size.

use crate::vdd_device::DeviceState;

const MAX_LINE: usize = 1800;
const MAX_ADAPTERS: usize = 8;
const MAX_TARGETS: usize = 16;
const MAX_NAME: usize = 48;

/// The virtual display's monitor reports the driver's EDID ID in its path.
const VDD_MONITOR: &str = "MTT1337";

/// A DXGI adapter.
#[derive(Debug, Clone)]
pub struct Adapter {
    pub luid: u64,
    pub description: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub driver: String,
    pub outputs: u32,
}

/// One display target from `QueryDisplayConfig(QDC_ALL_PATHS)`.
#[derive(Debug, Clone)]
pub struct Target {
    pub adapter: u64,
    pub id: u32,
    /// Source adapter and id, for an active path.
    pub source: Option<(u64, u32)>,
    pub available: bool,
    pub technology: i32,
    /// Only to tell the virtual display's monitor; never logged.
    pub monitor_path: String,
}

impl Target {
    fn is_vdd(&self) -> bool {
        self.monitor_path.to_ascii_uppercase().contains(VDD_MONITOR)
    }
}

/// The adapters the driver can be pinned to, in DXGI order, without the
/// virtual display's own adapter. While the virtual display is on, DXGI
/// lists that adapter under the name of the card rendering it, which would
/// otherwise read as two cards with one name.
pub fn render_adapters(adapters: &[Adapter], targets: &[Target]) -> Vec<Adapter> {
    let vdd: Vec<u64> = targets
        .iter()
        .filter(|t| t.is_vdd())
        .flat_map(|t| std::iter::once(t.adapter).chain(t.source.map(|(adapter, _)| adapter)))
        .collect();
    adapters
        .iter()
        .filter(|a| !vdd.contains(&a.luid))
        .cloned()
        .collect()
}

/// The card the driver renders the virtual display on, as DXGI shows it:
/// the driver's own adapter is listed under that card's name. It is the
/// adapter holding the virtual display's path, or, before a monitor
/// arrives, the one adapter that was not there before the Enable
/// (`before`, by LUID). `None` when neither tells.
pub fn driver_render_card<'a>(
    adapters: &'a [Adapter],
    targets: &[Target],
    before: &[u64],
) -> Option<&'a str> {
    let on_path = targets
        .iter()
        .filter(|t| t.is_vdd())
        .flat_map(|t| std::iter::once(t.adapter).chain(t.source.map(|(adapter, _)| adapter)))
        .find_map(|luid| adapters.iter().find(|a| a.luid == luid));
    let new = || {
        let mut new = adapters.iter().filter(|a| !before.contains(&a.luid));
        match (new.next(), new.next()) {
            (Some(adapter), None) if !before.is_empty() => Some(adapter),
            _ => None,
        }
    };
    on_path.or_else(new).map(|a| a.description.as_str())
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The device state, or why there is none.
    pub device: Result<DeviceState, &'static str>,
    /// The graphics card the host asked the driver for, or why it asked for none.
    pub pin: Result<String, &'static str>,
    /// The card the driver renders on (`driver_render_card`).
    pub render: Option<String>,
    /// `HwSchMode`: 2 hardware-accelerated GPU scheduling on, 1 off.
    pub hags: Option<u32>,
    /// Whether the driver reads its settings from `C:\VirtualDisplayDriver`.
    pub default_settings_path: Option<bool>,
    pub adapters: Vec<Adapter>,
    pub targets: Vec<Target>,
}

/// Printable, without the separators this record uses, and short.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '|' | '[' | ']' | '='))
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

fn luid(value: u64) -> String {
    format!("{:x}", value)
}

/// The record's text, from data already read.
pub fn line(snapshot: &Snapshot) -> String {
    let mut out = match snapshot.device {
        Ok(device) => format!("device={device}"),
        Err(reason) => format!("device=unknown({})", clean(reason)),
    };
    match &snapshot.pin {
        Ok(name) => {
            let matched = snapshot
                .adapters
                .iter()
                .any(|a| crate::vdd_settings::same_card(&a.description, name));
            out += &format!(" pin=\"{}\" pin_matched={matched}", clean(name));
        }
        Err(reason) => out += &format!(" pin=none pin_reason=\"{}\"", clean(reason)),
    }
    match (&snapshot.render, &snapshot.pin) {
        (Some(card), Ok(pin)) => {
            let same = crate::vdd_settings::same_card(card, pin);
            out += &format!(" render=\"{}\" render_is_pin={same}", clean(card));
        }
        (Some(card), Err(_)) => out += &format!(" render=\"{}\"", clean(card)),
        (None, _) => out += " render=unknown",
    }
    out += match snapshot.hags {
        Some(2) => " hags=on",
        Some(1) => " hags=off",
        Some(_) => " hags=other",
        None => " hags=unset",
    };
    out += match snapshot.default_settings_path {
        Some(true) => " settings_path=default",
        Some(false) => " settings_path=other",
        None => " settings_path=unknown",
    };

    let adapters: Vec<String> = snapshot
        .adapters
        .iter()
        .take(MAX_ADAPTERS)
        .map(|a| {
            format!(
                "{} \"{}\" {:04x}:{:04x} driver={} outputs={}",
                luid(a.luid),
                clean(&a.description),
                a.vendor_id,
                a.device_id,
                clean(&a.driver),
                a.outputs
            )
        })
        .collect();
    out += &format!(" adapters=[{}", adapters.join(" | "));
    if snapshot.adapters.len() > MAX_ADAPTERS {
        out += &format!(" | +{} more", snapshot.adapters.len() - MAX_ADAPTERS);
    }
    out += "]";

    let targets: Vec<String> = snapshot
        .targets
        .iter()
        .take(MAX_TARGETS)
        .map(|t| {
            let vdd = t.is_vdd();
            let state = match t.source {
                Some((adapter, id)) => format!("active src={}/{id}", luid(adapter)),
                None if t.available => "inactive available".to_string(),
                None => "inactive unavailable".to_string(),
            };
            format!(
                "{state} tgt={}/{} tech={} vdd={}",
                luid(t.adapter),
                t.id,
                t.technology,
                if vdd { "yes" } else { "no" }
            )
        })
        .collect();
    out += &format!(" paths=[{}", targets.join(" | "));
    if snapshot.targets.len() > MAX_TARGETS {
        out += &format!(" | +{} more", snapshot.targets.len() - MAX_TARGETS);
    }
    out += "]";

    if out.len() > MAX_LINE {
        let mut end = MAX_LINE;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out += " ...";
    }
    out
}

/// Read the adapters, display paths and settings now. `before` holds the
/// adapters' LUIDs from before the Enable.
#[cfg(windows)]
pub fn collect(
    device: Result<DeviceState, &'static str>,
    pin: Result<String, &'static str>,
    before: &[u64],
) -> Snapshot {
    let settings_key = r"SOFTWARE\MikeTheTech\VirtualDisplayDriver";
    let path = crate::diagnostics::registry_string(settings_key, "VDDPATH");
    let (adapters, targets) = (adapters(), targets());
    Snapshot {
        device,
        pin,
        render: driver_render_card(&adapters, &targets, before).map(str::to_string),
        hags: crate::diagnostics::registry_dword(
            r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers",
            "HwSchMode",
        )
        .parse()
        .ok(),
        default_settings_path: (!path.starts_with("unavailable")).then(|| {
            path.trim_end_matches('\\')
                .eq_ignore_ascii_case(r"C:\VirtualDisplayDriver")
        }),
        adapters,
        targets,
    }
}

/// `driver_render_card` read now.
#[cfg(windows)]
pub fn render_card_now(before: &[u64]) -> Option<String> {
    driver_render_card(&adapters(), &targets(), before).map(str::to_string)
}

#[cfg(windows)]
pub use windows_impl::{adapters, targets};

#[cfg(windows)]
mod windows_impl {
    use super::{Adapter, Target};
    use windows::core::Interface;
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ALL_PATHS,
    };
    use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};

    /// `DISPLAYCONFIG_PATH_ACTIVE`.
    const PATH_ACTIVE: u32 = 1;

    fn value(luid: LUID) -> u64 {
        ((luid.HighPart as u32 as u64) << 32) | luid.LowPart as u64
    }

    fn text(wide: &[u16]) -> String {
        String::from_utf16_lossy(&wide[..wide.iter().position(|&c| c == 0).unwrap_or(wide.len())])
    }

    pub fn adapters() -> Vec<Adapter> {
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
            return Vec::new();
        };
        (0..)
            .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
            .filter_map(|adapter| {
                let desc = unsafe { adapter.GetDesc1() }.ok()?;
                let driver = match unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) } {
                    Ok(version) => {
                        let v = version as u64;
                        format!(
                            "{}.{}.{}.{}",
                            v >> 48,
                            (v >> 32) & 0xffff,
                            (v >> 16) & 0xffff,
                            v & 0xffff
                        )
                    }
                    Err(_) => "unknown".to_string(),
                };
                let outputs = (0..)
                    .take_while(|&index| unsafe { adapter.EnumOutputs(index) }.is_ok())
                    .count() as u32;
                Some(Adapter {
                    luid: value(desc.AdapterLuid),
                    description: text(&desc.Description),
                    vendor_id: desc.VendorId,
                    device_id: desc.DeviceId,
                    driver,
                    outputs,
                })
            })
            .collect()
    }

    /// Every active path, then each available target no active path uses.
    pub fn targets() -> Vec<Target> {
        let (mut paths, mut modes) = (0u32, 0u32);
        if unsafe { GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &mut paths, &mut modes) }
            != ERROR_SUCCESS
        {
            return Vec::new();
        }
        let mut path_info = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
        let mut mode_info = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
        if unsafe {
            QueryDisplayConfig(
                QDC_ALL_PATHS,
                &mut paths,
                path_info.as_mut_ptr(),
                &mut modes,
                mode_info.as_mut_ptr(),
                None,
            )
        } != ERROR_SUCCESS
        {
            return Vec::new();
        }
        path_info.truncate(paths as usize);
        let monitor_path = |path: &DISPLAYCONFIG_PATH_INFO| {
            let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            name.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            name.header.adapterId = path.targetInfo.adapterId;
            name.header.id = path.targetInfo.id;
            if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } == 0 {
                text(&name.monitorDevicePath)
            } else {
                String::new()
            }
        };
        let mut targets: Vec<Target> = Vec::new();
        let (active, inactive): (Vec<&DISPLAYCONFIG_PATH_INFO>, Vec<_>) = path_info
            .iter()
            .partition(|path| path.flags & PATH_ACTIVE != 0);
        for path in active {
            targets.push(Target {
                adapter: value(path.targetInfo.adapterId),
                id: path.targetInfo.id,
                source: Some((value(path.sourceInfo.adapterId), path.sourceInfo.id)),
                available: path.targetInfo.targetAvailable.as_bool(),
                technology: path.targetInfo.outputTechnology.0,
                monitor_path: monitor_path(path),
            });
        }
        for path in inactive {
            let (adapter, id) = (value(path.targetInfo.adapterId), path.targetInfo.id);
            if !path.targetInfo.targetAvailable.as_bool()
                || targets.iter().any(|t| t.adapter == adapter && t.id == id)
            {
                continue;
            }
            targets.push(Target {
                adapter,
                id,
                source: None,
                available: true,
                technology: path.targetInfo.outputTechnology.0,
                monitor_path: monitor_path(path),
            });
        }
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            device: Ok(DeviceState::NoMonitor),
            pin: Ok("Intel(R) Iris(R) Xe Graphics".into()),
            render: None,
            hags: Some(2),
            default_settings_path: Some(true),
            adapters: vec![
                Adapter {
                    luid: 0xd2c1,
                    description: "Intel(R) Iris(R) Xe Graphics".into(),
                    vendor_id: 0x8086,
                    device_id: 0x46a6,
                    driver: "32.0.101.7092".into(),
                    outputs: 1,
                },
                Adapter {
                    luid: 0x1_0000_e5f0,
                    description: "NVIDIA GeForce RTX 3050 Ti Laptop GPU".into(),
                    vendor_id: 0x10de,
                    device_id: 0x25a0,
                    driver: "32.0.16.1742".into(),
                    outputs: 0,
                },
            ],
            targets: vec![
                Target {
                    adapter: 0xd2c1,
                    id: 4352,
                    source: Some((0xd2c1, 0)),
                    available: true,
                    technology: i32::MIN,
                    monitor_path: r"\\?\DISPLAY#BOE0A1C#4&2b5bd9a1&0&UID8388688#{e6f07b5f}".into(),
                },
                Target {
                    adapter: 0x2_0000_0101,
                    id: 256,
                    source: None,
                    available: true,
                    technology: 5,
                    monitor_path: r"\\?\DISPLAY#MTT1337#1&28a6823a&3&UID256#{e6f07b5f}".into(),
                },
            ],
        }
    }

    #[test]
    fn the_snapshot_is_one_line_with_each_part() {
        assert_eq!(
            line(&snapshot()),
            "device=no_monitor pin=\"Intel(R) Iris(R) Xe Graphics\" pin_matched=true render=unknown hags=on \
             settings_path=default adapters=[d2c1 \"Intel(R) Iris(R) Xe Graphics\" 8086:46a6 \
             driver=32.0.101.7092 outputs=1 | 10000e5f0 \"NVIDIA GeForce RTX 3050 Ti Laptop GPU\" \
             10de:25a0 driver=32.0.16.1742 outputs=0] paths=[active src=d2c1/0 tgt=d2c1/4352 \
             tech=-2147483648 vdd=no | inactive available tgt=200000101/256 tech=5 vdd=yes]"
        );
        let mut unpinned = snapshot();
        unpinned.pin = Err("two graphics cards have the same name");
        unpinned.hags = None;
        unpinned.adapters.truncate(1);
        let text = line(&unpinned);
        assert!(
            text.contains(" pin=none pin_reason=\"two graphics cards have the same name\""),
            "{text}"
        );
        assert!(text.contains(" hags=unset"));
        // An Enable that failed without a device state says why.
        let mut unknown = snapshot();
        unknown.device = Err("display tasks missing");
        assert!(
            line(&unknown).starts_with("device=unknown(display tasks missing) pin="),
            "{}",
            line(&unknown)
        );
        let mut unmatched = snapshot();
        unmatched.pin = Ok("Intel(R) UHD Graphics".into());
        assert!(line(&unmatched).contains("pin_matched=false"));
        // The driver compares names ignoring case.
        let mut cased = snapshot();
        cased.pin = Ok("intel(r) iris(r) xe graphics".into());
        cased.render = Some("Intel(R) Iris(R) Xe Graphics".into());
        let text = line(&cased);
        assert!(
            text.contains(
                "pin_matched=true render=\"Intel(R) Iris(R) Xe Graphics\" render_is_pin=true"
            ),
            "{text}"
        );
        let mut elsewhere = snapshot();
        elsewhere.render = Some("NVIDIA GeForce RTX 3050 Ti Laptop GPU".into());
        assert!(line(&elsewhere).contains("render_is_pin=false"));
        elsewhere.pin = Err("no adapter drives the main screen");
        assert!(
            line(&elsewhere).contains(" render=\"NVIDIA GeForce RTX 3050 Ti Laptop GPU\" hags=")
        );
    }

    /// The driver's own adapter carries the render card's name: found by
    /// the virtual display's path, or as the one adapter new since the
    /// Enable when no monitor has arrived.
    #[test]
    fn the_render_card_is_the_drivers_adapter() {
        let card = |luid, name: &str| Adapter {
            luid,
            description: name.into(),
            vendor_id: 0,
            device_id: 0,
            driver: String::new(),
            outputs: 0,
        };
        // mainpc with the driver named to its AMD iGPU.
        let adapters = [
            card(0x11e00, "NVIDIA GeForce RTX 5080"),
            card(0x13c3b, "AMD Radeon(TM) Graphics"),
            card(0x66f8f4a0, "AMD Radeon(TM) Graphics"),
        ];
        let mut vdd = snapshot().targets[1].clone();
        (vdd.adapter, vdd.source) = (0x66f8f4a0, None);
        let before = [0x11e00, 0x13c3b];
        assert_eq!(
            driver_render_card(&adapters, &[vdd.clone()], &[]),
            Some("AMD Radeon(TM) Graphics")
        );
        // No monitor yet, so no path: the adapter that is new.
        assert_eq!(
            driver_render_card(&adapters, &[], &before),
            Some("AMD Radeon(TM) Graphics")
        );
        // Nothing new, nothing known before, or more than one new: no telling.
        assert_eq!(driver_render_card(&adapters[..2], &[], &before), None);
        assert_eq!(driver_render_card(&adapters, &[], &[]), None);
        assert_eq!(driver_render_card(&adapters, &[], &[0x11e00]), None);
    }

    #[test]
    fn the_virtual_displays_own_adapter_is_not_a_second_card() {
        // mainpc with the virtual display on: DXGI lists its adapter under
        // the RTX 5080's name, and its path runs on that adapter.
        let card = |luid, name: &str| Adapter {
            luid,
            description: name.into(),
            vendor_id: 0,
            device_id: 0,
            driver: String::new(),
            outputs: 0,
        };
        let adapters = [
            card(0x11e00, "NVIDIA GeForce RTX 5080"),
            card(0x13c3b, "AMD Radeon(TM) Graphics"),
            card(0x66f8f4a0, "NVIDIA GeForce RTX 5080"),
        ];
        let mut vdd = snapshot().targets[1].clone();
        (vdd.adapter, vdd.source) = (0x66f8f4a0, Some((0x66f8f4a0, 0)));
        let monitor = Target {
            adapter: 0x11e00,
            source: Some((0x11e00, 0)),
            ..snapshot().targets[0].clone()
        };
        let cards = render_adapters(&adapters, &[monitor.clone(), vdd]);
        let names: Vec<&str> = cards.iter().map(|a| a.description.as_str()).collect();
        assert_eq!(
            names,
            ["NVIDIA GeForce RTX 5080", "AMD Radeon(TM) Graphics"]
        );
        assert_eq!(
            crate::vdd_settings::gpu_to_pin(Some("NVIDIA GeForce RTX 5080"), &cards),
            Ok("NVIDIA GeForce RTX 5080")
        );
        // Two cards with one name, neither known to drive a screen, still
        // are not pinned.
        let twins = render_adapters(&adapters, &[monitor]);
        assert!(crate::vdd_settings::gpu_to_pin(Some("NVIDIA GeForce RTX 5080"), &twins).is_err());
    }

    /// Prints the card the driver renders on while the virtual display is
    /// on: `cargo test -p eternal-host --lib native_render_card -- --ignored`
    #[cfg(windows)]
    #[test]
    #[ignore = "reads this PC's adapters and display paths"]
    fn native_render_card() {
        println!("render={:?}", render_card_now(&[]));
    }

    #[test]
    fn monitor_paths_and_odd_names_stay_out_and_the_line_is_bounded() {
        let mut odd = snapshot();
        odd.adapters[0].description =
            "Card \"one\"\n| [x]=y with a very long name that keeps going and going".into();
        let text = line(&odd);
        // Paths hold monitor serial-style instance IDs.
        for secret in ["UID256", "UID8388688", "BOE0A1C", "28a6823a", "#", "\\\\?"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        assert!(!text.contains('\n'));
        assert!(
            text.contains("\"Card one xy with a very long name that keeps goi\""),
            "{text}"
        );

        let mut many = snapshot();
        many.adapters = vec![many.adapters[1].clone(); 40];
        many.targets = vec![many.targets[1].clone(); 200];
        let text = line(&many);
        assert!(text.len() <= MAX_LINE + 4, "{}", text.len());
        assert!(text.contains("| +32 more]"), "{text}");
    }
}
