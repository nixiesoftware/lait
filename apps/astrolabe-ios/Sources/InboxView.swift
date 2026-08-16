import SwiftUI

/// The Inbox: where deliveries land — the receiving side of the sealed
/// mailbox. This build cannot receive yet, and the surface names that fact
/// rather than rendering an empty list that would read as "all caught up".
struct InboxView: View {
    var body: some View {
        NavigationStack {
            ContentUnavailableView {
                Label("Nothing can arrive yet", systemImage: "tray")
            } description: {
                Text("Deliveries land here once this iPhone can receive correspondence — in a later build.")
            }
            .navigationTitle("Inbox")
        }
    }
}
