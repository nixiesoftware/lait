import AVFoundation
import SwiftUI

/// The scan step: the camera reads the invite QR another device shows. The
/// scanner only delivers the string — reading, confirming, and joining stay
/// in the sheet, because a scan is a delivery, never a consent.
struct ScanInviteView: View {
    let onFound: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    /// Two different nothings: access the person can grant in Settings, and a
    /// camera fact no switch will change. The copy differs because the way
    /// out differs — a silent black view names neither.
    enum Absence {
        case denied
        case unavailable(String)
    }

    @State private var absence: Absence?

    var body: some View {
        NavigationStack {
            Group {
                switch absence {
                case .denied:
                    ContentUnavailableView {
                        Label("Camera access is off", systemImage: "video.slash")
                    } description: {
                        Text("Allow camera access in Settings to scan an invite, or paste the link instead.")
                    }
                case .unavailable(let reason):
                    ContentUnavailableView {
                        Label("The camera can't scan", systemImage: "video.slash")
                    } description: {
                        Text("\(reason) Paste the link instead.")
                    }
                case nil:
                    QRCameraView(
                        onFound: { value in
                            onFound(value)
                            dismiss()
                        },
                        onUnavailable: { reason in
                            // Reported from the controller's own load, which
                            // can land mid view update; the hop defers the
                            // write to a frame of its own.
                            Task { @MainActor in absence = .unavailable(reason) }
                        }
                    )
                    .ignoresSafeArea(edges: .bottom)
                    .overlay(alignment: .bottom) {
                        Text("Point at the invite QR on your other device")
                            .font(.caption)
                            .padding(8)
                            .background(.thinMaterial, in: Capsule())
                            .padding(.bottom, 24)
                    }
                }
            }
            .navigationTitle("Scan invite")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .task {
            let granted = await AVCaptureDevice.requestAccess(for: .video)
            if !granted { absence = .denied }
        }
    }
}

private struct QRCameraView: UIViewControllerRepresentable {
    let onFound: (String) -> Void
    let onUnavailable: (String) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(onFound: onFound)
    }

    func makeUIViewController(context: Context) -> ScannerController {
        let controller = ScannerController()
        controller.delegate = context.coordinator
        controller.onUnavailable = onUnavailable
        return controller
    }

    func updateUIViewController(_ controller: ScannerController, context: Context) {}

    final class Coordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate {
        let onFound: (String) -> Void
        private var delivered = false

        init(onFound: @escaping (String) -> Void) {
            self.onFound = onFound
        }

        func metadataOutput(
            _ output: AVCaptureMetadataOutput,
            didOutput objects: [AVMetadataObject],
            from connection: AVCaptureConnection
        ) {
            guard !delivered,
                  let object = objects.first as? AVMetadataMachineReadableCodeObject,
                  object.type == .qr,
                  let value = object.stringValue
            else { return }
            delivered = true
            onFound(value)
        }
    }
}

final class ScannerController: UIViewController {
    weak var delegate: AVCaptureMetadataOutputObjectsDelegate?
    /// A camera that cannot scan is a named fact, never a silent black view.
    var onUnavailable: ((String) -> Void)?
    private let session = AVCaptureSession()
    /// Held by name: "the first sublayer" is a position, not an identity.
    private var preview: AVCaptureVideoPreviewLayer?

    override func viewDidLoad() {
        super.viewDidLoad()
        guard let device = AVCaptureDevice.default(for: .video) else {
            onUnavailable?("This device has no camera.")
            return
        }
        guard let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input)
        else {
            onUnavailable?("The camera couldn't be opened.")
            return
        }
        session.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            onUnavailable?("The camera can't read codes.")
            return
        }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(delegate, queue: .main)
        output.metadataObjectTypes = [.qr]

        let preview = AVCaptureVideoPreviewLayer(session: session)
        preview.frame = view.layer.bounds
        preview.videoGravity = .resizeAspectFill
        view.layer.addSublayer(preview)
        self.preview = preview
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.layer.bounds
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        DispatchQueue.global(qos: .userInitiated).async { [session] in
            if !session.isRunning { session.startRunning() }
        }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        DispatchQueue.global(qos: .userInitiated).async { [session] in
            if session.isRunning { session.stopRunning() }
        }
    }
}
