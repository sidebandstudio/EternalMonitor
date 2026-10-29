//! Which graphics card Windows runs EternalMonitor on, and moving it to the
//! card a screen is wired to when Windows won't share that screen otherwise.
//!
//! On a hybrid PC (an integrated GPU and an NVIDIA or AMD card), DXGI lists
//! every screen under the adapter the process runs on. Desktop Duplication
//! then fails with DXGI_ERROR_UNSUPPORTED (0x887A0004) unless that adapter
//! is the one the screen is wired to. The display configuration paths
//! (`QueryDisplayConfig`) still report the real wiring, so the check uses
//! those and never DXGI's listing.
//!
//! Windows picks the adapter from the per-app value under
//! `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`, the same one
//! Settings > System > Display > Graphics writes, and reads it only when a
//! process starts. A change is written by a helper copy after this process
//! has exited (writing it while EternalMonitor still held a duplication hung
//! that process's next DuplicateOutput on the reference PC), and the helper
//! then opens EternalMonitor again. Nothing is written unless the user asks.

use serde::{Deserialize, Serialize};

/// A DXGI/display-configuration adapter LUID as (high, low).
pub type AdapterId = (i32, u32);

/// The per-app graphics preference, as Windows stores it in `GpuPreference`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preference {
    /// No preference: Windows decides (value 0 or none).
    Automatic,
    /// 1: the integrated adapter.
    PowerSaving,
    /// 2: an external GPU if there is one, otherwise the internal card.
    HighPerformance,
}

impl Preference {
    pub fn value(self) -> u32 {
        match self {
            Self::Automatic => 0,
            Self::PowerSaving => 1,
            Self::HighPerformance => 2,
        }
    }

    pub fn from_value(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Automatic),
            1 => Some(Self::PowerSaving),
            2 => Some(Self::HighPerformance),
            _ => None,
        }
    }
}

/// One graphics adapter as DXGI lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub id: AdapterId,
    pub name: String,
}

/// What Windows says about the graphics cards and screens. Gathered by
/// [`facts`] on Windows; tests build it by hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GpuFacts {
    /// Hardware adapters in DXGI order. Windows lists the adapter the
    /// process runs on first.
    pub adapters: Vec<Adapter>,
    /// The adapter each preference selects, from [`preference_cards`];
    /// `None` when that is not certain.
    pub power_saving: Option<AdapterId>,
    pub high_performance: Option<AdapterId>,
    /// Active screens: GDI name (`\\.\DISPLAY1`) and the adapter wired to it.
    pub screens: Vec<(String, AdapterId)>,
}

impl GpuFacts {
    fn adapter(&self, id: AdapterId) -> Option<&Adapter> {
        self.adapters.iter().find(|adapter| adapter.id == id)
    }

    /// The adapter EternalMonitor runs on now.
    pub fn running_on(&self) -> Option<&Adapter> {
        self.adapters.first()
    }

    /// The adapter wired to `screen`. `None` for a screen no hardware adapter
    /// drives: the virtual second screen sits on its driver's own adapter,
    /// and that driver renders on the card the app runs on (the host names
    /// the main screen's adapter, as DXGI lists it, before each Enable).
    pub fn wired_to(&self, screen: &str) -> Option<&Adapter> {
        let (_, id) = self
            .screens
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(screen))?;
        self.adapter(*id)
    }

    /// The card each preference selects, for the Troubleshooting choice.
    pub fn card_for(&self, preference: Preference) -> Option<&Adapter> {
        match preference {
            Preference::Automatic => None,
            Preference::PowerSaving => self.power_saving.and_then(|id| self.adapter(id)),
            Preference::HighPerformance => self.high_performance.and_then(|id| self.adapter(id)),
        }
    }

    /// Whether a choice between cards exists at all (a hybrid laptop or a PC
    /// with two cards).
    pub fn has_choice(&self) -> bool {
        self.power_saving.is_some()
            && self.high_performance.is_some()
            && self.power_saving != self.high_performance
    }
}

/// How Windows ranks an adapter for the per-app graphics preference, from
/// DXCore's hardware properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    /// Built into the processor: what Power saving selects.
    Integrated,
    /// An internal graphics card.
    Discrete,
    /// An external GPU. High performance ranks these ahead of internal cards.
    Detachable,
    /// Not integrated, but this Windows can't say whether it is detachable.
    NotIntegrated,
    /// Not a hardware adapter (the software renderer, a virtual display).
    Ignored,
    /// Windows could not classify it.
    Unknown,
}

/// The adapter each preference selects, as (Power saving, High
/// performance), following `EnumAdapterByGpuPreference`'s documented order:
/// Power saving puts integrated adapters first; High performance puts
/// detachable ones first, then the other cards. Video memory plays no part:
/// a 16 GB internal card loses to an 8 GB eGPU. Each is `None` unless one
/// adapter alone holds that rank and every adapter could be classified.
///
/// `EnumAdapterByGpuPreference` itself can't be asked: once a per-app value
/// is set, it lists the forced card first for both preferences (reference
/// PC, forced onto its iGPU: neither preference named the RTX card).
pub fn preference_cards(
    kinds: &[(AdapterId, AdapterKind)],
) -> (Option<AdapterId>, Option<AdapterId>) {
    use AdapterKind::*;
    if kinds.iter().any(|(_, kind)| *kind == Unknown) {
        return (None, None);
    }
    let only = |wanted: &[AdapterKind]| {
        let mut matching = kinds.iter().filter(|(_, kind)| wanted.contains(kind));
        match (matching.next(), matching.next()) {
            (Some((id, _)), None) => Some(*id),
            _ => None,
        }
    };
    let detachable = kinds.iter().filter(|(_, kind)| *kind == Detachable).count();
    let high_performance = match detachable {
        0 => only(&[Discrete, NotIntegrated]),
        // An undetermined card might be another eGPU.
        1 if !kinds.iter().any(|(_, kind)| *kind == NotIntegrated) => only(&[Detachable]),
        _ => None,
    };
    (only(&[Integrated]), high_performance)
}

/// The preference certain to put EternalMonitor on `wired`: the one that
/// selects that card. `None` when the app
/// already runs there (switching cannot help: a MUX laptop in dGPU-only
/// mode, a single card), or when neither preference certainly selects it (an
/// internal card that loses High performance to an eGPU); the notice then
/// explains Windows Settings instead.
pub fn preference_for(wired: AdapterId, facts: &GpuFacts) -> Option<Preference> {
    if facts.running_on().map(|a| a.id) == Some(wired) {
        return None;
    }
    if facts.power_saving == Some(wired) {
        Some(Preference::PowerSaving)
    } else if facts.high_performance == Some(wired) {
        Some(Preference::HighPerformance)
    } else {
        None
    }
}

/// The result of the check after Windows refused to duplicate a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// The card the captured screen is wired to.
    pub screen_gpu: String,
    /// The card EternalMonitor runs on.
    pub app_gpu: String,
    /// The preference that moves the app to `screen_gpu`, if one certainly does.
    pub preference: Option<Preference>,
}

/// Works out why `screen` cannot be duplicated. `None` when the screen's
/// card can't be found among the hardware adapters (the virtual second
/// screen, or a screen that just went away).
pub fn diagnose(facts: &GpuFacts, screen: &str) -> Option<Diagnosis> {
    let wired = facts.wired_to(screen)?;
    Some(Diagnosis {
        screen_gpu: wired.name.clone(),
        app_gpu: facts
            .running_on()
            .map(|a| a.name.clone())
            .unwrap_or_default(),
        preference: preference_for(wired.id, facts),
    })
}

/// Repeats the check after each refused duplication only when it can say
/// something new: once per screen and set of facts, again after the cards
/// or the screens' wiring change (a monitor plugged in, a topology change),
/// and on the next attempt when the facts couldn't be read at all.
#[derive(Debug, Default)]
pub struct GpuCheck {
    checked: Option<(String, GpuFacts)>,
}

impl GpuCheck {
    pub const fn new() -> Self {
        Self { checked: None }
    }

    /// `facts` as just read for `screen`, or `None` when Windows couldn't
    /// say. Returns the diagnosis to show and log, or `None` when it would
    /// repeat the last one.
    pub fn check(&mut self, screen: &str, facts: Option<GpuFacts>) -> Option<Option<Diagnosis>> {
        let facts = facts?;
        let key = (screen.to_ascii_uppercase(), facts);
        if self.checked.as_ref() == Some(&key) {
            return None;
        }
        let diagnosis = diagnose(&key.1, screen);
        self.checked = Some(key);
        Some(diagnosis)
    }
}

/// A display configuration read that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRead {
    /// The paths changed between sizing the buffers and reading them
    /// (ERROR_INSUFFICIENT_BUFFER): read the sizes again.
    Changed,
    /// Any other error.
    Failed,
}

/// Attempts at a display configuration read while the screens keep changing.
const CONFIG_READ_ATTEMPTS: usize = 4;

/// Runs `read` again after a [`ConfigRead::Changed`], as Microsoft documents
/// for `QueryDisplayConfig` during a topology change. `None` after any other
/// error, or when the configuration changed on every attempt.
pub fn read_display_config<T>(mut read: impl FnMut() -> Result<T, ConfigRead>) -> Option<T> {
    for _ in 0..CONFIG_READ_ATTEMPTS {
        match read() {
            Ok(value) => return Some(value),
            Err(ConfigRead::Changed) => continue,
            Err(ConfigRead::Failed) => return None,
        }
    }
    None
}

/// A graphics card switch EternalMonitor made from the Stream page notice,
/// kept in settings.json until the user undoes it or picks another choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchRecord {
    /// EternalMonitor's own `UserGpuPreferences` value before the first
    /// switch; `None` when there was none. Undo puts exactly this back.
    pub before: Option<String>,
    pub preference: Preference,
    /// The card the switch was for.
    pub gpu: String,
    /// The relaunched app has not tried to capture yet.
    #[serde(default)]
    pub checking: bool,
    /// Capture still failed the same way after the switch: something else
    /// (the NVIDIA app, NVIDIA Control Panel, AMD Software) decides the card.
    #[serde(default)]
    pub failed: bool,
}

impl SwitchRecord {
    /// The record for a new switch, keeping the original value from an
    /// earlier one so Undo still restores what the user had.
    pub fn new(
        previous: Option<&SwitchRecord>,
        current_value: Option<String>,
        preference: Preference,
        gpu: &str,
    ) -> Self {
        Self {
            before: match previous {
                Some(record) => record.before.clone(),
                None => current_value,
            },
            preference,
            gpu: gpu.to_string(),
            checking: true,
            failed: false,
        }
    }
}

/// What the relaunched app learned from its first capture attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Worked,
    StillUnsupported,
}

/// Updates the record after a capture attempt. Returns true when the result
/// is new and worth one log line.
pub fn settle(record: &mut SwitchRecord, outcome: Outcome) -> bool {
    match outcome {
        Outcome::Worked if record.checking || record.failed => {
            record.checking = false;
            record.failed = false;
            true
        }
        Outcome::StillUnsupported if record.checking => {
            record.checking = false;
            record.failed = true;
            true
        }
        _ => false,
    }
}

/// Whether the stored value holds the switch `record` asked for. False after
/// a helper copy could not save it (HKCU writes refused).
pub fn switch_saved(record: &SwitchRecord, value: Option<&str>) -> bool {
    preference_in(value).unwrap_or(0) == record.preference.value()
}

/// `GpuPreference` in a `UserGpuPreferences` value (`GpuPreference=2;`).
pub fn preference_in(value: Option<&str>) -> Option<u32> {
    value?
        .split(';')
        .filter_map(|entry| entry.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("GpuPreference"))
        .and_then(|(_, value)| value.trim().parse().ok())
}

/// The value with `GpuPreference` set to `preference`, keeping every other
/// setting Windows stores there (for example `SwapEffectUpgradeEnable=1;`).
/// Automatic removes the entry; `None` means nothing is left and the value
/// itself goes.
pub fn with_preference(existing: Option<&str>, preference: Preference) -> Option<String> {
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
    if preference != Preference::Automatic {
        value.push_str(&format!("GpuPreference={};", preference.value()));
    }
    (!value.is_empty()).then_some(value)
}

/// What the helper copy writes once the app has exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Set `GpuPreference`, keeping the value's other settings.
    Set(Preference),
    /// Put back a value exactly as it was (`None`: remove it).
    Restore(Option<String>),
}

impl Change {
    fn to_arg(&self) -> String {
        match self {
            Self::Set(preference) => preference.value().to_string(),
            Self::Restore(value) => format!("restore:{}", value.as_deref().unwrap_or_default()),
        }
    }

    fn from_arg(arg: &str) -> Option<Self> {
        if let Some(value) = arg.strip_prefix("restore:") {
            return Some(Self::Restore(
                (!value.is_empty()).then(|| value.to_string()),
            ));
        }
        arg.parse()
            .ok()
            .and_then(Preference::from_value)
            .map(Self::Set)
    }

    /// The value to store, given the one stored now.
    pub fn apply(&self, existing: Option<&str>) -> Option<String> {
        match self {
            Self::Set(preference) => with_preference(existing, *preference),
            Self::Restore(value) => value.clone(),
        }
    }
}

/// EternalMonitor's own `UserGpuPreferences` value. Only the value named by
/// this executable's path is ever read or written, under HKCU, which needs
/// no administrator rights.
pub trait PreferenceStore {
    fn read(&self) -> Result<Option<String>, String>;
    fn write(&mut self, value: Option<&str>) -> Result<(), String>;
}

/// Applies `change` to the store. Returns the value before and after.
pub fn apply(
    store: &mut impl PreferenceStore,
    change: &Change,
) -> Result<(Option<String>, Option<String>), String> {
    let before = store.read()?;
    let after = change.apply(before.as_deref());
    if after != before {
        store.write(after.as_deref())?;
    }
    Ok((before, after))
}

/// The helper copy's arguments: wait for this process to exit, then make
/// this change before opening the app again.
pub const AFTER_EXIT_ARG: &str = "--after-exit-of";
pub const CHANGE_ARG: &str = "--set-gpu-preference";

/// The user's own arguments (a custom port is the first one), without the
/// ones a reopen adds.
pub fn own_args(args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == AFTER_EXIT_ARG || arg == CHANGE_ARG {
            args.next();
        } else {
            kept.push(arg);
        }
    }
    kept
}

/// The arguments for a helper copy that waits for `pid`, makes `change` and
/// opens the app again with the user's own arguments.
pub fn helper_args(
    args: impl IntoIterator<Item = String>,
    pid: u32,
    change: Option<&Change>,
) -> Vec<String> {
    let mut kept = own_args(args);
    kept.extend([AFTER_EXIT_ARG.to_string(), pid.to_string()]);
    if let Some(change) = change {
        kept.extend([CHANGE_ARG.to_string(), change.to_arg()]);
    }
    kept
}

fn arg_after<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).map(String::as_str)
}

/// What a helper copy was asked to do, from its arguments: the process to
/// wait for and the change to make. `None` for an ordinary launch.
pub fn helper_request(args: &[String]) -> Option<(u32, Option<Change>)> {
    let pid = arg_after(args, AFTER_EXIT_ARG)?.parse().ok()?;
    Some((pid, arg_after(args, CHANGE_ARG).and_then(Change::from_arg)))
}

/// How long a helper copy waits for the app to exit. Closing can take 40 s
/// (the virtual display's Disable task may run 35 s, then its worker 5 s),
/// and the copy must not start EternalMonitor before then: the new copy
/// would find the old one's single-instance lock and quit, leaving no app.
pub const WAIT_FOR_EXIT: std::time::Duration = std::time::Duration::from_secs(120);

/// What a helper copy needs from Windows. Tests fake it.
pub trait HelperHost {
    /// Waits for the app to exit; false when it still runs after `timeout`.
    fn wait_for_exit(&mut self, timeout: std::time::Duration) -> bool;
    /// Opens EternalMonitor again with the user's own arguments.
    fn launch(&mut self) -> Result<(), String>;
    /// Shows a message box and waits for OK.
    fn tell(&mut self, title: &str, body: &str);
    /// Leaves a line for the next start to log (and so upload). The helper
    /// has no log of its own.
    fn note(&mut self, line: &str);
}

/// How a helper copy ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperEnd {
    Reopened,
    /// The app exited but could not be started again; the user was told.
    NotReopened,
    /// The app was still running at the deadline: nothing was changed or
    /// started, and the user was told.
    StillRunning,
}

pub const SLOW_EXIT_TITLE: &str = "EternalMonitor is taking a long time to close";
pub const REOPEN_FAILED_TITLE: &str = "EternalMonitor couldn't reopen";

/// A helper copy's work: wait for the app to exit, then make `change` with
/// `save` and open the app again. Nothing is written or started while the
/// app may still run, and every way this can end without a window tells
/// the user.
pub fn run_helper(
    host: &mut impl HelperHost,
    change: Option<&Change>,
    save: impl FnOnce(&Change) -> Result<(), String>,
) -> HelperEnd {
    if !host.wait_for_exit(WAIT_FOR_EXIT) {
        host.note(&format!(
            "Graphics card helper: EternalMonitor was still running after {} s; nothing was changed or reopened",
            WAIT_FOR_EXIT.as_secs()
        ));
        let body = if change.is_some() {
            "The graphics card wasn't changed. Wait until EternalMonitor has closed, then open it from the Start menu and choose the card again."
        } else {
            "Wait until EternalMonitor has closed, then open it from the Start menu."
        };
        host.tell(SLOW_EXIT_TITLE, body);
        return HelperEnd::StillRunning;
    }
    // If saving fails, the app opens as before and offers the switch again.
    let saved = change.map(save);
    if let Some(Err(error)) = &saved {
        host.note(&format!(
            "Graphics card helper: the graphics setting was not saved ({error})"
        ));
    }
    let Err(error) = host.launch() else {
        return HelperEnd::Reopened;
    };
    let (saved_word, setting) = match saved {
        None => ("no change", ""),
        Some(Ok(())) => ("yes", " The graphics card setting was saved."),
        Some(Err(_)) => ("no", " The graphics card setting couldn't be saved."),
    };
    host.note(&format!(
        "Graphics card helper: EternalMonitor could not be reopened ({error}); setting saved: {saved_word}"
    ));
    host.tell(
        REOPEN_FAILED_TITLE,
        &format!("Open EternalMonitor from the Start menu.{setting}"),
    );
    HelperEnd::NotReopened
}

/// The file a helper copy leaves its lines in, beside settings.json.
const HELPER_NOTES: &str = "graphics-helper-notes.txt";

/// Adds `line` to the helper notes in `dir`.
pub fn leave_note_in(dir: &std::path::Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(HELPER_NOTES))?;
    writeln!(file, "{}", line.replace(['\r', '\n'], " "))
}

/// The lines helper copies left in `dir`, removed once read.
pub fn take_notes_in(dir: &std::path::Path) -> Vec<String> {
    let path = dir.join(HELPER_NOTES);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let _ = std::fs::remove_file(&path);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// The lines helper copies left for this start to log.
pub fn take_helper_notes() -> Vec<String> {
    crate::settings::app_data_dir()
        .map(|dir| take_notes_in(&dir))
        .unwrap_or_default()
}

/// Whether the switch `record` waits on reached Windows, as the relaunched
/// app first sees it. A switch the helper couldn't save is no longer
/// awaited, so a refused capture offers it again instead of blaming
/// another app for choosing the card.
pub fn started_after_switch(record: &mut SwitchRecord, value: Option<&str>) -> bool {
    let saved = switch_saved(record, value);
    if !saved {
        record.checking = false;
    }
    saved
}

#[cfg(windows)]
pub use windows_impl::{facts, read_value, relaunch, run_helper_if_requested};

/// No graphics preference outside Windows.
#[cfg(not(windows))]
pub fn facts() -> Option<GpuFacts> {
    None
}

#[cfg(not(windows))]
pub fn read_value() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn relaunch(_change: Option<&Change>) -> Result<(), String> {
    Err("switching graphics cards needs Windows".into())
}

#[cfg(not(windows))]
pub fn run_helper_if_requested() {}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt;
    use std::time::Duration;

    use std::ffi::c_void;

    use windows::core::{s, w, Interface, GUID, HRESULT, PCWSTR};
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID,
    };
    use windows::Win32::Graphics::DXCore::{
        DXCoreAdapterProperty, IDXCoreAdapter, IDXCoreAdapterFactory, IDXCoreAdapterList,
        InstanceLuid, IsDetachable, IsHardware, IsIntegrated,
        DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
        REG_OPEN_CREATE_OPTIONS, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    use super::{AdapterId, AdapterKind, Change, GpuFacts, PreferenceStore};

    const PREFERENCES_KEY: PCWSTR = w!("Software\\Microsoft\\DirectX\\UserGpuPreferences");
    /// DXGI_ADAPTER_FLAG_SOFTWARE.
    const FLAG_SOFTWARE: u32 = 2;

    fn id(luid: LUID) -> AdapterId {
        (luid.HighPart, luid.LowPart)
    }

    fn text(wide: &[u16]) -> String {
        String::from_utf16_lossy(&wide[..wide.iter().position(|&c| c == 0).unwrap_or(wide.len())])
            .trim()
            .to_string()
    }

    /// The graphics cards, the card each preference selects and the card
    /// each screen is wired to. Creates no device. `None` when Windows
    /// can't say which card a screen is wired to.
    pub fn facts() -> Option<GpuFacts> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
        let adapters = (0..)
            .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
            .filter_map(|adapter| {
                let desc = unsafe { adapter.GetDesc1() }.ok()?;
                (desc.Flags & FLAG_SOFTWARE == 0).then(|| super::Adapter {
                    id: id(desc.AdapterLuid),
                    name: text(&desc.Description),
                })
            })
            .collect();
        let (power_saving, high_performance) =
            super::preference_cards(&classify().unwrap_or_default());
        Some(GpuFacts {
            adapters,
            power_saving,
            high_performance,
            screens: screens()?,
        })
    }

    /// Every adapter DXCore lists for Direct3D 11, by how the per-app
    /// preference ranks it. `None` without DXCore (loaded at run time:
    /// Windows 10 1809, which the installer accepts, may lack it).
    fn classify() -> Option<Vec<(AdapterId, AdapterKind)>> {
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

    /// Each active screen's GDI name and the adapter the display
    /// configuration says scans it out: the physical wiring, unlike DXGI,
    /// which lists screens under the adapter the process runs on. `None`
    /// when the configuration can't be read.
    fn screens() -> Option<Vec<(String, AdapterId)>> {
        let paths = super::read_display_config(read_paths)?;
        let screens = paths
            .iter()
            .filter_map(|path| {
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
                Some((
                    text(&source.viewGdiDeviceName),
                    id(path.sourceInfo.adapterId),
                ))
            })
            .collect();
        Some(screens)
    }

    /// One read of the active paths: sized, then read.
    fn read_paths() -> Result<Vec<DISPLAYCONFIG_PATH_INFO>, super::ConfigRead> {
        let (mut paths_len, mut modes_len) = (0u32, 0u32);
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths_len, &mut modes_len)
        } != ERROR_SUCCESS
        {
            return Err(super::ConfigRead::Failed);
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); paths_len as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); modes_len as usize];
        match unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut paths_len,
                paths.as_mut_ptr(),
                &mut modes_len,
                modes.as_mut_ptr(),
                None,
            )
        } {
            ERROR_SUCCESS => {
                paths.truncate(paths_len as usize);
                Ok(paths)
            }
            ERROR_INSUFFICIENT_BUFFER => Err(super::ConfigRead::Changed),
            _ => Err(super::ConfigRead::Failed),
        }
    }

    /// This executable's value under HKCU.
    struct OwnValue {
        name: Vec<u16>,
    }

    impl OwnValue {
        fn new() -> Result<Self, String> {
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            Ok(Self {
                name: exe.as_os_str().encode_wide().chain([0]).collect(),
            })
        }

        /// The key, read-only. `None` when it doesn't exist: reading must
        /// never create it, since every start reads the value.
        fn open(&self) -> Result<Option<Key>, String> {
            let mut key = HKEY::default();
            let status = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PREFERENCES_KEY,
                    0,
                    KEY_QUERY_VALUE,
                    &mut key,
                )
            };
            match status {
                ERROR_SUCCESS => Ok(Some(Key(key))),
                ERROR_FILE_NOT_FOUND => Ok(None),
                status => Err(format!("could not open the graphics settings: {status:?}")),
            }
        }

        /// The key for a write, created if Windows has none yet.
        fn create(&self) -> Result<Key, String> {
            let mut key = HKEY::default();
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PREFERENCES_KEY,
                    0,
                    PCWSTR::null(),
                    REG_OPEN_CREATE_OPTIONS(REG_OPTION_NON_VOLATILE.0),
                    KEY_SET_VALUE,
                    None,
                    &mut key,
                    None,
                )
            };
            if status == ERROR_SUCCESS {
                Ok(Key(key))
            } else {
                Err(format!("could not open the graphics settings: {status:?}"))
            }
        }
    }

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    impl PreferenceStore for OwnValue {
        fn read(&self) -> Result<Option<String>, String> {
            let Some(key) = self.open()? else {
                return Ok(None);
            };
            let name = PCWSTR(self.name.as_ptr());
            let mut size = 0u32;
            let status =
                unsafe { RegQueryValueExW(key.0, name, None, None, None, Some(&mut size)) };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS {
                return Err(format!("could not read the graphics setting: {status:?}"));
            }
            let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    name,
                    None,
                    None,
                    Some(buffer.as_mut_ptr().cast()),
                    Some(&mut size),
                )
            };
            if status != ERROR_SUCCESS {
                return Err(format!("could not read the graphics setting: {status:?}"));
            }
            Ok(Some(text(&buffer)))
        }

        fn write(&mut self, value: Option<&str>) -> Result<(), String> {
            let key = self.create()?;
            let name = PCWSTR(self.name.as_ptr());
            let status = match value {
                Some(value) => {
                    let bytes: Vec<u8> = value
                        .encode_utf16()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect();
                    unsafe { RegSetValueExW(key.0, name, 0, REG_SZ, Some(&bytes)) }
                }
                None => match unsafe { RegDeleteValueW(key.0, name) } {
                    ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                    status => status,
                },
            };
            if status == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(format!("could not save the graphics setting: {status:?}"))
            }
        }
    }

    /// EternalMonitor's own value now.
    pub fn read_value() -> Result<Option<String>, String> {
        OwnValue::new()?.read()
    }

    /// Starts `exe` with `args`, inheriting no handles (the host holds
    /// inheritable sockets and driver handles) and outside this process's
    /// job object where the job allows it, so it outlives this process.
    fn start(exe: &std::path::Path, args: &[String]) -> Result<(), String> {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let mut line = eternal_updater::quote_arg(&exe.display().to_string());
        for arg in args {
            line.push(' ');
            line.push_str(&eternal_updater::quote_arg(arg));
        }
        let flags = CREATE_NEW_PROCESS_GROUP;
        eternal_updater::win::start_suspended(exe, &line, flags | CREATE_BREAKAWAY_FROM_JOB, None)
            .or_else(|_| eternal_updater::win::start_suspended(exe, &line, flags, None))
            .and_then(|started| started.resume())
            .map(drop)
            .map_err(|error| format!("could not open EternalMonitor again: {error}"))
    }

    /// Starts a helper copy that waits for this process to exit, makes
    /// `change` and opens the app again. The caller then closes the window.
    /// This process never writes the value itself.
    pub fn relaunch(change: Option<&Change>) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        start(
            &exe,
            &super::helper_args(std::env::args().skip(1), std::process::id(), change),
        )
    }

    /// A helper copy's view of Windows: the app's process, this executable
    /// and the user's own arguments.
    struct Helper {
        pid: u32,
        args: Vec<String>,
    }

    impl super::HelperHost for Helper {
        fn wait_for_exit(&mut self, timeout: Duration) -> bool {
            // A process that can't be opened has already exited.
            eternal_updater::win::open_process(self.pid).map_or(true, |process| {
                eternal_updater::win::wait(&process, Some(timeout)).is_some()
            })
        }

        fn launch(&mut self) -> Result<(), String> {
            let exe = std::env::current_exe()
                .map_err(|error| format!("could not open EternalMonitor again: {error}"))?;
            start(&exe, &super::own_args(self.args.clone()))
        }

        fn tell(&mut self, title: &str, body: &str) {
            use windows::core::HSTRING;
            use windows::Win32::UI::WindowsAndMessaging::{
                MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
            };
            unsafe {
                MessageBoxW(
                    None,
                    &HSTRING::from(body),
                    &HSTRING::from(title),
                    MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
                );
            }
        }

        fn note(&mut self, line: &str) {
            if let Some(dir) = crate::settings::app_data_dir() {
                let _ = super::leave_note_in(&dir, line);
            }
        }
    }

    /// In a helper copy: wait for the app to exit, make the change, open the
    /// app again and exit. Runs before logging and the single-instance check,
    /// so it touches neither the session log nor the running app.
    pub fn run_helper_if_requested() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let Some((pid, change)) = super::helper_request(&args) else {
            return;
        };
        super::run_helper(&mut Helper { pid, args }, change.as_ref(), |change| {
            OwnValue::new()
                .and_then(|mut value| super::apply(&mut value, change))
                .map(drop)
        });
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IGPU: AdapterId = (0, 0x1111);
    const DGPU: AdapterId = (0, 0x2222);
    const EGPU: AdapterId = (0, 0x3333);
    /// The virtual display driver's own adapter: in the display
    /// configuration, never among DXGI's hardware adapters.
    const IDD: AdapterId = (0, 0x9999);

    fn adapter(id: AdapterId, name: &str) -> Adapter {
        Adapter {
            id,
            name: name.into(),
        }
    }

    /// A hybrid laptop: the panel on the Radeon iGPU, the app running on the
    /// NVIDIA card (the 0.3.3 field case).
    fn laptop_on_nvidia() -> GpuFacts {
        GpuFacts {
            adapters: vec![
                adapter(DGPU, "NVIDIA GeForce RTX 5060 Laptop GPU"),
                adapter(IGPU, "AMD Radeon 780M Graphics"),
            ],
            power_saving: Some(IGPU),
            high_performance: Some(DGPU),
            screens: vec![(r"\\.\DISPLAY1".into(), IGPU)],
        }
    }

    #[test]
    fn the_switch_names_the_card_the_screen_is_wired_to() {
        let facts = laptop_on_nvidia();
        let diagnosis = diagnose(&facts, r"\\.\DISPLAY1").unwrap();
        assert_eq!(diagnosis.screen_gpu, "AMD Radeon 780M Graphics");
        assert_eq!(diagnosis.app_gpu, "NVIDIA GeForce RTX 5060 Laptop GPU");
        assert_eq!(diagnosis.preference, Some(Preference::PowerSaving));
        // GDI names compare without case, as Windows treats them.
        assert!(diagnose(&facts, r"\\.\display1").is_some());
        // The reference PC forced onto its iGPU, with its monitors on the
        // RTX card: High performance moves it back.
        let desktop = GpuFacts {
            adapters: vec![
                adapter(IGPU, "AMD Radeon(TM) Graphics"),
                adapter(DGPU, "NVIDIA GeForce RTX 5080"),
            ],
            power_saving: Some(IGPU),
            high_performance: Some(DGPU),
            screens: vec![
                (r"\\.\DISPLAY1".into(), DGPU),
                (r"\\.\DISPLAY2".into(), DGPU),
            ],
        };
        let diagnosis = diagnose(&desktop, r"\\.\DISPLAY2").unwrap();
        assert_eq!(diagnosis.preference, Some(Preference::HighPerformance));
        assert_eq!(diagnosis.screen_gpu, "NVIDIA GeForce RTX 5080");
    }

    #[test]
    fn mixed_screens_use_the_card_of_the_screen_being_captured() {
        // Internal panel on the iGPU, external monitor on the NVIDIA card,
        // app on the NVIDIA card: only the panel needs a switch.
        let mut facts = laptop_on_nvidia();
        facts.screens.push((r"\\.\DISPLAY2".into(), DGPU));
        assert_eq!(
            diagnose(&facts, r"\\.\DISPLAY1").unwrap().preference,
            Some(Preference::PowerSaving)
        );
        assert_eq!(diagnose(&facts, r"\\.\DISPLAY2").unwrap().preference, None);
        // The same laptop running on its iGPU: the external monitor needs
        // High performance.
        facts.adapters.reverse();
        assert_eq!(
            diagnose(&facts, r"\\.\DISPLAY2").unwrap().preference,
            Some(Preference::HighPerformance)
        );
    }

    #[test]
    fn the_virtual_second_screen_never_asks_for_a_switch() {
        // The virtual display's path names the driver's own adapter, which
        // is no hardware card; the driver renders on the app's card.
        let mut facts = laptop_on_nvidia();
        facts.screens.push((r"\\.\DISPLAY9".into(), IDD));
        assert_eq!(diagnose(&facts, r"\\.\DISPLAY9"), None);
        // A screen that went away meanwhile gives no answer either.
        assert_eq!(diagnose(&facts, r"\\.\DISPLAY5"), None);
    }

    #[test]
    fn a_mux_laptop_in_dgpu_only_mode_or_a_single_card_has_nothing_to_switch() {
        // dGPU-only mode hides the iGPU: one card runs the app and the panel.
        let facts = GpuFacts {
            adapters: vec![adapter(DGPU, "NVIDIA GeForce RTX 4070 Laptop GPU")],
            power_saving: Some(DGPU),
            high_performance: Some(DGPU),
            screens: vec![(r"\\.\DISPLAY1".into(), DGPU)],
        };
        assert_eq!(diagnose(&facts, r"\\.\DISPLAY1").unwrap().preference, None);
        assert!(!facts.has_choice());
        assert!(laptop_on_nvidia().has_choice());
    }

    #[test]
    fn with_three_cards_only_a_preference_that_selects_the_card_is_offered() {
        // iGPU, internal NVIDIA card and an eGPU that wins High performance.
        let facts = GpuFacts {
            adapters: vec![
                adapter(IGPU, "Intel(R) Iris(R) Xe Graphics"),
                adapter(DGPU, "NVIDIA GeForce RTX 3050 Laptop GPU"),
                adapter(EGPU, "NVIDIA GeForce RTX 4090"),
            ],
            power_saving: Some(IGPU),
            high_performance: Some(EGPU),
            screens: vec![
                (r"\\.\DISPLAY1".into(), IGPU),
                (r"\\.\DISPLAY2".into(), DGPU),
                (r"\\.\DISPLAY3".into(), EGPU),
            ],
        };
        // No value reaches the internal card; the notice explains Windows
        // Settings instead.
        let internal = diagnose(&facts, r"\\.\DISPLAY2").unwrap();
        assert_eq!(internal.preference, None);
        assert_eq!(internal.screen_gpu, "NVIDIA GeForce RTX 3050 Laptop GPU");
        assert_eq!(
            diagnose(&facts, r"\\.\DISPLAY3").unwrap().preference,
            Some(Preference::HighPerformance)
        );
        assert_eq!(
            facts.card_for(Preference::HighPerformance).unwrap().name,
            "NVIDIA GeForce RTX 4090"
        );
        assert_eq!(
            facts.card_for(Preference::PowerSaving).unwrap().name,
            "Intel(R) Iris(R) Xe Graphics"
        );
        assert_eq!(facts.card_for(Preference::Automatic), None);
    }

    #[test]
    fn each_preference_selects_the_card_windows_ranks_first_for_it() {
        use AdapterKind::*;
        const BASIC: AdapterId = (0, 0x4444);
        // The reference PC as DXCore classified it while forced onto its
        // iGPU: the RTX card, the Radeon iGPU and Microsoft's renderer.
        assert_eq!(
            preference_cards(&[(DGPU, Discrete), (IGPU, Integrated), (BASIC, Ignored)]),
            (Some(IGPU), Some(DGPU))
        );
        // An eGPU wins High performance over the internal card.
        assert_eq!(
            preference_cards(&[(IGPU, Integrated), (DGPU, Discrete), (EGPU, Detachable)]),
            (Some(IGPU), Some(EGPU))
        );
        // A MUX laptop in dGPU-only mode hides its iGPU.
        assert_eq!(preference_cards(&[(DGPU, Discrete)]), (None, Some(DGPU)));
        // Two internal cards, or a card that might be another eGPU: High
        // performance is not certain.
        assert_eq!(
            preference_cards(&[(IGPU, Integrated), (DGPU, Discrete), (EGPU, Discrete)]),
            (Some(IGPU), None)
        );
        assert_eq!(
            preference_cards(&[
                (IGPU, Integrated),
                (DGPU, NotIntegrated),
                (EGPU, Detachable)
            ]),
            (Some(IGPU), None)
        );
        // An older Windows that can't say whether a card is detachable.
        assert_eq!(
            preference_cards(&[(IGPU, Integrated), (DGPU, NotIntegrated)]),
            (Some(IGPU), Some(DGPU))
        );
        // One adapter Windows can't classify makes neither certain; nor
        // does no DXCore at all.
        assert_eq!(
            preference_cards(&[(IGPU, Integrated), (DGPU, Unknown)]),
            (None, None)
        );
        assert_eq!(preference_cards(&[]), (None, None));
    }

    #[test]
    fn only_the_gpu_preference_changes_in_the_stored_value() {
        assert_eq!(
            with_preference(None, Preference::PowerSaving).as_deref(),
            Some("GpuPreference=1;")
        );
        assert_eq!(
            with_preference(Some("GpuPreference=2;"), Preference::PowerSaving).as_deref(),
            Some("GpuPreference=1;")
        );
        assert_eq!(
            with_preference(
                Some("SwapEffectUpgradeEnable=1;GpuPreference=2;VRROptimizeEnable=0;"),
                Preference::PowerSaving
            )
            .as_deref(),
            Some("SwapEffectUpgradeEnable=1;VRROptimizeEnable=0;GpuPreference=1;")
        );
        // Automatic removes only the preference, or the value when nothing
        // else is in it.
        assert_eq!(
            with_preference(
                Some("AutoHDREnable=1;GpuPreference=1;"),
                Preference::Automatic
            )
            .as_deref(),
            Some("AutoHDREnable=1;")
        );
        assert_eq!(
            with_preference(Some("GpuPreference=1;"), Preference::Automatic),
            None
        );
        assert_eq!(
            preference_in(Some("AutoHDREnable=1;GpuPreference=2;")),
            Some(2)
        );
        assert_eq!(preference_in(Some("gpupreference = 1")), Some(1));
        assert_eq!(preference_in(Some("AutoHDREnable=1;")), None);
        assert_eq!(preference_in(None), None);
    }

    /// `UserGpuPreferences` as a map from executable path to value, with
    /// this copy's own path fixed, as the Windows store is.
    struct FakeKey {
        values: std::collections::BTreeMap<String, String>,
        own: String,
        denied: bool,
    }

    impl PreferenceStore for FakeKey {
        fn read(&self) -> Result<Option<String>, String> {
            Ok(self.values.get(&self.own).cloned())
        }
        fn write(&mut self, value: Option<&str>) -> Result<(), String> {
            if self.denied {
                return Err("access denied".into());
            }
            match value {
                Some(value) => self.values.insert(self.own.clone(), value.into()),
                None => self.values.remove(&self.own),
            };
            Ok(())
        }
    }

    fn key(own: Option<&str>) -> FakeKey {
        let mut values = std::collections::BTreeMap::new();
        values.insert(
            r"C:\Games\game.exe".to_string(),
            "GpuPreference=2;".to_string(),
        );
        if let Some(own) = own {
            values.insert(
                r"C:\EM\EternalMonitor-host.exe".to_string(),
                own.to_string(),
            );
        }
        FakeKey {
            values,
            own: r"C:\EM\EternalMonitor-host.exe".into(),
            denied: false,
        }
    }

    #[test]
    fn a_switch_and_its_undo_touch_only_eternalmonitors_value() {
        let mut store = key(None);
        let (before, after) = apply(&mut store, &Change::Set(Preference::PowerSaving)).unwrap();
        assert_eq!(before, None);
        assert_eq!(after.as_deref(), Some("GpuPreference=1;"));
        assert_eq!(store.values[r"C:\Games\game.exe"], "GpuPreference=2;");
        // Undo removes the value it added.
        apply(&mut store, &Change::Restore(before)).unwrap();
        assert_eq!(store.values.len(), 1);
        assert_eq!(store.values[r"C:\Games\game.exe"], "GpuPreference=2;");
    }

    #[test]
    fn a_preference_the_user_already_set_comes_back_on_undo() {
        // The user chose High performance in Windows Settings earlier, with
        // another per-app setting beside it.
        let mut store = key(Some("SwapEffectUpgradeEnable=1;GpuPreference=2;"));
        let current = store.read().unwrap();
        let record = SwitchRecord::new(
            None,
            current.clone(),
            Preference::PowerSaving,
            "AMD Radeon 780M Graphics",
        );
        apply(&mut store, &Change::Set(Preference::PowerSaving)).unwrap();
        assert_eq!(
            store.read().unwrap().as_deref(),
            Some("SwapEffectUpgradeEnable=1;GpuPreference=1;")
        );
        // A second switch keeps the user's original value for Undo.
        let again = SwitchRecord::new(
            Some(&record),
            store.read().unwrap(),
            Preference::HighPerformance,
            "NVIDIA",
        );
        assert_eq!(again.before, current);
        apply(&mut store, &Change::Restore(again.before)).unwrap();
        assert_eq!(store.read().unwrap(), current);
    }

    #[test]
    fn a_refused_write_reports_an_error_and_changes_nothing() {
        // A locked-down account: HKCU writes refused. No administrator path
        // is tried.
        let mut store = key(None);
        store.denied = true;
        assert!(apply(&mut store, &Change::Set(Preference::PowerSaving)).is_err());
        assert_eq!(store.read().unwrap(), None);
        // Nothing to change writes nothing, so it can't fail.
        assert!(apply(&mut store, &Change::Restore(None)).is_ok());
    }

    #[test]
    fn the_relaunched_app_reports_whether_the_switch_helped_once() {
        let mut record = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        assert!(record.checking);
        assert!(settle(&mut record, Outcome::StillUnsupported));
        assert!(record.failed && !record.checking);
        // Later retries in the same state log nothing new.
        assert!(!settle(&mut record, Outcome::StillUnsupported));
        // The user fixed it in the NVIDIA app: one line, and the notice goes.
        assert!(settle(&mut record, Outcome::Worked));
        assert!(!record.failed && !record.checking);
        assert!(!settle(&mut record, Outcome::Worked));
        let mut record = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        assert!(settle(&mut record, Outcome::Worked));
        assert!(!record.failed);
    }

    #[test]
    fn the_relaunched_app_can_tell_a_switch_that_was_never_saved() {
        let record = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        assert!(switch_saved(
            &record,
            Some("AutoHDREnable=1;GpuPreference=1;")
        ));
        // The helper's HKCU write was refused, or another tool changed it.
        assert!(!switch_saved(&record, None));
        assert!(!switch_saved(&record, Some("GpuPreference=2;")));
    }

    #[test]
    fn a_helper_copy_finds_its_request_and_reopens_with_the_users_arguments() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(helper_request(&args(&["19876"])), None);
        assert_eq!(helper_request(&args(&[AFTER_EXIT_ARG, "x"])), None);
        let helper = helper_args(
            args(&["19876", AFTER_EXIT_ARG, "7", CHANGE_ARG, "1"]),
            4242,
            Some(&Change::Set(Preference::HighPerformance)),
        );
        assert_eq!(
            helper,
            args(&["19876", AFTER_EXIT_ARG, "4242", CHANGE_ARG, "2"])
        );
        assert_eq!(
            helper_request(&helper),
            Some((4242, Some(Change::Set(Preference::HighPerformance))))
        );
        assert_eq!(own_args(helper), args(&["19876"]));
        // A plain restart and an undo to a value or to none.
        assert_eq!(
            helper_request(&helper_args(args(&[]), 9, None)),
            Some((9, None))
        );
        let undo = Change::Restore(Some("AutoHDREnable=1;GpuPreference=2;".into()));
        assert_eq!(
            helper_request(&helper_args(args(&[]), 9, Some(&undo))),
            Some((9, Some(undo)))
        );
        let remove = Change::Restore(None);
        assert_eq!(
            helper_request(&helper_args(args(&[]), 9, Some(&remove))),
            Some((9, Some(remove)))
        );
        // A value outside 0-2 is never written.
        assert_eq!(
            helper_request(&args(&[AFTER_EXIT_ARG, "9", CHANGE_ARG, "3"])),
            Some((9, None))
        );
    }

    /// The old app and the single-instance lock it holds until it exits, as
    /// a helper copy sees them. The app exits `exits_after` into the wait
    /// (never when `None`). Events go to a log the test's save shares.
    struct FakeApp {
        exits_after: Option<std::time::Duration>,
        clock: std::time::Duration,
        events: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        launch_error: Option<String>,
    }

    impl FakeApp {
        fn new(exits_after: Option<u64>) -> Self {
            Self {
                exits_after: exits_after.map(std::time::Duration::from_secs),
                clock: std::time::Duration::ZERO,
                events: Default::default(),
                launch_error: None,
            }
        }

        fn running(&self) -> bool {
            self.exits_after.is_none_or(|at| self.clock < at)
        }

        fn events(&self) -> Vec<String> {
            self.events.borrow().clone()
        }
    }

    impl HelperHost for FakeApp {
        fn wait_for_exit(&mut self, timeout: std::time::Duration) -> bool {
            self.clock = match self.exits_after {
                Some(at) if at <= self.clock + timeout => at.max(self.clock),
                _ => self.clock + timeout,
            };
            if !self.running() {
                self.events
                    .borrow_mut()
                    .push(format!("exited after {} s", self.clock.as_secs()));
            }
            !self.running()
        }
        fn launch(&mut self) -> Result<(), String> {
            // A new copy started while the old one holds the lock shows the
            // old (closed) window and quits: no app at all.
            assert!(!self.running(), "started while the old app holds the lock");
            match self.launch_error.clone() {
                Some(error) => Err(error),
                None => {
                    self.events.borrow_mut().push("launch".into());
                    Ok(())
                }
            }
        }
        fn tell(&mut self, title: &str, body: &str) {
            self.events
                .borrow_mut()
                .push(format!("tell: {title}: {body}"));
        }
        fn note(&mut self, line: &str) {
            self.events.borrow_mut().push(format!("note: {line}"));
        }
    }

    #[test]
    fn a_helper_waits_out_a_slow_shutdown_before_saving_and_reopening() {
        // Closing with a virtual display Disable that runs its full 35 s,
        // then the worker's 5 s: past the old 30 s wait.
        let mut app = FakeApp::new(Some(41));
        let events = app.events.clone();
        let mut store = key(None);
        let change = Change::Set(Preference::HighPerformance);
        let end = run_helper(&mut app, Some(&change), |change| {
            events.borrow_mut().push("save".into());
            apply(&mut store, change).map(drop)
        });
        assert_eq!(end, HelperEnd::Reopened);
        assert_eq!(app.events(), ["exited after 41 s", "save", "launch"]);
        assert_eq!(store.read().unwrap().as_deref(), Some("GpuPreference=2;"));
    }

    #[test]
    fn a_helper_changes_and_starts_nothing_while_the_app_still_runs() {
        let mut app = FakeApp::new(None);
        let mut saved = false;
        let end = run_helper(
            &mut app,
            Some(&Change::Set(Preference::PowerSaving)),
            |_| {
                saved = true;
                Ok(())
            },
        );
        assert_eq!(end, HelperEnd::StillRunning);
        assert!(!saved, "wrote the preference while the app still ran");
        assert_eq!(app.clock, WAIT_FOR_EXIT);
        // No launch; one line for the next start and one message.
        assert_eq!(app.events().len(), 2, "{:?}", app.events());
        assert!(app.events()[0].starts_with(
            "note: Graphics card helper: EternalMonitor was still running after 120 s"
        ));
        assert!(app.events()[1].starts_with(&format!(
            "tell: {SLOW_EXIT_TITLE}: The graphics card wasn't changed."
        )));
        assert!(app.events()[1].contains("open it from the Start menu"));
        // A plain restart (Window drawing) says only how to reopen.
        let mut app = FakeApp::new(Some(500));
        assert_eq!(
            run_helper(&mut app, None, |_| Ok(())),
            HelperEnd::StillRunning
        );
        assert_eq!(
            app.events()[1],
            format!("tell: {SLOW_EXIT_TITLE}: Wait until EternalMonitor has closed, then open it from the Start menu.")
        );
    }

    #[test]
    fn a_failed_reopen_tells_the_user_and_the_next_start() {
        let mut app = FakeApp::new(Some(0));
        app.launch_error =
            Some("could not open EternalMonitor again: Access is denied. (0x80070005)".into());
        let mut store = key(None);
        let end = run_helper(
            &mut app,
            Some(&Change::Set(Preference::PowerSaving)),
            |change| apply(&mut store, change).map(drop),
        );
        assert_eq!(end, HelperEnd::NotReopened);
        assert_eq!(store.read().unwrap().as_deref(), Some("GpuPreference=1;"));
        assert_eq!(
            app.events(),
            [
                "exited after 0 s".to_string(),
                "note: Graphics card helper: EternalMonitor could not be reopened (could not open EternalMonitor again: Access is denied. (0x80070005)); setting saved: yes".to_string(),
                format!("tell: {REOPEN_FAILED_TITLE}: Open EternalMonitor from the Start menu. The graphics card setting was saved."),
            ]
        );
        // A refused write and a failed start: both noted, and the message
        // doesn't claim the setting was saved.
        let mut app = FakeApp::new(Some(0));
        app.launch_error = Some("no".into());
        let mut store = key(None);
        store.denied = true;
        run_helper(
            &mut app,
            Some(&Change::Set(Preference::PowerSaving)),
            |change| apply(&mut store, change).map(drop),
        );
        assert_eq!(
            app.events()[1],
            "note: Graphics card helper: the graphics setting was not saved (access denied)"
        );
        assert!(app.events()[2].ends_with("setting saved: no"));
        assert_eq!(
            app.events()[3],
            format!("tell: {REOPEN_FAILED_TITLE}: Open EternalMonitor from the Start menu. The graphics card setting couldn't be saved.")
        );
        // A plain restart that can't start again.
        let mut app = FakeApp::new(Some(0));
        app.launch_error = Some("no".into());
        run_helper(&mut app, None, |_| unreachable!());
        assert_eq!(
            app.events()[2],
            format!("tell: {REOPEN_FAILED_TITLE}: Open EternalMonitor from the Start menu.")
        );
    }

    #[test]
    fn the_next_start_reads_each_helper_note_once() {
        let dir = std::env::temp_dir().join(format!(
            "eternal-helper-notes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(take_notes_in(&dir).is_empty());
        leave_note_in(&dir, "first").unwrap();
        leave_note_in(&dir, "second\r\nline").unwrap();
        assert_eq!(take_notes_in(&dir), ["first", "second  line"]);
        assert!(take_notes_in(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_switch_the_helper_never_saved_is_offered_again() {
        let mut record = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        assert!(started_after_switch(&mut record, Some("GpuPreference=1;")));
        assert!(record.checking);
        // Not saved (the app took too long to close, or HKCU refused it):
        // a refused capture must not read as another app choosing the card.
        assert!(!started_after_switch(&mut record, None));
        assert!(!record.checking);
        assert!(!settle(&mut record, Outcome::StillUnsupported));
        assert!(!record.failed);
    }

    #[test]
    fn the_check_repeats_only_after_a_failed_read_or_a_change() {
        let mut check = GpuCheck::new();
        // QueryDisplayConfig failed: nothing is shown and nothing is marked.
        assert_eq!(check.check(r"\\.\DISPLAY1", None), None);
        let facts = laptop_on_nvidia();
        let diagnosis = check.check(r"\\.\DISPLAY1", Some(facts.clone())).unwrap();
        assert_eq!(diagnosis.unwrap().preference, Some(Preference::PowerSaving));
        // The same screen and facts on the next retry: no new line.
        assert_eq!(check.check(r"\\.\display1", Some(facts.clone())), None);
        assert_eq!(check.check(r"\\.\DISPLAY1", None), None);
        // A topology change (a monitor plugged into the NVIDIA card) is a
        // new answer.
        let mut changed = facts.clone();
        changed.screens.push((r"\\.\DISPLAY2".into(), DGPU));
        assert!(check
            .check(r"\\.\DISPLAY1", Some(changed.clone()))
            .is_some());
        assert_eq!(
            check
                .check(r"\\.\DISPLAY2", Some(changed))
                .unwrap()
                .unwrap()
                .preference,
            None
        );
        // A screen no hardware card drives is checked once too.
        let mut virtual_screen = facts;
        virtual_screen.screens.push((r"\\.\DISPLAY9".into(), IDD));
        assert_eq!(
            check.check(r"\\.\DISPLAY9", Some(virtual_screen.clone())),
            Some(None)
        );
        assert_eq!(check.check(r"\\.\DISPLAY9", Some(virtual_screen)), None);
    }

    #[test]
    fn a_display_configuration_read_retries_while_the_screens_change() {
        // The documented race: the paths change between sizing and reading.
        let mut reads = 0;
        let result = read_display_config(|| {
            reads += 1;
            if reads == 1 {
                Err(ConfigRead::Changed)
            } else {
                Ok(vec!["DISPLAY1"])
            }
        });
        assert_eq!(result, Some(vec!["DISPLAY1"]));
        assert_eq!(reads, 2);
        // Any other error gives up at once; endless changes give up too.
        let mut reads = 0;
        assert_eq!(
            read_display_config(|| -> Result<(), _> {
                reads += 1;
                Err(ConfigRead::Failed)
            }),
            None
        );
        assert_eq!(reads, 1);
        let mut reads = 0;
        assert_eq!(
            read_display_config(|| -> Result<(), _> {
                reads += 1;
                Err(ConfigRead::Changed)
            }),
            None
        );
        assert_eq!(reads, CONFIG_READ_ATTEMPTS);
    }

    #[test]
    fn uninstall_removes_only_eternalmonitors_own_value() {
        let iss = include_str!("../../installer/EternalMonitor.iss");
        let code: Vec<&str> = iss
            .lines()
            .filter(|line| line.contains("UserGpuPreferences"))
            .collect();
        assert!(!code.is_empty(), "the uninstaller leaves the value behind");
        for line in &code {
            assert!(
                !line.contains("RegDeleteKey") && !line.contains("deletekey"),
                "{line}"
            );
        }
        assert!(iss.contains(r"RegDeleteValue(HKEY_CURRENT_USER, GpuPreferencesKey, ExpandConstant('{app}\EternalMonitor-host.exe'))"));
    }
}
