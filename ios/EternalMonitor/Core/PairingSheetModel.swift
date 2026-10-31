import Foundation
import Combine

@MainActor
final class PairingSheetModel: ObservableObject, Identifiable {
    let id = UUID()
    @Published var code = ""
    @Published private(set) var error: String?
    @Published private(set) var submitting = false
    @Published private(set) var blockedUntil: Date?
    private var complete = false

    func reject(_ status: HelloStatus, now: Date = Date()) {
        if status == .rateLimited {
            blockedUntil = now.addingTimeInterval(60)
            error = "Too many attempts. Wait before trying again."
        } else if submitting {
            error = "That code didn’t match. Check the PC and try again."
        }
        submitting = false
    }

    func remaining(at now: Date) -> Int {
        max(0, Int(ceil(blockedUntil?.timeIntervalSince(now) ?? 0)))
    }

    func submit(now: Date = Date()) -> UInt32? {
        guard !submitting, remaining(at: now) == 0 else { return nil }
        guard code.utf8.count == 6, code.utf8.allSatisfy({ (48...57).contains($0) }), let number = UInt32(code) else {
            error = "Enter all six digits shown on the PC."
            return nil
        }
        error = nil
        submitting = true
        return number
    }

    /// Called after each edit of the code. On iPadOS 27 the number pad opens
    /// over the Pair button, so the edit that completes six digits pairs.
    /// A code left in the field after a refusal doesn't go again, and none
    /// goes while one is being checked or during the wait.
    func editCompletesCode(now: Date = Date()) -> Bool {
        let wasComplete = complete
        complete = code.count == 6
        return complete && !wasComplete && !submitting && remaining(at: now) == 0
    }

    func timedOut() {
        submitting = false
        error = "The PC did not respond. Check the connection and try again."
    }
}
