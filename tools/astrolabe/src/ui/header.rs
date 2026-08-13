//! The bar across the top: where you are, and the one control that belongs
//! beside it.
//!
//! Proportioned against the Steam redesign the Spec already names as this
//! client's reference shape. What was taken is the *composition* — a full-bleed
//! bar carrying primary navigation as pills, with a right cluster held apart
//! from it — and the *ratios*, not the pixels: that design runs at a 16px body
//! and this client runs at 13, so copying its 57/45/29 outright would give a
//! dense client a marketing header.
//!
//! | | reference | here |
//! |---|---|---|
//! | bar | 57 | `bar::lg()` 44 |
//! | item | 45 | `control::lg()` 32 |
//! | item ÷ bar | 0.79 | 0.73 |
//! | h-padding ÷ text | 1.8× | 1.4× |
//!
//! The one thing deliberately *not* carried over is a search field. The
//! reference has one in the middle and this client has nothing to search yet;
//! a box that looked like a control and did nothing would be worse than the
//! space it fills.
//!
//! Nor is there an account chip, for a sharper reason: there is nothing honest
//! to put in it. `HostContext` answers an identity *home* — a path — and a
//! person's name lives in a Space's `whoami`, which is per-Space and unread
//! until one is chosen. A chip showing the last path segment of a config
//! directory is not an identity, and inventing one here would be the same
//! defect as a synthesised figure wearing different clothes.

use egui::{Align, Frame, Layout, Margin, Stroke, Ui};

use crate::model::App;
use crate::runtime::Action;

use super::{geometry, theme, Chrome, Surface};

/// Draw the bar, full bleed.
///
/// Full bleed is the point: a header inset by the page margin reads as a row of
/// controls that happen to be at the top, and one that reaches the window edge
/// reads as chrome. Its *contents* are inset to the page margin so the first
/// nav item lines up with the first word of every surface below it.
pub fn draw(ui: &mut Ui, app: &App, chrome: &mut Chrome, actions: &mut Vec<Action>) {
    let page = geometry::page_margin();
    // Allocated at an explicit size rather than grown from its contents. A bar
    // that only sets a *minimum* height takes whatever the window has left,
    // which is the whole window — and its contents then centre themselves in
    // it, which looks exactly like the layout having no opinion at all.
    let strip = egui::vec2(
        ui.available_width() - f32::from(page.left + page.right),
        geometry::bar::lg(),
    );
    Frame::NONE
        .fill(theme::raised(ui))
        .stroke(Stroke::NONE)
        .inner_margin(Margin {
            left: page.left,
            right: page.right,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            // The pill's own measurements, set for this scope only. Applying
            // them to the whole style would make every button in the client a
            // navigation pill.
            ui.spacing_mut().button_padding = egui::vec2(geometry::nav::PADDING, 0.0);
            ui.spacing_mut().item_spacing.x = geometry::nav::GAP;
            ui.spacing_mut().interact_size.y = geometry::control::lg();
            // The *row* role, not the control one. That is the tracker's own
            // rule — its `--radius-row` is documented as "the sidebar's nav
            // items, the settings tabs" — and it is what the reference does
            // too: a corner of 3 on a 45-tall item, where a control's roundness
            // would read as a lozenge.
            {
                let widgets = &mut ui.visuals_mut().widgets;
                for widget in [
                    &mut widgets.inactive,
                    &mut widgets.hovered,
                    &mut widgets.active,
                    &mut widgets.noninteractive,
                ] {
                    widget.corner_radius = geometry::radius::row();
                }
            }

            ui.allocate_ui_with_layout(strip, Layout::left_to_right(Align::Center), |ui| {
                for candidate in Surface::ALL {
                    // `selectable_value` and not a hand-rolled pill: it carries
                    // the selected state into the semantic tree as the Toggle
                    // pattern, which `accesskit_windows` implements and a screen
                    // reader therefore reads out. A custom widget would look the
                    // same and announce nothing.
                    ui.selectable_value(&mut chrome.surface, candidate, candidate.title());
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Enabled while a re-read is in flight would let a person
                    // queue six of them by clicking a control that looked idle.
                    let waiting = app.is_in_flight(&Action::Refresh.key());
                    if ui
                        .add_enabled(!waiting, egui::Button::new("Refresh"))
                        .on_hover_text("Read this machine again (F5)")
                        .clicked()
                    {
                        actions.push(Action::Refresh);
                    }
                });
            });
        });

    // A hairline rather than egui's separator, which is a full-width line with
    // its own vertical spacing either side — three times the weight this needs
    // and enough to read as a divider between two regions rather than as the
    // bottom edge of one.
    let edge = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter()
        .hline(edge.x_range(), y, Stroke::new(1.0, theme::hairline(ui)));
}
