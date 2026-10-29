//! A hint under the Address row when the shown address is a VPN's or a
//! virtual switch's.
//!
//! The Address row and the QR code show what they always have: the address
//! the OS would route toward the internet (`gui::detect_local_ip`). With a
//! VPN or a Hyper-V/WSL switch owning that route, an iPad on the home Wi-Fi
//! may not reach it, so one more line offers the home network's Wi-Fi or
//! Ethernet address. A wrong guess here can only show or hide that line.
//!
//! The startup line counts the addresses by kind. It never logs an address or
//! an adapter's name: uploads carry it, and adapter names can name the user
//! or the PC.
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wifi,
    Ethernet,
    /// Hyper-V (WSL, Docker), VirtualBox and VMware switches.
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

/// How likely an iPad on the home Wi-Fi is to reach an address, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// Wi-Fi or Ethernet that is up with a default gateway, in 192.168/16,
    /// 10/8 or 172.16/12.
    Lan,
    Other,
    Virtual,
    Vpn,
    /// 169.254/16: the adapter got no answer from DHCP.
    LinkLocal,
}

impl Reach {
    fn label(self) -> &'static str {
        match self {
            Reach::Lan => "lan",
            Reach::Other => "other",
            Reach::Virtual => "virtual",
            Reach::Vpn => "vpn",
            Reach::LinkLocal => "link_local",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ranked {
    pub ip: Ipv4Addr,
    pub reach: Reach,
    pub kind: Kind,
}

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_PPP: u32 = 23;
const IF_TYPE_LOOPBACK: u32 = 24;
const IF_TYPE_PROP_VIRTUAL: u32 = 53;
const IF_TYPE_WIFI: u32 = 71;
const IF_TYPE_TUNNEL: u32 = 131;

/// VPN drivers that report an Ethernet type, by how their description
/// starts.
const VPN_DRIVERS: &[&str] = &[
    "tailscale",
    "wireguard",
    "wintun",
    "zerotier",
    "tap-windows",
    "openvpn",
    "cisco anyconnect",
    "pangp",
    "fortinet",
    "logmein hamachi",
];
/// Virtual switches that report an Ethernet type, by how their description
/// starts. WSL's and Docker's are Hyper-V switches.
const VIRTUAL_DRIVERS: &[&str] = &[
    "hyper-v virtual ethernet adapter",
    "virtualbox host-only",
    "vmware virtual ethernet adapter",
];
/// Hyper-V names its switches' adapters this way: "vEthernet (WSL)".
const HYPER_V_NAME: &str = "vEthernet (";

/// What kind of adapter Windows reports. The interface type decides first;
/// only an Ethernet type is looked at further, by the start of its driver's
/// description and Hyper-V's own naming. A name the user gave, such as
/// "VPN bypass", never makes a network card a VPN.
pub fn classify(if_type: u32, description: &str, name: &str) -> Kind {
    match if_type {
        IF_TYPE_WIFI => return Kind::Wifi,
        IF_TYPE_PPP | IF_TYPE_TUNNEL | IF_TYPE_PROP_VIRTUAL => return Kind::Vpn,
        IF_TYPE_LOOPBACK => return Kind::Virtual,
        IF_TYPE_ETHERNET => {}
        _ => return Kind::Other,
    }
    let description = description.to_lowercase();
    let starts = |drivers: &[&str]| drivers.iter().any(|d| description.starts_with(d));
    if starts(VPN_DRIVERS) {
        Kind::Vpn
    } else if starts(VIRTUAL_DRIVERS) || name.starts_with(HYPER_V_NAME) {
        Kind::Virtual
    } else {
        Kind::Ethernet
    }
}

fn reach(adapter: &Adapter, ip: Ipv4Addr) -> Reach {
    if ip.is_link_local() {
        Reach::LinkLocal
    } else if adapter.kind == Kind::Vpn {
        Reach::Vpn
    } else if adapter.kind == Kind::Virtual {
        Reach::Virtual
    } else if adapter.up
        && adapter.gateway
        && matches!(adapter.kind, Kind::Wifi | Kind::Ethernet)
        && ip.is_private()
    {
        Reach::Lan
    } else {
        Reach::Other
    }
}

/// Every IPv4 address on the PC, best first, in adapter order within a
/// reach. Ranking drops none; loopback is left out because it is never
/// offered to an iPad.
pub fn rank(adapters: &[Adapter]) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = adapters
        .iter()
        .flat_map(|adapter| {
            adapter
                .ipv4
                .iter()
                .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
                .map(|&ip| Ranked {
                    ip,
                    reach: reach(adapter, ip),
                    kind: adapter.kind,
                })
        })
        .collect();
    ranked.sort_by_key(|address| address.reach);
    ranked
}

/// The home network address to offer under `shown`: only when `shown` is on
/// a VPN or virtual adapter and another address is on Wi-Fi or Ethernet that
/// is up with a default gateway, in a private range.
pub fn home_hint(adapters: &[Adapter], shown: Ipv4Addr) -> Option<Ipv4Addr> {
    let tunnel = adapters
        .iter()
        .any(|a| matches!(a.kind, Kind::Vpn | Kind::Virtual) && a.ipv4.contains(&shown));
    if !tunnel {
        return None;
    }
    rank(adapters)
        .into_iter()
        .find(|address| address.reach == Reach::Lan && address.ip != shown)
        .map(|address| address.ip)
}

/// How long a listing answers for the same shown address.
const HINT_TTL: Duration = Duration::from_secs(30);

/// The Address row's hint, listed on its own thread at most every 30 s per
/// shown address. Drawing the window and opening the QR code never wait for
/// it, and no lock is held while Windows lists the adapters: the worker
/// takes only this cache's lock, to store what it found.
pub struct HintCache {
    list: fn() -> Option<Vec<Adapter>>,
    ttl: Duration,
    state: Arc<Mutex<HintState>>,
}

#[derive(Default)]
struct HintState {
    /// The shown address last listed for, when, and its hint.
    checked: Option<(Ipv4Addr, Instant, Option<Ipv4Addr>)>,
    listing: bool,
}

impl Default for HintCache {
    fn default() -> Self {
        Self::with(listed, HINT_TTL)
    }
}

impl HintCache {
    fn with(list: fn() -> Option<Vec<Adapter>>, ttl: Duration) -> Self {
        Self {
            list,
            ttl,
            state: Arc::default(),
        }
    }

    /// The hint for `shown` from the last listing. A missing or stale one
    /// starts a listing and reads as before (or none) until it is done.
    pub fn get(&self, shown: Ipv4Addr) -> Option<Ipv4Addr> {
        let (hint, start) = {
            let mut state = self.state.lock();
            let (hint, stale) = match state.checked {
                Some((ip, at, hint)) if ip == shown => (hint, at.elapsed() >= self.ttl),
                _ => (None, true),
            };
            let start = stale && !state.listing;
            state.listing |= start;
            (hint, start)
        };
        if start {
            let (list, state) = (self.list, self.state.clone());
            let spawned = std::thread::Builder::new()
                .name("address-hint".into())
                .spawn(move || {
                    let hint = list().and_then(|adapters| home_hint(&adapters, shown));
                    *state.lock() = HintState {
                        checked: Some((shown, Instant::now(), hint)),
                        listing: false,
                    };
                });
            if spawned.is_err() {
                self.state.lock().listing = false;
            }
        }
        hint
    }
}

/// The local address the OS would use toward a public address.
pub fn default_route() -> Option<IpAddr> {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("8.8.8.8:80")?;
            socket.local_addr()
        })
        .ok()
        .map(|addr| addr.ip())
}

/// The startup line: address counts by reach, the default route's reach and
/// the kinds of adapters with an address. No address and no adapter name.
pub fn summary(adapters: Option<&[Adapter]>, route: Option<IpAddr>) -> String {
    let Some(adapters) = adapters else {
        return "Network: addresses unavailable".to_string();
    };
    let ranked = rank(adapters);
    let count = |reach: Reach| ranked.iter().filter(|a| a.reach == reach).count();
    let route = match route {
        None => "none",
        Some(ip) => ranked
            .iter()
            .find(|address| IpAddr::V4(address.ip) == ip)
            .map_or("unknown", |address| address.reach.label()),
    };
    let mut kinds: Vec<&str> = ranked.iter().map(|address| address.kind.label()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    format!(
        "Network: addresses lan={} other={} virtual={} vpn={} link_local={} route={route} adapters=[{}]",
        count(Reach::Lan),
        count(Reach::Other),
        count(Reach::Virtual),
        count(Reach::Vpn),
        count(Reach::LinkLocal),
        kinds.join(" "),
    )
}

pub fn log_addresses() {
    info!("{}", summary(adapters().as_deref(), default_route()));
}

/// Debug builds only: the adapters a GUI test has the hint read.
#[cfg(debug_assertions)]
static TEST_ADAPTERS: Mutex<Option<Vec<Adapter>>> = Mutex::new(None);

/// Debug builds only: has the Address row's hint read `adapters` instead of
/// the PC's, until called with `None`.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_test_adapters(adapters: Option<Vec<Adapter>>) {
    *TEST_ADAPTERS.lock() = adapters;
}

fn listed() -> Option<Vec<Adapter>> {
    #[cfg(debug_assertions)]
    if let Some(adapters) = TEST_ADAPTERS.lock().clone() {
        return Some(adapters);
    }
    adapters()
}

#[cfg(not(windows))]
pub fn adapters() -> Option<Vec<Adapter>> {
    None
}

/// Calls `list` with a buffer of `size` bytes, growing it to the size the
/// call asks for at most three times: adapters that keep appearing must not
/// keep the caller listing. u64 elements keep the structures aligned.
#[cfg(any(windows, test))]
fn fill(mut list: impl FnMut(&mut [u64], &mut u32) -> u32) -> Option<Vec<u64>> {
    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    const GROWS: usize = 3;
    let mut size: u32 = 16 * 1024;
    for _ in 0..=GROWS {
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        match list(&mut buffer, &mut size) {
            0 => return Some(buffer),
            ERROR_BUFFER_OVERFLOW => {}
            code => {
                tracing::warn!(code, "Could not list the network adapters");
                return None;
            }
        }
    }
    tracing::warn!("Could not list the network adapters: the list kept growing");
    None
}

/// Every adapter with its IPv4 addresses, from the IP Helper API. `None`
/// when Windows can't list them.
#[cfg(windows)]
pub fn adapters() -> Option<Vec<Adapter>> {
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
    let buffer = fill(|buffer, size| unsafe {
        GetAdaptersAddresses(
            AF_INET.0 as u32,
            flags,
            None,
            Some(buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
            size,
        )
    })?;

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
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn adapter(kind: Kind, gateway: bool, ip: [u8; 4]) -> Adapter {
        Adapter {
            kind,
            up: true,
            gateway,
            ipv4: vec![Ipv4Addr::from(ip)],
        }
    }

    fn ip(ip: [u8; 4]) -> Ipv4Addr {
        Ipv4Addr::from(ip)
    }

    #[test]
    fn adapters_are_told_apart_by_type_then_driver() {
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
            classify(
                6,
                "Hyper-V Virtual Ethernet Adapter #2",
                "vEthernet (Default Switch)"
            ),
            Kind::Virtual
        );
        // Hyper-V's naming alone, whatever the driver says.
        assert_eq!(
            classify(6, "Microsoft Network Adapter", "vEthernet (DockerNAT)"),
            Kind::Virtual
        );
        assert_eq!(
            classify(6, "VirtualBox Host-Only Ethernet Adapter", "Ethernet 3"),
            Kind::Virtual
        );
        assert_eq!(
            classify(
                6,
                "VMware Virtual Ethernet Adapter for VMnet1",
                "VMware Network Adapter VMnet1"
            ),
            Kind::Virtual
        );
        assert_eq!(classify(53, "Tailscale Tunnel", "Tailscale"), Kind::Vpn);
        assert_eq!(classify(53, "WireGuard Tunnel", "wg0"), Kind::Vpn);
        assert_eq!(
            classify(6, "ZeroTier Virtual Port", "ZeroTier One"),
            Kind::Vpn
        );
        assert_eq!(
            classify(6, "TAP-Windows Adapter V9", "Ethernet 2"),
            Kind::Vpn
        );
        assert_eq!(
            classify(131, "Microsoft Teredo Tunneling Adapter", "Teredo"),
            Kind::Vpn
        );
        assert_eq!(classify(23, "WAN Miniport (PPPOE)", "Broadband"), Kind::Vpn);
        assert_eq!(
            classify(24, "Software Loopback Interface 1", ""),
            Kind::Virtual
        );
    }

    /// Names are the user's: a network card called after a VPN or Docker is
    /// still a network card, and a VPN's name on Wi-Fi is still Wi-Fi.
    #[test]
    fn a_network_card_keeps_its_kind_whatever_it_is_named() {
        for name in [
            "VPN bypass",
            "Docker",
            "Tailscale backup",
            "WSL",
            "vEthernet",
        ] {
            assert_eq!(
                classify(6, "Realtek PCIe GbE Family Controller", name),
                Kind::Ethernet,
                "{name}"
            );
            assert_eq!(
                classify(71, "Intel(R) Wi-Fi 6 AX201 160MHz", name),
                Kind::Wifi,
                "{name}"
            );
        }
        // A driver's description is matched from its start only.
        assert_eq!(
            classify(6, "Intel(R) Ethernet for VPN appliances", "Ethernet"),
            Kind::Ethernet
        );
        let renamed = classify(6, "Realtek PCIe GbE Family Controller", "VPN bypass");
        let adapters = [
            adapter(renamed, true, [192, 168, 1, 5]),
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
        ];
        assert_eq!(home_hint(&adapters, ip([192, 168, 1, 5])), None);
    }

    /// Tailscale with an exit node owns the default route.
    #[test]
    fn tailscale_gets_the_wifi_address_as_a_hint() {
        let adapters = [
            adapter(Kind::Vpn, false, [100, 80, 1, 2]),
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
        ];
        let ranked = rank(&adapters);
        assert_eq!(ranked[0].ip, ip([192, 168, 1, 20]));
        assert_eq!(ranked[1].reach, Reach::Vpn);
        assert_eq!(
            home_hint(&adapters, ip([100, 80, 1, 2])),
            Some(ip([192, 168, 1, 20]))
        );
        // Shown on Wi-Fi already: nothing to add.
        assert_eq!(home_hint(&adapters, ip([192, 168, 1, 20])), None);
    }

    /// Some ISPs hand out 100.64/10 addresses. That network card is the
    /// home network, not a VPN.
    #[test]
    fn cgnat_on_a_network_card_is_not_a_vpn() {
        let adapters = [
            adapter(Kind::Ethernet, true, [100, 100, 7, 1]),
            adapter(Kind::Ethernet, true, [10, 0, 0, 9]),
        ];
        assert!(rank(&adapters).iter().all(|a| a.reach != Reach::Vpn));
        assert_eq!(home_hint(&adapters, ip([100, 100, 7, 1])), None);
    }

    #[test]
    fn a_hyper_v_switch_gets_the_ethernet_address_as_a_hint() {
        let adapters = [
            adapter(Kind::Virtual, false, [172, 21, 0, 1]),
            adapter(Kind::Ethernet, true, [192, 168, 0, 14]),
        ];
        assert_eq!(rank(&adapters)[0].kind, Kind::Ethernet);
        assert_eq!(
            home_hint(&adapters, ip([172, 21, 0, 1])),
            Some(ip([192, 168, 0, 14]))
        );
    }

    #[test]
    fn wifi_comes_before_docker_and_wsl() {
        let adapters = [
            adapter(Kind::Virtual, false, [172, 17, 0, 1]),
            adapter(Kind::Virtual, false, [172, 28, 96, 1]),
            adapter(Kind::Wifi, true, [10, 0, 0, 5]),
        ];
        let ranked: Vec<(Ipv4Addr, Reach)> = rank(&adapters)
            .into_iter()
            .map(|address| (address.ip, address.reach))
            .collect();
        assert_eq!(
            ranked,
            [
                (ip([10, 0, 0, 5]), Reach::Lan),
                (ip([172, 17, 0, 1]), Reach::Virtual),
                (ip([172, 28, 96, 1]), Reach::Virtual),
            ]
        );
        assert_eq!(
            home_hint(&adapters, ip([172, 17, 0, 1])),
            Some(ip([10, 0, 0, 5]))
        );
    }

    #[test]
    fn only_a_vpn_gets_no_hint() {
        let adapters = [adapter(Kind::Vpn, true, [10, 8, 0, 2])];
        assert_eq!(rank(&adapters)[0].reach, Reach::Vpn);
        assert_eq!(home_hint(&adapters, ip([10, 8, 0, 2])), None);
    }

    #[test]
    fn link_local_only_gets_no_hint() {
        let adapters = [
            adapter(Kind::Vpn, false, [10, 8, 0, 2]),
            adapter(Kind::Wifi, false, [169, 254, 10, 2]),
        ];
        assert_eq!(rank(&adapters)[1].reach, Reach::LinkLocal);
        assert_eq!(home_hint(&adapters, ip([10, 8, 0, 2])), None);
    }

    /// The address with a default gateway is offered over one without.
    /// Between two home networks there is nothing to add.
    #[test]
    fn of_several_lans_the_one_with_a_gateway_is_offered() {
        let adapters = [
            adapter(Kind::Ethernet, false, [192, 168, 50, 2]),
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
            adapter(Kind::Vpn, true, [10, 41, 107, 216]),
        ];
        assert_eq!(rank(&adapters)[0].ip, ip([192, 168, 1, 20]));
        assert_eq!(
            home_hint(&adapters, ip([10, 41, 107, 216])),
            Some(ip([192, 168, 1, 20]))
        );

        let two = [
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
            adapter(Kind::Ethernet, true, [10, 0, 0, 7]),
        ];
        assert_eq!(home_hint(&two, ip([10, 0, 0, 7])), None);
    }

    #[test]
    fn a_down_adapter_is_not_the_home_network() {
        let mut down = adapter(Kind::Ethernet, true, [192, 168, 1, 5]);
        down.up = false;
        assert_eq!(rank(&[down.clone()])[0].reach, Reach::Other);
        let adapters = [adapter(Kind::Vpn, true, [10, 8, 0, 2]), down];
        assert_eq!(home_hint(&adapters, ip([10, 8, 0, 2])), None);
    }

    /// An address on no listed adapter (the list changed since) gets none.
    #[test]
    fn an_unlisted_address_gets_no_hint() {
        let adapters = [adapter(Kind::Wifi, true, [192, 168, 1, 20])];
        assert_eq!(home_hint(&adapters, ip([100, 80, 1, 2])), None);
    }

    /// Ranking only orders the PC's addresses; mDNS advertises every one of
    /// them regardless (see `discovery`).
    #[test]
    fn ranking_keeps_every_address() {
        let adapters = [
            adapter(Kind::Vpn, false, [100, 80, 1, 2]),
            Adapter {
                kind: Kind::Wifi,
                up: true,
                gateway: true,
                ipv4: vec![ip([192, 168, 1, 20]), ip([169, 254, 3, 4])],
            },
            adapter(Kind::Virtual, false, [127, 0, 0, 1]),
        ];
        let mut ranked: Vec<Ipv4Addr> = rank(&adapters).iter().map(|a| a.ip).collect();
        ranked.sort();
        assert_eq!(
            ranked,
            [
                ip([100, 80, 1, 2]),
                ip([169, 254, 3, 4]),
                ip([192, 168, 1, 20])
            ]
        );
    }

    #[test]
    fn the_summary_names_no_address_or_adapter() {
        let wifi = classify(71, "Intel(R) Wi-Fi 6 AX201", "Wi-Fi (Jane's DESKTOP-JANE)");
        let tailscale = classify(53, "Tailscale Tunnel", "Tailscale");
        let switch = classify(6, "Hyper-V Virtual Ethernet Adapter", "vEthernet (WSL)");
        let adapters = [
            adapter(tailscale, false, [100, 80, 1, 2]),
            adapter(wifi, true, [192, 168, 1, 20]),
            adapter(switch, false, [172, 21, 0, 1]),
        ];
        let line = summary(Some(&adapters), Some(IpAddr::V4(ip([100, 80, 1, 2]))));
        assert_eq!(
            line,
            "Network: addresses lan=1 other=0 virtual=1 vpn=1 link_local=0 route=vpn \
             adapters=[virtual vpn wifi]"
        );
        for private in [
            "100.",
            "192.",
            "172.",
            ".1",
            "Jane",
            "DESKTOP",
            "Intel",
            "Tailscale",
            "WSL",
        ] {
            assert!(!line.contains(private), "{private} in {line}");
        }
        assert_eq!(summary(None, None), "Network: addresses unavailable");
        assert!(summary(Some(&[]), None).contains("route=none adapters=[]"));
    }

    /// Windows says the list grew every time: three bigger buffers, then no
    /// list, so no hint.
    #[test]
    fn listing_gives_up_after_three_bigger_buffers() {
        let mut calls = 0;
        let listed = fill(|buffer, size| {
            calls += 1;
            assert!(buffer.len() * 8 >= *size as usize);
            *size += 4096;
            111
        });
        assert_eq!(listed, None);
        assert_eq!(calls, 4);

        let mut calls = 0;
        let listed = fill(|_, size| {
            calls += 1;
            if calls < 4 {
                *size = 64 * 1024;
                111
            } else {
                0
            }
        });
        assert_eq!(listed.map(|buffer| buffer.len()), Some(64 * 1024 / 8));
        assert_eq!(fill(|_, _| 232), None);
    }

    fn wait_for(cache: &HintCache, shown: Ipv4Addr) -> Option<Ipv4Addr> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(hint) = cache.get(shown) {
                return Some(hint);
            }
            assert!(Instant::now() < deadline, "the hint never came");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn tailscale_and_wifi() -> Option<Vec<Adapter>> {
        Some(vec![
            adapter(Kind::Vpn, false, [100, 80, 1, 2]),
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
        ])
    }

    static GATE: Mutex<()> = Mutex::new(());
    static GATED_CALLS: AtomicUsize = AtomicUsize::new(0);
    static GATED_THREAD: Mutex<Option<String>> = Mutex::new(None);

    fn gated() -> Option<Vec<Adapter>> {
        GATED_CALLS.fetch_add(1, Ordering::SeqCst);
        *GATED_THREAD.lock() = std::thread::current().name().map(str::to_string);
        let _open = GATE.lock();
        tailscale_and_wifi()
    }

    /// The window asks every frame. While Windows is still listing, it gets
    /// no hint at once, from the same single listing, on a thread of its
    /// own: no lock the window holds (`PIPELINE_STATS` included) is held
    /// while listing.
    #[test]
    fn the_window_never_waits_for_the_listing() {
        let cache = HintCache::with(gated, HINT_TTL);
        let shown = ip([100, 80, 1, 2]);
        let closed = GATE.lock();
        for _ in 0..50 {
            let asked = Instant::now();
            assert_eq!(cache.get(shown), None);
            assert!(asked.elapsed() < Duration::from_secs(1));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while GATED_CALLS.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "no listing started");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(GATED_THREAD.lock().as_deref(), Some("address-hint"));
        drop(closed);
        assert_eq!(wait_for(&cache, shown), Some(ip([192, 168, 1, 20])));
        assert_eq!(GATED_CALLS.load(Ordering::SeqCst), 1);
    }

    static COUNTED_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn counted() -> Option<Vec<Adapter>> {
        COUNTED_CALLS.fetch_add(1, Ordering::SeqCst);
        tailscale_and_wifi()
    }

    /// One listing answers for 30 s; another shown address is listed anew.
    #[test]
    fn a_listing_answers_until_it_is_stale() {
        let cache = HintCache::with(counted, HINT_TTL);
        let tailscale = ip([100, 80, 1, 2]);
        assert_eq!(wait_for(&cache, tailscale), Some(ip([192, 168, 1, 20])));
        for _ in 0..100 {
            assert_eq!(cache.get(tailscale), Some(ip([192, 168, 1, 20])));
        }
        assert_eq!(COUNTED_CALLS.load(Ordering::SeqCst), 1);

        // Wi-Fi shown: listed again, and nothing to add.
        let wifi = ip([192, 168, 1, 20]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while COUNTED_CALLS.load(Ordering::SeqCst) < 2 || cache.state.lock().listing {
            assert_eq!(cache.get(wifi), None);
            assert!(Instant::now() < deadline, "no second listing");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(cache.get(wifi), None);
        assert_eq!(COUNTED_CALLS.load(Ordering::SeqCst), 2);
    }

    fn unlisted() -> Option<Vec<Adapter>> {
        None
    }

    /// No list (Windows failed, or not Windows): no hint, ever.
    #[test]
    fn without_a_list_there_is_no_hint() {
        let cache = HintCache::with(unlisted, Duration::ZERO);
        let shown = ip([100, 80, 1, 2]);
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            assert_eq!(cache.get(shown), None);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
