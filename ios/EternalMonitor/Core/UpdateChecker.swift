import Foundation

/// The iPad half of https://eternalmonitor.dev/latest.json. The website bumps it
/// only after Apple approves the TestFlight build, so it never names a build
/// testers cannot install yet. No Apple credentials are involved.
/// Unknown keys are ignored, so a newer manifest still parses here.
struct UpdateManifest: Decodable, Equatable {
    struct IPad: Decodable, Equatable {
        let version: String
        let build: Int
        let testflight: String?
        /// False softens the popup's wording. Missing means recommended.
        var recommended: Bool? = nil
        /// One short sentence the popup shows under its message.
        var note: String? = nil
        /// Spreads the banner and popup for this build over a few days.
        /// Missing means everyone at once.
        var rollout: RemoteConfig.Rollout? = nil

        var isRecommended: Bool { recommended ?? true }
    }

    let schema: Int
    let ipad: IPad?

    static let maxNoteLength = 200

    /// The iPad entry, or nil for anything malformed or from a newer schema.
    static func parse(_ data: Data) -> IPad? {
        guard let manifest = try? JSONDecoder().decode(UpdateManifest.self, from: data),
              manifest.schema == 1,
              var ipad = manifest.ipad,
              AppVersion(ipad.version) != nil,
              ipad.build > 0
        else { return nil }
        ipad.note = shortNote(ipad.note)
        return ipad
    }

    /// A note the popup can show: one trimmed line, not empty and not too
    /// long, without control, format (U+200B and the like) or line and
    /// paragraph separator characters, as the PC and check-versions.sh
    /// refuse them. Anything else is dropped rather than failing the check.
    static func shortNote(_ note: String?) -> String? {
        guard let note = note?.trimmingCharacters(in: .whitespacesAndNewlines), !note.isEmpty,
              note.count <= maxNoteLength,
              !note.unicodeScalars.contains(where: { scalar in
                  switch scalar.properties.generalCategory {
                  case .control, .format, .lineSeparator, .paragraphSeparator: return true
                  default: return false
                  }
              })
        else { return nil }
        return note
    }
}

extension UpdateManifest.IPad {
    private enum CodingKeys: String, CodingKey {
        case version, build, testflight, recommended, note, rollout
    }

    private struct RawRollout: Decodable {
        let start: String
        let days: Int
    }

    /// The optional fields are read leniently: a wrong type must not hide the update.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(String.self, forKey: .version)
        build = try container.decode(Int.self, forKey: .build)
        testflight = try container.decodeIfPresent(String.self, forKey: .testflight)
        recommended = (try? container.decodeIfPresent(Bool.self, forKey: .recommended)) ?? nil
        note = (try? container.decodeIfPresent(String.self, forKey: .note)) ?? nil
        let raw = (try? container.decodeIfPresent(RawRollout.self, forKey: .rollout)) ?? nil
        rollout = raw.flatMap { RemoteConfig.Rollout(start: $0.start, days: $0.days) }
    }
}

/// A plain `major.minor.patch` release version. Previews and other forms are rejected.
struct AppVersion: Comparable {
    let parts: [Int]

    init?(_ string: String) {
        let pieces = string.split(separator: ".", omittingEmptySubsequences: false)
        guard pieces.count == 3 else { return nil }
        var parts: [Int] = []
        for piece in pieces {
            guard !piece.isEmpty, piece.allSatisfy(\.isASCII), piece.allSatisfy(\.isNumber),
                  let value = Int(piece)
            else { return nil }
            parts.append(value)
        }
        self.parts = parts
    }

    static func < (lhs: AppVersion, rhs: AppVersion) -> Bool {
        lhs.parts.lexicographicallyPrecedes(rhs.parts)
    }
}

struct InstalledApp: Equatable {
    let version: String
    let build: Int

    static var current: InstalledApp {
        let info = Bundle.main.infoDictionary
        return InstalledApp(
            version: info?["CFBundleShortVersionString"] as? String ?? "0.0.0",
            build: Int(info?["CFBundleVersion"] as? String ?? "") ?? 0
        )
    }

    /// True when TestFlight has a newer build than this one.
    func isOlder(than available: UpdateManifest.IPad) -> Bool {
        guard let newest = AppVersion(available.version), let mine = AppVersion(version) else { return false }
        if newest != mine { return mine < newest }
        return build < available.build
    }
}

/// What the Connect screen banner shows. Dismissing hides it for one build
/// for a day; Settings keeps showing the update.
struct UpdateBannerModel: Equatable {
    static let dismissInterval: TimeInterval = 24 * 60 * 60

    let version: String
    let build: Int

    var title: String { "A new version is ready in TestFlight" }
    var detail: String {
        "Version \(version), build \(build). In TestFlight, turn on Automatic Updates to get new versions without asking."
    }

    static func make(
        available: UpdateManifest.IPad?,
        installed: InstalledApp,
        automatic: Bool,
        dismissed: (build: Int, at: Date)?,
        now: Date
    ) -> UpdateBannerModel? {
        guard automatic, let available, installed.isOlder(than: available) else { return nil }
        if let dismissed, dismissed.build == available.build,
           now.timeIntervalSince(dismissed.at) < dismissInterval, dismissed.at <= now {
            return nil
        }
        return UpdateBannerModel(version: available.version, build: available.build)
    }
}

/// The Connect screen's popup: once per build, then at most every three days
/// after Later or Open TestFlight. The banner stays either way. A close time
/// ahead of the clock (it was set back) counts as now, so the next popup is
/// three days from this clock, neither at once nor never.
struct UpdatePopupModel: Equatable {
    static let closeInterval: TimeInterval = 3 * 24 * 60 * 60

    let version: String
    let build: Int
    let recommended: Bool
    let note: String?

    var title: String { "A new version of EternalMonitor is ready" }
    var message: String { recommended ? "Updating is highly recommended." : "An update is available." }

    static func make(
        available: UpdateManifest.IPad?,
        installed: InstalledApp,
        automatic: Bool,
        closed: (build: Int, at: Date)?,
        now: Date
    ) -> UpdatePopupModel? {
        guard automatic, let available, installed.isOlder(than: available) else { return nil }
        if let closed, closed.build == available.build,
           now.timeIntervalSince(min(closed.at, now)) < closeInterval {
            return nil
        }
        return UpdatePopupModel(
            version: available.version,
            build: available.build,
            recommended: available.isRecommended,
            note: available.note
        )
    }
}

/// Checks the manifest on launch and when the app returns to the foreground,
/// at most every few hours, and stays quiet offline. Debug builds only check
/// a manifest injected with `EM_UPDATE_MANIFEST`, so tests never see a banner
/// they did not ask for.
@MainActor
final class UpdateChecker: ObservableObject {
    static let shared = UpdateChecker()
    nonisolated static let manifestURL = URL(string: "https://eternalmonitor.dev/latest.json")!
    nonisolated static let joinURL = URL(string: "https://testflight.apple.com/join/ppc9QfXc")!
    static let checkInterval: TimeInterval = 4 * 60 * 60

    private enum Key {
        static let automatic = "checkForUpdates"
        static let checkedAt = "updateCheckedAt"
        static let version = "updateAvailableVersion"
        static let build = "updateAvailableBuild"
        static let testflight = "updateAvailableTestFlight"
        static let dismissedBuild = "updateDismissedBuild"
        static let dismissedAt = "updateDismissedAt"
        static let recommended = "updateAvailableRecommended"
        static let note = "updateAvailableNote"
        static let popupBuild = "updatePopupClosedBuild"
        static let popupAt = "updatePopupClosedAt"
        static let rolloutStart = "updateAvailableRolloutStart"
        static let rolloutDays = "updateAvailableRolloutDays"
        /// The staged build whose prompt this iPad has reached.
        static let rolloutReached = "updateRolloutReachedBuild"
        static let saved = [version, build, testflight, recommended, note, rolloutStart, rolloutDays]
    }

    enum Source {
        case network(URL)
        case injected(Data)
        case disabled
    }

    @Published private(set) var available: UpdateManifest.IPad?
    @Published private(set) var dismissed: (build: Int, at: Date)?
    @Published private(set) var popupClosed: (build: Int, at: Date)?
    @Published var automatic: Bool {
        didSet {
            defaults.set(automatic, forKey: Key.automatic)
            if automatic { checkIfDue() }
        }
    }

    let installed: InstalledApp
    private let defaults: UserDefaults
    private let source: Source
    private var checking = false
    /// Places this iPad in staged rollouts. Never sent anywhere by them.
    private let installationID: () -> String

    init(defaults: UserDefaults = .standard, source: Source = UpdateChecker.defaultSource, installed: InstalledApp = .current,
         now: Date = Date(), installationID: @escaping () -> String = { DiagnosticsUpload.shared.installationID }) {
        self.defaults = defaults
        self.source = source
        self.installed = installed
        self.installationID = installationID
        automatic = defaults.object(forKey: Key.automatic) as? Bool ?? true
        if let version = defaults.string(forKey: Key.version) {
            let found = UpdateManifest.IPad(
                version: version,
                build: defaults.integer(forKey: Key.build),
                testflight: defaults.string(forKey: Key.testflight),
                recommended: defaults.object(forKey: Key.recommended) as? Bool,
                note: defaults.string(forKey: Key.note),
                rollout: (defaults.object(forKey: Key.rolloutStart) as? Date).flatMap { start in
                    RemoteConfig.Rollout(start: start, days: defaults.integer(forKey: Key.rolloutDays))
                }
            )
            available = installed.isOlder(than: found) ? found : nil
        }
        let dismissedBuild = defaults.integer(forKey: Key.dismissedBuild)
        if dismissedBuild > 0, let at = defaults.object(forKey: Key.dismissedAt) as? Date {
            dismissed = (dismissedBuild, at)
        }
        let popupBuild = defaults.integer(forKey: Key.popupBuild)
        if popupBuild > 0, let at = defaults.object(forKey: Key.popupAt) as? Date {
            popupClosed = (popupBuild, at)
        }
        settlePopupClock(now: now)
    }

    /// Rewrites a popup close time from a clock that was ahead as now, so it
    /// can't hold the popup back for longer than three days.
    private func settlePopupClock(now: Date) {
        guard let closed = popupClosed, closed.at > now else { return }
        popupClosed = (closed.build, now)
        defaults.set(now, forKey: Key.popupAt)
    }

    nonisolated static var defaultSource: Source {
        #if DEBUG
        if let text = ProcessInfo.processInfo.environment["EM_UPDATE_MANIFEST"] {
            return .injected(Data(text.utf8))
        }
        return .disabled
        #else
        return .network(manifestURL)
        #endif
    }

    /// The update the banner and popup may offer at `now`: nil while a
    /// staged rollout hasn't reached this iPad.
    /// Once this iPad's turn comes for a build, its prompt keeps showing,
    /// whatever the clock does later.
    func prompted(now: Date) -> UpdateManifest.IPad? {
        guard let available, let rollout = available.rollout else { return available }
        if defaults.integer(forKey: Key.rolloutReached) == available.build { return available }
        guard rollout.shows(id: installationID(), version: available.version, now: now) else { return nil }
        defaults.set(available.build, forKey: Key.rolloutReached)
        return available
    }

    var banner: UpdateBannerModel? {
        let now = Date()
        return UpdateBannerModel.make(available: prompted(now: now), installed: installed, automatic: automatic,
                                      dismissed: dismissed, now: now)
    }

    var popup: UpdatePopupModel? {
        popup(now: Date())
    }

    func popup(now: Date) -> UpdatePopupModel? {
        UpdatePopupModel.make(available: prompted(now: now), installed: installed, automatic: automatic,
                              closed: popupClosed, now: now)
    }

    func closePopup(now: Date = Date()) {
        guard let available else { return }
        popupClosed = (available.build, now)
        defaults.set(available.build, forKey: Key.popupBuild)
        defaults.set(now, forKey: Key.popupAt)
    }

    /// The TestFlight invite named by the manifest, or the public link.
    var joinURL: URL {
        if let link = available?.testflight, let url = URL(string: link),
           url.scheme == "https", url.host == "testflight.apple.com" {
            return url
        }
        return Self.joinURL
    }

    func dismissBanner(now: Date = Date()) {
        guard let available else { return }
        dismissed = (available.build, now)
        defaults.set(available.build, forKey: Key.dismissedBuild)
        defaults.set(now, forKey: Key.dismissedAt)
    }

    func checkIfDue(now: Date = Date()) {
        settlePopupClock(now: now)
        guard automatic, !checking else { return }
        switch source {
        case .disabled:
            return
        case .injected(let data):
            apply(data, now: now)
        case .network(let url):
            if let last = defaults.object(forKey: Key.checkedAt) as? Date,
               now.timeIntervalSince(last) < Self.checkInterval, last <= now {
                return
            }
            checking = true
            Task {
                let data = await Self.fetch(url)
                checking = false
                // Offline: keep what we knew and try again next time.
                guard let data else { return }
                apply(data, now: now)
            }
        }
    }

    private func apply(_ data: Data, now: Date) {
        defaults.set(now, forKey: Key.checkedAt)
        // The first-picture wait applies from the next launch.
        RemoteConfig.save(RemoteConfig.parse(data), to: defaults, now: now)
        guard let found = UpdateManifest.parse(data), installed.isOlder(than: found) else {
            available = nil
            for key in Key.saved { defaults.removeObject(forKey: key) }
            return
        }
        available = found
        defaults.set(found.version, forKey: Key.version)
        defaults.set(found.build, forKey: Key.build)
        defaults.set(found.testflight, forKey: Key.testflight)
        defaults.set(found.recommended, forKey: Key.recommended)
        defaults.set(found.note, forKey: Key.note)
        defaults.set(found.rollout?.start, forKey: Key.rolloutStart)
        defaults.set(found.rollout?.days, forKey: Key.rolloutDays)
    }

    /// A plain GET: no cookies, no cache, no identifiers.
    private nonisolated static func fetch(_ url: URL) async -> Data? {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 10
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.httpAdditionalHeaders = ["User-Agent": "EternalMonitor"]
        let session = URLSession(configuration: configuration)
        defer { session.finishTasksAndInvalidate() }
        guard let (data, response) = try? await session.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200,
              data.count <= 256 * 1024
        else { return nil }
        return data
    }
}
