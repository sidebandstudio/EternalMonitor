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
//! process starts. The running app writes its own value when the user asks,
//! which changes nothing for itself, then restarts (`relaunch`): the new copy,
//! and any launch after the write, starts on the chosen card. Nothing is
//! written unless the user asks.

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

/// The screen EternalMonitor mirrors moved from one card to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub from: String,
    pub to: String,
}

/// Which card the screen EternalMonitor mirrors is on, as its captures show
/// it: the card the app runs on while Windows duplicates the screen, the
/// card it is wired to after a refusal. A laptop that moves its screen
/// between its cards while the app runs (a display mux, or Windows on its
/// own) changes it.
#[derive(Debug, Default)]
pub struct ScreenWatch {
    check: GpuCheck,
    card: Option<String>,
}

impl ScreenWatch {
    pub const fn new() -> Self {
        Self {
            check: GpuCheck::new(),
            card: None,
        }
    }

    /// Windows duplicates the screen from `app_gpu`, the card the app runs
    /// on, so the screen is on that card. Returns the move when it was on
    /// another one. An earlier refusal's diagnosis no longer holds: the
    /// next refusal is checked afresh, even with the same facts.
    pub fn works(&mut self, app_gpu: &str) -> Option<Moved> {
        self.check = GpuCheck::new();
        self.moved_to(app_gpu)
    }

    /// [`GpuCheck::check`] after a refused duplication, and the move when
    /// the card the screen is wired to is new.
    pub fn refused(
        &mut self,
        screen: &str,
        facts: Option<GpuFacts>,
    ) -> (Option<Option<Diagnosis>>, Option<Moved>) {
        let checked = self.check.check(screen, facts);
        let moved = match &checked {
            Some(Some(diagnosis)) => self.moved_to(&diagnosis.screen_gpu),
            _ => None,
        };
        (checked, moved)
    }

    fn moved_to(&mut self, card: &str) -> Option<Moved> {
        let from = self.card.replace(card.to_string())?;
        (from != card).then(|| Moved {
            from,
            to: card.to_string(),
        })
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

/// Whether the stored value holds the switch `record` asked for. False when
/// something else changed it before this start (Windows Settings, a tool).
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

/// A change to EternalMonitor's own value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Set `GpuPreference`, keeping the value's other settings.
    Set(Preference),
    /// Put back a value exactly as it was (`None`: remove it).
    Restore(Option<String>),
}

impl Change {
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

/// The whole error when settings.json can't hold the record Undo needs.
pub const SETTINGS_NOT_SAVED: &str =
    "EternalMonitor couldn't save its settings, so nothing was changed.";

/// Writes `change` to EternalMonitor's own value while the app runs: Windows
/// reads it only when a process starts, so this process keeps its card.
/// `record` becomes `next` only when the write succeeds, and `persist` saves
/// each record it holds (settings.json). A `next` that keeps an Undo value is
/// saved before the write, so a crash between the two never loses what Undo
/// restores; when it can't be saved, nothing is written
/// ([`SETTINGS_NOT_SAVED`]). A failed write puts the earlier record back and
/// changes nothing else.
pub fn save_change(
    store: &mut impl PreferenceStore,
    record: &mut Option<SwitchRecord>,
    change: &Change,
    next: Option<SwitchRecord>,
    mut persist: impl FnMut(&Option<SwitchRecord>) -> Result<(), String>,
) -> Result<(), String> {
    let previous = record.clone();
    if next.is_some() && next != previous {
        // The user sees only SETTINGS_NOT_SAVED; the log keeps why.
        persist(&next).map_err(|error| {
            tracing::warn!(
                %error,
                "Could not save the graphics switch record before the switch"
            );
            SETTINGS_NOT_SAVED.to_string()
        })?;
        *record = next.clone();
    }
    if let Err(error) = apply(store, change) {
        if *record != previous {
            *record = previous;
            // Unsaved, the file keeps `next`: the next start sees the
            // switch isn't in the value (`started_after_switch`).
            let _ = persist(record);
        }
        return Err(error);
    }
    if *record != next {
        *record = next;
        // The value is written, so this worked; a file that keeps the old
        // record gets this one at its next save.
        let _ = persist(record);
    }
    Ok(())
}

/// The Stream page's switch to `preference`, the card `gpu`. A switch made
/// earlier keeps the user's original value for Undo.
pub fn switch(
    store: &mut impl PreferenceStore,
    record: &mut Option<SwitchRecord>,
    preference: Preference,
    gpu: &str,
    persist: impl FnMut(&Option<SwitchRecord>) -> Result<(), String>,
) -> Result<(), String> {
    let current = store.read()?;
    let next = SwitchRecord::new(record.as_ref(), current, preference, gpu);
    save_change(store, record, &Change::Set(preference), Some(next), persist)
}

/// Undo: EternalMonitor's value back to what it was before the first switch,
/// or removed if there was none. The record (and with it Undo) goes only
/// once that is written.
pub fn undo(
    store: &mut impl PreferenceStore,
    record: &mut Option<SwitchRecord>,
    persist: impl FnMut(&Option<SwitchRecord>) -> Result<(), String>,
) -> Result<(), String> {
    let Some(before) = record.as_ref().map(|record| record.before.clone()) else {
        return Ok(());
    };
    save_change(store, record, &Change::Restore(before), None, persist)
}

/// Whether the switch `record` waits on reached Windows, as the restarted
/// app first sees it. A switch no longer in the value (something else
/// changed it) is no longer awaited, so a refused capture offers it again
/// instead of blaming another app for choosing the card.
pub fn started_after_switch(record: &mut SwitchRecord, value: Option<&str>) -> bool {
    let saved = switch_saved(record, value);
    if !saved {
        record.checking = false;
    }
    saved
}

#[cfg(windows)]
pub use windows_impl::{facts, OwnValue};

/// No graphics preference outside Windows.
#[cfg(not(windows))]
pub fn facts() -> Option<GpuFacts> {
    None
}

/// Outside Windows there is no value: reads find none and writes fail.
#[cfg(not(windows))]
pub struct OwnValue;

#[cfg(not(windows))]
impl OwnValue {
    pub fn new() -> Result<Self, String> {
        Ok(Self)
    }
}

#[cfg(not(windows))]
impl PreferenceStore for OwnValue {
    fn read(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn write(&mut self, _value: Option<&str>) -> Result<(), String> {
        Err("switching graphics cards needs Windows".into())
    }
}

/// EternalMonitor's own value now.
pub fn read_value() -> Result<Option<String>, String> {
    OwnValue::new()?.read()
}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt;

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

    use super::{AdapterId, AdapterKind, GpuFacts, PreferenceStore};

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
    pub struct OwnValue {
        name: Vec<u16>,
    }

    impl OwnValue {
        pub fn new() -> Result<Self, String> {
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
    fn the_restarted_app_can_tell_a_switch_that_is_no_longer_saved() {
        let record = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        assert!(switch_saved(
            &record,
            Some("AutoHDREnable=1;GpuPreference=1;")
        ));
        // Another tool changed it.
        assert!(!switch_saved(&record, None));
        assert!(!switch_saved(&record, Some("GpuPreference=2;")));
    }

    /// The switch record as settings.json last saved it, and every save.
    #[derive(Default)]
    struct Saved {
        file: Option<SwitchRecord>,
        saves: usize,
    }

    impl Saved {
        fn persist(&mut self) -> impl FnMut(&Option<SwitchRecord>) -> Result<(), String> + '_ {
            |record| {
                self.file = record.clone();
                self.saves += 1;
                Ok(())
            }
        }
    }

    #[test]
    fn a_switch_is_saved_while_the_app_runs_with_its_undo_value_first() {
        let mut store = key(Some("AutoHDREnable=1;"));
        let mut record = None;
        let mut saved = Saved::default();
        switch(
            &mut store,
            &mut record,
            Preference::HighPerformance,
            "NVIDIA GeForce RTX 5080",
            saved.persist(),
        )
        .unwrap();
        assert_eq!(
            store.read().unwrap().as_deref(),
            Some("AutoHDREnable=1;GpuPreference=2;")
        );
        let record = record.unwrap();
        assert_eq!(record.before.as_deref(), Some("AutoHDREnable=1;"));
        assert!(record.checking);
        assert_eq!(saved.file, Some(record));
        // Saved once, before the write: a crash after it still has Undo.
        assert_eq!(saved.saves, 1);
    }

    #[test]
    fn a_refused_switch_changes_nothing_and_keeps_the_earlier_record() {
        // An earlier switch to Power saving, then a switch back that HKCU
        // refuses: the value and the record stay as they were.
        let mut store = key(Some("GpuPreference=1;"));
        let earlier = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        let mut record = Some(earlier.clone());
        store.denied = true;
        let mut saved = Saved {
            file: record.clone(),
            saves: 0,
        };
        let error = switch(
            &mut store,
            &mut record,
            Preference::HighPerformance,
            "NVIDIA",
            saved.persist(),
        )
        .unwrap_err();
        assert_eq!(error, "access denied");
        assert_eq!(store.read().unwrap().as_deref(), Some("GpuPreference=1;"));
        assert_eq!(record, Some(earlier.clone()));
        assert_eq!(saved.file, Some(earlier));
    }

    #[test]
    fn undo_clears_its_record_only_once_the_value_is_back() {
        // The user had High performance; the switch set Power saving.
        let mut store = key(Some("GpuPreference=1;"));
        let switched = SwitchRecord::new(
            None,
            Some("GpuPreference=2;".into()),
            Preference::PowerSaving,
            "AMD",
        );
        // HKCU refuses the restore: Undo and the original value are kept.
        store.denied = true;
        let mut record = Some(switched.clone());
        let mut saved = Saved {
            file: record.clone(),
            saves: 0,
        };
        assert!(undo(&mut store, &mut record, saved.persist()).is_err());
        assert_eq!(record, Some(switched.clone()));
        assert_eq!(saved.file, Some(switched.clone()));
        assert_eq!(saved.saves, 0);
        assert_eq!(store.read().unwrap().as_deref(), Some("GpuPreference=1;"));
        // The next try works: the value is back, then the record goes.
        store.denied = false;
        undo(&mut store, &mut record, saved.persist()).unwrap();
        assert_eq!(store.read().unwrap().as_deref(), Some("GpuPreference=2;"));
        assert_eq!(record, None);
        assert_eq!(saved.file, None);
        // Undo of a switch over no value removes only EternalMonitor's.
        let mut store = key(Some("GpuPreference=1;"));
        let mut record = Some(SwitchRecord::new(
            None,
            None,
            Preference::PowerSaving,
            "AMD",
        ));
        undo(&mut store, &mut record, |_| Ok(())).unwrap();
        assert_eq!(store.values.len(), 1);
        assert_eq!(store.values[r"C:\Games\game.exe"], "GpuPreference=2;");
        // Nothing to undo writes nothing.
        let mut none = None;
        assert!(undo(&mut store, &mut none, |_| unreachable!()).is_ok());
    }

    #[test]
    fn a_switch_whose_undo_record_cant_be_saved_writes_nothing() {
        // settings.json can't be written (its folder is read-only, the disk
        // is full) while HKCU can.
        let mut store = key(Some("AutoHDREnable=1;"));
        let mut record = None;
        let mut tries = 0;
        let error = switch(
            &mut store,
            &mut record,
            Preference::HighPerformance,
            "NVIDIA",
            |_| {
                tries += 1;
                Err("Access is denied. (os error 5)".into())
            },
        )
        .unwrap_err();
        assert_eq!(error, SETTINGS_NOT_SAVED);
        assert_eq!(tries, 1);
        assert_eq!(store.read().unwrap().as_deref(), Some("AutoHDREnable=1;"));
        assert_eq!(record, None);
        // A second switch over an earlier one keeps that one's record.
        let earlier = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        let mut store = key(Some("GpuPreference=1;"));
        let mut record = Some(earlier.clone());
        assert_eq!(
            switch(
                &mut store,
                &mut record,
                Preference::HighPerformance,
                "NVIDIA",
                |_| Err("full".into()),
            ),
            Err(SETTINGS_NOT_SAVED.to_string())
        );
        assert_eq!(store.read().unwrap().as_deref(), Some("GpuPreference=1;"));
        assert_eq!(record, Some(earlier));
    }

    /// The user is told only that nothing was changed; the log says why.
    #[test]
    fn a_record_that_cannot_be_saved_logs_why() {
        use std::sync::{Arc, Mutex};
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer = Arc::clone(&out);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Sink(Arc::clone(&writer)))
            .finish();
        let mut store = key(None);
        let error = tracing::subscriber::with_default(subscriber, || {
            switch(
                &mut store,
                &mut None,
                Preference::HighPerformance,
                "NVIDIA",
                |_| Err("Access is denied. (os error 5)".into()),
            )
        })
        .unwrap_err();
        assert_eq!(error, SETTINGS_NOT_SAVED);
        let log = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("Could not save the graphics switch record")
                && log.contains("error=Access is denied. (os error 5)"),
            "{log}"
        );
    }

    #[test]
    fn a_switch_no_longer_in_the_value_is_offered_again() {
        let mut record = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        assert!(started_after_switch(&mut record, Some("GpuPreference=1;")));
        assert!(record.checking);
        // Something else changed the value before this start: a refused
        // capture must not read as another app choosing the card.
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

    /// The 0.3.4 beta field case: the screen of a laptop with an AMD iGPU
    /// and an NVIDIA card, the app on NVIDIA, moved to the iGPU and back.
    /// Each move is reported once, with both cards, and after the way back
    /// the same refusal names the iGPU again.
    #[test]
    fn a_screen_moving_between_cards_is_reported_once_each_way() {
        const NVIDIA: &str = "NVIDIA GeForce RTX 5060 Laptop GPU";
        const AMD: &str = "AMD Radeon 780M Graphics";
        let mut watch = ScreenWatch::new();
        // On the app's card: Windows duplicates it at every capture start.
        for _ in 0..3 {
            assert_eq!(watch.works(NVIDIA), None);
        }
        // Windows moved it to the iGPU.
        let (checked, moved) = watch.refused(r"\\.\DISPLAY1", Some(laptop_on_nvidia()));
        assert_eq!(checked.unwrap().unwrap().screen_gpu, AMD);
        let to_amd = Moved {
            from: NVIDIA.into(),
            to: AMD.into(),
        };
        assert_eq!(moved, Some(to_amd));
        // The retries every few seconds say nothing new.
        assert_eq!(
            watch.refused(r"\\.\DISPLAY1", Some(laptop_on_nvidia())),
            (None, None)
        );
        // Back on the app's card.
        let to_nvidia = Moved {
            from: AMD.into(),
            to: NVIDIA.into(),
        };
        assert_eq!(watch.works(NVIDIA), Some(to_nvidia));
        assert_eq!(watch.works(NVIDIA), None);
        // And over again, with the same facts: diagnosed again, so the
        // notice can name the iGPU.
        let (checked, moved) = watch.refused(r"\\.\DISPLAY1", Some(laptop_on_nvidia()));
        assert_eq!(checked.unwrap().unwrap().screen_gpu, AMD);
        assert_eq!(moved.unwrap().to, AMD);
    }

    /// A PC whose screen stays on the app's card, as on every PC where Mirror
    /// works: nothing is ever reported, and a refusal Windows can't explain
    /// (no facts, or the virtual screen) moves nothing.
    #[test]
    fn a_screen_on_the_apps_card_is_never_reported_as_moved() {
        let mut watch = ScreenWatch::new();
        assert_eq!(watch.works("NVIDIA GeForce RTX 5080"), None);
        assert_eq!(watch.works("NVIDIA GeForce RTX 5080"), None);
        assert_eq!(watch.refused(r"\\.\DISPLAY1", None), (None, None));
        let mut virtual_screen = laptop_on_nvidia();
        virtual_screen.screens = vec![(r"\\.\DISPLAY9".into(), IDD)];
        assert_eq!(
            watch.refused(r"\\.\DISPLAY9", Some(virtual_screen)),
            (Some(None), None)
        );
        assert_eq!(watch.works("NVIDIA GeForce RTX 5080"), None);
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
