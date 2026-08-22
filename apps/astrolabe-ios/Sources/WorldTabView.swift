import SwiftUI
import WebKit

/// The World view: WebKit full-bleed under the ink trust bar. The bar is
/// where trust lives — it carries the World's own mark, names where you are,
/// and holds the exits; World pixels can never cover it. A failed load is a
/// named state on this surface, never a silent white page.
struct WorldTabView: View {
    let tab: OpenTab
    let head: HeadReady?
    @Environment(\.dismiss) private var dismiss
    @State private var reloadToken = 0
    @State private var loadFailure: String?

    var body: some View {
        VStack(spacing: 0) {
            trustBar
            Group {
                if let head {
                    // Keyed on the announcement, not just the reload count: a
                    // head restarted after suspension is a new port and a new
                    // token, and the session must replay the two-step open
                    // against it — updating the old web view would leave it
                    // authenticated against a listener that no longer exists.
                    WorldWebView(head: head, orbitId: tab.orbitId, failure: $loadFailure)
                        .id("\(reloadToken):\(head.port):\(head.token)")
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
                        Label("Connecting…", systemImage: "clock")
                    } description: {
                        Text("This World opens as soon as the connection is up.")
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Theme.ground)
                }
            }
        }
        .background(Theme.ink)
    }

    /// The shell's chrome, worn like the quick-menu bar: Home out, the
    /// place in the middle (mark, World, Space), reload as recovery.
    @ViewBuilder private var trustBar: some View {
        HStack(spacing: 10) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "house")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 34, height: 34)
                    .background(.white.opacity(0.12), in: Circle())
            }
            .buttonStyle(.plain)
            Spacer()
            HStack(spacing: 8) {
                WorldTile(mount: tab.mount, accent: tab.accent, size: 24)
                VStack(alignment: .leading, spacing: 0) {
                    Text(tab.worldName)
                        .font(.footnote.weight(.heavy))
                        .foregroundStyle(.white)
                    Text(tab.spaceName)
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.white.opacity(0.6))
                }
            }
            Spacer()
            Button {
                loadFailure = nil
                reloadToken += 1
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 34, height: 34)
                    .background(.white.opacity(0.12), in: Circle())
            }
            .buttonStyle(.plain)
        }
        .padding(.init(top: 8, leading: 14, bottom: 10, trailing: 14))
        .background(Theme.ink)
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
        #if DEBUG
            // Safari's inspector, for development builds only — a shipped
            // client must not expose its session to anything that asks.
            view.isInspectable = true
        #endif
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
            // Only off the head's own front page: the first `didFinish` is
            // not guaranteed to be the trade — a slow redirect or an in-World
            // navigation finishing first must never hijack the session.
            guard !steered, let orbitId,
                  let finished = webView.url,
                  finished.host == "127.0.0.1",
                  finished.port == Int(port),
                  finished.path.isEmpty || finished.path == "/",
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
