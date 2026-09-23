import SwiftUI
import CoreText
import CoreFoundation
import os

// MARK: - Fonts
//
// Geist and Geist Mono, shared with the Windows host and the website. The
// faces are listed in Info.plist and registered again here so the app knows
// which ones actually loaded; anything missing falls back to the system font.

enum AppFontFace {
    case regular
    case medium
    case semibold
    case mono
    case monoMedium

    var postScriptName: String {
        switch self {
        case .regular: "Geist-Regular"
        case .medium: "Geist-Medium"
        case .semibold: "Geist-SemiBold"
        case .mono: "GeistMono-Regular"
        case .monoMedium: "GeistMono-Medium"
        }
    }

    func font(size: CGFloat, relativeTo style: Font.TextStyle) -> Font {
        if FontRegistry.shared.availablePostScriptNames.contains(postScriptName) {
            // relativeTo makes the custom faces track Dynamic Type.
            return Font.custom(postScriptName, size: size, relativeTo: style)
        }
        switch self {
        case .regular: return .system(size: size, weight: .regular)
        case .medium: return .system(size: size, weight: .medium)
        case .semibold: return .system(size: size, weight: .semibold)
        case .mono: return .system(size: size, weight: .regular, design: .monospaced)
        case .monoMedium: return .system(size: size, weight: .medium, design: .monospaced)
        }
    }
}

final class FontRegistry: @unchecked Sendable {
    static let shared = FontRegistry()

    private(set) var availablePostScriptNames: Set<String> = []
    private let logger = Logger(subsystem: "com.eternal.monitor", category: "fonts")
    private var didRegister = false

    private init() {}

    func registerBundledFonts() {
        guard !didRegister else { return }
        didRegister = true

        let files = ["Geist-Regular", "Geist-Medium", "Geist-SemiBold", "GeistMono-Regular", "GeistMono-Medium"]
        let bundle = Bundle.main
        let urls = files.compactMap {
            bundle.url(forResource: $0, withExtension: "ttf")
                ?? bundle.url(forResource: $0, withExtension: "ttf", subdirectory: "Fonts")
        }
        if urls.isEmpty {
            logger.error("No bundled font files were found in the app bundle")
            return
        }
        for url in urls {
            var error: Unmanaged<CFError>?
            if !CTFontManagerRegisterFontsForURL(url as CFURL, .process, &error), let error {
                let resolved = error.takeRetainedValue()
                // Info.plist already registered it. Expected, not a failure.
                if CFErrorGetCode(resolved) != CTFontManagerError.alreadyRegistered.rawValue {
                    logger.error("Font registration failed for \(url.lastPathComponent, privacy: .public): \(resolved.localizedDescription, privacy: .public)")
                }
            }
            if let descriptors = CTFontManagerCreateFontDescriptorsFromURL(url as CFURL) as? [CTFontDescriptor] {
                for descriptor in descriptors {
                    if let name = CTFontDescriptorCopyAttribute(descriptor, kCTFontNameAttribute) as? String {
                        availablePostScriptNames.insert(name)
                    }
                }
            }
        }
        logger.info("Available app fonts: \(self.availablePostScriptNames.sorted().joined(separator: ", "), privacy: .public)")
    }
}

extension Font {
    static func app(_ size: CGFloat, _ face: AppFontFace = .regular, relativeTo style: Font.TextStyle = .body) -> Font {
        face.font(size: size, relativeTo: style)
    }

    static func appMono(_ size: CGFloat, medium: Bool = false, relativeTo style: Font.TextStyle = .body) -> Font {
        (medium ? AppFontFace.monoMedium : .mono).font(size: size, relativeTo: style)
    }
}

// MARK: - Palette
//
// Near-black surfaces and one accent: the lime of the logo.

extension Color {
    /// Build a Color from a 0xRRGGBB literal. Single definition for the whole app.
    init(hex: UInt32) {
        let r = Double((hex >> 16) & 0xFF) / 255.0
        let g = Double((hex >> 8) & 0xFF) / 255.0
        let b = Double(hex & 0xFF) / 255.0
        self.init(red: r, green: g, blue: b)
    }
}

enum Theme {
    static let canvas = Color(hex: 0x0A0A0B)
    static let surface = Color(hex: 0x121214)
    static let surfaceRaised = Color(hex: 0x1A1A1D)
    static let border = Color.white.opacity(0.08)
    static let borderStrong = Color.white.opacity(0.14)

    static let text = Color(hex: 0xF4F4F5)
    static let textMuted = Color(hex: 0xA1A1AA)
    static let textFaint = Color(hex: 0x71717A)

    static let accent = Color(hex: 0xE8FF47)
    static let onAccent = Color(hex: 0x0C0D04)
    static let warning = Color(hex: 0xFBBF24)
    static let danger = Color(hex: 0xF87171)

    /// Accent → warning → danger for a 1–4 signal-bar count.
    static func quality(bars: Int) -> Color {
        switch bars {
        case 4: return accent
        case 3: return Color(hex: 0xC9E86A)
        case 2: return warning
        default: return danger
        }
    }
}

// MARK: - Surfaces

/// The app background: canvas with a faint glow of the logo's lime.
struct AppBackground: View {
    var body: some View {
        ZStack {
            Theme.canvas
            RadialGradient(
                colors: [Theme.accent.opacity(0.07), .clear],
                center: .init(x: 0.5, y: -0.05),
                startRadius: 0,
                endRadius: 520
            )
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
    }
}

struct CardModifier: ViewModifier {
    var padding: CGFloat = 20
    var tint: Color?

    func body(content: Content) -> some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .fill(tint.map { $0.opacity(0.08) } ?? Theme.surface)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .strokeBorder(tint.map { $0.opacity(0.28) } ?? Theme.border, lineWidth: 1)
            )
    }
}

extension View {
    func card(padding: CGFloat = 20, tint: Color? = nil) -> some View {
        modifier(CardModifier(padding: padding, tint: tint))
    }
}

/// Small uppercase label above a group.
struct SectionHeader: View {
    let title: String
    var trailing: String? = nil

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title.uppercased())
                .font(.app(11.5, .medium, relativeTo: .caption))
                .tracking(1.1)
                .foregroundStyle(Theme.textFaint)
            Spacer(minLength: 8)
            if let trailing {
                Text(trailing)
                    .font(.app(12, relativeTo: .caption))
                    .foregroundStyle(Theme.textFaint)
            }
        }
        .accessibilityAddTraits(.isHeader)
    }
}

/// The logo, clipped to its own rounded square (the artwork sits on a matte).
struct LogoMark: View {
    var size: CGFloat = 64

    var body: some View {
        Image("LogoImage")
            .resizable()
            .interpolation(.high)
            .aspectRatio(contentMode: .fit)
            .frame(width: size, height: size)
            .clipShape(RoundedRectangle(cornerRadius: size * 0.2237, style: .continuous))
            .shadow(color: Theme.accent.opacity(0.18), radius: size * 0.3)
            .accessibilityHidden(true)
    }
}

/// A steady status light with a soft halo.
struct StatusDot: View {
    var color: Color = Theme.accent
    var size: CGFloat = 8

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .background(Circle().fill(color.opacity(0.22)).frame(width: size * 2.2, height: size * 2.2))
    }
}

// MARK: - Buttons

struct PrimaryButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.app(16, .semibold, relativeTo: .headline))
            .frame(maxWidth: .infinity, minHeight: 52)
            .foregroundStyle(isEnabled ? Theme.onAccent : Theme.textFaint)
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(isEnabled ? Theme.accent : Theme.surfaceRaised)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(isEnabled ? Color.clear : Theme.border, lineWidth: 1)
            )
            .opacity(configuration.isPressed ? 0.82 : 1)
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
            .animation(.easeOut(duration: 0.15), value: isEnabled)
    }
}

struct SecondaryButtonStyle: ButtonStyle {
    var foreground: Color = Theme.text
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.app(15, .medium, relativeTo: .subheadline))
            .frame(maxWidth: .infinity, minHeight: 48)
            .foregroundStyle(foreground)
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(configuration.isPressed ? Theme.surfaceRaised.opacity(0.6) : Theme.surfaceRaised)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Theme.borderStrong, lineWidth: 1)
            )
            .opacity(isEnabled ? 1 : 0.4)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

/// Compact capsule button for toolbars and overlays.
struct PillButtonStyle: ButtonStyle {
    var foreground: Color = Theme.text
    var fill: Color = Color.white.opacity(0.08)

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.app(14, .medium, relativeTo: .subheadline))
            .foregroundStyle(foreground)
            .padding(.horizontal, 14)
            .frame(minHeight: 40)
            .background(Capsule().fill(fill))
            .opacity(configuration.isPressed ? 0.7 : 1)
            .contentShape(Capsule())
    }
}
