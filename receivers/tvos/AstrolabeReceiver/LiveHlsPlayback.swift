import AVFoundation
import Foundation
import UniformTypeIdentifiers

/// AVFoundation HLS playback whose bytes still flow through Astrolabe's
/// bounded, no-redirect, pinned-trust transport. A private URL scheme makes
/// AVFoundation delegate every playlist and segment request back to us.
final class LiveHlsPlayback: NSObject, AVAssetResourceLoaderDelegate {
    let sourceURL: URL
    let player: AVPlayer

    private let origin: URL
    private let transport: BoundedTransport
    private let onFailure: () -> Void
    private let asset: AVURLAsset
    private var statusObservation: NSKeyValueObservation?

    init(sourceURL: URL, origin: URL, transport: BoundedTransport, onFailure: @escaping () -> Void) {
        self.sourceURL = sourceURL
        self.origin = origin
        self.transport = transport
        self.onFailure = onFailure
        var components = URLComponents(url: sourceURL, resolvingAgainstBaseURL: false)!
        components.scheme = "astrolabe-hls"
        asset = AVURLAsset(url: components.url!)
        let item = AVPlayerItem(asset: asset)
        player = AVPlayer(playerItem: item)
        super.init()
        asset.resourceLoader.setDelegate(self, queue: DispatchQueue(label: "com.nixiesoftware.astrolabe.hls"))
        statusObservation = item.observe(\.status, options: [.new]) { [weak self] item, _ in
            if item.status == .failed { self?.onFailure() }
        }
    }

    func resourceLoader(
        _ resourceLoader: AVAssetResourceLoader,
        shouldWaitForLoadingOfRequestedResource loadingRequest: AVAssetResourceLoadingRequest
    ) -> Bool {
        guard let requested = loadingRequest.request.url,
              var components = URLComponents(url: requested, resolvingAgainstBaseURL: false),
              components.scheme == "astrolabe-hls",
              components.host?.caseInsensitiveCompare(origin.host ?? "") == .orderedSame,
              components.port == origin.port,
              components.user == nil, components.password == nil,
              components.query == nil, components.fragment == nil,
              components.path.hasPrefix("/head/v1/live/")
        else {
            loadingRequest.finishLoading(with: DisplayProtocolV1.refusal("origin_refused", "HLS resource"))
            return true
        }
        components.scheme = "https"
        guard let url = components.url else {
            loadingRequest.finishLoading(with: DisplayProtocolV1.refusal("origin_refused", "HLS URL"))
            return true
        }
        Task {
            do {
                var request = URLRequest(url: url)
                request.httpMethod = "GET"
                let maximum = url.path.hasSuffix(".ts") ? 34 * 1024 * 1024 : 65_536
                let response = try await transport.send(request, maximumBytes: maximum)
                guard response.status == 200 else {
                    throw DisplayProtocolV1.refusal("coordinator_refused", "HLS HTTP \(response.status)")
                }
                if let information = loadingRequest.contentInformationRequest {
                    let mime = response.headers.value(forHTTPHeaderField: "Content-Type")?.split(separator: ";").first.map(String.init)
                    information.contentType = mime.flatMap { UTType(mimeType: $0)?.identifier }
                    information.contentLength = Int64(response.body.count)
                    information.isByteRangeAccessSupported = false
                }
                if let dataRequest = loadingRequest.dataRequest {
                    let offset = max(0, Int(dataRequest.currentOffset))
                    guard offset <= response.body.count else { throw DisplayProtocolV1.refusal("bound_exceeded", "HLS range") }
                    let end = dataRequest.requestsAllDataToEndOfResource
                        ? response.body.count
                        : min(response.body.count, offset + dataRequest.requestedLength)
                    dataRequest.respond(with: response.body.subdata(in: offset..<end))
                }
                loadingRequest.finishLoading()
            } catch {
                loadingRequest.finishLoading(with: error)
                onFailure()
            }
        }
        return true
    }
}
