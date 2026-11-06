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
//! iPad would use, and only for what Setup or one switch removes: this
//! exe's own unscoped Block rule, or "Blocks all incoming connections", on
//! every profile in use, with a home network address and no iPad connected.
//! Everything else is only logged: a Block rule for every program or one
//! scoped to some addresses, interfaces, a service, a package, users or
//! machines, a block on one of two networks, and any block an Allow rule
//! asking for IPsec might let traffic past.
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

/// An inbound rule, as far as the verdict needs it. Text fields hold what
/// Windows stores (`*` or empty means any); `None` is a field that could not
/// be read, which the verdict treats as unknown, never as any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The program path as stored, which may use `%ProgramFiles%` and the
    /// like. Empty: the rule is for every program.
    pub program: Option<String>,
    pub allow: bool,
    pub enabled: bool,
    /// UDP or any protocol.
    pub udp: bool,
    pub profiles: u32,
    pub local_ports: Option<String>,
    pub remote_ports: Option<String>,
    pub local_addresses: Option<String>,
    pub remote_addresses: Option<String>,
    /// `All`, or some of `Wireless`, `Lan`, `RemoteAccess`.
    pub interface_types: Option<String>,
    /// The rule names particular interfaces.
    pub interfaces: Option<bool>,
    /// The rule is for a service, not every process of the program.
    pub service: Option<String>,
    /// INetFwRule3's restrictions: an app package, a user owning the
    /// process, and users or machines allowed to connect. Any of them limits
    /// whose traffic the rule is for.
    pub package: Option<String>,
    pub local_user_owner: Option<String>,
    pub local_users: Option<String>,
    pub remote_users: Option<String>,
    pub remote_machines: Option<String>,
    /// `NET_FW_AUTHENTICATE_TYPE`: 0 is none; anything else asks for IPsec.
    pub secure_flags: Option<i32>,
}

impl Rule {
    /// An inbound rule for `program` (empty: every program) with no scope
    /// beyond its protocol.
    pub fn unscoped(program: &str, allow: bool, profiles: u32) -> Self {
        Self {
            program: Some(program.into()),
            allow,
            enabled: true,
            udp: true,
            profiles,
            local_ports: Some("*".into()),
            remote_ports: Some("*".into()),
            local_addresses: Some("*".into()),
            remote_addresses: Some("*".into()),
            interface_types: Some("All".into()),
            interfaces: Some(false),
            service: Some(String::new()),
            package: Some(String::new()),
            local_user_owner: Some(String::new()),
            local_users: Some(String::new()),
            remote_users: Some(String::new()),
            remote_machines: Some(String::new()),
            secure_flags: Some(0),
        }
    }
}

/// The text fields of a firewall rule, read one by one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Program,
    LocalPorts,
    RemotePorts,
    LocalAddresses,
    RemoteAddresses,
    InterfaceTypes,
    Service,
    /// INetFwRule3 from here on.
    Package,
    LocalUserOwner,
    LocalUsers,
    RemoteUsers,
    RemoteMachines,
}

/// A rule as Windows hands it out. Each getter can fail on its own.
pub trait RuleSource {
    fn inbound(&self) -> Result<bool, String>;
    fn allow(&self) -> Result<bool, String>;
    fn enabled(&self) -> Result<bool, String>;
    /// `NET_FW_IP_PROTOCOL`: 17 is UDP, 256 any.
    fn protocol(&self) -> Result<i32, String>;
    fn profiles(&self) -> Result<i32, String>;
    fn text(&self, field: Field) -> Result<String, String>;
    /// Whether the rule names particular interfaces.
    fn interfaces_listed(&self) -> Result<bool, String>;
    /// INetFwRule3's `SecureFlags`.
    fn secure_flags(&self) -> Result<i32, String>;
}

/// The inbound rule `source` describes, or `None` for an outbound one. A
/// failed direction, action, state, protocol or profile read fails the
/// whole reading; a text field that can't be read stays unknown.
pub fn rule_from(source: &impl RuleSource) -> Result<Option<Rule>, String> {
    if !source.inbound()? {
        return Ok(None);
    }
    let text = |field| source.text(field).ok();
    let protocol = source.protocol()?;
    Ok(Some(Rule {
        program: text(Field::Program),
        allow: source.allow()?,
        enabled: source.enabled()?,
        udp: protocol == 17 || protocol == 256,
        profiles: source.profiles()? as u32,
        local_ports: text(Field::LocalPorts),
        remote_ports: text(Field::RemotePorts),
        local_addresses: text(Field::LocalAddresses),
        remote_addresses: text(Field::RemoteAddresses),
        interface_types: text(Field::InterfaceTypes),
        interfaces: source.interfaces_listed().ok(),
        service: text(Field::Service),
        package: text(Field::Package),
        local_user_owner: text(Field::LocalUserOwner),
        local_users: text(Field::LocalUsers),
        remote_users: text(Field::RemoteUsers),
        remote_machines: text(Field::RemoteMachines),
        secure_flags: source.secure_flags().ok(),
    }))
}

/// How far a rule reaches the stream's traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Every datagram an iPad sends to the stream port.
    All,
    /// Only some, or unknown: it names addresses, ports, interfaces, a
    /// service, a package, users or machines, asks for IPsec, or a field of
    /// it could not be read.
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

/// Read, and any: an unknown field is not.
fn any(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| value.is_empty() || value == "*")
}

/// How far `rule` reaches datagrams to `port`.
pub fn reach(rule: &Rule, port: u16) -> Reach {
    if rule
        .local_ports
        .as_deref()
        .is_some_and(|ports| !covers(ports, port))
    {
        return Reach::None;
    }
    let all_types = rule
        .interface_types
        .as_deref()
        .map(str::trim)
        .is_some_and(|types| types.is_empty() || types.eq_ignore_ascii_case("all"));
    // Ports that cover the stream's are no limit; ports never read are.
    let unscoped = rule.local_ports.is_some()
        && any(&rule.remote_ports)
        && any(&rule.local_addresses)
        && any(&rule.remote_addresses)
        && all_types
        && rule.interfaces == Some(false)
        && [
            &rule.service,
            &rule.package,
            &rule.local_user_owner,
            &rule.local_users,
            &rule.remote_users,
            &rule.remote_machines,
        ]
        .iter()
        .all(|value| value.as_deref().is_some_and(|v| v.trim().is_empty()))
        && rule.secure_flags == Some(0);
    if unscoped {
        Reach::All
    } else {
        Reach::Some
    }
}

/// Whose traffic a rule is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Whose {
    /// This exe.
    Ours,
    /// Every program: Setup's program-specific delete can't remove it.
    AllPrograms,
    /// The program could not be read.
    Unknown,
    Other,
}

fn whose(rule: &Rule, exe: &Path, lookup: impl Fn(&str) -> Option<String>) -> Whose {
    match rule.program.as_deref() {
        None => Whose::Unknown,
        Some(program) if program.trim().is_empty() => Whose::AllPrograms,
        Some(program) if same_program(program, exe, lookup) => Whose::Ours,
        Some(_) => Whose::Other,
    }
}

/// What the firewall does on one profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// An Allow rule for this exe or every program, and nothing certain
    /// that beats it.
    Allowed,
    /// An unscoped Block rule for this exe.
    Blocked,
    /// An unscoped Block rule for every program on the stream's port.
    BlockedForAllApps,
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
            Verdict::BlockedForAllApps => "blocked_all_programs",
            Verdict::BlocksAll => "blocks_all",
            Verdict::NoRule => "no_rule",
            Verdict::Off => "off",
        }
    }

    fn blocks(self) -> bool {
        matches!(
            self,
            Verdict::Blocked | Verdict::BlockedForAllApps | Verdict::BlocksAll
        )
    }
}

/// What the firewall does to this exe's inbound UDP on each network in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Each profile in use, with its verdict.
    pub profiles: Vec<(u32, Verdict)>,
    /// The stream's port.
    pub port: u16,
    pub allow_rules: usize,
    /// Unscoped Block rules for this exe.
    pub block_rules: usize,
    /// Unscoped Block rules for every program that cover the stream's port.
    pub all_programs_block_rules: usize,
    /// Block rules that reach only some of the stream's traffic, or that
    /// could not be read whole.
    pub scoped_block_rules: usize,
    /// Allow rules for this exe or every program that may let IPsec
    /// traffic past Block rules (see [`judge`]).
    pub bypass_rules: usize,
    /// The PC has a home network address (see [`crate::network::Reach::Lan`]).
    pub lan: bool,
}

/// What the Stream page may say: a block certain on every network in use,
/// and one Setup or a single switch removes. A rule for every program is
/// only logged.
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
        if blocking > 0 && self.bypass_rules > 0 {
            "uncertain"
        } else if !self.profiles.is_empty() && blocking == self.profiles.len() {
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

    /// A block certain for the network an iPad would use, and one Setup or
    /// a single switch removes: every profile in use is blocked by this
    /// exe's own rule or blocks all incoming connections, the PC has a home
    /// network address, and no Allow rule may let traffic past. Otherwise
    /// `None`, and the log line says what is known.
    pub fn certain_block(&self) -> Option<CertainBlock> {
        let fixable = |v: &Verdict| matches!(v, Verdict::Blocked | Verdict::BlocksAll);
        if !self.lan
            || self.bypass_rules > 0
            || self.profiles.is_empty()
            || !self.profiles.iter().all(|(_, v)| fixable(v))
        {
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
            "Windows Firewall: profiles=[{}] allow_rules={} block_rules={} all_programs_block_rules={} scoped_block_rules={} bypass_rules={} lan={} verdict={}",
            profiles.join(" "),
            self.allow_rules,
            self.block_rules,
            self.all_programs_block_rules,
            self.scoped_block_rules,
            self.bypass_rules,
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
/// lets "block all" beat every rule and a Block rule beat every Allow rule.
/// Only a Block rule, for this exe or every program, that reaches all of
/// the stream's traffic and was read whole is a block.
pub fn judge(
    profiles: &[Profile],
    rules: &[Rule],
    exe: &Path,
    port: u16,
    lan: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> Report {
    let on: u32 = profiles.iter().filter(|p| p.on).map(|p| p.bit).sum();
    let relevant: Vec<(&Rule, Whose, Reach)> = rules
        .iter()
        .filter(|r| r.enabled && r.udp)
        .map(|r| (r, whose(r, exe, &lookup), reach(r, port)))
        .filter(|(_, whose, reach)| *whose != Whose::Other && *reach != Reach::None)
        .collect();
    let count = |mask: u32, wanted: &dyn Fn(&Rule, Whose, Reach) -> bool| {
        relevant
            .iter()
            .filter(|(r, whose, reach)| r.profiles & mask != 0 && wanted(r, *whose, *reach))
            .count()
    };
    // Windows has many Allow rules for every program, each limited to a
    // service or some addresses: only an unscoped one lets any app in.
    let allows = |r: &Rule, whose, reach| {
        r.allow && (whose == Whose::Ours || (whose == Whose::AllPrograms && reach == Reach::All))
    };
    let ours = |r: &Rule, whose, reach| !r.allow && whose == Whose::Ours && reach == Reach::All;
    let everyone =
        |r: &Rule, whose, reach| !r.allow && whose == Whose::AllPrograms && reach == Reach::All;
    let unsure =
        |r: &Rule, whose, reach| !r.allow && (whose == Whose::Unknown || reach == Reach::Some);
    // An Allow rule that asks for IPsec (SecureFlags) can come with
    // "Override block rules": IPsec-authenticated traffic then gets past
    // Block rules for the exe. iPads never authenticate, so it changes
    // nothing for them, but the COM API doesn't say whether the override is
    // set: any such rule, or one whose flags couldn't be read, makes a block
    // uncertain. Protocol and port aren't looked at: fewer warnings, never
    // a wrong one.
    let bypass_rules = rules
        .iter()
        .filter(|r| {
            r.enabled
                && r.allow
                && r.secure_flags != Some(0)
                && matches!(whose(r, exe, &lookup), Whose::Ours | Whose::AllPrograms)
        })
        .count();
    let verdicts = profiles
        .iter()
        .map(|p| {
            let verdict = if !p.on {
                Verdict::Off
            } else if p.block_all_inbound {
                Verdict::BlocksAll
            } else if count(p.bit, &everyone) > 0 {
                Verdict::BlockedForAllApps
            } else if count(p.bit, &ours) > 0 {
                Verdict::Blocked
            } else if count(p.bit, &allows) > 0 {
                Verdict::Allowed
            } else {
                Verdict::NoRule
            };
            (p.bit, verdict)
        })
        .collect();
    Report {
        profiles: verdicts,
        port,
        allow_rules: count(on, &allows),
        block_rules: count(on, &ours),
        all_programs_block_rules: count(on, &everyone),
        scoped_block_rules: count(on, &unsure),
        bypass_rules,
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
    use windows::Win32::Foundation::E_FAIL;
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        INetFwPolicy2, INetFwRule, INetFwRule3, NetFwPolicy2, NET_FW_ACTION_ALLOW,
        NET_FW_PROFILE_TYPE2, NET_FW_RULE_DIR_IN,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Ole::IEnumVARIANT;

    use super::{
        judge, rule_from, Field, Profile, Report, Rule, RuleSource, DOMAIN, PRIVATE, PUBLIC,
    };

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

    impl RuleSource for INetFwRule {
        fn inbound(&self) -> Result<bool, String> {
            Ok(unsafe { self.Direction() }.map_err(|e| e.to_string())? == NET_FW_RULE_DIR_IN)
        }

        fn allow(&self) -> Result<bool, String> {
            Ok(unsafe { self.Action() }.map_err(|e| e.to_string())? == NET_FW_ACTION_ALLOW)
        }

        fn enabled(&self) -> Result<bool, String> {
            Ok(unsafe { self.Enabled() }
                .map_err(|e| e.to_string())?
                .as_bool())
        }

        fn protocol(&self) -> Result<i32, String> {
            unsafe { self.Protocol() }.map_err(|e| e.to_string())
        }

        fn profiles(&self) -> Result<i32, String> {
            unsafe { self.Profiles() }.map_err(|e| e.to_string())
        }

        fn text(&self, field: Field) -> Result<String, String> {
            let value = unsafe {
                match field {
                    Field::Program => self.ApplicationName(),
                    Field::LocalPorts => self.LocalPorts(),
                    Field::RemotePorts => self.RemotePorts(),
                    Field::LocalAddresses => self.LocalAddresses(),
                    Field::RemoteAddresses => self.RemoteAddresses(),
                    Field::InterfaceTypes => self.InterfaceTypes(),
                    Field::Service => self.ServiceName(),
                    restriction => {
                        let rule3: INetFwRule3 = self.cast().map_err(|e| e.to_string())?;
                        match restriction {
                            Field::Package => rule3.LocalAppPackageId(),
                            Field::LocalUserOwner => rule3.LocalUserOwner(),
                            Field::LocalUsers => rule3.LocalUserAuthorizedList(),
                            Field::RemoteUsers => rule3.RemoteUserAuthorizedList(),
                            _ => rule3.RemoteMachineAuthorizedList(),
                        }
                    }
                }
            };
            value.map(|v| v.to_string()).map_err(|e| e.to_string())
        }

        fn interfaces_listed(&self) -> Result<bool, String> {
            Ok(!unsafe { self.Interfaces() }
                .map_err(|e| e.to_string())?
                .is_empty())
        }

        fn secure_flags(&self) -> Result<i32, String> {
            let rule3: INetFwRule3 = self.cast().map_err(|e| e.to_string())?;
            unsafe { rule3.SecureFlags() }.map_err(|e| e.to_string())
        }
    }

    /// Every inbound rule, each with what could be read of it.
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
            if let Some(rule) =
                rule_from(&rule).map_err(|e| windows::core::Error::new(E_FAIL, e))?
            {
                rules.push(rule);
            }
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
                local_ports: Some(ports.into()),
                ..prompt_block(PUBLIC)
            };
            let report = report(&[on(PUBLIC)], &[installer_rule(), rule]);
            assert_eq!(report.profiles, [(PUBLIC, Verdict::Allowed)], "{ports}");
            assert_eq!((report.block_rules, report.scoped_block_rules), (0, 0));
        }
        for ports in ["*", "", "9876", "53, 9000-9999"] {
            let rule = Rule {
                local_ports: Some(ports.into()),
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
            local_ports: Some("19876".into()),
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
                remote_addresses: Some("203.0.113.0/24".into()),
                ..prompt_block(PUBLIC)
            },
            Rule {
                remote_addresses: Some("LocalSubnet".into()),
                ..prompt_block(PUBLIC)
            },
            Rule {
                local_addresses: Some("10.0.0.5".into()),
                ..prompt_block(PUBLIC)
            },
            Rule {
                remote_ports: Some("5000".into()),
                ..prompt_block(PUBLIC)
            },
            Rule {
                interface_types: Some("RemoteAccess".into()),
                ..prompt_block(PUBLIC)
            },
            Rule {
                interfaces: Some(true),
                ..prompt_block(PUBLIC)
            },
            Rule {
                service: Some("SomeService".into()),
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
            interface_types: Some("All".into()),
            ..prompt_block(PUBLIC)
        };
        assert_eq!(verdicts(&[on(PUBLIC)], &[all_types]), [Verdict::Blocked]);
    }

    /// A rule for every program blocks the stream as surely as one for this
    /// exe, but Setup's delete by program can't remove it: it is logged and
    /// never shown.
    #[test]
    fn a_block_for_every_program_on_the_port_is_logged_not_shown() {
        let everyone = report(
            &[on(PUBLIC)],
            &[installer_rule(), Rule::unscoped("", false, PUBLIC)],
        );
        assert_eq!(everyone.profiles, [(PUBLIC, Verdict::BlockedForAllApps)]);
        assert_eq!(
            (everyone.block_rules, everyone.all_programs_block_rules),
            (0, 1)
        );
        assert_eq!(everyone.verdict(), "blocked");
        assert_eq!(everyone.certain_block(), None);
        assert!(everyone.summary().contains("public:blocked_all_programs"));
        // With this exe's own block as well: Setup would leave the other.
        let both = report(
            &[on(PUBLIC)],
            &[prompt_block(PUBLIC), Rule::unscoped("  ", false, PUBLIC)],
        );
        assert_eq!(both.profiles, [(PUBLIC, Verdict::BlockedForAllApps)]);
        assert_eq!(both.certain_block(), None);
        // Another port, or scoped: not a block, or only logged.
        let other_port = Rule {
            local_ports: Some("443".into()),
            ..Rule::unscoped("", false, PUBLIC)
        };
        let scoped = Rule {
            remote_addresses: Some("LocalSubnet".into()),
            ..Rule::unscoped("", false, PUBLIC)
        };
        let unsure = report(&[on(PUBLIC)], &[installer_rule(), other_port, scoped]);
        assert_eq!(unsure.profiles, [(PUBLIC, Verdict::Allowed)]);
        assert_eq!(
            (unsure.all_programs_block_rules, unsure.scoped_block_rules),
            (0, 1)
        );
        assert_eq!(unsure.certain_block(), None);
        // An Allow rule for every program lets the stream in too, unless it
        // is one of Windows' own, limited to a service or some addresses.
        let service_only = Rule {
            service: Some("Dnscache".into()),
            ..Rule::unscoped("", true, PRIVATE)
        };
        let windows_own = report(&[on(PRIVATE)], &[service_only]);
        assert_eq!(windows_own.profiles, [(PRIVATE, Verdict::NoRule)]);
        assert_eq!(windows_own.allow_rules, 0);
        let open = report(&[on(PRIVATE)], &[Rule::unscoped("", true, PRIVATE)]);
        assert_eq!(open.profiles, [(PRIVATE, Verdict::Allowed)]);
    }

    /// A rule as a fake COM object: every getter answers, except `broken`.
    struct Fake {
        rule: Rule,
        protocol: i32,
        broken: Option<&'static str>,
    }

    impl Fake {
        fn get<T>(&self, name: &str, value: T) -> Result<T, String> {
            if self.broken == Some(name) {
                Err(format!("{name} failed (0x80004005)"))
            } else {
                Ok(value)
            }
        }
    }

    impl RuleSource for Fake {
        fn inbound(&self) -> Result<bool, String> {
            self.get("direction", true)
        }
        fn allow(&self) -> Result<bool, String> {
            self.get("action", self.rule.allow)
        }
        fn enabled(&self) -> Result<bool, String> {
            self.get("enabled", self.rule.enabled)
        }
        fn protocol(&self) -> Result<i32, String> {
            self.get("protocol", self.protocol)
        }
        fn profiles(&self) -> Result<i32, String> {
            self.get("profiles", self.rule.profiles as i32)
        }
        fn text(&self, field: Field) -> Result<String, String> {
            let (name, value) = match field {
                Field::Program => ("program", &self.rule.program),
                Field::LocalPorts => ("local_ports", &self.rule.local_ports),
                Field::RemotePorts => ("remote_ports", &self.rule.remote_ports),
                Field::LocalAddresses => ("local_addresses", &self.rule.local_addresses),
                Field::RemoteAddresses => ("remote_addresses", &self.rule.remote_addresses),
                Field::InterfaceTypes => ("interface_types", &self.rule.interface_types),
                Field::Service => ("service", &self.rule.service),
                Field::Package => ("package", &self.rule.package),
                Field::LocalUserOwner => ("local_user_owner", &self.rule.local_user_owner),
                Field::LocalUsers => ("local_users", &self.rule.local_users),
                Field::RemoteUsers => ("remote_users", &self.rule.remote_users),
                Field::RemoteMachines => ("remote_machines", &self.rule.remote_machines),
            };
            self.get(name, value.clone().unwrap_or_default())
        }
        fn interfaces_listed(&self) -> Result<bool, String> {
            self.get("interfaces", self.rule.interfaces.unwrap_or(false))
        }
        fn secure_flags(&self) -> Result<i32, String> {
            self.get("secure_flags", self.rule.secure_flags.unwrap_or(0))
        }
    }

    fn fake(rule: Rule, broken: Option<&'static str>) -> Fake {
        Fake {
            rule,
            protocol: 17,
            broken,
        }
    }

    /// A scope field that can't be read is unknown, never "any": the rule
    /// is logged and can't make a block certain.
    #[test]
    fn a_scope_that_could_not_be_read_never_makes_a_block_certain() {
        for block in [prompt_block(PUBLIC), Rule::unscoped("", false, PUBLIC)] {
            let read = rule_from(&fake(block.clone(), None)).unwrap().unwrap();
            assert_eq!(read, block);
            // Read whole, it blocks (this exe's rule or every program's).
            assert!(report(&[on(PUBLIC)], &[read]).profiles[0].1.blocks());
            for broken in [
                "program",
                "local_ports",
                "remote_ports",
                "local_addresses",
                "remote_addresses",
                "interface_types",
                "interfaces",
                "service",
                "package",
                "local_user_owner",
                "local_users",
                "remote_users",
                "remote_machines",
                "secure_flags",
            ] {
                let read = rule_from(&fake(block.clone(), Some(broken)))
                    .unwrap()
                    .unwrap();
                let judged = report(&[on(PUBLIC)], &[installer_rule(), read]);
                assert_eq!(judged.profiles, [(PUBLIC, Verdict::Allowed)], "{broken}");
                assert_eq!(
                    (
                        judged.block_rules,
                        judged.all_programs_block_rules,
                        judged.scoped_block_rules
                    ),
                    (0, 0, 1),
                    "{broken}"
                );
                assert_eq!(judged.certain_block(), None, "{broken}");
            }
        }
        // Unknown ports on another program's rule: not ours, not counted.
        let other = rule_from(&fake(
            Rule::unscoped(r"C:\Tools\Other.exe", false, PUBLIC),
            Some("local_ports"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(report(&[on(PUBLIC)], &[other]).scoped_block_rules, 0);
    }

    /// INetFwRule3's restrictions limit whose traffic a rule is for: a
    /// package-only rule for every program is not a block on this exe. Each
    /// one, set or unread, makes a rule scoped.
    #[test]
    fn a_rule_limited_to_a_package_user_or_machine_is_never_certain() {
        type Set = fn(&mut Rule);
        let limits: [(&str, Set); 6] = [
            ("package", |r| r.package = Some("S-1-15-2-1".into())),
            ("local_user_owner", |r| {
                r.local_user_owner = Some("S-1-5-21-1".into())
            }),
            ("local_users", |r| {
                r.local_users = Some("O:LSD:(A;;CC;;;S-1-5-21-1)".into())
            }),
            ("remote_users", |r| {
                r.remote_users = Some("O:LSD:(A;;CC;;;S-1-5-21-2)".into())
            }),
            ("remote_machines", |r| {
                r.remote_machines = Some("O:LSD:(A;;CC;;;S-1-5-21-3)".into())
            }),
            ("secure_flags", |r| r.secure_flags = Some(1)),
        ];
        for program in [EXE.to_lowercase(), String::new()] {
            for (name, set) in limits {
                let mut limited = Rule::unscoped(&program, false, PUBLIC);
                set(&mut limited);
                let read = rule_from(&fake(limited, None)).unwrap().unwrap();
                let judged = report(&[on(PUBLIC)], &[installer_rule(), read]);
                assert_eq!(
                    judged.profiles,
                    [(PUBLIC, Verdict::Allowed)],
                    "{name} {program:?}"
                );
                assert_eq!(judged.scoped_block_rules, 1, "{name}");
                assert_eq!(judged.certain_block(), None, "{name}");
            }
        }
        // Empty restrictions and no IPsec: the prompt's rule as Windows
        // stores it stays a certain block.
        let plain = rule_from(&fake(prompt_block(PUBLIC), None))
            .unwrap()
            .unwrap();
        assert!(report(&[on(PUBLIC)], &[plain]).certain_block().is_some());
    }

    /// An Allow rule asking for IPsec may override Block rules for
    /// authenticated traffic. Whatever blocks, the verdict is uncertain and
    /// nothing is shown; it is logged as bypass_rules.
    #[test]
    fn an_allow_rule_that_may_bypass_blocks_makes_any_block_uncertain() {
        let secure = |program: &str, flags: Option<i32>| Rule {
            secure_flags: flags,
            ..Rule::unscoped(program, true, 0x7fff_ffff)
        };
        let shields = Profile {
            block_all_inbound: true,
            ..on(PUBLIC)
        };
        for bypass in [
            secure(EXE, Some(2)),
            secure("", Some(4)),
            secure(&EXE.to_lowercase(), None),
        ] {
            for (profiles, rules) in [
                (vec![on(PUBLIC)], vec![prompt_block(PUBLIC), bypass.clone()]),
                (vec![shields], vec![bypass.clone()]),
            ] {
                let judged = report(&profiles, &rules);
                assert_eq!(judged.bypass_rules, 1, "{bypass:?}");
                assert_eq!(judged.verdict(), "uncertain");
                assert_eq!(judged.certain_block(), None);
                assert!(judged.summary().contains("bypass_rules=1 "));
            }
        }
        // Not ours, disabled, or a Block rule: no bypass.
        let others = [
            secure(r"C:\Tools\Other.exe", Some(2)),
            Rule {
                enabled: false,
                ..secure(EXE, Some(2))
            },
            Rule {
                allow: false,
                remote_addresses: Some("10.0.0.0/8".into()),
                ..secure(EXE, Some(2))
            },
        ];
        for other in others {
            let judged = report(&[on(PUBLIC)], &[prompt_block(PUBLIC), other.clone()]);
            assert_eq!(judged.bypass_rules, 0, "{other:?}");
            assert!(judged.certain_block().is_some(), "{other:?}");
        }
        // Nothing blocks: a bypass rule alone changes nothing.
        let open = report(&[on(PUBLIC)], &[secure(EXE, Some(2))]);
        assert_eq!(open.verdict(), "allowed");
    }

    /// The fields the verdict can't do without fail the whole reading,
    /// which clears what the Stream page shows.
    #[test]
    fn a_failed_action_or_profile_read_fails_the_reading() {
        for broken in ["direction", "action", "enabled", "protocol", "profiles"] {
            let failed = rule_from(&fake(prompt_block(PUBLIC), Some(broken)));
            assert!(failed.is_err(), "{broken}");
        }
        let any_protocol = Fake {
            protocol: 256,
            ..fake(prompt_block(PUBLIC), None)
        };
        assert!(rule_from(&any_protocol).unwrap().unwrap().udp);
        let tcp = Fake {
            protocol: 6,
            ..fake(prompt_block(PUBLIC), None)
        };
        assert!(!rule_from(&tcp).unwrap().unwrap().udp);
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
                            !rule
                                .program
                                .as_deref()
                                .is_some_and(|ours| same_program(ours, Path::new(&program), env))
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
            remote_addresses: Some("192.168.1.0/24".into()),
            ..prompt_block(PRIVATE)
        };
        let report = report(
            &[on(PRIVATE), on(PUBLIC)],
            &[installer_rule(), prompt_block(PUBLIC), scoped],
        );
        assert_eq!(
            report.summary(),
            "Windows Firewall: profiles=[private:allowed public:blocked] allow_rules=1 \
             block_rules=1 all_programs_block_rules=0 scoped_block_rules=1 bypass_rules=0 lan=true verdict=partly_blocked"
        );
        let line = format!("2026-11-05T12:00:00.000000Z  WARN {}", report.summary());
        assert_eq!(crate::telemetry::uploaded(&line), line);
    }
}
