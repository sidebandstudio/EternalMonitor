import Foundation
import os

/// Control and video share one link. Both implementations use the same
/// datagram decoder and session filter on their serial control queue.
protocol MediaLink: AnyObject {
    var datagrams: MediaDatagrams { get }
    var controlQueue: DispatchQueue { get }
    var isUSB: Bool { get }
    var onConnectionEstablished: (() -> Void)? { get set }
    var onError: ((String) -> Void)? { get set }
    var onListenerReady: ((UInt16) -> Void)? { get set }
    var onClosed: (() -> Void)? { get set }
    @discardableResult func start(host: String) -> Bool
    func send(_ data: Data)
    func stop()
}

extension MediaLink {
    var isUSB: Bool { false }
    var assembler: FrameAssembler? {
        get { datagrams.assembler }
        set { datagrams.assembler = newValue }
    }
    var onControlDatagram: ((Data) -> Void)? {
        get { datagrams.onControlDatagram }
        set { datagrams.onControlDatagram = newValue }
    }
    var onDatagramReceived: ((Int) -> Void)? {
        get { datagrams.onDatagramReceived }
        set { datagrams.onDatagramReceived = newValue }
    }
    var onDatagramIgnored: ((String) -> Void)? {
        get { datagrams.onDatagramIgnored }
        set { datagrams.onDatagramIgnored = newValue }
    }
    var onAudioPacket: ((AudioHeader, Data) -> Void)? {
        get { datagrams.onAudioPacket }
        set { datagrams.onAudioPacket = newValue }
    }
    func setAcceptedSessionId(_ id: UInt32) { datagrams.setAcceptedSessionId(id) }
    var legacyLookingDatagrams: Int { datagrams.legacyLookingDatagrams }
}

final class MediaDatagrams {
    var assembler: FrameAssembler?
    var onControlDatagram: ((Data) -> Void)?
    var onDatagramReceived: ((Int) -> Void)?
    var onDatagramIgnored: ((String) -> Void)?
    var onAudioPacket: ((AudioHeader, Data) -> Void)?
    private let acceptedSessionId = OSAllocatedUnfairLock<UInt32>(initialState: 0)
    private let unknownDatagramCount = OSAllocatedUnfairLock<Int>(initialState: 0)

    func setAcceptedSessionId(_ id: UInt32) { acceptedSessionId.withLock { $0 = id } }
    var legacyLookingDatagrams: Int { unknownDatagramCount.withLock { $0 } }
    func reset() {
        acceptedSessionId.withLock { $0 = 0 }
        unknownDatagramCount.withLock { $0 = 0 }
    }

    func handle(_ data: Data) {
        switch Wire.classify(data) {
        case .media:
            onDatagramReceived?(data.count)
            guard let (header, payloadRange) = MediaHeader.decode(data) else {
                onDatagramIgnored?("Dropped malformed media datagram (\(data.count) bytes)")
                return
            }
            let expected = acceptedSessionId.withLock { $0 }
            guard header.sessionId == expected, expected != 0 else {
                onDatagramIgnored?("Dropped media for foreign session \(header.sessionId)")
                return
            }
            assembler?.addFragment(
                seq: header.frameSeq,
                index: header.fragIndex,
                count: header.fragCount,
                epoch: header.streamEpoch,
                isKeyframe: header.isKeyframe,
                captureTimestampUs: header.captureTimestampUs,
                payload: data.subdata(in: payloadRange),
                isRetransmit: header.isRetransmit
            )
        case .control:
            onControlDatagram?(data)
        case .audio:
            guard let (header, payloadRange) = AudioHeader.decode(data) else { return }
            let expected = acceptedSessionId.withLock { $0 }
            guard expected != 0, header.sessionId == expected else { return }
            onAudioPacket?(header, data.subdata(in: payloadRange))
        case .legacyHello:
            // The host never sends this; ignore.
            break
        case .unknown:
            if data.count >= 16 {
                // Looks like v1 media from an old host — count it so the app
                // can say "update the Windows host" instead of "no host found".
                unknownDatagramCount.withLock { $0 += 1 }
            }
            onDatagramIgnored?("Ignored unrecognized datagram (\(data.count) bytes)")
        }
    }
}
