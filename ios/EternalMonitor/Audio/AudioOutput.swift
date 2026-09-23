@preconcurrency import AVFoundation

protocol AudioOutputDevice: AnyObject {
    func start(pcm: AudioPCMBuffer) throws
    func stop(deactivate: Bool)
}

/// Session and engine operations belong to the player's output queue. They can
/// wait on the audio service, so packet decoding must not share that queue.
final class AudioOutput: AudioOutputDevice {
    private var engine: AVAudioEngine?
    private var sessionActive = false

    func start(pcm: AudioPCMBuffer) throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playback, mode: .default)
        try session.setPreferredSampleRate(48_000)
        try session.setPreferredIOBufferDuration(0.01)
        try session.setActive(true)
        sessionActive = true
        let output = AVAudioEngine()
        let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2)!
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

    func stop(deactivate: Bool) {
        engine?.stop()
        engine = nil
        if deactivate, sessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            sessionActive = false
        }
    }
}
