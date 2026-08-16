import SwiftUI

/// The Library: the install list, exactly as on desktop. One row per World
/// this signed build carries, drawn from the compiled-in registry — name,
/// tagline, accent, and whether `Open` has anywhere to land. No probe runs to
/// draw it, and nothing here says which Spaces serve a World or whether any
/// is up: those are the destination's facts, and a row whose kind depends on
/// whether a daemon answers is the "Unnamed Space" defect.
struct LibraryView: View {
    let view: IosView

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(view.bundledWorlds, id: \.mount) { world in
                        HStack(spacing: 12) {
                            RoundedRectangle(cornerRadius: 2)
                                .fill(Color(accent: world.accent))
                                .frame(width: 4, height: 32)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(world.name).font(.body.weight(.medium))
                                if let tagline = world.tagline {
                                    Text(tagline).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            Spacer()
                            if !world.openable {
                                Text("not openable").font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                    }
                } footer: {
                    Text("What this signed build carries, compiled in. Open a World from one of your Spaces.")
                }
            }
            .navigationTitle("Library")
        }
    }
}
