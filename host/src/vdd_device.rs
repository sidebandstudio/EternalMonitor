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
    /// problem number, whether a driver is bound, and whether its monitor
    /// child is started (`None` without a child).
    #[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
    fn from_devnode(status: u32, problem: u32, has_driver: bool, monitor: Option<bool>) -> Self {
        if status & DN_HAS_PROBLEM != 0 {
            Self::Problem(problem)
        } else if !has_driver {
            Self::NoDriver
        } else if status & DN_STARTED == 0 {
            Self::NotStarted
        } else if monitor == Some(true) {
            Self::Ready
        } else {
            Self::NoMonitor
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
    use windows::core::{w, PCWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Child, CM_Get_DevNode_Registry_PropertyW, CM_Get_DevNode_Status,
        CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_Locate_DevNodeW,
        CM_DEVNODE_STATUS_FLAGS, CM_DRP_DRIVER, CM_DRP_HARDWAREID, CM_GETIDLIST_FILTER_ENUMERATOR,
        CM_GETIDLIST_FILTER_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CM_PROB, CR_BUFFER_SMALL,
        CR_SUCCESS,
    };

    let status = |devinst: u32| {
        let (mut status, mut problem) = (CM_DEVNODE_STATUS_FLAGS(0), CM_PROB(0));
        (unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) } == CR_SUCCESS)
            .then_some((status.0, problem.0))
    };
    let property = |devinst: u32, which: u32| -> Option<Vec<u16>> {
        let mut bytes = 0u32;
        let result =
            unsafe { CM_Get_DevNode_Registry_PropertyW(devinst, which, None, None, &mut bytes, 0) };
        if result != CR_BUFFER_SMALL || bytes == 0 {
            return None;
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
            .then_some(buffer)
    };

    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    let mut len = 0u32;
    if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, w!("ROOT"), flags) } != CR_SUCCESS {
        return DeviceState::Missing;
    }
    let mut ids = vec![0u16; len as usize];
    if unsafe { CM_Get_Device_ID_ListW(w!("ROOT"), &mut ids, flags) } != CR_SUCCESS {
        return DeviceState::Missing;
    }
    let states = ids
        .split(|&unit| unit == 0)
        .filter(|id| !id.is_empty())
        .filter_map(|id| {
            let id: Vec<u16> = id.iter().copied().chain(Some(0)).collect();
            let mut devinst = 0u32;
            if unsafe {
                CM_Locate_DevNodeW(&mut devinst, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL)
            } != CR_SUCCESS
            {
                return None;
            }
            let hardware_ids = property(devinst, CM_DRP_HARDWAREID)?;
            hardware_ids
                .split(|&unit| unit == 0)
                .any(|hwid| String::from_utf16_lossy(hwid).eq_ignore_ascii_case(HARDWARE_ID))
                .then_some(devinst)
        })
        .filter_map(|devinst| {
            let (flags, problem) = status(devinst)?;
            let mut child = 0u32;
            let monitor =
                (unsafe { CM_Get_Child(&mut child, devinst, 0) } == CR_SUCCESS).then(|| {
                    status(child).is_some_and(|(flags, _)| {
                        flags & DN_STARTED != 0 && flags & DN_HAS_PROBLEM == 0
                    })
                });
            let has_driver = property(devinst, CM_DRP_DRIVER).is_some();
            Some(DeviceState::from_devnode(
                flags, problem, has_driver, monitor,
            ))
        });
    DeviceState::best(states)
}

#[cfg(not(windows))]
pub fn query() -> DeviceState {
    DeviceState::Missing
}

#[cfg(test)]
mod tests {
    use super::*;

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
            DeviceState::from_devnode(problem, 22, true, None),
            DeviceState::Problem(22)
        );
        assert_eq!(
            DeviceState::from_devnode(problem, 43, true, None),
            DeviceState::Problem(43)
        );
        assert_eq!(
            DeviceState::from_devnode(problem, 28, false, None),
            DeviceState::Problem(28)
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, false, None),
            DeviceState::NoDriver
        );
        assert_eq!(
            DeviceState::from_devnode(0, 0, true, None),
            DeviceState::NotStarted
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, true, None),
            DeviceState::NoMonitor
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, true, Some(false)),
            DeviceState::NoMonitor
        );
        assert_eq!(
            DeviceState::from_devnode(started, 0, true, Some(true)),
            DeviceState::Ready
        );
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
