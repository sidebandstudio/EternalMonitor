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
/// Returns the `ServiceDaemon` handle — call [`say_goodbye`] on it at exit.
pub fn advertise_service(port: u16) -> Option<ServiceDaemon> {
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
