#if DEBUG
import SwiftUI

/// Debug builds only: put the UI in a state that normally needs a live PC,
/// for screenshots and design review. Never opens a connection.
///
/// `EM_UI_PREVIEW=display|quality|connecting|error|pairing|settings|recent`.
/// Launch with `-allowUSB NO` so the USB listener stays off as well.
enum UIPreview {
    static let mode = ProcessInfo.processInfo.environment["EM_UI_PREVIEW"] ?? ""
    static var showsBackdrop: Bool { mode == "display" || mode == "quality" }
    static var opensQuality: Bool { mode == "quality" }
    static var opensSettings: Bool { mode == "settings" }

    @MainActor static func apply(to manager: ConnectionManager) {
        switch mode {
        case "display", "quality":
            manager.fps = 60
            manager.transportMode = "USB"
            manager.state = .connected
        case "connecting":
            manager.state = .connecting
        case "error":
            manager.connectionError = "No response from 192.168.1.20:9876. Check that EternalMonitor is running on the PC and that both devices are on the same network."
        case "pairing":
            manager.pairingPrompt = PairingSheetModel()
        case "recent":
            RecentConnectionStore.shared.add(host: "192.168.1.20", port: 9876, isUSB: false)
            RecentConnectionStore.shared.add(host: "Studio PC", port: 9877, isUSB: true)
        default:
            break
        }
    }

    /// A stand-in Windows desktop, so the translucent controls have
    /// something behind them.
    struct Backdrop: View {
        var body: some View {
            ZStack {
                LinearGradient(
                    colors: [Color(hex: 0x1B2A4A), Color(hex: 0x3B2F5E), Color(hex: 0x0F1B33)],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
                GeometryReader { geo in
                    let w = geo.size.width, h = geo.size.height
                    window(title: "Notes.txt", width: w * 0.42, height: h * 0.46)
                        .position(x: w * 0.32, y: h * 0.38)
                    window(title: "Build output", width: w * 0.38, height: h * 0.34)
                        .position(x: w * 0.68, y: h * 0.62)
                    Rectangle()
                        .fill(Color.black.opacity(0.45))
                        .frame(height: 44)
                        .position(x: w / 2, y: h - 22)
                }
            }
            .ignoresSafeArea()
        }

        private func window(title: String, width: CGFloat, height: CGFloat) -> some View {
            VStack(spacing: 0) {
                HStack {
                    Text(title)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(.white.opacity(0.85))
                    Spacer()
                    HStack(spacing: 18) {
                        ForEach(["minus", "square", "xmark"], id: \.self) {
                            Image(systemName: $0).font(.system(size: 10)).foregroundStyle(.white.opacity(0.7))
                        }
                    }
                }
                .padding(.horizontal, 14)
                .frame(height: 34)
                .background(Color(hex: 0x202024))
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(0..<7, id: \.self) { line in
                        RoundedRectangle(cornerRadius: 3)
                            .fill(Color.white.opacity(0.12))
                            .frame(width: width * [0.8, 0.55, 0.7, 0.4, 0.75, 0.6, 0.3][line], height: 9)
                    }
                    Spacer()
                }
                .padding(18)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(hex: 0x19191C))
            }
            .frame(width: width, height: height)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .shadow(color: .black.opacity(0.4), radius: 24, y: 12)
        }
    }
}
#endif
