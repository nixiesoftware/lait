import SwiftUI

/// iOS cannot install native World runners independently under platform policy.
/// The mobile host therefore carries no product implementation.
struct LibraryView: View {
    let view: IosView

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ContentUnavailableView(
                        "No Worlds installed",
                        systemImage: "shippingbox",
                        description: Text("World runners are distributed independently and are not installable on iOS yet.")
                    )
                } footer: {
                    Text("Astrolabe does not bundle World products into the mobile host.")
                }
            }
            .navigationTitle("Library")
        }
    }
}
