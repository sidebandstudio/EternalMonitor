import SwiftUI
import UIKit

/// Full-screen touch layer that relays gestures to the host PC. Present only
/// when input relay is negotiated; SwiftUI controls above it (HUD, disconnect)
/// still hit-test first.
struct TouchRelayView: UIViewRepresentable {
    @EnvironmentObject var connectionManager: ConnectionManager
    @EnvironmentObject var settings: AppSettings
    @Binding var keyboardVisible: Bool
    var active: Bool
    var onToggleHUD: () -> Void

    func makeUIView(context: Context) -> RelayTouchUIView {
        let view = RelayTouchUIView()
        view.onEvents = { [weak connectionManager] events in
            connectionManager?.sendInputs(events)
        }
        view.onKeyboardDismiss = { keyboardVisible = false }
        view.onToggleHUD = onToggleHUD
        return view
    }

    func updateUIView(_ view: RelayTouchUIView, context: Context) {
        view.videoSize = connectionManager.videoSize
        view.onToggleHUD = onToggleHUD
        view.commandAsControl = settings.commandAsControl
        view.keyboardSupported = connectionManager.sessionHasKeyboard
        view.nativePenSupported = connectionManager.sessionHasPen
        view.drawingMode = connectionManager.sessionDrawingMode
        view.active = active
        view.keyboardVisible = keyboardVisible
        view.updateKeyboardPresentation()
    }
}

final class RelayTouchUIView: UIView {
    var onEvents: (([WireInputEvent]) -> Void)?
    var onKeyboardDismiss: (() -> Void)?
    var commandAsControl = true
    var keyboardSupported = false
    var nativePenSupported = false
    var drawingMode = false
    var keyboardVisible = false
    var active = true { didSet { if !active && oldValue { cancelInput(); resignFirstResponder(); keyboardInput.resignFirstResponder() } } }
    override var canBecomeFirstResponder: Bool { active }
    var onToggleHUD: (() -> Void)?
    var videoSize: CGSize = .zero {
        didSet {
            if oldValue != videoSize && pencil.isDown { cancelInput() }
            machine.videoPixelSize = videoSize
        }
    }

    private var machine = TouchRelayMachine()
    private var pencil = PencilRelayMachine()
    private var pencilTouch: UITouch?
    private var ignoredTouches = Set<UITouch>()
    private var inputBounds = CGSize.zero
    private var keyboard = KeyboardRelayMachine()
    private var pointer = PointerRelayMachine()
    private lazy var keyboardInput: RelayKeyboardInputView = {
        let view = RelayKeyboardInputView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        view.relay = self
        view.onDismiss = { [weak self] in self?.onKeyboardDismiss?() }
        view.isAccessibilityElement = false
        view.accessibilityLabel = "Remote keyboard"
        view.accessibilityIdentifier = "keyboard.input"
        view.accessibilityTraits = .allowsDirectInteraction
        addSubview(view)
        return view
    }()
    private var holdTimer: DispatchWorkItem?
    private var hudTapFired = false
    private var softwareKeyboardPresented = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = .clear
        // VoiceOver: this surface IS the remote pointer — pass touches
        // straight through instead of narrating them.
        isAccessibilityElement = true
        accessibilityLabel = "Remote control surface. Touches control the PC."
        accessibilityTraits = .allowsDirectInteraction
        accessibilityIdentifier = "display.relay"
        let scroll = UIPanGestureRecognizer(target: self, action: #selector(pointerScrolled(_:)))
        scroll.allowedScrollTypesMask = .all
        scroll.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.indirectPointer.rawValue)]
        scroll.cancelsTouchesInView = false
        addGestureRecognizer(scroll)
        let hover = UIHoverGestureRecognizer(target: self, action: #selector(pointerHovered(_:)))
        hover.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.indirectPointer.rawValue)]
        addGestureRecognizer(hover)
        let pencilHover = UIHoverGestureRecognizer(target: self, action: #selector(pencilHovered(_:)))
        pencilHover.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.pencil.rawValue)]
        addGestureRecognizer(pencilHover)
        NotificationCenter.default.addObserver(self, selector: #selector(cancelInput),
            name: UIApplication.willResignActiveNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(resumeInput),
            name: UIApplication.didBecomeActiveNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardFrameChanged(_:)),
            name: UIResponder.keyboardDidChangeFrameNotification, object: nil)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    private var mapper: ContentRectMapper {
        ContentRectMapper(viewSize: bounds.size, videoSize: videoSize)
    }

    private func norm(_ touch: UITouch) -> TouchRelayMachine.Point? {
        mapper.normalize(touch.location(in: self)).map {
            TouchRelayMachine.Point(x: $0.x, y: $0.y)
        }
    }

    private func centroid(of event: UIEvent?) -> TouchRelayMachine.Point? {
        guard let touches = event?.allTouches?.filter({
            $0.type == .direct && !ignoredTouches.contains($0)
                && ($0.phase == .began || $0.phase == .moved || $0.phase == .stationary)
        }), !touches.isEmpty else { return nil }
        var sum = CGPoint.zero
        for touch in touches {
            let p = touch.location(in: self)
            sum.x += p.x
            sum.y += p.y
        }
        let mean = CGPoint(x: sum.x / CGFloat(touches.count), y: sum.y / CGFloat(touches.count))
        return mapper.normalize(mean).map { TouchRelayMachine.Point(x: $0.x, y: $0.y) }
    }

    private func run(_ outputs: [TouchRelayMachine.Output]) {
        var lastID: UInt32?
        var events: [WireInputEvent] = []
        for output in outputs {
            switch output {
            case .send(let event):
                // The connection now owns edge duplication and the shared ID sequence.
                if lastID != event.eventId { events.append(event); lastID = event.eventId }
            case .toggleHUD: onToggleHUD?()
            }
        }
        onEvents?(events)
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window == nil { cancelInput() } else { updateKeyboardPresentation() }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        // Rotation changes the mapping under a held tip. Release before a
        // subsequent sample could draw a line across the newly fitted image.
        if inputBounds != bounds.size && pencil.isDown { cancelInput() }
        inputBounds = bounds.size
    }

    func updateKeyboardPresentation() {
        guard window != nil, active else { return }
        let showingKeyboard = keyboardVisible && keyboardSupported
        // An accessible parent hides its children from VoiceOver and XCTest.
        // Expose the active responder so typing reaches the remote keyboard.
        isAccessibilityElement = !showingKeyboard
        keyboardInput.isAccessibilityElement = showingKeyboard
        if showingKeyboard {
            if !keyboardInput.isFirstResponder { keyboardInput.becomeFirstResponder() }
        } else {
            if keyboardInput.isFirstResponder {
                onEvents?(keyboard.cancel())
                keyboardInput.updateSticky(keyboard.sticky)
                keyboardInput.resignFirstResponder()
            }
            if !isFirstResponder { becomeFirstResponder() }
        }
    }

    @objc private func resumeInput() { updateKeyboardPresentation() }

    @objc private func keyboardFrameChanged(_ notification: Notification) {
        guard keyboardVisible, keyboardInput.isFirstResponder, let window,
              let frame = notification.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect else { return }
        let visible = window.bounds.intersection(window.screen.coordinateSpace.convert(frame, to: window))
        let accessoryHeight = keyboardInput.inputAccessoryView?.bounds.height ?? 0
        if !visible.isNull && visible.height > accessoryHeight + window.safeAreaInsets.bottom + 1 {
            softwareKeyboardPresented = true
        } else if softwareKeyboardPresented {
            // The system Hide Keyboard key leaves an accessory-only responder.
            // Finish that dismissal, while still permitting an accessory bar
            // when a physical keyboard was attached from the start.
            softwareKeyboardPresented = false
            keyboardVisible = false
            DispatchQueue.main.async { [weak self] in self?.keyboardInput.resignFirstResponder() }
        }
    }

    func keyboardDidResign() {
        keyboardVisible = false
        softwareKeyboardPresented = false
        onEvents?(keyboard.cancel())
        keyboardInput.updateSticky(keyboard.sticky)
        // UIKit's Hide Keyboard key also dismisses the responder. Keep the
        // SwiftUI toggle in sync without changing it during updateUIView.
        DispatchQueue.main.async { [weak self] in self?.onKeyboardDismiss?() }
    }

    @objc private func cancelInput() {
        holdTimer?.cancel()
        holdTimer = nil
        onEvents?(keyboard.cancel() + pointer.ended(at: nil) + pencil.cancel())
        pencilTouch = nil
        ignoredTouches.removeAll()
        // Cancel active touch edges without toggling the HUD.
        run(machine.threeFingerTap(timeUs: ControlChannel.clientNowUs()).filter {
            if case .toggleHUD = $0 { return false }; return true
        })
        machine = TouchRelayMachine()
        machine.videoPixelSize = videoSize
        keyboardInput.updateSticky(keyboard.sticky)
    }

    private func modifierBits(_ flags: UIKeyModifierFlags) -> UInt8 {
        (flags.contains(.shift) ? 1 : 0) | (flags.contains(.control) ? 2 : 0)
            | (flags.contains(.alternate) ? 4 : 0) | (flags.contains(.command) ? 8 : 0)
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        guard active, keyboardSupported else { return super.pressesBegan(presses, with: event) }
        for press in presses {
            guard let key = press.key else { continue }
            onEvents?(keyboard.hardware(usage: UInt16(key.keyCode.rawValue), down: true,
                modifiers: modifierBits(key.modifierFlags), commandAsControl: commandAsControl))
        }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        guard active, keyboardSupported else { return super.pressesEnded(presses, with: event) }
        for press in presses {
            guard let key = press.key else { continue }
            onEvents?(keyboard.hardware(usage: UInt16(key.keyCode.rawValue), down: false,
                modifiers: modifierBits(key.modifierFlags), commandAsControl: commandAsControl))
        }
        keyboardInput.updateSticky(keyboard.sticky)
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        onEvents?(keyboard.cancel())
        keyboardInput.updateSticky(keyboard.sticky)
    }

    func insertRemoteText(_ text: String) {
        guard active, keyboardSupported else { return }
        onEvents?(keyboard.text(text)); keyboardInput.updateSticky(keyboard.sticky)
    }
    func tapRemoteKey(_ usage: UInt16) {
        guard active, keyboardSupported else { return }
        onEvents?(keyboard.tap(usage)); keyboardInput.updateSticky(keyboard.sticky)
    }
    func toggleRemoteModifier(_ usage: UInt16) {
        guard active, keyboardSupported else { return }
        onEvents?(keyboard.toggleSticky(usage)); keyboardInput.updateSticky(keyboard.sticky)
    }

    private func point(_ location: CGPoint) -> TouchRelayMachine.Point? {
        mapper.normalize(location).map { TouchRelayMachine.Point(x: $0.x, y: $0.y) }
    }

    @objc private func pointerHovered(_ gesture: UIHoverGestureRecognizer) {
        guard active, keyboardSupported, !pencil.isDown, gesture.state == .began || gesture.state == .changed else { return }
        onEvents?(pointer.hover(at: point(gesture.location(in: self)), timeUs: ControlChannel.clientNowUs()))
    }

    @objc private func pointerScrolled(_ gesture: UIPanGestureRecognizer) {
        guard active, keyboardSupported, !pencil.isDown else { return }
        let delta = gesture.translation(in: self)
        gesture.setTranslation(.zero, in: self)
        let rect = mapper.contentRect
        guard rect.width > 0, rect.height > 0 else { return }
        onEvents?(pointer.scroll(at: point(gesture.location(in: self)),
            delta: CGSize(width: delta.x * videoSize.width / rect.width,
                          height: delta.y * videoSize.height / rect.height)))
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard active else { return }
        let now = ControlChannel.clientNowUs()
        // Process Pencil first even when UIKit delivers palm and tip together.
        if nativePenSupported, let touch = touches.first(where: { $0.type == .pencil }) {
            if let reading = pencilReading(touch) {
                holdTimer?.cancel()
                run(machine.threeFingerTap(timeUs: now).filter {
                    if case .toggleHUD = $0 { return false }; return true
                })
                machine = TouchRelayMachine()
                machine.videoPixelSize = videoSize
                ignoredTouches.formUnion(event?.allTouches?.filter { $0.type != .pencil } ?? [])
                onEvents?(pointer.ended(at: nil) + pencil.began(reading))
                pencilTouch = touch
            }
        }
        for touch in touches {
            if touch.type == .pencil && (nativePenSupported || drawingMode) { continue }
            if touch.type != .indirectPointer && (drawingMode || pencil.isDown || ignoredTouches.contains(touch)) {
                ignoredTouches.insert(touch)
                continue
            }
            if pencil.isDown { continue }
            if touch.type == .indirectPointer {
                if keyboardSupported { onEvents?(pointer.began(at: norm(touch), mask: UInt8(truncatingIfNeeded: event?.buttonMask.rawValue ?? 1))) }
                continue
            }
            run(machine.touchBegan(
                at: norm(touch),
                isPencil: touch.type == .pencil,
                timeUs: now
            ))
        }

        let activeCount = event?.allTouches?.filter {
            $0.type == .direct && !ignoredTouches.contains($0) && $0.phase != .ended && $0.phase != .cancelled
        }.count ?? 0
        if activeCount >= 3 && !hudTapFired {
            hudTapFired = true
            run(machine.threeFingerTap(timeUs: now))
        }

        guard activeCount > 0, !pencil.isDown, !drawingMode else { return }
        holdTimer?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.run(self.machine.holdTimerFired(timeUs: ControlChannel.clientNowUs()))
        }
        holdTimer = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + .microseconds(Int(TouchRelayMachine.holdRightClickUs)),
            execute: work
        )
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard active else { return }
        if let pencilTouch, touches.contains(pencilTouch) {
            let samples = event?.coalescedTouches(for: pencilTouch) ?? [pencilTouch]
            let events = samples.compactMap { pencilReading($0, clamp: true) }.flatMap { pencil.moved($0) }
            onEvents?(events)
            return
        }
        guard !pencil.isDown, let touch = touches.first(where: {
            !ignoredTouches.contains($0) && (!drawingMode || $0.type == .indirectPointer)
                && (!nativePenSupported || $0.type != .pencil)
        }) else { return }
        if touch.type == .indirectPointer {
            if keyboardSupported { onEvents?(pointer.moved(to: norm(touch))) }
            return
        }
        let force = touch.maximumPossibleForce > 0 ? touch.force / touch.maximumPossibleForce : 0
        run(machine.touchMoved(
            to: norm(touch),
            centroid: centroid(of: event),
            isPencil: touch.type == .pencil,
            force: force,
            timeUs: ControlChannel.clientNowUs()
        ))
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        finish(touches, with: event, cancelled: false)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        finish(touches, with: event, cancelled: true)
    }

    private func finish(_ touches: Set<UITouch>, with event: UIEvent?, cancelled: Bool) {
        let now = ControlChannel.clientNowUs()
        if let pencilTouch, touches.contains(pencilTouch) {
            if !cancelled {
                let samples = event?.coalescedTouches(for: pencilTouch) ?? []
                onEvents?(samples.compactMap { pencilReading($0, clamp: true) }.flatMap { pencil.moved($0) })
            }
            onEvents?(pencil.ended(pencilReading(pencilTouch, clamp: true), cancelled: cancelled))
            self.pencilTouch = nil
        }
        for touch in touches {
            if ignoredTouches.remove(touch) != nil { continue }
            if touch.type == .pencil && (nativePenSupported || drawingMode) { continue }
            if pencil.isDown || (drawingMode && touch.type != .indirectPointer) { continue }
            if touch.type == .indirectPointer {
                if keyboardSupported { onEvents?(pointer.ended(at: norm(touch), remainingMask: UInt8(truncatingIfNeeded: event?.buttonMask.rawValue ?? 0))) }
                continue
            }
            run(machine.touchEnded(at: norm(touch), cancelled: cancelled, timeUs: now))
        }
        let remaining = event?.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.count ?? 0
        if remaining == 0 {
            holdTimer?.cancel()
            holdTimer = nil
            hudTapFired = false
        }
    }

    private func pencilReading(_ touch: UITouch, clamp: Bool = false) -> PencilReading? {
        var location = touch.preciseLocation(in: self)
        if clamp {
            let rect = mapper.contentRect
            guard rect.width > 0, rect.height > 0 else { return nil }
            location.x = min(rect.maxX, max(rect.minX, location.x))
            location.y = min(rect.maxY, max(rect.minY, location.y))
        }
        guard let point = point(location) else { return nil }
        return PencilReading(point: point, force: touch.force, maximumForce: touch.maximumPossibleForce,
            altitude: touch.altitudeAngle, azimuth: touch.azimuthAngle(in: self),
            timeUs: UInt64(max(0, touch.timestamp) * 1_000_000))
    }

    @objc private func pencilHovered(_ gesture: UIHoverGestureRecognizer) {
        guard active, nativePenSupported, !pencil.isDown else { return }
        if (gesture.state == .began || gesture.state == .changed), let point = point(gesture.location(in: self)) {
            onEvents?(pencil.hover(PencilReading(point: point, force: 0, maximumForce: 1,
                altitude: gesture.altitudeAngle, azimuth: gesture.azimuthAngle(in: self),
                timeUs: ControlChannel.clientNowUs())))
        } else {
            onEvents?(pencil.hover(nil))
        }
    }
}
