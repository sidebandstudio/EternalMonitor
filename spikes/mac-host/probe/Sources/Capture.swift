// Spike d: ScreenCaptureKit capture of a display at 60 and 120 fps, the
// idle-frame rate, capture latency, zero-copy encode through the host's
// direct VideoToolbox encoder, decode of what it produced, and system audio.
//
// A capture passes only if, over a fixed window, it delivered about 0.9×
// the target fps while the content changed every frame; the probe's content
// marker (Measure.swift) reads back from every captured frame with the
// captured display's ID, and every decoded frame is a submitted frame, in
// order, never repeated (DecodedMatch); the encoder kept up with its planned
// rate, capped at the rate the input actually had; and the probe's tone was
// heard. Frames are thinned to the planned rate by their presentation
// timestamps (FramePacer), never by when their callbacks ran. Full-size HEVC is capped at 60 fps on Main
// tier by design, so encode at 120 is tested at the Duo outer size; full size
// at 120 (experimental High tier) is informational.

/// Reads the probe's content marker from a 420v frame's luma plane.
func readMarker(_ image: CVPixelBuffer, _ centres: [(Int, Int)]) -> (display: UInt16, counter: UInt16)? {
    guard CVPixelBufferLockBaseAddress(image, .readOnly) == kCVReturnSuccess else { return nil }
    defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
    guard let base = CVPixelBufferGetBaseAddressOfPlane(image, 0) else { return nil }
    let stride = CVPixelBufferGetBytesPerRowOfPlane(image, 0)
    let width = CVPixelBufferGetWidthOfPlane(image, 0), height = CVPixelBufferGetHeightOfPlane(image, 0)
    var luma: [UInt8] = []
    for (x, y) in centres {
        guard x >= 0, y >= 0, x < width, y < height else { return nil }
        luma.append(base.load(fromByteOffset: y * stride + x, as: UInt8.self))
    }
    return Marker.decode(luma)
}
import AVFoundation
import AppKit
import CoreMedia
import ScreenCaptureKit
import VideoToolbox

/// Decodes Annex B access units with VideoToolbox, as the iPad app does.
final class AnnexBDecoder {
    let hevc: Bool
    private var format: CMVideoFormatDescription?
    private var parameterSets: [Data] = []
    private var session: VTDecompressionSession?
    private let lock = NSLock()
    private(set) var decoded = 0
    private(set) var failed = 0
    private(set) var size = (0, 0)
    /// Where to read the content marker in decoded frames, and what was
    /// read, in decode order.
    private let markerCentres: [(Int, Int)]?
    private(set) var markers: [(display: UInt16, counter: UInt16)?] = []

    init(hevc: Bool, markerCentres: [(Int, Int)]? = nil) {
        self.hevc = hevc
        self.markerCentres = markerCentres
    }

    private func nalType(_ nal: Data) -> Int {
        hevc ? Int((nal[nal.startIndex] >> 1) & 0x3f) : Int(nal[nal.startIndex] & 0x1f)
    }

    private func isParameterSet(_ nal: Data) -> Bool {
        hevc ? (32...34).contains(nalType(nal)) : (7...8).contains(nalType(nal))
    }

    static func split(_ unit: Data) -> [Data] {
        let bytes = [UInt8](unit)
        var starts: [(Int, Int)] = [] // (start code index, payload index)
        var i = 0
        while i + 3 <= bytes.count {
            if bytes[i] == 0, bytes[i + 1] == 0 {
                if bytes[i + 2] == 1 { starts.append((i, i + 3)); i += 3; continue }
                if i + 4 <= bytes.count, bytes[i + 2] == 0, bytes[i + 3] == 1 { starts.append((i, i + 4)); i += 4; continue }
            }
            i += 1
        }
        return starts.enumerated().map { index, entry in
            let end = index + 1 < starts.count ? starts[index + 1].0 : bytes.count
            return Data(bytes[entry.1..<end])
        }
    }

    func decode(_ unit: Data) {
        let nals = Self.split(unit).filter { !$0.isEmpty }
        let sets = nals.filter(isParameterSet)
        if !sets.isEmpty, sets != parameterSets {
            parameterSets = sets
            makeSession()
        }
        guard let format, let session else { failed += 1; return }
        var sample = Data()
        for nal in nals where !isParameterSet(nal) {
            var length = UInt32(nal.count).bigEndian
            sample.append(Data(bytes: &length, count: 4))
            sample.append(nal)
        }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator: nil, memoryBlock: nil, blockLength: sample.count,
                                                 blockAllocator: nil, customBlockSource: nil, offsetToData: 0,
                                                 dataLength: sample.count, flags: 0, blockBufferOut: &block) == noErr,
              let block else { failed += 1; return }
        _ = sample.withUnsafeBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: sample.count) }
        var buffer: CMSampleBuffer?
        var sizes = [sample.count]
        guard CMSampleBufferCreateReady(allocator: nil, dataBuffer: block, formatDescription: format, sampleCount: 1,
                                        sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1,
                                        sampleSizeArray: &sizes, sampleBufferOut: &buffer) == noErr,
              let buffer else { failed += 1; return }
        let status = VTDecompressionSessionDecodeFrame(session, sampleBuffer: buffer, flags: [], infoFlagsOut: nil) { [weak self] status, _, image, _, _ in
            guard let self else { return }
            self.lock.lock()
            if status == noErr, let image {
                self.decoded += 1
                self.size = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image))
                if let centres = self.markerCentres { self.markers.append(readMarker(image, centres)) }
            } else {
                self.failed += 1
            }
            self.lock.unlock()
        }
        if status != noErr { failed += 1 }
    }

    private func makeSession() {
        if let session { VTDecompressionSessionInvalidate(session) }
        session = nil
        format = nil
        let pointers = parameterSets.map { data -> UnsafeMutablePointer<UInt8> in
            let p = UnsafeMutablePointer<UInt8>.allocate(capacity: data.count)
            data.copyBytes(to: p, count: data.count)
            return p
        }
        defer { pointers.forEach { $0.deallocate() } }
        let constPointers = pointers.map { UnsafePointer($0) }
        let sizes = parameterSets.map(\.count)
        var description: CMFormatDescription?
        let status = hevc
            ? CMVideoFormatDescriptionCreateFromHEVCParameterSets(allocator: nil, parameterSetCount: constPointers.count, parameterSetPointers: constPointers, parameterSetSizes: sizes, nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &description)
            : CMVideoFormatDescriptionCreateFromH264ParameterSets(allocator: nil, parameterSetCount: constPointers.count, parameterSetPointers: constPointers, parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &description)
        guard status == noErr, let description else {
            Log.line("decoder: format description failed \(status)")
            return
        }
        format = description
        let attributes = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange] as CFDictionary
        var created: VTDecompressionSession?
        let made = VTDecompressionSessionCreate(allocator: nil, formatDescription: description, decoderSpecification: nil,
                                                imageBufferAttributes: attributes, outputCallback: nil, decompressionSessionOut: &created)
        if made != noErr { Log.line("decoder: session failed \(made)") }
        session = created
    }

    func finish() {
        if let session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
    }
}

/// One timed capture of one display.
final class CaptureRun: NSObject, SCStreamOutput, SCStreamDelegate {
    let label: String
    let fps: Int
    let pixelSize: (Int, Int)
    let display: CGDirectDisplayID
    /// Block centres of the probe's content marker in this display's frames,
    /// or nil if the probe window's marker isn't on it.
    let markerCentres: [(Int, Int)]?
    private let queue = DispatchQueue(label: "probe.capture", qos: .userInteractive)
    private var stream: SCStream?

    // Video
    private var statuses: [Int: Int] = [:]
    private var completeTimes: [UInt64] = []
    private var captureMarkers: [(display: UInt16, counter: UInt16)?] = []
    private var latencyMs: [Double] = []
    private var surfaceBacked = 0
    private var formats: Set<String> = []
    // Encode and decode
    private var encoder: OpaquePointer?
    private var plan = EmPlan()
    private var decoder: AnnexBDecoder?
    private var submitted = 0
    private var thinned = 0
    private var encodeErrors = 0
    private var encodeUs: [Double] = []
    private var captureToEncodedMs: [Double] = []
    private var displayTimes: [UInt64: UInt64] = [:]
    private var submitTimes: [UInt64: UInt64] = [:]
    private var encodedSubmitTimes: [UInt64] = []
    private var encodedFrames = 0
    private var keyframes = 0
    private var pacer = FramePacer(fps: 1)
    /// The capture marker of each submitted frame, in submission order.
    private var submittedMarkers: [(display: UInt16, counter: UInt16)?] = []
    // Audio
    private var audioBuffers = 0
    private var audioFrames = 0
    private var audioFormat = ""
    private var audioPeak: Float = 0
    private var stopError: Error?

    init(label: String, fps: Int, pixelSize: (Int, Int), display: CGDirectDisplayID, markerCentres: [(Int, Int)]?) {
        self.label = label
        self.fps = fps
        self.pixelSize = pixelSize
        self.display = display
        self.markerCentres = markerCentres
    }

    func start(display: SCDisplay, options: Options, encode: Bool) async throws {
        if encode {
            var error = [CChar](repeating: 0, count: 512)
            encoder = em_encoder_create(options.hevc, UInt32(pixelSize.0), UInt32(pixelSize.1), UInt32(fps), 15_000_000,
                                        options.experimental, &plan, &error, error.count)
            if encoder == nil {
                Log.check(false, "\(label): encoder did not open: \(String(cString: error))")
            } else {
                decoder = AnnexBDecoder(hevc: options.hevc, markerCentres: markerCentres)
                pacer = FramePacer(fps: Int(plan.encode_fps))
                Log.line("\(label): encoder plan \(pixelSize.0)x\(pixelSize.1) encode_fps=\(plan.encode_fps) level_idc=\(plan.level_idc) high_tier=\(plan.high_tier) max_bitrate=\(plan.max_bitrate_bps)")
                if plan.high_tier {
                    Log.line("\(label): ENCODE INFORMATIONAL: experimental HEVC High tier (ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER); its encode checks are INFO lines, not counted")
                } else if Int(plan.encode_fps) < fps {
                    Log.line("\(label): ENCODE CAPPED: \(pixelSize.0)x\(pixelSize.1) at \(fps) Hz plans \(plan.encode_fps) fps on HEVC Main tier, by design; encode at 120 is tested at the Duo outer size")
                } else {
                    Log.line("\(label): ENCODE GATE: \(pixelSize.0)x\(pixelSize.1) at \(plan.encode_fps) fps on \(options.hevc ? "HEVC Main tier" : "H.264")")
                }
            }
        }
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = pixelSize.0
        config.height = pixelSize.1
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        config.queueDepth = 6
        config.showsCursor = true
        config.capturesAudio = true
        config.sampleRate = 48_000
        config.channelCount = 2
        config.excludesCurrentProcessAudio = false // the probe's own tone proves audio arrives
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        self.stream = stream
        try await withDeadline(10) { try await stream.startCapture() }
    }

    func stop() async {
        if let stream {
            do { try await withDeadline(10) { try await stream.stopCapture() } } catch { Log.line("\(label): stopCapture \(error)") }
        }
        queue.sync {
            if let encoder {
                _ = em_encoder_flush(encoder)
                drain()
            }
            decoder?.finish()
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        stopError = error
        Log.line("\(label): stream stopped with error \(error)")
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        switch type {
        case .screen: onVideo(sample)
        case .audio: onAudio(sample)
        default: break
        }
    }

    private func onVideo(_ sample: CMSampleBuffer) {
        let now = mach_absolute_time()
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let raw = info[.status] as? Int else { return }
        statuses[raw, default: 0] += 1
        guard SCFrameStatus(rawValue: raw) == .complete, let image = CMSampleBufferGetImageBuffer(sample) else { return }
        let displayTime = (info[.displayTime] as? UInt64) ?? now
        let presented = CMSampleBufferGetPresentationTimeStamp(sample)
        let marker = markerCentres.flatMap { readMarker(image, $0) }
        completeTimes.append(now)
        captureMarkers.append(marker)
        latencyMs.append(machToMs(now &- displayTime))
        if CVPixelBufferGetIOSurface(image) != nil { surfaceBacked += 1 }
        let fourcc = CVPixelBufferGetPixelFormatType(image)
        formats.insert("\(CVPixelBufferGetWidth(image))x\(CVPixelBufferGetHeight(image)) \(String(format: "%08x", fourcc))")
        guard let encoder else { return }
        // Thin to the planned rate, as the host's encode loop does, by the
        // frame's presentation time: callback jitter must not drop frames.
        let time = presented.isValid ? presented.seconds : machToMs(displayTime) / 1000
        guard pacer.take(time) else {
            thinned += 1
            return
        }
        let number = UInt64(submitted)
        displayTimes[number] = displayTime
        submitTimes[number] = now
        submittedMarkers.append(marker)
        let force = submitted % max(Int(plan.encode_fps), 1) == 0
        if em_encoder_encode(encoder, Unmanaged.passUnretained(image).toOpaque(), number, force) != 0 {
            encodeErrors += 1
            if encodeErrors <= 3 { Log.line("\(label): encode error \(String(cString: em_encoder_error(encoder)))") }
        }
        submitted += 1
        drain()
    }

    private func drain() {
        guard let encoder else { return }
        var frame = EmFrame()
        while em_encoder_next(encoder, &frame) {
            encodedFrames += 1
            if frame.keyframe { keyframes += 1 }
            encodeUs.append(Double(frame.encode_us) / 1000)
            if let shown = displayTimes.removeValue(forKey: frame.frame_number) {
                captureToEncodedMs.append(machToMs(mach_absolute_time() &- shown))
            }
            if let submitted = submitTimes.removeValue(forKey: frame.frame_number) { encodedSubmitTimes.append(submitted) }
            decoder?.decode(Data(bytes: frame.data, count: frame.len))
        }
    }

    private func onAudio(_ sample: CMSampleBuffer) {
        audioBuffers += 1
        audioFrames += CMSampleBufferGetNumSamples(sample)
        if audioFormat.isEmpty, let description = CMSampleBufferGetFormatDescription(sample),
           let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description)?.pointee {
            audioFormat = "\(Int(asbd.mSampleRate))Hz ch=\(asbd.mChannelsPerFrame) bits=\(asbd.mBitsPerChannel) flags=\(asbd.mFormatFlags)"
        }
        // Non-interleaved stereo has one buffer per channel: ask for the size first.
        guard let description = CMSampleBufferGetFormatDescription(sample),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description)?.pointee,
              asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0, asbd.mBitsPerChannel == 32 else { return }
        var needed = 0
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, bufferListSizeNeededOut: &needed, bufferListOut: nil, bufferListSize: 0,
                                                                blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: nil)
        guard needed > 0 else { return }
        let raw = UnsafeMutableRawPointer.allocate(byteCount: needed, alignment: MemoryLayout<AudioBufferList>.alignment)
        defer { raw.deallocate() }
        let list = raw.bindMemory(to: AudioBufferList.self, capacity: 1)
        var block: CMBlockBuffer?
        guard CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, bufferListSizeNeededOut: nil, bufferListOut: list, bufferListSize: needed,
                                                                      blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0,
                                                                      blockBufferOut: &block) == noErr else { return }
        for buffer in UnsafeMutableAudioBufferListPointer(list) {
            guard let data = buffer.mData else { continue }
            let count = Int(buffer.mDataByteSize) / 4
            let samples = data.bindMemory(to: Float.self, capacity: count)
            for i in 0..<count { audioPeak = max(audioPeak, abs(samples[i])) }
        }
    }

    /// Delivered fps over the run, from complete frames only. `target` is the
    /// rate an animated run must reach (nil for the idle run); `expectEncodeFps`
    /// is the planned encode rate the run is meant to prove; `tone` says the
    /// probe's tone played, so audio must have arrived.
    func report(seconds: Double, target: Int?, expectEncodeFps: Int?, tone: Bool) {
        queue.sync {
            let complete = statuses[SCFrameStatus.complete.rawValue] ?? 0
            let idle = statuses[SCFrameStatus.idle.rawValue] ?? 0
            let span = completeTimes.count > 1 ? machToMs(completeTimes.last! - completeTimes.first!) / 1000 : 0
            let delivered = span > 0 ? Double(completeTimes.count - 1) / span : 0
            let gaps = zip(completeTimes.dropFirst(), completeTimes).map { machToMs($0 - $1) }
            let statusText = statuses.sorted { $0.key < $1.key }.map { "\(SCFrameStatus(rawValue: $0.key).map { "\($0)" } ?? "\($0.key)"):\($0.value)" }.joined(separator: ",")
            Log.result(label, [
                ("asked_fps", fps), ("size", "\(pixelSize.0)x\(pixelSize.1)"), ("seconds", String(format: "%.1f", seconds)),
                ("delivered_fps", String(format: "%.1f", delivered)), ("complete", complete), ("idle", idle),
                ("idle_per_s", String(format: "%.1f", Double(idle) / seconds)), ("statuses", statusText),
                ("iosurface", "\(surfaceBacked)/\(completeTimes.count)"), ("formats", formats.sorted().joined(separator: ";")),
            ])
            Log.line("\(label): frame gap ms \(percentiles(gaps))")
            Log.line("\(label): capture latency ms (displayTime -> handler) \(percentiles(latencyMs))")

            let times = completeTimes.map { machToMs($0) / 1000 }
            let window = FrameRate.window(first: times.first ?? 0, seconds: seconds)
            let inWindow = { (t: Double) in t >= window.start && t < window.start + window.length }
            let captureFps = FrameRate.inWindow(times, start: window.start, length: window.length)
            var allMarkers = MarkerStats(display: display), windowMarkers = MarkerStats(display: display)
            for (time, marker) in zip(times, captureMarkers) {
                allMarkers.add(marker)
                if inWindow(time) { windowMarkers.add(marker) }
            }
            let changesPerSecond = Double(windowMarkers.changes) / window.length
            Log.result("\(label)_window", [("window_s", String(format: "%.1f", window.length)), ("capture_fps", String(format: "%.1f", captureFps)),
                                           ("content_changes_per_s", String(format: "%.1f", changesPerSecond)), ("markers", "\"\(allMarkers.summary)\"")])
            if let target {
                if markerCentres == nil {
                    Log.check(false, "\(label): the probe window's marker is not on display \(display), so the captured content can't be proved")
                } else {
                    Log.check(allMarkers.proves, "\(label): every captured frame shows the probe's marker for display \(display) (\(allMarkers.summary))")
                    Log.check(FrameRate.meets(changesPerSecond, target: target),
                              String(format: "%@: content changed every frame: %.1f changes/s over %.0f s, target %d", label, changesPerSecond, window.length, target))
                }
                Log.check(FrameRate.meets(captureFps, target: target),
                          String(format: "%@: capture delivered %.1f fps over %.0f s, at least 0.9 × %d", label, captureFps, window.length, target))
            }
            if encoder != nil {
                let emitted = em_encoder_emitted(encoder!)
                Log.result("\(label)_encode", [
                    ("encode_fps", plan.encode_fps), ("submitted", submitted), ("thinned", thinned), ("encoded", encodedFrames),
                    ("keyframes", keyframes), ("errors", encodeErrors),
                    ("emitted", emitted.valid ? "profile=\(emitted.profile_idc) high_tier=\(emitted.high_tier) level_idc=\(emitted.level_idc)" : "none"),
                    ("decoded", decoder?.decoded ?? 0), ("decode_failed", decoder?.failed ?? 0),
                    ("decoded_size", "\(decoder?.size.0 ?? 0)x\(decoder?.size.1 ?? 0)"),
                ])
                Log.line("\(label): encode ms (submit -> callback) \(percentiles(encodeUs))")
                Log.line("\(label): capture -> encoded ms (displayTime -> packet) \(percentiles(captureToEncodedMs))")
                // Experimental High tier is informational: INFO lines, not counted.
                let check: (Bool, String) -> Void = plan.high_tier ? Log.info : Log.check
                let encodeTimes = encodedSubmitTimes.map { machToMs($0) / 1000 }
                let encodeFps = FrameRate.inWindow(encodeTimes, start: window.start, length: window.length)
                // One loss budget for submit -> encode -> decode (PipelineLoss);
                // errors (a refused submit, a failed output callback, a
                // decoder failure) fail on their own.
                let callbackErrors = Int(em_encoder_callback_errors(encoder!))
                let pipeline = PipelineLoss(submitted: submitted, encoded: encodedFrames, decoded: decoder?.decoded ?? 0,
                                            match: markerCentres.map { _ in
                                                DecodedMatch(submitted: submittedMarkers, decoded: decoder?.markers ?? [], display: display)
                                            },
                                            encodeErrors: encodeErrors + callbackErrors, decodeFailures: decoder?.failed ?? 0)
                Log.result("\(label)_encode_window", [("encode_fps", String(format: "%.1f", encodeFps)), ("planned", plan.encode_fps),
                                                      ("submit_errors", encodeErrors), ("callback_errors", callbackErrors),
                                                      ("vt_dropped", em_encoder_dropped(encoder!)), ("decode_failed", decoder?.failed ?? 0),
                                                      ("pipeline", "\"\(pipeline.summary)\"")])
                check(pipeline.passes,
                      "\(label): no encoder or decoder errors, submit -> encode -> decode lost \(pipeline.lost) of \(submitted) frames (allowed \(pipeline.allowed)), "
                      + "and every decoded frame is a submitted frame of display \(display), in order, none repeated (\(pipeline.summary))")
                check(emitted.valid && !(emitted.high_tier && !plan.high_tier), "\(label): emitted tier matches the plan")
                if let target {
                    // No faster than the input: a 60 Hz baseline display
                    // can't feed a plan that allows 120.
                    let required = min(Int(plan.encode_fps), target)
                    check(FrameRate.meets(encodeFps, target: required),
                          String(format: "%@: encoded %.1f fps over %.0f s, at least 0.9 × %d (planned %d, input target %d)",
                                 label, encodeFps, window.length, required, plan.encode_fps, target))
                }
                if let expectEncodeFps {
                    check(Int(plan.encode_fps) == expectEncodeFps && !plan.high_tier,
                          "\(label): \(pixelSize.0)x\(pixelSize.1) plans \(expectEncodeFps) fps on Main tier (planned \(plan.encode_fps), high_tier=\(plan.high_tier))")
                }
                em_encoder_destroy(encoder)
                encoder = nil
            }
            Log.result("\(label)_audio", [("buffers", audioBuffers), ("frames", audioFrames),
                                          ("frames_per_s", String(format: "%.0f", Double(audioFrames) / seconds)),
                                          ("format", "\"\(audioFormat)\""), ("peak", String(format: "%.3f", audioPeak))])
            if tone {
                Log.check(audioFrames > 0 && audioPeak >= 0.01,
                          String(format: "%@: system audio captured the probe's tone (%d frames, peak %.3f)", label, audioFrames, audioPeak))
            }
            if let stopError { Log.check(false, "\(label): stream error \(stopError)") }
        }
    }
}

/// A quiet 440 Hz tone, so system-audio capture has something to hear.
final class Tone {
    private let engine = AVAudioEngine()
    private var phase: Float = 0
    func play(seconds: Double) {
        let rate = Float(engine.outputNode.outputFormat(forBus: 0).sampleRate)
        let source = AVAudioSourceNode { [weak self] _, _, frames, buffers -> OSStatus in
            guard let self else { return noErr }
            for buffer in UnsafeMutableAudioBufferListPointer(buffers) {
                let out = buffer.mData!.assumingMemoryBound(to: Float.self)
                var p = self.phase
                for i in 0..<Int(frames) {
                    out[i] = 0.05 * sinf(p)
                    p += 2 * .pi * 440 / rate
                }
                if buffer.mData == UnsafeMutableAudioBufferListPointer(buffers).last?.mData { self.phase = p.truncatingRemainder(dividingBy: 2 * .pi) }
            }
            return noErr
        }
        engine.attach(source)
        engine.connect(source, to: engine.mainMixerNode, format: nil)
        do { try engine.start() } catch { Log.line("tone: \(error)"); return }
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { self.engine.stop() }
    }
}

@MainActor
struct CaptureSpike {
    let app: ProbeApp

    static func pixelSize(_ id: CGDirectDisplayID) -> (Int, Int) {
        guard let mode = CGDisplayCopyDisplayMode(id) else { return (Int(CGDisplayPixelsWide(id)), Int(CGDisplayPixelsHigh(id))) }
        return (mode.pixelWidth, mode.pixelHeight)
    }

    /// Block centres, in `id`'s pixels, of the marker the probe window draws,
    /// or nil if the window's marker isn't entirely on that display.
    private func markerCentres(on id: CGDirectDisplayID, pixelWidth: Int) -> [(Int, Int)]? {
        guard let window = app.animation.window else { return nil }
        let rect = window.convertToScreen(app.animation.convert(app.animation.markerRect, to: nil))
        let mainHeight = NSScreen.screens.first?.frame.height ?? 0
        let global = CGRect(x: rect.minX, y: mainHeight - rect.maxY, width: rect.width, height: rect.height)
        let bounds = CGDisplayBounds(id)
        guard bounds.contains(global), bounds.width > 0 else { return nil }
        let scale = Double(pixelWidth) / bounds.width
        return Marker.centres(x: (global.minX - bounds.minX) * scale, y: (global.minY - bounds.minY) * scale, blockPx: Marker.blockPt * scale)
    }

    /// Captures `displayID` at each rate, then once with the animation
    /// stopped to count idle frames. In the gate the target is the asked
    /// rate; on the main display (a baseline) it is capped at its refresh.
    func capture(displayID: CGDirectDisplayID, name: String, rates: [Int], idle: Bool = true, gate: Bool = false,
                 expectEncodeFps: Int? = nil) async {
        guard CGPreflightScreenCaptureAccess() else {
            Log.check(false, "spike d needs Screen Recording; run --mode grant first")
            return
        }
        let content: SCShareableContent
        do {
            content = try await withDeadline(10) { try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true) }
        } catch {
            Log.check(false, "SCShareableContent failed: \(error)")
            return
        }
        Log.line("SCK displays: " + content.displays.map { "\($0.displayID) \($0.width)x\($0.height)pt" }.joined(separator: ", "))
        guard let display = content.displays.first(where: { $0.displayID == displayID }) else {
            Log.check(false, "display \(displayID) is not shareable")
            return
        }
        let size = Self.pixelSize(displayID)
        let refresh = CGDisplayCopyDisplayMode(displayID)?.refreshRate ?? 0
        Log.line("capture \(name): display \(displayID) \(display.width)x\(display.height)pt \(size.0)x\(size.1)px refresh=\(refresh)")
        let centres = markerCentres(on: displayID, pixelWidth: size.0)
        Log.line("capture \(name): marker \(centres.map { "at pixel (\($0[0].0),\($0[0].1))…" } ?? "not on this display")")
        for fps in rates {
            app.animation.animating = true
            app.say("Spike d: capturing \(name) at \(fps) fps for \(Int(app.options.seconds)) s")
            let target = gate || refresh < 1 ? fps : min(fps, Int(refresh.rounded()))
            if target != fps { Log.line("capture \(name) at \(fps): this display refreshes at \(refresh) Hz, so the baseline's target is \(target) fps") }
            await timed(display: display, label: "capture_\(name)_\(fps)", fps: fps, size: size, encode: true, centres: centres,
                        target: target, expectEncodeFps: expectEncodeFps, tone: app.options.tone)
        }
        guard idle else { return }
        app.animation.animating = false
        app.say("Spike d: capturing \(name) with nothing changing (idle frames)")
        await timed(display: display, label: "capture_\(name)_idle", fps: 60, size: size, encode: false, centres: centres,
                    target: nil, expectEncodeFps: nil, tone: false)
        app.animation.animating = true
    }

    private func timed(display: SCDisplay, label: String, fps: Int, size: (Int, Int), encode: Bool, centres: [(Int, Int)]?,
                       target: Int?, expectEncodeFps: Int?, tone: Bool) async {
        let run = CaptureRun(label: label, fps: fps, pixelSize: size, display: display.displayID, markerCentres: centres)
        do { try await run.start(display: display, options: app.options, encode: encode) } catch {
            Log.check(false, "\(label): startCapture failed: \(error)")
            await run.stop()
            return
        }
        if tone { Tone().play(seconds: 1.5) }
        if !app.options.tone, target != nil { Log.line("INFO \(label): audio not checked (--no-tone)") }
        try? await Task.sleep(nanoseconds: UInt64(app.options.seconds * 1e9))
        await run.stop()
        run.report(seconds: app.options.seconds, target: target, expectEncodeFps: expectEncodeFps, tone: tone)
    }

    func run() async {
        Log.line("=== spike d: ScreenCaptureKit on the main display (baseline; the virtual display is captured in spike c) ===")
        await capture(displayID: CGMainDisplayID(), name: "main", rates: [60, 120])
    }
}

/// No permissions needed: IOSurface-backed 420v buffers through the encoder
/// FFI and back through the decoder, to prove spike d's encode path before
/// capture is allowed.
@MainActor
struct SelfTest {
    func run(options: Options) {
        Log.line("=== selftest: pixel buffers -> direct VideoToolbox encoder -> decoder ===")
        for (width, height, fps) in [(2670, 1878, 60), (2034, 1398, 120), (3440, 1440, 60)] {
            let label = "selftest_\(width)x\(height)_\(fps)"
            var plan = EmPlan()
            var error = [CChar](repeating: 0, count: 512)
            guard let encoder = em_encoder_create(options.hevc, UInt32(width), UInt32(height), UInt32(fps), 15_000_000,
                                                  options.experimental, &plan, &error, error.count) else {
                Log.check(false, "\(label): encoder did not open: \(String(cString: error))")
                continue
            }
            let attributes: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                kCVPixelBufferWidthKey: width, kCVPixelBufferHeightKey: height,
                kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any](),
            ]
            var pool: CVPixelBufferPool?
            CVPixelBufferPoolCreate(nil, nil, attributes as CFDictionary, &pool)
            let decoder = AnnexBDecoder(hevc: options.hevc)
            var encoded = 0, failed = 0
            var frame = EmFrame()
            for i in 0..<60 {
                var buffer: CVPixelBuffer?
                CVPixelBufferPoolCreatePixelBuffer(nil, pool!, &buffer)
                guard let buffer else { failed += 1; continue }
                CVPixelBufferLockBaseAddress(buffer, [])
                let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0)!.assumingMemoryBound(to: UInt8.self)
                let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
                for y in 0..<height { memset(luma + y * stride, Int32(16 + (y + i * 8) % 200), width) }
                let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)!
                memset(chroma, 128, CVPixelBufferGetBytesPerRowOfPlane(buffer, 1) * CVPixelBufferGetHeightOfPlane(buffer, 1))
                CVPixelBufferUnlockBaseAddress(buffer, [])
                if em_encoder_encode(encoder, Unmanaged.passUnretained(buffer).toOpaque(), UInt64(i), i % 30 == 0) != 0 { failed += 1 }
                while em_encoder_next(encoder, &frame) { encoded += 1; decoder.decode(Data(bytes: frame.data, count: frame.len)) }
            }
            _ = em_encoder_flush(encoder)
            while em_encoder_next(encoder, &frame) { encoded += 1; decoder.decode(Data(bytes: frame.data, count: frame.len)) }
            decoder.finish()
            let emitted = em_encoder_emitted(encoder)
            Log.result(label, [("encode_fps", plan.encode_fps), ("planned_level_idc", plan.level_idc), ("planned_high_tier", plan.high_tier),
                               ("emitted_level_idc", emitted.level_idc), ("emitted_high_tier", emitted.high_tier),
                               ("encoded", encoded), ("failed", failed), ("decoded", decoder.decoded), ("decode_failed", decoder.failed),
                               ("decoded_size", "\(decoder.size.0)x\(decoder.size.1)")])
            Log.check(failed == 0 && encoded == 60 && decoder.decoded == 60 && decoder.size == (width, height), "\(label): 60 frames encoded and decoded at size")
            Log.check(emitted.valid && emitted.high_tier == plan.high_tier && emitted.level_idc >= plan.level_idc, "\(label): emitted tier and level match the plan")
            em_encoder_destroy(encoder)
        }
    }
}
