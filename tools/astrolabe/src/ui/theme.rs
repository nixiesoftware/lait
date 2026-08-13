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

/// The contrast an accent must clear against what is behind it.
///
/// WCAG's large-text and non-text floor. These are short coloured labels beside
/// ordinary text, which is what that threshold is for; holding them to the 4.5∶1
/// body-text ratio would push a scheme's own colours around for no gain a person
/// would notice.
pub const MINIMUM_CONTRAST: f64 = 3.0;

/// Something the person should notice but that is not an error — a degraded
/// observation, a stale figure.
pub fn attention(ui: &Ui) -> Color32 {
    legible(ui.visuals().warn_fg_color, behind(ui))
}

/// Something went wrong.
pub fn danger(ui: &Ui) -> Color32 {
    legible(ui.visuals().error_fg_color, behind(ui))
}

/// Text that is present but secondary. Asked for rather than dimmed by a fixed
/// alpha, because "70% of the foreground" is illegible in exactly the scheme
/// that needs it most — and then held to the same floor, because a weak colour
/// that cannot be read is not weak, it is absent.
pub fn secondary(ui: &Ui) -> Color32 {
    legible(ui.visuals().weak_text_color(), behind(ui))
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
    if contrast(colour, background) >= MINIMUM_CONTRAST {
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
        if contrast(candidate, background) >= MINIMUM_CONTRAST {
            return candidate;
        }
    }
    toward
}

/// The WCAG contrast ratio: 1.0 for identical, 21.0 for black on white.
pub fn contrast(foreground: Color32, background: Color32) -> f64 {
    let (a, b) = (luminance(foreground), luminance(background));
    let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
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

    /// The ratio is the standard one, so the numbers in the comments mean what
    /// they say elsewhere.
    #[test]
    fn the_contrast_ratio_is_the_one_everybody_elses_numbers_are_in() {
        let extreme = contrast(Color32::BLACK, Color32::WHITE);
        assert!((extreme - 21.0).abs() < 0.01, "{extreme}");
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.001);
    }
}
