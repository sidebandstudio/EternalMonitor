//! Which graphics card Windows runs EternalMonitor on, and switching it when
//! that card cannot duplicate the screen.
//!
//! On a hybrid PC, DXGI lists every display under the adapter the process
//! runs on. Desktop Duplication then fails with DXGI_ERROR_UNSUPPORTED unless
//! that adapter is the one the display is wired to (0x887A0004 on 0.3.1
//! laptops). Windows picks the adapter from the per-app entry under
//! `UserGpuPreferences`, the same value Settings > System > Display > Graphics
//! writes, and reads it only when a process starts. So the switch writes
//! EternalMonitor's own entry, only after the user asks, and reopens the app.

/// A switch that would put EternalMonitor on the adapter driving the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSwitch {
    /// `GpuPreference` value: 1 = power saving, 2 = high performance.
    pub preference: u32,
    /// The adapter's name, for the notice.
    pub adapter_name: String,
}

/// An adapter LUID as (high, low), comparable across APIs.
pub type AdapterId = (i32, u32);

/// The preference that selects `display`, the adapter the screen is wired
/// to, given the first adapter Windows hands out for each preference. `None`
/// when neither preference reaches it, or when the process already runs on
/// it (`current`), so switching would change nothing.
pub fn preference_for(
    display: AdapterId,
    current: AdapterId,
    power_saving: AdapterId,
    high_performance: AdapterId,
) -> Option<u32> {
    if display == current {
        None
    } else if display == power_saving {
        Some(1)
    } else if display == high_performance {
        Some(2)
    } else {
        None
    }
}

/// Sets `GpuPreference` in a `UserGpuPreferences` value and keeps every other
/// setting Windows stores there (for example `SwapEffectUpgradeEnable=1;`).
pub fn with_gpu_preference(existing: Option<&str>, preference: u32) -> String {
    let mut value: String = existing
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter(|entry| {
            !entry
                .split('=')
                .next()
                .is_some_and(|key| key.trim().eq_ignore_ascii_case("GpuPreference"))
        })
        .map(|entry| format!("{entry};"))
        .collect();
    value.push_str(&format!("GpuPreference={preference};"));
    value
}

/// The argument a reopened copy gets: wait for this process to exit before
/// the single-instance check, which would otherwise send it away.
pub const AFTER_EXIT_ARG: &str = "--after-exit-of";

/// The arguments for a reopened copy: this process's own, without a wait
/// left from an earlier reopen, then the wait for `pid`.
pub fn reopen_args(args: impl IntoIterator<Item = String>, pid: u32) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == AFTER_EXIT_ARG {
            args.next();
        } else {
            kept.push(arg);
        }
    }
    kept.extend([AFTER_EXIT_ARG.to_string(), pid.to_string()]);
    kept
}

/// The process id a reopened copy must wait for, from its arguments.
pub fn waited_for_pid(args: impl IntoIterator<Item = String>) -> Option<u32> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == AFTER_EXIT_ARG {
            return args.next()?.parse().ok();
        }
    }
    None
}

#[cfg(windows)]
pub use windows_impl::{suggest_switch, switch_and_reopen, wait_for_previous_if_requested};

#[cfg(not(windows))]
pub fn wait_for_previous_if_requested() {}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt;
    use std::time::Duration;

    use tracing::{info, warn};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, LUID};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory6, DXGI_GPU_PREFERENCE,
        DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, DXGI_GPU_PREFERENCE_MINIMUM_POWER,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPEN_CREATE_OPTIONS, REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    use super::{preference_for, with_gpu_preference, AdapterId, GpuSwitch};

    const PREFERENCES_KEY: PCWSTR = w!("Software\\Microsoft\\DirectX\\UserGpuPreferences");
    /// How long a reopened copy waits for the old one to shut down.
    const WAIT_FOR_PREVIOUS: Duration = Duration::from_secs(30);

    fn id(luid: LUID) -> AdapterId {
        (luid.HighPart, luid.LowPart)
    }

    /// For a display EternalMonitor's adapter cannot duplicate, the switch
    /// that moves it to the adapter the display is wired to. `current` is the
    /// adapter DXGI listed the display under.
    pub fn suggest_switch(device_name: &str, current: LUID) -> Option<GpuSwitch> {
        let wired = display_adapter(device_name)?;
        let factory: IDXGIFactory6 = unsafe { CreateDXGIFactory1::<IDXGIFactory6>() }.ok()?;
        let first = |preference: DXGI_GPU_PREFERENCE| -> Option<(AdapterId, String)> {
            let adapter: IDXGIAdapter1 =
                unsafe { factory.EnumAdapterByGpuPreference(0, preference) }.ok()?;
            let desc = unsafe { adapter.GetDesc1() }.ok()?;
            let name = String::from_utf16_lossy(
                &desc
                    .Description
                    .iter()
                    .copied()
                    .take_while(|&c| c != 0)
                    .collect::<Vec<_>>(),
            );
            Some((id(desc.AdapterLuid), name))
        };
        let (power_saving, power_saving_name) = first(DXGI_GPU_PREFERENCE_MINIMUM_POWER)?;
        let (high_performance, high_performance_name) =
            first(DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)?;
        let preference = preference_for(wired, id(current), power_saving, high_performance);
        info!(
            device = device_name,
            display_adapter = ?wired,
            running_on = ?id(current),
            power_saving = %power_saving_name,
            high_performance = %high_performance_name,
            preference,
            "Checked which graphics card drives the screen"
        );
        let adapter_name = match preference? {
            1 => power_saving_name,
            _ => high_performance_name,
        };
        Some(GpuSwitch {
            preference: preference?,
            adapter_name,
        })
    }

    /// The adapter that scans out `device_name` (`\\.\DISPLAY1`). Display
    /// configuration paths report the physical adapter, unlike DXGI, which
    /// lists outputs under the adapter the process runs on.
    fn display_adapter(device_name: &str) -> Option<AdapterId> {
        let (mut paths_len, mut modes_len) = (0u32, 0u32);
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths_len, &mut modes_len)
        } != ERROR_SUCCESS
        {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); paths_len as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); modes_len as usize];
        if unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut paths_len,
                paths.as_mut_ptr(),
                &mut modes_len,
                modes.as_mut_ptr(),
                None,
            )
        } != ERROR_SUCCESS
        {
            return None;
        }
        paths.truncate(paths_len as usize);
        paths.into_iter().find_map(|path| {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
                return None;
            }
            let name = String::from_utf16_lossy(
                &source
                    .viewGdiDeviceName
                    .iter()
                    .copied()
                    .take_while(|&c| c != 0)
                    .collect::<Vec<_>>(),
            );
            (name == device_name).then(|| id(path.sourceInfo.adapterId))
        })
    }

    /// Writes EternalMonitor's own per-app graphics preference and starts a
    /// copy that waits for this process to exit. The caller then quits.
    pub fn switch_and_reopen(switch: &GpuSwitch) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let name: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
        let mut key = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PREFERENCES_KEY,
                0,
                PCWSTR::null(),
                REG_OPEN_CREATE_OPTIONS(REG_OPTION_NON_VOLATILE.0),
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("Could not open the graphics settings: {status:?}"));
        }
        let existing = read_string(key, PCWSTR(name.as_ptr()));
        let value = with_gpu_preference(existing.as_deref(), switch.preference);
        let bytes: Vec<u8> = value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let status = unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(&bytes)) };
        unsafe {
            let _ = RegCloseKey(key);
        }
        if status != ERROR_SUCCESS {
            return Err(format!("Could not save the graphics setting: {status:?}"));
        }
        info!(
            before = existing.as_deref().unwrap_or("(none)"),
            after = %value,
            adapter = %switch.adapter_name,
            "Saved EternalMonitor's graphics card choice; reopening"
        );
        // Keep the user's own arguments (a custom port is the first one).
        std::process::Command::new(&exe)
            .args(super::reopen_args(
                std::env::args().skip(1),
                std::process::id(),
            ))
            .spawn()
            .map_err(|error| format!("Could not reopen EternalMonitor: {error}"))?;
        Ok(())
    }

    fn read_string(key: HKEY, name: PCWSTR) -> Option<String> {
        let mut size = 0u32;
        if unsafe { RegQueryValueExW(key, name, None, None, None, Some(&mut size)) }
            != ERROR_SUCCESS
        {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        if unsafe {
            RegQueryValueExW(
                key,
                name,
                None,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        } != ERROR_SUCCESS
        {
            return None;
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }

    /// In a copy started by [`switch_and_reopen`], waits for the previous
    /// process to exit so the single-instance check lets this one run.
    pub fn wait_for_previous_if_requested() {
        let Some(pid) = super::waited_for_pid(std::env::args().skip(1)) else {
            return;
        };
        let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
            return; // Already gone.
        };
        let waited = unsafe { WaitForSingleObject(process, WAIT_FOR_PREVIOUS.as_millis() as u32) };
        unsafe {
            let _ = CloseHandle(process);
        }
        if waited.0 != 0 {
            warn!(pid, "The previous EternalMonitor did not exit in time");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IGPU: AdapterId = (0, 0x1111);
    const DGPU: AdapterId = (0, 0x2222);

    #[test]
    fn the_switch_targets_the_adapter_the_screen_is_wired_to() {
        // A laptop running on its NVIDIA GPU with the panel on the iGPU: the
        // 0.3.1 field case.
        assert_eq!(preference_for(IGPU, DGPU, IGPU, DGPU), Some(1));
        // mainpc forced to power saving with its monitors on the RTX card.
        assert_eq!(preference_for(DGPU, IGPU, IGPU, DGPU), Some(2));
        // Already on the right adapter: switching cannot help.
        assert_eq!(preference_for(IGPU, IGPU, IGPU, DGPU), None);
        // A third adapter (a dock or eGPU) that no preference selects.
        assert_eq!(preference_for((0, 0x3333), DGPU, IGPU, DGPU), None);
    }

    #[test]
    fn only_the_gpu_preference_changes_in_the_stored_value() {
        assert_eq!(with_gpu_preference(None, 1), "GpuPreference=1;");
        assert_eq!(
            with_gpu_preference(Some("GpuPreference=2;"), 1),
            "GpuPreference=1;"
        );
        assert_eq!(
            with_gpu_preference(
                Some("SwapEffectUpgradeEnable=1;GpuPreference=2;VRROptimizeEnable=0;"),
                1
            ),
            "SwapEffectUpgradeEnable=1;VRROptimizeEnable=0;GpuPreference=1;"
        );
        assert_eq!(
            with_gpu_preference(Some("AutoHDREnable=1"), 2),
            "AutoHDREnable=1;GpuPreference=2;"
        );
    }

    #[test]
    fn a_reopened_copy_finds_the_process_to_wait_for() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(waited_for_pid(args(&[AFTER_EXIT_ARG, "4242"])), Some(4242));
        assert_eq!(waited_for_pid(args(&["--minimized"])), None);
        assert_eq!(waited_for_pid(args(&[AFTER_EXIT_ARG])), None);
        assert_eq!(waited_for_pid(args(&[AFTER_EXIT_ARG, "x"])), None);
        // The port stays first and an earlier wait is dropped.
        let reopened = reopen_args(args(&["19876", AFTER_EXIT_ARG, "7"]), 4242);
        assert_eq!(reopened, args(&["19876", AFTER_EXIT_ARG, "4242"]));
        assert_eq!(waited_for_pid(reopened), Some(4242));
    }
}
