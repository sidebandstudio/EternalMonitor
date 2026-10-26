import MessageUI
import SwiftUI

/// One Send feedback tap: what to write and how to send it.
struct FeedbackRequest: Identifiable {
    let id = UUID()
    let kind: Feedback.Kind
    let context: Feedback.Context
    let route: Feedback.Route
    /// The Diagnostics report, attached in Mail. The user can remove it there.
    let attachment: Data?

    /// What Mail opens with. It says so whenever the report is attached.
    var mailBody: String {
        Feedback.body(kind, context, attachesDiagnostics: attachment != nil)
    }

    /// Mail with the Diagnostics report attached when Mail is set up, or the
    /// sheet to copy from when it isn't.
    @MainActor
    static func make(_ kind: Feedback.Kind, context: Feedback.Context,
                     canSendMail: Bool? = nil) async -> FeedbackRequest {
        switch Feedback.route(canSendMail: canSendMail ?? MFMailComposeViewController.canSendMail()) {
        case .mail:
            let report = await SupportDiagnostics.shared.report()
            return FeedbackRequest(kind: kind, context: context, route: .mail, attachment: Data(report.utf8))
        case .copy:
            return FeedbackRequest(kind: kind, context: context, route: .copy, attachment: nil)
        }
    }
}

extension View {
    /// Presents Mail or the copy sheet for `request`.
    func feedbackSheet(_ request: Binding<FeedbackRequest?>) -> some View {
        sheet(item: request) { request in
            switch request.route {
            case .mail:
                MailComposer(request: request).ignoresSafeArea()
            case .copy:
                FeedbackCopySheet(request: request)
            }
        }
    }
}

private struct MailComposer: UIViewControllerRepresentable {
    let request: FeedbackRequest
    @Environment(\.dismiss) private var dismiss

    func makeUIViewController(context: Context) -> MFMailComposeViewController {
        let mail = MFMailComposeViewController()
        mail.mailComposeDelegate = context.coordinator
        mail.setToRecipients([Feedback.email])
        mail.setSubject(Feedback.subject(request.kind, request.context))
        mail.setMessageBody(request.mailBody, isHTML: false)
        if let attachment = request.attachment {
            mail.addAttachmentData(attachment, mimeType: "text/plain", fileName: Feedback.attachmentName)
        }
        return mail
    }

    func updateUIViewController(_ controller: MFMailComposeViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(dismiss: { dismiss() }) }

    final class Coordinator: NSObject, MFMailComposeViewControllerDelegate {
        let dismiss: @MainActor () -> Void
        init(dismiss: @escaping @MainActor () -> Void) { self.dismiss = dismiss }

        func mailComposeController(_ controller: MFMailComposeViewController,
                                   didFinishWith result: MFMailComposeResult, error: Error?) {
            SupportDiagnostics.shared.record("info", "feedback", "Mail closed result=\(result.rawValue)")
            // Mail calls its delegate on the main thread.
            MainActor.assumeIsolated { dismiss() }
        }
    }
}

/// When Mail isn't set up: the message to copy, the address, and TestFlight.
struct FeedbackCopySheet: View {
    let request: FeedbackRequest
    @Environment(\.dismiss) private var dismiss
    @State private var copied: String?

    private var text: String { Feedback.fullText(request.kind, request.context) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Mail isn't set up on this iPad")
                            .font(.app(20, .semibold, relativeTo: .title3))
                            .foregroundStyle(Theme.text)
                            .accessibilityIdentifier("feedback.copyTitle")
                        Text("We're a small studio and we read every message. Copy this message, add what's on your mind, and send it to \(Feedback.email) from any email app.")
                            .font(.app(15, relativeTo: .callout))
                            .foregroundStyle(Theme.textMuted)
                            .fixedSize(horizontal: false, vertical: true)
                    }

                    Text(text.trimmingCharacters(in: .whitespacesAndNewlines))
                        .font(.appMono(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .card(padding: 14)
                        .accessibilityIdentifier("feedback.message")

                    HStack(spacing: 10) {
                        Button {
                            UIPasteboard.general.string = text
                            copied = "message"
                        } label: {
                            Label(copied == "message" ? "Message copied" : "Copy message", systemImage: "doc.on.doc")
                        }
                        .buttonStyle(PrimaryButtonStyle())
                        .accessibilityIdentifier("feedback.copyMessage")

                        Button {
                            UIPasteboard.general.string = Feedback.email
                            copied = "address"
                        } label: {
                            Label(copied == "address" ? "Address copied" : "Copy email address", systemImage: "envelope")
                        }
                        .buttonStyle(SecondaryButtonStyle())
                        .accessibilityIdentifier("feedback.copyAddress")
                    }

                    VStack(alignment: .leading, spacing: 8) {
                        Text("To add diagnostics, open Settings, then Diagnostics, tap Copy diagnostics and paste them into your email.")
                        Text("Testing the beta? You can also open TestFlight, tap EternalMonitor, then Send Beta Feedback.")
                    }
                    .font(.app(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textFaint)
                    .fixedSize(horizontal: false, vertical: true)
                }
                .padding(24)
            }
            .background(Theme.canvas.ignoresSafeArea())
            .navigationTitle("Send feedback")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .font(.app(16, .semibold, relativeTo: .body))
                        .foregroundStyle(Theme.accent)
                        .accessibilityIdentifier("feedback.done")
                }
            }
            .toolbarBackground(Theme.canvas, for: .navigationBar)
        }
        .presentationBackground(Theme.canvas)
    }
}

/// The one-time card on the Connect screen.
struct FeedbackAskCard: View {
    let send: () -> Void
    let notNow: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "envelope.fill")
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.accent)
                    .padding(.top, 2)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 6) {
                    Text("How's EternalMonitor working for you? :)")
                        .font(.app(16, .semibold, relativeTo: .headline))
                        .foregroundStyle(Theme.text)
                        .accessibilityIdentifier("feedback.askTitle")
                    Text("We're a small studio building EternalMonitor so students, and anyone with an iPad sitting around, can put it to work. We read every message, and your feedback shapes what we fix next.")
                        .font(.app(14, relativeTo: .callout))
                        .foregroundStyle(Theme.textMuted)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            HStack(spacing: 10) {
                Button("Send feedback", action: send)
                    .buttonStyle(PillButtonStyle(foreground: Theme.onAccent, fill: Theme.accent))
                    .accessibilityIdentifier("feedback.askSend")
                Button("Not now", action: notNow)
                    .buttonStyle(PillButtonStyle())
                    .accessibilityIdentifier("feedback.askNotNow")
            }
            .padding(.leading, 27)
        }
        .card(padding: 16, tint: Theme.accent)
        .transition(.opacity.combined(with: .move(edge: .top)))
    }
}
