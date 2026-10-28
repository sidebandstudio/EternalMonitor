import CryptoKit
import Foundation

/// Behaviour the update manifest may change without a release: when this
/// iPad sees the TestFlight prompt, and how long it waits for the first
/// picture. Never anything that stops a working app. Anything missing,
/// malformed, from a newer schema or out of range means the built-in
/// defaults, said in one support log line per launch. The schema is in
/// docs/remote-config.md; the PC reads the same rollout the same way.
enum RemoteConfig {
    static let schema = 1
    static let maxRolloutDays = 14
    /// Only ever longer than today's 10 s: a shorter wait would cut off a
    /// slow first connection and could stop automatic reconnects.
    static let firstFrameTimeoutRange = 10...20
    /// A saved config applies this long after the check that fetched it.
    static let maxAge: TimeInterval = 14 * 24 * 60 * 60

    /// `ipad.rollout: {"start": "2026-11-02T00:00:00Z", "days": 3}`: installs
    /// see the TestFlight banner and popup spread evenly over `days`.
    /// Settings always shows the waiting update.
    struct Rollout: Equatable {
        let start: Date
        let days: Int

        /// Nil for anything malformed or out of range: everyone at once.
        init?(start: String, days: Int) {
            guard let date = RemoteConfig.utcDate(start) else { return nil }
            self.init(start: date, days: days)
        }

        init?(start: Date, days: Int) {
            guard (0...RemoteConfig.maxRolloutDays).contains(days) else { return nil }
            self.start = start
            self.days = days
        }

        /// Whether install `id` may see the prompt for `version` at `now`.
        func shows(id: String, version: String, now: Date) -> Bool {
            let day: TimeInterval = 24 * 60 * 60
            // A start further ahead than any ramp lasts is a typo, not a plan.
            if start.timeIntervalSince(now) > Double(RemoteConfig.maxRolloutDays) * day { return true }
            let ramp = UInt64(days) * 86_400
            let wait = RemoteConfig.bucket(id: id, version: version).multipliedFullWidth(by: ramp).high
            return now >= start.addingTimeInterval(TimeInterval(wait))
        }
    }

    /// Where in the ramp an install falls: the first 8 bytes of
    /// SHA-256("{version}/{id}"), as the PC computes it.
    static func bucket(id: String, version: String) -> UInt64 {
        SHA256.hash(data: Data("\(version)/\(id)".utf8)).prefix(8).reduce(0) { $0 << 8 | UInt64($1) }
    }

    /// `YYYY-MM-DDTHH:MM:SSZ` only, as check-versions.sh accepts.
    static func utcDate(_ text: String) -> Date? {
        guard text.range(of: #"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$"#, options: .regularExpression) != nil
        else { return nil }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "UTC")
        formatter.dateFormat = "yyyy-MM-dd'T'HH:mm:ss'Z'"
        formatter.isLenient = false
        return formatter.date(from: text)
    }

    /// `ipad.config`, validated. `reason` is a log token when the whole
    /// object is unusable.
    struct Settings: Equatable {
        var firstFrameTimeoutSeconds: Int?
        var reason: String?

        static func defaults(_ reason: String) -> Settings { Settings(firstFrameTimeoutSeconds: nil, reason: reason) }
    }

    /// The iPad's config in a fetched manifest.
    static func parse(_ data: Data) -> Settings {
        guard let manifest = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              manifest["schema"] as? Int == 1, let ipad = manifest["ipad"] as? [String: Any]
        else { return .defaults("missing") }
        return settings(ipad["config"])
    }

    static func settings(_ value: Any?) -> Settings {
        guard let value, !(value is NSNull) else { return .defaults("missing") }
        guard let config = value as? [String: Any], let schema = integer(config["schema"]) else { return .defaults("invalid") }
        if schema > Self.schema { return .defaults("schema") }
        if schema != Self.schema { return .defaults("invalid") }
        let timeout = integer(config["first_frame_timeout_s"]).flatMap { firstFrameTimeoutRange.contains($0) ? $0 : nil }
        return Settings(firstFrameTimeoutSeconds: timeout, reason: nil)
    }

    /// A JSON whole number; not a Bool, a fraction or a string.
    private static func integer(_ value: Any?) -> Int? {
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID(),
              !CFNumberIsFloatType(number) else { return nil }
        return number.intValue
    }

    enum Key {
        static let fetchedAt = "remoteConfigFetchedAt"
        static let firstFrameTimeout = "remoteConfigFirstFrameTimeout"
        static let reason = "remoteConfigReason"
    }

    static func save(_ settings: Settings, to defaults: UserDefaults, now: Date) {
        defaults.set(now, forKey: Key.fetchedAt)
        defaults.set(settings.firstFrameTimeoutSeconds, forKey: Key.firstFrameTimeout)
        defaults.set(settings.reason, forKey: Key.reason)
    }

    /// What a launch at `now` applies from the last saved check.
    static func read(from defaults: UserDefaults, now: Date) -> Settings {
        guard let fetched = defaults.object(forKey: Key.fetchedAt) as? Date else { return .defaults("missing") }
        if fetched > now || now.timeIntervalSince(fetched) > maxAge { return .defaults("stale") }
        if let reason = defaults.string(forKey: Key.reason) { return .defaults(reason) }
        let timeout = defaults.object(forKey: Key.firstFrameTimeout) as? Int
        return Settings(firstFrameTimeoutSeconds: timeout.flatMap { firstFrameTimeoutRange.contains($0) ? $0 : nil }, reason: nil)
    }

    /// This launch's values, read once.
    static let launch = read(from: .standard, now: Date())

    /// The one support log line. Diagnostics count `remote_config=`.
    static func summary(_ settings: Settings, rollout: Rollout?) -> String {
        let staged = rollout.map { "\($0.days)d" } ?? "none"
        if let reason = settings.reason { return "remote_config=defaults reason=\(reason) rollout=\(staged)" }
        let timeout = settings.firstFrameTimeoutSeconds.map(String.init) ?? "default"
        return "remote_config=applied rollout=\(staged) first_frame_s=\(timeout)"
    }
}
