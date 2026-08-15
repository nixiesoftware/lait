import SwiftUI

/// Open World sessions — a plain list, each row's identity (Space, World),
/// swipe to close. Restored across launches; a restored tab reconciles
/// against the live head when reopened.
struct TabsView: View {
    let view: IosView
    let tabs: [OpenTab]
    let onSelect: (OpenTab) -> Void
    let onClose: (OpenTab) -> Void

    var body: some View {
        NavigationStack {
            Group {
                if tabs.isEmpty {
                    ContentUnavailableView {
                        Label("Nothing open", systemImage: "square.on.square")
                    } description: {
                        Text("Open a World from Spaces and it appears here as a tab.")
                    }
                } else {
                    List {
                        ForEach(tabs) { tab in
                            Button {
                                onSelect(tab)
                            } label: {
                                HStack(spacing: 12) {
                                    RoundedRectangle(cornerRadius: 2)
                                        .fill(Color(accent: tab.accent))
                                        .frame(width: 4, height: 36)
                                    VStack(alignment: .leading, spacing: 3) {
                                        Text("\(tab.worldName) · \(tab.spaceName)")
                                            .font(.body.weight(.medium))
                                            .foregroundStyle(.primary)
                                        if view.head == nil {
                                            StateChip(kind: .unmeasured(.orange), text: "head starting")
                                        } else {
                                            StateChip(kind: .measured(.green), text: "servable locally")
                                        }
                                    }
                                }
                            }
                            .swipeActions {
                                Button(role: .destructive) {
                                    onClose(tab)
                                } label: {
                                    Label("Close", systemImage: "xmark")
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("Tabs")
        }
    }
}
