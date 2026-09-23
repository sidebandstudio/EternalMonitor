import SwiftUI

struct PairingSheet: View {
    @ObservedObject var model: PairingSheetModel
    let submit: () -> Void
    let cancel: () -> Void
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("6-digit code", text: $model.code)
                        .keyboardType(.numberPad)
                        .textContentType(.oneTimeCode)
                        .autocorrectionDisabled()
                        .font(.system(size: 28, weight: .medium, design: .monospaced))
                        .focused($focused)
                        .accessibilityLabel("Pairing code")
                        .accessibilityIdentifier("pairing.code")
                    if let error = model.error {
                        Text(error).foregroundStyle(Theme.fault)
                            .accessibilityIdentifier("pairing.error")
                    }
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        let remaining = model.remaining(at: context.date)
                        Button(action: submit) {
                            HStack {
                                Text(remaining > 0 ? "Try again in \(remaining) s" : "Pair iPad")
                                if model.submitting { ProgressView() }
                            }
                            .frame(minHeight: 44)
                        }
                        .disabled(model.submitting || remaining > 0)
                        .accessibilityIdentifier("pairing.submit")
                    }
                } header: {
                    Text("Enter the 6-digit code shown on the PC")
                }
            }
            .navigationTitle("Pair with PC")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: cancel).accessibilityIdentifier("pairing.cancel")
                }
            }
            .tint(Theme.amber)
            .accessibilityIdentifier("pairing.sheet")
            .onAppear { focused = true }
        }
    }
}
