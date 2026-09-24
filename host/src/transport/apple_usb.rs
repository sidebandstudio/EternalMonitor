//! Why USB cannot work yet: an iPad on the cable, and whether Apple Devices
//! is installed. Windows only reaches an iPad through Apple's device service,
//! and the Microsoft Store Apple Devices app runs that service only while the
//! app is open. The USB supervisor asks only when the service is unreachable.

/// The Microsoft Store package that provides Apple's device service.
pub const APPLE_DEVICES_FAMILY: &str = "AppleInc.AppleDevices_nzyj5cx40ttqa";
pub const APPLE_DEVICES_STORE_URL: &str = "https://apps.microsoft.com/detail/9np83lwlpz9k";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppleUsbState {
    /// iPhones and iPads attached by cable, whether or not Apple's service runs.
    pub cabled_devices: usize,
    /// The Microsoft Store Apple Devices app is installed for this user.
    pub apple_devices_installed: bool,
}

/// A present USB instance ID naming an iPhone or iPad itself, e.g.
/// `USB\VID_05AC&PID_12AB\00008112000625DA2E23C01E`. Its interface children
/// (`&MI_00`, `&MI_01`) and Apple keyboards, mice and adapters do not count.
pub fn is_apple_mobile_device(instance_id: &str) -> bool {
    let mut parts = instance_id.split('\\');
    if !parts
        .next()
        .is_some_and(|bus| bus.eq_ignore_ascii_case("USB"))
    {
        return false;
    }
    let Some(hardware) = parts.next() else {
        return false;
    };
    let hardware = hardware.to_ascii_uppercase();
    if hardware.contains("&MI_") {
        return false;
    }
    let Some(pid) = hardware
        .strip_prefix("VID_05AC&PID_")
        .and_then(|pid| u16::from_str_radix(pid.get(..4)?, 16).ok())
    else {
        return false;
    };
    // Apple assigns 0x12xx product IDs to iPhone, iPad and iPod touch.
    pid >> 8 == 0x12
}

pub fn probe() -> AppleUsbState {
    #[cfg(windows)]
    {
        AppleUsbState {
            cabled_devices: windows_impl::present_usb_ids()
                .iter()
                .filter(|id| is_apple_mobile_device(id))
                .count(),
            apple_devices_installed: windows_impl::apple_devices_installed(),
        }
    }
    #[cfg(not(windows))]
    {
        AppleUsbState::default()
    }
}

/// Opens the Apple Devices app, which starts Apple's device service.
pub fn open_apple_devices() -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe")
            .arg(format!(r"shell:AppsFolder\{APPLE_DEVICES_FAMILY}!App"))
            .spawn()
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod windows_impl {
    use windows::core::w;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_GETIDLIST_FILTER_ENUMERATOR,
        CM_GETIDLIST_FILTER_PRESENT, CR_SUCCESS,
    };

    /// Instance IDs of every present device on the USB bus.
    pub fn present_usb_ids() -> Vec<String> {
        let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
        // The list can grow between the size query and the read; retry once.
        for _ in 0..2 {
            let mut len = 0u32;
            if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, w!("USB"), flags) } != CR_SUCCESS {
                return Vec::new();
            }
            let mut buffer = vec![0u16; len as usize];
            if unsafe { CM_Get_Device_ID_ListW(w!("USB"), &mut buffer, flags) } == CR_SUCCESS {
                return buffer
                    .split(|&unit| unit == 0)
                    .filter(|id| !id.is_empty())
                    .map(String::from_utf16_lossy)
                    .collect();
            }
        }
        Vec::new()
    }

    /// Windows creates a package's data folder when it is installed for the
    /// user and removes it on uninstall.
    pub fn apple_devices_installed() -> bool {
        std::env::var_os("LOCALAPPDATA").is_some_and(|local| {
            std::path::Path::new(&local)
                .join("Packages")
                .join(super::APPLE_DEVICES_FAMILY)
                .is_dir()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::is_apple_mobile_device;

    #[test]
    fn counts_only_the_attached_iphone_or_ipad_itself() {
        // Captured on the reference PC with an iPad Pro on the cable.
        assert!(is_apple_mobile_device(
            r"USB\VID_05AC&PID_12AB\00008112000625DA2E23C01E"
        ));
        assert!(!is_apple_mobile_device(
            r"USB\VID_05AC&PID_12AB&MI_00\9&41CBFE6&0&0000"
        ));
        assert!(!is_apple_mobile_device(
            r"USB\VID_05AC&PID_12AB&MI_01\9&41CBFE6&0&0001"
        ));
        assert!(is_apple_mobile_device(r"usb\vid_05ac&pid_12a8\serial"));
        // An Apple keyboard, another vendor's device and non-USB buses.
        assert!(!is_apple_mobile_device(r"USB\VID_05AC&PID_0267\5&1234"));
        assert!(!is_apple_mobile_device(r"USB\VID_046D&PID_12AB\serial"));
        assert!(!is_apple_mobile_device(r"HID\VID_05AC&PID_12AB\serial"));
        assert!(!is_apple_mobile_device(r"USB\VID_05AC&PID_12"));
        assert!(!is_apple_mobile_device("USB"));
    }
}
