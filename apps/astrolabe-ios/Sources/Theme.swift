import SwiftUI

/// The design language's tokens — structure and palette from the reviewed
/// direction ("The cut" canvas). The client's own chrome is cobalt on a cool
/// ground; a World's accent seed stays that World's fact and is never used as
/// client brand. Type stays system until the brand face is decided.
enum Theme {
    /// #F4F6FB — the page ground.
    static let ground = Color(red: 0.957, green: 0.965, blue: 0.984)
    /// #16213E — ink, and the trust chrome inside a World.
    static let ink = Color(red: 0.086, green: 0.129, blue: 0.243)
    /// #2B5CE6 — the client's own cobalt.
    static let cobalt = Color(red: 0.169, green: 0.361, blue: 0.902)
    /// #7A84A3 — secondary text.
    static let secondary = Color(red: 0.478, green: 0.518, blue: 0.639)
    /// #E9A23B — the wait: gates, asks, everything amber.
    static let amber = Color(red: 0.914, green: 0.635, blue: 0.231)
    /// Card corner radius.
    static let cardRadius: CGFloat = 20
}

extension Color {
    /// A Space's emblem color, derived deterministically from its NAME — the
    /// founding sheet promises "its colour comes from its name", so the
    /// preview and the real emblem must agree by construction. The palette
    /// deliberately avoids the bundled Worlds' accent seeds — a Space must
    /// never be mistaken for a World.
    static func emblem(for name: String) -> Color {
        let palette: [Color] = [
            Theme.cobalt,
            Theme.amber,
            Color(red: 0.133, green: 0.651, blue: 0.384), // green
            Color(red: 0.055, green: 0.604, blue: 0.655), // teal
            Color(red: 0.839, green: 0.365, blue: 0.694), // rose
            Color(red: 0.369, green: 0.424, blue: 0.588), // slate
        ]
        let sum = name.utf8.reduce(0) { $0 &+ Int($1) }
        return palette[sum % palette.count]
    }
}

/// A World's pictorial mark — the identity rule made visible: a letter is a
/// place, a MARK is software, a face is a person, a bare glyph is an agent.
/// Compiled in beside the accent, name and tagline; when a World ships real
/// artwork it replaces this at every size and nothing else changes.
///
/// White shapes on the World's accent tile; Signage's play triangle is a true
/// knockout so the accent shows through whatever the tile sits on.
struct WorldMark: View {
    let mount: String

    var body: some View {
        Canvas { context, size in
            let u = min(size.width, size.height) / 24
            func bar(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat,
                     radius: CGFloat, opacity: Double) {
                let rect = CGRect(x: x * u, y: y * u, width: w * u, height: h * u)
                context.fill(
                    Path(roundedRect: rect, cornerRadius: radius * u),
                    with: .color(.white.opacity(opacity))
                )
            }
            switch mount {
            case "issues":
                // Columns of work, descending. Solid, so it survives 18px.
                bar(4, 5, 4.5, 14, radius: 1.5, opacity: 1)
                bar(9.75, 5, 4.5, 9.5, radius: 1.5, opacity: 0.75)
                bar(15.5, 5, 4.5, 6, radius: 1.5, opacity: 0.5)
            case "signage":
                // A screen playing something, on its stand.
                bar(3, 5, 18, 12, radius: 2.4, opacity: 1)
                bar(10.5, 18, 3, 2, radius: 1, opacity: 1)
                var triangle = Path()
                triangle.move(to: CGPoint(x: 10.4 * u, y: 8.4 * u))
                triangle.addLine(to: CGPoint(x: 15 * u, y: 11 * u))
                triangle.addLine(to: CGPoint(x: 10.4 * u, y: 13.6 * u))
                triangle.closeSubpath()
                context.blendMode = .destinationOut
                context.fill(triangle, with: .color(.black))
                context.blendMode = .normal
            default:
                // An unnamed World until it ships a mark: four quiet cells.
                bar(5, 5, 6, 6, radius: 1.5, opacity: 1)
                bar(13, 5, 6, 6, radius: 1.5, opacity: 0.75)
                bar(5, 13, 6, 6, radius: 1.5, opacity: 0.75)
                bar(13, 13, 6, 6, radius: 1.5, opacity: 0.5)
            }
        }
    }
}

/// A World's tile: its mark on its accent, app-icon corners (~22%) because a
/// World IS an app — where people and Spaces keep the crisp 15%.
struct WorldTile: View {
    let mount: String
    let accent: UInt32?
    var size: CGFloat = 46

    var body: some View {
        WorldMark(mount: mount)
            .frame(width: size * 0.57, height: size * 0.57)
            .frame(width: size, height: size)
            .background(
                Color(accent: accent),
                in: RoundedRectangle(cornerRadius: size * 0.22)
            )
    }
}
