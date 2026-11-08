import CryptoKit
import Foundation

// `Noise_NNpsk0_25519_ChaChaPoly_SHA256`, Noise revision 34, on CryptoKit.
// The Rust reference runs the same handshake through `snow`
// (`proto/src/v3/handshake.rs`); the cacophony NNpsk0 vector and the golden
// transcripts pin the two together byte for byte.
//
// The pattern (spec §7.5 and §9):
//
//     NNpsk0:
//       -> psk, e
//       <- e, ee
//
// - `psk` is `MixKeyAndHash(K_dev)`.
// - In a PSK handshake every `e` token is `MixHash(e.pub)` then
//   `MixKey(e.pub)` (§9.2), on both the writing and the reading side.
// - `ee` is `MixKey(DH(e, re))`.
// - The payloads are `EncryptAndHash`ed; `Split()` gives the
//   initiator→responder key first.
//
// Only primitives come from CryptoKit: X25519, ChaCha20-Poly1305, SHA-256
// and HMAC-SHA-256. HKDF is Noise's (§4.3), two or three HMAC outputs with
// no info: RFC 5869 with an empty info string.

/// Noise's `SymmetricState` with its embedded `CipherState`.
///
/// A value type over immutable `SecretBytes`: every mix makes new key
/// objects, so working on a copy and dropping it leaves the original state
/// untouched. That is how a failed message 2 read changes nothing.
struct SymmetricState {
    private(set) var chainingKey: SecretBytes
    private(set) var handshakeHash: [UInt8]
    private var cipherKey: SecretBytes?
    private var nonce: UInt64 = 0

    /// `InitializeSymmetric(protocol_name)`: a name longer than 32 bytes is
    /// hashed.
    init(protocolName: String) {
        let name = Array(protocolName.utf8)
        if name.count <= 32 {
            handshakeHash = name + [UInt8](repeating: 0, count: 32 - name.count)
        } else {
            handshakeHash = Array(SHA256.hash(data: name))
        }
        chainingKey = SecretBytes(copying: handshakeHash)
    }

    mutating func mixHash(_ data: [UInt8]) {
        var hasher = SHA256()
        hasher.update(data: handshakeHash)
        hasher.update(data: data)
        handshakeHash = Array(hasher.finalize())
    }

    mutating func mixKey(_ inputKeyMaterial: UnsafeRawBufferPointer) {
        let outputs = Self.hkdf(chainingKey: chainingKey, inputKeyMaterial: inputKeyMaterial, outputs: 2)
        chainingKey = outputs[0]
        cipherKey = outputs[1]
        nonce = 0
    }

    mutating func mixKey(_ inputKeyMaterial: [UInt8]) {
        inputKeyMaterial.withUnsafeBytes { mixKey($0) }
    }

    mutating func mixKeyAndHash(_ inputKeyMaterial: SecretBytes) {
        let outputs = inputKeyMaterial.withUnsafeBytes {
            Self.hkdf(chainingKey: chainingKey, inputKeyMaterial: $0, outputs: 3)
        }
        chainingKey = outputs[0]
        outputs[1].withUnsafeBytes { mixHash(Array($0)) }
        cipherKey = outputs[2]
        nonce = 0
    }

    mutating func encryptAndHash(_ plaintext: [UInt8]) -> [UInt8] {
        guard let cipherKey else {
            mixHash(plaintext)
            return plaintext
        }
        // Can't throw: the key is 32 bytes and the nonce 12.
        let box = try! ChaChaPoly.seal(
            plaintext, using: cipherKey.symmetricKey,
            nonce: noiseNonce(nonce), authenticating: handshakeHash)
        let ciphertext = Array(box.ciphertext) + Array(box.tag)
        nonce += 1
        mixHash(ciphertext)
        return ciphertext
    }

    /// Throws `V3Error.auth` and changes nothing if the tag doesn't verify.
    mutating func decryptAndHash(_ ciphertext: [UInt8]) throws -> [UInt8] {
        guard let cipherKey else {
            mixHash(ciphertext)
            return ciphertext
        }
        guard ciphertext.count >= 16 else { throw V3Error.auth }
        let plaintext: [UInt8]
        do {
            let box = try ChaChaPoly.SealedBox(
                nonce: noiseNonce(nonce),
                ciphertext: ciphertext[..<(ciphertext.count - 16)],
                tag: ciphertext[(ciphertext.count - 16)...])
            plaintext = Array(try ChaChaPoly.open(box, using: cipherKey.symmetricKey, authenticating: handshakeHash))
        } catch {
            throw V3Error.auth
        }
        nonce += 1
        mixHash(ciphertext)
        return plaintext
    }

    /// `Split()`: the initiator→responder key, then responder→initiator.
    func split() -> (SecretBytes, SecretBytes) {
        let outputs = Self.hkdf(chainingKey: chainingKey, inputKeyMaterial: UnsafeRawBufferPointer(start: nil, count: 0), outputs: 2)
        return (outputs[0], outputs[1])
    }

    /// Noise's `HKDF(chaining_key, input_key_material, num_outputs)`.
    static func hkdf(chainingKey: SecretBytes, inputKeyMaterial: UnsafeRawBufferPointer, outputs: Int) -> [SecretBytes] {
        let tempKey = SecretBytes(count: 32) { out in
            var mac = HMAC<SHA256>(key: chainingKey.symmetricKey)
            mac.update(data: inputKeyMaterial)
            mac.finalize().withUnsafeBytes { out.copyMemory(from: $0) }
        }
        var result: [SecretBytes] = []
        for index in 1...outputs {
            let previous = result.last
            result.append(SecretBytes(count: 32) { out in
                var mac = HMAC<SHA256>(key: tempKey.symmetricKey)
                previous?.withUnsafeBytes { mac.update(data: $0) }
                mac.update(data: [UInt8(index)])
                mac.finalize().withUnsafeBytes { out.copyMemory(from: $0) }
            })
        }
        return result
    }
}

/// The Noise ChaChaPoly nonce: 32 zero bits, then the counter
/// little-endian. Records use the same nonce with their counter.
func noiseNonce(_ counter: UInt64) -> ChaChaPoly.Nonce {
    var bytes = [UInt8](repeating: 0, count: 4)
    appendLE(&bytes, counter)
    // Can't throw: always 12 bytes.
    return try! ChaChaPoly.Nonce(data: bytes)
}

/// The two directions' keys from `Split()`, and the handshake hash `h`.
/// Turning them into a sealer and an opener uses them up, so each key gets
/// exactly one sealer and a counter never repeats under a key.
public final class SessionKeys {
    /// iPad → host.
    let initiatorToResponder: SecretBytes
    /// Host → iPad.
    let responderToInitiator: SecretBytes
    /// The Noise handshake hash, which later pairing messages bind to.
    public let handshakeHash: [UInt8]
    private var used = false

    init(split: (SecretBytes, SecretBytes), handshakeHash: [UInt8]) {
        initiatorToResponder = split.0
        responderToInitiator = split.1
        self.handshakeHash = handshakeHash
    }

    /// The iPad's halves for `sessionId`. Once only.
    public func initiator(sessionId: UInt32) -> (Sealer, Opener) {
        precondition(!used, "session keys already used")
        used = true
        return (Sealer(key: initiatorToResponder, sessionId: sessionId),
                Opener(key: responderToInitiator, sessionId: sessionId))
    }

    /// The host's halves, for the tests' host side. Once only.
    func responder(sessionId: UInt32) -> (Sealer, Opener) {
        precondition(!used, "session keys already used")
        used = true
        return (Sealer(key: responderToInitiator, sessionId: sessionId),
                Opener(key: initiatorToResponder, sessionId: sessionId))
    }
}

/// The iPad's side of one attempt, between sending message 1 and reading
/// message 2. Each attempt has its own ephemeral key, so a recorded
/// message 1 can never complete a session.
public final class Initiator {
    /// The message 1 datagram. Retries resend these same bytes.
    public let msg1: [UInt8]
    /// Nil once a message 2 has authenticated: the handshake is over.
    private var state: SymmetricState?
    private var ephemeral: Curve25519.KeyAgreement.PrivateKey?

    /// Starts an attempt with a fresh ephemeral key from the system CSPRNG.
    public convenience init(key: ClientKey, hostId: Id16, clientNonce: UInt32, payload: HelloPayload) throws {
        try self.init(key: key, hostId: hostId, clientNonce: clientNonce, payload: payload,
                      ephemeral: Curve25519.KeyAgreement.PrivateKey())
    }

    /// With a fixed ephemeral, for golden transcripts and tests only.
    init(key: ClientKey, hostId: Id16, clientNonce: UInt32, payload: HelloPayload,
         ephemeral: Curve25519.KeyAgreement.PrivateKey) throws {
        var state = SymmetricState(protocolName: V3.noiseParams)
        state.mixHash(V3.sessionPrologue(devId: key.devId, hostId: hostId))
        // -> psk, e
        state.mixKeyAndHash(key.kDev)
        let ephemeralPublic = Array(ephemeral.publicKey.rawRepresentation)
        state.mixHash(ephemeralPublic)
        state.mixKey(ephemeralPublic)
        let ciphertext = state.encryptAndHash(payload.encode())
        let noise = ephemeralPublic + ciphertext
        guard noise.count <= V3.maxNoiseMessage else { throw V3Error.handshake }
        msg1 = Wire.encodeMsg1(clientNonce: clientNonce, listenPort: payload.listenPort,
                               devId: key.devId, noise: noise)
        self.state = state
        self.ephemeral = ephemeral
    }

    /// True once a message 2 has authenticated, whatever its payload said.
    public var isFinished: Bool { state == nil }

    /// Reads a message 2 datagram.
    ///
    /// - A framing error or a failed authentication changes nothing, so a
    ///   forged or stale message 2 doesn't spoil the real one arriving
    ///   later (as `snow` restores its state).
    /// - Once Noise authenticates, the handshake is over (gap 7): the
    ///   ephemeral key is dropped, and if the payload then fails to parse,
    ///   the attempt can't read another message 2. Only a holder of `K_dev`
    ///   can produce such a message, so it's a bug signal, not an attack.
    /// - Returns the session keys for an `Accept` only.
    public func readMsg2(_ datagram: [UInt8]) throws -> (Msg2Payload, SessionKeys?) {
        guard var working = state, let ephemeral else { throw V3Error.handshake }
        let noise: [UInt8]
        do {
            noise = try Wire.parseMsg2(datagram)
        } catch let error as WireError {
            throw V3Error.wire(error)
        }
        // <- e, ee
        let responderEphemeral = Array(noise[..<32])
        working.mixHash(responderEphemeral)
        working.mixKey(responderEphemeral)
        do {
            let peer = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: responderEphemeral)
            let shared = try ephemeral.sharedSecretFromKeyAgreement(with: peer)
            shared.withUnsafeBytes { working.mixKey($0) }
        } catch {
            // CryptoKit may refuse a low-order point that snow would accept.
            // A genuine host's ephemeral is random, so only a forged message
            // could get here: drop it like any other that fails to
            // authenticate.
            throw V3Error.auth
        }
        let plaintext = try working.decryptAndHash(Array(noise[32...]))
        state = nil
        self.ephemeral = nil
        let reply: Msg2Payload
        do {
            reply = try Msg2Payload.parse(plaintext)
        } catch let error as WireError {
            throw V3Error.wire(error)
        }
        guard case .accept = reply else { return (reply, nil) }
        return (reply, SessionKeys(split: working.split(), handshakeHash: working.handshakeHash))
    }
}
