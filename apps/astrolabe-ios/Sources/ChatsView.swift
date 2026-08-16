import SwiftUI

/// Chats: correspondence between people, riding the sealed mailbox — a
/// payload sealed once, unlocked per device. That contract is not yet issued,
/// and this surface says so: "cannot yet" is a different absence from "none",
/// and only one of them invites the person to look for a compose button.
struct ChatsView: View {
    var body: some View {
        NavigationStack {
            ContentUnavailableView {
                Label("No chats can exist yet", systemImage: "bubble.left.and.bubble.right")
            } description: {
                Text("Chats arrive with correspondence — the sealed mailbox — in a later build.")
            }
            .navigationTitle("Chats")
        }
    }
}
