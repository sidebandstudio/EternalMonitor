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
    type Saved;
    /// The active paths now. `None` when Windows can't say.
    fn read(&mut self) -> Option<Layout<Self::Saved>>;
    /// `SetDisplayConfig` with the saved paths and modes. The error is its code.
    fn reapply(&mut self, saved: &Self::Saved) -> Result<(), i32>;
    /// `SetDisplayConfig` with "PC screen only".
    fn internal_only(&mut self) -> Result<(), i32>;
    fn sleep(&mut self, duration: Duration);
    fn log(&mut self, warn: bool, text: &str);
}

/// How a turn off ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do, or nothing known to compare with.
    Left,
    Restored {
        internal: bool,
    },
    Failed,
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

    /// Before a driver change: log the screens. A turn on keeps them to
    /// compare with; a turn off keeps them only when no turn on did, as at
    /// startup or after a crash.
    pub fn before(&mut self, enabled: bool, displays: &mut impl Displays<Saved = S>) {
        let layout = displays.read();
        let text = format!(
            "Screens before the virtual display turns {}: {}",
            if enabled { "on" } else { "off" },
            line(layout.as_ref().map(|layout| &layout.screens[..]))
        );
        displays.log(false, &text);
        if enabled || self.reference.is_none() {
            self.reference = layout;
        }
    }

    /// After a driver change: log the screens, and after a turn off,
    /// restore them if every real screen is gone.
    pub fn after(&mut self, enabled: bool, displays: &mut impl Displays<Saved = S>) -> Outcome {
        if enabled {
            let layout = displays.read();
            let text = format!(
                "Screens after the virtual display turned on: {}",
                line(layout.as_ref().map(|layout| &layout.screens[..]))
            );
            displays.log(false, &text);
            return Outcome::Left;
        }
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
        let mut restored = Some(after);
        match displays.reapply(&reference.saved) {
            Ok(()) => {
                restored = settle(displays, &reference.screens);
                if !has_real(&restored) {
                    tried.push("reason=saved_layout_no_screen".to_string());
                }
            }
            Err(code) => tried.push(format!("reason=saved_layout_refused code={code}")),
        }
        let mut applied = "saved_layout";
        if !has_real(&restored) && internal {
            applied = "internal";
            match displays.internal_only() {
                Ok(()) => {
                    restored = settle(displays, &reference.screens);
                    if !has_real(&restored) {
                        tried.push("reason=internal_no_screen".to_string());
                    }
                }
                Err(code) => tried.push(format!("reason=internal_refused code={code}")),
            }
        }
        let now = line(restored.as_ref().map(|layout| &layout.screens[..]));
        if has_real(&restored) {
            displays.log(
                false,
                &format!("Screens restored outcome=restored applied={applied} {now}"),
            );
            Outcome::Restored {
                internal: applied == "internal",
            }
        } else {
            let tried = if tried.is_empty() {
                String::new()
            } else {
                format!("{} ", tried.join(" "))
            };
            displays.log(
                true,
                &format!(
                    "Could not restore the screens outcome=restore_failed {tried}{now}; \
                     Windows+P then \"PC screen only\" brings the built-in screen back"
                ),
            );
            Outcome::Failed
        }
    }
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

/// Run a driver change with the guard around it. `log` writes one line.
#[cfg(windows)]
pub fn around<T>(enabled: bool, log: fn(bool, &str), change: impl FnOnce() -> T) -> T {
    let mut displays = windows_impl::Windows { log };
    GUARD.lock().before(enabled, &mut displays);
    let result = change();
    GUARD.lock().after(enabled, &mut displays);
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

    pub struct Windows {
        pub log: fn(bool, &str),
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
        reapply_calls: Vec<usize>,
        internal_calls: usize,
        slept: Duration,
        logs: Vec<(bool, String)>,
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
    }

    /// A Second screen session: before and after the turn on, then the
    /// turn off, with each read taking the next layout.
    fn session(fake: &mut Fake) -> (Guard<usize>, Outcome) {
        let mut guard = Guard::new();
        guard.before(true, fake);
        guard.after(true, fake);
        guard.before(false, fake);
        let outcome = guard.after(false, fake);
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
        assert_eq!(guard.after(true, &mut fake), Outcome::Left);
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
        assert_eq!(guard.after(false, &mut fake), Outcome::Left);
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
            guard.after(false, &mut fake),
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
}
