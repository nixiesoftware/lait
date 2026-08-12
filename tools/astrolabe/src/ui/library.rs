//! The front page: what this device serves, and what `Open` does about it.

use egui::{RichText, Ui};

use crate::client::library::Placement;
use crate::model::App;

use super::theme;

pub fn draw(ui: &mut Ui, app: &App) {
    ui.heading("Library");

    let Some(entries) = app.library() else {
        ui.label(RichText::new("Loading…").italics());
        return;
    };

    if entries.is_empty() {
        // Only sayable because the read succeeded and answered nothing. The
        // loading case above is what stops this line from being drawn at a
        // machine that has simply not been asked yet.
        ui.label("This device serves no Worlds yet.");
        return;
    }

    for entry in entries {
        ui.horizontal(|ui| {
            // A row nobody named is drawn as unnamed. Substituting the id would
            // put something in the name column that is not a name, and a person
            // cannot tell that apart from a World genuinely called that.
            match &entry.display_name {
                Some(name) => ui.label(RichText::new(name).strong()),
                None => ui.label(RichText::new("Unnamed Space").italics()),
            };
            ui.label(RichText::new(&entry.world_mount).color(theme::secondary(ui)));
            ui.label(
                RichText::new(placement_text(entry.placement)).color(match entry.placement {
                    Placement::Unknown => theme::attention(ui),
                    _ => theme::secondary(ui),
                }),
            );

            // A World that declares no entry path cannot be opened, and the
            // control says so instead of being enabled and failing. `/` is not
            // a guess worth making on somebody's behalf.
            let openable = entry.entry_path.is_some();
            ui.add_enabled(openable, egui::Button::new("Open"))
                .on_disabled_hover_text("This World declares no entry path yet (SUB-2).");
        });
    }
}

/// Three states, spelled as three things.
///
/// "Not running" and "could not ask" are different facts, and a Library that
/// prints the first when it means the second is the false-disconnection defect
/// in its mildest and most common form.
const fn placement_text(placement: Placement) -> &'static str {
    match placement {
        Placement::Placed => "running",
        Placement::Vacant => "not running",
        Placement::Unknown => "unknown",
    }
}
