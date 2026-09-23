// AVFoundation QR scanner sheet. Presented from ConnectView when
// the user taps "Scan QR". Returns the decoded string via `onScan`; closing the sheet
// without scanning returns nothing.

import AVFoundation
import SwiftUI
import UIKit

struct QRScannerView: UIViewControllerRepresentable {
    let onScan: (String) -> Void
    let onCancel: () -> Void

    func makeUIViewController(context: Context) -> QRScannerViewController {
        let controller = QRScannerViewController()
        controller.onScan = onScan
        controller.onCancel = onCancel
        return controller
    }

    func updateUIViewController(_ uiViewController: QRScannerViewController, context: Context) {}
}

final class QRScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onScan: ((String) -> Void)?
    var onCancel: (() -> Void)?

    private let captureSession = AVCaptureSession()
    private let overlay = ScannerOverlayView()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    private var didFinish = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black

        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            configureSession()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    if granted {
                        self?.configureSession()
                    } else {
                        self?.showCameraDeniedAlert()
                    }
                }
            }
        case .denied, .restricted:
            showCameraDeniedAlert()
        @unknown default:
            showCameraDeniedAlert()
        }

        view.addSubview(overlay)
        overlay.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            overlay.topAnchor.constraint(equalTo: view.topAnchor),
            overlay.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            overlay.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            overlay.trailingAnchor.constraint(equalTo: view.trailingAnchor),
        ])

        let hint = UILabel()
        hint.text = "Point at the QR code in EternalMonitor on your PC"
        hint.font = UIFont(name: "Geist-Medium", size: 17) ?? .systemFont(ofSize: 17, weight: .medium)
        hint.textColor = .white
        hint.textAlignment = .center
        hint.numberOfLines = 0
        hint.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(hint)
        NSLayoutConstraint.activate([
            hint.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            hint.bottomAnchor.constraint(equalTo: overlay.windowLayoutGuide.topAnchor, constant: -28),
            hint.widthAnchor.constraint(lessThanOrEqualTo: view.widthAnchor, constant: -64),
        ])

        var cancel = UIButton.Configuration.filled()
        cancel.title = "Cancel"
        cancel.baseBackgroundColor = UIColor.white.withAlphaComponent(0.16)
        cancel.baseForegroundColor = .white
        cancel.cornerStyle = .capsule
        cancel.contentInsets = NSDirectionalEdgeInsets(top: 10, leading: 22, bottom: 10, trailing: 22)
        cancel.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { attributes in
            var attributes = attributes
            attributes.font = UIFont(name: "Geist-SemiBold", size: 16) ?? .systemFont(ofSize: 16, weight: .semibold)
            return attributes
        }
        let cancelButton = UIButton(configuration: cancel)
        cancelButton.addTarget(self, action: #selector(handleCancel), for: .touchUpInside)
        cancelButton.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(cancelButton)
        NSLayoutConstraint.activate([
            cancelButton.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            cancelButton.topAnchor.constraint(equalTo: overlay.windowLayoutGuide.bottomAnchor, constant: 32),
        ])
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        if previewLayer != nil && !captureSession.isRunning {
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                self?.captureSession.startRunning()
            }
        }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if captureSession.isRunning {
            captureSession.stopRunning()
        }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.bounds
    }

    private func configureSession() {
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device) else {
            showCameraDeniedAlert()
            return
        }
        if captureSession.canAddInput(input) {
            captureSession.addInput(input)
        }
        let output = AVCaptureMetadataOutput()
        if captureSession.canAddOutput(output) {
            captureSession.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: DispatchQueue.main)
            output.metadataObjectTypes = [.qr]
        }
        let preview = AVCaptureVideoPreviewLayer(session: captureSession)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.bounds
        view.layer.insertSublayer(preview, at: 0)
        previewLayer = preview
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            self?.captureSession.startRunning()
        }
    }

    private func showCameraDeniedAlert() {
        let alert = UIAlertController(
            title: "Camera unavailable",
            message: "Enable camera access in Settings to scan QR codes.",
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(title: "OK", style: .default) { [weak self] _ in
            self?.handleCancel()
        })
        present(alert, animated: true)
    }

    @objc private func handleCancel() {
        guard !didFinish else { return }
        didFinish = true
        onCancel?()
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !didFinish else { return }
        guard let metadata = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
              metadata.type == .qr,
              let value = metadata.stringValue else { return }
        didFinish = true
        captureSession.stopRunning()
        onScan?(value)
    }
}

/// Dims everything except a rounded window and marks its corners in the
/// accent color. Purely visual; touches pass through.
final class ScannerOverlayView: UIView {
    /// The clear window, for laying out the hint and buttons around it.
    let windowLayoutGuide = UILayoutGuide()
    private let dim = CAShapeLayer()
    private let corners = CAShapeLayer()

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        dim.fillRule = .evenOdd
        dim.fillColor = UIColor.black.withAlphaComponent(0.55).cgColor
        layer.addSublayer(dim)
        corners.strokeColor = UIColor(red: 0.91, green: 1.0, blue: 0.28, alpha: 1).cgColor
        corners.fillColor = UIColor.clear.cgColor
        corners.lineWidth = 4
        corners.lineCap = .round
        corners.lineJoin = .round
        layer.addSublayer(corners)
        addLayoutGuide(windowLayoutGuide)
        let side = windowLayoutGuide.widthAnchor.constraint(equalToConstant: 300)
        side.priority = .defaultHigh
        NSLayoutConstraint.activate([
            windowLayoutGuide.centerXAnchor.constraint(equalTo: centerXAnchor),
            windowLayoutGuide.centerYAnchor.constraint(equalTo: centerYAnchor),
            windowLayoutGuide.heightAnchor.constraint(equalTo: windowLayoutGuide.widthAnchor),
            windowLayoutGuide.widthAnchor.constraint(lessThanOrEqualTo: widthAnchor, multiplier: 0.7),
            side,
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    override func layoutSubviews() {
        super.layoutSubviews()
        let window = windowLayoutGuide.layoutFrame
        let radius: CGFloat = 28
        let dimPath = UIBezierPath(rect: bounds)
        dimPath.append(UIBezierPath(roundedRect: window, cornerRadius: radius))
        dim.frame = bounds
        dim.path = dimPath.cgPath

        let arm: CGFloat = 44
        let path = UIBezierPath()
        let (l, r, t, b) = (window.minX, window.maxX, window.minY, window.maxY)
        // top-left
        path.move(to: CGPoint(x: l, y: t + arm))
        path.addLine(to: CGPoint(x: l, y: t + radius))
        path.addArc(withCenter: CGPoint(x: l + radius, y: t + radius), radius: radius, startAngle: .pi, endAngle: 1.5 * .pi, clockwise: true)
        path.addLine(to: CGPoint(x: l + arm, y: t))
        // top-right
        path.move(to: CGPoint(x: r - arm, y: t))
        path.addLine(to: CGPoint(x: r - radius, y: t))
        path.addArc(withCenter: CGPoint(x: r - radius, y: t + radius), radius: radius, startAngle: 1.5 * .pi, endAngle: 0, clockwise: true)
        path.addLine(to: CGPoint(x: r, y: t + arm))
        // bottom-right
        path.move(to: CGPoint(x: r, y: b - arm))
        path.addLine(to: CGPoint(x: r, y: b - radius))
        path.addArc(withCenter: CGPoint(x: r - radius, y: b - radius), radius: radius, startAngle: 0, endAngle: 0.5 * .pi, clockwise: true)
        path.addLine(to: CGPoint(x: r - arm, y: b))
        // bottom-left
        path.move(to: CGPoint(x: l + arm, y: b))
        path.addLine(to: CGPoint(x: l + radius, y: b))
        path.addArc(withCenter: CGPoint(x: l + radius, y: b - radius), radius: radius, startAngle: 0.5 * .pi, endAngle: .pi, clockwise: true)
        path.addLine(to: CGPoint(x: l, y: b - arm))
        corners.frame = bounds
        corners.path = path.cgPath
    }
}
