//! The Stream page's "Fix it" for a Windows Firewall block on this exe.
//!
//! The block Windows adds when its "allow access" prompt is dismissed beats
//! the installer's Allow rule, and the only fix the PC offered was to run
//! Setup again. "Fix it" starts this exe as administrator, with one Windows
//! prompt, with [`FIX_ARG`] and the stream's port. That copy changes the
//! firewall through its COM interface, never a shell, and exits.
//!
//! It can't leave the PC worse off than before. Setup deletes every rule
//! for the exe, then adds its Allow rule; deletions with no add after them
//! could lose rules that worked. The fix is narrower ([`fix_rules`]), and
//! judges every step as the Stream page's verdict does
//! ([`crate::firewall::judge`]), on the networks in use now:
//!
//! 1. Unless the exe would be allowed on every network in use once its
//!    blocks are gone, it adds the installer's Allow rule, first, so a
//!    failure here changes nothing. An Allow rule the user made, however
//!    narrow, that already covers those networks means no add.
//! 2. It removes only this exe's own enabled Block rules that reach the
//!    stream port with no other scope, on a network in use: the blocks the
//!    verdict counts. Each is renamed to a fresh name, then removed by that
//!    name, so no other rule can go with it. Removing a Block rule never
//!    stops anything that worked. Allow rules, scoped rules, blocks for
//!    networks not in use and other programs' rules stay as they are.
//! 3. It reads everything again and fails unless the verdict now finds the
//!    exe allowed, and not blocked by its own rule, on every network in use.
//!    A Block rule that may be this exe's, with its file name and a path the
//!    machine's values can't resolve, is never removed and keeps the fix
//!    from claiming success.
//!
//! The exe's path comes from the elevated process itself, and the port is
//! only ever a number. A rule's path is this exe's only as written, or with
//! `%name%` read from the machine's own settings
//! ([`crate::firewall::machine_env`]), never from the environment the
//! elevated copy inherits from the user who clicked. A user who answers No to the Windows prompt changes
//! nothing: the notice stays as it was. After the copy ran, the firewall
//! is read again at once.

use std::path::Path;

use crate::firewall::{judge, reach, Profile, Reach, Rule, Verdict, DOMAIN, PRIVATE, PUBLIC};

/// The argument the elevated copy runs with, before the stream's port.
pub const FIX_ARG: &str = "--fix-firewall";

/// The installer's Allow rule, as its `[Run]` section adds it: this name,
/// inbound, UDP, every profile, enabled.
pub const ALLOW_NAME: &str = "EternalMonitor Host UDP";
pub const ALLOW_PROFILES: u32 = DOMAIN | PRIVATE | PUBLIC;

/// A firewall rule as the last read found it. `id` names it for
/// [`RuleStore::remove`] until the next read. `rule` is None for an
/// outbound rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub id: usize,
    pub rule: Option<Rule>,
}

/// The firewall, as the elevated copy changes it. The real one is the
/// Windows Firewall COM interface; tests fake it.
pub trait RuleStore {
    /// The profiles of the networks in use now.
    fn profiles(&mut self) -> Result<Vec<Profile>, String>;
    /// Every rule, in any direction.
    fn rules(&mut self) -> Result<Vec<Found>, String>;
    /// Adds the installer's Allow rule for `exe`.
    fn add_allow(&mut self, exe: &str) -> Result<(), String>;
    /// Removes the rule the last read found as `id`, and no other.
    fn remove(&mut self, id: usize) -> Result<(), String>;
}

/// Why the fix failed, as the elevated copy's exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The copy got no usable path or port.
    Arguments = 3,
    /// The rules or the networks in use couldn't be read.
    Read = 10,
    /// The Allow rule couldn't be added. Nothing was changed.
    Add = 11,
    /// A Block rule couldn't be removed.
    Remove = 12,
    /// Read again, the verdict still finds the exe blocked by its own rule,
    /// or not allowed, on a network in use.
    StillBlocked = 14,
}

impl Failure {
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// What `%name%` in a rule's path stands for: [`crate::firewall::machine_env`]
/// in the elevated copy, never its environment, which the user who clicked
/// "Fix it" could have set.
type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The bits of the profiles in use with Windows Firewall on: where the
/// verdict counts rules.
fn in_use(profiles: &[Profile]) -> u32 {
    profiles
        .iter()
        .filter(|profile| profile.on)
        .map(|profile| profile.bit)
        .sum()
}

/// This exe's own Block rules the verdict counts: enabled, reaching every
/// datagram to `port`, on a network in use. All the fix removes.
fn is_target(rule: &Rule, exe: &Path, port: u16, in_use: u32, lookup: Lookup) -> bool {
    !rule.allow
        && rule.enabled
        && rule.udp
        && rule.profiles & in_use != 0
        && crate::firewall::ours(rule, exe, lookup)
        && reach(rule, port) == Reach::All
}

/// Whether the verdict on `rules` finds the exe allowed, and not blocked by
/// its own rule, on every network in use. A network that blocks all
/// incoming connections or has the firewall off is the user's switch, not
/// a rule the fix could change.
fn allowed_everywhere(
    profiles: &[Profile],
    rules: &[&Rule],
    exe: &Path,
    port: u16,
    lookup: Lookup,
) -> bool {
    let rules: Vec<Rule> = rules.iter().map(|rule| (*rule).clone()).collect();
    let report = judge(profiles, &rules, exe, port, true, lookup);
    report.profiles.iter().all(|(_, verdict)| {
        matches!(
            verdict,
            Verdict::Allowed | Verdict::BlocksAll | Verdict::Off
        )
    })
}

fn inbound(found: &[Found]) -> impl Iterator<Item = (usize, &Rule)> {
    found
        .iter()
        .filter_map(|found| found.rule.as_ref().map(|rule| (found.id, rule)))
}

/// The fix, on `store`, for this exe at `exe` and the stream's `port`.
pub fn fix_rules(
    store: &mut impl RuleStore,
    exe: &Path,
    port: u16,
    lookup: Lookup,
) -> Result<(), Failure> {
    let program = exe
        .to_str()
        .filter(|path| !path.is_empty())
        .ok_or(Failure::Arguments)?;
    let profiles = store.profiles().map_err(|_| Failure::Read)?;
    let found = store.rules().map_err(|_| Failure::Read)?;
    let bits = in_use(&profiles);
    let targets: Vec<usize> = inbound(&found)
        .filter(|(_, rule)| is_target(rule, exe, port, bits, lookup))
        .map(|(id, _)| id)
        .collect();
    let without_targets: Vec<&Rule> = inbound(&found)
        .filter(|(id, _)| !targets.contains(id))
        .map(|(_, rule)| rule)
        .collect();
    if !allowed_everywhere(&profiles, &without_targets, exe, port, lookup) {
        store.add_allow(program).map_err(|_| Failure::Add)?;
    }
    for id in targets {
        store.remove(id).map_err(|_| Failure::Remove)?;
    }
    let profiles = store.profiles().map_err(|_| Failure::Read)?;
    let after = store.rules().map_err(|_| Failure::Read)?;
    let after: Vec<&Rule> = inbound(&after).map(|(_, rule)| rule).collect();
    if allowed_everywhere(&profiles, &after, exe, port, lookup) {
        Ok(())
    } else {
        Err(Failure::StillBlocked)
    }
}

/// [`fix_rules`] as the elevated copy runs it, with the machine's own
/// values for `%name%` in rule paths.
#[cfg(any(windows, test))]
pub(crate) fn fix_on_this_machine(
    store: &mut impl RuleStore,
    exe: &Path,
    port: u16,
) -> Result<(), Failure> {
    fix_rules(store, exe, port, &crate::firewall::machine_env)
}

/// Runs `fix` on a store opened inside an apartment, and drops the store
/// before the apartment: COM objects released after `CoUninitialize` may
/// call into a library Windows has already unloaded.
#[cfg(any(windows, test))]
pub(crate) fn in_apartment<A, S, R>(
    apartment: impl FnOnce() -> Result<A, Failure>,
    store: impl FnOnce() -> Result<S, Failure>,
    fix: impl FnOnce(&mut S) -> Result<R, Failure>,
) -> Result<R, Failure> {
    let apartment = apartment()?;
    let mut store = store()?;
    let result = fix(&mut store);
    drop(store);
    drop(apartment);
    result
}

/// How starting the elevated copy went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevated {
    /// It ran and exited with this code.
    Exited(u32),
    /// The user answered No to the Windows prompt.
    Declined,
    /// Windows couldn't start it.
    Failed(String),
}

/// Starts this exe again as administrator with `args`, and waits for it to
/// exit, however long it takes: a second copy never starts meanwhile. The
/// real one asks Windows; tests fake it.
pub trait Elevate {
    fn run_elevated(&self, exe: &Path, args: &str) -> Elevated;
}

/// What the Stream page shows after "Fix it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Read again, the verdict finds the exe allowed on every network in
    /// use; the Stream page reads it again too, and the notice follows.
    Fixed,
    /// No to the Windows prompt: nothing changed.
    Declined,
    /// The fix didn't run or didn't finish. English, as other errors are.
    Failed(&'static str),
}

/// The arguments the elevated copy gets: [`FIX_ARG`] and the port.
pub fn arguments(port: u16) -> String {
    format!("{FIX_ARG} {port}")
}

/// Runs the fix: the elevated copy, then a fresh read of the firewall
/// (`recheck`), unless the user said No, so the notice shows what is true.
pub fn fix(elevate: &impl Elevate, exe: &Path, port: u16, recheck: impl FnOnce()) -> Outcome {
    if exe.to_str().is_none_or(str::is_empty) {
        return Outcome::Failed(FAILED);
    }
    let outcome = match elevate.run_elevated(exe, &arguments(port)) {
        Elevated::Declined => return Outcome::Declined,
        Elevated::Exited(0) => Outcome::Fixed,
        Elevated::Exited(code) if code == Failure::Add.code() => Outcome::Failed(NOT_CHANGED),
        Elevated::Exited(_) | Elevated::Failed(_) => Outcome::Failed(FAILED),
    };
    recheck();
    outcome
}

/// The errors the Stream page shows when the fix didn't work.
pub const FAILED: &str =
    "Windows didn't apply the whole fix. Run the EternalMonitor installer again.";
pub const NOT_CHANGED: &str = "Windows didn't let EternalMonitor add its firewall rule, so nothing was changed. Run the EternalMonitor installer again.";

/// Where the Stream page's "Fix it" is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Progress {
    #[default]
    Idle,
    /// The Windows prompt or the fix is running.
    Running,
    Done(Outcome),
}

static PROGRESS: std::sync::Mutex<Progress> = std::sync::Mutex::new(Progress::Idle);

pub fn progress() -> Progress {
    PROGRESS.lock().map_or(Progress::Idle, |progress| *progress)
}

/// Forgets how the last fix went: the user dismissed its message.
pub fn dismiss() {
    if let Ok(mut progress) = PROGRESS.lock() {
        if matches!(*progress, Progress::Done(_)) {
            *progress = Progress::Idle;
        }
    }
}

/// Runs `fix` off the window's thread, once at a time: the Windows prompt
/// waits for the user, and the fix for Windows.
pub fn start(fix: impl FnOnce() -> Outcome + Send + 'static) {
    {
        let Ok(mut progress) = PROGRESS.lock() else {
            return;
        };
        if *progress == Progress::Running {
            return;
        }
        *progress = Progress::Running;
    }
    let spawned = std::thread::Builder::new()
        .name("firewall-fix".into())
        .spawn(move || {
            let outcome = fix();
            tracing::info!(?outcome, "Firewall fix finished");
            if let Ok(mut progress) = PROGRESS.lock() {
                *progress = Progress::Done(outcome);
            }
        });
    if spawned.is_err() {
        if let Ok(mut progress) = PROGRESS.lock() {
            *progress = Progress::Done(Outcome::Failed(FAILED));
        }
    }
}

/// The stream's port the last fix was for, for Try again.
static LAST_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Starts the fix for the stream on `port` ([`fix_now`]).
pub fn start_for(port: u16) {
    LAST_PORT.store(port, std::sync::atomic::Ordering::Relaxed);
    start(move || fix_now(port));
}

/// The last fix's port, if one was started.
pub fn last_port() -> Option<u16> {
    Some(LAST_PORT.load(std::sync::atomic::Ordering::Relaxed)).filter(|port| *port != 0)
}

/// The elevated copy's arguments after [`FIX_ARG`]: the port, a number.
pub fn parse_port(args: &[String]) -> Option<u16> {
    match args {
        [port] => port.parse().ok().filter(|port| *port != 0),
        _ => None,
    }
}

/// If this process is the elevated copy ([`FIX_ARG`]), runs the fix and
/// exits with its code. Called first thing in `main`.
pub fn run_if_requested() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some(FIX_ARG) {
        return;
    }
    let code = match (std::env::current_exe(), parse_port(&args[1..])) {
        (Ok(exe), Some(port)) => windows_firewall::fix(&exe, port),
        _ => Failure::Arguments.code(),
    };
    std::process::exit(code as i32);
}

/// The fix as the Stream page runs it, on Windows.
#[cfg(windows)]
pub fn fix_now(port: u16) -> Outcome {
    let Ok(exe) = std::env::current_exe() else {
        return Outcome::Failed(FAILED);
    };
    fix(
        &windows_elevate::Shell,
        &exe,
        port,
        crate::firewall::recheck_now,
    )
}

/// Off Windows there is no firewall notice, so no fix.
#[cfg(not(windows))]
pub fn fix_now(_port: u16) -> Outcome {
    Outcome::Failed(FAILED)
}

#[cfg(windows)]
mod windows_firewall {
    use std::path::Path;

    use windows::core::BSTR;
    use windows::Win32::Foundation::{E_FAIL, VARIANT_TRUE};
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        INetFwPolicy2, INetFwRule, NetFwPolicy2, NetFwRule, NET_FW_ACTION_ALLOW, NET_FW_RULE_DIR_IN,
    };
    use windows::Win32::System::Com::{CoCreateGuid, CoCreateInstance, CLSCTX_INPROC_SERVER};

    use super::{
        fix_on_this_machine, in_apartment, Failure, Found, RuleStore, ALLOW_NAME, ALLOW_PROFILES,
    };
    use crate::firewall::windows_policy::{current_profiles, for_each_rule, Apartment};
    use crate::firewall::{rule_from, Profile};

    /// UDP, as `NET_FW_IP_PROTOCOL`.
    const UDP: i32 = 17;

    /// The policy, and the rules its last read found, by id.
    struct Com {
        policy: INetFwPolicy2,
        found: Vec<INetFwRule>,
    }

    impl RuleStore for Com {
        fn profiles(&mut self) -> Result<Vec<Profile>, String> {
            current_profiles(&self.policy).map_err(|e| e.to_string())
        }

        fn rules(&mut self) -> Result<Vec<Found>, String> {
            let mut found = Vec::new();
            let mut rules = Vec::new();
            for_each_rule(&self.policy, |rule| {
                let read = rule_from(rule).map_err(|e| windows::core::Error::new(E_FAIL, e))?;
                found.push(Found {
                    id: rules.len(),
                    rule: read,
                });
                rules.push(rule.clone());
                Ok(())
            })
            .map_err(|e| e.to_string())?;
            self.found = rules;
            Ok(found)
        }

        fn add_allow(&mut self, exe: &str) -> Result<(), String> {
            let add = || -> windows::core::Result<()> {
                let rule: INetFwRule =
                    unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER)? };
                unsafe {
                    rule.SetName(&BSTR::from(ALLOW_NAME))?;
                    rule.SetApplicationName(&BSTR::from(exe))?;
                    rule.SetProtocol(UDP)?;
                    rule.SetDirection(NET_FW_RULE_DIR_IN)?;
                    rule.SetAction(NET_FW_ACTION_ALLOW)?;
                    rule.SetProfiles(ALLOW_PROFILES as i32)?;
                    rule.SetEnabled(VARIANT_TRUE)?;
                    self.policy.Rules()?.Add(&rule)
                }
            };
            add().map_err(|e| e.to_string())
        }

        /// Renames the rule to a fresh name first, so removing by name takes
        /// it alone: not a rule that shares its name, whatever the case, nor
        /// one added since the read. A failed remove leaves a renamed Block
        /// rule, no worse than before.
        fn remove(&mut self, id: usize) -> Result<(), String> {
            let rule = self.found.get(id).ok_or("no such rule")?;
            let remove = || -> windows::core::Result<()> {
                let guid = unsafe { CoCreateGuid()? };
                let name = BSTR::from(format!("EternalMonitor fix {guid:?}"));
                unsafe {
                    rule.SetName(&name)?;
                    self.policy.Rules()?.Remove(&name)
                }
            };
            remove().map_err(|e| e.to_string())
        }
    }

    /// The elevated copy's work: the exit code.
    pub fn fix(exe: &Path, port: u16) -> u32 {
        in_apartment(
            || Apartment::new().map_err(|_| Failure::Read),
            || {
                let policy: INetFwPolicy2 =
                    unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }
                        .map_err(|_| Failure::Read)?;
                Ok(Com {
                    policy,
                    found: Vec::new(),
                })
            },
            |store| fix_on_this_machine(store, exe, port),
        )
        .map_or_else(Failure::code, |()| 0)
    }
}

#[cfg(not(windows))]
mod windows_firewall {
    pub fn fix(_exe: &std::path::Path, _port: u16) -> u32 {
        super::Failure::Read.code()
    }
}

#[cfg(windows)]
mod windows_elevate {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SW_HIDE};

    use super::{Elevate, Elevated};

    /// How long to wait between looks at a copy that hasn't finished.
    const WAIT_MS: u32 = 60_000;

    pub struct Shell;

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    /// The single-threaded apartment ShellExecuteEx expects, for this call.
    struct Sta(bool);

    impl Drop for Sta {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    impl Elevate for Shell {
        fn run_elevated(&self, exe: &Path, args: &str) -> Elevated {
            let _sta = Sta(unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok()
            });
            let verb = wide("runas".as_ref());
            let file = wide(exe.as_os_str());
            let params = wide(args.as_ref());
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                // No message loop here, and the thread blocks below: the
                // call must finish before it returns.
                fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
                // The window just clicked: the Windows prompt belongs to it,
                // not to whatever is behind.
                hwnd: unsafe { GetForegroundWindow() },
                lpVerb: PCWSTR(verb.as_ptr()),
                lpFile: PCWSTR(file.as_ptr()),
                lpParameters: PCWSTR(params.as_ptr()),
                nShow: SW_HIDE.0,
                ..Default::default()
            };
            if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
                return if error.code() == ERROR_CANCELLED.to_hresult() {
                    Elevated::Declined
                } else {
                    Elevated::Failed(error.to_string())
                };
            }
            if info.hProcess.is_invalid() {
                return Elevated::Failed("no process".into());
            }
            // Until it exits, however long: the page stays on "running", so
            // Try again can't start a second copy.
            let elevated = loop {
                match unsafe { WaitForSingleObject(info.hProcess, WAIT_MS) } {
                    WAIT_OBJECT_0 => {
                        let mut code = 0u32;
                        break match unsafe { GetExitCodeProcess(info.hProcess, &mut code) } {
                            Ok(()) => Elevated::Exited(code),
                            Err(error) => Elevated::Failed(error.to_string()),
                        };
                    }
                    WAIT_TIMEOUT => {
                        tracing::warn!("The firewall fix is taking a while; still waiting for it");
                    }
                    _ => break Elevated::Failed("lost track of the fix".into()),
                }
            };
            unsafe {
                let _ = CloseHandle(info.hProcess);
            }
            elevated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const EXE: &str = r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe";
    const PORT: u16 = 9876;

    fn exe() -> &'static Path {
        Path::new(EXE)
    }

    fn on(bit: u32) -> Profile {
        Profile {
            bit,
            on: true,
            block_all_inbound: false,
        }
    }

    /// What Windows adds when its prompt is dismissed.
    fn prompt_block(profiles: u32) -> Rule {
        Rule::unscoped(&EXE.to_lowercase(), false, profiles)
    }

    fn installer_allow() -> Rule {
        Rule::unscoped(EXE, true, ALLOW_PROFILES)
    }

    /// The machine's own values.
    fn machine(name: &str) -> Option<String> {
        name.eq_ignore_ascii_case("ProgramFiles")
            .then(|| r"C:\Program Files".to_string())
    }

    /// A firewall in memory. Removes take the rule they name and no other.
    struct Fake {
        profiles: Vec<Profile>,
        rules: Vec<(String, Option<Rule>)>,
        add_fails: bool,
        remove_fails: bool,
        read_fails: bool,
        calls: Vec<String>,
    }

    impl Fake {
        fn new(profiles: &[Profile], rules: Vec<(&str, Rule)>) -> Self {
            Self {
                profiles: profiles.to_vec(),
                rules: rules
                    .into_iter()
                    .map(|(name, rule)| (name.to_string(), Some(rule)))
                    .chain([("Outbound".to_string(), None)])
                    .collect(),
                add_fails: false,
                remove_fails: false,
                read_fails: false,
                calls: Vec::new(),
            }
        }

        fn names(&self) -> Vec<&str> {
            self.rules.iter().map(|(name, _)| name.as_str()).collect()
        }
    }

    impl RuleStore for Fake {
        fn profiles(&mut self) -> Result<Vec<Profile>, String> {
            if self.read_fails {
                return Err("read".into());
            }
            Ok(self.profiles.clone())
        }

        fn rules(&mut self) -> Result<Vec<Found>, String> {
            if self.read_fails {
                return Err("read".into());
            }
            Ok(self
                .rules
                .iter()
                .enumerate()
                .map(|(id, (_, rule))| Found {
                    id,
                    rule: rule.clone(),
                })
                .collect())
        }

        fn add_allow(&mut self, exe: &str) -> Result<(), String> {
            self.calls.push(format!("add {exe}"));
            if self.add_fails {
                return Err("add".into());
            }
            self.rules.push((
                ALLOW_NAME.into(),
                Some(Rule::unscoped(exe, true, ALLOW_PROFILES)),
            ));
            Ok(())
        }

        fn remove(&mut self, id: usize) -> Result<(), String> {
            self.calls.push(format!("remove {}", self.rules[id].0));
            if self.remove_fails {
                return Err("remove".into());
            }
            self.rules.remove(id);
            Ok(())
        }
    }

    const PROMPT_UDP: &str = "UDP Query User{0-1-2-3-4}eternalmonitor-host.exe";

    fn dismissed_prompt() -> Vec<(&'static str, Rule)> {
        vec![
            (ALLOW_NAME, installer_allow()),
            (PROMPT_UDP, prompt_block(PRIVATE | PUBLIC)),
            (
                "TCP Query User{0-1-2-3-4}eternalmonitor-host.exe",
                Rule {
                    udp: false,
                    ..prompt_block(PRIVATE)
                },
            ),
            (
                "Other",
                Rule::unscoped(r"C:\Tools\Other.exe", false, PUBLIC),
            ),
        ]
    }

    /// Windows' Block rule for the exe goes; its Allow rule, the TCP block
    /// that doesn't reach the stream, other programs' rules and outbound
    /// rules stay.
    #[test]
    fn the_fix_removes_only_this_exes_block_and_keeps_the_rest() {
        let mut store = Fake::new(&[on(PUBLIC)], dismissed_prompt());
        assert_eq!(fix_rules(&mut store, exe(), PORT, &machine), Ok(()));
        assert_eq!(
            store.names(),
            [
                ALLOW_NAME,
                "TCP Query User{0-1-2-3-4}eternalmonitor-host.exe",
                "Other",
                "Outbound"
            ]
        );
        assert_eq!(store.calls, [format!("remove {PROMPT_UDP}")]);
    }

    /// The user's own block for a network not in use was never counted,
    /// and stays.
    #[test]
    fn a_block_on_a_network_not_in_use_is_left() {
        let mut rules = dismissed_prompt();
        rules.push(("Block EternalMonitor on Public", prompt_block(PUBLIC)));
        let mut store = Fake::new(&[on(PRIVATE)], rules);
        assert_eq!(fix_rules(&mut store, exe(), PORT, &machine), Ok(()));
        assert!(store.names().contains(&"Block EternalMonitor on Public"));
        assert_eq!(store.calls, [format!("remove {PROMPT_UDP}")]);
    }

    /// A network with Windows Firewall off, or one that blocks all incoming
    /// connections, is the user's switch: a block there is left, and the
    /// fix still works on the other networks.
    #[test]
    fn the_users_switches_are_left_as_they_are() {
        let mut rules = dismissed_prompt();
        rules.push(("Block EternalMonitor on Domain", prompt_block(DOMAIN)));
        let switches = [
            Profile {
                bit: DOMAIN,
                on: false,
                block_all_inbound: false,
            },
            Profile {
                bit: PRIVATE,
                on: true,
                block_all_inbound: true,
            },
        ];
        let mut store = Fake::new(&[switches[0], switches[1], on(PUBLIC)], rules);
        assert_eq!(fix_rules(&mut store, exe(), PORT, &machine), Ok(()));
        assert!(store.names().contains(&"Block EternalMonitor on Domain"));
        assert_eq!(store.calls, [format!("remove {PROMPT_UDP}")]);
    }

    /// No Allow rule at all (deleted, or never added): it comes first, as
    /// the installer adds it, then the block goes.
    #[test]
    fn a_missing_allow_rule_is_added_before_anything_is_removed() {
        let mut rules = dismissed_prompt();
        rules.remove(0);
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        assert_eq!(fix_rules(&mut store, exe(), PORT, &machine), Ok(()));
        assert_eq!(
            store.calls,
            [format!("add {EXE}"), format!("remove {PROMPT_UDP}")]
        );
    }

    /// The user's own Allow rule that already lets the stream in on the
    /// networks in use, however narrow, means no add: the fix doesn't
    /// widen what the user chose.
    #[test]
    fn the_users_own_allow_rule_means_no_add() {
        let private_only = Rule::unscoped(EXE, true, PRIVATE);
        let lan_only = Rule {
            remote_addresses: Some("LocalSubnet".into()),
            ..Rule::unscoped(EXE, true, PRIVATE)
        };
        for allow in [private_only, lan_only] {
            let mut store = Fake::new(
                &[on(PRIVATE)],
                vec![("Mine", allow.clone()), (PROMPT_UDP, prompt_block(PRIVATE))],
            );
            assert_eq!(
                fix_rules(&mut store, exe(), PORT, &machine),
                Ok(()),
                "{allow:?}"
            );
            assert_eq!(store.calls, [format!("remove {PROMPT_UDP}")], "{allow:?}");
        }
    }

    /// The fix's Allow rule is the one the installer's [Run] section adds.
    #[test]
    fn the_allow_rule_is_the_installers() {
        let iss = include_str!("../../installer/EternalMonitor.iss");
        let run = iss
            .split("\n[Run]")
            .nth(1)
            .and_then(|section| section.split("\n[").next())
            .unwrap();
        let add = run
            .lines()
            .find(|line| line.contains("advfirewall firewall add rule"))
            .expect("the installer adds its Allow rule");
        assert!(add.contains(&format!(r#"name=""{ALLOW_NAME}"""#)), "{add}");
        assert!(
            add.contains(r#"program=""{app}\EternalMonitor-host.exe"""#),
            "{add}"
        );
        for part in [
            "dir=in",
            "action=allow",
            "protocol=UDP",
            "profile=domain,private,public",
            "enable=yes",
        ] {
            assert!(add.contains(part), "{part} in {add}");
        }
    }

    /// Never worse than before: when the Allow rule can't be added,
    /// nothing is removed, and the rules are exactly as they were.
    #[test]
    fn a_failed_add_leaves_the_rules_exactly_as_they_were() {
        let mut rules = dismissed_prompt();
        rules.remove(0);
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        let before = store.rules.clone();
        store.add_fails = true;
        assert_eq!(
            fix_rules(&mut store, exe(), PORT, &machine),
            Err(Failure::Add)
        );
        assert_eq!(store.rules, before);
        assert_eq!(store.calls, [format!("add {EXE}")]);
    }

    /// A Block rule that can't be removed is a failure, not a success, and
    /// nothing that let traffic through went.
    #[test]
    fn a_failed_remove_is_a_failure() {
        let mut store = Fake::new(&[on(PUBLIC)], dismissed_prompt());
        let before = store.rules.clone();
        store.remove_fails = true;
        assert_eq!(
            fix_rules(&mut store, exe(), PORT, &machine),
            Err(Failure::Remove)
        );
        assert_eq!(store.rules, before);
    }

    /// A remove that reports success but leaves the block, or an add that
    /// leaves no Allow rule, is caught when everything is read again.
    #[test]
    fn a_fix_that_did_not_take_is_a_failure() {
        struct Stubborn(Fake);
        impl RuleStore for Stubborn {
            fn profiles(&mut self) -> Result<Vec<Profile>, String> {
                self.0.profiles()
            }
            fn rules(&mut self) -> Result<Vec<Found>, String> {
                self.0.rules()
            }
            fn add_allow(&mut self, exe: &str) -> Result<(), String> {
                self.0.add_allow(exe)
            }
            fn remove(&mut self, _id: usize) -> Result<(), String> {
                Ok(())
            }
        }
        let mut store = Stubborn(Fake::new(&[on(PUBLIC)], dismissed_prompt()));
        assert_eq!(
            fix_rules(&mut store, exe(), PORT, &machine),
            Err(Failure::StillBlocked)
        );

        struct NoAdd(Fake);
        impl RuleStore for NoAdd {
            fn profiles(&mut self) -> Result<Vec<Profile>, String> {
                self.0.profiles()
            }
            fn rules(&mut self) -> Result<Vec<Found>, String> {
                self.0.rules()
            }
            fn add_allow(&mut self, _exe: &str) -> Result<(), String> {
                Ok(())
            }
            fn remove(&mut self, id: usize) -> Result<(), String> {
                self.0.remove(id)
            }
        }
        let mut rules = dismissed_prompt();
        rules.remove(0);
        let mut store = NoAdd(Fake::new(&[on(PUBLIC)], rules));
        assert_eq!(
            fix_rules(&mut store, exe(), PORT, &machine),
            Err(Failure::StillBlocked)
        );
    }

    /// A Windows prompt answered for one network type can leave an Allow
    /// and a Block under one name: only the block goes.
    #[test]
    fn a_block_sharing_its_name_goes_alone() {
        let mut rules = dismissed_prompt();
        rules.push((
            PROMPT_UDP,
            Rule::unscoped(&EXE.to_lowercase(), true, PRIVATE),
        ));
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        assert_eq!(fix_rules(&mut store, exe(), PORT, &machine), Ok(()));
        let left: Vec<_> = store
            .rules
            .iter()
            .filter(|(name, _)| name == PROMPT_UDP)
            .collect();
        assert_eq!(left.len(), 1);
        assert!(left[0].1.as_ref().unwrap().allow, "the Allow rule stays");
    }

    /// A scoped Block rule, a disabled one, or one for another port is the
    /// user's own: left, as the verdict leaves it.
    #[test]
    fn blocks_the_verdict_does_not_count_are_left() {
        let rules = vec![
            (ALLOW_NAME, installer_allow()),
            (
                "Scoped",
                Rule {
                    remote_addresses: Some("10.0.0.0/8".into()),
                    ..prompt_block(PUBLIC)
                },
            ),
            (
                "Off",
                Rule {
                    enabled: false,
                    ..prompt_block(PUBLIC)
                },
            ),
            (
                "Port",
                Rule {
                    local_ports: Some("80".into()),
                    ..prompt_block(PUBLIC)
                },
            ),
        ];
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        let before = store.rules.clone();
        assert_eq!(fix_rules(&mut store, exe(), PORT, &machine), Ok(()));
        assert_eq!(store.rules, before);
        assert!(store.calls.is_empty());
    }

    /// A rule's `%name%` means what the machine says, not what this
    /// process's environment says: a user can set their own value, and the
    /// elevated copy inherits it. Another program's rule never goes because
    /// of it.
    #[test]
    fn only_the_machines_values_make_a_rule_this_exes() {
        const USERS_OWN: &str = "EM_FIREWALL_FIX_TEST_ROOT";
        // A value only this process has, set as a user would.
        std::env::set_var(USERS_OWN, r"C:\Program Files");
        let installed = r"D:\Apps\EternalMonitor\EternalMonitor-host.exe";
        let rules = vec![
            (ALLOW_NAME, Rule::unscoped(installed, true, ALLOW_PROFILES)),
            (
                "Program Files copy",
                Rule::unscoped(
                    r"%ProgramFiles%\EternalMonitor\EternalMonitor-host.exe",
                    false,
                    PUBLIC,
                ),
            ),
        ];
        // The process would say D:\Apps for ProgramFiles; the machine says
        // C:\Program Files.
        let mut store = Fake::new(&[on(PUBLIC)], rules.clone());
        assert_eq!(
            fix_rules(&mut store, Path::new(installed), PORT, &machine),
            Ok(())
        );
        assert!(store.calls.is_empty(), "{:?}", store.calls);

        // The same rules, for the copy the machine's value names: its
        // block goes, and it gets its own Allow rule.
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        assert_eq!(fix_rules(&mut store, exe(), PORT, &machine), Ok(()));
        assert_eq!(
            store.calls,
            [format!("add {EXE}"), "remove Program Files copy".into()]
        );

        // A variable only this process has, as the elevated copy runs it:
        // the rule is never removed, and as it may be this exe's, the fix
        // doesn't claim success.
        let rules = vec![
            (ALLOW_NAME, installer_allow()),
            (
                "Users own",
                Rule::unscoped(
                    &format!(r"%{USERS_OWN}%\EternalMonitor\EternalMonitor-host.exe"),
                    false,
                    PUBLIC,
                ),
            ),
        ];
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        assert_eq!(
            fix_on_this_machine(&mut store, exe(), PORT),
            Err(Failure::StillBlocked)
        );
        assert!(store.names().contains(&"Users own"));
        assert!(!store.calls.iter().any(|call| call.starts_with("remove")));
        std::env::remove_var(USERS_OWN);
    }

    /// A machine value naming another (`EMROOT=%SystemDrive%\Apps`) makes
    /// the rule this exe's: it goes. One that can't be resolved, with this
    /// exe's file name, stays, and the fix fails rather than claim success.
    #[test]
    fn a_block_through_nested_machine_values_goes_and_an_unresolved_one_fails_the_fix() {
        use crate::firewall::{resolve, Stored};
        let stored = |name: &str| match name.to_lowercase().as_str() {
            "systemdrive" => Some(Stored::Literal("C:".into())),
            "emroot" => Some(Stored::Expand(r"%SystemDrive%\Apps".into())),
            "userish" => Some(Stored::Expand(r"%USERPROFILE%\Apps".into())),
            _ => None,
        };
        let lookup = |name: &str| resolve(name, &stored);
        let exe = Path::new(r"C:\Apps\EternalMonitor\EternalMonitor-host.exe");
        let allow = Rule::unscoped(&exe.to_string_lossy(), true, ALLOW_PROFILES);
        let block = |root: &str| {
            Rule::unscoped(
                &format!(r"%{root}%\EternalMonitor\EternalMonitor-host.exe"),
                false,
                PUBLIC,
            )
        };

        let rules = vec![(ALLOW_NAME, allow.clone()), ("Nested", block("EMROOT"))];
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        assert_eq!(fix_rules(&mut store, exe, PORT, &lookup), Ok(()));
        assert_eq!(store.calls, ["remove Nested"]);

        let rules = vec![(ALLOW_NAME, allow), ("Unresolved", block("USERISH"))];
        let mut store = Fake::new(&[on(PUBLIC)], rules);
        assert_eq!(
            fix_rules(&mut store, exe, PORT, &lookup),
            Err(Failure::StillBlocked)
        );
        assert!(store.names().contains(&"Unresolved"));
        assert!(!store.calls.iter().any(|call| call.starts_with("remove")));
    }

    #[test]
    fn rules_that_cant_be_read_are_a_failure() {
        let mut store = Fake::new(&[on(PUBLIC)], dismissed_prompt());
        store.read_fails = true;
        assert_eq!(
            fix_rules(&mut store, exe(), PORT, &machine),
            Err(Failure::Read)
        );
        assert!(store.calls.is_empty());
        assert_eq!(
            fix_rules(&mut store, Path::new(""), PORT, &machine),
            Err(Failure::Arguments)
        );
    }

    /// The store goes before the apartment, whatever the fix returns.
    #[test]
    fn the_store_is_released_before_the_apartment() {
        struct Noted(&'static str, std::rc::Rc<RefCell<Vec<&'static str>>>);
        impl Drop for Noted {
            fn drop(&mut self) {
                self.1.borrow_mut().push(self.0);
            }
        }
        for result in [Ok(()), Err(Failure::Remove)] {
            let order = std::rc::Rc::new(RefCell::new(Vec::new()));
            let got = in_apartment(
                || Ok(Noted("apartment", order.clone())),
                || Ok(Noted("store", order.clone())),
                |_| result,
            );
            assert_eq!(got, result);
            assert_eq!(*order.borrow(), ["store", "apartment"]);
        }
    }

    /// The elevated copy gets the fixed argument and a port, nothing else.
    #[test]
    fn the_elevated_copy_takes_only_a_port() {
        assert_eq!(arguments(PORT), "--fix-firewall 9876");
        assert_eq!(parse_port(&["9876".into()]), Some(9876));
        let bad: [&[&str]; 5] = [&[], &["0"], &["98765"], &["9876 x"], &["1", "2"]];
        for args in bad {
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            assert_eq!(parse_port(&args), None, "{args:?}");
        }
    }

    struct FakeElevate {
        answer: Elevated,
        asked: RefCell<Vec<(String, String)>>,
    }

    impl Elevate for FakeElevate {
        fn run_elevated(&self, exe: &Path, args: &str) -> Elevated {
            self.asked
                .borrow_mut()
                .push((exe.display().to_string(), args.to_string()));
            self.answer.clone()
        }
    }

    fn elevate(answer: Elevated) -> FakeElevate {
        FakeElevate {
            answer,
            asked: RefCell::new(Vec::new()),
        }
    }

    /// The elevated copy is this exe with the fixed arguments, and the
    /// firewall is read again once it ran.
    #[test]
    fn the_fix_starts_this_exe_elevated_then_reads_the_firewall_again() {
        let elevate = elevate(Elevated::Exited(0));
        let rechecked = Cell::new(false);
        assert_eq!(
            fix(&elevate, exe(), PORT, || rechecked.set(true)),
            Outcome::Fixed
        );
        assert!(rechecked.get());
        assert_eq!(
            *elevate.asked.borrow(),
            [(EXE.to_string(), "--fix-firewall 9876".to_string())]
        );
    }

    /// No to the Windows prompt: nothing ran, nothing is read again, and no
    /// error shows.
    #[test]
    fn declining_the_windows_prompt_changes_nothing() {
        let rechecked = Cell::new(false);
        assert_eq!(
            fix(&elevate(Elevated::Declined), exe(), PORT, || rechecked
                .set(true)),
            Outcome::Declined
        );
        assert!(!rechecked.get());
    }

    /// Each failure says what happened, and the firewall is read again in
    /// case some of it ran.
    #[test]
    fn a_fix_that_fails_says_so() {
        for (answer, said) in [
            (Elevated::Exited(Failure::Add.code()), NOT_CHANGED),
            (Elevated::Exited(Failure::Remove.code()), FAILED),
            (Elevated::Exited(Failure::StillBlocked.code()), FAILED),
            (Elevated::Exited(1), FAILED),
            (Elevated::Failed("x".into()), FAILED),
        ] {
            let rechecked = Cell::new(false);
            assert_eq!(
                fix(&elevate(answer.clone()), exe(), PORT, || rechecked
                    .set(true)),
                Outcome::Failed(said),
                "{answer:?}"
            );
            assert!(rechecked.get(), "{answer:?}");
        }
        let nobody = elevate(Elevated::Exited(0));
        assert_eq!(
            fix(&nobody, Path::new(""), PORT, || {}),
            Outcome::Failed(FAILED)
        );
        assert!(nobody.asked.borrow().is_empty());
    }

    /// One fix at a time; the window hears how it went until dismissed.
    #[test]
    fn the_page_sees_the_fix_run_and_finish() {
        let (go, wait) = std::sync::mpsc::channel::<()>();
        start(move || {
            let _ = wait.recv();
            Outcome::Failed(FAILED)
        });
        assert_eq!(progress(), Progress::Running);
        start(|| panic!("a second fix started"));
        dismiss();
        assert_eq!(progress(), Progress::Running, "a running fix stays");
        go.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while progress() == Progress::Running {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(progress(), Progress::Done(Outcome::Failed(FAILED)));
        dismiss();
        assert_eq!(progress(), Progress::Idle);
    }
}
