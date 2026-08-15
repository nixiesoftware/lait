//! The bar across the top: where you are, the one control that belongs beside
//! it, and the window's own three.
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
//!
//! ## This bar is the title bar
//!
//! The window has no decorations, so there is nothing above this — the caption
//! controls at its right end are drawn by [`super::caption`], and the bar itself
//! is what a person drags. That is why the order of what happens below is
//! load-bearing rather than incidental, and it is commented where it matters.

use egui::{Align, CornerRadius, Layout, Rect, Sense, Stroke, Ui, UiBuilder};

use crate::model::App;
use crate::runtime::Action;

use super::{caption, geometry, theme, Chrome, Surface};

/// Draw the bar, full bleed.
///
/// Full bleed is the point: a header inset by the page margin reads as a row of
/// controls that happen to be at the top, and one that reaches the window edge
/// reads as chrome. Its *contents* are inset to the page margin so the first
/// nav item lines up with the first word of every surface below it — but the
/// window controls are not, because the screen corner is their target.
pub fn draw(ui: &mut Ui, app: &App, chrome: &mut Chrome, actions: &mut Vec<Action>) {
    let page = geometry::page_margin();

    // Allocated at an explicit size rather than grown from its contents. A bar
    // that only sets a *minimum* height takes whatever the window has left,
    // which is the whole window — and its contents then centre themselves in
    // it, which looks exactly like the layout having no opinion at all.
    let available = ui.available_rect_before_wrap();
    let bar = Rect::from_min_size(
        available.min,
        egui::vec2(available.width(), geometry::bar::lg()),
    );
    ui.allocate_rect(bar, Sense::hover());
    ui.painter()
        .rect_filled(bar, CornerRadius::ZERO, theme::raised(ui));

    // The whole bar is the window's drag handle, and it is claimed *first*.
    // egui hit-tests the last widget to claim a point, so a bar-wide sense
    // registered after the pills would swallow every click on every one of
    // them — the tabs would still highlight, and nothing would ever change
    // surface.
    caption::bar(ui, bar, &mut chrome.window);

    // The window's own controls, flush with the corner. Drawn before the
    // navigation because the navigation is sized against what they leave.
    let corner = Rect::from_min_max(
        egui::pos2(bar.right() - geometry::caption::span(), bar.top()),
        bar.max,
    );
    // Both children are salted, because both are built from the same parent and
    // an unsalted child derives its widget ids from the parent's cursor. Two
    // strips of controls in one bar is exactly the shape that produces a silent
    // id clash — and a clash costs the *second* widget its interaction, so the
    // symptom is one control that draws correctly and does not respond.
    caption::draw(
        &mut ui.new_child(
            UiBuilder::new()
                .id_salt("caption")
                .max_rect(corner)
                .layout(Layout::right_to_left(Align::Center)),
        ),
        &mut chrome.window,
    );

    let content = Rect::from_min_max(
        egui::pos2(bar.left() + f32::from(page.left), bar.top()),
        egui::pos2(corner.left() - geometry::gap::row(), bar.bottom()),
    );
    let mut inner = ui.new_child(
        UiBuilder::new()
            .id_salt("navigation")
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Center)),
    );
    navigation(&mut inner, app, chrome, actions);

    // A hairline rather than egui's separator, which is a full-width line with
    // its own vertical spacing either side — three times the weight this needs
    // and enough to read as a divider between two regions rather than as the
    // bottom edge of one.
    ui.painter().hline(
        bar.x_range(),
        bar.bottom(),
        Stroke::new(1.0, theme::hairline(ui)),
    );
}

/// The surfaces, and the one action that belongs on the chrome rather than on a
/// page.
fn navigation(ui: &mut Ui, app: &App, chrome: &mut Chrome, actions: &mut Vec<Action>) {
    // The pill's own measurements, set for this scope only. Applying them to
    // the whole style would make every button in the client a navigation pill.
    ui.spacing_mut().button_padding = egui::vec2(geometry::nav::PADDING, 0.0);
    ui.spacing_mut().item_spacing.x = geometry::nav::GAP;
    ui.spacing_mut().interact_size.y = geometry::control::lg();
    // The *row* role, not the control one. That is the tracker's own rule — its
    // `--radius-row` is documented as "the sidebar's nav items, the settings
    // tabs" — and it is what the reference does too: a corner of 3 on a 45-tall
    // item, where a control's roundness would read as a lozenge.
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

    for candidate in Surface::ALL {
        // `selectable_value` and not a hand-rolled pill: it carries the
        // selected state into the semantic tree as the Toggle pattern, which
        // `accesskit_windows` implements and a screen reader therefore reads
        // out. A custom widget would look the same and announce nothing.
        ui.selectable_value(&mut chrome.surface, candidate, candidate.title());
    }

    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        // Enabled while a re-read is in flight would let a person queue six of
        // them by clicking a control that looked idle.
        let waiting = app.is_in_flight(&Action::Refresh.key());
        if ui
            .add_enabled(!waiting, egui::Button::new("Refresh"))
            .on_hover_text("Read this machine again (F5)")
            .clicked()
        {
            actions.push(Action::Refresh);
        }
    });
}
