import CryptoKit
import Foundation

/// Fixed-size secret bytes in memory this module owns, wiped with `memset_s`
/// when the last reference goes away.
///
/// Immutable once made: every step that derives a new key makes a new
/// `SecretBytes`, so a copied handshake state can be thrown away on a failed
/// read without touching the original's keys. `description` never shows the
/// bytes.
///
/// What this covers, and what it can't (the Swift counterpart of the Rust
/// side's "key memory inside dependencies" residual):
/// - our own copies of `K_dev`, the Noise chaining and cipher keys, and the
///   raw `Split()` outputs live here and are wiped;
/// - transient `[UInt8]` buffers that held a secret are wiped with
///   ``wipe(_:)`` before release;
/// - CryptoKit makes its own copies (`SymmetricKey`, the X25519 private key,
///   HMAC states and outputs). Its open-source twin, swift-crypto, keeps keys
///   in storage it zeroes on release; we can't reach CryptoKit's internals,
///   and the Keychain hands keys back in a `CFData` we can't wipe. Reading
///   freed memory needs code running as the app, which section 1 of the
///   design puts out of scope.
public final class SecretBytes: @unchecked Sendable, CustomStringConvertible {
    private let storage: UnsafeMutableRawBufferPointer

    /// Copies `bytes` into new wiped-on-release storage.
    public init<Bytes: ContiguousBytes>(copying bytes: Bytes) {
        storage = bytes.withUnsafeBytes { source in
            let buffer = UnsafeMutableRawBufferPointer.allocate(byteCount: source.count, alignment: 16)
            if source.count > 0 {
                buffer.copyMemory(from: source)
            }
            return buffer
        }
    }

    /// `count` bytes written by `fill`, which must write all of them.
    init(count: Int, fill: (UnsafeMutableRawBufferPointer) throws -> Void) rethrows {
        storage = UnsafeMutableRawBufferPointer.allocate(byteCount: count, alignment: 16)
        storage.initializeMemory(as: UInt8.self, repeating: 0)
        do {
            try fill(storage)
        } catch {
            Self.zero(storage)
            storage.deallocate()
            throw error
        }
    }

    deinit {
        Self.zero(storage)
        storage.deallocate()
    }

    public var count: Int { storage.count }

    public func withUnsafeBytes<R>(_ body: (UnsafeRawBufferPointer) throws -> R) rethrows -> R {
        try body(UnsafeRawBufferPointer(storage))
    }

    /// A CryptoKit key over these bytes, for one operation. CryptoKit copies
    /// them into its own key storage.
    var symmetricKey: SymmetricKey {
        withUnsafeBytes { SymmetricKey(data: $0) }
    }

    /// Compares in time that depends only on the lengths.
    public func constantTimeEquals(_ other: SecretBytes) -> Bool {
        guard count == other.count else { return false }
        return withUnsafeBytes { a in
            other.withUnsafeBytes { b in
                var diff: UInt8 = 0
                for i in 0..<a.count { diff |= a[i] ^ b[i] }
                return diff == 0
            }
        }
    }

    public var description: String { "SecretBytes(\(count) bytes)" }

    private static func zero(_ buffer: UnsafeMutableRawBufferPointer) {
        guard let base = buffer.baseAddress, buffer.count > 0 else { return }
        _ = memset_s(base, buffer.count, 0, buffer.count)
    }
}

/// Overwrites a transient buffer that held secret bytes. The array's own
/// storage is written in place; copies made earlier (copy-on-write) are the
/// caller's to avoid.
func wipe(_ bytes: inout [UInt8]) {
    bytes.withUnsafeMutableBytes { buffer in
        guard let base = buffer.baseAddress, buffer.count > 0 else { return }
        _ = memset_s(base, buffer.count, 0, buffer.count)
    }
}
