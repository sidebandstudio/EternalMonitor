//! Whether Windows Firewall lets iPads reach this copy of the host.
//!
//! An iPad whose HELLO never arrives says "Your PC didn't answer" while the
//! PC says it is ready, and neither side could tell why. The usual blocks are
//! on the PC: an inbound Block rule for this exe, which Windows adds when its
//! "allow access" prompt is dismissed and which beats every Allow rule, or
//! "Blocks all incoming connections" on the network's profile. A background
//! thread reads the firewall policy at startup and every [`RECHECK`] and logs
//! the result when it changes.
//!
//! The Stream page warns only when the block is certain for the network an
//! iPad would use: every profile in use blocks, the PC has a home network
//! address, and no iPad is connected. A Block rule scoped to some addresses,
//! interfaces or a service, or a block on one of two networks, is only
//! logged.
//!
//! The log line names profiles and counts rules. It holds no path, rule name
//! or address.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

#[cfg(any(windows, test))]
use tracing::{info, warn};

/// How often the policy is read again: a network can turn Public, and a rule
/// can be added, while the app runs.
pub const RECHECK: Duration = Duration::from_secs(60);

/// The Windows Firewall profiles, as `NET_FW_PROFILE_TYPE2` bits.
pub const DOMAIN: u32 = 1;
pub const PRIVATE: u32 = 2;
pub const PUBLIC: u32 = 4;

/// One profile in use on a connected network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub bit: u32,
    pub on: bool,
    /// "Blocks all incoming connections, including those in the list of
    /// allowed apps".
    pub block_all_inbound: bool,
}

/// An inbound rule, as far as the verdict needs it. Scope fields hold what
/// Windows stores; `*` (or empty) means any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The program path as stored, which may use `%ProgramFiles%` and the like.
    pub program: String,
    pub allow: bool,
    pub enabled: bool,
    /// UDP or any protocol.
    pub udp: bool,
    pub profiles: u32,
    pub local_ports: String,
    pub remote_ports: String,
    pub local_addresses: String,
    pub remote_addresses: String,
    /// `All`, or some of `Wireless`, `Lan`, `RemoteAccess`.
    pub interface_types: String,
    /// The rule names particular interfaces.
    pub interfaces: bool,
    /// The rule is for a service, not every process of the program.
    pub service: String,
}

impl Rule {
    /// An inbound rule for `program` with no scope beyond its protocol.
    pub fn unscoped(program: &str, allow: bool, profiles: u32) -> Self {
        Self {
            program: program.into(),
            allow,
            enabled: true,
            udp: true,
            profiles,
            local_ports: "*".into(),
            remote_ports: "*".into(),
            local_addresses: "*".into(),
            remote_addresses: "*".into(),
            interface_types: "All".into(),
            interfaces: false,
            service: String::new(),
        }
    }
}

/// How far a rule reaches the stream's traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Every datagram an iPad sends to the stream port.
    All,
    /// Only some: it names addresses, ports, interfaces or a service.
    Some,
    /// None of it: another port.
    None,
}

/// Whether `ports` (`*`, or numbers and ranges such as `80,9000-9999`)
/// covers `port`. Keywords such as `RPC` cover no stream port.
pub fn covers(ports: &str, port: u16) -> bool {
    let ports = ports.trim();
    if ports.is_empty() || ports == "*" {
        return true;
    }
    ports.split(',').map(str::trim).any(|part| {
        let (low, high) = part.split_once('-').unwrap_or((part, part));
        match (low.trim().parse::<u16>(), high.trim().parse::<u16>()) {
            (Ok(low), Ok(high)) => (low..=high).contains(&port),
            _ => false,
        }
    })
}

fn any(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value == "*"
}

/// How far `rule` reaches datagrams to `port`.
pub fn reach(rule: &Rule, port: u16) -> Reach {
    if !covers(&rule.local_ports, port) {
        return Reach::None;
    }
    let unscoped = any(&rule.remote_ports)
        && any(&rule.local_addresses)
        && any(&rule.remote_addresses)
        && (any(&rule.interface_types) || rule.interface_types.trim().eq_ignore_ascii_case("all"))
        && !rule.interfaces
        && rule.service.trim().is_empty();
    if unscoped {
        Reach::All
    } else {
        Reach::Some
    }
}

/// What the firewall does on one profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// An Allow rule for this exe, and nothing certain that beats it.
    Allowed,
    /// An unscoped Block rule for this exe.
    Blocked,
    /// The profile blocks all incoming connections.
    BlocksAll,
    /// Neither rule exists: Windows asks, or blocks quietly if it may not.
    NoRule,
    /// Windows Firewall is off on this profile: another firewall may be on.
    Off,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Allowed => "allowed",
            Verdict::Blocked => "blocked",
            Verdict::BlocksAll => "blocks_all",
            Verdict::NoRule => "no_rule",
            Verdict::Off => "off",
        }
    }

    fn blocks(self) -> bool {
        matches!(self, Verdict::Blocked | Verdict::BlocksAll)
    }
}

/// What the firewall does to this exe's inbound UDP on each network in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Each profile in use, with its verdict.
    pub profiles: Vec<(u32, Verdict)>,
    pub allow_rules: usize,
    pub block_rules: usize,
    /// Block rules that reach only some of the stream's traffic.
    pub scoped_block_rules: usize,
    /// The PC has a home network address (see [`crate::network::Reach::Lan`]).
    pub lan: bool,
}

/// What the Stream page may say: a block certain on every network in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertainBlock {
    /// Block rules for this exe, and on `all_blocked` networks "Blocks all
    /// incoming connections" as well.
    Rule { all_blocked: Vec<&'static str> },
    /// Only "Blocks all incoming connections", on these networks.
    AllIncoming(Vec<&'static str>),
}

impl Report {
    /// The whole picture in one word for the log.
    pub fn verdict(&self) -> &'static str {
        let blocking = self.profiles.iter().filter(|(_, v)| v.blocks()).count();
        if !self.profiles.is_empty() && blocking == self.profiles.len() {
            "blocked"
        } else if blocking > 0 {
            "partly_blocked"
        } else if self.profiles.iter().all(|(_, v)| *v == Verdict::Off) {
            "off"
        } else if self.profiles.iter().any(|(_, v)| *v == Verdict::NoRule) {
            "no_rule"
        } else {
            "allowed"
        }
    }

    /// A block certain for the network an iPad would use: every profile in
    /// use blocks, and the PC has a home network address. Otherwise `None`,
    /// and the log line says what is known.
    pub fn certain_block(&self) -> Option<CertainBlock> {
        if !self.lan || self.profiles.is_empty() || !self.profiles.iter().all(|(_, v)| v.blocks()) {
            return None;
        }
        let all_blocked: Vec<_> = self
            .profiles
            .iter()
            .filter(|(_, v)| *v == Verdict::BlocksAll)
            .map(|(bit, _)| profile_name(*bit))
            .collect();
        Some(if all_blocked.len() == self.profiles.len() {
            CertainBlock::AllIncoming(all_blocked)
        } else {
            CertainBlock::Rule { all_blocked }
        })
    }

    /// The log line: profile names, verdicts and counts only.
    pub fn summary(&self) -> String {
        let profiles: Vec<String> = self
            .profiles
            .iter()
            .map(|(bit, verdict)| format!("{}:{}", profile_name(*bit), verdict.label()))
            .collect();
        format!(
            "Windows Firewall: profiles=[{}] allow_rules={} block_rules={} scoped_block_rules={} lan={} verdict={}",
            profiles.join(" "),
            self.allow_rules,
            self.block_rules,
            self.scoped_block_rules,
            self.lan,
            self.verdict(),
        )
    }
}

pub fn profile_name(bit: u32) -> &'static str {
    match bit {
        DOMAIN => "domain",
        PRIVATE => "private",
        PUBLIC => "public",
        _ => "other",
    }
}

/// `%NAME%` replaced with `lookup(NAME)`, as Windows stores a rule made in
/// its own window (`%ProgramFiles%\...`). An unknown name stays as written.
pub fn expand(path: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else {
            break;
        };
        let name = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match lookup(name).filter(|_| !name.is_empty()) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

/// Whether a rule's program is `exe`. Windows compares paths without case,
/// and its prompt stores them in lower case.
pub fn same_program(program: &str, exe: &Path, lookup: impl Fn(&str) -> Option<String>) -> bool {
    let plain = |path: &str| {
        path.trim()
            .trim_matches('"')
            .replace('/', "\\")
            .to_lowercase()
    };
    let exe = plain(&exe.to_string_lossy());
    let exe = exe.strip_prefix(r"\\?\").unwrap_or(&exe);
    !exe.is_empty() && plain(&expand(program, lookup)) == exe
}

/// The verdict on each profile for inbound UDP to `exe` on `port`. Windows
/// lets "block all" beat every rule and a Block rule beat every Allow rule;
/// only a Block rule that reaches all of the stream's traffic is a block.
pub fn judge(
    profiles: &[Profile],
    rules: &[Rule],
    exe: &Path,
    port: u16,
    lan: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> Report {
    let on: u32 = profiles.iter().filter(|p| p.on).map(|p| p.bit).sum();
    let ours: Vec<(&Rule, Reach)> = rules
        .iter()
        .filter(|r| r.enabled && r.udp && same_program(&r.program, exe, &lookup))
        .map(|r| (r, reach(r, port)))
        .filter(|(_, reach)| *reach != Reach::None)
        .collect();
    let count = |allow: bool, wanted: Reach, mask: u32| {
        ours.iter()
            .filter(|(r, reach)| {
                r.allow == allow && r.profiles & mask != 0 && (allow || *reach == wanted)
            })
            .count()
    };
    let verdicts = profiles
        .iter()
        .map(|p| {
            let verdict = if !p.on {
                Verdict::Off
            } else if p.block_all_inbound {
                Verdict::BlocksAll
            } else if count(false, Reach::All, p.bit) > 0 {
                Verdict::Blocked
            } else if count(true, Reach::All, p.bit) > 0 {
                Verdict::Allowed
            } else {
                Verdict::NoRule
            };
            (p.bit, verdict)
        })
        .collect();
    Report {
        profiles: verdicts,
        allow_rules: count(true, Reach::All, on),
        block_rules: count(false, Reach::All, on),
        scoped_block_rules: count(false, Reach::Some, on),
        lan,
    }
}

static LATEST: Mutex<Option<Report>> = Mutex::new(None);

/// The last reading, if the last read worked. `None` off Windows.
pub fn latest() -> Option<Report> {
    LATEST.lock().ok()?.clone()
}

/// Takes one read's result: stores and logs a report when it changes, and
/// clears it when the read failed, so an old block never stays on screen.
/// The first failure in a row is a warning.
#[cfg(any(windows, test))]
fn observe(read: Result<Report, String>, failed: &mut bool) {
    let Ok(mut latest) = LATEST.lock() else {
        return;
    };
    match read {
        Ok(report) => {
            *failed = false;
            if latest.as_ref() == Some(&report) {
                return;
            }
            let line = report.summary();
            if report.verdict() == "allowed" || report.verdict() == "no_rule" {
                info!("{line}");
            } else {
                warn!("{line}");
            }
            *latest = Some(report);
        }
        Err(error) => {
            *latest = None;
            if !std::mem::replace(failed, true) {
                warn!(%error, "Could not read the Windows Firewall policy");
            }
        }
    }
}

/// Reads the policy for the stream's `port` now and every [`RECHECK`] on
/// its own thread.
pub fn watch(port: u16) {
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("firewall".into())
            .spawn(move || {
                let mut failed = false;
                loop {
                    let lan = crate::network::adapters().is_some_and(|adapters| {
                        crate::network::rank(&adapters)
                            .iter()
                            .any(|address| address.reach == crate::network::Reach::Lan)
                    });
                    let read = windows_policy::read(&exe, port, lan).map_err(|e| e.to_string());
                    observe(read, &mut failed);
                    std::thread::sleep(RECHECK);
                }
            });
    }
    #[cfg(not(windows))]
    let _ = port;
}

#[cfg(windows)]
mod windows_policy {
    use std::path::Path;

    use windows::core::{IUnknown, Interface, VARIANT};
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        INetFwPolicy2, INetFwRule, NetFwPolicy2, NET_FW_ACTION_ALLOW, NET_FW_IP_PROTOCOL_ANY,
        NET_FW_IP_PROTOCOL_UDP, NET_FW_PROFILE_TYPE2, NET_FW_RULE_DIR_IN,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Ole::IEnumVARIANT;

    use super::{judge, Profile, Report, Rule, DOMAIN, PRIVATE, PUBLIC};

    /// VT_DISPATCH: what the rule enumerator hands out.
    const VT_DISPATCH: u16 = 9;

    struct Apartment;

    impl Apartment {
        fn new() -> windows::core::Result<Self> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
            Ok(Self)
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    pub fn read(exe: &Path, port: u16, lan: bool) -> windows::core::Result<Report> {
        let _apartment = Apartment::new()?;
        let policy: INetFwPolicy2 =
            unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)? };
        let current = unsafe { policy.CurrentProfileTypes()? } as u32;
        let mut profiles = Vec::new();
        for bit in [DOMAIN, PRIVATE, PUBLIC] {
            if current & bit == 0 {
                continue;
            }
            let profile = NET_FW_PROFILE_TYPE2(bit as i32);
            profiles.push(Profile {
                bit,
                on: unsafe { policy.get_FirewallEnabled(profile)? }.as_bool(),
                block_all_inbound: unsafe { policy.get_BlockAllInboundTraffic(profile)? }.as_bool(),
            });
        }
        let rules = inbound_rules(&policy)?;
        Ok(judge(&profiles, &rules, exe, port, lan, |name| {
            std::env::var(name).ok()
        }))
    }

    /// Every inbound rule that names a program.
    fn inbound_rules(policy: &INetFwPolicy2) -> windows::core::Result<Vec<Rule>> {
        let all = unsafe { policy.Rules()? };
        let each: IEnumVARIANT = unsafe { all._NewEnum()? }.cast()?;
        let mut rules = Vec::new();
        loop {
            let mut item = [VARIANT::default()];
            let mut fetched = 0;
            unsafe { each.Next(&mut item, &mut fetched).ok()? };
            if fetched == 0 {
                break;
            }
            let raw = item[0].as_raw();
            let (vt, pointer) = unsafe {
                (
                    raw.Anonymous.Anonymous.vt,
                    raw.Anonymous.Anonymous.Anonymous.pdispVal,
                )
            };
            if vt != VT_DISPATCH {
                continue;
            }
            // IDispatch is an IUnknown; the VARIANT keeps its reference.
            let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&pointer) }) else {
                continue;
            };
            let Ok(rule) = unknown.cast::<INetFwRule>() else {
                continue;
            };
            if unsafe { rule.Direction()? } != NET_FW_RULE_DIR_IN {
                continue;
            }
            let text = |value: windows::core::Result<windows::core::BSTR>| {
                value.map(|v| v.to_string()).unwrap_or_default()
            };
            let program = text(unsafe { rule.ApplicationName() });
            if program.is_empty() {
                continue;
            }
            let protocol = unsafe { rule.Protocol()? };
            rules.push(Rule {
                program,
                allow: unsafe { rule.Action()? } == NET_FW_ACTION_ALLOW,
                enabled: unsafe { rule.Enabled()? }.as_bool(),
                udp: protocol == NET_FW_IP_PROTOCOL_UDP.0 || protocol == NET_FW_IP_PROTOCOL_ANY.0,
                profiles: unsafe { rule.Profiles()? } as u32,
                local_ports: text(unsafe { rule.LocalPorts() }),
                remote_ports: text(unsafe { rule.RemotePorts() }),
                local_addresses: text(unsafe { rule.LocalAddresses() }),
                remote_addresses: text(unsafe { rule.RemoteAddresses() }),
                interface_types: text(unsafe { rule.InterfaceTypes() }),
                // A rule read back without its list counts as scoped: never
                // a certain block.
                interfaces: unsafe { rule.Interfaces() }.map_or(true, |list| !list.is_empty()),
                service: text(unsafe { rule.ServiceName() }),
            });
        }
        Ok(rules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const EXE: &str = r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe";
    const PORT: u16 = 9876;

    fn env(name: &str) -> Option<String> {
        match name.to_lowercase().as_str() {
            "programfiles" => Some(r"C:\Program Files".into()),
            _ => None,
        }
    }

    fn on(bit: u32) -> Profile {
        Profile {
            bit,
            on: true,
            block_all_inbound: false,
        }
    }

    /// The installer's rule: all profiles, UDP.
    fn installer_rule() -> Rule {
        Rule::unscoped(EXE, true, 0x7fff_ffff)
    }

    /// What Windows adds when its prompt is dismissed: lower case, the
    /// profiles in use then, no other scope.
    fn prompt_block(profiles: u32) -> Rule {
        Rule::unscoped(&EXE.to_lowercase(), false, profiles)
    }

    fn report(profiles: &[Profile], rules: &[Rule]) -> Report {
        judge(profiles, rules, &PathBuf::from(EXE), PORT, true, env)
    }

    fn verdicts(profiles: &[Profile], rules: &[Rule]) -> Vec<Verdict> {
        report(profiles, rules)
            .profiles
            .iter()
            .map(|(_, v)| *v)
            .collect()
    }

    #[test]
    fn the_installer_rule_allows_on_every_network() {
        for bit in [DOMAIN, PRIVATE, PUBLIC] {
            let report = report(&[on(bit)], &[installer_rule()]);
            assert_eq!(report.profiles, [(bit, Verdict::Allowed)]);
            assert_eq!(report.verdict(), "allowed");
            assert_eq!(report.certain_block(), None);
        }
    }

    #[test]
    fn a_dismissed_prompt_is_certain_on_the_only_network() {
        let report = report(&[on(PUBLIC)], &[installer_rule(), prompt_block(PUBLIC)]);
        assert_eq!(report.profiles, [(PUBLIC, Verdict::Blocked)]);
        assert_eq!(report.verdict(), "blocked");
        assert_eq!(
            report.certain_block(),
            Some(CertainBlock::Rule {
                all_blocked: vec![]
            })
        );
        // Another network, the same rules: nothing blocks there.
        let private =
            super::tests::report(&[on(PRIVATE)], &[installer_rule(), prompt_block(PUBLIC)]);
        assert_eq!(private.profiles, [(PRIVATE, Verdict::Allowed)]);
    }

    /// Ethernet on a private network, Wi-Fi on a public one: which an iPad
    /// uses isn't known, so the block is logged per network, not shown.
    #[test]
    fn a_block_on_one_of_two_networks_is_logged_not_certain() {
        let report = report(
            &[on(PRIVATE), on(PUBLIC)],
            &[installer_rule(), prompt_block(PUBLIC)],
        );
        assert_eq!(
            report.profiles,
            [(PRIVATE, Verdict::Allowed), (PUBLIC, Verdict::Blocked)]
        );
        assert_eq!(report.verdict(), "partly_blocked");
        assert_eq!(report.certain_block(), None);
        // Blocked on both: certain again.
        let both = super::tests::report(
            &[on(PRIVATE), on(PUBLIC)],
            &[installer_rule(), prompt_block(PRIVATE | PUBLIC)],
        );
        assert!(both.certain_block().is_some());
    }

    #[test]
    fn blocking_all_incoming_connections_beats_every_rule() {
        let shields = Profile {
            block_all_inbound: true,
            ..on(PUBLIC)
        };
        let alone = report(&[shields], &[installer_rule()]);
        assert_eq!(alone.profiles, [(PUBLIC, Verdict::BlocksAll)]);
        assert_eq!(
            alone.certain_block(),
            Some(CertainBlock::AllIncoming(vec!["public"]))
        );
        // A private network that lets iPads in: not certain.
        let mixed = report(&[on(PRIVATE), shields], &[installer_rule()]);
        assert_eq!(mixed.certain_block(), None);
        // Both networks blocked, one by a rule and one by blocking all.
        let both = report(&[on(PRIVATE), shields], &[prompt_block(PRIVATE)]);
        assert_eq!(
            both.certain_block(),
            Some(CertainBlock::Rule {
                all_blocked: vec!["public"]
            })
        );
    }

    #[test]
    fn a_block_for_another_port_does_not_count() {
        for ports in ["443", "1-1024,9877", "RPC", "9000-9875"] {
            let rule = Rule {
                local_ports: ports.into(),
                ..prompt_block(PUBLIC)
            };
            let report = report(&[on(PUBLIC)], &[installer_rule(), rule]);
            assert_eq!(report.profiles, [(PUBLIC, Verdict::Allowed)], "{ports}");
            assert_eq!((report.block_rules, report.scoped_block_rules), (0, 0));
        }
        for ports in ["*", "", "9876", "53, 9000-9999"] {
            let rule = Rule {
                local_ports: ports.into(),
                ..prompt_block(PUBLIC)
            };
            assert_eq!(
                verdicts(&[on(PUBLIC)], &[rule]),
                [Verdict::Blocked],
                "{ports}"
            );
        }
        // The port the stream really listens on decides.
        let rule = Rule {
            local_ports: "19876".into(),
            ..prompt_block(PUBLIC)
        };
        let moved = judge(
            &[on(PUBLIC)],
            &[rule],
            &PathBuf::from(EXE),
            19876,
            true,
            env,
        );
        assert_eq!(moved.profiles, [(PUBLIC, Verdict::Blocked)]);
    }

    #[test]
    fn a_scoped_block_is_logged_not_certain() {
        let scoped = [
            Rule {
                remote_addresses: "203.0.113.0/24".into(),
                ..prompt_block(PUBLIC)
            },
            Rule {
                remote_addresses: "LocalSubnet".into(),
                ..prompt_block(PUBLIC)
            },
            Rule {
                local_addresses: "10.0.0.5".into(),
                ..prompt_block(PUBLIC)
            },
            Rule {
                remote_ports: "5000".into(),
                ..prompt_block(PUBLIC)
            },
            Rule {
                interface_types: "RemoteAccess".into(),
                ..prompt_block(PUBLIC)
            },
            Rule {
                interfaces: true,
                ..prompt_block(PUBLIC)
            },
            Rule {
                service: "SomeService".into(),
                ..prompt_block(PUBLIC)
            },
        ];
        for rule in scoped {
            let report = report(&[on(PUBLIC)], &[installer_rule(), rule.clone()]);
            assert_eq!(report.profiles, [(PUBLIC, Verdict::Allowed)], "{rule:?}");
            assert_eq!((report.block_rules, report.scoped_block_rules), (0, 1));
            assert_eq!(report.certain_block(), None);
            assert!(report.summary().contains("scoped_block_rules=1"));
        }
        let all_types = Rule {
            interface_types: "All".into(),
            ..prompt_block(PUBLIC)
        };
        assert_eq!(verdicts(&[on(PUBLIC)], &[all_types]), [Verdict::Blocked]);
    }

    #[test]
    fn without_a_home_network_address_no_block_is_certain() {
        let report = judge(
            &[on(PUBLIC)],
            &[prompt_block(PUBLIC)],
            &PathBuf::from(EXE),
            PORT,
            false,
            env,
        );
        assert_eq!(report.verdict(), "blocked");
        assert_eq!(report.certain_block(), None);
    }

    #[test]
    fn rules_that_do_not_apply_are_not_counted() {
        let tcp = Rule {
            udp: false,
            ..prompt_block(PUBLIC)
        };
        let disabled = Rule {
            enabled: false,
            ..prompt_block(PUBLIC)
        };
        let other_exe = Rule::unscoped(r"C:\Tools\EternalMonitor-host.exe", false, PUBLIC);
        let report = report(
            &[on(PUBLIC)],
            &[
                installer_rule(),
                tcp,
                disabled,
                other_exe,
                prompt_block(PRIVATE),
            ],
        );
        assert_eq!((report.allow_rules, report.block_rules), (1, 0));
        assert_eq!(report.profiles, [(PUBLIC, Verdict::Allowed)]);
    }

    #[test]
    fn no_rule_and_firewall_off_are_told_apart() {
        assert_eq!(report(&[on(PRIVATE)], &[]).verdict(), "no_rule");
        let off = Profile {
            on: false,
            ..on(PRIVATE)
        };
        let report_off = report(&[off], &[prompt_block(PRIVATE)]);
        assert_eq!(report_off.verdict(), "off");
        assert_eq!(report_off.certain_block(), None);
        // A network whose firewall is off lets iPads in: not certain.
        let half = report(&[off, on(PUBLIC)], &[prompt_block(PUBLIC)]);
        assert_eq!(
            half.profiles,
            [(PRIVATE, Verdict::Off), (PUBLIC, Verdict::Blocked)]
        );
        assert_eq!(half.certain_block(), None);
    }

    #[test]
    fn a_rule_made_in_windows_security_matches_through_its_variables() {
        let ui_rule = Rule::unscoped(
            r"%ProgramFiles%\EternalMonitor\EternalMonitor-host.exe",
            true,
            0x7fff_ffff,
        );
        assert_eq!(verdicts(&[on(PRIVATE)], &[ui_rule]), [Verdict::Allowed]);
        assert!(same_program(
            "\"c:/program files/eternalmonitor/eternalmonitor-host.exe\"",
            Path::new(EXE),
            env
        ));
        assert!(same_program(EXE, Path::new(&format!(r"\\?\{EXE}")), env));
        assert!(!same_program("", Path::new(EXE), env));
    }

    #[test]
    fn expansion_leaves_unknown_and_unpaired_names() {
        assert_eq!(expand(r"%nope%\a%", env), r"%nope%\a%");
        assert_eq!(expand(r"%%\%ProgramFiles%", env), r"%%\C:\Program Files");
    }

    /// A read that fails drops the last reading, so a block that is gone
    /// can't stay on the Stream page.
    #[test]
    fn a_failed_read_clears_the_last_reading() {
        let blocked = report(&[on(PUBLIC)], &[prompt_block(PUBLIC)]);
        let mut failed = false;
        observe(Ok(blocked.clone()), &mut failed);
        assert_eq!(latest(), Some(blocked.clone()));
        observe(Err("RPC server unavailable".into()), &mut failed);
        assert!(failed);
        assert_eq!(latest(), None);
        observe(Err("again".into()), &mut failed);
        assert_eq!(latest(), None);
        observe(Ok(blocked.clone()), &mut failed);
        assert!(!failed);
        assert_eq!(latest(), Some(blocked));
    }

    /// `key=value` or `key="value with spaces"` from a netsh command line.
    fn netsh_arg(params: &str, key: &str) -> Option<String> {
        let mut rest = params;
        while let Some(start) = rest.find(&format!(" {key}=")) {
            let value = &rest[start + key.len() + 2..];
            if let Some(quoted) = value.strip_prefix('"') {
                return quoted.split('"').next().map(str::to_string);
            }
            let value = value.split(' ').next().unwrap_or_default();
            if !value.is_empty() {
                return Some(value.to_string());
            }
            rest = &rest[start + 1..];
        }
        None
    }

    /// What the firewall holds after Setup runs the netsh lines of its
    /// [Run] section, in order, into `app`. Every rule here is inbound.
    fn after_setup(mut rules: Vec<(String, Rule)>, app: &str) -> Vec<Rule> {
        let iss = include_str!("../../installer/EternalMonitor.iss");
        let run = iss
            .split("\n[Run]")
            .nth(1)
            .and_then(|section| section.split("\n[").next())
            .expect("EternalMonitor.iss has a [Run] section");
        for line in run.lines().filter(|line| line.contains(r"\netsh.exe")) {
            let params = line
                .split("Parameters: \"")
                .nth(1)
                .and_then(|rest| rest.split("\"; ").next())
                .expect("a netsh line has Parameters")
                .replace("\"\"", "\"")
                .replace("{app}", app);
            let arg = |key| netsh_arg(&params, key);
            let name = arg("name").expect("a netsh rule line names its rule");
            if params.starts_with("advfirewall firewall delete rule ") {
                match arg("program") {
                    Some(program) if name == "all" => {
                        assert_ne!(arg("dir").as_deref(), Some("out"));
                        rules.retain(|(_, rule)| {
                            !same_program(&rule.program, Path::new(&program), env)
                        });
                    }
                    _ => rules.retain(|(named, _)| *named != name),
                }
            } else if params.starts_with("advfirewall firewall add rule ") {
                assert_eq!(arg("dir").as_deref(), Some("in"), "{params}");
                let profiles = arg("profile").unwrap_or_default();
                let bits = [("domain", DOMAIN), ("private", PRIVATE), ("public", PUBLIC)];
                rules.push((
                    name,
                    Rule {
                        enabled: arg("enable").as_deref() != Some("no"),
                        udp: arg("protocol").as_deref() == Some("UDP"),
                        ..Rule::unscoped(
                            &arg("program").unwrap_or_default(),
                            arg("action").as_deref() == Some("allow"),
                            bits.iter()
                                .filter(|(word, _)| profiles.split(',').any(|p| p == *word))
                                .map(|(_, bit)| bit)
                                .sum(),
                        )
                    },
                ));
            } else {
                panic!("unexpected netsh line in [Run]: {params}");
            }
        }
        rules.into_iter().map(|(_, rule)| rule).collect()
    }

    #[test]
    fn running_setup_again_removes_a_dismissed_prompts_block() {
        let app = r"C:\Program Files\EternalMonitor";
        let other = Rule::unscoped(r"C:\Tools\Other.exe", false, PUBLIC);
        let before = vec![
            ("EternalMonitor Host UDP".to_string(), installer_rule()),
            (
                format!("UDP Query User{{0-1-2-3-4}}{}", EXE.to_lowercase()),
                prompt_block(PRIVATE | PUBLIC),
            ),
            ("Other".to_string(), other.clone()),
        ];
        let rules: Vec<Rule> = before.iter().map(|(_, rule)| rule.clone()).collect();
        assert_eq!(verdicts(&[on(PUBLIC)], &rules), [Verdict::Blocked]);

        let after = after_setup(before, app);
        assert_eq!(
            verdicts(&[on(PUBLIC)], &after),
            [Verdict::Allowed],
            "{after:?}"
        );
        assert_eq!(verdicts(&[on(PRIVATE)], &after), [Verdict::Allowed]);
        assert!(
            after.contains(&other),
            "Setup removed another program's rule"
        );
        // A first install: nothing to remove, the Allow rule still comes.
        assert_eq!(
            verdicts(&[on(DOMAIN)], &after_setup(Vec::new(), app)),
            [Verdict::Allowed]
        );
    }

    #[test]
    fn the_log_line_names_no_path_or_rule() {
        let scoped = Rule {
            remote_addresses: "192.168.1.0/24".into(),
            ..prompt_block(PRIVATE)
        };
        let report = report(
            &[on(PRIVATE), on(PUBLIC)],
            &[installer_rule(), prompt_block(PUBLIC), scoped],
        );
        assert_eq!(
            report.summary(),
            "Windows Firewall: profiles=[private:allowed public:blocked] allow_rules=1 \
             block_rules=1 scoped_block_rules=1 lan=true verdict=partly_blocked"
        );
        let line = format!("2026-11-05T12:00:00.000000Z  WARN {}", report.summary());
        assert_eq!(crate::telemetry::uploaded(&line), line);
    }
}
