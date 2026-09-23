import XCTest
@testable import EternalMonitor

final class DeviceIdentityTests: XCTestCase {
    func testIdentityPersistsAndPreservesTheFullUnsignedRange() {
        let name = "eternal.identity.test.\(UUID())"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        let first = DeviceIdentity.load(defaults: defaults)
        XCTAssertNotEqual(first, 0)
        XCTAssertEqual(DeviceIdentity.load(defaults: defaults), first)
        defaults.set(String(UInt64.max), forKey: "deviceId")
        XCTAssertEqual(DeviceIdentity.load(defaults: defaults), UInt64.max)
        for invalid in ["0", "-1", "invalid"] {
            defaults.set(invalid, forKey: "deviceId")
            XCTAssertNotEqual(DeviceIdentity.load(defaults: defaults), 0)
        }
    }
}
