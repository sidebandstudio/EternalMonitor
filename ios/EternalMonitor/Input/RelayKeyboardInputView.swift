import SwiftUI
import UIKit

/// A software-keyboard responder. It keeps no local text; the PC owns the text.
final class RelayKeyboardInputView: UIView, UIKeyInput {
    weak var relay: RelayTouchUIView?
    var onDismiss: (() -> Void)?
    var hasText: Bool { true }
    var keyboardType: UIKeyboardType = .asciiCapable
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    override var canBecomeFirstResponder: Bool { true }
    override var inputAccessoryView: UIView? { accessory }
    private var modifierButtons: [UInt16: UIButton] = [:]

    override func resignFirstResponder() -> Bool {
        let wasFirstResponder = isFirstResponder
        let resigned = super.resignFirstResponder()
        if wasFirstResponder && resigned { relay?.keyboardDidResign() }
        return resigned
    }

    func insertText(_ text: String) { relay?.insertRemoteText(text) }
    func deleteBackward() { relay?.tapRemoteKey(0x2a) }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        relay?.pressesBegan(presses, with: event)
    }
    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        relay?.pressesEnded(presses, with: event)
    }
    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        relay?.pressesCancelled(presses, with: event)
    }

    func updateSticky(_ held: Set<UInt16>) {
        for (usage, button) in modifierButtons {
            button.isSelected = held.contains(usage)
            button.accessibilityValue = held.contains(usage) ? "On" : "Off"
            button.configuration?.baseBackgroundColor = held.contains(usage) ? UIColor(Theme.amber) : .secondarySystemFill
            button.configuration?.baseForegroundColor = held.contains(usage) ? .black : .label
        }
    }

    private lazy var accessory: UIInputView = {
        let view = UIInputView(frame: CGRect(x: 0, y: 0, width: 700, height: 52), inputViewStyle: .keyboard)
        let scroll = UIScrollView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.showsHorizontalScrollIndicator = false
        view.addSubview(scroll)
        let done = UIButton(type: .system)
        var doneConfiguration = UIButton.Configuration.plain()
        doneConfiguration.title = "Done"
        doneConfiguration.baseForegroundColor = UIColor(Theme.amber)
        doneConfiguration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8)
        done.configuration = doneConfiguration
        done.accessibilityIdentifier = "keyboard.done"
        done.translatesAutoresizingMaskIntoConstraints = false
        done.addAction(UIAction { [weak self] _ in self?.onDismiss?() }, for: .touchUpInside)
        view.addSubview(done)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor),
            scroll.topAnchor.constraint(equalTo: view.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            scroll.trailingAnchor.constraint(equalTo: done.leadingAnchor, constant: -4),
            done.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -4),
            done.topAnchor.constraint(equalTo: view.topAnchor, constant: 4),
            done.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -4),
            done.widthAnchor.constraint(greaterThanOrEqualToConstant: 44)
        ])
        let stack = UIStackView()
        stack.axis = .horizontal
        stack.spacing = 4
        stack.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 4),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -4),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 4),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -4),
            stack.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -8)
        ])
        let keys: [(String, String, UInt16)] = [
            ("Esc", "Escape", 0x29), ("Tab", "Tab", 0x2b),
            ("Ctrl", "Control", 0xe0), ("Alt", "Alt", 0xe2), ("Win", "Windows", 0xe3),
            ("←", "Left arrow", 0x50), ("↑", "Up arrow", 0x52),
            ("↓", "Down arrow", 0x51), ("→", "Right arrow", 0x4f),
            ("Home", "Home", 0x4a), ("End", "End", 0x4d), ("Del", "Delete", 0x4c)
        ]
        for (title, label, usage) in keys {
            let button = UIButton(type: .system)
            var configuration = UIButton.Configuration.gray()
            configuration.title = title
            configuration.baseForegroundColor = .label
            configuration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 6, bottom: 0, trailing: 6)
            button.configuration = configuration
            button.accessibilityLabel = label
            button.accessibilityIdentifier = "keyboard.key.\(usage)"
            button.widthAnchor.constraint(greaterThanOrEqualToConstant: 44).isActive = true
            if KeyboardRelayMachine.modifierBit(usage) != 0 { modifierButtons[usage] = button }
            button.addAction(UIAction { [weak self] _ in
                guard let self else { return }
                if KeyboardRelayMachine.modifierBit(usage) != 0 { self.relay?.toggleRemoteModifier(usage) }
                else { self.relay?.tapRemoteKey(usage) }
            }, for: .touchUpInside)
            stack.addArrangedSubview(button)
        }
        return view
    }()
}
