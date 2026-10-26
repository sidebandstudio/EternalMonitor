import Foundation

/// Discovered host on the local network.
struct DiscoveredHost: Identifiable, Equatable {
    var id: String { "\(name)|\(port)" }
    let name: String
    /// The PC's IPv4 address. The host listens on IPv4 only, and Bonjour's
    /// IPv6 answers are often link-local without their interface scope, so
    /// an IPv6-only answer leaves this nil until IPv4 resolves.
    let address: String?
    let port: UInt16
    /// Still resolving. False once resolution gave up without IPv4.
    var finding = false

    init(name: String, address: String?, port: UInt16, finding: Bool = false) {
        self.name = name
        self.address = address
        self.port = port
        self.finding = finding
    }

    /// The service as far as it has resolved: its first IPv4 address, or a
    /// PC still being found when only IPv6 (or nothing usable) came back.
    static func resolved(name: String, port: Int, addresses: [Data]) -> DiscoveredHost {
        let port = port > 0 ? UInt16(clamping: port) : 9876
        let ipv4 = addresses.lazy.compactMap { NetworkScanner.ipAddress(from: $0, family: AF_INET) }.first
        return DiscoveredHost(name: name, address: ipv4, port: port, finding: ipv4 == nil)
    }
}

/// Scans the local network for EternalMonitor hosts using Bonjour service discovery.
@MainActor
final class NetworkScanner: NSObject, ObservableObject {
    @Published var hosts: [DiscoveredHost] = []
    @Published var isScanning = false
    @Published var statusMessage: String = ""
    /// Why Bonjour couldn't search, or nil while it can.
    @Published var searchProblem: SearchProblem?

    enum SearchProblem: Equatable {
        /// Local Network access is off for EternalMonitor.
        case localNetworkOff
        /// Any other Bonjour failure. Searching again may work.
        case couldNotSearch

        /// Only a policy denial means Local Network access is off.
        init(errorDict: [String: NSNumber]) {
            switch errorDict[NetService.errorCode]?.intValue {
            // kDNSServiceErr_PolicyDenied, kDNSServiceErr_NoAuth
            case -65570, -65555: self = .localNetworkOff
            default: self = .couldNotSearch
            }
        }
    }

    private let browser = NetServiceBrowser()
    private var resolvingServices: [String: NetService] = [:]
    /// Services whose IPv4 address is being looked up by host name, each
    /// with the lookup whose answer still counts. A removed service, a new
    /// scan or a newer address drops its entry, so a late answer can't list
    /// that PC again or replace the address it has now.
    private var ipv4Lookups: [String: UUID] = [:]
    /// Looks up a host name's IPv4 address off the main actor. Tests replace
    /// it to answer late.
    var resolveIPv4: @Sendable (String) -> String? = { NetworkScanner.ipv4Address(hostName: $0) }
    private var scanTask: Task<Void, Never>?
    // track auto-retry state so we don't bounce forever when discovery is broken.
    private var didAutoRetry = false

    override init() {
        super.init()
        browser.delegate = self
    }

    func startScan() {
        guard !isScanning else { return }
        didAutoRetry = false
        beginScan()
    }

    private func beginScan() {
        isScanning = true
        searchProblem = nil
        hosts = []
        resolvingServices.removeAll()
        ipv4Lookups.removeAll()
        statusMessage = "Looking for PCs…"

        #if DEBUG
        if let code = UIPreview.searchErrorCode {
            netServiceBrowser(browser, didNotSearch: [NetService.errorCode: NSNumber(value: code)])
            return
        }
        #endif
        browser.searchForServices(ofType: "_eternaldisplay._udp.", inDomain: "local.")

        scanTask = Task { @MainActor in
            // 10s timeout — many routers are slow to forward the first mDNS
            // response, especially right after Wi-Fi reconnect. The brief raised this from 5s.
            try? await Task.sleep(nanoseconds: 10_000_000_000)
            if !Task.isCancelled {
                stopScan()
            }
        }
    }

    func stopScan() {
        browser.stop()
        resolvingServices.values.forEach { service in
            service.stop()
            service.delegate = nil
        }
        resolvingServices.removeAll()
        scanTask?.cancel()
        scanTask = nil
        isScanning = false
        for index in hosts.indices where hosts[index].address == nil && ipv4Lookups[hosts[index].name] == nil {
            hosts[index].finding = false
        }

        if hosts.isEmpty {
            if !didAutoRetry {
                // auto-retry once after 2s. Bonjour browse occasionally
                // misses the first multicast burst.
                didAutoRetry = true
                statusMessage = "Still looking…"
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    if self.hosts.isEmpty && !self.isScanning {
                        self.beginScan()
                    }
                }
            } else {
                statusMessage = "No PCs found."
            }
        } else {
            statusMessage = "Found \(hosts.count) PC\(hosts.count == 1 ? "" : "s")"
        }
    }
}

extension NetworkScanner: NetServiceBrowserDelegate, NetServiceDelegate {
    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didFind service: NetService,
        moreComing: Bool
    ) {
        Task { @MainActor in
            let key = Self.serviceKey(service)
            guard resolvingServices[key] == nil else { return }

            service.delegate = self
            resolvingServices[key] = service
            service.resolve(withTimeout: 4)

            if !moreComing {
                statusMessage = "Found a PC…"
            }
        }
    }

    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didRemove service: NetService,
        moreComing: Bool
    ) {
        Task { @MainActor in
            let key = Self.serviceKey(service)
            // Stop its resolve, so it can't answer for a PC that left.
            if let resolving = resolvingServices.removeValue(forKey: key) {
                resolving.delegate = nil
                resolving.stop()
            }
            ipv4Lookups.removeValue(forKey: service.name)
            hosts.removeAll { $0.name == service.name }

            if !moreComing {
                statusMessage = hosts.isEmpty
                    ? "No PCs found."
                    : "Found \(hosts.count) PC\(hosts.count == 1 ? "" : "s")"
            }
        }
    }

    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didNotSearch errorDict: [String: NSNumber]
    ) {
        let problem = SearchProblem(errorDict: errorDict)
        print("[mDNS] Search failed: \(errorDict)")
        Task { @MainActor in
            stopScan()
            searchProblem = problem
        }
    }

    nonisolated func netServiceDidResolveAddress(_ sender: NetService) {
        Task { @MainActor in
            let key = Self.serviceKey(sender)
            guard isCurrent(sender), sender.addresses?.isEmpty == false else { return }
            let host = DiscoveredHost.resolved(name: sender.name, port: sender.port, addresses: sender.addresses ?? [])
            // discovery diagnostic — surface every resolved host so we
            // can tell whether mDNS is finding the right machine.
            print("[mDNS] Found: \(host.name) at \(host.address ?? "no IPv4 yet"):\(host.port)")
            if let version = PCVersions.version(fromTXT: sender.txtRecordData()) {
                PCVersions.record(version, name: host.name, address: host.address)
            }
            merge(host)
            if host.address != nil {
                resolvingServices.removeValue(forKey: key)
                sender.stop()
            } else if let hostName = sender.hostName {
                // Only IPv6 so far. Keep the resolve running for more answers,
                // and ask for the name's IPv4 address directly as well.
                lookUpIPv4(hostName: hostName, service: host)
            }
        }
    }

    /// Adds or updates one PC. A PC still being found never replaces one
    /// that already has an address.
    func merge(_ host: DiscoveredHost) {
        if host.address == nil, hosts.contains(where: { $0.name == host.name && $0.address != nil }) { return }
        // A lookup still running would answer with an older address.
        if host.address != nil { ipv4Lookups.removeValue(forKey: host.name) }
        hosts.removeAll { $0.name == host.name || (host.address != nil && $0.address == host.address) }
        hosts.append(host)
        // Resolve the local subnet ONCE per sort — the old comparator called
        // getifaddrs() for every comparison, a syscall storm on the MainActor.
        let subnet = Self.primaryIPv4Subnet()
        hosts.sort { Self.hostQualityCompare($0, $1, ourSubnet: subnet) }
        statusMessage = "Found \(hosts.count) PC\(hosts.count == 1 ? "" : "s")"
    }

    func lookUpIPv4(hostName: String, service: DiscoveredHost) {
        guard ipv4Lookups[service.name] == nil else { return }
        let lookup = UUID()
        ipv4Lookups[service.name] = lookup
        let resolve = resolveIPv4
        Task.detached {
            let address = resolve(hostName)
            await MainActor.run {
                guard self.ipv4Lookups[service.name] == lookup else { return }
                self.ipv4Lookups.removeValue(forKey: service.name)
                if let address {
                    self.merge(DiscoveredHost(name: service.name, address: address, port: service.port))
                } else if !self.isScanning, let index = self.hosts.firstIndex(where: { $0.name == service.name && $0.address == nil }) {
                    self.hosts[index].finding = false
                }
            }
        }
    }

    /// The host name's IPv4 address through the system resolver, which
    /// answers `.local` names over multicast DNS.
    nonisolated static func ipv4Address(hostName: String) -> String? {
        var hints = addrinfo()
        hints.ai_family = AF_INET
        hints.ai_socktype = SOCK_DGRAM
        var result: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(hostName, nil, &hints, &result) == 0, let first = result else { return nil }
        defer { freeaddrinfo(first) }
        guard let address = first.pointee.ai_addr else { return nil }
        return ipAddress(from: Data(bytes: address, count: Int(first.pointee.ai_addrlen)), family: AF_INET)
    }

    /// Prefer IPv4 over IPv6, then prefer same-subnet over different-subnet, then alpha by name.
    private static func hostQualityCompare(
        _ a: DiscoveredHost, _ b: DiscoveredHost, ourSubnet: String?
    ) -> Bool {
        let aV4 = a.address != nil
        let bV4 = b.address != nil
        if aV4 != bV4 {
            return aV4 // PCs with an address first
        }
        if let prefix = ourSubnet {
            let aSame = a.address?.hasPrefix(prefix) == true
            let bSame = b.address?.hasPrefix(prefix) == true
            if aSame != bSame {
                return aSame
            }
        }
        return a.name.localizedCaseInsensitiveCompare(b.name) == .orderedAscending
    }

    /// Best-effort: return the first three octets ("192.168.1.") of the iPad's primary
    /// non-loopback IPv4 interface, so we can prefer same-subnet hosts when sorting.
    /// Returns nil on failure.
    private static func primaryIPv4Subnet() -> String? {
        var ifaddr: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifaddr) == 0, let first = ifaddr else { return nil }
        defer { freeifaddrs(ifaddr) }

        var ptr: UnsafeMutablePointer<ifaddrs>? = first
        while let cur = ptr {
            let flags = Int32(cur.pointee.ifa_flags)
            let family = cur.pointee.ifa_addr.pointee.sa_family
            if (flags & IFF_UP) != 0 && (flags & IFF_LOOPBACK) == 0 && family == AF_INET {
                var hostname = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                let result = getnameinfo(
                    cur.pointee.ifa_addr,
                    socklen_t(cur.pointee.ifa_addr.pointee.sa_len),
                    &hostname,
                    socklen_t(hostname.count),
                    nil,
                    socklen_t(0),
                    NI_NUMERICHOST
                )
                if result == 0 {
                    let ip = String(cString: hostname)
                    let parts = ip.split(separator: ".")
                    if parts.count == 4 {
                        return "\(parts[0]).\(parts[1]).\(parts[2])."
                    }
                }
            }
            ptr = cur.pointee.ifa_next
        }
        return nil
    }

    nonisolated func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) {
        Task { @MainActor in
            guard isCurrent(sender) else { return }
            resolvingServices.removeValue(forKey: Self.serviceKey(sender))
            resolveEnded(name: sender.name)
            if hosts.isEmpty {
                statusMessage = "Found a PC…"
            }
        }
    }

    nonisolated func netServiceDidStop(_ sender: NetService) {
        Task { @MainActor in
            guard isCurrent(sender) else { return }
            resolvingServices.removeValue(forKey: Self.serviceKey(sender))
            resolveEnded(name: sender.name)
        }
    }

    /// The resolve timed out or stopped. A PC that never gave an IPv4
    /// address, with no lookup left running, stops showing as being found.
    private func resolveEnded(name: String) {
        guard ipv4Lookups[name] == nil,
              let index = hosts.firstIndex(where: { $0.name == name && $0.address == nil }) else { return }
        hosts[index].finding = false
    }

    /// Whether a callback comes from the service this scan is resolving.
    /// Each scan starts with none, and a service that left or already gave
    /// its IPv4 address is dropped, so their late answers are ignored.
    private func isCurrent(_ service: NetService) -> Bool {
        resolvingServices[Self.serviceKey(service)] === service
    }

    private static func serviceKey(_ service: NetService) -> String {
        "\(service.name)|\(service.type)|\(service.domain)"
    }

    nonisolated static func ipAddress(from data: Data, family expectedFamily: Int32? = nil) -> String? {
        data.withUnsafeBytes { rawBuffer in
            guard let base = rawBuffer.baseAddress else { return nil }
            let sockaddr = base.assumingMemoryBound(to: sockaddr.self)
            let family = Int32(sockaddr.pointee.sa_family)

            if let expectedFamily, family != expectedFamily {
                return nil
            }

            switch family {
            case AF_INET:
                var addr = base.assumingMemoryBound(to: sockaddr_in.self).pointee.sin_addr
                var buffer = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                guard inet_ntop(AF_INET, &addr, &buffer, socklen_t(buffer.count)) != nil else {
                    return nil
                }
                return String(cString: buffer)
            case AF_INET6:
                var addr = base.assumingMemoryBound(to: sockaddr_in6.self).pointee.sin6_addr
                var buffer = [CChar](repeating: 0, count: Int(INET6_ADDRSTRLEN))
                guard inet_ntop(AF_INET6, &addr, &buffer, socklen_t(buffer.count)) != nil else {
                    return nil
                }
                return String(cString: buffer)
            default:
                return nil
            }
        }
    }
}
