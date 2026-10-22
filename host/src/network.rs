//! Where an iPad can reach this PC, and whether Windows Firewall lets it in.
//!
//! The Address and QR code come from the adapters that face the home network
//! (Wi-Fi and Ethernet with a gateway). A VPN or a Hyper-V/WSL switch can own
//! the default route; an iPad can't reach those addresses, so they are never
//! shown while a LAN address exists.
//!
//! The startup summary says which kinds of adapters were up, whether Windows
//! Firewall lets EternalMonitor in on the current network, and which security
//! products manage the firewall. It never logs an address, a network name or
//! an adapter name.
use std::net::Ipv4Addr;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Deserialize;
use tracing::{info, warn};

/// Runs elevated to add the inbound rules, like Setup does. An optional
/// second argument names the rules (tests use their own name).
pub const ALLOW_FIREWALL_ARG: &str = "--allow-firewall";
/// Setup's rule names, with " UDP" and " TCP" appended.
pub const RULE_NAME: &str = "EternalMonitor Host";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wifi,
    Ethernet,
    /// Hyper-V, WSL, Docker, VirtualBox and VMware switches.
    Virtual,
    /// VPN and tunnel adapters.
    Vpn,
    Other,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Wifi => "wifi",
            Kind::Ethernet => "ethernet",
            Kind::Virtual => "virtual",
            Kind::Vpn => "vpn",
            Kind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub kind: Kind,
    pub up: bool,
    /// Has a default gateway: it leads to a real network.
    pub gateway: bool,
    pub ipv4: Vec<Ipv4Addr>,
}

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_PPP: u32 = 23;
const IF_TYPE_LOOPBACK: u32 = 24;
const IF_TYPE_PROP_VIRTUAL: u32 = 53;
const IF_TYPE_WIFI: u32 = 71;
const IF_TYPE_TUNNEL: u32 = 131;

const VPN_NAMES: &[&str] = &[
    "vpn",
    "tap-windows",
    "wireguard",
    "wintun",
    "tailscale",
    "zerotier",
    "hamachi",
    "radmin",
    "anyconnect",
    "fortinet",
    "forticlient",
    "globalprotect",
    "pangp",
    "juniper",
    "pulse secure",
    "sonicwall",
    "check point",
    "nordlynx",
    "mullvad",
    "proton",
    "cloudflare warp",
    "wan miniport",
];
const VIRTUAL_NAMES: &[&str] = &[
    "hyper-v",
    "vethernet",
    "wsl",
    "docker",
    "virtualbox",
    "vmware",
    "npcap",
    "loopback",
    "bluetooth",
];

/// What kind of adapter Windows reports, from its interface type and its
/// description or name.
pub fn classify(if_type: u32, description: &str, name: &str) -> Kind {
    let text = format!("{description} {name}").to_lowercase();
    if matches!(if_type, IF_TYPE_PPP | IF_TYPE_TUNNEL | IF_TYPE_PROP_VIRTUAL)
        || VPN_NAMES.iter().any(|vpn| text.contains(vpn))
    {
        return Kind::Vpn;
    }
    if if_type == IF_TYPE_LOOPBACK || VIRTUAL_NAMES.iter().any(|v| text.contains(v)) {
        return Kind::Virtual;
    }
    match if_type {
        IF_TYPE_WIFI => Kind::Wifi,
        IF_TYPE_ETHERNET => Kind::Ethernet,
        _ => Kind::Other,
    }
}

fn usable(ip: &Ipv4Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast())
}

/// How well an iPad on the home network can reach an adapter's addresses;
/// `None` when it can't. A Hyper-V external switch carries the real network,
/// so a virtual adapter with a gateway still counts.
fn reach(adapter: &Adapter) -> Option<u8> {
    if !adapter.up {
        return None;
    }
    match (adapter.kind, adapter.gateway) {
        (Kind::Wifi | Kind::Ethernet, true) => Some(0),
        (Kind::Wifi | Kind::Ethernet, false) => Some(1),
        (Kind::Virtual, true) => Some(2),
        (Kind::Other, true) => Some(3),
        _ => None,
    }
}

/// The addresses an iPad on the same network can use, best first. Private
/// addresses come before public ones at the same reach.
pub fn lan_addresses(adapters: &[Adapter]) -> Vec<Ipv4Addr> {
    let mut ranked: Vec<(u8, bool, Ipv4Addr)> = adapters
        .iter()
        .filter_map(|adapter| reach(adapter).map(|rank| (rank, adapter)))
        .flat_map(|(rank, adapter)| {
            adapter
                .ipv4
                .iter()
                .filter(|ip| usable(ip))
                .map(move |ip| (rank, !ip.is_private(), *ip))
        })
        .collect();
    ranked.sort_by_key(|(rank, public, _)| (*rank, *public));
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for (_, _, ip) in ranked {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// An address's range, for logs that must not carry the address itself.
pub fn address_class(ip: Ipv4Addr) -> &'static str {
    let [a, b, ..] = ip.octets();
    match (a, b) {
        (192, 168) => "192.168/16",
        (10, _) => "10/8",
        (172, 16..=31) => "172.16/12",
        (100, 64..=127) => "100.64/10",
        (169, 254) => "link-local",
        _ if ip.is_loopback() => "loopback",
        _ => "public",
    }
}

/// The adapters as of the last look, and the addresses chosen from them.
static LAST: Lazy<Mutex<Vec<Ipv4Addr>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// The LAN addresses from the last call to [`refresh_addresses`].
pub fn addresses() -> Vec<Ipv4Addr> {
    LAST.lock().clone()
}

/// Reads the adapters again and returns the LAN addresses, best first.
pub fn refresh_addresses() -> Vec<Ipv4Addr> {
    let found = lan_addresses(&adapters());
    *LAST.lock() = found.clone();
    found
}

// ── Windows Firewall ────────────────────────────────────────────────────────

pub const PROFILE_DOMAIN: u8 = 1;
pub const PROFILE_PRIVATE: u8 = 2;
pub const PROFILE_PUBLIC: u8 = 4;
const PROFILE_ALL: u8 = PROFILE_DOMAIN | PROFILE_PRIVATE | PROFILE_PUBLIC;

/// One enabled or disabled inbound rule that could apply to this host's UDP
/// port: it names this executable, or any program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub enabled: bool,
    pub allow: bool,
    pub profiles: u8,
    /// Covers UDP: the protocol is UDP or Any.
    pub udp: bool,
    /// `None`: any program. Otherwise the program as Windows stores it.
    pub program: Option<String>,
    pub ports: Ports,
    /// Scoped by something this check doesn't evaluate: remote or local
    /// address, service, interface type, owner or app package.
    pub scoped: bool,
    /// In the PC's own (local) store, so the elevated repair may change it.
    /// Group policy and other stores are left alone.
    pub local: bool,
}

/// A rule's local ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ports {
    Any,
    /// Single ports and inclusive ranges.
    List(Vec<(u16, u16)>),
    /// A keyword such as RPC or Teredo, which this check doesn't evaluate.
    Unknown,
}

impl Ports {
    fn parse(values: &[String]) -> Ports {
        if values.is_empty() || values.iter().any(|value| value.eq_ignore_ascii_case("any")) {
            return Ports::Any;
        }
        let mut list = Vec::new();
        for value in values {
            for part in value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
            {
                let range = match part.split_once('-') {
                    Some((low, high)) => low.trim().parse().ok().zip(high.trim().parse().ok()),
                    None => part.parse().ok().map(|port| (port, port)),
                };
                match range {
                    Some(range) => list.push(range),
                    None => return Ports::Unknown,
                }
            }
        }
        Ports::List(list)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Firewall {
    /// Profiles of the networks this PC is connected to now.
    pub active: u8,
    /// Profiles where Windows Firewall is on.
    pub enabled: u8,
    /// This host's executable, normalized with [`normalize_path`].
    pub exe: String,
    /// The UDP port the host listens on.
    pub port: u16,
    pub rules: Vec<Rule>,
    /// Security products registered as the PC's firewall, other than Windows'.
    pub products: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// An enabled inbound rule allows the port on a current network.
    Allowed,
    /// Windows Firewall is off on the current networks.
    Off,
    /// No network is connected, so there is nothing to check.
    NoNetwork,
    /// Rules this check can't evaluate may decide it. Never a notice: a
    /// wrong "blocked" message is worse than none.
    Unknown,
    /// Nothing allows EternalMonitor in on a current network: Windows drops
    /// the iPad's packets without asking.
    Missing,
    /// A local rule for this executable blocks it, as when someone dismissed
    /// Windows' prompt. Block wins over allow; the repair removes it.
    Blocked,
    /// A block the repair must not change: one for any program or a port,
    /// or one set by group policy (the PC's administrator).
    BlockedByAdmin,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Allowed => "allowed",
            Verdict::Off => "firewall-off",
            Verdict::NoNetwork => "no-network",
            Verdict::Unknown => "unknown",
            Verdict::Missing => "missing",
            Verdict::Blocked => "blocked",
            Verdict::BlockedByAdmin => "blocked-by-policy",
        }
    }

    /// The user should be offered the repair.
    pub fn needs_repair(self) -> bool {
        matches!(self, Verdict::Missing | Verdict::Blocked)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applies {
    Yes,
    No,
    Unknown,
}

/// Whether `rule` decides inbound UDP to `port` for `exe` on the `on`
/// profiles. Anything it can't evaluate is `Unknown`.
fn applies(rule: &Rule, exe: &str, port: u16, on: u8) -> Applies {
    if !rule.enabled || !rule.udp || rule.profiles & on == 0 {
        return Applies::No;
    }
    if let Some(program) = &rule.program {
        if normalize_path(program) != exe {
            return Applies::No;
        }
    }
    match &rule.ports {
        Ports::Any => {}
        Ports::List(list)
            if list
                .iter()
                .any(|(low, high)| (*low..=*high).contains(&port)) => {}
        Ports::List(_) => return Applies::No,
        Ports::Unknown => return Applies::Unknown,
    }
    if rule.scoped {
        Applies::Unknown
    } else {
        Applies::Yes
    }
}

/// Whether an iPad's UDP packets get through Windows Firewall to this host,
/// told only when certain: an applicable enabled block, or no allow at all
/// that could apply.
pub fn verdict(firewall: &Firewall) -> Verdict {
    if firewall.active == 0 {
        return Verdict::NoNetwork;
    }
    let on = firewall.active & firewall.enabled;
    if on == 0 {
        return Verdict::Off;
    }
    let decided = |allow: bool, answer: Applies| {
        firewall
            .rules
            .iter()
            .filter(move |rule| rule.allow == allow)
            .filter(move |rule| applies(rule, &firewall.exe, firewall.port, on) == answer)
    };
    let mut blocks = decided(false, Applies::Yes).peekable();
    if blocks.peek().is_some() {
        let ours = |rule: &Rule| rule.program.is_some() && rule.local;
        return if blocks.all(ours) {
            Verdict::Blocked
        } else {
            Verdict::BlockedByAdmin
        };
    }
    if decided(true, Applies::Yes).next().is_some() {
        Verdict::Allowed
    } else if decided(true, Applies::Unknown).next().is_some()
        || decided(false, Applies::Unknown).next().is_some()
    {
        Verdict::Unknown
    } else {
        Verdict::Missing
    }
}

/// A program path as Windows Firewall compares it: no `\\?\` or `\??\`
/// prefix, environment variables expanded, short (8.3) names made long
/// when the file exists, and lowercase.
pub fn normalize_path(raw: &str) -> String {
    let mut path = raw.trim().replace('/', "\\");
    for prefix in [r"\\?\UNC\", r"\??\UNC\"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = format!(r"\\{rest}");
        }
    }
    for prefix in [r"\\?\", r"\??\"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = rest.to_string();
        }
    }
    let expanded = expand_env(&path);
    long_path(&expanded).unwrap_or(expanded).to_lowercase()
}

/// `%NAME%` replaced by the variable's value; unknown names stay as written.
fn expand_env(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else {
            break;
        };
        let name = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match std::env::var(name) {
            Ok(value) if !name.is_empty() => out.push_str(&value),
            _ => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(not(windows))]
fn long_path(_path: &str) -> Option<String> {
    None
}

#[cfg(windows)]
fn long_path(path: &str) -> Option<String> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetLongPathNameW;
    let wide = HSTRING::from(path);
    let needed = unsafe { GetLongPathNameW(&wide, None) };
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u16; needed as usize];
    let len = unsafe { GetLongPathNameW(&wide, Some(&mut buffer)) } as usize;
    (len > 0 && len < buffer.len()).then(|| String::from_utf16_lossy(&buffer[..len]))
}

fn profile_mask(text: &str) -> u8 {
    text.split(',')
        .map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "any" => PROFILE_ALL,
            "domain" | "domainauthenticated" => PROFILE_DOMAIN,
            "private" => PROFILE_PRIVATE,
            "public" => PROFILE_PUBLIC,
            _ => 0,
        })
        .fold(0, |mask, bit| mask | bit)
}

fn profile_names(mask: u8) -> String {
    let names: Vec<&str> = [
        (PROFILE_DOMAIN, "domain"),
        (PROFILE_PRIVATE, "private"),
        (PROFILE_PUBLIC, "public"),
    ]
    .iter()
    .filter(|(bit, _)| mask & bit != 0)
    .map(|(_, name)| *name)
    .collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join("+")
    }
}

#[derive(Deserialize)]
struct RawFirewall {
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default)]
    firewall: Vec<RawProfile>,
    #[serde(default)]
    rules: Vec<RawRule>,
    #[serde(default)]
    products: Vec<String>,
}

#[derive(Deserialize)]
struct RawProfile {
    name: String,
    enabled: String,
}

/// One rule from [`FIREWALL_QUERY`]. Filters this check doesn't read are
/// reported only as "is it Any".
#[derive(Deserialize)]
struct RawRule {
    enabled: String,
    action: String,
    profile: String,
    protocol: String,
    #[serde(default)]
    program: String,
    #[serde(default)]
    localport: Vec<String>,
    #[serde(default)]
    remote: Vec<String>,
    #[serde(default)]
    localaddress: Vec<String>,
    #[serde(default)]
    service: String,
    #[serde(default)]
    interface: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    package: String,
    #[serde(default)]
    store: String,
}

fn any_or_empty(text: &str) -> bool {
    text.is_empty() || text.eq_ignore_ascii_case("any") || text == "*"
}

/// Reads [`FIREWALL_QUERY`]'s output for `exe` listening on UDP `port`.
pub fn parse_firewall(json: &str, exe: &str, port: u16) -> Option<Firewall> {
    let raw: RawFirewall = serde_json::from_str(json.trim()).ok()?;
    let truthy = |text: &str| text.eq_ignore_ascii_case("true");
    Some(Firewall {
        active: raw
            .profiles
            .iter()
            .fold(0, |mask, name| mask | profile_mask(name)),
        enabled: raw
            .firewall
            .iter()
            .filter(|profile| truthy(&profile.enabled))
            .fold(0, |mask, profile| mask | profile_mask(&profile.name)),
        exe: normalize_path(exe),
        port,
        rules: raw
            .rules
            .iter()
            .map(|rule| Rule {
                enabled: truthy(&rule.enabled),
                allow: rule.action.eq_ignore_ascii_case("allow"),
                profiles: profile_mask(&rule.profile),
                udp: rule.protocol.eq_ignore_ascii_case("udp")
                    || rule.protocol.eq_ignore_ascii_case("any")
                    || rule.protocol == "17",
                program: (!any_or_empty(&rule.program)).then(|| rule.program.clone()),
                ports: Ports::parse(&rule.localport),
                scoped: !rule.remote.iter().all(|value| any_or_empty(value))
                    || !rule.localaddress.iter().all(|value| any_or_empty(value))
                    || !any_or_empty(&rule.service)
                    || !any_or_empty(&rule.interface)
                    || !rule.owner.is_empty()
                    || !rule.package.is_empty(),
                local: rule.store.is_empty() || rule.store.eq_ignore_ascii_case("local"),
            })
            .collect(),
        products: raw
            .products
            .into_iter()
            .filter(|name| !name.to_lowercase().contains("windows"))
            .collect(),
    })
}

/// Lists the connected networks' categories, the firewall profiles, every
/// inbound UDP rule for any program or for a program named like this host
/// (or written as a short name), and third-party firewalls, as one line of
/// JSON. The executable comes in `ETERNAL_FW_EXE`, never in the script text.
/// Rule names are left out.
pub const FIREWALL_QUERY: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$leaf = [IO.Path]::GetFileName($env:ETERNAL_FW_EXE)
$profiles = @(Get-NetConnectionProfile | ForEach-Object { [string]$_.NetworkCategory })
$firewall = @(Get-NetFirewallProfile -PolicyStore ActiveStore | ForEach-Object { @{ name = [string]$_.Name; enabled = [string]$_.Enabled } })
$apps = @{}; Get-NetFirewallApplicationFilter -PolicyStore ActiveStore | ForEach-Object { $apps[$_.InstanceID] = $_ }
$ports = @{}; Get-NetFirewallPortFilter -PolicyStore ActiveStore | ForEach-Object { $ports[$_.InstanceID] = $_ }
$addresses = @{}; Get-NetFirewallAddressFilter -PolicyStore ActiveStore | ForEach-Object { $addresses[$_.InstanceID] = $_ }
$services = @{}; Get-NetFirewallServiceFilter -PolicyStore ActiveStore | ForEach-Object { $services[$_.InstanceID] = $_ }
$interfaces = @{}; Get-NetFirewallInterfaceTypeFilter -PolicyStore ActiveStore | ForEach-Object { $interfaces[$_.InstanceID] = $_ }
$rules = @(Get-NetFirewallRule -PolicyStore ActiveStore -Direction Inbound | ForEach-Object {
    $id = $_.InstanceID; $app = $apps[$id]; $port = $ports[$id]
    $program = [string]$app.Program
    $anyProgram = $program -eq '' -or $program -eq 'Any'
    $named = -not $anyProgram -and ([IO.Path]::GetFileName([Environment]::ExpandEnvironmentVariables($program)) -ieq $leaf -or $program -like '*~*')
    $udp = @('UDP', 'Any', '17') -contains [string]$port.Protocol
    if ($udp -and (($anyProgram -and [string]$_.Enabled -eq 'True') -or $named)) {
        @{ enabled = [string]$_.Enabled; action = [string]$_.Action; profile = [string]$_.Profile; protocol = [string]$port.Protocol
           program = $program; localport = @($port.LocalPort | ForEach-Object { [string]$_ })
           remote = @($addresses[$id].RemoteAddress | ForEach-Object { [string]$_ }); localaddress = @($addresses[$id].LocalAddress | ForEach-Object { [string]$_ })
           service = [string]$services[$id].Service; interface = [string]$interfaces[$id].InterfaceType
           owner = [string]$_.Owner; package = [string]$app.Package; store = [string]$_.PolicyStoreSourceType }
    }
})
$products = @(Get-CimInstance -Namespace root/SecurityCenter2 -ClassName FirewallProduct | ForEach-Object { [string]$_.displayName })
@{ profiles = $profiles; firewall = $firewall; rules = $rules; products = $products } | ConvertTo-Json -Depth 4 -Compress
"#;

/// Lists inbound Block rules in the PC's own store that name a program, with
/// their ids, for the elevated repair.
pub const LOCAL_BLOCKS_QUERY: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$found = @(Get-NetFirewallRule -PolicyStore PersistentStore -Direction Inbound -Action Block | ForEach-Object {
    $program = [string]($_ | Get-NetFirewallApplicationFilter).Program
    if ($program -and $program -ne 'Any') { @{ id = [string]$_.Name; program = $program } }
})
ConvertTo-Json -InputObject $found -Compress
"#;

/// Removes the rules whose ids come in `ETERNAL_FW_RULE_IDS`, one per line,
/// from the PC's own store only.
pub const REMOVE_RULES: &str = r#"
$ErrorActionPreference = 'Stop'
$ids = @($env:ETERNAL_FW_RULE_IDS -split "`n" | Where-Object { $_ })
if ($ids.Count -gt 0) { Remove-NetFirewallRule -PolicyStore PersistentStore -Name $ids }
"#;

#[derive(Debug, Deserialize)]
pub struct LocalBlock {
    pub id: String,
    pub program: String,
}

/// The ids of local Block rules for exactly this executable: what dismissing
/// Windows' prompt leaves. Rules for any program, or another program, stay.
pub fn blocks_to_remove(blocks: &[LocalBlock], exe: &str) -> Vec<String> {
    let exe = normalize_path(exe);
    blocks
        .iter()
        .filter(|block| !block.id.is_empty() && !block.id.contains(['\n', '\r']))
        .filter(|block| normalize_path(&block.program) == exe)
        .map(|block| block.id.clone())
        .collect()
}

/// Rule names the elevated repair writes: Setup's, or a test's own.
fn repair_name_allowed(name: &str) -> bool {
    name == RULE_NAME || (name.starts_with("EM-TEST ") && !name.contains(['\n', '\r', '"']))
}

/// The netsh arguments that replace this host's inbound rules, exactly as
/// Setup writes them.
pub fn allow_commands(name: &str, exe: &str) -> Vec<Vec<String>> {
    ["UDP", "TCP"]
        .iter()
        .flat_map(|protocol| {
            let rule = format!("name={name} {protocol}");
            let base = ["advfirewall", "firewall"].map(String::from);
            [
                base.iter()
                    .cloned()
                    .chain(["delete".into(), "rule".into(), rule.clone()])
                    .collect(),
                base.iter()
                    .cloned()
                    .chain([
                        "add".into(),
                        "rule".into(),
                        rule,
                        "dir=in".into(),
                        "action=allow".into(),
                        format!("program={exe}"),
                        format!("protocol={protocol}"),
                        "profile=domain,private,public".into(),
                        "enable=yes".into(),
                    ])
                    .collect(),
            ]
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallCheck {
    /// Not checked yet, or this isn't Windows.
    Unknown,
    Checked(Verdict),
    /// The Windows prompt for the repair is open.
    Repairing,
}

static FIREWALL: Lazy<Mutex<FirewallCheck>> = Lazy::new(|| Mutex::new(FirewallCheck::Unknown));

pub fn firewall_check() -> FirewallCheck {
    *FIREWALL.lock()
}

/// The startup summary: one line describing the network without naming it.
pub fn summary(adapters: &[Adapter], firewall: Option<&Firewall>) -> String {
    let mut kinds: Vec<String> = adapters
        .iter()
        .filter(|adapter| adapter.kind != Kind::Other || adapter.up)
        .map(|adapter| {
            let mut text = adapter.kind.label().to_string();
            text.push_str(if adapter.up { ":up" } else { ":down" });
            if adapter.up && adapter.gateway {
                text.push_str("+gw");
            }
            if adapter.up && adapter.ipv4.iter().any(usable) {
                text.push_str("+ipv4");
            }
            text
        })
        .collect();
    kinds.sort();
    let lan = lan_addresses(adapters);
    let mut mdns: Vec<&str> = adapters
        .iter()
        .filter(|adapter| adapter.up && adapter.ipv4.iter().any(usable))
        .map(|adapter| adapter.kind.label())
        .collect();
    mdns.sort();
    mdns.dedup();
    let mut line = format!(
        "adapters=[{}] lan_addresses={} first_address={} mdns_on=[{}]",
        kinds.join(" "),
        lan.len(),
        lan.first().map_or("none", |ip| address_class(*ip)),
        mdns.join(" "),
    );
    match firewall {
        Some(firewall) => line.push_str(&format!(
            " network={} windows_firewall_on={} eternal_rules={} rules_found={} other_firewall=[{}]",
            profile_names(firewall.active),
            profile_names(firewall.enabled),
            verdict(firewall).label(),
            firewall.rules.len(),
            firewall.products.join(", "),
        )),
        None => line.push_str(" firewall=unknown"),
    }
    line
}

/// Logs the network summary and checks the firewall in the background, so a
/// slow PowerShell never delays the window.
pub fn start_check(port: u16) {
    PORT.store(port, std::sync::atomic::Ordering::Relaxed);
    std::thread::Builder::new()
        .name("network-check".into())
        .spawn(|| {
            let adapters = adapters();
            *LAST.lock() = lan_addresses(&adapters);
            let firewall = query_firewall();
            info!("Network summary {}", summary(&adapters, firewall.as_ref()));
            record(firewall.as_ref());
        })
        .ok();
}

fn record(firewall: Option<&Firewall>) {
    let check = match firewall {
        Some(firewall) => FirewallCheck::Checked(verdict(firewall)),
        None => FirewallCheck::Unknown,
    };
    match check {
        FirewallCheck::Checked(Verdict::Missing) => {
            warn!("Windows Firewall has no rule letting iPads reach EternalMonitor on this network")
        }
        FirewallCheck::Checked(Verdict::Blocked) => {
            warn!("A Windows Firewall rule blocks iPads from reaching EternalMonitor")
        }
        FirewallCheck::Checked(Verdict::BlockedByAdmin) => warn!(
            "A firewall rule EternalMonitor may not change (policy, port or any-program) blocks iPads"
        ),
        _ => {}
    }
    *FIREWALL.lock() = check;
}

/// Whether this executable may offer the repair: Setup's file name. A
/// developer build must not rewrite the installed host's rules.
pub fn can_repair() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|name| name.to_os_string()))
        .is_some_and(|name| name.eq_ignore_ascii_case("EternalMonitor-host.exe"))
}

/// Asks Windows to run the rule repair elevated (the user approves the
/// prompt), then checks again. Runs on its own thread.
pub fn repair_firewall() {
    {
        let mut check = FIREWALL.lock();
        if *check == FirewallCheck::Repairing {
            return;
        }
        *check = FirewallCheck::Repairing;
    }
    std::thread::Builder::new()
        .name("firewall-repair".into())
        .spawn(|| {
            match run_elevated_repair() {
                Ok(code) => info!(code, "Firewall repair finished"),
                Err(error) => warn!(%error, "Firewall repair did not run"),
            }
            let firewall = query_firewall();
            if let Some(firewall) = &firewall {
                info!(
                    eternal_rules = verdict(firewall).label(),
                    "Firewall checked after the repair"
                );
            }
            record(firewall.as_ref());
        })
        .ok();
}

/// Run as the elevated repair when the command line asks for it; never
/// returns then.
pub fn run_repair_if_requested() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(ALLOW_FIREWALL_ARG) {
        let name = args.get(2).map_or(RULE_NAME, String::as_str);
        if !repair_name_allowed(name) {
            std::process::exit(2);
        }
        std::process::exit(allow_through_firewall(name));
    }
}

#[cfg(not(windows))]
fn allow_through_firewall(_name: &str) -> i32 {
    1
}

/// The elevated repair. It reads nothing from the network: the rule name is
/// Setup's (or a test's `EM-TEST` name), the executable is this one, and the
/// rules it removes are local Block rules for exactly this executable.
#[cfg(windows)]
fn allow_through_firewall(name: &str) -> i32 {
    use std::os::windows::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return 2;
    };
    let exe = crate::single_instance::canonical(&exe)
        .display()
        .to_string();
    let mut code = 0;
    // A block wins over Setup's allow, so this executable's own blocks go
    // first: what dismissing Windows' "allow access" prompt leaves behind.
    let blocks: Vec<LocalBlock> = powershell(LOCAL_BLOCKS_QUERY, &[])
        .ok()
        .and_then(|output| {
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).ok()
        })
        .unwrap_or_default();
    let ids = blocks_to_remove(&blocks, &exe);
    if !ids.is_empty() {
        let removed = powershell(REMOVE_RULES, &[("ETERNAL_FW_RULE_IDS", &ids.join("\n"))]);
        if !removed.is_ok_and(|output| output.status.success()) {
            code = 1;
        }
    }
    println!("removed_blocks={}", ids.len());
    for args in allow_commands(name, &exe) {
        let add = args.get(2).map(String::as_str) == Some("add");
        let status = std::process::Command::new("netsh.exe")
            .args(&args)
            .creation_flags(0x0800_0000)
            .status();
        if add && !status.is_ok_and(|status| status.success()) {
            code = 1;
        }
    }
    code
}

#[cfg(not(windows))]
fn run_elevated_repair() -> Result<u32, String> {
    Err("Windows Firewall exists only on Windows".to_string())
}

#[cfg(windows)]
fn run_elevated_repair() -> Result<u32, String> {
    use windows::core::{w, HSTRING};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let file = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(ALLOW_FIREWALL_ARG);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: windows::core::PCWSTR(file.as_ptr()),
        lpParameters: windows::core::PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // Fails when the user declines the prompt.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|error| error.to_string())?;
    if info.hProcess.is_invalid() {
        return Err("Windows started no process".to_string());
    }
    let mut code = 0;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        let result = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        result.map_err(|error| error.to_string())?;
    }
    Ok(code)
}

/// The UDP port this host listens on, for the firewall check.
static PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(9876);

fn query_firewall() -> Option<Firewall> {
    query_firewall_for(
        &std::env::current_exe().ok()?,
        PORT.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Runs one of this module's fixed scripts. Values reach it only through
/// environment variables, never through the script text.
#[cfg(windows)]
fn powershell(script: &str, env: &[(&str, &str)]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .envs(env.iter().copied())
        .creation_flags(0x0800_0000)
        .output()
}

#[cfg(not(windows))]
pub fn query_firewall_for(_exe: &std::path::Path, _port: u16) -> Option<Firewall> {
    None
}

/// The firewall as it applies to `exe` listening on UDP `port`.
#[cfg(windows)]
pub fn query_firewall_for(exe: &std::path::Path, port: u16) -> Option<Firewall> {
    let exe = crate::single_instance::canonical(exe).display().to_string();
    match powershell(FIREWALL_QUERY, &[("ETERNAL_FW_EXE", &exe)]) {
        Ok(output) => {
            let parsed = parse_firewall(&String::from_utf8_lossy(&output.stdout), &exe, port);
            if parsed.is_none() {
                warn!(
                    code = ?output.status.code(),
                    "Could not read the Windows Firewall settings"
                );
            }
            parsed
        }
        Err(error) => {
            warn!(%error, "Could not start PowerShell to read the firewall settings");
            None
        }
    }
}

#[cfg(not(windows))]
pub fn adapters() -> Vec<Adapter> {
    Vec::new()
}

/// Every adapter with its IPv4 addresses, from the IP Helper API.
#[cfg(windows)]
pub fn adapters() -> Vec<Adapter> {
    use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
        GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    // u64 elements keep the structures aligned.
    let mut buffer: Vec<u64>;
    loop {
        buffer = vec![0u64; (size as usize).div_ceil(8)];
        let result = unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                flags,
                None,
                Some(buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
                &mut size,
            )
        };
        if result == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if result != 0 {
            warn!(code = result, "Could not list the network adapters");
            return Vec::new();
        }
        break;
    }

    let mut out = Vec::new();
    let mut next = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !next.is_null() {
        let adapter = unsafe { &*next };
        let text = |text: windows::core::PWSTR| {
            if text.is_null() {
                String::new()
            } else {
                unsafe { text.to_string() }.unwrap_or_default()
            }
        };
        let mut ipv4 = Vec::new();
        let mut unicast = adapter.FirstUnicastAddress;
        while !unicast.is_null() {
            let address = unsafe { &*unicast };
            let sockaddr = address.Address.lpSockaddr;
            if !sockaddr.is_null() && unsafe { (*sockaddr).sa_family } == AF_INET {
                let sin = unsafe { &*sockaddr.cast::<SOCKADDR_IN>() };
                let raw = unsafe { sin.sin_addr.S_un.S_addr };
                ipv4.push(Ipv4Addr::from(raw.to_ne_bytes()));
            }
            unicast = address.Next;
        }
        out.push(Adapter {
            kind: classify(
                adapter.IfType,
                &text(adapter.Description),
                &text(adapter.FriendlyName),
            ),
            up: adapter.OperStatus == IfOperStatusUp,
            gateway: !adapter.FirstGatewayAddress.is_null(),
            ipv4,
        });
        next = adapter.Next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(kind: Kind, gateway: bool, ip: [u8; 4]) -> Adapter {
        Adapter {
            kind,
            up: true,
            gateway,
            ipv4: vec![Ipv4Addr::from(ip)],
        }
    }

    #[test]
    fn adapters_are_told_apart_by_type_and_description() {
        assert_eq!(
            classify(71, "Intel(R) Wi-Fi 6 AX201 160MHz", "Wi-Fi"),
            Kind::Wifi
        );
        assert_eq!(
            classify(6, "Realtek PCIe GbE Family Controller", "Ethernet"),
            Kind::Ethernet
        );
        assert_eq!(
            classify(6, "Hyper-V Virtual Ethernet Adapter", "vEthernet (WSL)"),
            Kind::Virtual
        );
        assert_eq!(
            classify(6, "VirtualBox Host-Only Ethernet Adapter", "Ethernet 3"),
            Kind::Virtual
        );
        assert_eq!(classify(53, "Tailscale Tunnel", "Tailscale"), Kind::Vpn);
        assert_eq!(
            classify(6, "TAP-Windows Adapter V9", "Ethernet 2"),
            Kind::Vpn
        );
        assert_eq!(
            classify(6, "Famatech Radmin VPN Ethernet Adapter", "Radmin VPN"),
            Kind::Vpn
        );
        assert_eq!(
            classify(131, "Microsoft Teredo Tunneling Adapter", "Teredo"),
            Kind::Vpn
        );
        assert_eq!(
            classify(6, "Bluetooth Device (Personal Area Network)", "Bluetooth"),
            Kind::Virtual
        );
    }

    /// A VPN or a WSL switch can own the default route. The Address and QR
    /// code must still be the home network's.
    #[test]
    fn the_home_network_comes_before_vpn_and_virtual_switches() {
        let adapters = [
            adapter(Kind::Vpn, true, [10, 41, 107, 216]),
            adapter(Kind::Virtual, false, [172, 21, 0, 1]),
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
            adapter(Kind::Ethernet, false, [192, 168, 50, 2]),
        ];
        assert_eq!(
            lan_addresses(&adapters),
            vec![
                Ipv4Addr::new(192, 168, 1, 20),
                Ipv4Addr::new(192, 168, 50, 2)
            ]
        );
    }

    /// With a Hyper-V external switch the LAN address lives on the virtual
    /// adapter; it must still be shown.
    #[test]
    fn a_virtual_switch_that_carries_the_network_counts() {
        let adapters = [
            adapter(Kind::Virtual, true, [192, 168, 1, 30]),
            adapter(Kind::Virtual, false, [172, 30, 16, 1]),
        ];
        assert_eq!(
            lan_addresses(&adapters),
            vec![Ipv4Addr::new(192, 168, 1, 30)]
        );
    }

    #[test]
    fn down_and_link_local_adapters_are_left_out() {
        let mut down = adapter(Kind::Ethernet, true, [192, 168, 1, 5]);
        down.up = false;
        let adapters = [
            down,
            adapter(Kind::Wifi, false, [169, 254, 10, 2]),
            adapter(Kind::Vpn, true, [100, 80, 1, 2]),
        ];
        assert!(lan_addresses(&adapters).is_empty());
    }

    const EXE: &str = r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe";

    fn firewall(active: u8, enabled: u8, rules: Vec<Rule>) -> Firewall {
        Firewall {
            active,
            enabled,
            exe: normalize_path(EXE),
            port: 9876,
            rules,
            products: Vec::new(),
        }
    }

    /// Setup's rule shape: this executable, every port.
    fn rule(allow: bool, profiles: u8, udp: bool) -> Rule {
        Rule {
            enabled: true,
            allow,
            profiles,
            udp,
            program: Some(EXE.to_string()),
            ports: Ports::Any,
            scoped: false,
            local: true,
        }
    }

    fn any_program(allow: bool, ports: &[&str]) -> Rule {
        Rule {
            program: None,
            ports: Ports::parse(
                &ports
                    .iter()
                    .map(|port| port.to_string())
                    .collect::<Vec<_>>(),
            ),
            ..rule(allow, PROFILE_ALL, true)
        }
    }

    #[test]
    fn setups_rules_let_the_ipad_in_on_every_network() {
        let setup = vec![
            rule(true, PROFILE_ALL, true),
            rule(true, PROFILE_ALL, false),
        ];
        for active in [PROFILE_PUBLIC, PROFILE_PRIVATE, PROFILE_DOMAIN] {
            assert_eq!(
                verdict(&firewall(active, PROFILE_ALL, setup.clone())),
                Verdict::Allowed
            );
        }
    }

    #[test]
    fn missing_rules_and_this_programs_blocks_are_found() {
        assert_eq!(
            verdict(&firewall(PROFILE_PUBLIC, PROFILE_ALL, vec![])),
            Verdict::Missing
        );
        // What Windows' own prompt leaves when only Private was ticked.
        let prompt = vec![
            rule(true, PROFILE_PRIVATE, true),
            rule(false, PROFILE_PUBLIC, true),
        ];
        assert_eq!(
            verdict(&firewall(PROFILE_PRIVATE, PROFILE_ALL, prompt.clone())),
            Verdict::Allowed
        );
        assert_eq!(
            verdict(&firewall(PROFILE_PUBLIC, PROFILE_ALL, prompt)),
            Verdict::Blocked
        );
        // A TCP-only rule doesn't carry the stream.
        assert_eq!(
            verdict(&firewall(
                PROFILE_PRIVATE,
                PROFILE_ALL,
                vec![rule(true, PROFILE_ALL, false)]
            )),
            Verdict::Missing
        );
        let mut disabled = rule(true, PROFILE_ALL, true);
        disabled.enabled = false;
        assert_eq!(
            verdict(&firewall(PROFILE_PRIVATE, PROFILE_ALL, vec![disabled])),
            Verdict::Missing
        );
        // Another program's rules never count.
        let mut other = rule(false, PROFILE_ALL, true);
        other.program = Some(r"C:\Games\game.exe".into());
        assert_eq!(
            verdict(&firewall(
                PROFILE_PRIVATE,
                PROFILE_ALL,
                vec![rule(true, PROFILE_ALL, true), other]
            )),
            Verdict::Allowed
        );
    }

    /// Sol #2: an allow for any program on the port lets the iPad in.
    #[test]
    fn an_any_program_allow_on_the_port_is_no_reason_for_a_notice() {
        for ports in [&["9876"][..], &["9000-9999"], &["80", "9876"], &["Any"]] {
            let check = firewall(PROFILE_PUBLIC, PROFILE_ALL, vec![any_program(true, ports)]);
            assert_eq!(verdict(&check), Verdict::Allowed, "{ports:?}");
            assert!(!verdict(&check).needs_repair());
        }
        // On other ports only: still nothing lets the iPad in.
        let other = firewall(
            PROFILE_PUBLIC,
            PROFILE_ALL,
            vec![any_program(true, &["53", "10000-10010"])],
        );
        assert_eq!(verdict(&other), Verdict::Missing);
    }

    /// Sol #3: a rule for this program on another port doesn't apply.
    #[test]
    fn a_rule_for_another_port_does_not_apply() {
        let mut elsewhere = rule(true, PROFILE_ALL, true);
        elsewhere.ports = Ports::parse(&["9999".to_string()]);
        let check = firewall(PROFILE_PRIVATE, PROFILE_ALL, vec![elsewhere.clone()]);
        assert_eq!(verdict(&check), Verdict::Missing);
        assert!(verdict(&check).needs_repair());
        // And a block on another port blocks nothing here.
        let mut block = elsewhere;
        block.allow = false;
        assert_eq!(
            verdict(&firewall(
                PROFILE_PRIVATE,
                PROFILE_ALL,
                vec![block, rule(true, PROFILE_ALL, true)]
            )),
            Verdict::Allowed
        );
    }

    /// Restrictions this check doesn't evaluate never produce a notice.
    #[test]
    fn rules_it_cannot_evaluate_never_raise_a_notice() {
        let mut scoped = rule(true, PROFILE_ALL, true);
        scoped.scoped = true;
        assert_eq!(
            verdict(&firewall(
                PROFILE_PRIVATE,
                PROFILE_ALL,
                vec![scoped.clone()]
            )),
            Verdict::Unknown
        );
        let mut scoped_block = scoped.clone();
        scoped_block.allow = false;
        assert_eq!(
            verdict(&firewall(
                PROFILE_PRIVATE,
                PROFILE_ALL,
                vec![scoped_block, rule(true, PROFILE_ALL, true)]
            )),
            Verdict::Allowed
        );
        let keyword = any_program(true, &["RPC"]);
        assert_eq!(keyword.ports, Ports::Unknown);
        assert_eq!(
            verdict(&firewall(PROFILE_PRIVATE, PROFILE_ALL, vec![keyword])),
            Verdict::Unknown
        );
        assert!(!Verdict::Unknown.needs_repair());
    }

    /// Sol #1: a block the repair may not change is told apart.
    #[test]
    fn blocks_the_repair_may_not_change_are_told_apart() {
        let setup = rule(true, PROFILE_ALL, true);
        let check = |block: Rule| {
            verdict(&firewall(
                PROFILE_PRIVATE,
                PROFILE_ALL,
                vec![setup.clone(), block],
            ))
        };
        assert_eq!(check(rule(false, PROFILE_ALL, true)), Verdict::Blocked);
        assert_eq!(
            check(any_program(false, &["9876"])),
            Verdict::BlockedByAdmin
        );
        assert_eq!(check(any_program(false, &["Any"])), Verdict::BlockedByAdmin);
        let mut policy = rule(false, PROFILE_ALL, true);
        policy.local = false;
        assert_eq!(check(policy), Verdict::BlockedByAdmin);
        assert!(!Verdict::BlockedByAdmin.needs_repair());
    }

    /// Sol #4: the spellings Windows stores reach the same program.
    #[test]
    fn program_paths_are_compared_as_windows_does() {
        let exe = normalize_path(EXE);
        assert_eq!(normalize_path(&format!(r"\\?\{EXE}")), exe);
        assert_eq!(normalize_path(&format!(r"\??\{EXE}")), exe);
        assert_eq!(normalize_path(&EXE.to_uppercase()), exe);
        assert_eq!(
            normalize_path(r"\\?\UNC\server\share\a.exe"),
            r"\\server\share\a.exe"
        );
        std::env::set_var("ETERNAL_TEST_PROGRAM_FILES", r"C:\Program Files");
        assert_eq!(
            normalize_path(r"%ETERNAL_TEST_PROGRAM_FILES%\EternalMonitor\EternalMonitor-host.exe"),
            exe
        );
        assert_eq!(
            normalize_path("%NO_SUCH_VAR_HERE%\\a.exe"),
            "%no_such_var_here%\\a.exe"
        );
        let mut written = rule(true, PROFILE_ALL, true);
        written.program = Some(format!(r"\\?\{}", EXE.to_uppercase()));
        assert_eq!(
            verdict(&firewall(PROFILE_PUBLIC, PROFILE_ALL, vec![written])),
            Verdict::Allowed
        );
    }

    /// A short (8.3) name on a real Windows volume reaches the long one.
    #[cfg(windows)]
    #[test]
    fn short_names_are_made_long() {
        let long = normalize_path(r"C:\Program Files");
        let short = normalize_path(r"C:\PROGRA~1");
        if short.contains('~') {
            eprintln!("8.3 names are off on C:, nothing to compare");
            return;
        }
        assert_eq!(short, long);
    }

    /// Sol #1: Allow removes this program's own local blocks and nothing else.
    #[test]
    fn the_repair_removes_only_this_programs_blocks() {
        let blocks = [
            LocalBlock {
                id: "{A}".into(),
                program: format!(r"\\?\{}", EXE.to_lowercase()),
            },
            LocalBlock {
                id: "{B}".into(),
                program: r"C:\Games\game.exe".into(),
            },
            LocalBlock {
                id: "TCP Query User{C}".into(),
                program: EXE.into(),
            },
            LocalBlock {
                id: "bad\nid".into(),
                program: EXE.into(),
            },
            LocalBlock {
                id: String::new(),
                program: EXE.into(),
            },
        ];
        assert_eq!(
            blocks_to_remove(&blocks, EXE),
            vec!["{A}", "TCP Query User{C}"]
        );
        assert!(repair_name_allowed(RULE_NAME));
        assert!(repair_name_allowed("EM-TEST connect-funnel"));
        for name in [
            "Other",
            "EM-TEST a\" dir=out",
            "EM-TEST a\nb",
            "EternalMonitor Host X",
        ] {
            assert!(!repair_name_allowed(name), "{name}");
        }
    }

    #[test]
    fn a_firewall_that_is_off_or_no_network_needs_nothing() {
        assert_eq!(
            verdict(&firewall(PROFILE_PUBLIC, PROFILE_PRIVATE, vec![])),
            Verdict::Off
        );
        assert_eq!(
            verdict(&firewall(0, PROFILE_ALL, vec![])),
            Verdict::NoNetwork
        );
    }

    #[test]
    fn powershell_output_is_read() {
        let json = r#"{"profiles":["Public"],"firewall":[{"name":"Domain","enabled":"True"},{"name":"Private","enabled":"True"},{"name":"Public","enabled":"True"}],"rules":[{"enabled":"True","action":"Allow","profile":"Domain, Private, Public","protocol":"UDP","program":"C:\\PROGRAM FILES\\EternalMonitor\\EternalMonitor-host.exe","localport":["Any"],"remote":["Any"],"localaddress":["Any"],"service":"Any","interface":"Any","owner":"","package":"","store":"Local"},{"enabled":"True","action":"Block","profile":"Public","protocol":"UDP","program":"Any","localport":["9876"],"remote":["LocalSubnet"],"localaddress":["Any"],"service":"Any","interface":"Any","owner":"","package":"","store":"GroupPolicy"}],"products":["Windows Defender Firewall","Norton Firewall"]}"#;
        let parsed = parse_firewall(json, EXE, 9876).expect("parsed");
        assert_eq!(parsed.active, PROFILE_PUBLIC);
        assert_eq!(parsed.enabled, PROFILE_ALL);
        assert_eq!(parsed.rules.len(), 2);
        assert!(parsed.rules[0].udp && parsed.rules[0].allow && !parsed.rules[0].scoped);
        assert_eq!(parsed.rules[1].program, None);
        assert!(parsed.rules[1].scoped && !parsed.rules[1].local);
        assert_eq!(parsed.rules[1].ports, Ports::List(vec![(9876, 9876)]));
        assert_eq!(parsed.products, vec!["Norton Firewall".to_string()]);
        // The scoped block may or may not apply: no notice.
        assert_eq!(verdict(&parsed), Verdict::Allowed);

        let empty = r#"{"profiles":["DomainAuthenticated","Private"],"firewall":[],"rules":[],"products":[]}"#;
        let parsed = parse_firewall(empty, EXE, 9876).expect("parsed");
        assert_eq!(parsed.active, PROFILE_DOMAIN | PROFILE_PRIVATE);
        assert_eq!(verdict(&parsed), Verdict::Off);
        assert!(parse_firewall("", EXE, 9876).is_none());
    }

    #[test]
    fn the_summary_names_no_address_or_network() {
        let adapters = [
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
            adapter(Kind::Vpn, true, [10, 41, 107, 216]),
        ];
        let firewall = Firewall {
            products: vec!["Norton Firewall".into()],
            ..firewall(PROFILE_PUBLIC, PROFILE_ALL, vec![])
        };
        let line = summary(&adapters, Some(&firewall));
        assert_eq!(
            line,
            "adapters=[vpn:up+gw+ipv4 wifi:up+gw+ipv4] lan_addresses=1 first_address=192.168/16 \
             mdns_on=[vpn wifi] network=public windows_firewall_on=domain+private+public \
             eternal_rules=missing rules_found=0 other_firewall=[Norton Firewall]"
        );
        assert!(!line.contains("192.168.1") && !line.contains("10.41"));
        assert!(summary(&[], None).ends_with("firewall=unknown"));
    }

    #[test]
    fn the_repair_writes_setups_rules_for_this_executable() {
        let exe = r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe";
        let commands = allow_commands(RULE_NAME, exe);
        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[0],
            [
                "advfirewall",
                "firewall",
                "delete",
                "rule",
                "name=EternalMonitor Host UDP"
            ]
        );
        assert_eq!(
            commands[1].join(" "),
            "advfirewall firewall add rule name=EternalMonitor Host UDP dir=in action=allow \
             program=C:\\Program Files\\EternalMonitor\\EternalMonitor-host.exe protocol=UDP \
             profile=domain,private,public enable=yes"
        );
        assert!(commands[3].contains(&"protocol=TCP".to_string()));
    }

    /// On a real PC: the startup summary, and the firewall verdict for
    /// `ETERNAL_FIREWALL_EXE` (default: this test binary). Run with
    /// `cargo test -p eternal-host --lib native_network -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn native_network_summary_and_firewall() {
        let exe = std::env::var_os("ETERNAL_FIREWALL_EXE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_exe().unwrap());
        let port = std::env::var("ETERNAL_FIREWALL_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(9876);
        let firewall = query_firewall_for(&exe, port).expect("firewall settings");
        let adapters = adapters();
        println!("NATIVE adapters={}", adapters.len());
        println!("NATIVE lan_addresses={}", lan_addresses(&adapters).len());
        println!("NATIVE summary: {}", summary(&adapters, Some(&firewall)));
        println!(
            "NATIVE verdict={:?} rules={:?}",
            verdict(&firewall),
            firewall.rules
        );
        let notice = crate::gui::firewall_notice(FirewallCheck::Checked(verdict(&firewall)), true);
        println!(
            "NATIVE notice={:?}",
            notice.map(|(title, _, allow)| (title, allow))
        );
    }
}
