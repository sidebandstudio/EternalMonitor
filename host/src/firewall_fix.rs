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
//! could lose rules that worked. The fix is narrower ([`fix_rules`]):
//!
//! 1. It adds the installer's Allow rule, unless one that works is there.
//!    It does this first, so a failure here changes nothing.
//! 2. It removes only this exe's own Block rules that reach the stream
//!    port with no other scope: the rules the Stream page's verdict counts.
//!    Removing a Block rule never stops anything that worked. Allow rules,
//!    scoped rules and other programs' rules stay as they are.
//! 3. It reads the rules again and fails unless the Allow rule is there and
//!    none of those Block rules is left.
//!
//! The exe's path comes from the elevated process itself, and the port is
//! only ever a number. A user who answers No to the Windows prompt changes
//! nothing: the notice stays as it was. After the copy ran, the firewall
//! is read again at once.

use std::path::Path;

use crate::firewall::{reach, Reach, Rule, DOMAIN, PRIVATE, PUBLIC};

/// The argument the elevated copy runs with, before the stream's port.
pub const FIX_ARG: &str = "--fix-firewall";

/// The installer's Allow rule, as its `[Run]` section adds it: this name,
/// inbound, UDP, every profile, enabled.
pub const ALLOW_NAME: &str = "EternalMonitor Host UDP";
pub const ALLOW_PROFILES: u32 = DOMAIN | PRIVATE | PUBLIC;

/// A firewall rule with its name. `rule` is None for an outbound rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub rule: Option<Rule>,
}

/// The firewall, as the elevated copy changes it. The real one is the
/// Windows Firewall COM interface; tests fake it.
pub trait RuleStore {
    /// Every rule, in any direction.
    fn rules(&mut self) -> Result<Vec<Named>, String>;
    /// Adds the installer's Allow rule for `exe`.
    fn add_allow(&mut self, exe: &str) -> Result<(), String>;
    /// Removes the rule named `name`.
    fn remove(&mut self, name: &str) -> Result<(), String>;
}

/// Why the fix failed, as the elevated copy's exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The copy got no usable path or port.
    Arguments = 3,
    /// The rules couldn't be read.
    Read = 10,
    /// The Allow rule couldn't be added. Nothing was changed.
    Add = 11,
    /// A Block rule couldn't be removed.
    Remove = 12,
    /// A Block rule shares its name with another rule, so removing it by
    /// name could remove that one too. It was left.
    SharedName = 13,
    /// Read again, a Block rule is still there, or the Allow rule isn't.
    StillBlocked = 14,
}

impl Failure {
    pub fn code(self) -> u32 {
        self as u32
    }
}

fn ours(rule: &Rule, exe: &Path) -> bool {
    crate::firewall::ours(rule, exe, |name| std::env::var(name).ok())
}

/// This exe's own Block rules that reach every datagram to `port`: what
/// the verdict counts as a block, and all the fix removes.
fn is_block(rule: &Rule, exe: &Path, port: u16) -> bool {
    !rule.allow && rule.enabled && rule.udp && ours(rule, exe) && reach(rule, port) == Reach::All
}

/// An Allow rule that lets every datagram to `port` through on every
/// profile, as the installer's does.
fn is_working_allow(rule: &Rule, exe: &Path, port: u16) -> bool {
    rule.allow
        && rule.enabled
        && rule.udp
        && rule.profiles & ALLOW_PROFILES == ALLOW_PROFILES
        && ours(rule, exe)
        && reach(rule, port) == Reach::All
}

fn inbound(rules: &[Named]) -> impl Iterator<Item = (&str, &Rule)> {
    rules
        .iter()
        .filter_map(|named| named.rule.as_ref().map(|rule| (named.name.as_str(), rule)))
}

/// The fix, on `store`, for this exe at `exe` and the stream's `port`.
pub fn fix_rules(store: &mut impl RuleStore, exe: &Path, port: u16) -> Result<(), Failure> {
    let program = exe
        .to_str()
        .filter(|path| !path.is_empty())
        .ok_or(Failure::Arguments)?;
    let rules = store.rules().map_err(|_| Failure::Read)?;
    if !inbound(&rules).any(|(_, rule)| is_working_allow(rule, exe, port)) {
        store.add_allow(program).map_err(|_| Failure::Add)?;
    }
    let mut shared = false;
    for (name, rule) in inbound(&rules) {
        if !is_block(rule, exe, port) {
            continue;
        }
        if rules.iter().filter(|other| other.name == name).count() > 1 {
            shared = true;
            continue;
        }
        store.remove(name).map_err(|_| Failure::Remove)?;
    }
    let after = store.rules().map_err(|_| Failure::Read)?;
    let blocked = inbound(&after).any(|(_, rule)| is_block(rule, exe, port));
    let allowed = inbound(&after).any(|(_, rule)| is_working_allow(rule, exe, port));
    if blocked && shared {
        Err(Failure::SharedName)
    } else if blocked || !allowed {
        Err(Failure::StillBlocked)
    } else {
        Ok(())
    }
}

/// How starting the elevated copy went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevated {
    /// It ran and exited with this code.
    Exited(u32),
    /// The user answered No to the Windows prompt.
    Declined,
    /// Windows couldn't start it, or it didn't finish in time.
    Failed(String),
}

/// Starts this exe again as administrator with `args`, and waits for it.
/// The real one asks Windows; tests fake it.
pub trait Elevate {
    fn run_elevated(&self, exe: &Path, args: &str) -> Elevated;
}

/// What the Stream page shows after "Fix it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The firewall now has the Allow rule and none of the Block rules; it
    /// was read again, and the notice follows it.
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
        Elevated::Exited(code) if code == Failure::SharedName.code() => {
            Outcome::Failed(SHARED_NAME)
        }
        Elevated::Exited(_) | Elevated::Failed(_) => Outcome::Failed(FAILED),
    };
    recheck();
    outcome
}

/// The errors the Stream page shows when the fix didn't work.
pub const FAILED: &str =
    "Windows didn't apply the whole fix. Run the EternalMonitor installer again.";
pub const NOT_CHANGED: &str = "Windows didn't let EternalMonitor add its firewall rule, so nothing was changed. Run the EternalMonitor installer again.";
pub const SHARED_NAME: &str = "A firewall rule blocking EternalMonitor shares its name with another rule, so it was left. Remove it in Windows Defender Firewall, or run the EternalMonitor installer again.";

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
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

    use super::{fix_rules, Failure, Named, RuleStore, ALLOW_NAME, ALLOW_PROFILES};
    use crate::firewall::rule_from;
    use crate::firewall::windows_policy::{for_each_rule, Apartment};

    /// UDP, as `NET_FW_IP_PROTOCOL`.
    const UDP: i32 = 17;

    struct Com(INetFwPolicy2);

    impl RuleStore for Com {
        fn rules(&mut self) -> Result<Vec<Named>, String> {
            let mut rules = Vec::new();
            for_each_rule(&self.0, |rule| {
                let name = unsafe { rule.Name()? }.to_string();
                let rule = rule_from(rule).map_err(|e| windows::core::Error::new(E_FAIL, e))?;
                rules.push(Named { name, rule });
                Ok(())
            })
            .map_err(|e| e.to_string())?;
            Ok(rules)
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
                    self.0.Rules()?.Add(&rule)
                }
            };
            add().map_err(|e| e.to_string())
        }

        fn remove(&mut self, name: &str) -> Result<(), String> {
            unsafe {
                self.0
                    .Rules()
                    .and_then(|rules| rules.Remove(&BSTR::from(name)))
            }
            .map_err(|e| e.to_string())
        }
    }

    /// The elevated copy's work: the exit code.
    pub fn fix(exe: &Path, port: u16) -> u32 {
        let run = || -> Result<(), Failure> {
            let _apartment = Apartment::new().map_err(|_| Failure::Read)?;
            let policy: INetFwPolicy2 =
                unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|_| Failure::Read)?;
            fix_rules(&mut Com(policy), exe, port)
        };
        run().map_or_else(Failure::code, |()| 0)
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
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    use super::{Elevate, Elevated};

    /// How long the copy may take.
    const WAIT_MS: u32 = 60_000;

    pub struct Shell;

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    impl Elevate for Shell {
        fn run_elevated(&self, exe: &Path, args: &str) -> Elevated {
            let verb = wide("runas".as_ref());
            let file = wide(exe.as_os_str());
            let params = wide(args.as_ref());
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_NOCLOSEPROCESS,
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
            let elevated = unsafe {
                if WaitForSingleObject(info.hProcess, WAIT_MS) != WAIT_OBJECT_0 {
                    Elevated::Failed("did not finish in time".into())
                } else {
                    let mut code = 0u32;
                    match GetExitCodeProcess(info.hProcess, &mut code) {
                        Ok(()) => Elevated::Exited(code),
                        Err(error) => Elevated::Failed(error.to_string()),
                    }
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

    /// What Windows adds when its prompt is dismissed.
    fn prompt_block(profiles: u32) -> Rule {
        Rule::unscoped(&EXE.to_lowercase(), false, profiles)
    }

    fn installer_allow() -> Rule {
        Rule::unscoped(EXE, true, ALLOW_PROFILES)
    }

    fn named(name: &str, rule: Rule) -> Named {
        Named {
            name: name.into(),
            rule: Some(rule),
        }
    }

    /// A firewall in memory. Removes go by name, as INetFwRules::Remove does.
    #[derive(Default)]
    struct Fake {
        rules: Vec<Named>,
        add_fails: bool,
        remove_fails: bool,
        read_fails: bool,
        calls: Vec<String>,
    }

    impl RuleStore for Fake {
        fn rules(&mut self) -> Result<Vec<Named>, String> {
            if self.read_fails {
                return Err("read".into());
            }
            Ok(self.rules.clone())
        }

        fn add_allow(&mut self, exe: &str) -> Result<(), String> {
            self.calls.push(format!("add {exe}"));
            if self.add_fails {
                return Err("add".into());
            }
            self.rules
                .push(named(ALLOW_NAME, Rule::unscoped(exe, true, ALLOW_PROFILES)));
            Ok(())
        }

        fn remove(&mut self, name: &str) -> Result<(), String> {
            self.calls.push(format!("remove {name}"));
            if self.remove_fails {
                return Err("remove".into());
            }
            self.rules.retain(|rule| rule.name != name);
            Ok(())
        }
    }

    const PROMPT_UDP: &str = "UDP Query User{0-1-2-3-4}eternalmonitor-host.exe";

    fn dismissed_prompt() -> Vec<Named> {
        vec![
            named(ALLOW_NAME, installer_allow()),
            named(PROMPT_UDP, prompt_block(PRIVATE | PUBLIC)),
            named(
                "TCP Query User{0-1-2-3-4}eternalmonitor-host.exe",
                Rule {
                    udp: false,
                    ..prompt_block(PRIVATE)
                },
            ),
            named(
                "Other",
                Rule::unscoped(r"C:\Tools\Other.exe", false, PUBLIC),
            ),
            Named {
                name: "Outbound".into(),
                rule: None,
            },
        ]
    }

    /// Windows' Block rule for the exe goes; its Allow rule, the TCP block
    /// that doesn't reach the stream, and other programs' rules stay.
    #[test]
    fn the_fix_removes_only_this_exes_block_and_keeps_the_rest() {
        let mut store = Fake {
            rules: dismissed_prompt(),
            ..Fake::default()
        };
        assert_eq!(fix_rules(&mut store, exe(), PORT), Ok(()));
        let mut expected = dismissed_prompt();
        expected.remove(1);
        assert_eq!(store.rules, expected);
        assert_eq!(store.calls, [format!("remove {PROMPT_UDP}")]);
    }

    /// No Allow rule at all (deleted, or never added): it comes first, as
    /// the installer adds it, then the block goes.
    #[test]
    fn a_missing_allow_rule_is_added_before_anything_is_removed() {
        let mut rules = dismissed_prompt();
        rules.remove(0);
        let mut store = Fake {
            rules,
            ..Fake::default()
        };
        assert_eq!(fix_rules(&mut store, exe(), PORT), Ok(()));
        assert_eq!(
            store.calls,
            [format!("add {EXE}"), format!("remove {PROMPT_UDP}")]
        );
        assert!(store.rules.contains(&named(ALLOW_NAME, installer_allow())));
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
        let before = rules.clone();
        let mut store = Fake {
            rules,
            add_fails: true,
            ..Fake::default()
        };
        assert_eq!(fix_rules(&mut store, exe(), PORT), Err(Failure::Add));
        assert_eq!(store.rules, before);
        assert_eq!(store.calls, [format!("add {EXE}")]);
    }

    /// A Block rule that can't be removed is a failure, not a success, and
    /// nothing that let traffic through went.
    #[test]
    fn a_failed_remove_is_a_failure() {
        let mut store = Fake {
            rules: dismissed_prompt(),
            remove_fails: true,
            ..Fake::default()
        };
        assert_eq!(fix_rules(&mut store, exe(), PORT), Err(Failure::Remove));
        assert_eq!(store.rules, dismissed_prompt());
    }

    /// A remove that reports success but leaves the block, or an add that
    /// leaves no Allow rule, is caught when the rules are read again.
    #[test]
    fn a_fix_that_did_not_take_is_a_failure() {
        struct Stubborn(Fake);
        impl RuleStore for Stubborn {
            fn rules(&mut self) -> Result<Vec<Named>, String> {
                self.0.rules()
            }
            fn add_allow(&mut self, exe: &str) -> Result<(), String> {
                self.0.add_allow(exe)
            }
            fn remove(&mut self, _name: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let mut store = Stubborn(Fake {
            rules: dismissed_prompt(),
            ..Fake::default()
        });
        assert_eq!(
            fix_rules(&mut store, exe(), PORT),
            Err(Failure::StillBlocked)
        );

        struct NoAdd(Fake);
        impl RuleStore for NoAdd {
            fn rules(&mut self) -> Result<Vec<Named>, String> {
                self.0.rules()
            }
            fn add_allow(&mut self, _exe: &str) -> Result<(), String> {
                Ok(())
            }
            fn remove(&mut self, name: &str) -> Result<(), String> {
                self.0.remove(name)
            }
        }
        let mut rules = dismissed_prompt();
        rules.remove(0);
        let mut store = NoAdd(Fake {
            rules,
            ..Fake::default()
        });
        assert_eq!(
            fix_rules(&mut store, exe(), PORT),
            Err(Failure::StillBlocked)
        );
    }

    /// A block whose name another rule has too is left: removing it by name
    /// could remove a rule that works.
    #[test]
    fn a_block_sharing_its_name_is_left() {
        let mut rules = dismissed_prompt();
        rules.push(named(
            PROMPT_UDP,
            Rule::unscoped(r"C:\Tools\Other.exe", true, PUBLIC),
        ));
        let before = rules.clone();
        let mut store = Fake {
            rules,
            ..Fake::default()
        };
        assert_eq!(fix_rules(&mut store, exe(), PORT), Err(Failure::SharedName));
        assert_eq!(store.rules, before);
    }

    /// A scoped Block rule, a disabled one, or one for another port is the
    /// user's own: left, as the verdict leaves it.
    #[test]
    fn blocks_the_verdict_does_not_count_are_left() {
        let rules = vec![
            named(ALLOW_NAME, installer_allow()),
            named(
                "Scoped",
                Rule {
                    remote_addresses: Some("10.0.0.0/8".into()),
                    ..prompt_block(PUBLIC)
                },
            ),
            named(
                "Off",
                Rule {
                    enabled: false,
                    ..prompt_block(PUBLIC)
                },
            ),
            named(
                "Port",
                Rule {
                    local_ports: Some("80".into()),
                    ..prompt_block(PUBLIC)
                },
            ),
        ];
        let mut store = Fake {
            rules: rules.clone(),
            ..Fake::default()
        };
        assert_eq!(fix_rules(&mut store, exe(), PORT), Ok(()));
        assert_eq!(store.rules, rules);
        assert!(store.calls.is_empty());
    }

    #[test]
    fn rules_that_cant_be_read_are_a_failure() {
        let mut store = Fake {
            read_fails: true,
            ..Fake::default()
        };
        assert_eq!(fix_rules(&mut store, exe(), PORT), Err(Failure::Read));
        assert!(store.calls.is_empty());
        assert_eq!(
            fix_rules(&mut Fake::default(), Path::new(""), PORT),
            Err(Failure::Arguments)
        );
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
            (Elevated::Exited(Failure::SharedName.code()), SHARED_NAME),
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
