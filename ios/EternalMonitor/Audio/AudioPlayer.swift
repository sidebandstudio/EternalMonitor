@preconcurrency import AVFoundation
import Foundation
import os

struct AudioStats: Equatable {
    var packets: UInt64 = 0
    var decoded: UInt64 = 0
    var lost: UInt64 = 0
    var bufferMs = 0
    var targetMs = 60
    var playing = false
    var rms = 0.0
    var tone1kDB = -120.0
    var error: String?
}

final class AudioPlayer {
    private struct Incoming {
        var packets: [ReceivedAudio] = []
        var enabled = true
        var stopped = false
    }
    private let incoming = OSAllocatedUnfairLock(initialState: Incoming())
    private let measurements = OSAllocatedUnfairLock(initialState: AudioStats())
    private let queue = DispatchQueue(label: "eternal.audio", qos: .userInteractive)
    private let queueKey = DispatchSpecificKey<Bool>()
    private let pcm = AudioPCMBuffer(meterEnabled: E2E.enabled)
    private var timeline: AudioTimeline?
    private var engine: AVAudioEngine?
    private var sessionActive = false
    private var timer: DispatchSourceTimer?
    private var observers: [NSObjectProtocol] = []
    private var failed = false
    private var interrupted = false
    private var lastPacketUs: UInt64 = 0
    private var lastLogUs: UInt64 = 0
    private var previousPackets: UInt64 = 0
    private var previousDecoded: UInt64 = 0
    private var previousLost: UInt64 = 0
    var onError: ((String) -> Void)?

    init() {
        queue.setSpecific(key: queueKey, value: true)
        let timer = DispatchSource.makeTimerSource(flags: .strict, queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(5), leeway: .milliseconds(1))
        timer.setEventHandler { [weak self] in self?.process() }
        self.timer = timer
        timer.resume()
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(), queue: nil) { [weak self] notice in
            let began = (notice.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt)
                == AVAudioSession.InterruptionType.began.rawValue
            self?.queue.async { [weak self] in
                guard let self else { return }
                self.interrupted = began
                self.closeOutput(deactivate: false)
                self.retireTimeline()
            }
        })
        observers.append(center.addObserver(forName: AVAudioSession.routeChangeNotification,
            object: AVAudioSession.sharedInstance(), queue: nil) { [weak self] _ in
            self?.queue.async { [weak self] in
                // Rebuild the output on the next packet after headphones,
                // speakers or the hardware sample rate changes.
                self?.closeOutput(deactivate: false)
            }
        })
    }

    deinit {
        timer?.cancel()
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        closeOutput(deactivate: true)
    }

    var stats: AudioStats { measurements.withLock { $0 } }

    func receive(header: AudioHeader, opus: Data) {
        let now = ControlChannel.clientNowUs()
        incoming.withLock { s in
            guard s.enabled, !s.stopped else { return }
            if s.packets.count == 8 { s.packets.removeFirst() }
            s.packets.append(ReceivedAudio(header: header, opus: opus, receivedUs: now))
        }
    }

    func setEnabled(_ enabled: Bool) {
        incoming.withLock { $0.enabled = enabled; $0.packets.removeAll(keepingCapacity: true) }
        queue.async { [weak self] in
            guard let self else { return }
            self.closeOutput(deactivate: true)
            self.retireTimeline()
            self.failed = false
            self.measurements.withLock {
                $0.playing = false; $0.bufferMs = 0; $0.error = nil
            }
        }
    }

    func stop() {
        incoming.withLock { $0.stopped = true; $0.packets.removeAll() }
        let cleanup = {
            self.timer?.cancel(); self.timer = nil
            self.closeOutput(deactivate: true)
            self.timeline = nil
            self.measurements.withLock { $0 = AudioStats() }
        }
        // Finish old-session output before a USB takeover activates its player.
        if DispatchQueue.getSpecific(key: queueKey) == true { cleanup() }
        else { queue.sync(execute: cleanup) }
    }

    private func process() {
        let packets = incoming.withLock { s -> [ReceivedAudio] in
            guard s.enabled, !s.stopped else { return [] }
            let batch = s.packets
            s.packets.removeAll(keepingCapacity: true)
            return batch
        }
        guard !failed, !interrupted else { return }
        let now = ControlChannel.clientNowUs()
        do {
            if !packets.isEmpty {
                if timeline == nil { timeline = try AudioTimeline(pcm: pcm) }
                for packet in packets { try timeline?.receive(packet) }
                lastPacketUs = packets.last?.receivedUs ?? now
            }
            guard let timeline else { return }
            try timeline.drain(nowUs: now)
            if !packets.isEmpty, engine == nil { try openOutput() }
            let buffer = pcm.snapshot
            let stats = AudioStats(packets: previousPackets + timeline.packets,
                decoded: previousDecoded + timeline.decoded,
                lost: previousLost + timeline.lost, bufferMs: buffer.bufferMs + timeline.pendingCount * 20,
                targetMs: buffer.targetMs,
                playing: engine?.isRunning == true && now >= lastPacketUs && now - lastPacketUs < 500_000,
                rms: buffer.rms, tone1kDB: buffer.tone1kDB)
            measurements.withLock { $0 = stats }
            if E2E.enabled, now - lastLogUs >= 1_000_000 {
                lastLogUs = now
                E2E.audio(stats)
            }
        } catch {
            failed = true
            closeOutput(deactivate: true)
            let message = "PC audio stopped: \(error.localizedDescription)"
            measurements.withLock { $0.playing = false; $0.error = "Audio unavailable. Reconnect to retry." }
            onError?(message)
        }
    }

    private func openOutput() throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playback, mode: .default)
        try session.setPreferredSampleRate(48_000)
        try session.setPreferredIOBufferDuration(0.01)
        try session.setActive(true)
        sessionActive = true
        let output = AVAudioEngine()
        let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2)!
        let pcm = self.pcm
        let source = AVAudioSourceNode(format: format) { silence, _, frameCount, list in
            let buffers = UnsafeMutableAudioBufferListPointer(list)
            pcm.render(frames: Int(frameCount), buffers: buffers, nowUs: ControlChannel.clientNowUs())
            silence.pointee = false
            return noErr
        }
        output.attach(source)
        output.connect(source, to: output.mainMixerNode, format: format)
        output.prepare()
        try output.start()
        engine = output
    }

    private func retireTimeline() {
        if let timeline {
            previousPackets += timeline.packets
            previousDecoded += timeline.decoded
            previousLost += timeline.lost
        }
        timeline = nil
    }

    private func closeOutput(deactivate: Bool) {
        engine?.stop()
        engine = nil
        pcm.reset()
        if deactivate, sessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            sessionActive = false
        }
    }
}
