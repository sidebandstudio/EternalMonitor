import Foundation

enum DeviceIdentity {
    /// Store decimal text so the full UInt64 range survives UserDefaults on
    /// every supported device. Zero remains the legacy unknown-device value.
    static func load(defaults: UserDefaults = .standard) -> UInt64 {
        if let value = defaults.string(forKey: "deviceId"), let id = UInt64(value), id != 0 { return id }
        let id = UInt64.random(in: 1...UInt64.max)
        defaults.set(String(id), forKey: "deviceId")
        return id
    }
}
