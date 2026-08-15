//! Colour that survives a theme it did not choose.
//!
//! Light, dark and *high contrast* are honoured rather than approximated. The
//! third is the one naive implementations lose: a hard-coded amber that reads
//! fine on both light and dark backgrounds is invisible against a high-contrast
//! scheme the person selected precisely because they cannot see it otherwise.
//!
//! So nothing here returns a fixed colour. Every accent is asked of the visuals
//! in force, which is what the platform theme actually reaches.
//!
//! ## Asking is not enough
//!
//! Asking the visuals gets an answer; it does not get a *readable* answer. egui's
//! own light theme answers `warn_fg_color` with an orange that sits at 2.79∶1
//! against the panel it is drawn on — below every threshold there is — and this
//! module returned it verbatim for months. Every stale figure, every degraded
//! observation and every unmeasured footprint in the client was drawn in it.
//!
//! So each accent is now taken from the visuals *and then held to a floor*: if it
//! does not clear [`MINIMUM_CONTRAST`] against the surface behind it, it is moved
//! away from that surface until it does. The hue survives, the theme still
//! decides, and the answer is legible whatever the theme decided.

use egui::{Color32, Ui};

/// The contrast a short coloured **label** must clear against what is behind
/// it.
///
/// WCAG's large-text and non-text floor. A word or two beside ordinary text is
/// what that threshold is for.
pub const MINIMUM_CONTRAST: f64 = 3.0;

/// The contrast a **paragraph** must clear.
///
/// WCAG's body-text ratio, and the distinction is not pedantry. The client
/// explains itself in full sentences — what an MCP binding pins, why a transfer
/// lane is empty, what deleting a device destroys — and every one of them was
/// drawn at the label floor, which is how a paragraph ends up technically
/// compliant and actually unreadable. A floor that does not depend on the role
/// is a floor set for the shortest thing that uses it.
pub const MINIMUM_BODY_CONTRAST: f64 = 4.5;

/// Something the person should notice but that is not an error — a degraded
/// observation, a stale figure.
pub fn attention(ui: &Ui) -> Color32 {
    legible(ui.visuals().warn_fg_color, behind(ui))
}

/// Something went wrong.
pub fn danger(ui: &Ui) -> Color32 {
    legible(ui.visuals().error_fg_color, behind(ui))
}

/// A short supporting label — a state word, a count, a mount name.
///
/// Asked for rather than dimmed by a fixed alpha, because "70% of the
/// foreground" is illegible in exactly the scheme that needs it most — and then
/// held to the label floor, because a weak colour that cannot be read is not
/// weak, it is absent.
pub fn secondary(ui: &Ui) -> Color32 {
    legible(ui.visuals().weak_text_color(), behind(ui))
}

/// Supporting text a person reads as *prose* — a sentence explaining what a
/// surface does or why something is empty.
///
/// Quieter than the body it sits under, and held to the body floor rather than
/// the label one. Use this for anything with a verb in it.
pub fn prose(ui: &Ui) -> Color32 {
    legible_to(
        ui.visuals().weak_text_color(),
        behind(ui),
        MINIMUM_BODY_CONTRAST,
    )
}

/// A surface one step in front of the page — the header's fill.
///
/// Derived from the page rather than named, so it is still the theme deciding.
/// Lighter in a dark scheme and darker in a light one, which is the direction
/// "in front" means in each; a fixed nudge would raise the header in one theme
/// and sink it in the other.
pub fn raised(ui: &Ui) -> Color32 {
    step(behind(ui), 0.08)
}

/// The line under the header. A hairline, not a divider: it marks where the
/// chrome ends rather than separating two regions of equal weight.
pub fn hairline(ui: &Ui) -> Color32 {
    step(behind(ui), 0.14)
}

/// A surface with the pointer on it.
///
/// Takes the surface rather than the `Ui` because the thing under the pointer is
/// not always sitting on the page: a window control sits on the header, which is
/// already a step in front of it, and deriving its hover from the page would put
/// the two at the same colour in one theme and invert them in the other.
pub fn hovered(surface: Color32) -> Color32 {
    step(surface, 0.10)
}

/// A surface with the button down on it. Further than [`hovered`], in the same
/// direction, so press reads as more of what hover already said.
pub fn pressed(surface: Color32) -> Color32 {
    step(surface, 0.18)
}

/// The one colour that means "this is the thing to press".
///
/// The scheme's own selection fill, which is already what marks the current
/// surface in the header — so a page's primary control and the tab a person is
/// on are the same colour by construction rather than by two constants that
/// agree today. Whatever is drawn *on* it must still be put through
/// [`legible`], because a fill is not a background this module has measured.
pub fn accent(ui: &Ui) -> Color32 {
    over(ui.visuals().selection.bg_fill, behind(ui))
}

/// The fill under a control whose click *ends* something — the window's close
/// button, and so far nothing else.
///
/// The theme's own error colour rather than the platform's red, for the same
/// reason nothing else here is a constant: a fixed `#C42B1C` is invisible in
/// exactly the scheme somebody chose because they cannot see. Flattened before
/// it leaves, because a translucent fill that measured as one colour and drew as
/// another is the defect [`over`] exists to prevent.
pub fn danger_fill(ui: &Ui) -> Color32 {
    over(ui.visuals().error_fg_color, behind(ui))
}

/// `surface`, moved one step away from itself.
///
/// Lighter in a dark scheme and darker in a light one, which is the direction
/// "in front" means in each; a fixed nudge would raise a surface in one theme
/// and sink it in the other.
fn step(surface: Color32, amount: f64) -> Color32 {
    let toward = if luminance(surface) > 0.5 {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    blend(surface, toward, amount)
}

/// What these are drawn on.
fn behind(ui: &Ui) -> Color32 {
    ui.visuals().panel_fill
}

/// `colour`, moved away from `background` until it can be read on it.
///
/// Blended toward black or white in fixed steps. Blending rather than
/// recomputing keeps the hue the theme chose, and stepping rather than solving
/// keeps this a pure function with no iteration limit to reason about.
///
/// The end it moves toward is whichever of black and white contrasts *more* with
/// the background, measured rather than guessed from a lightness threshold. A
/// threshold gets it wrong in the middle of the range — against a background at
/// 0.4 luminance, "not light, so go lighter" ends at white, which is 2.3∶1 and
/// still unreadable, while black is 9∶1. Every background has at least one
/// extreme that clears the floor, so choosing by measurement always terminates
/// somewhere legible.
pub fn legible(colour: Color32, background: Color32) -> Color32 {
    legible_to(colour, background, MINIMUM_CONTRAST)
}

/// `legible`, against a floor the caller chooses because the caller knows what
/// the text is.
pub fn legible_to(colour: Color32, background: Color32, floor: f64) -> Color32 {
    // Flattened first. A translucent answer that measured correctly and was
    // then drawn at its own alpha would be a different colour again, so what
    // comes back here is always what was measured.
    let colour = over(colour, background);
    if contrast(colour, background) >= floor {
        return colour;
    }
    let toward = if contrast(Color32::BLACK, background) >= contrast(Color32::WHITE, background) {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    const STEPS: u8 = 20;
    for step in 1..=STEPS {
        let candidate = blend(colour, toward, f64::from(step) / f64::from(STEPS));
        if contrast(candidate, background) >= floor {
            return candidate;
        }
    }
    toward
}

/// The WCAG contrast ratio between a colour *as drawn* and what is behind it:
/// 1.0 for identical, 21.0 for black on white.
///
/// The foreground is composited over the background first, and that is not a
/// detail. egui's `weak_text_color()` is `#303030` at 60% alpha — opaque, it is
/// 13∶1 against white and clears every threshold there is; drawn, it composites
/// to roughly `#8f8f8f` and is about 3.9∶1. Measuring the stored colour rather
/// than the drawn one made this whole module report a floor it was not holding.
pub fn contrast(foreground: Color32, background: Color32) -> f64 {
    let foreground = over(foreground, background);
    let (a, b) = (luminance(foreground), luminance(background));
    let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

/// `foreground` composited over `background`, source-over.
///
/// The result is opaque, which is the point: it is what a person's eye receives,
/// and it is therefore the only thing worth measuring or returning.
pub fn over(foreground: Color32, background: Color32) -> Color32 {
    let alpha = f64::from(foreground.a()) / 255.0;
    if alpha >= 1.0 {
        return foreground;
    }
    let mix = |front: u8, back: u8| -> u8 {
        let value = (f64::from(front) - f64::from(back)).mul_add(alpha, f64::from(back));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=255 immediately above"
        )]
        {
            value.round().clamp(0.0, 255.0) as u8
        }
    };
    Color32::from_rgb(
        mix(foreground.r(), background.r()),
        mix(foreground.g(), background.g()),
        mix(foreground.b(), background.b()),
    )
}

/// WCAG relative luminance.
///
/// Written as fused multiply-adds because that is both what the coefficients
/// are and the more accurate way to accumulate them.
fn luminance(colour: Color32) -> f64 {
    0.0722f64.mul_add(
        channel(colour.b()),
        0.7152f64.mul_add(channel(colour.g()), 0.2126 * channel(colour.r())),
    )
}

/// One channel, linearised.
///
/// WCAG's transfer function, spelled out rather than approximated with a gamma
/// of 2.2 — the two disagree most in exactly the dark range a high-contrast
/// scheme lives in.
fn channel(value: u8) -> f64 {
    let value = f64::from(value) / 255.0;
    if value <= 0.039_28 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn blend(from: Color32, to: Color32, amount: f64) -> Color32 {
    let mix = |a: u8, b: u8| -> u8 {
        let value = (f64::from(b) - f64::from(a)).mul_add(amount, f64::from(a));
        // Clamped before the cast, so the conversion cannot wrap. `round` keeps
        // a 50% blend of two neighbouring values from collapsing onto one.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=255 immediately above"
        )]
        {
            value.round().clamp(0.0, 255.0) as u8
        }
    };
    Color32::from_rgb(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accents must actually differ between schemes. A constant would pass
    /// a "does it compile" check and fail the person using high contrast.
    #[test]
    fn accents_follow_the_scheme_rather_than_being_fixed() {
        let ctx = egui::Context::default();

        let read = |visuals: egui::Visuals| {
            ctx.set_visuals(visuals);
            let mut captured = None;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                captured = Some((attention(ui), danger(ui), secondary(ui)));
            });
            captured.expect("a frame drew")
        };

        let light = read(egui::Visuals::light());
        let dark = read(egui::Visuals::dark());
        assert_ne!(
            light, dark,
            "the accents are the same in light and dark, so they are hard-coded"
        );
    }

    /// The floor is a floor. This is the case that was actually broken: egui's
    /// light theme answers `warn_fg_color` with an orange at 2.79∶1 against the
    /// panel, and returning it verbatim drew every stale figure in the client in
    /// a colour that cannot be read.
    #[test]
    fn an_accent_the_theme_answers_illegibly_is_moved_until_it_can_be_read() {
        let visuals = egui::Visuals::light();
        let raw = visuals.warn_fg_color;
        let background = visuals.panel_fill;
        assert!(
            contrast(raw, background) < MINIMUM_CONTRAST,
            "this test no longer exercises the case it exists for: egui's light \
             warning colour now clears the floor on its own"
        );

        let fixed = legible(raw, background);
        assert!(
            contrast(fixed, background) >= MINIMUM_CONTRAST,
            "an illegible accent came back illegible"
        );
        assert_ne!(fixed, raw);
    }

    /// Prose is held to a higher floor than a label, and the difference is
    /// visible rather than nominal — this is the case the screenshot exposed:
    /// three lines of explanation in a grey that cleared the label floor and
    /// could not be read.
    #[test]
    fn a_paragraph_is_held_to_a_higher_floor_than_a_word() {
        const { assert!(MINIMUM_BODY_CONTRAST > MINIMUM_CONTRAST) };

        let ctx = egui::Context::default();
        for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            ctx.set_visuals(visuals);
            let mut captured = None;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                captured = Some((secondary(ui), prose(ui), ui.visuals().panel_fill));
            });
            let (label, paragraph, background) = captured.expect("a frame drew");
            assert!(contrast(label, background) >= MINIMUM_CONTRAST);
            assert!(
                contrast(paragraph, background) >= MINIMUM_BODY_CONTRAST,
                "prose came back at {:.2}:1",
                contrast(paragraph, background)
            );
            assert_ne!(
                label, paragraph,
                "a label and a paragraph resolved to the same colour, so the \
                 distinction is nominal"
            );
        }
    }

    /// A colour that is already readable is left exactly as the theme chose it.
    /// Adjusting one that did not need it would be this module overruling a
    /// scheme it has no opinion about.
    #[test]
    fn a_legible_accent_is_left_alone() {
        let black = Color32::BLACK;
        let white = Color32::WHITE;
        assert_eq!(legible(black, white), black);
        assert_eq!(legible(white, black), white);
    }

    /// The direction is away from the background, not toward a fixed end. On a
    /// dark scheme the answer gets lighter; on a light one it gets darker.
    #[test]
    fn an_accent_moves_away_from_what_is_behind_it() {
        let nearly_black = Color32::from_rgb(60, 60, 60);
        let lifted = legible(nearly_black, Color32::BLACK);
        assert!(
            luminance(lifted) > luminance(nearly_black),
            "an accent on a dark background got darker"
        );

        let nearly_white = Color32::from_rgb(200, 200, 200);
        let dropped = legible(nearly_white, Color32::WHITE);
        assert!(
            luminance(dropped) < luminance(nearly_white),
            "an accent on a light background got lighter"
        );
    }

    /// The property, over the whole range rather than over the three schemes
    /// that happen to ship. Every background has at least one extreme that
    /// clears the floor; a threshold-based direction picks the wrong one in the
    /// middle of the range, and this is what catches that.
    #[test]
    fn every_colour_on_every_background_comes_back_readable() {
        let steps = [0_u8, 40, 80, 100, 110, 120, 130, 160, 200, 255];
        for &background in &steps {
            let behind = Color32::from_rgb(background, background, background);
            for &shade in &steps {
                for candidate in [
                    Color32::from_rgb(shade, shade, shade),
                    Color32::from_rgb(shade, 0, 0),
                    Color32::from_rgb(0, shade, 0),
                    Color32::from_rgb(0, 0, shade),
                ] {
                    let fixed = legible(candidate, behind);
                    let ratio = contrast(fixed, behind);
                    assert!(
                        ratio >= MINIMUM_CONTRAST,
                        "{candidate:?} on {behind:?} came back at {ratio:.2}:1 as {fixed:?}"
                    );
                }
            }
        }
    }

    /// Alpha is not a detail. This is the defect the role split exposed:
    /// `weak_text_color()` is `#303030` at 60% alpha, which is 13∶1 against
    /// white if you read the stored bytes and about 3.9∶1 once it is drawn.
    /// Measuring the stored colour made the floor a number this module
    /// reported rather than one it held.
    #[test]
    fn contrast_is_measured_on_the_colour_that_is_actually_drawn() {
        let translucent = Color32::from_rgba_unmultiplied(0x30, 0x30, 0x30, 0x99);
        let white = Color32::WHITE;

        let opaque_reading = contrast(Color32::from_rgb(0x30, 0x30, 0x30), white);
        let drawn_reading = contrast(translucent, white);
        assert!(
            opaque_reading > 12.0,
            "the opaque colour is not the high-contrast one this test assumes"
        );
        assert!(
            drawn_reading < 5.0,
            "a 60%-alpha grey measured as though it were opaque: {drawn_reading:.2}:1"
        );

        // And what comes back is opaque, so drawing it cannot undo the fix.
        let fixed = legible_to(translucent, white, MINIMUM_BODY_CONTRAST);
        assert_eq!(fixed.a(), 255, "a corrected colour is still translucent");
        assert!(contrast(fixed, white) >= MINIMUM_BODY_CONTRAST);
    }

    /// Hover and press are steps in one direction, and the direction is away
    /// from the surface they are on. A press that landed *between* the surface
    /// and its hover would read as the control coming back up.
    #[test]
    fn press_goes_further_than_hover_and_both_leave_the_surface() {
        for surface in [
            Color32::BLACK,
            Color32::WHITE,
            Color32::from_rgb(30, 30, 30),
            Color32::from_rgb(200, 200, 200),
        ] {
            let (hover, press) = (hovered(surface), pressed(surface));
            let (from_surface, from_hover) = (
                (luminance(hover) - luminance(surface)).abs(),
                (luminance(press) - luminance(surface)).abs(),
            );
            assert!(
                from_surface > 0.0,
                "the pointer landing on {surface:?} changed nothing"
            );
            assert!(
                from_hover > from_surface,
                "pressing {surface:?} came back nearer the surface than hovering it"
            );
        }
    }

    /// The ratio is the standard one, so the numbers in the comments mean what
    /// they say elsewhere.
    #[test]
    fn the_contrast_ratio_is_the_one_everybody_elses_numbers_are_in() {
        let extreme = contrast(Color32::BLACK, Color32::WHITE);
        assert!((extreme - 21.0).abs() < 0.01, "{extreme}");
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.001);
    }
}
