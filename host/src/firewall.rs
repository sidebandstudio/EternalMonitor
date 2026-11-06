//! Whether Windows Firewall lets iPads reach this copy of the host.
//!
//! An iPad whose HELLO never arrives says "Your PC didn't answer" while the
//! PC says it is ready, and neither side could tell why. The usual blocks are
//! on the PC: an inbound Block rule for this exe, which Windows adds when its
//! "allow access" prompt is dismissed and which beats every Allow rule, or
//! "Blocks all incoming connections" on the network's profile. A background
//! thread reads the firewall policy at startup and every [`RECHECK`], logs
//! the result when it changes, and the Stream page warns while it blocks.
//!
//! The log line names profiles and counts rules. It holds no path, rule name
//! or address.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

#[cfg(windows)]
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

/// An inbound rule, as far as the verdict needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The program path as stored, which may use `%ProgramFiles%` and the like.
    pub program: String,
    pub allow: bool,
    pub enabled: bool,
    /// UDP or any protocol.
    pub udp: bool,
    pub profiles: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// An Allow rule for this exe, and nothing that beats it.
    Allowed,
    /// An enabled Block rule for this exe in a profile in use.
    Blocked,
    /// A profile in use blocks all incoming connections.
    BlocksAll,
    /// Neither rule exists: Windows asks, or blocks quietly if it may not.
    NoRule,
    /// Windows Firewall is off on every network: another firewall may be.
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
}

/// What the firewall does to this exe's inbound UDP on the networks in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub profiles: Vec<Profile>,
    pub allow_rules: usize,
    pub block_rules: usize,
    pub verdict: Verdict,
}

impl Report {
    /// The profiles that block all incoming connections, by name.
    pub fn blocking_profiles(&self) -> Vec<&'static str> {
        self.profiles
            .iter()
            .filter(|p| p.on && p.block_all_inbound)
            .map(|p| profile_name(p.bit))
            .collect()
    }

    /// The log line: profile names, switches and counts only.
    pub fn summary(&self) -> String {
        let profiles: Vec<String> = self
            .profiles
            .iter()
            .map(|p| {
                let state = match (p.on, p.block_all_inbound) {
                    (false, _) => "off",
                    (true, true) => "blocks_all",
                    (true, false) => "on",
                };
                format!("{}:{state}", profile_name(p.bit))
            })
            .collect();
        format!(
            "Windows Firewall: profiles=[{}] allow_rules={} block_rules={} verdict={}",
            profiles.join(" "),
            self.allow_rules,
            self.block_rules,
            self.verdict.label(),
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

/// The verdict for inbound UDP to `exe` on the profiles in use. Windows lets
/// "block all" beat every rule and a Block rule beat every Allow rule.
pub fn judge(
    profiles: &[Profile],
    rules: &[Rule],
    exe: &Path,
    lookup: impl Fn(&str) -> Option<String>,
) -> Report {
    let on: u32 = profiles.iter().filter(|p| p.on).map(|p| p.bit).sum();
    let counted = |allow: bool| {
        rules
            .iter()
            .filter(|r| {
                r.allow == allow
                    && r.enabled
                    && r.udp
                    && r.profiles & on != 0
                    && same_program(&r.program, exe, &lookup)
            })
            .count()
    };
    let (allow_rules, block_rules) = (counted(true), counted(false));
    let verdict = if on == 0 {
        Verdict::Off
    } else if profiles.iter().any(|p| p.on && p.block_all_inbound) {
        Verdict::BlocksAll
    } else if block_rules > 0 {
        Verdict::Blocked
    } else if allow_rules > 0 {
        Verdict::Allowed
    } else {
        Verdict::NoRule
    };
    Report {
        profiles: profiles.to_vec(),
        allow_rules,
        block_rules,
        verdict,
    }
}

static LATEST: Mutex<Option<Report>> = Mutex::new(None);

/// The last reading, once there is one. `None` off Windows.
pub fn latest() -> Option<Report> {
    LATEST.lock().ok()?.clone()
}

/// Stores `report` and says whether it differs from the one before.
#[cfg(windows)]
fn store(report: Report) -> bool {
    let Ok(mut latest) = LATEST.lock() else {
        return false;
    };
    let changed = latest.as_ref() != Some(&report);
    *latest = Some(report);
    changed
}

/// Logs a new reading: a warning while it blocks.
#[cfg(windows)]
fn note(report: Report) {
    let line = report.summary();
    let blocks = matches!(report.verdict, Verdict::Blocked | Verdict::BlocksAll);
    if store(report) {
        if blocks {
            warn!("{line}");
        } else {
            info!("{line}");
        }
    }
}

/// Reads the policy now and every [`RECHECK`] on its own thread.
pub fn watch() {
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
                    match windows_policy::read(&exe) {
                        Ok(report) => {
                            failed = false;
                            note(report);
                        }
                        Err(error) if !failed => {
                            failed = true;
                            warn!(%error, "Could not read the Windows Firewall policy");
                        }
                        Err(_) => {}
                    }
                    std::thread::sleep(RECHECK);
                }
            });
    }
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

    pub fn read(exe: &Path) -> windows::core::Result<Report> {
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
        Ok(judge(&profiles, &rules, exe, |name| {
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
            let program = unsafe { rule.ApplicationName() }
                .map(|name| name.to_string())
                .unwrap_or_default();
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
        Rule {
            program: EXE.into(),
            allow: true,
            enabled: true,
            udp: true,
            profiles: 0x7fff_ffff,
        }
    }

    /// What Windows adds when its prompt is dismissed: lower case, the
    /// profiles in use then.
    fn prompt_block(profiles: u32) -> Rule {
        Rule {
            program: EXE.to_lowercase(),
            allow: false,
            enabled: true,
            udp: true,
            profiles,
        }
    }

    fn verdict(profiles: &[Profile], rules: &[Rule]) -> Verdict {
        judge(profiles, rules, &PathBuf::from(EXE), env).verdict
    }

    #[test]
    fn the_installer_rule_allows_on_every_network() {
        for bit in [DOMAIN, PRIVATE, PUBLIC] {
            assert_eq!(verdict(&[on(bit)], &[installer_rule()]), Verdict::Allowed);
        }
    }

    #[test]
    fn a_dismissed_prompt_beats_the_installer_rule_on_its_networks_only() {
        let rules = [installer_rule(), prompt_block(PUBLIC)];
        assert_eq!(verdict(&[on(PUBLIC)], &rules), Verdict::Blocked);
        assert_eq!(verdict(&[on(PRIVATE)], &rules), Verdict::Allowed);
        // Ethernet on a private network, Wi-Fi on a public one.
        assert_eq!(
            verdict(&[on(PRIVATE), on(PUBLIC)], &rules),
            Verdict::Blocked
        );
    }

    #[test]
    fn blocking_all_incoming_connections_beats_every_rule() {
        let shields = Profile {
            block_all_inbound: true,
            ..on(PUBLIC)
        };
        assert_eq!(verdict(&[shields], &[installer_rule()]), Verdict::BlocksAll);
        let report = judge(
            &[on(PRIVATE), shields],
            &[installer_rule()],
            &PathBuf::from(EXE),
            env,
        );
        assert_eq!(report.blocking_profiles(), ["public"]);
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
        let other_exe = Rule {
            program: r"C:\Tools\EternalMonitor-host.exe".into(),
            ..prompt_block(PUBLIC)
        };
        let report = judge(
            &[on(PUBLIC)],
            &[
                installer_rule(),
                tcp,
                disabled,
                other_exe,
                prompt_block(PRIVATE),
            ],
            &PathBuf::from(EXE),
            env,
        );
        assert_eq!((report.allow_rules, report.block_rules), (1, 0));
        assert_eq!(report.verdict, Verdict::Allowed);
    }

    #[test]
    fn no_rule_and_firewall_off_are_told_apart() {
        assert_eq!(verdict(&[on(PRIVATE)], &[]), Verdict::NoRule);
        let off = Profile {
            on: false,
            ..on(PRIVATE)
        };
        assert_eq!(verdict(&[off], &[prompt_block(PRIVATE)]), Verdict::Off);
        // A rule for a profile whose firewall is off doesn't count.
        assert_eq!(
            verdict(&[off, on(PUBLIC)], &[prompt_block(PRIVATE)]),
            Verdict::NoRule
        );
    }

    #[test]
    fn a_rule_made_in_windows_security_matches_through_its_variables() {
        let ui_rule = Rule {
            program: r"%ProgramFiles%\EternalMonitor\EternalMonitor-host.exe".into(),
            ..installer_rule()
        };
        assert_eq!(verdict(&[on(PRIVATE)], &[ui_rule]), Verdict::Allowed);
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
                        program: arg("program").unwrap_or_default(),
                        allow: arg("action").as_deref() == Some("allow"),
                        enabled: arg("enable").as_deref() != Some("no"),
                        udp: arg("protocol").as_deref() == Some("UDP"),
                        profiles: bits
                            .iter()
                            .filter(|(word, _)| profiles.split(',').any(|p| p == *word))
                            .map(|(_, bit)| bit)
                            .sum(),
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
        let other = Rule {
            program: r"C:\Tools\Other.exe".into(),
            ..prompt_block(PUBLIC)
        };
        let before = vec![
            ("EternalMonitor Host UDP".to_string(), installer_rule()),
            (
                format!("UDP Query User{{0-1-2-3-4}}{}", EXE.to_lowercase()),
                prompt_block(PRIVATE | PUBLIC),
            ),
            ("Other".to_string(), other.clone()),
        ];
        let rules: Vec<Rule> = before.iter().map(|(_, rule)| rule.clone()).collect();
        assert_eq!(verdict(&[on(PUBLIC)], &rules), Verdict::Blocked);

        let after = after_setup(before, app);
        assert_eq!(
            verdict(&[on(PUBLIC)], &after),
            Verdict::Allowed,
            "{after:?}"
        );
        assert_eq!(verdict(&[on(PRIVATE)], &after), Verdict::Allowed);
        assert!(
            after.contains(&other),
            "Setup removed another program's rule"
        );
        // A first install: nothing to remove, the Allow rule still comes.
        assert_eq!(
            verdict(&[on(DOMAIN)], &after_setup(Vec::new(), app)),
            Verdict::Allowed
        );
    }

    #[test]
    fn the_log_line_names_no_path_or_rule() {
        let report = judge(
            &[on(PRIVATE), on(PUBLIC)],
            &[installer_rule(), prompt_block(PUBLIC)],
            &PathBuf::from(EXE),
            env,
        );
        assert_eq!(
            report.summary(),
            "Windows Firewall: profiles=[private:on public:on] allow_rules=1 block_rules=1 verdict=blocked"
        );
        let line = format!("2026-11-05T12:00:00.000000Z  WARN {}", report.summary());
        assert_eq!(crate::telemetry::uploaded(&line), line);
    }
}
