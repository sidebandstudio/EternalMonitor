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

/// The graphics card the screen is wired to, and the preference that would
/// put EternalMonitor on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSwitch {
    /// `GpuPreference` value to write (1 = power saving, 2 = high
    /// performance), or `None` when no value is certain to select the card.
    /// The notice then explains how to pick it in Windows Settings instead.
    pub preference: Option<u32>,
    /// The adapter's name, for the notice.
    pub adapter_name: String,
}

/// An adapter LUID as (high, low), comparable across APIs.
pub type AdapterId = (i32, u32);

/// How Windows ranks an adapter for the per-app graphics preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    /// Built into the processor: what power saving selects.
    Integrated,
    /// An internal graphics card.
    Discrete,
    /// An external GPU. High performance ranks these ahead of discrete ones.
    Detachable,
    /// Not integrated, but this Windows cannot say whether it is detachable.
    NotIntegrated,
    /// Not a hardware graphics adapter (software rendering, a virtual
    /// display): never selected by a preference.
    Ignored,
    /// Windows could not classify it.
    Unknown,
}

/// The preference certain to select `display`, the adapter the screen is
/// wired to, following `EnumAdapterByGpuPreference`'s documented order: power
/// saving puts integrated adapters first; high performance puts detachable
/// adapters first, then the other discrete ones. Video memory says nothing
/// here: a 16 GB internal card loses to an 8 GB eGPU. `None` when the process
/// already runs on that adapter, or when several adapters share its rank or
/// one could not be classified, so the choice is not certain.
pub fn preference_for(
    display: AdapterId,
    current: AdapterId,
    adapters: &[(AdapterId, AdapterKind)],
) -> Option<u32> {
    if display == current {
        return None;
    }
    let ranked: Vec<_> = adapters
        .iter()
        .filter(|(_, kind)| *kind != AdapterKind::Ignored)
        .collect();
    let count = |wanted: &[AdapterKind]| {
        ranked
            .iter()
            .filter(|(_, kind)| wanted.contains(kind))
            .count()
    };
    if count(&[AdapterKind::Unknown]) > 0 {
        return None;
    }
    let kind = ranked.iter().find(|(id, _)| *id == display)?.1;
    match kind {
        AdapterKind::Integrated if count(&[AdapterKind::Integrated]) == 1 => Some(1),
        AdapterKind::Detachable if count(&[AdapterKind::Detachable]) == 1 => Some(2),
        AdapterKind::Discrete | AdapterKind::NotIntegrated
            if count(&[
                AdapterKind::Discrete,
                AdapterKind::Detachable,
                AdapterKind::NotIntegrated,
            ]) == 1 =>
        {
            Some(2)
        }
        _ => None,
    }
}

/// What the notice may offer for `switch`. After a switch EternalMonitor
/// made (`switch_made`, saved in its own settings until capture works),
/// capture failing again means the switch did not help: offering it again
/// would reopen the app in a loop, so the notice explains Windows Settings.
pub fn offered(switch: Option<GpuSwitch>, switch_made: bool) -> Option<GpuSwitch> {
    switch.map(|switch| GpuSwitch {
        preference: switch.preference.filter(|_| !switch_made),
        ..switch
    })
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
/// With [`AFTER_EXIT_ARG`]: once the old process is gone, save this
/// `GpuPreference` and start the app again. Windows reads the preference
/// only when a process starts, and writing it while EternalMonitor still
/// used the GPU hung that process's next Desktop Duplication call (mainpc).
pub const GPU_PREFERENCE_ARG: &str = "--set-gpu-preference";

/// The user's own arguments (a custom port is the first one), without the
/// ones a reopen adds.
pub fn own_args(args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == AFTER_EXIT_ARG || arg == GPU_PREFERENCE_ARG {
            args.next();
        } else {
            kept.push(arg);
        }
    }
    kept
}

/// The arguments for a copy that waits for `pid` to exit and, with a
/// `preference`, saves it before starting the app again.
pub fn reopen_args(
    args: impl IntoIterator<Item = String>,
    pid: u32,
    preference: Option<u32>,
) -> Vec<String> {
    let mut kept = own_args(args);
    kept.extend([AFTER_EXIT_ARG.to_string(), pid.to_string()]);
    if let Some(preference) = preference {
        kept.extend([GPU_PREFERENCE_ARG.to_string(), preference.to_string()]);
    }
    kept
}

fn arg_value(args: impl IntoIterator<Item = String>, name: &str) -> Option<u32> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == name {
            return args.next()?.parse().ok();
        }
    }
    None
}

/// The process id a reopened copy must wait for, from its arguments.
pub fn waited_for_pid(args: impl IntoIterator<Item = String>) -> Option<u32> {
    arg_value(args, AFTER_EXIT_ARG)
}

/// The `GpuPreference` a reopened copy must save, from its arguments.
pub fn requested_preference(args: impl IntoIterator<Item = String>) -> Option<u32> {
    arg_value(args, GPU_PREFERENCE_ARG).filter(|value| matches!(value, 1 | 2))
}

#[cfg(windows)]
pub use windows_impl::{suggest_switch, switch_and_reopen, wait_for_previous_if_requested};

#[cfg(not(windows))]
pub fn wait_for_previous_if_requested() {}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt;
    use std::time::Duration;

    use std::ffi::c_void;
    use tracing::info;

    use windows::core::{s, w, Interface, GUID, HRESULT, PCWSTR};
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, LUID};
    use windows::Win32::Graphics::DXCore::{
        DXCoreAdapterProperty, IDXCoreAdapter, IDXCoreAdapterFactory, IDXCoreAdapterList,
        InstanceLuid, IsDetachable, IsHardware, IsIntegrated,
        DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPEN_CREATE_OPTIONS, REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    use super::{preference_for, with_gpu_preference, AdapterId, AdapterKind, GpuSwitch};

    const PREFERENCES_KEY: PCWSTR = w!("Software\\Microsoft\\DirectX\\UserGpuPreferences");
    /// How long a reopened copy waits for the old one to shut down.
    const WAIT_FOR_PREVIOUS: Duration = Duration::from_secs(30);

    fn id(luid: LUID) -> AdapterId {
        (luid.HighPart, luid.LowPart)
    }

    /// For a display EternalMonitor's adapter cannot duplicate, the card the
    /// display is wired to and the preference certain to select it, if any.
    /// `current` is the adapter DXGI listed the display under. `None` when
    /// the wired adapter cannot be found or named.
    pub fn suggest_switch(device_name: &str, current: LUID) -> Option<GpuSwitch> {
        let wired = display_adapter(device_name)?;
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
        let mut names = Vec::new();
        for index in 0.. {
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                continue;
            };
            let name = String::from_utf16_lossy(
                &desc
                    .Description
                    .iter()
                    .copied()
                    .take_while(|&c| c != 0)
                    .collect::<Vec<_>>(),
            );
            names.push((id(desc.AdapterLuid), name));
        }
        let kinds = classify_adapters();
        let preference = kinds
            .as_deref()
            .and_then(|kinds| preference_for(wired, id(current), kinds));
        let adapter_name = names
            .iter()
            .find(|(luid, _)| *luid == wired)
            .map(|(_, name)| name.clone());
        info!(
            device = device_name,
            display_adapter = ?wired,
            display_adapter_name = adapter_name.as_deref().unwrap_or("unknown"),
            running_on = ?id(current),
            adapters = ?kinds,
            preference,
            "Checked which graphics card drives the screen"
        );
        Some(GpuSwitch {
            preference,
            adapter_name: adapter_name?,
        })
    }

    /// Every adapter DXCore lists for Direct3D 11, by how the per-app
    /// preference ranks it. `None` when DXCore is missing (it is loaded at
    /// run time: Windows 10 1809, which the installer accepts, may lack it).
    fn classify_adapters() -> Option<Vec<(AdapterId, AdapterKind)>> {
        type CreateFactory = unsafe extern "system" fn(*const GUID, *mut *mut c_void) -> HRESULT;
        let module = unsafe { LoadLibraryW(w!("dxcore.dll")) }.ok()?;
        let create = unsafe { GetProcAddress(module, s!("DXCoreCreateAdapterFactory")) }?;
        let create: CreateFactory = unsafe { std::mem::transmute(create) };
        let mut raw = std::ptr::null_mut();
        unsafe { create(&IDXCoreAdapterFactory::IID, &mut raw) }
            .ok()
            .ok()?;
        let factory = unsafe { IDXCoreAdapterFactory::from_raw(raw) };
        let list: IDXCoreAdapterList =
            unsafe { factory.CreateAdapterList(&[DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS]) }
                .ok()?;
        let mut adapters = Vec::new();
        for index in 0..unsafe { list.GetAdapterCount() } {
            let Ok(adapter) = (unsafe { list.GetAdapter::<IDXCoreAdapter>(index) }) else {
                continue;
            };
            let mut luid = LUID::default();
            if unsafe {
                adapter.GetProperty(
                    InstanceLuid,
                    std::mem::size_of::<LUID>(),
                    (&mut luid as *mut LUID).cast(),
                )
            }
            .is_err()
            {
                continue;
            }
            let flag = |property: DXCoreAdapterProperty| -> Option<bool> {
                if !unsafe { adapter.IsPropertySupported(property) } {
                    return None;
                }
                let mut value = 0u8;
                unsafe { adapter.GetProperty(property, 1, (&mut value as *mut u8).cast()) }.ok()?;
                Some(value != 0)
            };
            let kind = match (flag(IsHardware), flag(IsIntegrated), flag(IsDetachable)) {
                (Some(false), _, _) => AdapterKind::Ignored,
                (_, None, _) => AdapterKind::Unknown,
                (_, Some(true), _) => AdapterKind::Integrated,
                (_, Some(false), Some(true)) => AdapterKind::Detachable,
                (_, Some(false), Some(false)) => AdapterKind::Discrete,
                (_, Some(false), None) => AdapterKind::NotIntegrated,
            };
            adapters.push((id(luid), kind));
        }
        Some(adapters)
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

    /// Starts a copy that waits for this process to exit, saves the
    /// preference, and opens the app again. The caller then quits. Only for a
    /// switch with a certain preference. This process never writes the
    /// preference itself: it still uses the GPU until it exits.
    pub fn switch_and_reopen(switch: &GpuSwitch) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let preference = switch
            .preference
            .ok_or("no graphics card setting is certain to select this card")?;
        info!(
            preference,
            adapter = %switch.adapter_name,
            "Reopening EternalMonitor to switch its graphics card"
        );
        std::process::Command::new(&exe)
            .args(super::reopen_args(
                std::env::args().skip(1),
                std::process::id(),
                Some(preference),
            ))
            .spawn()
            .map_err(|error| format!("Could not reopen EternalMonitor: {error}"))?;
        Ok(())
    }

    /// Saves `preference` in EternalMonitor's own `UserGpuPreferences`
    /// value, keeping its other settings. Returns the value before and after.
    fn save_preference(preference: u32) -> Result<(Option<String>, String), String> {
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
        let value = with_gpu_preference(existing.as_deref(), preference);
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
        Ok((existing, value))
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
    /// process to exit so the single-instance check lets this one run. With a
    /// preference to save, saves it and starts the app again, which is when
    /// Windows applies it, then exits. Runs before logging starts.
    pub fn wait_for_previous_if_requested() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let Some(pid) = super::waited_for_pid(args.clone()) else {
            return;
        };
        if let Ok(process) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
            let waited =
                unsafe { WaitForSingleObject(process, WAIT_FOR_PREVIOUS.as_millis() as u32) };
            unsafe {
                let _ = CloseHandle(process);
            }
            if waited.0 != 0 {
                eprintln!("The previous EternalMonitor (pid {pid}) did not exit in time");
            }
        }
        let Some(preference) = super::requested_preference(args.clone()) else {
            return;
        };
        match save_preference(preference) {
            Ok((before, after)) => eprintln!("Saved GpuPreference {before:?} -> {after:?}"),
            Err(error) => eprintln!("{error}"),
        }
        // If saving failed, the app opens as before: the saved switch marker
        // then makes the notice explain Windows Settings instead.
        if let Ok(exe) = std::env::current_exe() {
            if let Err(error) = std::process::Command::new(exe)
                .args(super::own_args(args))
                .spawn()
            {
                eprintln!("Could not reopen EternalMonitor: {error}");
            }
        }
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IGPU: AdapterId = (0, 0x1111);
    const DGPU: AdapterId = (0, 0x2222);

    use AdapterKind::*;

    #[test]
    fn the_switch_targets_the_adapter_the_screen_is_wired_to() {
        let laptop = [(IGPU, Integrated), (DGPU, Discrete)];
        // A laptop running on its NVIDIA GPU with the panel on the iGPU: the
        // 0.3.1 field case.
        assert_eq!(preference_for(IGPU, DGPU, &laptop), Some(1));
        // mainpc forced to power saving with its monitors on the RTX card.
        assert_eq!(preference_for(DGPU, IGPU, &laptop), Some(2));
        // Already on the right adapter: switching cannot help.
        assert_eq!(preference_for(IGPU, IGPU, &laptop), None);
        // A Windows without the detachable property still has one choice.
        assert_eq!(
            preference_for(DGPU, IGPU, &[(IGPU, Integrated), (DGPU, NotIntegrated)]),
            Some(2)
        );
    }

    #[test]
    fn an_egpu_takes_high_performance_from_a_larger_internal_card() {
        // Sol's case: a 16 GB internal card (screen) and an 8 GB eGPU. High
        // performance selects the eGPU, so no value reaches the internal card.
        let internal = (0, 0x4444);
        let egpu = (0, 0x5555);
        let pc = [(IGPU, Integrated), (internal, Discrete), (egpu, Detachable)];
        assert_eq!(preference_for(internal, IGPU, &pc), None);
        // A screen on the eGPU or on the iGPU is still certain.
        assert_eq!(preference_for(egpu, IGPU, &pc), Some(2));
        assert_eq!(preference_for(IGPU, internal, &pc), Some(1));
    }

    #[test]
    fn an_igpu_only_pc_or_two_discrete_cards_give_no_certain_switch() {
        // Only an iGPU: the process already runs on it.
        assert_eq!(preference_for(IGPU, IGPU, &[(IGPU, Integrated)]), None);
        // RTX 4090 + RTX 3080 (+ Radeon iGPU): high performance could pick
        // either card.
        let other = (0, 0x3333);
        let pc = [(DGPU, Discrete), (other, Discrete), (IGPU, Integrated)];
        assert_eq!(preference_for(DGPU, IGPU, &pc), None);
        assert_eq!(preference_for(other, IGPU, &pc), None);
        assert_eq!(preference_for(IGPU, DGPU, &pc), Some(1));
        // An adapter Windows could not classify makes nothing certain.
        let unknown = [(IGPU, Integrated), (DGPU, Discrete), (other, Unknown)];
        assert_eq!(preference_for(DGPU, IGPU, &unknown), None);
    }

    #[test]
    fn a_switch_that_did_not_help_is_not_offered_again() {
        let switch = || {
            Some(GpuSwitch {
                preference: Some(2),
                adapter_name: "NVIDIA GeForce RTX 5080".into(),
            })
        };
        assert_eq!(offered(switch(), false), switch());
        let after = offered(switch(), true).unwrap();
        assert_eq!(after.preference, None);
        assert_eq!(after.adapter_name, "NVIDIA GeForce RTX 5080");
        assert_eq!(offered(None, true), None);
    }

    #[test]
    fn a_virtual_display_adapter_does_not_count() {
        let vdd = (0, 0x6666);
        let pc = [(IGPU, Integrated), (DGPU, Discrete), (vdd, Ignored)];
        assert_eq!(preference_for(DGPU, IGPU, &pc), Some(2));
        // A screen on the virtual display itself has no preference.
        assert_eq!(preference_for(vdd, IGPU, &pc), None);
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
        // The port stays first and an earlier reopen's arguments are dropped.
        let reopened = reopen_args(
            args(&["19876", AFTER_EXIT_ARG, "7", GPU_PREFERENCE_ARG, "1"]),
            4242,
            Some(2),
        );
        assert_eq!(
            reopened,
            args(&["19876", AFTER_EXIT_ARG, "4242", GPU_PREFERENCE_ARG, "2"])
        );
        assert_eq!(waited_for_pid(reopened.clone()), Some(4242));
        assert_eq!(requested_preference(reopened.clone()), Some(2));
        // The app the helper starts gets only the user's own arguments.
        assert_eq!(own_args(reopened), args(&["19876"]));
        assert_eq!(requested_preference(args(&[GPU_PREFERENCE_ARG, "3"])), None);
        let plain = reopen_args(args(&[]), 9, None);
        assert_eq!(requested_preference(plain), None);
    }
}
