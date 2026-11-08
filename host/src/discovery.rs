use std::net::IpAddr;
use std::thread;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::{error, info, warn};

use crate::stats::PIPELINE_STATS;

const SERVICE_TYPE: &str = "_eternaldisplay._udp.local.";
const INSTANCE_NAME: &str = "EternalMonitor";
const RE_ADVERT_INTERVAL: Duration = Duration::from_secs(60);
/// A re-advertisement with the addresses of the last logged one is logged
/// once in this long, with how many there were. Every minute, it was one of
/// the most common lines in the uploads.
const RE_ADVERT_LOG_INTERVAL: Duration = Duration::from_secs(3600);

/// Which re-advertisements get a log line.
#[derive(Default)]
struct ReAdvertLog {
    last: Option<(Vec<IpAddr>, Instant)>,
    unchanged: u32,
}

impl ReAdvertLog {
    /// For a re-advertisement of `addrs` at `now`: `Some(n)` to log it, where
    /// `n` is how many unchanged ones went unlogged before it.
    fn note(&mut self, mut addrs: Vec<IpAddr>, now: Instant) -> Option<u32> {
        addrs.sort();
        if let Some((last, logged)) = &self.last {
            if *last == addrs && now.duration_since(*logged) < RE_ADVERT_LOG_INTERVAL {
                self.unchanged += 1;
                return None;
            }
        }
        self.last = Some((addrs, now));
        Some(std::mem::take(&mut self.unchanged))
    }
}

/// Registers the EternalMonitor service on the local network via mDNS/DNS-SD
/// so the iOS app's NetworkScanner (NWBrowser) can discover it.
///
/// Also spawns a background thread that re-registers the service every 60s.
/// Some routers and clients suppress mDNS TTL refreshes; `register` is a
/// documented upsert in mdns-sd, so the refresh never takes the service off
/// the air (the old unregister-first cycle opened a ~1s discovery hole).
///
/// `on_browse` runs each time another device on the network looks for PCs
/// running EternalMonitor, as an iPad does on its connect screen (see
/// [`watch_browsers`] and [`crate::gpu_wake`]).
///
/// Returns the `ServiceDaemon` handle — call [`say_goodbye`] on it at exit.
pub fn advertise_service(
    port: u16,
    on_browse: impl FnMut() + Send + 'static,
) -> Option<ServiceDaemon> {
    let mdns = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            error!(error = %e, "Failed to create mDNS daemon");
            return None;
        }
    };

    let host = hostname().unwrap_or_else(|| "eternal-host".to_string());
    let host_fqdn = format!("{}.local.", host);
    let instance_name = format!("{} ({})", INSTANCE_NAME, host);

    let service_info = build_service_info(port, &host_fqdn, &instance_name)?;

    let full_name = service_info.get_fullname().to_string();
    let initial_addrs: Vec<_> = service_info.get_addresses().iter().copied().collect();
    info!(
        service_type = SERVICE_TYPE,
        port,
        host = %host,
        instance = %instance_name,
        addrs = ?initial_addrs,
        "mDNS service registering — advertising on all non-loopback addresses"
    );
    if let Err(e) = mdns.register(service_info) {
        error!(error = %e, "Failed to register mDNS service");
        return None;
    }
    info!("mDNS service registered — iPad can now discover this host");
    PIPELINE_STATS.lock().mdns_active = true;
    watch_browsers(on_browse);

    let mdns_clone = mdns.clone();
    let host_fqdn_for_thread = host_fqdn.clone();
    let instance_for_thread = instance_name.clone();
    let _ = full_name; // fullname only mattered for the old unregister cycle
    let mut log = ReAdvertLog::default();
    thread::spawn(move || loop {
        thread::sleep(RE_ADVERT_INTERVAL);
        let Some(info) = build_service_info(port, &host_fqdn_for_thread, &instance_for_thread)
        else {
            warn!("mDNS re-advertisement: failed to rebuild ServiceInfo");
            continue;
        };
        let addrs: Vec<_> = info.get_addresses().iter().copied().collect();
        // Plain upsert: the service stays continuously advertised (addresses
        // may have changed, e.g. DHCP renew or interface flap).
        match mdns_clone.register(info) {
            Ok(_) => {
                if let Some(unchanged) = log.note(addrs.clone(), Instant::now()) {
                    info!(addrs = ?addrs, unchanged, "mDNS re-advertisement");
                }
            }
            Err(error) => warn!(error = %error, "mDNS re-advertisement register failed"),
        }
    });

    Some(mdns)
}

/// Calls `on_browse` on a thread of its own each time a device that isn't
/// this PC asks the network for the service: a PTR or ANY question for
/// [`SERVICE_TYPE`]. The daemon's own events can't tell: it answers its own
/// probes too, at each registration and re-advertisement. Reads alongside
/// the daemon on the shared mDNS port and sends nothing.
#[cfg(windows)]
fn watch_browsers(mut on_browse: impl FnMut() + Send + 'static) {
    use socket2::{Domain, Protocol, Socket, Type};
    let group = std::net::Ipv4Addr::new(224, 0, 0, 251);
    let socket = (|| -> std::io::Result<std::net::UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], 5353)).into())?;
        let mut joined = 0;
        for address in own_ipv4() {
            joined += usize::from(socket.join_multicast_v4(&group, &address).is_ok());
        }
        if joined == 0 {
            socket.join_multicast_v4(&group, &std::net::Ipv4Addr::UNSPECIFIED)?;
        }
        Ok(socket.into())
    })();
    let socket = match socket {
        Ok(socket) => socket,
        Err(error) => {
            info!(%error, "Not watching for iPads looking for this PC");
            return;
        }
    };
    let _ = thread::Builder::new()
        .name("mdns-browsers".into())
        .spawn(move || {
            let mut packet = [0u8; 1500];
            while let Ok((len, from)) = socket.recv_from(&mut packet) {
                if asks_for_service(&packet[..len]) && !is_own(from.ip()) {
                    on_browse();
                }
            }
        });
}

#[cfg(not(windows))]
fn watch_browsers(_on_browse: impl FnMut() + Send + 'static) {}

/// This PC's IPv4 addresses.
#[cfg(windows)]
fn own_ipv4() -> Vec<std::net::Ipv4Addr> {
    crate::network::adapters()
        .unwrap_or_default()
        .into_iter()
        .filter(|adapter| adapter.up)
        .flat_map(|adapter| adapter.ipv4)
        .collect()
}

/// Whether a query came from this PC: its own daemon, or another program on it.
#[cfg(windows)]
fn is_own(address: IpAddr) -> bool {
    address.is_loopback()
        || match address {
            IpAddr::V4(address) => own_ipv4().contains(&address),
            IpAddr::V6(_) => false,
        }
}

/// Whether `packet` is a DNS query (not a response) with a PTR or ANY
/// question for [`SERVICE_TYPE`], as a device browsing for EternalMonitor
/// PCs sends.
#[cfg(any(windows, test))]
pub(crate) fn asks_for_service(packet: &[u8]) -> bool {
    const PTR: u16 = 12;
    const ANY: u16 = 255;
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_be_bytes([*packet.get(at)?, *packet.get(at + 1)?]))
    };
    let (Some(flags), Some(questions)) = (u16_at(2), u16_at(4)) else {
        return false;
    };
    if flags & 0x8000 != 0 {
        return false;
    }
    let wanted = SERVICE_TYPE.trim_end_matches('.');
    let mut at = 12;
    for _ in 0..questions {
        let Some((name, next)) = dns_name(packet, at) else {
            return false;
        };
        let Some(kind) = u16_at(next) else {
            return false;
        };
        if (kind == PTR || kind == ANY) && name.eq_ignore_ascii_case(wanted) {
            return true;
        }
        at = next + 4;
    }
    false
}

/// The dotted name at `at`, following compression pointers, and where the
/// record goes on after it. None if it is malformed or loops.
#[cfg(any(windows, test))]
fn dns_name(packet: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut after = None;
    for _ in 0..32 {
        let len = *packet.get(at)? as usize;
        if len == 0 {
            return Some((name, after.unwrap_or(at + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let target = ((len & 0x3F) << 8) | *packet.get(at + 1)? as usize;
            after.get_or_insert(at + 2);
            at = target;
            continue;
        }
        let label = packet.get(at + 1..at + 1 + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(std::str::from_utf8(label).ok()?);
        at += 1 + len;
    }
    None
}

/// Send mDNS goodbye packets and stop the daemon. Called on the way out so
/// iPads drop the host from their scan list promptly instead of waiting for
/// the TTL to lapse. The short grace period lets the goodbyes hit the wire —
/// `std::process::exit` would otherwise cut them off.
pub fn say_goodbye(mdns: ServiceDaemon) {
    if mdns.shutdown().is_ok() {
        thread::sleep(Duration::from_millis(300));
        info!("mDNS goodbye sent");
    }
}

fn build_service_info(port: u16, host_fqdn: &str, instance_name: &str) -> Option<ServiceInfo> {
    let platform = if cfg!(windows) { "windows" } else { "other" };
    let properties = [
        ("version", env!("CARGO_PKG_VERSION")),
        ("proto", "2"),
        ("platform", platform),
    ];
    match ServiceInfo::new(
        SERVICE_TYPE,
        instance_name,
        host_fqdn,
        "",
        port,
        &properties[..],
    )
    .map(|info| info.enable_addr_auto())
    {
        Ok(info) => Some(info),
        Err(e) => {
            error!(error = %e, "Failed to create mDNS ServiceInfo");
            None
        }
    }
}

fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DNS packet with one question per `(name, type)`.
    fn query(flags: u16, questions: &[(&str, u16)]) -> Vec<u8> {
        let mut packet = vec![0, 0];
        packet.extend_from_slice(&flags.to_be_bytes());
        packet.extend_from_slice(&(questions.len() as u16).to_be_bytes());
        packet.extend_from_slice(&[0; 6]);
        for (name, kind) in questions {
            for label in name.split('.') {
                packet.push(label.len() as u8);
                packet.extend_from_slice(label.as_bytes());
            }
            packet.push(0);
            packet.extend_from_slice(&kind.to_be_bytes());
            packet.extend_from_slice(&[0x80, 1]);
        }
        packet
    }

    #[test]
    fn a_browse_for_the_service_is_told_from_other_mdns_traffic() {
        let ours = "_eternaldisplay._udp.local";
        assert!(asks_for_service(&query(0, &[(ours, 12)])));
        assert!(asks_for_service(&query(
            0,
            &[("_airplay._tcp.local", 12), (ours, 12)]
        )));
        assert!(asks_for_service(&query(
            0,
            &[("_EternalDisplay._UDP.local", 255)]
        )));
        assert!(!asks_for_service(&query(0, &[("_airplay._tcp.local", 12)])));
        assert!(!asks_for_service(&query(0, &[(ours, 1)])), "an A question");
        assert!(
            !asks_for_service(&query(0x8400, &[(ours, 12)])),
            "a response"
        );
        // A second question that points back into the first name.
        let mut packet = query(0, &[("_airplay._tcp.local", 12)]);
        packet[5] = 2;
        packet.extend_from_slice(&[15]);
        packet.extend_from_slice(b"_eternaldisplay");
        packet.extend_from_slice(&[4]);
        packet.extend_from_slice(b"_udp");
        // "local" is at offset 12 + 1 + 8 + 1 + 4 = 26.
        packet.extend_from_slice(&[0xC0, 26, 0, 12, 0, 1]);
        assert!(asks_for_service(&packet));
    }

    #[test]
    fn a_malformed_or_looping_packet_is_not_a_browse() {
        assert!(!asks_for_service(&[]));
        assert!(!asks_for_service(&[0; 11]));
        let full = query(0, &[("_eternaldisplay._udp.local", 12)]);
        for cut in 12..full.len() - 4 {
            assert!(!asks_for_service(&full[..cut]), "cut at {cut}");
        }
        // A pointer to itself.
        let mut looping = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        looping.extend_from_slice(&[0xC0, 12, 0, 12, 0, 1]);
        assert!(!asks_for_service(&looping));
    }

    /// The service pins no address: mdns-sd advertises every interface's,
    /// Wi-Fi and Tailscale alike. The Address row's ranking never reaches it.
    #[test]
    fn every_address_is_advertised() {
        let info = build_service_info(9876, "PC.local.", "EternalMonitor (PC)").unwrap();
        assert!(info.is_addr_auto());
        assert!(info.get_addresses().is_empty());
    }

    /// The minute's re-advertisement is logged when its addresses change and
    /// otherwise once an hour, with the count since the last line.
    #[test]
    fn unchanged_re_advertisements_are_logged_hourly() {
        let start = Instant::now();
        let minute = |n: u32| start + RE_ADVERT_INTERVAL * n;
        let wifi: IpAddr = "192.0.2.10".parse().unwrap();
        let usb: IpAddr = "198.51.100.1".parse().unwrap();
        let mut log = ReAdvertLog::default();
        assert_eq!(log.note(vec![], minute(1)), Some(0));
        let logged: Vec<_> = (2..=121)
            .filter_map(|n| log.note(vec![], minute(n)).map(|c| (n, c)))
            .collect();
        assert_eq!(logged, [(61, 59), (121, 59)]);
        // A new address list is logged at once; the order doesn't matter.
        assert_eq!(log.note(vec![wifi, usb], minute(122)), Some(0));
        assert_eq!(log.note(vec![usb, wifi], minute(123)), None);
        assert_eq!(log.note(vec![wifi], minute(124)), Some(1));
    }
}
