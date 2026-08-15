//! The front page: what this device serves, and what `Open` does about it.
//!
//! ## Structured against the reference
//!
//! The Spec names the Steam client as this client's reference shape, and its
//! library page is **master–detail**: a fixed rail of everything you have, and
//! a pane that says everything about the one you picked, with a single loud
//! primary action and a strip of facts under it. What was taken is that
//! composition and its proportions — a rail a fifth of the window, rows a
//! notch under two body-heights, a section rhythm between blocks — and not the
//! pixels, which are drawn at a 16px body against this client's 13.
//!
//! What was *not* taken is everything the reference fills the pane with that we
//! would have to invent: hero art, a play-time figure, an achievement bar, a
//! friends-who-play strip. A World has no capsule image and this client has no
//! honest number to put in a stat block, and a pane padded out with
//! plausible-looking figures is the exact defect the rest of this interface is
//! written to avoid. The pane says the four things that are true.
//!
//! ## Selecting is not choosing
//!
//! Picking a row reads nothing, places nothing and starts nothing — it moves
//! which of the facts already in hand are drawn. That is what keeps the rule
//! intact: listing is passive, and `Open` is the act. It is also why the
//! selection lives in [`Draft`] beside the surface rather than in the model.

use egui::{Align, Layout, Margin, RichText, Ui, UiBuilder};

use crate::client::library::{LibraryEntry, Opens, Placement};
use crate::model::App;
use crate::runtime::Action;

use super::{act_primary, geometry, theme};

/// Which row the pane is about.
///
/// A row key rather than an index: the library is re-read on every refresh and
/// an index would silently follow whatever moved into that position. When the
/// key names a row that is no longer served, the pane falls back to the first
/// row rather than emptying — a selection that vanished is not a reason to show
/// nothing.
#[derive(Debug, Default)]
pub struct Draft {
    pub selected: Option<String>,
}

/// A row's identity, which is the Orbit it belongs to plus the World in it.
///
/// An Orbit alone is not enough — one Orbit can serve several Worlds, and they
/// are separate rows.
fn key(entry: &LibraryEntry) -> String {
    format!("{}/{}", entry.orbit, entry.world_mount)
}

pub fn draw(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Library");
    ui.label(
        RichText::new("What this device serves. Open hands one to your browser.")
            .color(theme::prose(ui)),
    );
    ui.add_space(geometry::gap::section());

    let Some(entries) = app.library() else {
        ui.label(RichText::new("Loading…").italics());
        return;
    };

    if entries.is_empty() {
        // Only sayable because the read succeeded and answered nothing. The
        // loading case above is what stops this line from being drawn at a
        // machine that has simply not been asked yet.
        ui.label("This device serves no Worlds yet.");
        // And the way out is named rather than left to be found. A person with
        // a fresh install and an invite in hand is exactly who is looking at
        // this line, and the flow they need cannot live in a World's head.
        ui.label(
            RichText::new("Found a Space, or enter one from an invite, on the Spaces tab.")
                .color(theme::prose(ui)),
        );
        return;
    }

    // Resolved rather than corrected. Writing the fallback back into the draft
    // would turn "nothing is selected yet" into "the first row was chosen",
    // and the next refresh would keep a choice the person never made.
    let showing = draft
        .selected
        .as_ref()
        .and_then(|chosen| entries.iter().find(|entry| &key(entry) == chosen))
        .unwrap_or(&entries[0]);
    let showing = showing.clone();

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(geometry::rail::WIDTH, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| rail(ui, entries, &showing, draft),
        );
        ui.add_space(geometry::gap::section());
        ui.vertical(|ui| detail(ui, app, &showing, actions));
    });
}

/// The rail: every row, and which one the pane is about.
fn rail(ui: &mut Ui, entries: &[LibraryEntry], showing: &LibraryEntry, draft: &mut Draft) {
    // The rail's own measurements, for this scope only — a list row is denser
    // than a control and rounder than a button, which is the same distinction
    // the header's nav pills make and the same rung the tracker's own system
    // documents for "the sidebar's nav items".
    ui.spacing_mut().item_spacing.y = geometry::gap::tight();
    ui.spacing_mut().interact_size.y = geometry::control::md();
    ui.spacing_mut().button_padding = egui::vec2(geometry::gap::row(), 0.0);
    {
        let widgets = &mut ui.visuals_mut().widgets;
        for widget in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
        ] {
            widget.corner_radius = geometry::radius::row();
        }
    }

    // Justified, so a row's fill spans the rail and its label still starts at
    // the left. A selectable that sized itself to its own text would give a
    // ragged column of highlights, each as wide as the name in it.
    ui.scope_builder(
        UiBuilder::new().layout(Layout::top_down_justified(Align::LEFT)),
        |ui| {
            for entry in entries {
                let chosen = key(entry) == key(showing);
                // What a row's label sits on is the accent when it is the
                // chosen row and the page when it is not — and a colour held to
                // a floor against the wrong one of those is precisely the
                // colour that disappears. `theme::secondary` measures against
                // the page by construction, so the chosen row asks for the
                // same hue against what is actually behind it.
                let behind = if chosen {
                    theme::accent(ui)
                } else {
                    ui.visuals().panel_fill
                };
                let ink = theme::legible(
                    match entry.placement {
                        // Dimmed when the Orbit is not up, which is the
                        // reference's own device for "you have this but it is
                        // not installed" — and it costs no colour the theme did
                        // not already answer.
                        Placement::Vacant => ui.visuals().weak_text_color(),
                        Placement::Unknown => ui.visuals().warn_fg_color,
                        Placement::Placed => ui.visuals().text_color(),
                    },
                    behind,
                );
                // `selectable_label` and not a painted row: it carries the
                // chosen state into the semantic tree as the Toggle pattern,
                // which is what a screen reader announces. A hand-rolled row
                // would look identical and say nothing.
                let label = RichText::new(name_of(entry)).color(ink);
                if ui.selectable_label(chosen, label).clicked() {
                    draft.selected = Some(key(entry));
                }
            }
        },
    );
}

/// The pane: one row, its facts, and the one act on this page.
fn detail(ui: &mut Ui, app: &App, showing: &LibraryEntry, actions: &mut Vec<Action>) {
    egui::Frame::NONE
        .fill(theme::raised(ui))
        .corner_radius(geometry::radius::surface())
        .inner_margin(Margin::same(geometry::pad::card()))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.heading(name_of(showing));
                    ui.label(RichText::new(subtitle(showing)).color(theme::prose(ui)));
                });
                // The act, held apart from the name and given the weight the
                // reference gives its own: one primary control per page, and
                // this is the page's.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    open(ui, app, showing, actions);
                });
            });

            ui.add_space(geometry::gap::section());
            facts(ui, app, showing);

            let serving = heads_for(app, &showing.orbit);
            if !serving.is_empty() {
                ui.add_space(geometry::gap::section());
                ui.label(
                    RichText::new("SERVED BY")
                        .small()
                        .color(theme::secondary(ui)),
                );
                ui.add_space(geometry::gap::tight());
                for head in serving {
                    // Where, and not how to get in. It is what a person wants
                    // in the second after clicking Open — "where did that go" —
                    // and the Heads surface is where the rest of it lives.
                    ui.label(&head);
                }
            }
        });
}

/// The one act on this page.
fn open(ui: &mut Ui, app: &App, showing: &LibraryEntry, actions: &mut Vec<Action>) {
    // A World that declares no entry path cannot be opened, and the control
    // says so instead of being enabled and failing. A *Space* row is a
    // different case entirely and opens at its Orbit's own front door —
    // conflating the two is what made every row on a freshly started daemon
    // unopenable, because a vacant Orbit lists no Worlds at all.
    let entry_path = showing.opens.entry_path().map(str::to_owned);
    let orbit = showing.orbit.clone();
    let opened = act_primary(
        ui,
        app,
        "Open",
        entry_path.is_some(),
        refusal(&showing.opens),
        || Action::OpenWorld {
            orbit: orbit.clone(),
            entry_path: entry_path.clone().unwrap_or_default(),
        },
    );
    if let Some(action) = opened {
        actions.push(action);
    }
}

/// The strip: four things that are true, each under what it is.
fn facts(ui: &mut Ui, app: &App, showing: &LibraryEntry) {
    let registered = app.context().and_then(|context| {
        context
            .orbits
            .iter()
            .find(|orbit| orbit.space == showing.orbit)
    });
    let opened = registered.map_or_else(
        // Unmeasured, not never. A row whose registry entry could not be read
        // has no last-opened *reading*, which is not the same fact as one that
        // has never been opened.
        || "not read".to_owned(),
        |orbit| ago(orbit.last_opened, mechanics::wallclock::now_secs()),
    );
    let store = registered.map_or("not read", |orbit| orbit.path.as_str());

    ui.horizontal_top(|ui| {
        fact(ui, "STATE", placement_text(showing.placement), {
            match showing.placement {
                Placement::Unknown => Some(theme::attention(ui)),
                _ => None,
            }
        });
        ui.add_space(geometry::gap::section());
        fact(ui, "LAST OPENED", &opened, None);
        ui.add_space(geometry::gap::section());
        fact(
            ui,
            "OPENS AT",
            showing.opens.entry_path().unwrap_or("nowhere"),
            None,
        );
        ui.add_space(geometry::gap::section());
        fact(ui, "STORE", store, None);
    });
}

/// One fact: what it is, and what it says.
fn fact(ui: &mut Ui, what: &str, value: &str, colour: Option<egui::Color32>) {
    ui.vertical(|ui| {
        ui.label(RichText::new(what).small().color(theme::secondary(ui)));
        ui.label(RichText::new(value).color(colour.unwrap_or_else(|| ui.visuals().text_color())));
    });
}

/// Every head this client knows of that answers for `orbit`.
///
/// A browser head is bound to an identity and serves every Orbit that identity
/// has, so it counts for all of them; an MCP head is authored against one.
fn heads_for(app: &App, orbit: &str) -> Vec<String> {
    app.heads()
        .iter()
        .filter(|head| head.orbit.as_deref().is_none_or(|bound| bound == orbit))
        .filter_map(|head| head.url.as_deref().map(|url| origin(url).to_owned()))
        .collect()
}

/// A head's address, without the credential in its query.
///
/// A head URL carries a run credential — that is what makes it openable — and
/// the front page is not where a person needs it: `Open` mints a single-use
/// ticket of its own for exactly this reason. What is worth saying here is
/// *where* the Orbit is being served. The Heads surface is where the whole URL
/// is handed over, deliberately and once.
fn origin(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// A row nobody named is drawn as unnamed. Substituting the id would put
/// something in the name column that is not a name, and a person cannot tell
/// that apart from a World genuinely called that.
fn name_of(entry: &LibraryEntry) -> String {
    entry
        .display_name
        .clone()
        .unwrap_or_else(|| "Unnamed Space".to_owned())
}

/// The line under the name: what kind of row this is, and where it lives.
fn subtitle(entry: &LibraryEntry) -> String {
    if entry.world_mount.is_empty() {
        format!("A Space in {}", entry.space)
    } else {
        format!("{} in {}", entry.world_mount, entry.space)
    }
}

/// Why a row cannot be opened — two different facts, spelled as two.
const fn refusal(opens: &Opens) -> &'static str {
    match opens {
        Opens::Unhosted => "This build hosts no head for that World, so there is nothing to open.",
        Opens::Undeclared => "This World has not declared where to open it.",
        Opens::Declared(_) | Opens::Front => "",
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
        Placement::Unknown => "could not ask",
    }
}

/// How long ago, at the scale a person reads.
///
/// Zero is *never*, not the epoch: the registry writes a timestamp when an
/// Orbit is opened, so a zero is an Orbit nobody has opened yet — and "1 Jan
/// 1970" is the kind of answer that makes a person distrust every other figure
/// on the page. A clock that has gone backwards says so rather than reporting a
/// gigantic age.
fn ago(then_secs: u64, now_secs: u64) -> String {
    if then_secs == 0 {
        return "never".to_owned();
    }
    let Some(elapsed) = now_secs
        .checked_sub(then_secs)
        .filter(|_| now_secs >= then_secs)
    else {
        return "in the future".to_owned();
    };
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match elapsed {
        0..MINUTE => "just now".to_owned(),
        MINUTE..HOUR => plural(elapsed / MINUTE, "minute"),
        HOUR..DAY => plural(elapsed / HOUR, "hour"),
        _ => plural(elapsed / DAY, "day"),
    }
}

fn plural(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An age a person reads, and the two edges that are worth spelling out: a
    /// registry that has never recorded an opening, and a clock that disagrees
    /// with the one that wrote the timestamp.
    #[test]
    fn an_age_is_rendered_at_a_scale_a_person_reads() {
        const NOW: u64 = 1_000_000;
        assert_eq!(ago(0, NOW), "never");
        assert_eq!(ago(NOW, NOW), "just now");
        assert_eq!(ago(NOW - 59, NOW), "just now");
        assert_eq!(ago(NOW - 60, NOW), "1 minute ago");
        assert_eq!(ago(NOW - 3_600, NOW), "1 hour ago");
        assert_eq!(ago(NOW - 7_200, NOW), "2 hours ago");
        assert_eq!(ago(NOW - 86_400 * 3, NOW), "3 days ago");
        assert_eq!(
            ago(NOW + 500, NOW),
            "in the future",
            "a timestamp ahead of this clock was rendered as an age"
        );
    }

    /// The front page says where a head is, not how to get into it. A head URL
    /// carries a run credential, and this page has no use for one — `Open`
    /// mints its own, single-use and Orbit-scoped, which is the whole reason
    /// that ceremony exists.
    #[test]
    fn a_heads_address_is_shown_without_its_credential() {
        assert_eq!(
            origin("http://127.0.0.1:52713/?token=secret"),
            "http://127.0.0.1:52713/"
        );
        assert_eq!(origin("http://127.0.0.1:7717/"), "http://127.0.0.1:7717/");
        assert!(
            !origin("http://127.0.0.1:52713/?token=secret").contains("secret"),
            "a run credential reached the front page"
        );
    }

    /// The two reasons a row cannot be opened are different sentences, because
    /// they are different facts and only one of them is about the World.
    #[test]
    fn a_row_that_cannot_be_opened_says_which_kind_of_cannot() {
        assert_ne!(refusal(&Opens::Unhosted), refusal(&Opens::Undeclared));
        assert!(refusal(&Opens::Front).is_empty());
        assert!(refusal(&Opens::Declared("/".into())).is_empty());
    }
}
