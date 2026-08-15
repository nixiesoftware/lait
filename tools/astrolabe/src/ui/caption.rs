//! The window's own controls: minimise, maximise, close — and the bar itself,
//! which is what you drag.
//!
//! The client asks for a window with no decorations, so nothing draws these but
//! this file. That is a trade rather than a preference. What is given up is real
//! — the system menu on `Alt+Space`, and Windows 11's snap-layout flyout, which
//! appears only for a window that answers `WM_NCHITTEST` with `HTMAXBUTTON` and
//! winit does not offer that. What is bought is that the top of the window is
//! *one* surface: a native caption above a client bar means two bars, two
//! colours the theme did not agree on, and a title bar that stays light while
//! the client under it goes dark.
//!
//! ## These are asks, not calls
//!
//! Drawing returns what it was asked for and calls nothing — the rule the whole
//! interface is built on — and the window is no exception. A caption control
//! records an [`Ask`]; the shell carries it out with [`carry`]. That keeps the
//! close button testable without a window (press it, read the ask) and keeps
//! *what closing means* in one place: this client's close minimises to the tray,
//! and it would be two policies the moment a button sent its own command.
//!
//! ## The marks are painted, not typed
//!
//! Windows draws these glyphs from Segoe Fluent Icons, at private-use code
//! points egui's default font does not carry, and the Unicode near-misses
//! (`─ ☐ ✕`) are a different weight from each other in every font that has all
//! three. Four line segments and two rectangles are fewer moving parts than a
//! font fallback chain, and they are crisp at any density.

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo};
use egui::{ViewportCommand, WidgetType};

use super::{geometry, theme};

/// What the caption asked the shell to do to the window.
///
/// Deliberately about the *window* and nothing else: none of these reaches the
/// client, the daemon or the network, which is why they travel beside
/// [`crate::runtime::Action`] rather than as one of its variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Out of the way, into the taskbar.
    Minimise,
    /// Fill the work area, or come back off it. Carries the state wanted rather
    /// than "toggle", so what was drawn and what is carried out came from the
    /// same reading of the same frame.
    Maximise(bool),
    /// Move the window with the pointer until the button comes up.
    ///
    /// Handed to the platform rather than implemented, so dragging a maximised
    /// window restores it, dragging to a screen edge snaps it, and both do
    /// whatever the person's own settings say they do.
    Move,
    /// Close — which this client answers by minimising to the tray, because a
    /// person who clicked the wrong X did not ask their Spaces to stop
    /// converging. That decision lives in the shell, and this ask is what lets
    /// it stay there.
    Close,
}

/// Carry out what the caption asked for.
///
/// The other half of [`draw`], kept beside it: an ask and the command it becomes
/// are one decision, and splitting them across files is how the two drift.
pub fn carry(ctx: &egui::Context, asks: &[Ask]) {
    for ask in asks {
        ctx.send_viewport_cmd(match *ask {
            Ask::Minimise => ViewportCommand::Minimized(true),
            Ask::Maximise(wanted) => ViewportCommand::Maximized(wanted),
            Ask::Move => ViewportCommand::StartDrag,
            Ask::Close => ViewportCommand::Close,
        });
    }
}

/// The three controls, right to left from the window's corner.
///
/// `ui` is expected to be a right-to-left strip the height of the bar and
/// [`geometry::caption::span`] wide, flush with the window's right edge — the
/// corner is the target, so nothing may sit between these and it.
pub fn draw(ui: &mut Ui, asks: &mut Vec<Ask>) {
    // Asked of the platform rather than remembered, because the window can be
    // maximised by a route this client never sees — a double-click on the bar,
    // `Win`+`↑`, a snap gesture, another program. A remembered flag would then
    // draw the wrong mark and the control would do the wrong thing once.
    //
    // `None` is *unmeasured*, not "not maximised": a surface rendered offscreen
    // has no window to ask. It draws the maximise mark because that is the state
    // a window is in unless something says otherwise, and the ask it sends says
    // so explicitly rather than negating something it never read.
    let maximised = ui
        .input(|input| input.viewport().maximized)
        .unwrap_or(false);

    if control(
        ui,
        Mark::Close,
        "Close",
        "Close (it keeps serving in the tray)",
    ) {
        asks.push(Ask::Close);
    }
    let restore = if maximised {
        ("Restore", "Restore the window to its previous size")
    } else {
        ("Maximise", "Fill the screen")
    };
    if control(ui, Mark::Maximise { maximised }, restore.0, restore.1) {
        asks.push(Ask::Maximise(!maximised));
    }
    if control(ui, Mark::Minimise, "Minimise", "Out of the way") {
        asks.push(Ask::Minimise);
    }
}

/// Was the bar itself dragged, or double-clicked?
///
/// Separate from [`draw`] because it is about the whole bar and has to be
/// registered *before* the controls that sit on it — egui hit-tests the last
/// widget that claimed a point, so a bar-wide sense registered afterwards would
/// swallow every click on every nav item in it.
pub fn bar(ui: &Ui, within: Rect, asks: &mut Vec<Ask>) {
    let response = ui.interact(within, ui.id().with("caption-bar"), Sense::click_and_drag());
    let maximised = ui
        .input(|input| input.viewport().maximized)
        .unwrap_or(false);

    if response.double_clicked() {
        asks.push(Ask::Maximise(!maximised));
        return;
    }
    // `drag_started`, not "the button is down": handing the platform a move loop
    // the instant a button went down would eat the second half of every
    // double-click, because the loop is modal and this process stops seeing the
    // pointer for as long as it runs. Waiting for the pointer to actually travel
    // costs a few points of lag at the start of a drag and is what every other
    // custom-chrome client does.
    if response.drag_started_by(egui::PointerButton::Primary) {
        asks.push(Ask::Move);
    }
}

/// Which mark a control wears.
#[derive(Clone, Copy)]
enum Mark {
    Minimise,
    Maximise { maximised: bool },
    Close,
}

/// One control: the platform's width, the bar's full height, and a mark.
///
/// Returns whether it was clicked. Built out of an allocated rect rather than an
/// `egui::Button` because a button carries the ladder's padding, corner and fill
/// — everything that makes it a *control on a page* — and a caption control is
/// none of those things: it is a full-height slab that lights up under the
/// pointer, which is what makes the corner clickable.
fn control(ui: &mut Ui, mark: Mark, name: &str, hint: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(geometry::caption::WIDTH, ui.max_rect().height()),
        Sense::click(),
    );
    // Without this the tree has a clickable node with no name, which is exactly
    // what a screen reader reads out: "button". Every control here is a real
    // widget to Windows UI Automation, as the platform's own caption buttons are.
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), name));

    let surface = theme::raised(ui);
    let danger = matches!(mark, Mark::Close);
    let fill = if response.is_pointer_button_down_on() {
        Some(if danger {
            theme::pressed(theme::danger_fill(ui))
        } else {
            theme::pressed(surface)
        })
    } else if response.hovered() {
        Some(if danger {
            theme::danger_fill(ui)
        } else {
            theme::hovered(surface)
        })
    } else {
        None
    };

    let painter = ui.painter();
    // Square, and to the edge. A rounded fill would leave the window's own
    // corner unpainted at exactly the pixel a maximised window is aimed at.
    if let Some(fill) = fill {
        painter.rect_filled(rect, CornerRadius::ZERO, fill);
    }
    // Held to the label floor against whatever is actually behind it, which on
    // the close control's hover is a saturated red and not the bar at all.
    let ink = theme::legible(ui.visuals().text_color(), fill.unwrap_or(surface));
    paint(painter, rect, mark, ink, ui.pixels_per_point());

    let response = response.on_hover_text(hint);
    response.clicked()
}

/// The marks themselves, on the pixel grid.
///
/// Rounded to pixel centres because these are one-pixel lines: half a pixel out
/// and a crisp hairline becomes two grey ones, which at this size is the
/// difference between a system glyph and a smudge.
fn paint(painter: &egui::Painter, within: Rect, mark: Mark, ink: Color32, density: f32) {
    use egui::emath::GuiRounding as _;

    let stroke = Stroke::new(geometry::caption::HAIRLINE, ink);
    let box_ = Rect::from_center_size(within.center(), Vec2::splat(geometry::caption::MARK))
        .round_to_pixel_center(density);

    match mark {
        Mark::Minimise => {
            let y = box_.center().y.round_to_pixel_center(density);
            painter.hline(box_.x_range(), y, stroke);
        }
        Mark::Maximise { maximised: false } => {
            painter.rect_stroke(box_, CornerRadius::ZERO, stroke, StrokeKind::Inside);
        }
        Mark::Maximise { maximised: true } => {
            // Two sheets: the one in front, and the corner of the one behind it
            // showing past its top-right. Drawn as two segments rather than a
            // second rectangle so nothing has to be filled — a filled backing
            // sheet would have to know the hover colour under it.
            let offset = geometry::caption::HAIRLINE * 3.0;
            let front = Rect::from_min_max(
                egui::pos2(box_.left(), box_.top() + offset),
                egui::pos2(box_.right() - offset, box_.bottom()),
            )
            .round_to_pixel_center(density);
            painter.rect_stroke(front, CornerRadius::ZERO, stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    egui::pos2(front.left() + offset, box_.top()),
                    egui::pos2(box_.right(), box_.top()),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(box_.right(), box_.top()),
                    egui::pos2(box_.right(), front.bottom() - offset),
                ],
                stroke,
            );
        }
        Mark::Close => {
            painter.line_segment([box_.left_top(), box_.right_bottom()], stroke);
            painter.line_segment([box_.right_top(), box_.left_bottom()], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every ask becomes a command, and the mapping is total. A variant added
    /// without an arm here would be a control that draws, announces itself,
    /// records an ask — and does nothing at all, which is the one failure this
    /// shape can still have.
    #[test]
    fn every_ask_reaches_the_window() {
        let ctx = egui::Context::default();
        let asks = [
            Ask::Minimise,
            Ask::Maximise(true),
            Ask::Maximise(false),
            Ask::Move,
            Ask::Close,
        ];
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        carry(&ctx, &asks);
        let output = ctx.run_ui(egui::RawInput::default(), |_| {});
        let commands = &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("the root viewport")
            .commands;

        assert_eq!(
            commands.len(),
            asks.len(),
            "{} asks became {} commands: {commands:?}",
            asks.len(),
            commands.len()
        );
        assert!(commands.contains(&ViewportCommand::Minimized(true)));
        assert!(commands.contains(&ViewportCommand::Maximized(true)));
        assert!(commands.contains(&ViewportCommand::Maximized(false)));
        assert!(commands.contains(&ViewportCommand::StartDrag));
        assert!(commands.contains(&ViewportCommand::Close));
    }

    /// Closing is the shell's decision, not the button's. If this ever became
    /// `ViewportCommand::Close`-plus-something, the tray policy would have two
    /// homes and the second one would win on the frame nobody tested.
    #[test]
    fn closing_asks_and_does_not_decide() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        carry(&ctx, &[Ask::Close]);
        let output = ctx.run_ui(egui::RawInput::default(), |_| {});
        assert_eq!(
            output.viewport_output[&egui::ViewportId::ROOT].commands,
            vec![ViewportCommand::Close],
            "the close control did more than ask to close"
        );
    }
}
