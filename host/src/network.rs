//! Which of the PC's addresses the Address row and QR code show.
//!
//! A VPN or a Hyper-V/WSL switch can own the default route, and an iPad on
//! the home Wi-Fi can't reach those addresses. When the default route isn't
//! the home network, the home network's Wi-Fi or Ethernet address is shown
//! instead. This only chooses what is shown: the host still listens on every
//! address and mDNS still advertises all of them, so an iPad that connects
//! through Tailscale or another VPN keeps working.
//!
//! The startup line counts the addresses by kind. It never logs an address or
//! an adapter's name: uploads carry it, and adapter names can name the user
//! or the PC.
use std::net::{IpAddr, Ipv4Addr};

use tracing::info;

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

/// How likely an iPad on the home Wi-Fi is to reach an address, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// Wi-Fi or Ethernet with a default gateway, in 192.168/16, 10/8 or
    /// 172.16/12.
    Lan,
    Other,
    Virtual,
    /// A VPN or tunnel adapter, or CGNAT 100.64/10 (Tailscale) on any adapter.
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

fn reach(adapter: &Adapter, ip: Ipv4Addr) -> Reach {
    let [a, b, ..] = ip.octets();
    if ip.is_link_local() {
        Reach::LinkLocal
    } else if adapter.kind == Kind::Vpn || (a == 100 && (64..128).contains(&b)) {
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

/// The address to show. The default route's when it is on the home network;
/// otherwise the first home network address; with none (or no adapter list),
/// what was shown before ranking: the default route's, then the first
/// adapter's.
pub fn pick(
    adapters: Option<&[Adapter]>,
    route: Option<IpAddr>,
    first_adapter: impl FnOnce() -> Option<Ipv4Addr>,
) -> Option<IpAddr> {
    let route = route.filter(|ip| !ip.is_unspecified() && !ip.is_loopback());
    let lan: Vec<Ipv4Addr> = adapters
        .map(rank)
        .unwrap_or_default()
        .into_iter()
        .filter(|address| address.reach == Reach::Lan)
        .map(|address| address.ip)
        .collect();
    let home = match route {
        Some(IpAddr::V4(ip)) if lan.contains(&ip) => Some(ip),
        _ => lan.first().copied(),
    };
    home.map(IpAddr::V4)
        .or(route)
        .or_else(|| first_adapter().map(IpAddr::V4))
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

#[cfg(not(windows))]
pub fn adapters() -> Option<Vec<Adapter>> {
    None
}

/// Every adapter with its IPv4 addresses, from the IP Helper API. `None`
/// when Windows can't list them.
#[cfg(windows)]
pub fn adapters() -> Option<Vec<Adapter>> {
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
            tracing::warn!(code = result, "Could not list the network adapters");
            return None;
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
    Some(out)
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

    fn v4(ip: [u8; 4]) -> Option<IpAddr> {
        Some(IpAddr::V4(Ipv4Addr::from(ip)))
    }

    fn none() -> Option<Ipv4Addr> {
        None
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
            classify(
                6,
                "Hyper-V Virtual Ethernet Adapter #2",
                "vEthernet (Default Switch)"
            ),
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
        assert_eq!(
            classify(6, "Bluetooth Device (Personal Area Network)", "Bluetooth"),
            Kind::Virtual
        );
    }

    /// Tailscale with an exit node owns the default route.
    #[test]
    fn wifi_comes_before_tailscale() {
        let adapters = [
            adapter(Kind::Vpn, false, [100, 80, 1, 2]),
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
        ];
        let ranked = rank(&adapters);
        assert_eq!(ranked[0].ip, Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(ranked[1].reach, Reach::Vpn);
        assert_eq!(
            pick(Some(&adapters), v4([100, 80, 1, 2]), none),
            v4([192, 168, 1, 20])
        );
    }

    /// CGNAT ranks as a VPN even when the adapter isn't recognized.
    #[test]
    fn cgnat_on_an_unknown_adapter_is_a_vpn() {
        let adapters = [
            adapter(Kind::Ethernet, true, [100, 100, 7, 1]),
            adapter(Kind::Ethernet, true, [10, 0, 0, 9]),
        ];
        assert_eq!(rank(&adapters)[1].reach, Reach::Vpn);
        assert_eq!(
            pick(Some(&adapters), v4([100, 100, 7, 1]), none),
            v4([10, 0, 0, 9])
        );
    }

    #[test]
    fn ethernet_comes_before_a_hyper_v_switch() {
        let adapters = [
            adapter(Kind::Virtual, false, [172, 21, 0, 1]),
            adapter(Kind::Ethernet, true, [192, 168, 0, 14]),
        ];
        assert_eq!(rank(&adapters)[0].kind, Kind::Ethernet);
        assert_eq!(
            pick(Some(&adapters), v4([172, 21, 0, 1]), none),
            v4([192, 168, 0, 14])
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
                (Ipv4Addr::new(10, 0, 0, 5), Reach::Lan),
                (Ipv4Addr::new(172, 17, 0, 1), Reach::Virtual),
                (Ipv4Addr::new(172, 28, 96, 1), Reach::Virtual),
            ]
        );
        assert_eq!(
            pick(Some(&adapters), v4([172, 17, 0, 1]), none),
            v4([10, 0, 0, 5])
        );
    }

    /// With no home network address, the default route's is shown as before,
    /// then the first adapter's.
    #[test]
    fn only_a_vpn_is_shown_as_before() {
        let adapters = [adapter(Kind::Vpn, true, [10, 8, 0, 2])];
        assert_eq!(rank(&adapters)[0].reach, Reach::Vpn);
        assert_eq!(
            pick(Some(&adapters), v4([10, 8, 0, 2]), none),
            v4([10, 8, 0, 2])
        );
        assert_eq!(
            pick(Some(&adapters), None, || Some(Ipv4Addr::new(10, 8, 0, 2))),
            v4([10, 8, 0, 2])
        );
    }

    #[test]
    fn link_local_only_is_shown_as_before() {
        let adapters = [adapter(Kind::Wifi, false, [169, 254, 10, 2])];
        assert_eq!(rank(&adapters)[0].reach, Reach::LinkLocal);
        assert_eq!(
            pick(Some(&adapters), None, || Some(Ipv4Addr::new(
                169, 254, 10, 2
            ))),
            v4([169, 254, 10, 2])
        );
        assert_eq!(pick(Some(&adapters), None, none), None);
    }

    /// The address with a default gateway wins over one without. Between two
    /// home networks, the default route's stays shown, as before.
    #[test]
    fn of_several_lans_the_one_with_a_gateway_is_shown() {
        let adapters = [
            adapter(Kind::Ethernet, false, [192, 168, 50, 2]),
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
            adapter(Kind::Vpn, true, [10, 41, 107, 216]),
        ];
        assert_eq!(rank(&adapters)[0].ip, Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(
            pick(Some(&adapters), v4([10, 41, 107, 216]), none),
            v4([192, 168, 1, 20])
        );

        let two = [
            adapter(Kind::Wifi, true, [192, 168, 1, 20]),
            adapter(Kind::Ethernet, true, [10, 0, 0, 7]),
        ];
        assert_eq!(pick(Some(&two), v4([10, 0, 0, 7]), none), v4([10, 0, 0, 7]));
        assert_eq!(pick(Some(&two), None, none), v4([192, 168, 1, 20]));
    }

    #[test]
    fn a_down_adapter_is_not_the_home_network() {
        let mut down = adapter(Kind::Ethernet, true, [192, 168, 1, 5]);
        down.up = false;
        assert_eq!(rank(&[down.clone()])[0].reach, Reach::Other);
        assert_eq!(
            pick(Some(&[down]), v4([10, 8, 0, 2]), none),
            v4([10, 8, 0, 2])
        );
    }

    #[test]
    fn without_an_adapter_list_the_address_is_found_as_before() {
        assert_eq!(
            pick(None, v4([10, 8, 0, 2]), || Some(Ipv4Addr::new(
                192, 168, 1, 20
            ))),
            v4([10, 8, 0, 2])
        );
        assert_eq!(
            pick(None, None, || Some(Ipv4Addr::new(192, 168, 1, 20))),
            v4([192, 168, 1, 20])
        );
        assert_eq!(pick(None, v4([0, 0, 0, 0]), none), None);
        assert_eq!(pick(None, v4([127, 0, 0, 1]), none), None);
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
                ipv4: vec![
                    Ipv4Addr::new(192, 168, 1, 20),
                    Ipv4Addr::new(169, 254, 3, 4),
                ],
            },
            adapter(Kind::Virtual, false, [127, 0, 0, 1]),
        ];
        let mut ranked: Vec<Ipv4Addr> = rank(&adapters).iter().map(|a| a.ip).collect();
        ranked.sort();
        assert_eq!(
            ranked,
            [
                Ipv4Addr::new(100, 80, 1, 2),
                Ipv4Addr::new(169, 254, 3, 4),
                Ipv4Addr::new(192, 168, 1, 20),
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
        let line = summary(Some(&adapters), v4([100, 80, 1, 2]));
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
}
