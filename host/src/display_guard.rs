//! Puts the PC's own screens back when turning the virtual display on and
//! off left it with none.
//!
//! When the virtual display's monitor arrives, Windows applies the layout it
//! saved for that set of monitors, or picks one for a set it has never seen.
//! That can leave the desktop on the virtual display alone, and when the
//! monitor then goes, Windows may not fall back to the screens the PC had:
//! a hybrid laptop's panel went black at install this way, and the driver's
//! users report the same (VirtualDrivers/Virtual-Display-Driver#277, #440).
//!
//! Every driver change logs the screens before and after it. After a turn
//! off, if the PC has lost every real screen it had before the display was
//! turned on, the guard re-applies that layout, or else switches Windows to
//! "PC screen only" when the built-in panel was on, and logs the result.
//! Nothing else ever changes the layout, which stays the user's: a PC that
//! keeps any real screen sees no change at all. A turn on is only logged:
//! a layout with the virtual display alone cannot attach, so the attempt
//! times out and its turn off brings the guard in. One attempt per change.
//! Right before "PC screen only" the guard reads the screens again, and
//! stops if one came back or they can't be read.
//!
//! The layout from before a turn on is also kept on disk, so the failure
//! reporter, a new process, compares with it when the host that turned
//! the display on ended. The record names that host by id and start time,
//! and anything else is ignored; a turn off that worked deletes it.
//!
//! `installer/scripts/display-guard.ps1` does the same around Setup's
//! driver steps.

use std::time::Duration;

/// How long a turn off waits for Windows to bring a real screen back by
/// itself before the guard steps in, and for a restore to take.
const SETTLE: Duration = Duration::from_secs(3);
const SETTLE_STEP: Duration = Duration::from_millis(250);
/// Screens listed in one log line.
const MAX_SCREENS: usize = 8;

/// One active display path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub adapter: u64,
    pub target: u32,
    /// The laptop's built-in panel.
    pub internal: bool,
    /// EternalMonitor's virtual display.
    pub virtual_display: bool,
    /// Width, height and refresh rate in millihertz, when Windows gives them.
    pub mode: Option<(u32, u32, u32)>,
}

/// The active paths read at one moment, with what Windows needs to apply
/// them again.
#[derive(Debug, Clone)]
pub struct Layout<S> {
    pub screens: Vec<Screen>,
    pub saved: S,
}

fn real(screens: &[Screen]) -> impl Iterator<Item = &Screen> {
    screens.iter().filter(|screen| !screen.virtual_display)
}

/// What a turn off calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The PC still has a real screen, or had none to lose.
    Leave,
    /// Every real screen is gone: re-apply the saved layout, and if that
    /// does not bring one back, "PC screen only" when `internal`, that is
    /// when the built-in panel was on. A switch to it would turn off an
    /// external monitor the user had on alone.
    Restore { internal: bool },
}

/// The decision, from the screens before and after the change.
pub fn plan(before: &[Screen], after: &[Screen]) -> Plan {
    if real(before).next().is_none() || real(after).next().is_some() {
        return Plan::Leave;
    }
    Plan::Restore {
        internal: real(before).any(|screen| screen.internal),
    }
}

/// The screens as one log value: the count of real screens, then each
/// one's kind, target and mode. The virtual display only counts.
pub fn line(screens: Option<&[Screen]>) -> String {
    let Some(screens) = screens else {
        return "paths=unknown".into();
    };
    let shown: Vec<String> = real(screens)
        .take(MAX_SCREENS)
        .map(|screen| {
            let mode = match screen.mode {
                Some((width, height, millihertz)) => {
                    let hz = format!("{:.2}", millihertz as f64 / 1000.0);
                    let hz = hz.trim_end_matches('0').trim_end_matches('.');
                    format!(" {width}x{height}@{hz}Hz")
                }
                None => String::new(),
            };
            format!(
                "{} tgt={:x}/{}{mode}",
                if screen.internal {
                    "internal"
                } else {
                    "external"
                },
                screen.adapter,
                screen.target
            )
        })
        .collect();
    let count = real(screens).count();
    let more = if count > MAX_SCREENS {
        format!(" | +{} more", count - MAX_SCREENS)
    } else {
        String::new()
    };
    let vdd = screens.len() - count;
    format!("paths={count} [{}{more}] vdd={vdd}", shown.join(" | "))
}

/// What the guard needs from Windows.
pub trait Displays {
    type Saved: Saved;
    /// The active paths now. `None` when Windows can't say.
    fn read(&mut self) -> Option<Layout<Self::Saved>>;
    /// `SetDisplayConfig` with the saved paths and modes. The error is its code.
    fn reapply(&mut self, saved: &Self::Saved) -> Result<(), i32>;
    /// `SetDisplayConfig` with "PC screen only".
    fn internal_only(&mut self) -> Result<(), i32>;
    fn sleep(&mut self, duration: Duration);
    fn log(&mut self, warn: bool, text: &str);
    /// The host whose layout from before its turn on is kept on disk: this
    /// one, or in the failure reporter the one that ended. `None` when a
    /// later process with the same id could not be told from it.
    fn owner(&self) -> Option<String>;
    /// Replace the kept record, or delete it with `None`.
    fn keep(&mut self, record: Option<&str>);
    /// The kept record, if there is one.
    fn kept(&mut self) -> Option<String>;
}

/// A layout's saved paths and modes as one line of text, for the kept record.
pub trait Saved: Sized {
    fn encode(&self) -> String;
    fn decode(text: &str) -> Option<Self>;
}

/// How a turn off ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do, or nothing known to compare with.
    Left,
    Restored {
        internal: bool,
    },
    /// Windows brought a real screen back while the guard was restoring, so
    /// it changed nothing more.
    Recovered,
    Failed,
}

const RECORD_HEADER: &str = "eternalmonitor-screens 1";
/// A kept record longer than this, or with more screens, is not one.
pub const MAX_RECORD: usize = 64 * 1024;
const MAX_RECORD_SCREENS: usize = 64;

/// The kept record: `layout` from before `owner`'s turn on. It holds what
/// `SetDisplayConfig` needs and each screen's kind, target and mode, and
/// nothing that names the PC, its user or a monitor.
pub fn record<S: Saved>(owner: &str, layout: &Layout<S>) -> String {
    let mut text = format!("{RECORD_HEADER}\nowner {owner}\n");
    for screen in &layout.screens {
        let mode = match screen.mode {
            Some((width, height, millihertz)) => format!("{width} {height} {millihertz}"),
            None => "- - -".into(),
        };
        text += &format!(
            "screen {:x} {} {} {} {mode}\n",
            screen.adapter,
            screen.target,
            u8::from(screen.internal),
            u8::from(screen.virtual_display)
        );
    }
    text + &format!("saved {}\n", layout.saved.encode())
}

/// The layout in a kept record, if `owner` wrote it. Anything else, a
/// record from another host, boot or session included, is ignored.
pub fn parse<S: Saved>(owner: &str, text: &str) -> Option<Layout<S>> {
    if text.len() > MAX_RECORD {
        return None;
    }
    let mut lines = text.lines();
    if lines.next()? != RECORD_HEADER || lines.next()?.strip_prefix("owner ")? != owner {
        return None;
    }
    let mut screens = Vec::new();
    for line in lines.by_ref() {
        if let Some(saved) = line.strip_prefix("saved ") {
            return (lines.next().is_none() && screens.len() <= MAX_RECORD_SCREENS)
                .then(|| S::decode(saved))
                .flatten()
                .map(|saved| Layout { screens, saved });
        }
        let fields: Vec<&str> = line.strip_prefix("screen ")?.split(' ').collect();
        let [adapter, target, internal, virtual_display, width, height, millihertz] = fields[..]
        else {
            return None;
        };
        let flag = |text: &str| match text {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        };
        let mode = match (width, height, millihertz) {
            ("-", "-", "-") => None,
            _ => Some((
                width.parse().ok()?,
                height.parse().ok()?,
                millihertz.parse().ok()?,
            )),
        };
        screens.push(Screen {
            adapter: u64::from_str_radix(adapter, 16).ok()?,
            target: target.parse().ok()?,
            internal: flag(internal)?,
            virtual_display: flag(virtual_display)?,
            mode,
        });
    }
    None
}

/// Whether a kept record names `owner`, read or not.
fn names(owner: &str, text: &str) -> bool {
    let mut lines = text.lines();
    lines.next() == Some(RECORD_HEADER)
        && lines.next().and_then(|line| line.strip_prefix("owner ")) == Some(owner)
}

/// Lower-case hex, for saved bytes.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// The layout from before the virtual display was turned on, until the
/// next turn off checks against it.
pub struct Guard<S> {
    reference: Option<Layout<S>>,
}

impl<S> Default for Guard<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Guard<S> {
    pub const fn new() -> Self {
        Self { reference: None }
    }
}

impl<S: Saved> Guard<S> {
    /// Before a driver change: log the screens. A turn on keeps them to
    /// compare with, also on disk for the failure reporter. A turn off
    /// with nothing kept, as at startup or in the reporter, compares with
    /// the record its host kept, else with the screens just before it.
    pub fn before(&mut self, enabled: bool, displays: &mut impl Displays<Saved = S>) {
        let layout = displays.read();
        let text = format!(
            "Screens before the virtual display turns {}: {}",
            if enabled { "on" } else { "off" },
            line(layout.as_ref().map(|layout| &layout.screens[..]))
        );
        displays.log(false, &text);
        if enabled {
            if let Some(owner) = displays.owner() {
                let kept = layout.as_ref().map(|layout| record(&owner, layout));
                displays.keep(kept.as_deref());
            }
            self.reference = layout;
        } else if self.reference.is_none() {
            let kept = displays
                .owner()
                .and_then(|owner| parse::<S>(&owner, &displays.kept()?));
            if let Some(kept) = &kept {
                let text = format!(
                    "Comparing with the screens kept from before the virtual display turned on: {}",
                    line(Some(&kept.screens))
                );
                displays.log(false, &text);
            }
            self.reference = kept.or(layout);
        }
    }

    /// After a driver change: log the screens, and after a turn off,
    /// restore them if every real screen is gone. A turn off that `done`
    /// deletes the kept record once checked.
    pub fn after(
        &mut self,
        enabled: bool,
        done: bool,
        displays: &mut impl Displays<Saved = S>,
    ) -> Outcome {
        if enabled {
            let layout = displays.read();
            let text = format!(
                "Screens after the virtual display turned on: {}",
                line(layout.as_ref().map(|layout| &layout.screens[..]))
            );
            displays.log(false, &text);
            return Outcome::Left;
        }
        let outcome = self.check(displays);
        if done {
            if let Some(owner) = displays.owner() {
                if displays.kept().is_some_and(|text| names(&owner, &text)) {
                    displays.keep(None);
                }
            }
        }
        outcome
    }

    fn check(&mut self, displays: &mut impl Displays<Saved = S>) -> Outcome {
        let reference = self.reference.take();
        let before = reference
            .as_ref()
            .map_or(&[][..], |layout| &layout.screens[..]);
        let after = settle(displays, before);
        let text = format!(
            "Screens after the virtual display turned off: {}",
            line(after.as_ref().map(|layout| &layout.screens[..]))
        );
        displays.log(false, &text);
        let (Some(reference), Some(after)) = (reference, after) else {
            return Outcome::Left;
        };
        let Plan::Restore { internal } = plan(&reference.screens, &after.screens) else {
            return Outcome::Left;
        };
        displays.log(
            true,
            &format!(
                "The PC lost every screen it had before the virtual display turned on; \
                 restoring them. Before: {}",
                line(Some(&reference.screens))
            ),
        );
        // Each step's result, for the log when nothing worked.
        let mut tried = Vec::new();
        match displays.reapply(&reference.saved) {
            Ok(()) => {
                let now = settle(displays, &reference.screens);
                if has_real(&now) {
                    return restored(displays, "saved_layout", &now);
                }
                tried.push("reason=saved_layout_no_screen".to_string());
            }
            Err(code) => tried.push(format!("reason=saved_layout_refused code={code}")),
        }
        // Windows may have changed the screens during that call. Look again
        // right before changing anything more: a screen that came back
        // stays as Windows set it.
        let now = displays.read();
        if has_real(&now) {
            let text = format!(
                "Screens came back without a change outcome=restored applied=none {}",
                line(now.as_ref().map(|layout| &layout.screens[..]))
            );
            displays.log(false, &text);
            return Outcome::Recovered;
        }
        if now.is_none() {
            tried.push("reason=screens_unknown".into());
            return failed(displays, &tried, &now);
        }
        if !internal {
            return failed(displays, &tried, &now);
        }
        match displays.internal_only() {
            Ok(()) => {
                let now = settle(displays, &reference.screens);
                if has_real(&now) {
                    return restored(displays, "internal", &now);
                }
                tried.push("reason=internal_no_screen".into());
                failed(displays, &tried, &now)
            }
            Err(code) => {
                tried.push(format!("reason=internal_refused code={code}"));
                failed(displays, &tried, &now)
            }
        }
    }
}

fn restored<D: Displays>(
    displays: &mut D,
    applied: &str,
    now: &Option<Layout<D::Saved>>,
) -> Outcome {
    let text = format!(
        "Screens restored outcome=restored applied={applied} {}",
        line(now.as_ref().map(|layout| &layout.screens[..]))
    );
    displays.log(false, &text);
    Outcome::Restored {
        internal: applied == "internal",
    }
}

fn failed<D: Displays>(
    displays: &mut D,
    tried: &[String],
    now: &Option<Layout<D::Saved>>,
) -> Outcome {
    let text = format!(
        "Could not restore the screens outcome=restore_failed {}{}; Windows+P then \
         \"PC screen only\" brings the built-in screen back",
        tried
            .iter()
            .map(|step| format!("{step} "))
            .collect::<String>(),
        line(now.as_ref().map(|layout| &layout.screens[..]))
    );
    displays.log(true, &text);
    Outcome::Failed
}

fn has_real<S>(layout: &Option<Layout<S>>) -> bool {
    layout
        .as_ref()
        .is_some_and(|layout| real(&layout.screens).next().is_some())
}

/// The screens once a real one is back, or as they are after [`SETTLE`]. A
/// PC that has one returns at once; only one that lost them all waits.
fn settle<D: Displays>(displays: &mut D, before: &[Screen]) -> Option<Layout<D::Saved>> {
    let mut waited = Duration::ZERO;
    loop {
        let layout = displays.read();
        let lost = layout
            .as_ref()
            .is_some_and(|layout| plan(before, &layout.screens) != Plan::Leave);
        if !lost || waited >= SETTLE {
            return layout;
        }
        displays.sleep(SETTLE_STEP);
        waited += SETTLE_STEP;
    }
}

#[cfg(windows)]
static GUARD: parking_lot::Mutex<Guard<windows_impl::Saved>> =
    parking_lot::Mutex::new(Guard::new());

/// Run a driver change for `owner`'s display with the guard around it.
/// `log` writes one line; `done` tells a change that succeeded.
#[cfg(windows)]
pub(crate) fn around<T>(
    enabled: bool,
    owner: crate::startup::ProcessId,
    log: fn(bool, &str),
    change: impl FnOnce() -> T,
    done: impl FnOnce(&T) -> bool,
) -> T {
    let mut displays = windows_impl::Windows::new(owner, log);
    GUARD.lock().before(enabled, &mut displays);
    let result = change();
    let done = done(&result);
    GUARD.lock().after(enabled, done, &mut displays);
    result
}

#[cfg(windows)]
mod windows_impl {
    use super::{Layout, Screen};
    use std::time::Duration;
    use windows::Win32::Devices::Display::{
        SetDisplayConfig, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED, DISPLAYCONFIG_PATH_INFO,
        QDC_ONLY_ACTIVE_PATHS, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_TOPOLOGY_INTERNAL,
        SDC_USE_SUPPLIED_DISPLAY_CONFIG, SDC_VALIDATE,
    };

    pub type Saved = (Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>);

    /// The kept record, next to the virtual display's owner record.
    const RECORD_FILE: &str = "virtual-display-screens";

    pub struct Windows {
        log: fn(bool, &str),
        owner: Option<String>,
        file: Option<std::path::PathBuf>,
    }

    impl Windows {
        pub fn new(owner: crate::startup::ProcessId, log: fn(bool, &str)) -> Self {
            Self {
                log,
                // Without its start time a process id could be a later process's.
                owner: (owner.start != 0).then(|| owner.to_string()),
                file: crate::settings::app_data_dir().map(|directory| directory.join(RECORD_FILE)),
            }
        }
    }

    /// The structs as Windows filled them: plain data, every byte written.
    fn bytes<T: Copy>(items: &[T]) -> &[u8] {
        unsafe { std::slice::from_raw_parts(items.as_ptr().cast(), std::mem::size_of_val(items)) }
    }

    fn items<T: Copy + Default>(bytes: &[u8]) -> Option<Vec<T>> {
        let size = std::mem::size_of::<T>();
        if !bytes.len().is_multiple_of(size) {
            return None;
        }
        let mut items = vec![T::default(); bytes.len() / size];
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), items.as_mut_ptr().cast(), bytes.len())
        };
        Some(items)
    }

    impl super::Saved for Saved {
        fn encode(&self) -> String {
            format!(
                "{} {}",
                super::to_hex(bytes(&self.0)),
                super::to_hex(bytes(&self.1))
            )
        }

        fn decode(text: &str) -> Option<Self> {
            let (paths, modes) = text.split_once(' ')?;
            Some((
                items(&super::from_hex(paths)?)?,
                items(&super::from_hex(modes)?)?,
            ))
        }
    }

    fn screen(path: &DISPLAYCONFIG_PATH_INFO, modes: &[DISPLAYCONFIG_MODE_INFO]) -> Screen {
        let technology = path.targetInfo.outputTechnology;
        // Without QDC_VIRTUAL_MODE_AWARE the index is the whole field.
        let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let rate = path.targetInfo.refreshRate;
        let mode = modes
            .get(index)
            .filter(|mode| mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
            .map(|mode| {
                let source = unsafe { mode.Anonymous.sourceMode };
                let millihertz = (rate.Numerator as u64 * 1000)
                    .checked_div(rate.Denominator as u64)
                    .unwrap_or(0);
                (source.width, source.height, millihertz as u32)
            });
        Screen {
            adapter: crate::vdd_snapshot::luid_value(path.targetInfo.adapterId),
            target: path.targetInfo.id,
            internal: [
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
            ]
            .contains(&technology),
            virtual_display: crate::vdd_snapshot::is_vdd_monitor(
                &crate::vdd_snapshot::monitor_path(path),
            ),
            mode,
        }
    }

    impl super::Displays for Windows {
        type Saved = Saved;

        fn read(&mut self) -> Option<Layout<Saved>> {
            let (paths, modes) = crate::vdd_snapshot::display_config(QDC_ONLY_ACTIVE_PATHS)?;
            Some(Layout {
                screens: paths.iter().map(|path| screen(path, &modes)).collect(),
                saved: (paths, modes),
            })
        }

        fn reapply(&mut self, (paths, modes): &Saved) -> Result<(), i32> {
            let supplied = SDC_USE_SUPPLIED_DISPLAY_CONFIG;
            // A saved adapter or target that is gone fails the check, and
            // nothing changes.
            for flags in [
                supplied | SDC_VALIDATE,
                supplied | SDC_APPLY | SDC_ALLOW_CHANGES,
            ] {
                match unsafe {
                    SetDisplayConfig(Some(paths.as_slice()), Some(modes.as_slice()), flags)
                } {
                    0 => {}
                    code => return Err(code),
                }
            }
            Ok(())
        }

        fn internal_only(&mut self) -> Result<(), i32> {
            match unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_TOPOLOGY_INTERNAL) } {
                0 => Ok(()),
                code => Err(code),
            }
        }

        fn sleep(&mut self, duration: Duration) {
            std::thread::sleep(duration);
        }

        fn log(&mut self, warn: bool, text: &str) {
            (self.log)(warn, text);
        }

        fn owner(&self) -> Option<String> {
            self.owner.clone()
        }

        fn keep(&mut self, record: Option<&str>) {
            let Some(file) = &self.file else { return };
            let result = match record {
                // Replaced in one step, so the reporter never reads half of it.
                Some(record) => {
                    let temp = file.with_extension("tmp");
                    std::fs::write(&temp, record).and_then(|()| std::fs::rename(&temp, file))
                }
                None => match std::fs::remove_file(file) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    result => result,
                },
            };
            if let Err(error) = result {
                (self.log)(
                    true,
                    &format!("Could not keep the screens for the failure reporter error={error}"),
                );
            }
        }

        fn kept(&mut self) -> Option<String> {
            use std::io::Read;
            let mut text = String::new();
            std::fs::File::open(self.file.as_ref()?)
                .ok()?
                .take(super::MAX_RECORD as u64 + 1)
                .read_to_string(&mut text)
                .ok()?;
            Some(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn panel() -> Screen {
        Screen {
            adapter: 0xd2c1,
            target: 4352,
            internal: true,
            virtual_display: false,
            mode: Some((1920, 1200, 165_000)),
        }
    }

    fn monitor() -> Screen {
        Screen {
            adapter: 0xd2c1,
            target: 4353,
            internal: false,
            virtual_display: false,
            mode: Some((2560, 1440, 59_940)),
        }
    }

    fn vdd() -> Screen {
        Screen {
            adapter: 0x2_0000_0101,
            target: 256,
            internal: false,
            virtual_display: true,
            mode: Some((2732, 2048, 60_000)),
        }
    }

    /// Windows as the guard sees it: each read takes the next layout, the
    /// last one repeating, and each `SetDisplayConfig` call is counted.
    /// `Saved` names the layout a read returned.
    #[derive(Default)]
    struct Fake {
        reads: VecDeque<Option<Vec<Screen>>>,
        last: Option<Vec<Screen>>,
        read_count: usize,
        /// After an accepted call, reads return this.
        reapplied: Option<Vec<Screen>>,
        internal: Option<Vec<Screen>>,
        refuse_reapply: bool,
        refuse_internal: bool,
        /// What reads return after a refused call: Windows changed the
        /// screens meanwhile. `Some(None)`: they can't be read.
        after_refused: Option<Option<Vec<Screen>>>,
        reapply_calls: Vec<usize>,
        internal_calls: usize,
        slept: Duration,
        logs: Vec<(bool, String)>,
        owner: Option<String>,
        /// The record on disk.
        kept: Option<String>,
    }

    impl Saved for usize {
        fn encode(&self) -> String {
            self.to_string()
        }

        fn decode(text: &str) -> Option<Self> {
            text.parse().ok()
        }
    }

    impl Fake {
        fn reading(reads: impl IntoIterator<Item = Vec<Screen>>) -> Self {
            Self {
                reads: reads.into_iter().map(Some).collect(),
                ..Self::default()
            }
        }

        fn set_display_config_calls(&self) -> usize {
            self.reapply_calls.len() + self.internal_calls
        }

        fn text(&self) -> String {
            let lines: Vec<&str> = self.logs.iter().map(|(_, text)| text.as_str()).collect();
            lines.join("\n")
        }
    }

    impl Displays for Fake {
        type Saved = usize;

        fn read(&mut self) -> Option<Layout<usize>> {
            if let Some(next) = self.reads.pop_front() {
                self.last = next;
            }
            self.read_count += 1;
            self.last.clone().map(|screens| Layout {
                screens,
                saved: self.read_count,
            })
        }

        fn reapply(&mut self, saved: &usize) -> Result<(), i32> {
            self.reapply_calls.push(*saved);
            if self.refuse_reapply {
                if let Some(screens) = self.after_refused.clone() {
                    self.reads.clear();
                    self.last = screens;
                }
                return Err(87);
            }
            if let Some(screens) = self.reapplied.clone() {
                self.reads.clear();
                self.last = Some(screens);
            }
            Ok(())
        }

        fn internal_only(&mut self) -> Result<(), i32> {
            self.internal_calls += 1;
            if self.refuse_internal {
                return Err(31);
            }
            if let Some(screens) = self.internal.clone() {
                self.reads.clear();
                self.last = Some(screens);
            }
            Ok(())
        }

        fn sleep(&mut self, duration: Duration) {
            self.slept += duration;
        }

        fn log(&mut self, warn: bool, text: &str) {
            self.logs.push((warn, text.to_string()));
        }

        fn owner(&self) -> Option<String> {
            self.owner.clone()
        }

        fn keep(&mut self, record: Option<&str>) {
            self.kept = record.map(str::to_string);
        }

        fn kept(&mut self) -> Option<String> {
            self.kept.clone()
        }
    }

    /// A Second screen session: before and after the turn on, then the
    /// turn off, with each read taking the next layout.
    fn session(fake: &mut Fake) -> (Guard<usize>, Outcome) {
        let mut guard = Guard::new();
        guard.before(true, fake);
        guard.after(true, true, fake);
        guard.before(false, fake);
        let outcome = guard.after(false, true, fake);
        (guard, outcome)
    }

    #[test]
    fn the_decision() {
        let restore = |internal| Plan::Restore { internal };
        // Healthy: the screens it had, or some of them, are still on.
        assert_eq!(plan(&[panel()], &[panel()]), Plan::Leave);
        assert_eq!(plan(&[panel(), monitor()], &[panel(), vdd()]), Plan::Leave);
        // An external monitor unplugged while the panel stays.
        assert_eq!(plan(&[panel(), monitor()], &[panel()]), Plan::Leave);
        // A real screen it did not have before still counts.
        assert_eq!(plan(&[panel()], &[monitor()]), Plan::Leave);
        // Every real screen lost; the virtual display left alone counts as lost.
        assert_eq!(plan(&[panel()], &[]), restore(true));
        assert_eq!(plan(&[panel(), monitor()], &[vdd()]), restore(true));
        // Without the panel before, no switch to it.
        assert_eq!(plan(&[monitor()], &[]), restore(false));
        // Nothing to lose: no real screen before, or nothing known.
        assert_eq!(plan(&[vdd()], &[]), Plan::Leave);
        assert_eq!(plan(&[], &[]), Plan::Leave);
    }

    #[test]
    fn a_healthy_pc_sees_no_set_display_config_call() {
        // Panel and monitor stay through the session; the monitor as the
        // turn on extends to the virtual display.
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor()],
        ]);
        let (_, outcome) = session(&mut fake);
        assert_eq!(outcome, Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
        // Nor any wait: one read per log line.
        assert_eq!((fake.read_count, fake.slept), (4, Duration::ZERO));
        // An external monitor unplugged while the panel stays.
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), vdd()],
            vec![panel()],
        ]);
        assert_eq!(session(&mut fake).1, Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
        // A turn on that leaves the virtual display alone is only logged:
        // the layout stays the user's until the turn off.
        let mut fake = Fake::reading([vec![panel()], vec![vdd()]]);
        let mut guard = Guard::new();
        guard.before(true, &mut fake);
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
    }

    #[test]
    fn each_change_logs_the_screens_before_and_after() {
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor()],
        ]);
        session(&mut fake);
        let both = "paths=2 [internal tgt=d2c1/4352 1920x1200@165Hz | \
                    external tgt=d2c1/4353 2560x1440@59.94Hz]";
        assert_eq!(
            fake.text(),
            format!(
                "Screens before the virtual display turns on: {both} vdd=0\n\
                 Screens after the virtual display turned on: {both} vdd=1\n\
                 Screens before the virtual display turns off: {both} vdd=1\n\
                 Screens after the virtual display turned off: {both} vdd=0"
            )
        );
        assert!(fake.logs.iter().all(|(warn, _)| !warn));
        assert_eq!(line(None), "paths=unknown");
        assert_eq!(line(Some(&[])), "paths=0 [] vdd=0");
        let unknown_mode = Screen {
            mode: None,
            ..monitor()
        };
        assert_eq!(
            line(Some(&[unknown_mode])),
            "paths=1 [external tgt=d2c1/4353] vdd=0"
        );
        let many = vec![monitor(); 11];
        assert!(
            line(Some(&many)).ends_with(" | +3 more] vdd=0"),
            "{}",
            line(Some(&many))
        );
    }

    #[test]
    fn every_real_screen_lost_reapplies_the_layout_from_before_the_turn_on() {
        // The desktop moved to the virtual display alone, then nothing.
        let mut fake = Fake::reading([vec![panel(), monitor()], vec![vdd()], vec![vdd()], vec![]]);
        fake.reapplied = Some(vec![panel(), monitor()]);
        let (_, outcome) = session(&mut fake);
        assert_eq!(outcome, Outcome::Restored { internal: false });
        // The layout read before the turn on, not the one before the turn off.
        assert_eq!(fake.reapply_calls, [1]);
        assert_eq!(fake.internal_calls, 0);
        let text = fake.text();
        assert!(
            text.contains(
                "Screens after the virtual display turned off: paths=0 [] vdd=0\n\
                 The PC lost every screen it had before the virtual display turned on; \
                 restoring them. Before: paths=2 [internal"
            ),
            "{text}"
        );
        assert!(
            text.ends_with(
                "Screens restored outcome=restored applied=saved_layout paths=2 \
                 [internal tgt=d2c1/4352 1920x1200@165Hz | external tgt=d2c1/4353 \
                 2560x1440@59.94Hz] vdd=0"
            ),
            "{text}"
        );
        // Windows got its chance to bring one back first, within a bound.
        assert_eq!(fake.slept, SETTLE);
    }

    #[test]
    fn a_screen_windows_brings_back_by_itself_is_left_alone() {
        let mut fake = Fake::reading([vec![panel()], vec![vdd()], vec![vdd()], vec![], vec![]]);
        fake.reads.push_back(Some(vec![panel()]));
        assert_eq!(session(&mut fake).1, Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
        assert_eq!(fake.slept, SETTLE_STEP * 2);
    }

    #[test]
    fn a_refused_layout_falls_back_to_pc_screen_only_when_the_panel_was_on() {
        let mut fake = Fake::reading([vec![panel(), monitor()], vec![vdd()], vec![vdd()], vec![]]);
        fake.refuse_reapply = true;
        fake.internal = Some(vec![panel()]);
        assert_eq!(session(&mut fake).1, Outcome::Restored { internal: true });
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 1));
        assert!(
            fake.text().ends_with(
                "Screens restored outcome=restored applied=internal paths=1 \
                 [internal tgt=d2c1/4352 1920x1200@165Hz] vdd=0"
            ),
            "{}",
            fake.text()
        );
        // A layout Windows took that brought no screen back counts as failed too.
        let mut fake = Fake::reading([vec![panel()], vec![vdd()], vec![vdd()], vec![]]);
        fake.internal = Some(vec![panel()]);
        assert_eq!(session(&mut fake).1, Outcome::Restored { internal: true });
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 1));
    }

    #[test]
    fn without_the_panel_before_there_is_no_switch_to_it() {
        // A closed laptop on an external monitor that went dark.
        let mut fake = Fake::reading([vec![monitor()], vec![vdd()], vec![vdd()], vec![]]);
        fake.refuse_reapply = true;
        assert_eq!(session(&mut fake).1, Outcome::Failed);
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 0));
        let (warn, text) = fake.logs.last().unwrap();
        assert!(*warn);
        assert_eq!(
            text,
            "Could not restore the screens outcome=restore_failed \
             reason=saved_layout_refused code=87 paths=0 [] vdd=0; Windows+P then \
             \"PC screen only\" brings the built-in screen back"
        );
    }

    #[test]
    fn one_attempt_per_change() {
        let mut fake = Fake::reading([vec![panel()], vec![vdd()], vec![vdd()], vec![]]);
        (fake.refuse_reapply, fake.refuse_internal) = (true, true);
        let (mut guard, outcome) = session(&mut fake);
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 1));
        assert!(
            fake.text().contains(
                "outcome=restore_failed reason=saved_layout_refused code=87 \
                 reason=internal_refused code=31 paths=0"
            ),
            "{}",
            fake.text()
        );
        // The next turn off, with nothing turned on since, compares with the
        // screens just before it: none, so nothing more is tried.
        guard.before(false, &mut fake);
        assert_eq!(guard.after(false, true, &mut fake), Outcome::Left);
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 1));
        // Reads stop once the bounded wait ends.
        assert!(fake.read_count < 40, "{}", fake.read_count);
    }

    #[test]
    fn a_turn_off_with_no_turn_on_before_it_compares_with_the_screens_just_before() {
        // At startup or in the failure reporter: the display was left on,
        // the panel with it.
        let mut fake = Fake::reading([vec![panel(), vdd()], vec![]]);
        fake.refuse_reapply = true;
        fake.internal = Some(vec![panel()]);
        let mut guard = Guard::new();
        guard.before(false, &mut fake);
        assert_eq!(
            guard.after(false, true, &mut fake),
            Outcome::Restored { internal: true }
        );
        assert_eq!(fake.reapply_calls, [1]);
    }

    #[test]
    fn unknown_screens_change_nothing() {
        let mut fake = Fake::default();
        fake.reads.push_back(None);
        assert_eq!(session(&mut fake).1, Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
        assert!(
            fake.text().ends_with("turned off: paths=unknown"),
            "{}",
            fake.text()
        );
        // Known before, unknown after.
        let mut fake = Fake::reading([vec![panel()], vec![vdd()], vec![vdd()]]);
        fake.reads.push_back(None);
        assert_eq!(session(&mut fake).1, Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
    }

    #[test]
    fn the_fallback_looks_again_and_leaves_screens_windows_brought_back() {
        // Windows put both screens back while it refused the saved layout.
        let mut fake = Fake::reading([vec![panel(), monitor()], vec![vdd()], vec![vdd()], vec![]]);
        fake.refuse_reapply = true;
        fake.after_refused = Some(Some(vec![panel(), monitor()]));
        fake.internal = Some(vec![panel()]);
        assert_eq!(session(&mut fake).1, Outcome::Recovered);
        // No "PC screen only", which would turn the monitor off.
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 0));
        assert!(
            fake.text().ends_with(
                "Screens came back without a change outcome=restored applied=none paths=2 \
                 [internal tgt=d2c1/4352 1920x1200@165Hz | external tgt=d2c1/4353 \
                 2560x1440@59.94Hz] vdd=0"
            ),
            "{}",
            fake.text()
        );
        // The same after a layout Windows took that showed nothing at first.
        let mut fake = Fake::reading([vec![panel(), monitor()], vec![vdd()], vec![vdd()], vec![]]);
        fake.internal = Some(vec![panel()]);
        let mut guard = Guard::new();
        guard.before(true, &mut fake);
        guard.after(true, true, &mut fake);
        guard.before(false, &mut fake);
        let mut late = LateRecovery(&mut fake, 0);
        assert_eq!(guard.after(false, true, &mut late), Outcome::Recovered);
        assert_eq!(fake.internal_calls, 0);
    }

    /// Reads nothing back through the wait after the reapply, then both
    /// screens: Windows finished the change just after the guard gave up
    /// waiting. `.1` counts the reads at the end of that wait.
    struct LateRecovery<'a>(&'a mut Fake, usize);

    impl Displays for LateRecovery<'_> {
        type Saved = usize;
        fn read(&mut self) -> Option<Layout<usize>> {
            if !self.0.reapply_calls.is_empty() && self.0.slept >= SETTLE * 2 {
                self.1 += 1;
                if self.1 > 1 {
                    self.0.last = Some(vec![panel(), monitor()]);
                }
            }
            self.0.read()
        }
        fn reapply(&mut self, saved: &usize) -> Result<(), i32> {
            self.0.reapply(saved)
        }
        fn internal_only(&mut self) -> Result<(), i32> {
            self.0.internal_only()
        }
        fn sleep(&mut self, duration: Duration) {
            self.0.sleep(duration)
        }
        fn log(&mut self, warn: bool, text: &str) {
            self.0.log(warn, text)
        }
        fn owner(&self) -> Option<String> {
            self.0.owner()
        }
        fn keep(&mut self, record: Option<&str>) {
            self.0.keep(record)
        }
        fn kept(&mut self) -> Option<String> {
            self.0.kept()
        }
    }

    #[test]
    fn the_fallback_stops_when_the_screens_cannot_be_read() {
        let mut fake = Fake::reading([vec![panel()], vec![vdd()], vec![vdd()], vec![]]);
        fake.refuse_reapply = true;
        fake.after_refused = Some(None);
        fake.internal = Some(vec![panel()]);
        assert_eq!(session(&mut fake).1, Outcome::Failed);
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 0));
        assert!(
            fake.text().contains(
                "outcome=restore_failed reason=saved_layout_refused code=87 \
                 reason=screens_unknown paths=unknown"
            ),
            "{}",
            fake.text()
        );
    }

    const HOST: &str = "4242-133456789012345678";

    /// The host turned the display on, which left the virtual display alone,
    /// then crashed. Its failure reporter, a new process with nothing in
    /// memory, turns the display off and the PC has no screen.
    #[test]
    fn the_failure_reporter_restores_the_layout_its_host_kept() {
        let mut host = Fake::reading([vec![panel(), monitor()], vec![vdd()]]);
        host.owner = Some(HOST.into());
        let mut guard = Guard::new();
        guard.before(true, &mut host);
        guard.after(true, true, &mut host);
        let kept = host.kept.clone().expect("the host kept its screens");

        let mut reporter = Fake::reading([vec![vdd()], vec![]]);
        reporter.read_count = 100;
        reporter.owner = Some(HOST.into());
        reporter.kept = Some(kept.clone());
        reporter.reapplied = Some(vec![panel(), monitor()]);
        let mut guard = Guard::new();
        guard.before(false, &mut reporter);
        assert_eq!(
            guard.after(false, true, &mut reporter),
            Outcome::Restored { internal: false }
        );
        // The host's layout, read before its turn on: its first read.
        assert_eq!(reporter.reapply_calls, [1]);
        assert!(
            reporter.text().contains(
                "Comparing with the screens kept from before the virtual display turned on: \
                 paths=2 [internal"
            ),
            "{}",
            reporter.text()
        );
        // Checked after a turn off that worked: deleted.
        assert_eq!(reporter.kept, None);

        // Without the record, the reporter only sees the virtual display before:
        // nothing to compare with, the screen stays black. This is what the
        // record fixes.
        let mut reporter = Fake::reading([vec![vdd()], vec![]]);
        reporter.owner = Some(HOST.into());
        reporter.reapplied = Some(vec![panel()]);
        let mut guard = Guard::new();
        guard.before(false, &mut reporter);
        assert_eq!(guard.after(false, true, &mut reporter), Outcome::Left);
        assert_eq!(reporter.set_display_config_calls(), 0);
    }

    #[test]
    fn a_record_from_another_host_or_one_that_does_not_parse_is_ignored() {
        let mut host = Fake::reading([vec![panel()]]);
        host.owner = Some(HOST.into());
        Guard::new().before(true, &mut host);
        let kept = host.kept.clone().unwrap();
        for (owner, record) in [
            // Another host, boot or session: another id or start time.
            (Some("4242-1"), kept.clone()),
            (Some("17-133456789012345678"), kept.clone()),
            // No start time to tell it apart: never used.
            (None, kept.clone()),
            (Some(HOST), kept.replace("saved 1", "saved x")),
            (
                Some(HOST),
                kept.replace("screen d2c1 4352 1 0", "screen d2c1 4352 2 0"),
            ),
            (Some(HOST), format!("{kept}screen d2c1 1 0 0 - - -\n")),
            (Some(HOST), String::new()),
        ] {
            let mut reporter = Fake::reading([vec![vdd()], vec![]]);
            reporter.owner = owner.map(str::to_string);
            reporter.kept = Some(record.clone());
            reporter.reapplied = Some(vec![panel()]);
            let mut guard = Guard::new();
            guard.before(false, &mut reporter);
            assert_eq!(
                guard.after(false, true, &mut reporter),
                Outcome::Left,
                "{record}"
            );
            assert_eq!(reporter.set_display_config_calls(), 0, "{record}");
            // Another host's record stays, for its own reporter.
            if owner != Some(HOST) {
                assert_eq!(reporter.kept.as_deref(), Some(record.as_str()));
            }
        }
        // A host with no start time keeps nothing.
        let mut host = Fake::reading([vec![panel()]]);
        Guard::new().before(true, &mut host);
        assert_eq!(host.kept, None);
    }

    #[test]
    fn the_record_is_deleted_only_after_a_turn_off_that_worked() {
        let mut fake = Fake::reading([vec![panel()], vec![panel(), vdd()]]);
        fake.owner = Some(HOST.into());
        let mut guard = Guard::new();
        guard.before(true, &mut fake);
        guard.after(true, true, &mut fake);
        // A failed turn off may leave the display on: the record stays.
        guard.before(false, &mut fake);
        guard.after(false, false, &mut fake);
        assert!(fake.kept.is_some());
        // The next turn off, with nothing in memory, still compares with it.
        fake.reads.push_back(Some(vec![panel()]));
        guard.before(false, &mut fake);
        assert!(
            fake.text().contains("Comparing with the screens kept"),
            "{}",
            fake.text()
        );
        guard.after(false, true, &mut fake);
        assert_eq!(fake.kept, None);
    }

    #[test]
    fn the_record_round_trips_and_is_bounded() {
        let layout = Layout {
            screens: vec![
                panel(),
                Screen {
                    mode: None,
                    ..monitor()
                },
                vdd(),
            ],
            saved: 7usize,
        };
        let text = record(HOST, &layout);
        assert_eq!(
            text,
            format!(
                "eternalmonitor-screens 1\nowner {HOST}\nscreen d2c1 4352 1 0 1920 1200 165000\n\
                 screen d2c1 4353 0 0 - - -\nscreen 200000101 256 0 1 2732 2048 60000\nsaved 7\n"
            )
        );
        let back = parse::<usize>(HOST, &text).unwrap();
        assert_eq!((back.screens, back.saved), (layout.screens, 7));
        assert!(parse::<usize>(HOST, &text.replace("eternalmonitor-screens 1", "x")).is_none());
        assert!(parse::<usize>(HOST, &format!("{text}saved 8\n")).is_none());
        assert!(parse::<usize>(HOST, &text.replace("\nsaved 7\n", "\n")).is_none());
        let many = Layout {
            screens: vec![monitor(); MAX_RECORD_SCREENS + 1],
            saved: 1usize,
        };
        assert!(parse::<usize>(HOST, &record(HOST, &many)).is_none());
        let long = Layout {
            screens: vec![],
            saved: usize::MAX,
        };
        let padded =
            record(HOST, &long).replace("saved ", &format!("saved {}", "0".repeat(MAX_RECORD)));
        assert!(parse::<usize>(HOST, &padded).is_none());
        // The saved bytes go as hex.
        assert_eq!(to_hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(from_hex("00ab10"), Some(vec![0, 0xab, 0x10]));
        for bad in ["0", "zz", "é0"] {
            assert_eq!(from_hex(bad), None, "{bad}");
        }
    }
}
