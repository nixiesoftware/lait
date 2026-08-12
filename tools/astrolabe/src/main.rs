//! `astrolabe.exe`.
//!
//! `windows_subsystem = "windows"` is why a packaged launch opens no console.
//! It is an attribute rather than a linker flag in CI so that *every* build has
//! the property, including one somebody makes by hand — a release criterion
//! that only holds when the release runner is used is not a property of the
//! program.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use anyhow::{Context, Result};
use astrolabe::model::App;
use astrolabe::runtime::{Action, Runtime};
use astrolabe::tray::{Tray, TrayCommand};
use astrolabe::ui::{Chrome, Surface};
use astrolabe::Config;

fn main() -> Result<()> {
    tracing_subscriber_init();

    // Single instance, before anything is drawn: a second launch must hand its
    // work to the first and exit, not race it for the daemon and the state root.
    let _instance = match astrolabe::single_instance::acquire()? {
        astrolabe::single_instance::Outcome::Held(guard) => guard,
        astrolabe::single_instance::Outcome::AlreadyRunning => {
            tracing::info!("another Astrolabe is already running; leaving it to it");
            return Ok(());
        }
    };

    // A `lait:` link the shell handed us. Parsed here and carried in, because
    // parsing is the only thing that may happen to it before a person confirms:
    // opening the client is not accepting an invite.
    let arrived = astrolabe::link::Link::from_args(std::env::args());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Astrolabe")
            .with_inner_size([1_040.0, 720.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Astrolabe",
        options,
        Box::new(move |creation| Ok(Box::new(Shell::new(creation, arrived)?))),
    )
    .map_err(|error| anyhow::anyhow!("run the Astrolabe window: {error}"))
}

/// The window. It owns the model and draws it, and holds no state of its own
/// beyond which surface is showing and what is half-typed on it.
struct Shell {
    app: App,
    chrome: Chrome,
    /// The background half. Everything slow happens there; the frame loop only
    /// ever drains what it has already produced.
    runtime: Runtime,
    /// The tray, and what it has been asked for. `None` when the platform has
    /// no tray or the shell refused one — in which case closing the window
    /// closes the client, because minimising to something that is not there
    /// would make the window unrecoverable.
    tray: Option<(Tray, std::sync::mpsc::Receiver<TrayCommand>)>,
    /// Whether the window is currently hidden in the tray.
    minimised: bool,
    /// Set once an exit has been carried out. The next frame closes.
    leaving: bool,
}

impl Shell {
    fn new(
        creation: &eframe::CreationContext<'_>,
        arrived: Option<astrolabe::link::Link>,
    ) -> Result<Self> {
        let sidecar = astrolabe::sidecar::resolve()?;
        let state_root = state_root()?;

        // The repaint request is passed in rather than captured inside the
        // runtime, so the background half has no opinion about what a frame
        // loop is and can be driven by a test that has none.
        let context = creation.egui_ctx.clone();
        let runtime = Runtime::start(Config::new(state_root, sidecar), move || {
            context.request_repaint();
        })?;

        // A failure to place a tray is not a failure to start: the client still
        // works, closing just means closing. Reported rather than fatal.
        let tray = match Tray::place("Astrolabe") {
            Ok(placed) => Some(placed),
            Err(error) => {
                tracing::warn!(%error, "no tray icon; closing the window will close the client");
                None
            }
        };

        let mut chrome = Chrome::default();
        let mut app = App::new();
        if let Some(astrolabe::link::Link::Invite { ticket }) = arrived {
            // Filled in, not acted on. The person still presses Enter, which is
            // the difference between opening a link and accepting an invite.
            chrome.surface = Surface::Spaces;
            chrome.spaces.invite = ticket;
            app.fail(
                "an invite link opened this window",
                astrolabe::ClientError::invalid(
                    "the invite is filled in below — nothing has been accepted yet",
                ),
            );
        }

        Ok(Self {
            app,
            chrome,
            runtime,
            tray,
            minimised: false,
            leaving: false,
        })
    }

    /// Everything the tray asked for since the last frame.
    fn drain_tray(&mut self, ui: &egui::Ui) {
        let Some((_, commands)) = &self.tray else {
            return;
        };
        let asked: Vec<TrayCommand> = commands.try_iter().collect();
        for command in asked {
            match command {
                TrayCommand::Restore => {
                    self.minimised = false;
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayCommand::Exit(request) => {
                    self.app.dispatched(&Action::Exit(request));
                    self.runtime.dispatch(Action::Exit(request));
                }
            }
        }
    }

    /// Closing minimises; it does not stop a peer.
    ///
    /// A person who clicked the wrong X did not ask for their Spaces to stop
    /// converging, and the daemon outlives every window by design. With no tray
    /// to minimise *to*, the window would become unrecoverable — so that case
    /// closes, which is the lesser wrong.
    fn handle_close(&mut self, ui: &egui::Ui) {
        if !ui.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.tray.is_none() || self.leaving {
            return;
        }
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.minimised = true;
        if let Some((tray, _)) = &self.tray {
            tray.notify(
                "Astrolabe is still serving",
                "Your devices stay online. Open it again from here.",
            );
        }
    }
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Applied at the top of the frame, so everything drawn below sees the
        // same state — a surface that drained mid-frame could show two
        // different readings of the same moment.
        self.runtime.drain_into(&mut self.app);
        self.drain_tray(ui);

        if self.app.exit().is_some() && !self.leaving {
            self.leaving = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        for action in astrolabe::ui::draw(ui, &self.app, &mut self.chrome) {
            // Recorded as in flight on the frame the click happened, so the
            // control that caused it is disabled now rather than on whichever
            // later frame the background half gets round to answering.
            self.app.dispatched(&action);
            self.runtime.dispatch(action);
        }

        self.handle_close(ui);
    }
}

/// Where this client keeps what it manages.
///
/// Under the user's local data directory, not beside the executable: a program
/// directory may be read-only, may be shared between users, and is replaced
/// wholesale by an upgrade.
fn state_root() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context("locate a local data directory for the managed state root")?;
    let root = base.join("Astrolabe").join("devices");
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create the managed state root {}", root.display()))?;
    Ok(root)
}

fn tracing_subscriber_init() {
    // Stderr, not a console: a windows-subsystem program has no console to
    // write to, and asking for one would undo the point of not having it.
    let _ = std::io::Write::flush(&mut std::io::stderr());
}
