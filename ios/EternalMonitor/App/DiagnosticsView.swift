import SwiftUI

private struct DiagnosticsExport: Identifiable {
    let id = UUID()
    let url: URL
}

private struct DiagnosticsShareSheet: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [url], applicationActivities: nil)
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

struct DiagnosticsView: View {
    @State private var exported: DiagnosticsExport?
    @State private var preparing = false
    @State private var error: String?
    @State private var copied = false
    @State private var supportIDCopied = false
    private let supportID = DiagnosticsUpload.shared.installationID
    @AppStorage(DiagnosticsUpload.enabledKey) private var uploadEnabled = true
    @State private var uploadStatus = "Checking automatic upload"


    var body: some View {
        Form {
            Section {
                LabeledContent("Support ID") {
                    Text(supportID)
                        .font(.footnote.monospaced())
                        .textSelection(.enabled)
                }
                .accessibilityIdentifier("diagnostics.supportID")
                Button {
                    UIPasteboard.general.string = supportID
                    supportIDCopied = true
                } label: {
                    Label(supportIDCopied ? "Support ID copied" : "Copy Support ID", systemImage: "doc.on.doc")
                }
                .accessibilityIdentifier("diagnostics.copySupportID")
            } footer: {
                Text("Include this ID when you write to us, so we can find the diagnostics this iPad shared.")
            }
            .listRowBackground(Theme.surfaceRaised)

            Section {
                Toggle("Automatically share diagnostics", isOn: $uploadEnabled)
                    .accessibilityIdentifier("diagnostics.automaticUpload")
                    .onChange(of: uploadEnabled) { _, enabled in DiagnosticsUpload.shared.setEnabled(enabled) }
                Text(uploadStatus).font(.footnote)
            } footer: {
                Text("Device details, app errors and stream statistics are sent to EternalMonitor support while the app runs. Offline events retry automatically. Reports include PC names and network addresses. Screen content, audio and pairing credentials are excluded.")
            }
            .listRowBackground(Theme.surfaceRaised)

            Section {
                Text("Device and app versions, connection history, stream statistics and errors are saved automatically. Recent logs remain available after restarting the app.")
                Button {
                    preparing = true
                    error = nil
                    Task {
                        do {
                            let url = try await SupportDiagnostics.shared.export()
                            exported = DiagnosticsExport(url: url)
                        } catch { self.error = error.localizedDescription }
                        preparing = false
                    }
                } label: {
                    Label(preparing ? "Preparing diagnostics" : "Export diagnostics", systemImage: "square.and.arrow.up")
                }
                .disabled(preparing)
                .accessibilityIdentifier("diagnostics.export")
                Button {
                    Task {
                        UIPasteboard.general.string = await SupportDiagnostics.shared.report()
                        copied = true
                    }
                } label: {
                    Label(copied ? "Diagnostics copied" : "Copy diagnostics", systemImage: "doc.on.doc")
                }
                .accessibilityIdentifier("diagnostics.copy")
                if let error { Text("Could not export diagnostics: \(error)") }
            } footer: {
                Text("Reports include local network addresses and PC names. Pairing codes and tokens are removed.")
            }
            .listRowBackground(Theme.surfaceRaised)
        }
        .scrollContentBackground(.hidden)
        .background(Theme.canvas.ignoresSafeArea())
        .task {
            while !Task.isCancelled {
                uploadStatus = await DiagnosticsUpload.shared.status()
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
        .navigationTitle("Diagnostics")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $exported) { item in DiagnosticsShareSheet(url: item.url) }
    }
}
