import Foundation
import Network

/// A connection problem told in plain words: what happened, what to do, and
/// the button that does it. The technical detail stays in Connection details
/// and the support log.
enum ConnectionProblem: Equatable {
    /// Nothing came back from the address (PC closed, asleep, other network,
    /// blocked by a firewall or the router).
    case noReply(usb: Bool)
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
    case reconnectFailed
    case pcEndedSession
    /// The PC is streaming to another device, or still to this iPad's cable.
    case pcBusy
    /// The PC stopped the stream to install an update and restarts on it.
    case pcUpdating(UpdateWait)

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
        case .reconnectFailed: return "Couldn't reconnect to your PC"
        case .pcEndedSession: return "Your PC ended the session"
        case .pcBusy: return "Your PC is already streaming to another device"
        case .pcUpdating(.reconnecting), .pcUpdating(.manual): return "Your PC is updating EternalMonitor"
        case .pcUpdating(.notBack): return "Your PC hasn't come back from its update yet"
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
        }
    }

    /// Whether the card offers Report this problem. Not on cards that only
    /// say what is happening or what the user did: a reconnect or an update
    /// still under way, the cable pulled out, USB turned off, the PC busy
    /// with another device, or the pairing code typed as the address.
    var canReport: Bool {
        switch self {
        case .reconnecting, .pcUpdating(.reconnecting), .pcUpdating(.manual),
             .lost(.cableUnplugged), .lost(.usbTurnedOff), .pcBusy, .pairingCodeAsAddress:
            return false
        default:
            return true
        }
    }

    var actions: [Action] {
        switch self {
        case .localNetworkBlocked: return [.openSettings]
        case .reconnecting, .lost(.cableUnplugged), .lost(.usbTurnedOff), .receiverNotReady, .pairingCodeAsAddress: return []
        default: return [.retry]
        }
    }

    /// Why a connect attempt timed out, from how far it got.
    /// A PC that accepted the handshake did answer: no picture after that is
    /// a problem on the PC (screen capture or video encoding), not the network.
    static func timeout(listenerReady: Bool, helloAttempts: Int, handshakeDone: Bool,
                        datagramsReceived: Int, isUSB: Bool, host: String = "") -> ConnectionProblem {
        if handshakeDone { return .noPicture }
        if !listenerReady || helloAttempts == 0 { return .receiverNotReady }
        if datagramsReceived > 0 { return .noPicture }
        if !isUSB && isRouterAddress(host) { return .routerAddress }
        return .noReply(usb: isUSB)
    }

    /// A problem with the typed address found before connecting. The pairing
    /// code would otherwise be read as a numeric IPv4 address that never answers.
    static func typedAddress(_ host: String) -> ConnectionProblem? {
        let text = host.filter { !$0.isWhitespace }
        return text.count == 6 && text.allSatisfy({ ("0"..."9").contains($0) }) ? .pairingCodeAsAddress : nil
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
        if case .pcUpdating = kept, [.pcBusy, .noReply(usb: false), .pcEndedSession].contains(attempt) {
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

    /// This iPad's own network is down. No route to a host or network
    /// (EHOSTUNREACH, ENETUNREACH) usually means the PC is asleep or
    /// elsewhere while Wi-Fi works, so it gets the general advice.
    private static let offline: Set<POSIXErrorCode> = [.ENETDOWN]
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
