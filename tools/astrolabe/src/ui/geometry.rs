//! The ladder — one vocabulary for every measurement in the client.
//!
//! Ported from the tracker's own system (`viewer/src/styles.css`) rather than
//! invented, because that one is thought through and two design systems in one
//! product is one more than anybody can hold. The names, the axes and the
//! reasoning are the same; only the units are Rust rather than custom
//! properties.
//!
//! ## The question is never "how many pixels"
//!
//! It is *which rung*. A raw number at a call site is a decision nobody can
//! find again, and a hundred of them are a system nobody can retune. Every
//! measurement here is named for what it is, and the tuning happens by pointing
//! a name at a different rung — never by editing the places that use it.
//!
//! ## Two axes that are deliberately not one
//!
//! [`UNIT`] is the rhythm's *quantum*. [`SCALE`] is the system's *density*.
//! Splitting them is what lets a comfortable layout loosen its rows without
//! also inflating the glyphs and marks, which are **pinned** and read the scale
//! not at all. Today the two move together, so nothing shifts; they are
//! separable the moment we want a denser rhythm at the same control height.
//!
//! A thing in this file either scales or is pinned, and which one it is is an
//! argument about the thing rather than a default:
//!
//! - **Control heights scale.** A control's height *is* rhythm — comfortable
//!   should mean a roomier row, which is what density means.
//! - **Glyphs and marks are pinned.** How loud a status dot reads is a claim
//!   about the dot, not about how roomy the row around it is.
//! - **Bars are named, not laddered.** A bar is sized by what it must hold.

use egui::{FontFamily, FontId, Margin, Style, TextStyle, Vec2};

/// The rhythm's quantum. Every gap in the client is a multiple of this.
pub const UNIT: f32 = 4.0;

/// The density factor. Applied to what scales, and to nothing else.
pub const SCALE: f32 = 1.0;

/// `px` at the current density.
fn scaled(px: f32) -> f32 {
    px * SCALE
}

/// Control heights — the one height vocabulary the client has.
///
/// These exist because the alternative is what the tracker had before it grew
/// them: heights arriving from five places under three names, so "one notch
/// taller" was a five-file edit.
///
/// - `XS` 20 — a chip or an inline label
/// - `SM` 24 — toolbar buttons, segmented filters
/// - `MD` 28 — the default: property rows, buttons, small fields
/// - `LG` 32 — inputs, list rows, menu rows
/// - `XL` 40 — full-width list lines
pub mod control {
    use super::scaled;

    pub fn xs() -> f32 {
        scaled(20.0)
    }
    pub fn sm() -> f32 {
        scaled(24.0)
    }
    pub fn md() -> f32 {
        scaled(28.0)
    }
    pub fn lg() -> f32 {
        scaled(32.0)
    }
    pub fn xl() -> f32 {
        scaled(40.0)
    }
}

/// Chrome bars — a separate axis from controls, because a bar is sized by what
/// it must hold and not by the ladder.
///
/// The surface tab row is `lg`: it carries seven names and the re-read control,
/// and it is the one bar a person aims at with a mouse. A section header is
/// `md`, deliberately one step under the rows it introduces.
pub mod bar {
    use super::scaled;

    pub fn sm() -> f32 {
        scaled(32.0)
    }
    pub fn md() -> f32 {
        scaled(36.0)
    }
    pub fn lg() -> f32 {
        scaled(44.0)
    }
}

/// Gaps, in quanta. Named for the relationship they express rather than for
/// their size, so "these two things belong together" survives a retune.
pub mod gap {
    use super::UNIT;

    /// Between a label and the thing it labels.
    pub fn tight() -> f32 {
        UNIT
    }
    /// Between controls on one row.
    pub fn row() -> f32 {
        UNIT * 2.0
    }
    /// Between rows in a list.
    pub fn stack() -> f32 {
        UNIT * 1.5
    }
    /// Between one section and the next.
    pub fn section() -> f32 {
        UNIT * 5.0
    }
}

/// The navigation bar's own measurements.
///
/// Its own module because a nav pill is not a button: the reference this client
/// is proportioned against pads its nav items to 1.8× their text size, which on
/// an ordinary button would be absurd. Applying these to `Style` would make
/// every button in the client a navigation pill, so they are set for the header's
/// scope and nowhere else.
pub mod nav {
    use super::{text, UNIT};

    /// Horizontal padding inside a pill.
    ///
    /// 1.4× the body size — the reference runs 1.8× at a 16px body, and a dense
    /// client wants some of that air but not all of it. This is the single
    /// biggest difference between a row of tabs and a header.
    pub const PADDING: f32 = text::BASE * 1.4;

    /// Between one pill and the next.
    ///
    /// The reference's padding-to-gap ratio is about 4∶1, which is what keeps
    /// the row reading as separate targets rather than as one striped block.
    pub const GAP: f32 = UNIT;
}

/// The window's own controls — the one place the platform outranks the ladder.
///
/// Minimise, maximise and close are drawn by this client rather than by the
/// window manager, and a person aims at them with muscle memory built on every
/// other window on the machine. So the width is the platform's figure, pinned,
/// and the height is whatever bar they sit in: **full** height, so that the
/// screen corner itself is the target when the window is maximised. A cluster
/// inset from the corner by even a point is a cluster you have to aim at.
pub mod caption {
    /// One control's width. Windows' own caption metric.
    pub const WIDTH: f32 = 46.0;
    /// The mark inside it. A mark, so it is pinned like every other one — how
    /// big a close cross reads is a claim about the cross.
    pub const MARK: f32 = 10.0;
    /// The mark is drawn as a hairline at every density, because it is a glyph
    /// standing in for a system font's and that one does not thicken either.
    pub const HAIRLINE: f32 = 1.0;

    /// How wide the three of them are together.
    pub fn span() -> f32 {
        WIDTH * 3.0
    }
}

/// The smallest window this client opens.
///
/// Named here rather than left as a literal in the shell, because it is the
/// width every layout has to survive: a measurement that only works at the size
/// the window happens to open at is one that breaks the first time somebody
/// drags a corner, and there is nowhere else in this file to check it against.
pub const NARROWEST: Vec2 = Vec2::new(640.0, 480.0);

/// The Library's rail — the list column a detail pane sits beside.
///
/// Pinned rather than proportional, which is the reference's own choice and the
/// right one: a rail is sized by the names in it, so a fifth of a 1040 window is
/// a readable measure and a fifth of a maximised 2560 one is a wide column of
/// short words with the pane squeezed behind it. The reference runs 390 of 1920
/// at a 16px body; this is the same fifth of this client's window.
pub mod rail {
    /// The column's width, on the rhythm.
    pub const WIDTH: f32 = 208.0;
}

/// Padding inside a thing that is not the page.
pub mod pad {
    /// A card: the raised block a section of a page sits in.
    ///
    /// The reference pads its cards 25 against a 16px body; this is the same
    /// ratio landed on the rhythm, and it is deliberately the section gap — the
    /// space around a block and the space between blocks being equal is what
    /// makes a page read as one grid rather than as several.
    pub const fn card() -> i8 {
        20
    }
}

/// The page's own breathing room.
///
/// A surface flush against the window edge reads as unfinished whatever else is
/// right about it, and this is the single place that stops being true.
pub fn page_margin() -> Margin {
    Margin {
        left: 16,
        right: 16,
        top: 12,
        bottom: 16,
    }
}

/// Roundness by **role**, never by value.
///
/// The rungs are private on purpose. In the tracker you cannot write
/// `rounded-8`, so a corner is always chosen by what the thing *is*; here you
/// cannot reach the rung either. Retuning points a role at a different rung and
/// every call site follows.
pub mod radius {
    use egui::CornerRadius;

    /// The private ladder. Named for their values, so a name never lies.
    const R4: u8 = 4;
    const R6: u8 = 6;
    const R8: u8 = 8;
    const R12: u8 = 12;

    /// A swatch or a status dot — a corner with no glyph inside it.
    pub const fn mark() -> CornerRadius {
        CornerRadius::same(R4)
    }
    /// A list row's fill. A fourth role rather than bending one of the others:
    /// a control's 8 reads as a lozenge on a 28px row, and a mark's 4 came out
    /// sharper than a stack of row fills wants.
    pub const fn row() -> CornerRadius {
        CornerRadius::same(R6)
    }
    /// Buttons, inputs, anything a person aims at.
    pub const fn control() -> CornerRadius {
        CornerRadius::same(R8)
    }
    /// A panel, a dialog, a card.
    pub const fn surface() -> CornerRadius {
        CornerRadius::same(R12)
    }
}

/// The type scale.
///
/// Dense-UI sizes: 13 is the body size a tracker actually reads at, and the
/// steps below it are for text that is genuinely secondary rather than for text
/// somebody wanted to fit.
pub mod text {
    /// A count on a chip, a timestamp in a log line.
    pub const XXS: f32 = 10.0;
    /// Supporting detail beside a row.
    pub const XS: f32 = 11.0;
    /// Secondary text that is still read in full sentences.
    pub const SM: f32 = 12.0;
    /// Body. The default for everything a person reads rather than glances at.
    pub const BASE: f32 = 13.0;
    /// A section heading.
    pub const LG: f32 = 15.0;
}

/// Wire the ladder into egui's own vocabulary.
///
/// Called once at startup, and by the snapshot harness, so the shell and every
/// rendered surface are measured by the same thing. This is the seam: it is the
/// only function in the client that turns a rung into an egui field.
pub fn apply(style: &mut Style) {
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(text::XS, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(text::BASE, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(text::BASE, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(text::LG, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(text::SM, FontFamily::Monospace),
        ),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(gap::row(), gap::stack());
    // Anything clickable is at least a control tall. This is what stops a
    // button from being sized by its label alone, which is how a row of them
    // ends up ragged.
    spacing.interact_size = Vec2::new(control::md(), control::md());
    spacing.button_padding = Vec2::new(UNIT * 2.5, UNIT);
    spacing.window_margin = page_margin();
    spacing.menu_margin = Margin::same(4);
    spacing.indent = UNIT * 4.0;
    spacing.icon_width = 14.0;
    spacing.icon_width_inner = 8.0;
    // A field is a control, not a paragraph: it stops at a readable measure
    // rather than growing to whatever the row allows.
    spacing.text_edit_width = 220.0;
    spacing.combo_width = 140.0;

    // Every interactive state gets the same corner, because they are the same
    // control in different moods. A hovered button that changed shape would be
    // saying something the hover does not mean.
    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = radius::control();
    }
    style.visuals.window_corner_radius = radius::surface();
    style.visuals.menu_corner_radius = radius::surface();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ladder whose rungs are not ordered is not a ladder. This is the one
    /// property every later retune has to preserve.
    #[test]
    fn every_ladder_climbs() {
        let controls = [
            control::xs(),
            control::sm(),
            control::md(),
            control::lg(),
            control::xl(),
        ];
        for pair in controls.windows(2) {
            assert!(
                pair[0] < pair[1],
                "the control ladder is not ordered: {controls:?}"
            );
        }

        let bars = [bar::sm(), bar::md(), bar::lg()];
        for pair in bars.windows(2) {
            assert!(pair[0] < pair[1], "the bar ladder is not ordered: {bars:?}");
        }

        let sizes = [text::XXS, text::XS, text::SM, text::BASE, text::LG];
        for pair in sizes.windows(2) {
            assert!(
                pair[0] < pair[1],
                "the type scale is not ordered: {sizes:?}"
            );
        }
    }

    /// Every gap lands on the rhythm, to the half-quantum.
    ///
    /// Whole quanta wherever possible; `stack` is the one half-step and it
    /// earns it — a list needs more air between rows than a label needs from
    /// its own control, and less than two controls side by side. Rounding it up
    /// to 8 would make row-to-row and control-to-control the same gap, which is
    /// the distinction the ladder exists to keep.
    #[test]
    fn every_gap_lands_on_the_rhythm() {
        for gap in [gap::tight(), gap::stack(), gap::row(), gap::section()] {
            let halves = gap / (UNIT / 2.0);
            assert!(
                (halves - halves.round()).abs() < f32::EPSILON,
                "{gap} is not on the rhythm of {UNIT}"
            );
        }
    }

    /// A nav pill is padded far wider than it is gapped. Lose that ratio and
    /// the row stops reading as separate targets and becomes one striped block.
    #[test]
    fn a_nav_pill_is_padded_wider_than_it_is_gapped() {
        const {
            assert!(
                nav::PADDING > nav::GAP * 3.0,
                "the nav's padding-to-gap ratio collapsed"
            );
        }
        // And the pill is airier than an ordinary button, which is the whole
        // reason it has its own measurements.
        let mut style = Style::default();
        apply(&mut style);
        assert!(
            nav::PADDING > style.spacing.button_padding.x,
            "a nav pill is padded no wider than an ordinary button"
        );
    }

    /// A caption control is the platform's target, and its mark sits inside it
    /// with room to spare. The failure this guards is the tempting one: pulling
    /// the cluster onto the control ladder, where it would be `lg` wide, look
    /// tidier than the system's, and be missed by everybody who aims at the
    /// corner without looking.
    #[test]
    fn a_caption_control_is_the_platforms_target_and_not_the_ladders() {
        assert!(
            caption::WIDTH > control::xl(),
            "the caption cluster shrank onto the control ladder"
        );
        assert!(
            gap::row().mul_add(2.0, caption::MARK) < caption::WIDTH,
            "the mark leaves no room inside its control"
        );
        assert!(
            caption::MARK < bar::lg(),
            "the mark is taller than the bar that holds it"
        );
        assert!(caption::WIDTH.mul_add(-3.0, caption::span()).abs() < f32::EPSILON);
    }

    /// A card's padding is the section gap. Two names for one measurement is
    /// how a grid stops being a grid: the moment they drift, the space inside a
    /// block and the space between blocks disagree and the page reads as
    /// several pages.
    #[test]
    fn a_card_is_padded_by_the_same_step_that_separates_sections() {
        assert!(
            (f32::from(pad::card()) - gap::section()).abs() < f32::EPSILON,
            "a card's padding and the section rhythm have drifted apart"
        );
    }

    /// The rail is a readable measure, not a fraction. Both bounds matter: too
    /// narrow and every name is elided, too wide and the pane it exists to
    /// introduce is the smaller half of the window.
    #[test]
    fn the_rail_is_a_measure_rather_than_a_fraction() {
        assert!(
            rail::WIDTH > control::xl() * 4.0,
            "the rail is too narrow to hold a name"
        );
        const {
            assert!(
                rail::WIDTH < NARROWEST.x / 2.0,
                "the rail takes more than half of the narrowest window this client opens"
            );
        }
        assert!(
            (rail::WIDTH / UNIT).fract() < f32::EPSILON,
            "the rail is off the rhythm"
        );
    }

    /// Gaps express relationships, and the relationships have an order: things
    /// that belong together sit closer than things that merely follow.
    #[test]
    fn a_gap_says_how_related_two_things_are() {
        assert!(gap::tight() < gap::stack());
        assert!(gap::stack() < gap::row());
        assert!(gap::row() < gap::section());
    }

    /// The roles are distinct. Two roles that resolved to the same corner would
    /// be one role with two names, and the next retune would move both.
    #[test]
    fn each_radius_role_is_its_own_rung() {
        let roles = [
            radius::mark(),
            radius::row(),
            radius::control(),
            radius::surface(),
        ];
        for (index, role) in roles.iter().enumerate() {
            for other in roles.iter().skip(index + 1) {
                assert_ne!(role, other, "two roles resolve to the same corner");
            }
        }
    }

    /// Applying the ladder is what makes it true of the interface, and applying
    /// it twice must be the same as applying it once — the shell and the
    /// snapshot harness both call it, and a test that renders after the shell
    /// has run must measure the same thing.
    #[test]
    fn applying_the_ladder_is_idempotent() {
        let mut once = Style::default();
        apply(&mut once);
        let mut twice = once.clone();
        apply(&mut twice);

        assert_eq!(once.spacing.item_spacing, twice.spacing.item_spacing);
        assert_eq!(once.spacing.interact_size, twice.spacing.interact_size);
        assert_eq!(
            once.visuals.widgets.inactive.corner_radius,
            twice.visuals.widgets.inactive.corner_radius
        );
        assert_eq!(
            once.text_styles.get(&TextStyle::Body),
            twice.text_styles.get(&TextStyle::Body)
        );
    }

    /// The ladder actually reaches egui, rather than being a set of constants
    /// nobody wired up. Body text at the default size is the tell.
    #[test]
    fn the_ladder_reaches_egui() {
        let mut style = Style::default();
        apply(&mut style);

        // Asserted against the ladder rather than against "it changed",
        // because egui's own default body size happens to be 13 and a
        // difference test would pass on a ladder that had never been applied.
        assert!(
            style
                .text_styles
                .get(&TextStyle::Body)
                .is_some_and(|font| (font.size - text::BASE).abs() < f32::EPSILON),
            "body text is not the ladder's body size"
        );
        assert!(
            (style.spacing.item_spacing.y - gap::stack()).abs() < f32::EPSILON,
            "the vertical rhythm is not the ladder's stack gap"
        );
        assert!(
            (style.spacing.item_spacing.x - gap::row()).abs() < f32::EPSILON,
            "the horizontal rhythm is not the ladder's row gap"
        );
        assert!(
            style.spacing.interact_size.y >= control::md(),
            "a control can be shorter than the ladder's default height"
        );
        assert_eq!(
            style.visuals.widgets.inactive.corner_radius,
            radius::control(),
            "a control's corner did not come from the role"
        );
        assert_eq!(style.spacing.window_margin, page_margin());
    }
}
