import SwiftUI

struct PairingSheet: View {
    @ObservedObject var model: PairingSheetModel
    let submit: () -> Void
    let cancel: () -> Void
    @FocusState private var focused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var appeared = false
    // Counts wrong codes; each one shakes the field once.
    @State private var shakes: CGFloat = 0

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 22) {
                    Image(systemName: "lock.shield")
                        .font(.system(size: 30, weight: .medium))
                        .foregroundStyle(Theme.accent)
                        .frame(width: 64, height: 64)
                        .background(
                            RoundedRectangle(cornerRadius: 18, style: .continuous)
                                .fill(Theme.accent.opacity(0.1))
                        )
                        .scaleEffect(appeared || reduceMotion ? 1 : 0.8)
                        .opacity(appeared ? 1 : 0)
                        .animation(reduceMotion ? nil : Motion.gentle, value: appeared)
                        .accessibilityHidden(true)

                    VStack(spacing: 8) {
                        Text("Pair with your PC")
                            .font(.app(24, .semibold, relativeTo: .title2))
                            .foregroundStyle(Theme.text)
                        Text("Enter the 6-digit code shown in EternalMonitor on your PC. You only need to do this once.")
                            .font(.app(15, relativeTo: .body))
                            .foregroundStyle(Theme.textMuted)
                            .multilineTextAlignment(.center)
                            .fixedSize(horizontal: false, vertical: true)
                    }

                    TextField("000000", text: $model.code, prompt: Text("000000").foregroundStyle(Theme.textFaint.opacity(0.6)))
                        .keyboardType(.numberPad)
                        .textContentType(.oneTimeCode)
                        .autocorrectionDisabled()
                        .font(.appMono(34, medium: true, relativeTo: .largeTitle))
                        .tracking(10)
                        .multilineTextAlignment(.center)
                        .foregroundStyle(Theme.text)
                        .frame(maxWidth: 320, minHeight: 72)
                        .background(
                            RoundedRectangle(cornerRadius: 16, style: .continuous)
                                .fill(Theme.surface)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 16, style: .continuous)
                                .strokeBorder(model.error == nil ? (focused ? Theme.accent : Theme.borderStrong) : Theme.danger, lineWidth: 1.5)
                        )
                        .animation(Motion.fade, value: model.error == nil)
                        .animation(Motion.fade, value: focused)
                        .modifier(Shake(animatableData: shakes))
                        .focused($focused)
                        .onChange(of: model.code) { _, code in
                            // Digits only, at most six.
                            let digits = String(code.filter(\.isNumber).prefix(6))
                            if digits != code { model.code = digits }
                        }
                        .accessibilityLabel("Pairing code")
                        .accessibilityIdentifier("pairing.code")

                    if let error = model.error {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Image(systemName: "exclamationmark.circle.fill")
                                .accessibilityHidden(true)
                            Text(error)
                                .multilineTextAlignment(.leading)
                                .accessibilityIdentifier("pairing.error")
                        }
                        .font(.app(14, relativeTo: .callout))
                        .foregroundStyle(Theme.danger)
                        .frame(maxWidth: 360)
                        .transition(.opacity.combined(with: .move(edge: .top)))
                    }

                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        let remaining = model.remaining(at: context.date)
                        Button(action: submit) {
                            HStack(spacing: 10) {
                                if model.submitting {
                                    ProgressView().tint(Theme.onAccent)
                                }
                                Text(remaining > 0 ? "Try again in \(remaining) s" : "Pair iPad")
                            }
                        }
                        .buttonStyle(PrimaryButtonStyle())
                        .frame(maxWidth: 320)
                        .disabled(model.submitting || remaining > 0)
                        .accessibilityIdentifier("pairing.submit")
                    }

                    Text("Can't see a code? Open EternalMonitor on the PC and look on the Stream page, or connect with a USB cable instead.")
                        .font(.app(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textFaint)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 360)
                }
                .padding(.horizontal, 28)
                .padding(.top, 28)
                .padding(.bottom, 24)
                .frame(maxWidth: .infinity)
                .animation(Motion.fade, value: model.error)
                .onChange(of: model.error) { _, error in
                    if error != nil && !reduceMotion {
                        withAnimation(.easeInOut(duration: 0.45)) { shakes += 1 }
                    }
                }
            }
            .background(Theme.canvas.ignoresSafeArea())
            .navigationTitle("Pair with PC")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: cancel)
                        .foregroundStyle(Theme.text)
                        .accessibilityIdentifier("pairing.cancel")
                }
            }
            .tint(Theme.accent)
            .accessibilityIdentifier("pairing.sheet")
            .onAppear {
                focused = true
                appeared = true
            }
        }
        .presentationBackground(Theme.canvas)
    }
}
