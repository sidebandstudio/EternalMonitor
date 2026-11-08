//! What Windows reports for the bundled virtual display driver's device, and
//! what the Stream page tells the user to do about it.
//!
//! The Enable task used to succeed whenever the device reported no problem
//! code right after it was enabled. The display could still never appear, and
//! all the host could say was that it did not attach in time. The task now
//! says why it failed (installer/scripts/vdd-toggle.ps1), and the host reads
//! the device itself when the display does not appear.

use std::fmt;

/// cfgmgr32.h devnode status flags.
#[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
const DN_STARTED: u32 = 0x8;
#[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
const DN_HAS_PROBLEM: u32 = 0x400;

/// The driver's hardware ID, as its INF and vdd-toggle.ps1 match it.
#[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
const HARDWARE_ID: &str = r"Root\MttVDD";

/// The driver's device, as the Enable task or Configuration Manager reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// No present device has the driver's hardware ID.
    Missing,
    /// The device exists without a driver bound to it.
    NoDriver,
    /// Windows reports this problem code. 22 is "disabled".
    Problem(u32),
    /// No problem code, but Windows has not started the device.
    NotStarted,
    /// The device started, but its monitor did not arrive.
    NoMonitor,
    /// The device and its monitor are running.
    Ready,
}

/// What the user can do about a second screen that did not appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VddProblem {
    /// The driver or its device is gone: run Setup again.
    NotInstalled,
    /// Windows could not verify the driver's signature (code 52).
    SignatureBlocked,
    /// The driver failed, with Windows' problem code when there is one.
    DriverError(Option<u32>),
    /// The driver runs, but Windows added no screen for it.
    NoDisplay,
    /// Windows added the screen but kept the desktop off it.
    NotExtended,
}

/// A device's monitor children, as Configuration Manager listed them.
#[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Monitors {
    /// The list was read and is empty.
    None,
    /// At least one child started without a problem.
    Started,
    /// Every child was read, and none has started yet.
    Pending,
    /// The list, or a child's status, couldn't be read.
    Unread,
}

impl Monitors {
    /// From every child's status flags (`None` when a child's couldn't be
    /// read), or `None` when the children couldn't be listed. A started
    /// child counts even when another couldn't be read.
    #[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
    fn from_children(children: Option<&[Option<u32>]>) -> Self {
        let Some(children) = children else {
            return Self::Unread;
        };
        let started = |flags: &u32| flags & DN_STARTED != 0 && flags & DN_HAS_PROBLEM == 0;
        if children.iter().flatten().any(started) {
            Self::Started
        } else if children.is_empty() {
            Self::None
        } else if children.iter().any(Option::is_none) {
            Self::Unread
        } else {
            Self::Pending
        }
    }
}

impl DeviceState {
    /// The Enable task's result, as vdd-toggle.ps1 exits.
    pub fn from_task_result(code: i64) -> Option<Self> {
        Some(match code {
            2 => Self::Missing,
            3 => Self::NoDriver,
            4 => Self::NotStarted,
            5 => Self::NoMonitor,
            101..=199 => Self::Problem((code - 100) as u32),
            _ => return None,
        })
    }

    /// The device as Configuration Manager reports it: its status flags and
    /// problem number, whether a driver is bound, and its monitor children.
    /// A running device without a started monitor reads as `NoMonitor`, for
    /// the snapshot and the notice once the display did not appear.
    #[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
    fn from_devnode(status: u32, problem: u32, has_driver: bool, monitors: Monitors) -> Self {
        if status & DN_HAS_PROBLEM != 0 {
            Self::Problem(problem)
        } else if !has_driver {
            Self::NoDriver
        } else if status & DN_STARTED == 0 {
            Self::NotStarted
        } else if monitors == Monitors::Started {
            Self::Ready
        } else {
            Self::NoMonitor
        }
    }

    /// As `from_devnode`, for a decision made while the display may still be
    /// arriving: `NoMonitor` only when the device runs and Windows reported
    /// no child at all. A child that has not started, or children that could
    /// not all be read, are `None`: the monitor may be on its way.
    #[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
    fn confirmed(status: u32, problem: u32, has_driver: bool, monitors: Monitors) -> Option<Self> {
        let state = Self::from_devnode(status, problem, has_driver, monitors);
        match (state, monitors) {
            (Self::NoMonitor, Monitors::Pending | Monitors::Unread) => None,
            (state, _) => Some(state),
        }
    }

    /// With more than one device, the one closest to showing a display.
    #[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
    fn best(states: impl IntoIterator<Item = Self>) -> Self {
        let rank = |state: &Self| match state {
            Self::Missing => 0,
            Self::NoDriver => 1,
            Self::Problem(_) => 2,
            Self::NotStarted => 3,
            Self::NoMonitor => 4,
            Self::Ready => 5,
        };
        states.into_iter().max_by_key(rank).unwrap_or(Self::Missing)
    }

    /// What to tell the user once the display did not appear. A device that
    /// is ready with its monitor means Windows kept the desktop off it.
    pub fn problem(self) -> VddProblem {
        match self {
            // Not configured, reinstall needed, or the driver install failed.
            Self::Missing | Self::NoDriver | Self::Problem(1 | 18 | 28) => VddProblem::NotInstalled,
            Self::Problem(52) => VddProblem::SignatureBlocked,
            Self::Problem(code) => VddProblem::DriverError(Some(code)),
            Self::NotStarted => VddProblem::DriverError(None),
            Self::NoMonitor => VddProblem::NoDisplay,
            Self::Ready => VddProblem::NotExtended,
        }
    }
}

/// Stable names for logs and diagnostics. No instance IDs or other detail.
impl fmt::Display for DeviceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("missing"),
            Self::NoDriver => f.write_str("no_driver"),
            Self::Problem(code) => write!(f, "problem_{code}"),
            Self::NotStarted => f.write_str("not_started"),
            Self::NoMonitor => f.write_str("no_monitor"),
            Self::Ready => f.write_str("ready"),
        }
    }
}

/// The Enable task's result from vdd_task.ps1's failure message,
/// "VDD task failed: <task>, result=<code>".
pub fn from_task_stderr(stderr: &str) -> Option<DeviceState> {
    let digits: String = stderr
        .split("result=")
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    DeviceState::from_task_result(digits.parse().ok()?)
}

/// Read the driver's device from Configuration Manager.
#[cfg(windows)]
pub fn query() -> DeviceState {
    DeviceState::best(read_devices().into_iter().filter_map(|read| {
        let (flags, problem, has_driver, monitors) = read?;
        Some(DeviceState::from_devnode(
            flags, problem, has_driver, monitors,
        ))
    }))
}

/// As `query`, for the attach wait: `NoMonitor` only when every running
/// device of the driver has confirmed no monitor child (see
/// `DeviceState::confirmed`). `None` when that isn't certain: a device or
/// its children couldn't be read, or a monitor hasn't started yet.
#[cfg(windows)]
pub fn query_confirmed() -> Option<DeviceState> {
    confirmed_from(read_devices())
}

/// `query_confirmed` from each driver device's read (`None` when its status
/// couldn't be read). A device with a monitor wins; otherwise anything
/// uncertain makes the whole read uncertain.
#[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
fn confirmed_from(reads: Vec<Option<(u32, u32, bool, Monitors)>>) -> Option<DeviceState> {
    let states: Vec<Option<DeviceState>> = reads
        .into_iter()
        .map(|read| {
            let (flags, problem, has_driver, monitors) = read?;
            DeviceState::confirmed(flags, problem, has_driver, monitors)
        })
        .collect();
    if states.contains(&Some(DeviceState::Ready)) {
        return Some(DeviceState::Ready);
    }
    let states: Option<Vec<DeviceState>> = states.into_iter().collect();
    Some(DeviceState::best(states?))
}

/// Each present device of the driver: its status flags and problem code,
/// whether a driver is bound, and its monitor children; `None` for one whose
/// status couldn't be read.
#[cfg(windows)]
fn read_devices() -> Vec<Option<(u32, u32, bool, Monitors)>> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::CM_DRP_DRIVER;
    let Some(devnodes) = root_devnodes() else {
        return Vec::new();
    };
    reads_from(&devnodes, |devinst, (flags, problem)| {
        let has_driver = matches!(registry_property(devinst, CM_DRP_DRIVER), Ok(Some(_)));
        let children = children(devinst);
        (
            flags,
            problem,
            has_driver,
            Monitors::from_children(children.as_deref()),
        )
    })
}

/// `read_devices` from the present root devices, with `read` reading one
/// device of the driver whose status was read. A root device that couldn't
/// be read may be the driver's, with a monitor of its own: it stays, as a
/// read that isn't certain. Only devices known to be another driver's are
/// left out.
#[cfg(any(windows, test))]
fn reads_from(
    devnodes: &[Devnode],
    mut read: impl FnMut(u32, (u32, u32)) -> (u32, u32, bool, Monitors),
) -> Vec<Option<(u32, u32, bool, Monitors)>> {
    devnodes
        .iter()
        .filter_map(|devnode| match *devnode {
            Devnode::Other => None,
            Devnode::Unread | Devnode::Driver { status: None, .. } => Some(None),
            Devnode::Driver {
                devinst,
                status: Some(status),
            } => Some(Some(read(devinst, status))),
        })
        .collect()
}

/// The status flags of every child of `devinst` (`None` for one whose
/// status couldn't be read), or `None` when the children couldn't be listed.
#[cfg(windows)]
fn children(devinst: u32) -> Option<Vec<Option<u32>>> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Child, CM_Get_Sibling, CR_NO_SUCH_DEVNODE, CR_SUCCESS,
    };
    /// More children than a virtual display ever has: a list this long is
    /// not one to trust.
    const MOST: usize = 64;
    let mut statuses = Vec::new();
    let mut child = 0u32;
    match unsafe { CM_Get_Child(&mut child, devinst, 0) } {
        CR_NO_SUCH_DEVNODE => return Some(statuses),
        CR_SUCCESS => {}
        _ => return None,
    }
    loop {
        statuses.push(devnode_status(child).map(|(flags, _)| flags));
        if statuses.len() > MOST {
            return None;
        }
        let mut next = 0u32;
        match unsafe { CM_Get_Sibling(&mut next, child, 0) } {
            CR_NO_SUCH_DEVNODE => return Some(statuses),
            CR_SUCCESS => child = next,
            _ => return None,
        }
    }
}

/// One present root device, as Configuration Manager read it.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Devnode {
    /// Its hardware IDs were read (or it has none) and don't name the driver.
    Other,
    /// The driver's device, with its status flags and problem code, `None`
    /// when they couldn't be read.
    Driver {
        devinst: u32,
        status: Option<(u32, u32)>,
    },
    /// It couldn't be located, or its hardware IDs couldn't be read: it may
    /// be the driver's.
    Unread,
}

/// The driver as Configuration Manager reads it (`vdd_settings::DriverRun`),
/// from the present root devices (`None` when they couldn't be listed):
/// off when every device of the driver reads as not started, or there is
/// none; running with the start `start` reads for its one running device;
/// unreadable when any read failed, a device started with a problem, or
/// more than one runs.
#[cfg(any(windows, test))]
pub(crate) fn run_from(
    devnodes: Option<&[Devnode]>,
    start: impl FnOnce(u32) -> Option<crate::vdd_settings::DriverInstance>,
) -> crate::vdd_settings::DriverRun {
    use crate::vdd_settings::DriverRun;
    let Some(devnodes) = devnodes else {
        return DriverRun::Unreadable;
    };
    let mut running = None;
    for devnode in devnodes {
        match *devnode {
            Devnode::Other => {}
            Devnode::Unread | Devnode::Driver { status: None, .. } => return DriverRun::Unreadable,
            Devnode::Driver {
                devinst,
                status: Some((flags, _)),
            } => {
                if flags & DN_STARTED == 0 {
                    continue;
                }
                if flags & DN_HAS_PROBLEM != 0 || running.replace(devinst).is_some() {
                    return DriverRun::Unreadable;
                }
            }
        }
    }
    match running {
        None => DriverRun::Off,
        Some(devinst) => start(devinst).map_or(DriverRun::Unreadable, DriverRun::Running),
    }
}

/// The driver as Configuration Manager reads it now (`run_from`).
#[cfg(windows)]
pub fn run_state() -> crate::vdd_settings::DriverRun {
    run_from(root_devnodes().as_deref(), driver_start)
}

/// The start of the driver running on `devinst`: this boot and the
/// device's last arrival.
#[cfg(windows)]
fn driver_start(devinst: u32) -> Option<crate::vdd_settings::DriverInstance> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_PropertyW, CR_SUCCESS,
    };
    use windows::Win32::Devices::Properties::{
        DEVPKEY_Device_LastArrivalDate, DEVPROPTYPE, DEVPROP_TYPE_FILETIME,
    };
    let (mut arrival, mut kind, mut size) = (0u64, DEVPROPTYPE(0), 8u32);
    let result = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_Device_LastArrivalDate,
            &mut kind,
            Some((&mut arrival as *mut u64).cast()),
            &mut size,
            0,
        )
    };
    if result != CR_SUCCESS || kind != DEVPROP_TYPE_FILETIME || size != 8 || arrival == 0 {
        return None;
    }
    Some(crate::vdd_settings::DriverInstance {
        boot_id: boot_id()?,
        arrival,
    })
}

/// Windows' count of boots, which the kernel raises at each one.
#[cfg(windows)]
fn boot_id() -> Option<u32> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
    let (mut value, mut bytes) = (0u32, 4u32);
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters"),
            w!("BootId"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut bytes),
        )
    }
    .is_ok()
    .then_some(value)
}

/// The present root devices (`Devnode`), or `None` when Windows can't list
/// them. A device that can't be located or whose hardware IDs can't be read
/// is kept as `Unread`, never dropped.
#[cfg(windows)]
fn root_devnodes() -> Option<Vec<Devnode>> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_Locate_DevNodeW, CM_DRP_HARDWAREID,
        CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_LOCATE_DEVNODE_NORMAL,
        CR_SUCCESS,
    };

    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    let mut len = 0u32;
    if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, w!("ROOT"), flags) } != CR_SUCCESS {
        return None;
    }
    let mut ids = vec![0u16; len as usize];
    if unsafe { CM_Get_Device_ID_ListW(w!("ROOT"), &mut ids, flags) } != CR_SUCCESS {
        return None;
    }
    Some(
        ids.split(|&unit| unit == 0)
            .filter(|id| !id.is_empty())
            .map(|id| {
                let id: Vec<u16> = id.iter().copied().chain(Some(0)).collect();
                let mut devinst = 0u32;
                if unsafe {
                    CM_Locate_DevNodeW(&mut devinst, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL)
                } != CR_SUCCESS
                {
                    return Devnode::Unread;
                }
                match registry_property(devinst, CM_DRP_HARDWAREID) {
                    Err(()) => Devnode::Unread,
                    Ok(Some(hardware_ids))
                        if hardware_ids.split(|&unit| unit == 0).any(|hwid| {
                            String::from_utf16_lossy(hwid).eq_ignore_ascii_case(HARDWARE_ID)
                        }) =>
                    {
                        Devnode::Driver {
                            devinst,
                            status: devnode_status(devinst),
                        }
                    }
                    Ok(_) => Devnode::Other,
                }
            })
            .collect(),
    )
}

#[cfg(windows)]
fn devnode_status(devinst: u32) -> Option<(u32, u32)> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Status, CM_DEVNODE_STATUS_FLAGS, CM_PROB, CR_SUCCESS,
    };
    let (mut status, mut problem) = (CM_DEVNODE_STATUS_FLAGS(0), CM_PROB(0));
    (unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) } == CR_SUCCESS)
        .then_some((status.0, problem.0))
}

/// A device's registry property: `Ok(None)` when it has none
/// (`CR_NO_SUCH_VALUE`), `Err` when the read failed.
#[cfg(windows)]
fn registry_property(devinst: u32, which: u32) -> Result<Option<Vec<u16>>, ()> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Registry_PropertyW, CR_BUFFER_SMALL, CR_NO_SUCH_VALUE, CR_SUCCESS,
    };
    let mut bytes = 0u32;
    match unsafe { CM_Get_DevNode_Registry_PropertyW(devinst, which, None, None, &mut bytes, 0) } {
        CR_NO_SUCH_VALUE => return Ok(None),
        CR_BUFFER_SMALL if bytes > 0 => {}
        _ => return Err(()),
    }
    let mut buffer = vec![0u16; bytes.div_ceil(2) as usize];
    (unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            devinst,
            which,
            None,
            Some(buffer.as_mut_ptr().cast()),
            &mut bytes,
            0,
        )
    } == CR_SUCCESS)
        .then_some(Some(buffer))
        .ok_or(())
}

/// Set the mode of the virtual display `device` (a GDI name like
/// `\\.\DISPLAY5`), and save it for this set of displays. Only that
/// display changes. Tried with the refresh rate first, then without it.
#[cfg(windows)]
pub fn set_display_mode(device: &str, mode: crate::vdd_settings::VddMode) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, CDS_UPDATEREGISTRY, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
        DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    };
    let name: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
    [
        DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
        DM_PELSWIDTH | DM_PELSHEIGHT,
    ]
    .into_iter()
    .any(|fields| {
        let devmode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            dmFields: fields,
            dmPelsWidth: mode.width,
            dmPelsHeight: mode.height,
            dmDisplayFrequency: mode.refresh_hz,
            ..Default::default()
        };
        (unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                Some(&devmode),
                None,
                CDS_UPDATEREGISTRY,
                None,
            )
        }) == DISP_CHANGE_SUCCESSFUL
    })
}

/// The sizes Windows offers for the display `device`.
#[cfg(windows)]
pub fn offered_modes(device: &str) -> Vec<(u32, u32)> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, DEVMODEW, ENUM_DISPLAY_SETTINGS_MODE,
    };
    let name: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
    let mut sizes = Vec::new();
    for index in 0..1024 {
        let mut devmode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !unsafe {
            EnumDisplaySettingsW(
                PCWSTR(name.as_ptr()),
                ENUM_DISPLAY_SETTINGS_MODE(index),
                &mut devmode,
            )
        }
        .as_bool()
        {
            break;
        }
        let size = (devmode.dmPelsWidth, devmode.dmPelsHeight);
        if !sizes.contains(&size) {
            sizes.push(size);
        }
    }
    sizes
}

#[cfg(not(windows))]
pub fn query() -> DeviceState {
    DeviceState::Missing
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review r6: a root device whose location or hardware IDs couldn't be
    /// read may be the driver's, so the driver's state is unreadable, not
    /// off, and an Enable after it keeps no record. One whose hardware IDs
    /// were read and don't name the driver is not the driver's.
    #[test]
    fn a_device_that_could_not_be_read_makes_the_driver_unreadable() {
        use crate::vdd_settings::{DriverCursor, DriverInstance, DriverRun};
        let start = DriverInstance {
            boot_id: 205,
            arrival: 30,
        };
        let starts = |devinst| (devinst == 7).then_some(start);
        let off = Devnode::Driver {
            devinst: 7,
            status: Some((0, 22)),
        };
        let on = Devnode::Driver {
            devinst: 7,
            status: Some((DN_STARTED, 0)),
        };
        let unread = [Devnode::Other, off, Devnode::Unread];
        let before = run_from(Some(&unread), starts);
        assert_eq!(before, DriverRun::Unreadable);
        assert_eq!(
            crate::vdd_settings::record_after_enable(
                before,
                run_from(Some(&[Devnode::Other, on]), starts),
                Some("<vdd_settings/>"),
                DriverCursor::Software,
            ),
            None,
            "no record after an unreadable before"
        );

        for (devnodes, run) in [
            (None, DriverRun::Unreadable),
            (Some(&[][..]), DriverRun::Off),
            (Some(&[Devnode::Other][..]), DriverRun::Off),
            (Some(&[Devnode::Other, off][..]), DriverRun::Off),
            (Some(&[Devnode::Unread][..]), DriverRun::Unreadable),
            (Some(&[Devnode::Unread, on][..]), DriverRun::Unreadable),
            (Some(&[Devnode::Other, on][..]), DriverRun::Running(start)),
            (
                Some(
                    &[Devnode::Driver {
                        devinst: 7,
                        status: None,
                    }][..],
                ),
                DriverRun::Unreadable,
            ),
            (
                Some(
                    &[Devnode::Driver {
                        devinst: 7,
                        status: Some((DN_STARTED | DN_HAS_PROBLEM, 43)),
                    }][..],
                ),
                DriverRun::Unreadable,
            ),
            (
                Some(
                    &[
                        on,
                        Devnode::Driver {
                            devinst: 8,
                            status: Some((DN_STARTED, 0)),
                        },
                    ][..],
                ),
                DriverRun::Unreadable,
            ),
        ] {
            assert_eq!(run_from(devnodes, starts), run, "{devnodes:?}");
        }
        assert_eq!(
            run_from(Some(&[on]), |_| None),
            DriverRun::Unreadable,
            "its start couldn't be read"
        );
    }

    #[test]
    fn the_enable_task_result_names_the_device_state() {
        assert_eq!(DeviceState::from_task_result(0), None);
        assert_eq!(DeviceState::from_task_result(1), None);
        assert_eq!(DeviceState::from_task_result(2), Some(DeviceState::Missing));
        assert_eq!(
            DeviceState::from_task_result(3),
            Some(DeviceState::NoDriver)
        );
        assert_eq!(
            DeviceState::from_task_result(4),
            Some(DeviceState::NotStarted)
        );
        assert_eq!(
            DeviceState::from_task_result(5),
            Some(DeviceState::NoMonitor)
        );
        assert_eq!(
            DeviceState::from_task_result(122),
            Some(DeviceState::Problem(22))
        );
        assert_eq!(
            DeviceState::from_task_result(143),
            Some(DeviceState::Problem(43))
        );
        // Task Scheduler's own codes are not device states.
        assert_eq!(DeviceState::from_task_result(0x41301), None);
        assert_eq!(
            from_task_stderr("VDD task failed: EternalMonitor VDD Enable, result=152"),
            Some(DeviceState::Problem(52))
        );
        assert_eq!(
            from_task_stderr("VDD task failed: EternalMonitor VDD Enable, result=1"),
            None
        );
        assert_eq!(from_task_stderr("Previous VDD task timed out: x"), None);
    }

    #[test]
    fn devnode_status_maps_to_a_device_state() {
        let started = DN_STARTED;
        let problem = DN_HAS_PROBLEM;
        assert_eq!(
            DeviceState::from_devnode(problem, 22, true, Monitors::None),
            DeviceState::Problem(22)
        );
        assert_eq!(
            DeviceState::from_devnode(problem, 43, true, Monitors::None),
            DeviceState::Problem(43)
        );
        assert_eq!(
            DeviceState::from_devnode(problem, 28, false, Monitors::None),
            DeviceState::Problem(28)
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, false, Monitors::None),
            DeviceState::NoDriver
        );
        assert_eq!(
            DeviceState::from_devnode(0, 0, true, Monitors::None),
            DeviceState::NotStarted
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, true, Monitors::None),
            DeviceState::NoMonitor
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, true, Monitors::Pending),
            DeviceState::NoMonitor
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, true, Monitors::Started),
            DeviceState::Ready
        );
    }

    /// Review r1 of #233: the attach wait ends early on `NoMonitor`, so only
    /// a confirmed absence may read as one. Every child counts, not only the
    /// first: Windows lists further children as its siblings.
    #[test]
    fn only_a_confirmed_absence_of_monitors_reads_as_no_monitor_while_waiting() {
        let (started, problem) = (DN_STARTED, DN_HAS_PROBLEM);
        let starting = 0;
        // The children as listed.
        assert_eq!(Monitors::from_children(None), Monitors::Unread);
        assert_eq!(Monitors::from_children(Some(&[])), Monitors::None);
        assert_eq!(
            Monitors::from_children(Some(&[Some(starting)])),
            Monitors::Pending
        );
        assert_eq!(Monitors::from_children(Some(&[None])), Monitors::Unread);
        assert_eq!(
            Monitors::from_children(Some(&[Some(started | problem)])),
            Monitors::Pending
        );
        assert_eq!(
            Monitors::from_children(Some(&[Some(starting), Some(started)])),
            Monitors::Started,
            "a monitor among the siblings"
        );
        assert_eq!(
            Monitors::from_children(Some(&[None, Some(started)])),
            Monitors::Started
        );
        assert_eq!(
            Monitors::from_children(Some(&[Some(starting), None])),
            Monitors::Unread
        );

        // A running device: only no children at all is a confirmed absence.
        let running = |monitors| DeviceState::confirmed(started, 0, true, monitors);
        assert_eq!(running(Monitors::None), Some(DeviceState::NoMonitor));
        assert_eq!(running(Monitors::Started), Some(DeviceState::Ready));
        assert_eq!(running(Monitors::Pending), None);
        assert_eq!(running(Monitors::Unread), None);
        // The snapshot after a timeout still names the device as before.
        for monitors in [Monitors::None, Monitors::Pending, Monitors::Unread] {
            assert_eq!(
                DeviceState::from_devnode(started, 0, true, monitors),
                DeviceState::NoMonitor
            );
        }
        // States that don't depend on the children are certain.
        assert_eq!(
            DeviceState::confirmed(0, 0, true, Monitors::Unread),
            Some(DeviceState::NotStarted)
        );
        assert_eq!(
            DeviceState::confirmed(problem, 22, true, Monitors::Unread),
            Some(DeviceState::Problem(22))
        );

        // Across the driver's devices.
        let confirmed_none = Some((started, 0, true, Monitors::None));
        assert_eq!(
            confirmed_from(vec![confirmed_none]),
            Some(DeviceState::NoMonitor)
        );
        assert_eq!(
            confirmed_from(vec![None]),
            None,
            "a status that couldn't be read"
        );
        assert_eq!(
            confirmed_from(vec![
                confirmed_none,
                Some((started, 0, true, Monitors::Pending))
            ]),
            None
        );
        assert_eq!(
            confirmed_from(vec![None, Some((started, 0, true, Monitors::Started))]),
            Some(DeviceState::Ready)
        );
        assert_eq!(confirmed_from(vec![]), Some(DeviceState::Missing));
    }

    /// Review r2 of #233: a root device that couldn't be read may be the
    /// driver's, with a monitor still arriving. Next to a running device
    /// with no children, the read isn't a confirmed absence.
    #[test]
    fn an_unread_root_device_keeps_the_read_uncertain() {
        let running_childless = |_, (flags, problem)| (flags, problem, true, Monitors::None);
        let alone = reads_from(
            &[
                Devnode::Other,
                Devnode::Driver {
                    devinst: 7,
                    status: Some((DN_STARTED, 0)),
                },
            ],
            running_childless,
        );
        assert_eq!(confirmed_from(alone), Some(DeviceState::NoMonitor));
        let with_unread = reads_from(
            &[
                Devnode::Unread,
                Devnode::Driver {
                    devinst: 7,
                    status: Some((DN_STARTED, 0)),
                },
            ],
            running_childless,
        );
        assert_eq!(confirmed_from(with_unread), None);
        let unread_status = reads_from(
            &[
                Devnode::Driver {
                    devinst: 8,
                    status: None,
                },
                Devnode::Driver {
                    devinst: 7,
                    status: Some((DN_STARTED, 0)),
                },
            ],
            running_childless,
        );
        assert_eq!(confirmed_from(unread_status), None);
    }

    #[test]
    fn the_device_closest_to_a_display_wins() {
        assert_eq!(DeviceState::best([]), DeviceState::Missing);
        assert_eq!(
            DeviceState::best([DeviceState::Problem(43), DeviceState::Ready]),
            DeviceState::Ready
        );
        assert_eq!(
            DeviceState::best([DeviceState::NoDriver, DeviceState::Problem(22)]),
            DeviceState::Problem(22)
        );
    }

    #[test]
    fn each_device_state_says_what_to_do() {
        use VddProblem::*;
        for state in [
            DeviceState::Missing,
            DeviceState::NoDriver,
            DeviceState::Problem(1),
            DeviceState::Problem(18),
            DeviceState::Problem(28),
        ] {
            assert_eq!(state.problem(), NotInstalled, "{state}");
        }
        assert_eq!(DeviceState::Problem(52).problem(), SignatureBlocked);
        for code in [10, 22, 31, 39, 43] {
            assert_eq!(
                DeviceState::Problem(code).problem(),
                DriverError(Some(code))
            );
        }
        assert_eq!(DeviceState::NotStarted.problem(), DriverError(None));
        assert_eq!(DeviceState::NoMonitor.problem(), NoDisplay);
        assert_eq!(DeviceState::Ready.problem(), NotExtended);
    }

    /// Reads this PC's driver device through Configuration Manager:
    /// `EM_EXPECT_VDD=problem_22 cargo test -p eternal-host --lib native_device_state -- --ignored`
    #[cfg(windows)]
    #[test]
    #[ignore = "reads the virtual display device of this PC"]
    fn native_device_state() {
        let state = query();
        println!("vdd_device={state} problem={:?}", state.problem());
        // DXGI lists a virtual display under the adapter that renders it.
        for output in crate::capture::listing::outputs(&crate::control::SharedControl::new(0, 1)) {
            println!(
                "output {} {}x{} at {},{} on {}",
                output.device_name,
                output.width,
                output.height,
                output.left,
                output.top,
                output.adapter_name
            );
        }
        if let Ok(expected) = std::env::var("EM_EXPECT_VDD") {
            assert_eq!(state.to_string(), expected);
        }
    }

    #[test]
    fn log_names_carry_no_instance_details() {
        assert_eq!(DeviceState::Problem(43).to_string(), "problem_43");
        assert_eq!(DeviceState::NoMonitor.to_string(), "no_monitor");
        assert_eq!(DeviceState::Missing.to_string(), "missing");
    }
}
