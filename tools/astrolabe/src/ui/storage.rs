//! What each Space is holding, and what is moving.
//!
//! Every figure here is read from the engine or drawn as absent. There is no
//! third option: an estimate that makes the surface look populated is the
//! observation-failure defect wearing different clothes, and it is harder to
//! spot because it looks like data.
//!
//! An absence carries its reason. "Not up" and "nobody could ask" are different
//! facts about a Space, and only one of them is worth doing anything about — a
//! surface that printed one sentence for both would be honest about the number
//! and misleading about the machine.

use egui::{ProgressBar, RichText, Ui};

use crate::client::storage::{Direction, Missing, StorageFacts, TransferFacts};

use super::theme;

/// Draw storage and transfers.
///
/// Takes the facts rather than reaching for them: this is a surface, and a
/// surface that fetched would be doing work on the frame thread and would be
/// untestable without a daemon.
pub fn draw(ui: &mut Ui, storage: &[StorageFacts], transfers: &[TransferFacts]) {
    ui.heading("Storage");

    if storage.is_empty() {
        ui.label("No Spaces on this device.");
    }
    for facts in storage {
        ui.horizontal(|ui| {
            // The registry's name where there is one. Advisory — a Space's
            // display name is owned by a World today (SUB-1) — so an unnamed
            // row says so rather than wearing its id as a name.
            match &facts.name {
                Some(name) => ui.label(RichText::new(name).strong()),
                None => ui.label(RichText::new("Unnamed Space").italics()),
            };
            ui.label(
                RichText::new(&facts.orbit)
                    .small()
                    .color(theme::secondary(ui)),
            );
            ui.label(
                RichText::new(footprint(facts)).color(if facts.is_measured() {
                    theme::secondary(ui)
                } else {
                    // Not an error and not a zero: nobody has measured it. Drawn in
                    // the attention colour so it reads as a gap rather than as a
                    // small number.
                    theme::attention(ui)
                }),
            );
            ui.label(RichText::new(verified(facts)).color(theme::secondary(ui)));
        });
    }

    ui.separator();
    ui.heading("Transfers");
    if transfers.is_empty() {
        // Hedged twice over, because both hedges are true. Nothing here can
        // tell "no transfers" from "no producer has ever fed this lane" — and
        // today it is the second: the progress lane is plumbed end to end and
        // nothing in the engine feeds it (SUB-3). Saying so beats an empty
        // panel that reads as a quiet machine.
        ui.label("No transfers observed.");
        ui.label(
            RichText::new(
                "Nothing in the engine reports transfer progress yet (SUB-3), so this \
                 stays empty even while a Space is converging.",
            )
            .small()
            .color(theme::secondary(ui)),
        );
    }
    for transfer in transfers {
        draw_transfer(ui, transfer);
    }
}

fn draw_transfer(ui: &mut Ui, transfer: &TransferFacts) {
    ui.horizontal(|ui| {
        ui.label(match transfer.direction {
            Direction::Incoming => "↓",
            Direction::Outgoing => "↑",
        });
        ui.label(RichText::new(&transfer.peer).strong());
        ui.label(RichText::new(&transfer.state).color(theme::secondary(ui)));

        match transfer.bytes_total {
            // A total nobody knows must never be drawn as a full bar, and
            // must not be drawn as an empty one either — both are claims about
            // a proportion that does not exist.
            None => {
                ui.label(
                    RichText::new(format!(
                        "{} so far, total unknown",
                        bytes(transfer.bytes_done)
                    ))
                    .color(theme::attention(ui)),
                );
            }
            Some(0) => {
                ui.label(RichText::new("nothing to transfer").color(theme::secondary(ui)));
            }
            Some(total) => {
                // Only computable because the total is known and non-zero,
                // which the arms above are what establish.
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a progress bar is a fraction of a pixel row; f32 is the widest \
                              precision anything downstream can use"
                )]
                let fraction = (transfer.bytes_done as f32) / (total as f32);
                ui.add(ProgressBar::new(fraction.clamp(0.0, 1.0)).show_percentage());
            }
        }
    });
}

/// `None` is spelled, never defaulted — and it says which kind of `None`.
fn footprint(facts: &StorageFacts) -> String {
    match (facts.bytes_on_disk, facts.object_count) {
        (Some(size), Some(count)) => format!("{} across {count} objects", bytes(size)),
        (Some(size), None) => format!("{}, object count not measured", bytes(size)),
        (None, Some(count)) => format!("{count} objects, footprint not measured"),
        (None, None) => match facts.missing {
            // Listing is passive: an Orbit that is not up was not woken to
            // produce a number, and saying so is the difference between an
            // empty row and a mysterious one.
            Some(Missing::NotPlaced) => "not measured — this Space is not running".to_owned(),
            Some(Missing::Unreachable) => "not measured — this Space could not be asked".to_owned(),
            None => "not measured".to_owned(),
        },
    }
}

fn verified(facts: &StorageFacts) -> String {
    match facts.last_verified_ms {
        // Never is a real answer and a different one from "a long time ago".
        None => "never verified".to_owned(),
        Some(at) => format!("verified at {at} ms"),
    }
}

/// Bytes, at a scale a person reads.
///
/// Deliberately blunt: this rounds, and rounding is fine because it is labelled
/// as a size rather than as an exact count. The figure it rounds is measured —
/// which is the property that actually matters here.
#[allow(
    clippy::cast_precision_loss,
    reason = "a size shown to a person is rounded to one decimal place; the               precision f64 loses above 2^53 bytes is nine petabytes past               anything the label could convey"
)]
fn bytes(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = size as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 {
        format!("{size:.0} {name}")
    } else {
        format!("{size:.1} {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule this surface exists to hold: unmeasured says so, and never
    /// borrows a zero to look like data.
    #[test]
    fn an_unmeasured_footprint_is_spelled_out_rather_than_shown_as_zero() {
        let unmeasured = StorageFacts::unmeasured("orb_one", Missing::NotPlaced);
        let drawn = footprint(&unmeasured);
        assert!(drawn.starts_with("not measured"), "{drawn}");
        assert!(
            !drawn.contains('0'),
            "an unmeasured footprint borrowed a zero: {drawn}"
        );
        assert_eq!(verified(&unmeasured), "never verified");
    }

    /// And which kind of unmeasured. A Space that is simply not running and one
    /// nobody could reach are different facts about the machine.
    #[test]
    fn an_absence_says_whether_anybody_could_have_asked() {
        let vacant = footprint(&StorageFacts::unmeasured("orb_one", Missing::NotPlaced));
        let unreachable = footprint(&StorageFacts::unmeasured("orb_one", Missing::Unreachable));
        assert_ne!(vacant, unreachable);
        assert!(vacant.contains("not running"), "{vacant}");
        assert!(unreachable.contains("could not be asked"), "{unreachable}");
    }

    /// Half-measured is its own state. Reporting a known footprint as unknown
    /// because the object count is missing throws away a figure somebody paid
    /// to measure.
    #[test]
    fn a_partly_measured_footprint_reports_what_it_knows() {
        let partial = StorageFacts {
            orbit: "orb_one".into(),
            name: Some("Work".into()),
            bytes_on_disk: Some(2_097_152),
            object_count: None,
            last_verified_ms: None,
            missing: None,
        };
        let drawn = footprint(&partial);
        assert!(drawn.contains("2.0 MB"), "{drawn}");
        assert!(drawn.contains("not measured"), "{drawn}");
    }

    #[test]
    fn sizes_are_rendered_at_a_scale_a_person_reads() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KB");
        assert_eq!(bytes(1_572_864), "1.5 MB");
    }
}
