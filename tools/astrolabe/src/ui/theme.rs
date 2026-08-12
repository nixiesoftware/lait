//! Colour that survives a theme it did not choose.
//!
//! Light, dark and *high contrast* are honoured rather than approximated. The
//! third is the one naive implementations lose: a hard-coded amber that reads
//! fine on both light and dark backgrounds is invisible against a high-contrast
//! scheme the person selected precisely because they cannot see it otherwise.
//!
//! So nothing here returns a fixed colour. Every accent is asked of the visuals
//! in force, which is what the platform theme actually reaches.

use egui::{Color32, Ui};

/// Something the person should notice but that is not an error — a degraded
/// observation, a stale figure.
pub fn attention(ui: &Ui) -> Color32 {
    ui.visuals().warn_fg_color
}

/// Something went wrong.
pub fn danger(ui: &Ui) -> Color32 {
    ui.visuals().error_fg_color
}

/// Text that is present but secondary. Asked for rather than dimmed by a fixed
/// alpha, because "70% of the foreground" is illegible in exactly the scheme
/// that needs it most.
pub fn secondary(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
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
}
