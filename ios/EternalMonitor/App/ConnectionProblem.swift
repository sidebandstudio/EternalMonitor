import Foundation
import Network

/// A connection problem told in plain words: what happened, what to do, and
/// the button that does it. The technical detail stays in Connection details
/// and the support log.
enum ConnectionProblem: Equatable {
    /// Nothing came back from the address (PC closed, asleep, other network,
    /// blocked by a firewall or the router).
    case noReply(usb: Bool)
    /// This iPad found the PC on the network, but it didn't answer: the
    /// network carries its announcements, so something on the PC (a firewall,
    /// security software, the stream being stopped) drops the connection.
    case noReplyFound
    /// Nothing answered at a private address outside this iPad's Wi-Fi
    /// network. Routed home networks and VPNs use such addresses too, so it
    /// is only a likely cause.
    case otherNetwork
    /// The PC answered, but no picture could be shown.
    case noPicture
    /// This iPad could not open its side of the connection.
    case receiverNotReady
    case badAddress(String)
    /// The typed address is a six-digit number: the pairing code.
    case pairingCodeAsAddress
    /// Nothing answered at a home network address ending in .1, which is
    /// usually the router.
    case routerAddress
    case noNetwork
    case localNetworkBlocked
    case couldNotConnect
    /// The cable connection failed after it had opened.
    case usbFailed
    /// A live session stopped: heartbeats ran out or the cable came out.
    case lost(LostReason)
    case reconnecting(attempt: Int, of: Int)
    /// Reconnecting waits for this iPad's Wi-Fi to come back.
    case waitingForNetwork
    case reconnectFailed
    case pcEndedSession
    /// The PC is streaming to another device, or still to this iPad's cable.
    case pcBusy
    /// The PC stopped the stream to install an update and restarts on it.
    case pcUpdating(UpdateWait)
    /// The PC answered but isn't streaming. This iPad keeps asking and
    /// connects as soon as it is.
    case pcNotStreaming(StreamWait)

    enum StreamWait: Equatable {
        /// Someone clicked Stop on the PC.
        case stopped
        case restarting
        /// The PC gave up after repeated errors, for the reason it shows.
        case failed(String)

        /// What the PC's answer means, if it is one of these.
        init?(_ status: HelloStatus, reason: String) {
            switch status {
            case .stopped: self = .stopped
            case .restarting: self = .restarting
            case .failed: self = .failed(reason)
            default: return nil
            }
        }
    }

    /// What this iPad does while the PC updates.
    enum UpdateWait: Equatable {
        /// Automatic attempts are running or about to.
        case reconnecting
        /// Auto-reconnect is off: the user connects again.
        case manual
        /// Every automatic attempt ran out.
        case notBack
    }

    enum LostReason: Equatable {
        case pcStoppedResponding
        case cableUnplugged
        case usbTurnedOff
    }

    enum Action: Equatable {
        case retry
        case openSettings
    }

    var title: String {
        switch self {
        case .noReply(usb: false): return "Your PC didn't answer"
        case .noReply(usb: true): return "Your PC didn't answer over the cable"
        case .noReplyFound: return "Your PC is on this network but didn't answer"
        case .otherNetwork: return "Your iPad and PC may be on different networks"
        case .noPicture: return "Your PC answered, but no picture arrived"
        case .receiverNotReady: return "This iPad couldn't start the connection"
        case .badAddress: return "That address doesn't look right"
        case .pairingCodeAsAddress: return "That looks like the pairing code, not the PC's address"
        case .routerAddress: return "That looks like your router's address, not your PC's"
        case .noNetwork: return "This iPad isn't connected to a network"
        case .localNetworkBlocked: return "EternalMonitor isn't allowed on your local network"
        case .couldNotConnect: return "Couldn't connect to your PC"
        case .usbFailed: return "The connection over the cable stopped"
        case .lost(.pcStoppedResponding): return "Lost the connection to your PC"
        case .lost(.cableUnplugged): return "The USB cable was disconnected"
        case .lost(.usbTurnedOff): return "USB connections were turned off"
        case .reconnecting: return "Reconnecting to your PC…"
        case .waitingForNetwork: return "Waiting for Wi-Fi…"
        case .reconnectFailed: return "Couldn't reconnect to your PC"
        case .pcEndedSession: return "Your PC ended the session"
        case .pcBusy: return "Your PC is already streaming to another device"
        case .pcUpdating(.reconnecting), .pcUpdating(.manual): return "Your PC is updating EternalMonitor"
        case .pcUpdating(.notBack): return "Your PC hasn't come back from its update yet"
        case .pcNotStreaming(.stopped): return "Your PC isn't streaming"
        case .pcNotStreaming(.restarting): return "Your PC is restarting the stream…"
        case .pcNotStreaming(.failed): return "Your PC couldn't start streaming"
        }
    }

    var detail: String {
        switch self {
        case .noReply(usb: false):
            return "Check that EternalMonitor is open on the PC and says Ready for your iPad, that the PC is awake, "
                + "and that both are on the same Wi-Fi, not a guest network. If all of that is right, Windows Firewall "
                + "or security software on the PC may be blocking EternalMonitor: run the PC installer again, or connect with a USB cable."
        case .noReply(usb: true):
            return "Check that EternalMonitor and Apple Devices are both open on the PC, then unplug the cable and plug it in again."
        case .noReplyFound:
            return "Check that EternalMonitor on the PC says Ready for your iPad. If it shows a Windows Firewall message, click Allow there. "
                + "Security software on the PC may also be blocking EternalMonitor: allow it there, run the PC installer again, or connect with a USB cable."
        case .otherNetwork:
            return "That address isn't on this iPad's Wi-Fi network, and nothing answered there. Join the same Wi-Fi as the PC "
                + "(not a guest network), or check that EternalMonitor on the PC is open and says Ready for your iPad. "
                + "A USB cable works too."
        case .noPicture:
            return "EternalMonitor on the PC couldn't send its screen. Look there for a message and follow it, click Restart, then try again. "
                + "If it keeps happening, send us Settings, Diagnostics from this iPad and Performance, Copy logs from the PC."
        case .receiverNotReady:
            return "Close EternalMonitor on this iPad and open it again."
        case .badAddress(let host):
            return "This iPad couldn't find \"\(host)\". Type the Address exactly as the PC window shows it, for example 192.168.1.20."
        case .pairingCodeAsAddress:
            return "Type the Address shown in the EternalMonitor window on the PC, for example 192.168.1.20. "
                + "This iPad asks for the pairing code after it connects."
        case .routerAddress:
            return "Addresses ending in .1 usually belong to the Wi-Fi router. Type the Address shown in the "
                + "EternalMonitor window on the PC, for example 192.168.1.20."
        case .noNetwork:
            return "Turn on Wi-Fi and join the same network as your PC, or connect with a USB cable."
        case .localNetworkBlocked:
            return "Open Settings and turn on Local Network for EternalMonitor, then try again."
        case .couldNotConnect:
            return "Check that this iPad and your PC are on the same Wi-Fi and that EternalMonitor is open on the PC, then try again."
        case .usbFailed:
            return "Keep EternalMonitor and Apple Devices open on the PC, unlock this iPad, then unplug the cable and plug it in again."
        case .lost(.pcStoppedResponding):
            return "The PC may have gone to sleep, closed EternalMonitor, or lost its network. Wake the PC, check EternalMonitor is open, then try again."
        case .lost(.cableUnplugged):
            return "Plug the cable back in. This iPad reconnects by itself."
        case .lost(.usbTurnedOff):
            return "Turn on Allow USB connections in Settings to use the cable again."
        case .waitingForNetwork:
            return "This iPad lost its network connection. It reconnects to your PC as soon as Wi-Fi is back."
        case .reconnecting(let attempt, let total):
            return "Attempt \(attempt) of \(total). Keep EternalMonitor open on the PC."
        case .reconnectFailed:
            return "Check that the PC is awake and EternalMonitor is open on it, then try again."
        case .pcEndedSession:
            return "Someone clicked Stop on the PC, changed a setting that restarts the stream, or closed EternalMonitor. Try again when the PC says Ready for your iPad."
        case .pcUpdating(.reconnecting):
            return "This iPad reconnects when it's back. If it doesn't, check that EternalMonitor opened again on the PC, then try again."
        case .pcUpdating(.manual):
            return "Tap Try again once EternalMonitor has opened again on the PC."
        case .pcUpdating(.notBack):
            return "Check that EternalMonitor opened again on the PC, then try again. Windows may still be waiting for you to approve the update there."
        case .pcBusy:
            return "Disconnect the other iPad first, then try again."
        case .pcNotStreaming(.stopped):
            return "Click Start streaming in EternalMonitor on the PC. This iPad connects as soon as it starts."
        case .pcNotStreaming(.restarting):
            return "This iPad connects as soon as it's back."
        case .pcNotStreaming(.failed(let reason)):
            let start = "Click Start streaming in EternalMonitor on the PC to try again. This iPad connects as soon as it starts."
            return reason.isEmpty ? start : "\(reason). \(start)"
        }
    }

    var actions: [Action] {
        switch self {
        case .localNetworkBlocked: return [.openSettings]
        case .reconnecting, .waitingForNetwork, .lost(.cableUnplugged), .lost(.usbTurnedOff), .receiverNotReady, .pairingCodeAsAddress,
             .pcNotStreaming: return []
        default: return [.retry]
        }
    }

    /// Why a connect attempt timed out, from how far it got.
    /// A PC that accepted the handshake did answer: no picture after that is
    /// a problem on the PC (screen capture or video encoding), not the network.
    /// `pcFound`: this iPad's search found a PC at `host`. `local` is this
    /// iPad's own IPv4 networks.
    static func timeout(listenerReady: Bool, helloAttempts: Int, handshakeDone: Bool,
                        datagramsReceived: Int, isUSB: Bool, host: String = "",
                        pcFound: Bool = false, local: [LocalNetwork.Subnet] = []) -> ConnectionProblem {
        if handshakeDone { return .noPicture }
        if !listenerReady || helloAttempts == 0 { return .receiverNotReady }
        if datagramsReceived > 0 { return .noPicture }
        if isUSB { return .noReply(usb: true) }
        if pcFound { return .noReplyFound }
        if isRouterAddress(host) { return .routerAddress }
        if LocalNetwork.isOtherHomeNetwork(host, local: local) { return .otherNetwork }
        return .noReply(usb: false)
    }

    /// A problem with the typed address found before connecting. The pairing
    /// code would otherwise be read as a numeric IPv4 address that never
    /// answers, and so would an address with a dot missing ("19216801").
    static func typedAddress(_ host: String) -> ConnectionProblem? {
        let text = host.filter { !$0.isWhitespace }
        let digits = text.filter { $0 != "." }
        guard !digits.isEmpty, digits.allSatisfy({ ("0"..."9").contains($0) }) else { return nil }
        if text.count == 6 && digits.count == 6 { return .pairingCodeAsAddress }
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        let valid = parts.count == 4 && parts.allSatisfy { part in
            !part.isEmpty && part.count <= 3 && (Int(part) ?? 256) <= 255
        }
        return valid ? nil : .badAddress(text)
    }

    /// A private IPv4 address ending in .1, where home routers sit. A PC can
    /// have one, so this only changes the advice once nothing answered.
    static func isRouterAddress(_ host: String) -> Bool {
        guard host.split(separator: ".").count == 4, let bytes = IPv4Address(host)?.rawValue, bytes.count == 4,
              bytes[3] == 1 else { return false }
        return bytes[0] == 10 || (bytes[0] == 172 && (16...31).contains(bytes[1])) || (bytes[0] == 192 && bytes[1] == 168)
    }

    /// The banner once automatic reconnecting gives up. A last attempt that
    /// failed for a specific reason keeps that advice; otherwise the PC just
    /// never answered.
    static func afterReconnecting(lastAttempt: ConnectionProblem?) -> ConnectionProblem {
        switch lastAttempt {
        case .localNetworkBlocked, .noNetwork, .badAddress, .usbFailed, .noPicture, .pcBusy, .pcEndedSession, .pcUpdating:
            return lastAttempt ?? .reconnectFailed
        default:
            return .reconnectFailed
        }
    }

    /// The failure a reconnect cycle keeps after one more attempt. While the
    /// PC updates it turns sessions away (Busy), then stops answering until
    /// the new version starts; neither replaces the update as the reason.
    static func kept(_ kept: ConnectionProblem?, after attempt: ConnectionProblem?) -> ConnectionProblem? {
        if case .pcUpdating = kept, [.pcBusy, .noReply(usb: false), .noReplyFound, .pcEndedSession].contains(attempt) {
            return kept
        }
        return afterReconnecting(lastAttempt: attempt) != .reconnectFailed ? attempt : kept
    }

    /// The problem to show when no automatic attempt will run (Auto-reconnect
    /// is off): it must not promise a reconnect.
    var withoutReconnecting: ConnectionProblem {
        if case .pcUpdating = self { return .pcUpdating(.manual) }
        return self
    }

    /// The problem to show once every automatic attempt ran out.
    var afterTheCycle: ConnectionProblem {
        if case .pcUpdating = self { return .pcUpdating(.notBack) }
        return self
    }

    /// What a link failure means for the user. The raw error stays in the
    /// support log; nothing here shows it.
    static func linkFailure(_ error: LinkError, isUSB: Bool, host: String) -> ConnectionProblem {
        // The cable is a local tunnel: Wi-Fi, DNS and Local Network don't apply.
        if isUSB { return .usbFailed }
        if error.kind == .localNetworkDenied { return .localNetworkBlocked }
        if let nw = error.underlying as? NWError {
            switch nw {
            case .dns(let code):
                // kDNSServiceErr_PolicyDenied: Local Network access is off.
                return code == -65570 ? .localNetworkBlocked : .badAddress(host)
            case .posix(let code) where offline.contains(code):
                return .noNetwork
            default:
                break
            }
        }
        if let posix = error.underlying as? POSIXError, offline.contains(posix.code) { return .noNetwork }
        switch error.kind {
        case .resolve: return .badAddress(host)
        case .pathUnavailable: return .noNetwork
        default: return .couldNotConnect
        }
    }

    private static let offline: Set<POSIXErrorCode> = [.ENETDOWN, .ENETUNREACH, .EHOSTUNREACH]
}

/// This iPad's own IPv4 networks, to tell a PC on another network from one
/// that doesn't answer.
enum LocalNetwork {
    struct Subnet: Equatable {
        let address: UInt32
        let mask: UInt32

        init(address: UInt32, mask: UInt32) {
            self.address = address
            self.mask = mask
        }

        /// From dotted strings, for tests.
        init?(_ address: String, mask: String) {
            guard let address = LocalNetwork.ipv4(address), let mask = LocalNetwork.ipv4(mask) else { return nil }
            self.init(address: address, mask: mask)
        }

        func contains(_ ip: UInt32) -> Bool { ip & mask == address & mask }
    }

    static func ipv4(_ text: String) -> UInt32? {
        guard text.split(separator: ".").count == 4, let bytes = IPv4Address(text)?.rawValue, bytes.count == 4 else {
            return nil
        }
        return bytes.reduce(0) { $0 << 8 | UInt32($1) }
    }

    static func isPrivate(_ ip: UInt32) -> Bool {
        ip >> 24 == 10 || ip >> 20 == 0xAC1 || ip >> 16 == 0xC0A8
    }

    /// A private address outside every private network this iPad is on, so
    /// "maybe another network" is worth saying. Not a verdict: routed LANs and
    /// VPNs use such addresses. A Tailscale or public address, or an iPad
    /// without Wi-Fi, says nothing either way.
    static func isOtherHomeNetwork(_ host: String, local: [Subnet]) -> Bool {
        guard let ip = ipv4(host), isPrivate(ip) else { return false }
        let home = local.filter { isPrivate($0.address) }
        return !home.isEmpty && !home.contains { $0.contains(ip) }
    }

    /// The first address in `addresses` on one of `local`'s networks, else the
    /// first private one, else the first.
    static func best(_ addresses: [String], local: [Subnet]) -> String? {
        let parsed = addresses.compactMap { text in ipv4(text).map { (text, $0) } }
        return parsed.first { item in local.contains { $0.contains(item.1) } }?.0
            ?? parsed.first { isPrivate($0.1) }?.0
            ?? addresses.first
    }

    /// The Wi-Fi and Ethernet networks this iPad is on now.
    static func current() -> [Subnet] {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return [] }
        defer { freeifaddrs(list) }
        var subnets: [Subnet] = []
        for entry in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let flags = Int32(entry.pointee.ifa_flags)
            let name = String(cString: entry.pointee.ifa_name)
            guard flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0, name.hasPrefix("en"),
                  let address = entry.pointee.ifa_addr, address.pointee.sa_family == UInt8(AF_INET),
                  let mask = entry.pointee.ifa_netmask else { continue }
            let ip = address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
            let netmask = mask.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
            subnets.append(Subnet(address: ip, mask: netmask))
        }
        return subnets
    }
}

/// What the Connecting card says while an attempt runs, so nobody stares at
/// a spinner for ten seconds.
enum ConnectingStatus: Equatable {
    case reaching
    /// A few tries went unanswered; still trying.
    case noAnswerYet(usb: Bool)
    case answered
    /// The PC answered that it isn't streaming; still asking.
    case waitingForStream

    /// HELLOs go out every quarter second at first: eight unanswered is two
    /// seconds of silence.
    static func of(helloAttempts: Int, handshakeDone: Bool, isUSB: Bool) -> ConnectingStatus {
        if handshakeDone { return .answered }
        return helloAttempts > 8 ? .noAnswerYet(usb: isUSB) : .reaching
    }

    func detail(host: String) -> String {
        switch self {
        case .reaching:
            return host.isEmpty || host == "USB" ? "Waiting for your PC" : "Reaching \(host)"
        case .noAnswerYet(usb: false):
            return "No answer yet. Check that EternalMonitor is open on the PC and says Ready for your iPad. Still trying…"
        case .noAnswerYet(usb: true):
            return "No answer over the cable yet. Check that EternalMonitor and Apple Devices are open on the PC. Still trying…"
        case .answered:
            return "Your PC answered. Waiting for the picture…"
        case .waitingForStream:
            return "Waiting for your PC to start streaming…"
        }
    }
}

/// What the USB cable card says for each listener status.
enum USBStatusText {
    static func describe(_ status: String) -> String {
        switch status {
        case "USB: off": return "Off. Turn on to connect with a cable."
        case "USB: waiting for a cable": return "Plug this iPad into your PC. Keep Apple Devices open on the PC."
        case "USB: disconnected": return "Paused. Tap Connect USB to use the cable again."
        case "USB: connecting": return "Connecting over the cable…"
        case "USB: unavailable": return "The cable can't be used right now. Wi-Fi still works."
        default:
            if status.hasPrefix("USB: connected to ") {
                return "Connected to \(status.dropFirst("USB: connected to ".count)) over the cable."
            }
            return status
        }
    }
}
