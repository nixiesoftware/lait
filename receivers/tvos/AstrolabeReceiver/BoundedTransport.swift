import Foundation

struct BoundedHTTPResponse {
    let status: Int
    let body: Data
    let headers: HTTPURLResponse
}

final class NoRedirectDelegate: NSObject, URLSessionTaskDelegate {
    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

actor BoundedTransport {
    private let delegate = NoRedirectDelegate()
    private lazy var session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieStorage = nil
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForRequest = 35
        configuration.timeoutIntervalForResource = 45
        configuration.waitsForConnectivity = false
        return URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
    }()

    func send(_ request: URLRequest, maximumBytes: Int) async throws -> BoundedHTTPResponse {
        let requestedURL = request.url
        let (stream, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse, http.url == requestedURL else {
            throw DisplayProtocolV1.refusal("redirect_refused", "response URL changed")
        }
        if let declared = http.value(forHTTPHeaderField: "Content-Length"),
           let count = Int(declared), count < 0 || count > maximumBytes {
            throw DisplayProtocolV1.refusal("bound_exceeded", "response Content-Length")
        }
        var body = Data()
        body.reserveCapacity(min(maximumBytes, Int(http.expectedContentLength.clamped(to: 0...Int64(maximumBytes)))))
        for try await byte in stream {
            guard body.count < maximumBytes else {
                throw DisplayProtocolV1.refusal("bound_exceeded", "streaming response")
            }
            body.append(byte)
        }
        return BoundedHTTPResponse(status: http.statusCode, body: body, headers: http)
    }
}

private extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self { min(max(self, range.lowerBound), range.upperBound) }
}
