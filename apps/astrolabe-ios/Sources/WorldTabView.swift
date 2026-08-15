import SwiftUI
import WebKit

/// The World view: WebKit full-bleed under a minimal native bar. The bar is
/// where trust lives — it names the tab and carries the controls, and World
/// pixels can never cover it. A failed load is a named state on this surface,
/// never a silent white page.
struct WorldTabView: View {
    let tab: OpenTab
    let head: HeadReady?
    @Environment(\.dismiss) private var dismiss
    @State private var reloadToken = 0
    @State private var loadFailure: String?

    var body: some View {
        NavigationStack {
            Group {
                if let head {
                    WorldWebView(head: head, orbitId: tab.orbitId, failure: $loadFailure)
                        .id(reloadToken)
                        .ignoresSafeArea(edges: .bottom)
                        .overlay(alignment: .top) {
                            if let loadFailure {
                                Text(loadFailure)
                                    .font(.caption)
                                    .foregroundStyle(.red)
                                    .padding(8)
                                    .frame(maxWidth: .infinity)
                                    .background(.thinMaterial)
                            }
                        }
                } else {
                    ContentUnavailableView {
                        Label("Head starting", systemImage: "clock")
                    } description: {
                        Text("This tab opens as soon as the node's head is up.")
                    }
                }
            }
            .navigationTitle("\(tab.worldName) · \(tab.spaceName)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Tabs") { dismiss() }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        loadFailure = nil
                        reloadToken += 1
                    } label: {
                        Label("Reload", systemImage: "arrow.clockwise")
                    }
                }
            }
        }
    }
}

/// The two-step open: land on `/` with the run token (the head trades it for
/// a session cookie and redirects), then navigate to the Space. The token
/// never lingers in a URL the person can copy.
private struct WorldWebView: UIViewRepresentable {
    let head: HeadReady
    let orbitId: String?
    @Binding var failure: String?

    func makeCoordinator() -> Coordinator {
        Coordinator(orbitId: orbitId, port: head.port) { failure = $0 }
    }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        // Partitioned per tab in v0; the per-(Space, World) persistent store
        // arrives with the isolation work.
        configuration.websiteDataStore = .nonPersistent()
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        if #available(iOS 16.4, *) {
            view.isInspectable = true
        }
        if let url = URL(string: head.url) {
            view.load(URLRequest(url: url))
        }
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {}

    final class Coordinator: NSObject, WKNavigationDelegate {
        let orbitId: String?
        let port: UInt16
        let report: (String?) -> Void
        private var steered = false

        init(orbitId: String?, port: UInt16, report: @escaping (String?) -> Void) {
            self.orbitId = orbitId
            self.port = port
            self.report = report
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            // After the token-for-cookie trade lands, steer once to the Space.
            guard !steered, let orbitId,
                  let url = URL(string: "http://127.0.0.1:\(port)/spaces/\(orbitId)")
            else { return }
            steered = true
            webView.load(URLRequest(url: url))
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor response: WKNavigationResponse,
            decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void
        ) {
            if let http = response.response as? HTTPURLResponse, http.statusCode >= 400 {
                report("HTTP \(http.statusCode) at \(http.url?.path ?? "?")")
            }
            decisionHandler(.allow)
        }

        func webView(
            _ webView: WKWebView,
            didFailProvisionalNavigation navigation: WKNavigation!,
            withError error: Error
        ) {
            report("load failed: \(error.localizedDescription)")
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            report("load failed: \(error.localizedDescription)")
        }
    }
}
