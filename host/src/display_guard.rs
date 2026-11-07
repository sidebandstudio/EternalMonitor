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
//! keeps any real screen sees no change at all.
//!
//! Turning the display off while it is the only screen on is what loses the
//! panel for good: on a hybrid laptop, Windows then stops reporting any
//! screen, and refuses every layout until the PC restarts. So the guard acts
//! while the display is still on. A turn on that leaves the virtual display
//! alone, where the PC had a real screen before, switches Windows to
//! "Extend", which keeps the second screen, or else puts the saved layout
//! back, or "PC screen only". A turn off that finds the virtual display
//! alone first puts the saved layout back, "PC screen only", or "Extend",
//! and if none brings a real screen back it leaves the display on: the
//! desktop stays on it, which the iPad and Windows+P can still reach.
//! One attempt per change. Between two calls the guard reads the screens
//! again, and stops if one came back or they can't be read.
//!
//! On a hybrid laptop (AMD integrated and discrete graphics), the virtual
//! display's arrival also takes the built-in panel away below the layout:
//! Windows lists the panel's target as unavailable, every layout that names
//! it is refused, and only a restart gives it back. The guard tells that
//! case apart, on a PC with more than one graphics card, by the panel's
//! target: the virtual display is on, the panel is not, and no built-in
//! target is offered any more. It then makes no `SetDisplayConfig` call and
//! never turns the display off: the desktop stays on it, which the iPad
//! still shows. Only when Windows ends the session does the turn off run,
//! so the restart that brings the panel back starts without the display.
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
    /// `SetDisplayConfig` with "Extend".
    fn extend(&mut self) -> Result<(), i32>;
    /// What Windows offers of the built-in panel's targets now, active or
    /// not. `None` when it can't say.
    fn built_in(&mut self) -> Option<BuiltIn>;
    /// The PC has more than one graphics card.
    fn hybrid(&self) -> bool;
    /// Windows is ending the session: shutting down, restarting or signing out.
    fn session_ending(&self) -> bool;
    fn sleep(&mut self, duration: Duration);
    fn log(&mut self, warn: bool, text: &str);
    /// The host whose layout from before its turn on is kept on disk: this
    /// one, or in the failure reporter the one that ended. `None` when a
    /// later process with the same id could not be told from it.
    fn owner(&self) -> Option<String>;
    /// Replace the kept record.
    fn keep(&mut self, record: &str);
    /// Delete the kept record if it names [`Displays::owner`], read and
    /// deleted in one step: a record another host put in its place stays.
    fn forget(&mut self);
    /// The kept record, if there is one.
    fn kept(&mut self) -> Option<String>;
}

/// A layout's saved paths and modes as one line of text, for the kept record.
pub trait Saved: Sized {
    fn encode(&self) -> String;
    fn decode(text: &str) -> Option<Self>;
}

/// The built-in panel's targets, as `QDC_ALL_PATHS` lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltIn {
    /// None listed: a desktop PC.
    Missing,
    /// At least one is available for a path.
    Offered,
    /// Listed, and none available: a closed lid, or the hybrid laptop's
    /// panel after the virtual display took it.
    Withdrawn,
}

/// How a change ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do, or nothing known to compare with.
    Left,
    /// A turn on left the virtual display alone, and "Extend" put the PC's
    /// screens back beside it.
    Extended,
    /// The PC's screens are back, without the virtual display.
    Restored {
        internal: bool,
    },
    /// Windows brought a real screen back while the guard was restoring, so
    /// it changed nothing more.
    Recovered,
    Failed,
    /// A turn on took the built-in panel away for good: see [`lost_panel`].
    /// Nothing was tried, and the display stays on.
    PanelLost,
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
pub fn names(owner: &str, text: &str) -> bool {
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
    /// The last turn on left the virtual display alone, and "Extend" did
    /// not put the PC's screens beside it: it will not attach.
    alone: bool,
    /// The last turn on took the built-in panel away for good.
    panel_lost: bool,
}

impl<S> Default for Guard<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Guard<S> {
    pub const fn new() -> Self {
        Self {
            reference: None,
            alone: false,
            panel_lost: false,
        }
    }

    /// Whether the last turn on left the virtual display alone, with no
    /// real screen beside it.
    pub fn alone(&self) -> bool {
        self.alone
    }

    /// Whether the last turn on took the built-in panel away until the PC
    /// restarts.
    pub fn panel_lost(&self) -> bool {
        self.panel_lost
    }
}

/// A `SetDisplayConfig` call that can bring a real screen back while the
/// virtual display is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// "Extend": the PC's screens beside the virtual display.
    Extend,
    /// The layout from before the turn on, without the virtual display.
    SavedLayout,
    /// "PC screen only".
    Internal,
}

impl Step {
    const fn name(self) -> &'static str {
        match self {
            Self::Extend => "extend",
            Self::SavedLayout => "saved_layout",
            Self::Internal => "internal",
        }
    }
}

/// How [`take_back`] ended.
enum TakeBack<S> {
    /// This call brought a real screen back.
    Applied(Step, Option<Layout<S>>),
    /// A real screen came back between two calls.
    CameBack(Option<Layout<S>>),
    /// None did: each call's result, and the screens now.
    Failed(Vec<String>, Option<Layout<S>>),
}

impl<S: Saved> Guard<S> {
    /// Before a driver change: log the screens. A turn on keeps them to
    /// compare with, also on disk for the failure reporter. A turn off
    /// with nothing kept, as at startup or in the reporter, compares with
    /// the record its host kept, else with the screens just before it.
    /// `display_off`: the driver tasks know the display is off. False when
    /// the turn off must not run: the virtual display is the only screen
    /// on, the PC had a real one, and nothing brought it back.
    pub fn before(
        &mut self,
        enabled: bool,
        display_off: bool,
        displays: &mut impl Displays<Saved = S>,
    ) -> bool {
        let layout = displays.read();
        let text = format!(
            "Screens before the virtual display turns {}: {}",
            if enabled { "on" } else { "off" },
            line(layout.as_ref().map(|layout| &layout.screens[..]))
        );
        displays.log(false, &text);
        let alone = layout.as_ref().is_some_and(|layout| alone(&layout.screens));
        let screens = layout.as_ref().map(|layout| layout.screens.clone());
        if enabled {
            // A turn on can run while the display is still on: after an
            // Enable that finished once the host had given up on it, or a
            // failed Disable. The screens then may be the virtual display's
            // alone, so they never replace a kept layout with a real
            // screen, unless the display is known to be off.
            let earlier = self.reference.take().or_else(|| own_record(displays));
            let layout = if !display_off && !has_real(&layout) && has_real(&earlier) {
                let text = format!(
                    "Keeping the screens from before the virtual display first turned on: {}",
                    line(earlier.as_ref().map(|layout| &layout.screens[..]))
                );
                displays.log(false, &text);
                earlier
            } else {
                layout
            };
            match (displays.owner(), &layout) {
                (Some(owner), Some(layout)) => displays.keep(&record(&owner, layout)),
                (Some(_), None) => displays.forget(),
                (None, _) => {}
            }
            self.reference = layout;
        } else if self.reference.is_none() {
            let kept = own_record(displays);
            if let Some(kept) = &kept {
                let text = format!(
                    "Comparing with the screens kept from before the virtual display turned on: {}",
                    line(Some(&kept.screens))
                );
                displays.log(false, &text);
            }
            self.reference = kept.or(layout);
        }
        if let (false, true, Some(screens)) = (enabled, alone, screens) {
            return self.take_back_before_off(displays, &screens);
        }
        true
    }

    /// The virtual display is the only screen on, and is about to turn off:
    /// put a real screen back first. False when none came back and the PC
    /// had one before, so the turn off must not run, nor while the panel is
    /// lost. Windows ending the session lets it run: the desktop goes, and
    /// the next start then begins without the display. `screens`: now.
    fn take_back_before_off(
        &mut self,
        displays: &mut impl Displays<Saved = S>,
        screens: &[Screen],
    ) -> bool {
        if displays.session_ending() {
            displays.log(
                false,
                "Windows is ending the session — turning off the virtual display, the only \
                 screen on, so the next start begins without it outcome=session_end",
            );
            return true;
        }
        if lost_panel(displays, screens) {
            let text = format!(
                "Leaving the virtual display on: the built-in screen is still unavailable, and \
                 turning the display off would leave the PC with no screen outcome=kept_on \
                 reason=panel_lost {}; restarting the PC brings the built-in screen back",
                line(Some(screens))
            );
            displays.log(true, &text);
            return false;
        }
        let reference = self.reference.as_ref();
        let had_real = reference.is_some_and(|layout| real(&layout.screens).next().is_some());
        let internal = reference.is_some_and(|layout| real(&layout.screens).any(|s| s.internal));
        displays.log(
            true,
            &format!(
                "The virtual display is the only screen on; bringing the PC's screens back \
                 before turning it off. Before: {}",
                line(reference.map(|layout| &layout.screens[..]))
            ),
        );
        let mut steps = Vec::new();
        if had_real {
            steps.push(Step::SavedLayout);
        }
        if internal {
            steps.push(Step::Internal);
        }
        steps.push(Step::Extend);
        match take_back(displays, &steps, reference) {
            TakeBack::Applied(step, now) => {
                restored(displays, step.name(), &now);
                true
            }
            TakeBack::CameBack(now) => {
                came_back(displays, &now);
                true
            }
            TakeBack::Failed(tried, now) if had_real => {
                let text = format!(
                    "Leaving the virtual display on: turning it off now would leave the PC with \
                     no screen outcome=kept_on {}{}; Windows+P then \"PC screen only\" brings \
                     the built-in screen back",
                    tried
                        .iter()
                        .map(|step| format!("{step} "))
                        .collect::<String>(),
                    line(now.as_ref().map(|layout| &layout.screens[..]))
                );
                displays.log(true, &text);
                false
            }
            TakeBack::Failed(tried, now) => {
                failed(displays, &tried, &now);
                true
            }
        }
    }

    /// After a driver change: log the screens. After a turn on that left
    /// the virtual display alone, put the PC's screens back; after a turn
    /// off, restore them if every real screen is gone. A turn off that
    /// `done` deletes the kept record once checked.
    pub fn after(
        &mut self,
        enabled: bool,
        done: bool,
        displays: &mut impl Displays<Saved = S>,
    ) -> Outcome {
        if enabled {
            let outcome = self.check_turn_on(displays);
            self.alone = matches!(
                outcome,
                Outcome::Restored { .. } | Outcome::Failed | Outcome::PanelLost
            );
            self.panel_lost = outcome == Outcome::PanelLost;
            return outcome;
        }
        let outcome = self.check(displays);
        if done {
            displays.forget();
        }
        outcome
    }

    /// Windows may have put the desktop on the virtual display alone. A
    /// turn off then would lose the PC's screens, so bring them back now,
    /// while Windows still reports them: beside it with "Extend", which
    /// keeps the second screen, else without it.
    fn check_turn_on(&mut self, displays: &mut impl Displays<Saved = S>) -> Outcome {
        let reference = self.reference.as_ref();
        let before = reference.map_or(&[][..], |layout| &layout.screens[..]);
        let layout = settle(displays, before);
        let text = format!(
            "Screens after the virtual display turned on: {}",
            line(layout.as_ref().map(|layout| &layout.screens[..]))
        );
        displays.log(false, &text);
        let (Some(reference), Some(after)) = (reference, layout) else {
            return Outcome::Left;
        };
        // No layout can name a target Windows withdrew, so none is tried.
        if real(&reference.screens).any(|screen| screen.internal)
            && lost_panel(displays, &after.screens)
        {
            let text = format!(
                "The built-in screen went dark when the virtual display turned on: Windows \
                 reports it unavailable, and only a restart brings it back. Leaving the virtual \
                 display on, so the desktop stays on it outcome=panel_lost {}. Before: {}",
                line(Some(&after.screens)),
                line(Some(&reference.screens))
            );
            displays.log(true, &text);
            return Outcome::PanelLost;
        }
        let Plan::Restore { internal } = plan(&reference.screens, &after.screens) else {
            return Outcome::Left;
        };
        displays.log(
            true,
            &format!(
                "Windows put the desktop on the virtual display alone; bringing the PC's \
                 screens back while it is on. Before: {}",
                line(Some(&reference.screens))
            ),
        );
        let mut steps = vec![Step::Extend, Step::SavedLayout];
        if internal {
            steps.push(Step::Internal);
        }
        match take_back(displays, &steps, Some(reference)) {
            TakeBack::Applied(step, now)
                if now.as_ref().is_some_and(|now| has_vdd(&now.screens)) =>
            {
                let text = format!(
                    "Screens back beside the virtual display outcome=extended applied={} {}",
                    step.name(),
                    line(now.as_ref().map(|layout| &layout.screens[..]))
                );
                displays.log(false, &text);
                Outcome::Extended
            }
            TakeBack::Applied(step, now) => restored(displays, step.name(), &now),
            TakeBack::CameBack(now) => came_back(displays, &now),
            TakeBack::Failed(tried, now) => failed(displays, &tried, &now),
        }
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
        let mut steps = vec![Step::SavedLayout];
        // A switch to "PC screen only" would turn off an external monitor
        // the user had on alone.
        if internal {
            steps.push(Step::Internal);
        }
        match take_back(displays, &steps, Some(&reference)) {
            TakeBack::Applied(step, now) => restored(displays, step.name(), &now),
            TakeBack::CameBack(now) => came_back(displays, &now),
            TakeBack::Failed(tried, now) => failed(displays, &tried, &now),
        }
    }
}

/// Make each call of `steps` in turn until a real screen is on, waiting up
/// to [`SETTLE`] after each one Windows takes. `reference` is the layout
/// from before the turn on, for [`Step::SavedLayout`].
fn take_back<D: Displays>(
    displays: &mut D,
    steps: &[Step],
    reference: Option<&Layout<D::Saved>>,
) -> TakeBack<D::Saved> {
    // Each call's result, for the log when nothing worked.
    let mut tried = Vec::new();
    let mut now = None;
    for &step in steps {
        let result = match (step, reference) {
            (Step::Extend, _) => displays.extend(),
            (Step::SavedLayout, Some(reference)) => displays.reapply(&reference.saved),
            (Step::SavedLayout, None) => continue,
            (Step::Internal, _) => displays.internal_only(),
        };
        match result {
            Ok(()) => {
                now = wait_for_real(displays);
                if has_real(&now) {
                    return TakeBack::Applied(step, now);
                }
                tried.push(format!("reason={}_no_screen", step.name()));
            }
            Err(code) => tried.push(format!("reason={}_refused code={code}", step.name())),
        }
        // Windows may have changed the screens during that call. Look again
        // right before changing anything more: a screen that came back
        // stays as Windows set it.
        now = displays.read();
        if has_real(&now) {
            return TakeBack::CameBack(now);
        }
        if now.is_none() {
            tried.push("reason=screens_unknown".into());
            break;
        }
    }
    TakeBack::Failed(tried, now)
}

/// The layout in the record this host kept, if there is one.
fn own_record<D: Displays>(displays: &mut D) -> Option<Layout<D::Saved>> {
    let owner = displays.owner()?;
    parse(&owner, &displays.kept()?)
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

fn came_back<D: Displays>(displays: &mut D, now: &Option<Layout<D::Saved>>) -> Outcome {
    let text = format!(
        "Screens came back without a change outcome=restored applied=none {}",
        line(now.as_ref().map(|layout| &layout.screens[..]))
    );
    displays.log(false, &text);
    Outcome::Recovered
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

fn has_vdd(screens: &[Screen]) -> bool {
    screens.iter().any(|screen| screen.virtual_display)
}

/// The virtual display is on, and no real screen is.
fn alone(screens: &[Screen]) -> bool {
    has_vdd(screens) && real(screens).next().is_none()
}

/// The hybrid laptop's black panel: on a PC with more than one graphics
/// card, the virtual display is on in `screens`, the built-in panel is off,
/// and Windows no longer offers any built-in target. After a turn on this
/// is read once the screens settle ([`SETTLE`] after the Enable, while no
/// real screen came back). A closed lid reads the same; holding the display
/// on then costs nothing.
fn lost_panel<D: Displays>(displays: &mut D, screens: &[Screen]) -> bool {
    has_vdd(screens)
        && !screens.iter().any(|screen| screen.internal)
        && displays.hybrid()
        && displays.built_in() == Some(BuiltIn::Withdrawn)
}

/// The screens once a real one is back, or as they are after [`SETTLE`]. A
/// PC that has one returns at once; only one that lost them all waits.
fn settle<D: Displays>(displays: &mut D, before: &[Screen]) -> Option<Layout<D::Saved>> {
    if real(before).next().is_none() {
        return displays.read();
    }
    wait_for_real(displays)
}

/// The screens once a real one is on, or as they are after [`SETTLE`], or
/// once they can't be read.
fn wait_for_real<D: Displays>(displays: &mut D) -> Option<Layout<D::Saved>> {
    let mut waited = Duration::ZERO;
    loop {
        let layout = displays.read();
        let lost = layout
            .as_ref()
            .is_some_and(|layout| real(&layout.screens).next().is_none());
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
/// `log` writes one line; `display_off` says the driver tasks know the
/// display is off; `done` tells a change that succeeded. A turn off that
/// would leave the PC with no screen does not run: `kept_on` is its result.
#[cfg(windows)]
pub(crate) fn around<T>(
    enabled: bool,
    owner: crate::startup::ProcessId,
    log: fn(bool, &str),
    display_off: bool,
    change: impl FnOnce() -> T,
    done: impl FnOnce(&T) -> bool,
    kept_on: impl FnOnce() -> T,
) -> T {
    let mut displays = windows_impl::Windows::new(owner, log);
    if !GUARD.lock().before(enabled, display_off, &mut displays) {
        return kept_on();
    }
    let result = change();
    let done = done(&result);
    GUARD.lock().after(enabled, done, &mut displays);
    result
}

/// Whether the last turn on left the virtual display alone, with no real
/// screen beside it: then it does not attach as a second screen.
#[cfg(windows)]
pub(crate) fn last_turn_on_alone() -> bool {
    GUARD.lock().alone()
}

/// Whether the last turn on took the built-in panel away until the PC
/// restarts.
#[cfg(windows)]
pub(crate) fn last_turn_on_lost_panel() -> bool {
    GUARD.lock().panel_lost()
}

#[cfg(windows)]
mod windows_impl {
    use super::{BuiltIn, Layout, Screen};
    use std::time::Duration;
    use windows::Win32::Devices::Display::{
        SetDisplayConfig, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS,
        SDC_ALLOW_CHANGES, SDC_APPLY, SDC_TOPOLOGY_EXTEND, SDC_TOPOLOGY_INTERNAL,
        SDC_USE_SUPPLIED_DISPLAY_CONFIG, SDC_VALIDATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};

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

    /// A laptop's built-in panel.
    fn is_built_in(technology: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> bool {
        [
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        ]
        .contains(&technology)
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
            internal: is_built_in(technology),
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

        fn extend(&mut self) -> Result<(), i32> {
            match unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_TOPOLOGY_EXTEND) } {
                0 => Ok(()),
                code => Err(code),
            }
        }

        fn built_in(&mut self) -> Option<BuiltIn> {
            let (paths, _) = crate::vdd_snapshot::display_config(QDC_ALL_PATHS)?;
            let mut panels = paths
                .iter()
                .filter(|path| is_built_in(path.targetInfo.outputTechnology))
                .peekable();
            Some(if panels.peek().is_none() {
                BuiltIn::Missing
            } else if panels.any(|path| path.targetInfo.targetAvailable.as_bool()) {
                BuiltIn::Offered
            } else {
                BuiltIn::Withdrawn
            })
        }

        fn hybrid(&self) -> bool {
            crate::diagnostics::host_facts().hybrid == Some(true)
        }

        fn session_ending(&self) -> bool {
            unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
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

        // As the virtual display's owner record: replaced in one step, so
        // the reporter never reads half of it, and deleted only through the
        // handle that read it.
        fn keep(&mut self, record: &str) {
            let Some(file) = &self.file else { return };
            let writer = crate::startup::ProcessId::current();
            if let Err(error) = crate::startup::display::replace(file, writer, record) {
                (self.log)(
                    true,
                    &format!("Could not keep the screens for the failure reporter error={error}"),
                );
            }
        }

        fn forget(&mut self) {
            let (Some(file), Some(owner)) = (&self.file, &self.owner) else {
                return;
            };
            let owns = |text: &str| super::names(owner, text);
            if let Err(error) = crate::startup::display::delete_if_its(file, owns) {
                (self.log)(
                    true,
                    &format!(
                        "Could not delete the screens kept for the failure reporter error={error}"
                    ),
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
        extended: Option<Vec<Screen>>,
        refuse_reapply: bool,
        refuse_internal: bool,
        refuse_extend: bool,
        /// What reads return after a refused call: Windows changed the
        /// screens meanwhile. `Some(None)`: they can't be read.
        after_refused: Option<Option<Vec<Screen>>>,
        reapply_calls: Vec<usize>,
        internal_calls: usize,
        extend_calls: usize,
        /// What `QDC_ALL_PATHS` says of the panel; `None` (unread) by default.
        built_in: Option<BuiltIn>,
        hybrid: bool,
        session_ending: bool,
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
            self.reapply_calls.len() + self.internal_calls + self.extend_calls
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

        fn extend(&mut self) -> Result<(), i32> {
            self.extend_calls += 1;
            if self.refuse_extend {
                return Err(1610);
            }
            if let Some(screens) = self.extended.clone() {
                self.reads.clear();
                self.last = Some(screens);
            }
            Ok(())
        }

        fn built_in(&mut self) -> Option<BuiltIn> {
            self.built_in
        }

        fn hybrid(&self) -> bool {
            self.hybrid
        }

        fn session_ending(&self) -> bool {
            self.session_ending
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

        fn keep(&mut self, record: &str) {
            self.kept = Some(record.to_string());
        }

        fn forget(&mut self) {
            let owner = self.owner.clone();
            if owner
                .is_some_and(|owner| self.kept.as_deref().is_some_and(|text| names(&owner, text)))
            {
                self.kept = None;
            }
        }

        fn kept(&mut self) -> Option<String> {
            self.kept.clone()
        }
    }

    /// A Second screen session: before and after the turn on, then the
    /// turn off, with each read taking the next layout.
    fn session(fake: &mut Fake) -> (Guard<usize>, Outcome) {
        let mut guard = Guard::new();
        guard.before(true, false, fake);
        guard.after(true, true, fake);
        guard.before(false, false, fake);
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

    /// The hybrid laptop where the panel went black: its panel, on the
    /// integrated card, was the only screen before the turn on.
    fn laptop_panel() -> Screen {
        Screen {
            adapter: 0x1298b,
            target: 512,
            internal: true,
            virtual_display: false,
            mode: Some((1920, 1200, 165_000)),
        }
    }

    /// After its turn on, Windows showed the desktop on the virtual display
    /// alone, at 800x600.
    fn lone_vdd() -> Screen {
        Screen {
            mode: Some((800, 600, 60_000)),
            ..vdd()
        }
    }

    /// The guard's check before a turn off: whether it may run.
    fn turn_off_allowed(guard: &mut Guard<usize>, fake: &mut Fake) -> bool {
        guard.before(false, false, fake)
    }

    /// The laptop's log: `paths=1 [internal …] vdd=0` before the turn on,
    /// `paths=0 [] vdd=1` after it. The 0.3.4 guard only logged that, the
    /// attempt timed out, and its turn off left Windows reporting no screen
    /// at all (`paths=unknown`), which nothing but a restart repaired. Now
    /// "Extend" puts the panel back beside the virtual display at once,
    /// and the second screen can still attach.
    #[test]
    fn a_turn_on_that_leaves_the_virtual_display_alone_extends_at_once() {
        let mut fake = Fake::reading([vec![laptop_panel()], vec![lone_vdd()]]);
        fake.extended = Some(vec![laptop_panel(), lone_vdd()]);
        let mut guard = Guard::new();
        assert!(guard.before(true, false, &mut fake));
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Extended);
        assert_eq!(
            (
                fake.extend_calls,
                fake.reapply_calls.len(),
                fake.internal_calls
            ),
            (1, 0, 0)
        );
        assert!(!guard.alone());
        let text = fake.text();
        assert!(
            text.contains(
                "Screens after the virtual display turned on: paths=0 [] vdd=1\n\
                 Windows put the desktop on the virtual display alone; bringing the PC's \
                 screens back while it is on. Before: paths=1 [internal tgt=1298b/512 \
                 1920x1200@165Hz] vdd=0\n\
                 Screens back beside the virtual display outcome=extended applied=extend \
                 paths=1 [internal tgt=1298b/512 1920x1200@165Hz] vdd=1"
            ),
            "{text}"
        );
        // The session's turn off then finds the panel on, as on any PC.
        assert!(turn_off_allowed(&mut guard, &mut fake));
        fake.reads.push_back(Some(vec![laptop_panel()]));
        assert_eq!(guard.after(false, true, &mut fake), Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 1);
    }

    /// Where "Extend" does not bring the panel back, the layout from before
    /// the turn on does, without the virtual display: the start falls back
    /// to the mirror, and its turn off finds the panel on.
    #[test]
    fn a_lone_virtual_display_that_extend_cannot_fix_gives_the_panel_back() {
        let mut fake = Fake::reading([vec![laptop_panel()], vec![lone_vdd()]]);
        fake.refuse_extend = true;
        fake.reapplied = Some(vec![laptop_panel()]);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        assert_eq!(
            guard.after(true, true, &mut fake),
            Outcome::Restored { internal: false }
        );
        assert_eq!((fake.extend_calls, &fake.reapply_calls[..]), (1, &[1][..]));
        assert!(guard.alone(), "the display will not attach");
        assert!(
            fake.text().ends_with(
                "Screens restored outcome=restored applied=saved_layout paths=1 \
                 [internal tgt=1298b/512 1920x1200@165Hz] vdd=0"
            ),
            "{}",
            fake.text()
        );
        assert!(turn_off_allowed(&mut guard, &mut fake));
        // Even a turn off after which Windows reports nothing, as on the
        // laptop, changes nothing more: the panel was on before it.
        fake.reads.push_back(None);
        assert_eq!(guard.after(false, true, &mut fake), Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 2);

        // Nothing works: the display stays alone and will not attach.
        let mut fake = Fake::reading([vec![laptop_panel()], vec![lone_vdd()]]);
        (fake.refuse_extend, fake.refuse_reapply) = (true, true);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Failed);
        assert_eq!((fake.extend_calls, fake.reapply_calls.len()), (1, 1));
        // "PC screen only" too: the panel is the built-in one.
        assert_eq!(fake.internal_calls, 1);
        assert!(guard.alone());
        // The next turn on that extends clears it.
        let mut fake = Fake::reading([vec![laptop_panel()], vec![laptop_panel(), lone_vdd()]]);
        guard.before(true, false, &mut fake);
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Left);
        assert!(!guard.alone());
    }

    /// The virtual display ended up alone during the session, Windows+P
    /// "Second screen only" say. Its turn off would leave Windows with no
    /// screen to report, so the guard puts the panel back first.
    #[test]
    fn a_turn_off_with_the_virtual_display_alone_puts_a_real_screen_back_first() {
        let mut fake = Fake::reading([
            vec![laptop_panel()],
            vec![laptop_panel(), lone_vdd()],
            vec![lone_vdd()],
        ]);
        fake.reapplied = Some(vec![laptop_panel()]);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Left);
        assert!(turn_off_allowed(&mut guard, &mut fake));
        // The layout from before the turn on, before the turn off runs.
        assert_eq!(fake.reapply_calls, [1]);
        let text = fake.text();
        assert!(
            text.ends_with(
                "Screens before the virtual display turns off: paths=0 [] vdd=1\n\
                 The virtual display is the only screen on; bringing the PC's screens back \
                 before turning it off. Before: paths=1 [internal tgt=1298b/512 \
                 1920x1200@165Hz] vdd=0\n\
                 Screens restored outcome=restored applied=saved_layout paths=1 \
                 [internal tgt=1298b/512 1920x1200@165Hz] vdd=0"
            ),
            "{text}"
        );
        assert_eq!(guard.after(false, true, &mut fake), Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 1);
    }

    /// When nothing brings a real screen back, the turn off does not run:
    /// the desktop stays on the virtual display, where the iPad and
    /// Windows+P still reach it, rather than on no screen at all.
    #[test]
    fn the_virtual_display_is_never_turned_off_while_it_is_the_only_screen() {
        let mut fake = Fake::reading([
            vec![laptop_panel()],
            vec![laptop_panel(), lone_vdd()],
            vec![lone_vdd()],
        ]);
        fake.owner = Some(HOST.into());
        (fake.refuse_reapply, fake.refuse_extend) = (true, true);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        guard.after(true, true, &mut fake);
        assert!(!turn_off_allowed(&mut guard, &mut fake));
        // Every call, once: the layout, "PC screen only", "Extend".
        assert_eq!(
            (
                fake.reapply_calls.len(),
                fake.internal_calls,
                fake.extend_calls
            ),
            (1, 1, 1)
        );
        let (warn, text) = fake.logs.last().unwrap();
        assert!(*warn);
        assert_eq!(
            text,
            "Leaving the virtual display on: turning it off now would leave the PC with no \
             screen outcome=kept_on reason=saved_layout_refused code=87 \
             reason=internal_no_screen reason=extend_refused code=1610 paths=0 [] vdd=1; \
             Windows+P then \"PC screen only\" brings the built-in screen back"
        );
        // The layout stays kept, in memory and for the failure reporter.
        assert!(fake.kept.is_some());
        // The next turn off, the quit's say, tries again with it.
        fake.refuse_reapply = false;
        fake.reapplied = Some(vec![laptop_panel()]);
        assert!(turn_off_allowed(&mut guard, &mut fake));
        assert_eq!(fake.reapply_calls, [1, 1]);
        guard.after(false, true, &mut fake);
        assert_eq!(fake.kept, None);

        // A PC that had no real screen before turns it off as it did.
        let mut fake = Fake::reading([vec![], vec![lone_vdd()], vec![lone_vdd()]]);
        fake.refuse_extend = true;
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Left);
        assert!(turn_off_allowed(&mut guard, &mut fake));
        assert_eq!(
            (
                fake.reapply_calls.len(),
                fake.internal_calls,
                fake.extend_calls
            ),
            (0, 0, 1)
        );
    }

    /// The hybrid laptop as `QDC_ALL_PATHS` showed it on the second run:
    /// after the turn on, the panel's target was listed for every source
    /// but `available=False`, so "Extend" was refused (31), the saved layout
    /// was refused (1610) and "PC screen only" brought nothing back. Each
    /// later turn off was held back after the same three calls.
    fn hybrid_laptop() -> Fake {
        let mut fake = Fake::reading([vec![laptop_panel()], vec![lone_vdd()]]);
        fake.owner = Some(HOST.into());
        (fake.hybrid, fake.built_in) = (true, Some(BuiltIn::Withdrawn));
        (fake.refuse_extend, fake.refuse_reapply) = (true, true);
        fake
    }

    /// Its signature, the panel's target withdrawn while the virtual
    /// display is on, ends the turn on with no `SetDisplayConfig` call, and
    /// keeps the display on: the desktop stays on it, which the iPad shows.
    #[test]
    fn a_turn_on_that_takes_the_hybrid_laptops_panel_away_changes_nothing() {
        let mut fake = hybrid_laptop();
        let mut guard = Guard::new();
        assert!(guard.before(true, false, &mut fake));
        assert_eq!(guard.after(true, true, &mut fake), Outcome::PanelLost);
        assert_eq!(fake.set_display_config_calls(), 0);
        assert!(guard.alone() && guard.panel_lost());
        let (warn, text) = fake.logs.last().unwrap();
        assert!(*warn);
        assert_eq!(
            text,
            "The built-in screen went dark when the virtual display turned on: Windows reports \
             it unavailable, and only a restart brings it back. Leaving the virtual display on, \
             so the desktop stays on it outcome=panel_lost paths=0 [] vdd=1. Before: paths=1 \
             [internal tgt=1298b/512 1920x1200@165Hz] vdd=0"
        );
        // Every turn off, the quit's too, stays held, still without a call.
        for _ in 0..2 {
            assert!(!turn_off_allowed(&mut guard, &mut fake));
        }
        assert_eq!(fake.set_display_config_calls(), 0);
        assert_eq!(
            fake.logs.last().unwrap().1,
            "Leaving the virtual display on: the built-in screen is still unavailable, and \
             turning the display off would leave the PC with no screen outcome=kept_on \
             reason=panel_lost paths=0 [] vdd=1; restarting the PC brings the built-in screen \
             back"
        );
        assert!(fake.kept.is_some(), "kept for the failure reporter");
        // Once Windows ends the session, the turn off runs, so the restart
        // starts without the display.
        fake.session_ending = true;
        assert!(turn_off_allowed(&mut guard, &mut fake));
        assert_eq!(fake.set_display_config_calls(), 0);
        assert!(fake.logs.last().unwrap().1.ends_with("outcome=session_end"));
        // A panel that comes back (the lid opened, say) lets it run as on any PC.
        let mut fake = hybrid_laptop();
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        guard.after(true, true, &mut fake);
        fake.built_in = Some(BuiltIn::Offered);
        fake.refuse_reapply = false;
        fake.reapplied = Some(vec![laptop_panel()]);
        assert!(turn_off_allowed(&mut guard, &mut fake));
        assert_eq!(fake.reapply_calls, [1]);
    }

    /// The host that starts after a restart with the display still on
    /// (Windows ended before the turn off ran) has no layout from before.
    /// Its start-up turn off must not leave the PC with no screen either.
    #[test]
    fn a_start_up_turn_off_with_the_panel_withdrawn_leaves_the_display_on() {
        let mut fake = hybrid_laptop();
        fake.reads = VecDeque::from([Some(vec![lone_vdd()])]);
        let mut guard = Guard::new();
        assert!(!turn_off_allowed(&mut guard, &mut fake));
        assert_eq!(fake.set_display_config_calls(), 0);
        // On a PC with one graphics card, it goes as before.
        fake.hybrid = false;
        assert!(turn_off_allowed(&mut guard, &mut fake));
        assert_eq!(fake.extend_calls, 1);
    }

    /// The other ways a turn on leaves the virtual display alone keep the
    /// guard's calls: one graphics card, a panel Windows still offers, a
    /// desktop PC, or a target list Windows can't give.
    #[test]
    fn only_a_withdrawn_panel_on_a_hybrid_pc_is_the_lost_panel() {
        for (hybrid, built_in) in [
            (false, Some(BuiltIn::Withdrawn)),
            (true, Some(BuiltIn::Offered)),
            (true, Some(BuiltIn::Missing)),
            (true, None),
        ] {
            let mut fake = hybrid_laptop();
            (fake.hybrid, fake.built_in) = (hybrid, built_in);
            let mut guard = Guard::new();
            guard.before(true, false, &mut fake);
            assert_eq!(
                guard.after(true, true, &mut fake),
                Outcome::Failed,
                "{hybrid} {built_in:?}"
            );
            assert!(!guard.panel_lost());
            assert_eq!(fake.set_display_config_calls(), 3, "{hybrid} {built_in:?}");
        }
        // The panel still on beside the display: nothing happened to it.
        let mut fake = hybrid_laptop();
        fake.reads = VecDeque::from([
            Some(vec![laptop_panel()]),
            Some(vec![laptop_panel(), lone_vdd()]),
        ]);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Left);
        assert!(!guard.panel_lost());
        // A closed laptop on an external monitor never had the panel on.
        let mut fake = hybrid_laptop();
        fake.reads = VecDeque::from([Some(vec![monitor()]), Some(vec![lone_vdd()])]);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        assert_eq!(guard.after(true, true, &mut fake), Outcome::Failed);
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
        // Both screens went with the virtual display's turn off.
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor(), vdd()],
            vec![],
        ]);
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
        let mut fake = Fake::reading([
            vec![panel()],
            vec![panel(), vdd()],
            vec![panel(), vdd()],
            vec![],
            vec![],
        ]);
        fake.reads.push_back(Some(vec![panel()]));
        assert_eq!(session(&mut fake).1, Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
        assert_eq!(fake.slept, SETTLE_STEP * 2);
    }

    #[test]
    fn a_refused_layout_falls_back_to_pc_screen_only_when_the_panel_was_on() {
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor(), vdd()],
            vec![],
        ]);
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
        let mut fake = Fake::reading([
            vec![panel()],
            vec![panel(), vdd()],
            vec![panel(), vdd()],
            vec![],
        ]);
        fake.internal = Some(vec![panel()]);
        assert_eq!(session(&mut fake).1, Outcome::Restored { internal: true });
        assert_eq!((fake.reapply_calls.len(), fake.internal_calls), (1, 1));
    }

    #[test]
    fn without_the_panel_before_there_is_no_switch_to_it() {
        // A closed laptop on an external monitor that went dark.
        let mut fake = Fake::reading([
            vec![monitor()],
            vec![monitor(), vdd()],
            vec![monitor(), vdd()],
            vec![],
        ]);
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
        let mut fake = Fake::reading([
            vec![panel()],
            vec![panel(), vdd()],
            vec![panel(), vdd()],
            vec![],
        ]);
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
        guard.before(false, false, &mut fake);
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
        guard.before(false, false, &mut fake);
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
        let mut fake = Fake::reading([vec![panel()], vec![panel(), vdd()], vec![panel(), vdd()]]);
        fake.reads.push_back(None);
        assert_eq!(session(&mut fake).1, Outcome::Left);
        assert_eq!(fake.set_display_config_calls(), 0);
    }

    #[test]
    fn the_fallback_looks_again_and_leaves_screens_windows_brought_back() {
        // Windows put both screens back while it refused the saved layout.
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor(), vdd()],
            vec![],
        ]);
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
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor(), vdd()],
            vec![panel(), monitor(), vdd()],
            vec![],
        ]);
        fake.internal = Some(vec![panel()]);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        guard.after(true, true, &mut fake);
        guard.before(false, false, &mut fake);
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
        fn extend(&mut self) -> Result<(), i32> {
            self.0.extend()
        }
        fn built_in(&mut self) -> Option<BuiltIn> {
            self.0.built_in()
        }
        fn hybrid(&self) -> bool {
            self.0.hybrid()
        }
        fn session_ending(&self) -> bool {
            self.0.session_ending()
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
        fn keep(&mut self, record: &str) {
            self.0.keep(record)
        }
        fn forget(&mut self) {
            self.0.forget()
        }
        fn kept(&mut self) -> Option<String> {
            self.0.kept()
        }
    }

    #[test]
    fn the_fallback_stops_when_the_screens_cannot_be_read() {
        let mut fake = Fake::reading([
            vec![panel()],
            vec![panel(), vdd()],
            vec![panel(), vdd()],
            vec![],
        ]);
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

    /// The host turned the display on, then crashed. Its failure reporter,
    /// a new process with nothing in memory, turns the display off, which
    /// takes the PC's screens with it.
    #[test]
    fn the_failure_reporter_restores_the_layout_its_host_kept() {
        let mut host = Fake::reading([vec![panel(), monitor()], vec![panel(), monitor(), vdd()]]);
        host.owner = Some(HOST.into());
        let mut guard = Guard::new();
        guard.before(true, false, &mut host);
        guard.after(true, true, &mut host);
        let kept = host.kept.clone().expect("the host kept its screens");

        let mut reporter = Fake::reading([vec![panel(), monitor(), vdd()], vec![]]);
        reporter.read_count = 100;
        reporter.owner = Some(HOST.into());
        reporter.kept = Some(kept.clone());
        reporter.reapplied = Some(vec![panel(), monitor()]);
        let mut guard = Guard::new();
        guard.before(false, false, &mut reporter);
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

        // The host's display was left on alone: the reporter puts that
        // layout back before its turn off.
        let mut reporter = Fake::reading([vec![vdd()]]);
        reporter.read_count = 100;
        reporter.owner = Some(HOST.into());
        reporter.kept = Some(kept);
        reporter.reapplied = Some(vec![panel(), monitor()]);
        let mut guard = Guard::new();
        assert!(guard.before(false, false, &mut reporter));
        assert_eq!(reporter.reapply_calls, [1]);
        assert_eq!(guard.after(false, true, &mut reporter), Outcome::Left);

        // Without the record, the reporter only sees the virtual display
        // before: no layout to put back, only "Extend", which found no
        // screen here. This is what the record fixes.
        let mut reporter = Fake::reading([vec![vdd()], vec![]]);
        reporter.owner = Some(HOST.into());
        reporter.reapplied = Some(vec![panel()]);
        let mut guard = Guard::new();
        assert!(guard.before(false, false, &mut reporter));
        assert_eq!(guard.after(false, true, &mut reporter), Outcome::Left);
        assert_eq!(
            (reporter.reapply_calls.len(), reporter.extend_calls),
            (0, 1)
        );
    }

    #[test]
    fn a_record_from_another_host_or_one_that_does_not_parse_is_ignored() {
        let mut host = Fake::reading([vec![panel()]]);
        host.owner = Some(HOST.into());
        Guard::new().before(true, false, &mut host);
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
            guard.before(false, false, &mut reporter);
            assert_eq!(
                guard.after(false, true, &mut reporter),
                Outcome::Left,
                "{record}"
            );
            // Only "Extend", which needs no layout.
            assert!(reporter.reapply_calls.is_empty(), "{record}");
            assert_eq!(reporter.internal_calls, 0, "{record}");
            // Another host's record stays, for its own reporter.
            if owner != Some(HOST) {
                assert_eq!(reporter.kept.as_deref(), Some(record.as_str()));
            }
        }
        // A host with no start time keeps nothing.
        let mut host = Fake::reading([vec![panel()]]);
        Guard::new().before(true, false, &mut host);
        assert_eq!(host.kept, None);
    }

    #[test]
    fn the_record_is_deleted_only_after_a_turn_off_that_worked() {
        let mut fake = Fake::reading([vec![panel()], vec![panel(), vdd()]]);
        fake.owner = Some(HOST.into());
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        guard.after(true, true, &mut fake);
        // A failed turn off may leave the display on: the record stays.
        guard.before(false, false, &mut fake);
        guard.after(false, false, &mut fake);
        assert!(fake.kept.is_some());
        // The next turn off, with nothing in memory, still compares with it.
        fake.reads.push_back(Some(vec![panel()]));
        guard.before(false, false, &mut fake);
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

    /// An Enable timed out, then finished late, leaving the virtual display
    /// alone; the retry's turn on must not save that as the layout to
    /// restore, for the host or its failure reporter.
    #[test]
    fn a_turn_on_while_the_display_is_on_keeps_the_layout_with_a_real_screen() {
        let mut fake = Fake::reading([
            vec![panel(), monitor()],
            vec![panel(), monitor()],
            vec![vdd()],
        ]);
        fake.owner = Some(HOST.into());
        fake.reapplied = Some(vec![panel(), monitor()]);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        // The Enable timed out: the gate no longer knows the display's state.
        guard.after(true, false, &mut fake);
        // The retry, after the late Enable left the virtual display alone.
        guard.before(true, false, &mut fake);
        assert!(
            fake.text().contains(
                "Keeping the screens from before the virtual display first turned on: paths=2"
            ),
            "{}",
            fake.text()
        );
        // The record on disk still holds the first read, for the reporter.
        let kept = parse::<usize>(HOST, fake.kept.as_deref().unwrap()).unwrap();
        assert_eq!((kept.screens.len(), kept.saved), (2, 1));
        // "Extend" brings nothing back here; the first read's layout does.
        assert_eq!(
            guard.after(true, true, &mut fake),
            Outcome::Restored { internal: false }
        );
        assert_eq!((fake.extend_calls, &fake.reapply_calls[..]), (1, &[1][..]));

        // The same retry in a host whose layout is only on disk, as after a
        // failed Disable that left nothing in memory.
        let mut fake = Fake::reading([vec![panel()], vec![vdd()]]);
        fake.owner = Some(HOST.into());
        let mut first = Guard::new();
        first.before(true, false, &mut fake);
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        assert_eq!(
            parse::<usize>(HOST, fake.kept.as_deref().unwrap())
                .unwrap()
                .saved,
            1
        );

        // Known off, the screens now are the truth, even with none.
        let mut fake = Fake::reading([vec![panel()], vec![]]);
        fake.owner = Some(HOST.into());
        let mut guard = Guard::new();
        guard.before(true, false, &mut fake);
        guard.before(true, true, &mut fake);
        assert_eq!(
            parse::<usize>(HOST, fake.kept.as_deref().unwrap())
                .unwrap()
                .saved,
            2
        );
    }

    /// A host started again while the reporter restored: its record
    /// replaced the ended host's, and the reporter's turn off leaves it.
    #[test]
    fn the_reporter_never_deletes_a_record_another_host_wrote_meanwhile() {
        let mut host = Fake::reading([vec![panel()]]);
        host.owner = Some(HOST.into());
        Guard::new().before(true, false, &mut host);
        let mut reporter = Fake::reading([vec![panel(), vdd()], vec![]]);
        reporter.owner = Some(HOST.into());
        reporter.kept = host.kept.clone();
        reporter.reapplied = Some(vec![panel()]);
        let mut guard = Guard::new();
        guard.before(false, false, &mut reporter);
        let mut new_host = Fake::reading([vec![panel()]]);
        new_host.owner = Some("5150-133456789099999999".into());
        Guard::new().before(true, false, &mut new_host);
        reporter.kept = new_host.kept.clone();
        assert_eq!(
            guard.after(false, true, &mut reporter),
            Outcome::Restored { internal: false }
        );
        assert_eq!(reporter.kept, new_host.kept);
    }
}
