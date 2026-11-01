import SwiftUI

// MARK: - Language
//
// The app's own language, chosen in setup or Settings, or the device's when
// it is one of these. Strings come from that language's .lproj in
// Localizable.xcstrings, so a change redraws every screen at once, without
// a relaunch. Only the basics are translated (titles, buttons, setting names
// and values, short status words, setup); errors, troubleshooting, support
// and update text stay English and are not in the catalog.

enum AppLanguage: String, CaseIterable, Identifiable {
    case en, es, ar
    case zhHans = "zh-Hans"
    case hi, ru, ja
    case ptBR = "pt-BR"
    case fr, de, ko

    var id: String { rawValue }

    /// The name in the picker, in the language itself (i18n/glossary.md).
    var nativeName: String {
        switch self {
        case .en: "English"
        case .es: "Español"
        case .ar: "العربية"
        case .zhHans: "简体中文"
        case .hi: "हिन्दी"
        case .ru: "Русский"
        case .ja: "日本語"
        case .ptBR: "Português (Brasil)"
        case .fr: "Français"
        case .de: "Deutsch"
        case .ko: "한국어"
        }
    }

    var isRightToLeft: Bool { self == .ar }

    /// The first of `preferences` (device languages, most preferred first)
    /// the app has; English when none is. Any Portuguese reads pt-BR;
    /// Chinese only in Simplified script.
    static func matching(_ preferences: [String]) -> AppLanguage {
        for preference in preferences {
            let language = Locale.Language(identifier: preference)
            switch language.languageCode?.identifier {
            case "zh":
                let script = Locale.Language(identifier: language.maximalIdentifier).script
                if script == .hanSimplified { return .zhHans }
            case "pt":
                return .ptBR
            case let code?:
                if let match = AppLanguage(rawValue: code) { return match }
            case nil:
                break
            }
        }
        return .en
    }
}

/// The chosen language. `choice` nil follows the device.
final class LanguageStore: ObservableObject, @unchecked Sendable {
    static let shared = LanguageStore()
    static let defaultsKey = "appLanguage"

    @Published var choice: AppLanguage? {
        didSet {
            UserDefaults.standard.set(choice?.rawValue ?? "", forKey: Self.defaultsKey)
            resolve()
        }
    }

    /// The language on screen.
    private(set) var current: AppLanguage = .en
    /// Its .lproj, where `L` looks strings up.
    private(set) var bundle: Bundle = .main
    private(set) var locale = Locale(identifier: "en")

    var layoutDirection: LayoutDirection { current.isRightToLeft ? .rightToLeft : .leftToRight }

    init(defaults: UserDefaults = .standard) {
        // The wrapper itself: assigning `choice` here would run didSet and
        // save a value that came from a launch argument.
        _choice = Published(initialValue: AppLanguage(rawValue: defaults.string(forKey: Self.defaultsKey) ?? ""))
        resolve()
    }

    private func resolve() {
        current = choice ?? AppLanguage.matching(Locale.preferredLanguages)
        locale = Locale(identifier: current.rawValue)
        bundle = Bundle.main.path(forResource: current.rawValue, ofType: "lproj").flatMap(Bundle.init(path:)) ?? .main
    }
}

/// `key` in the app's language. Views that call this observe
/// `LanguageStore.shared`, so they redraw when it changes.
func L(_ key: String.LocalizationValue) -> String {
    let store = LanguageStore.shared
    return String(localized: key, bundle: store.bundle, locale: store.locale)
}

/// The chosen language's locale and layout direction. Sheets and covers
/// don't take the layout direction from the screen under them, so each
/// presented screen applies this too.
struct AppLanguageEnvironment: ViewModifier {
    @ObservedObject private var language = LanguageStore.shared

    func body(content: Content) -> some View {
        content
            .environment(\.locale, language.locale)
            .environment(\.layoutDirection, language.layoutDirection)
    }
}

extension View {
    func appLanguage() -> some View {
        modifier(AppLanguageEnvironment())
    }

    /// The streamed PC picture and everything that maps touches onto it
    /// stay left to right in every language: a tap on the left of the iPad
    /// clicks the left of the PC.
    func streamSurfaceDirection() -> some View {
        environment(\.layoutDirection, .leftToRight)
    }
}
